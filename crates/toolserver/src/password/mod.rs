//! Email-and-password sign-in with a required authenticator-app code
//! (`docs/password-sign-in.md`).
//!
//! The tool server owns users, so it owns their password hash, their authenticator secret and
//! their recovery codes. `pmos-web` forwards what a person typed to the routes in
//! `app/password.rs` and gets back a decision; it never stores or sees a hash or a secret.
//!
//! - [`hash`]: the rules for a new password, and Argon2id.
//! - [`totp`]: the authenticator's codes (RFC 6238).
//! - [`codes`]: setup codes, recovery codes and the tokens between steps.
//! - [`limits`]: the per-account delay and the tool server's global limit.
//! - [`store`]: [`store::PasswordStore`], one person's state changed whole or not at all.
//! - [`PasswordService`]: the flows: sign in (password, then the second step), set up (or
//!   reset) with a setup code, change the password, regenerate recovery codes; and
//!   [`issue_setup_code`], for an Admin in the web app or the host.
//!
//! **There is no way to sign in with a password alone**, and no setting that turns the second
//! step off: a password step that passes gives only a pending token, good for the second step
//! of that one person for five minutes and three tries.
//!
//! **Nothing secret leaves this module but on purpose:** a setup code, a recovery code and an
//! enrolment secret are returned once, to be shown once; a pending token once, to be handed
//! to the browser. None is logged or put in an audit event or an error. Audit details are
//! fixed words and counts ([`is_safe_detail`]).

pub mod codes;
pub mod hash;
pub mod limits;
pub mod store;
pub mod totp;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use tokio::sync::Semaphore;
use zeroize::Zeroizing;

use crate::audit::{AuditEvent, Client, Outcome};
use crate::store::StoreError;
use crate::users::{Actor, ChangeKind, HostUser, RoleChange, Via, EVALS};
use crate::vault::KeyRing;
use hash::{HashParams, Weak};
use limits::{
    AccountWork, BlockGuesses, Draw, GlobalLimit, SourceCounters, HASH_PERMITS, HASH_WAIT_SECS,
};
use store::{
    CodeKind, Credential, NewSetupCode, PasswordStore, Pending, Person, Purpose, ResetNotice,
    Snapshot, Writes,
};

/// How long the pending token after a correct password lives.
pub const SIGN_IN_PENDING: Duration = Duration::minutes(5);
/// Second-step tries per pending token.
pub const SIGN_IN_TRIES: i32 = 3;
/// How long a setup token lives (never past its setup code's expiry).
pub const SETUP_PENDING: Duration = Duration::minutes(15);
/// Code tries when confirming the authenticator during setup, per setup token.
pub const SETUP_TRIES: i32 = 5;
/// How long a setup (or reset) code lives.
pub const SETUP_CODE_TTL: Duration = Duration::hours(24);
/// The most sign-in tokens one person holds at once; past it the oldest go.
pub const MAX_SIGN_IN_PENDING: usize = 5;

/// The run id password events are recorded under.
pub const RUN: &str = "password";
/// Who an event is recorded as before anyone proved who they are.
pub const ANONYMOUS: &str = "anonymous";

/// Why a password call didn't do what was asked. [`PwError::code`] is the API's error code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PwError {
    /// The password step, for every reason alike: no such user, no password set, a wrong
    /// password, a deactivated user, an account waiting out its delay.
    SignInFailed,
    /// A wrong authenticator or recovery code; the pending token has this many tries left.
    CodeFailed {
        attempts_left: i32,
    },
    /// No sign-in in progress for that token (unknown, expired, used up): start again.
    SignInExpired,
    /// The setup's first step, for every reason alike: no such user, no code, a wrong,
    /// expired or used code, an account waiting out its delay.
    SetupFailed,
    /// No setup in progress for that token: start again with the code.
    SetupExpired,
    /// The setup code gave all the work it may (its tokens or its password sets): an admin
    /// issues a new one (H1).
    SetupCodeSpent,
    /// A setup step out of order (the authenticator before the password, or the confirmation
    /// before the authenticator).
    WrongStep,
    Weak(Weak),
    /// A signed-in person's current password or code was wrong.
    ReauthFailed,
    /// The signed-in person has no password to change.
    NoPassword,
    /// Too many attempts: the account's delay, or the tool server's limit.
    TooMany {
        retry_after: u64,
    },
    /// Every hashing slot is busy.
    Busy,
    /// Deactivated (after a correct password or setup code).
    NotAMember,
    SignInUnconfirmed,
    HostOnly,
    SelfChange,
    EvalsAccount,
    NotActive,
    NotFound,
    Forbidden,
    /// Storage (or the vault) failed; the detail goes to the log only.
    Unavailable(String),
}

impl From<StoreError> for PwError {
    fn from(e: StoreError) -> Self {
        Self::Unavailable(e.0)
    }
}

impl PwError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::SignInFailed => "sign_in_failed",
            Self::CodeFailed { .. } => "code_failed",
            Self::SignInExpired => "sign_in_expired",
            Self::SetupFailed => "setup_failed",
            Self::SetupExpired => "setup_expired",
            Self::SetupCodeSpent => "setup_code_spent",
            Self::WrongStep => "wrong_step",
            Self::Weak(_) => "weak_password",
            Self::ReauthFailed => "reauth_failed",
            Self::NoPassword => "no_password",
            Self::TooMany { .. } => "too_many_attempts",
            Self::Busy => "sign_in_busy",
            Self::NotAMember => "not_a_member",
            Self::SignInUnconfirmed => "sign_in_unconfirmed",
            Self::HostOnly => "host_only",
            Self::SelfChange => "self_change",
            Self::EvalsAccount => "evals_account",
            Self::NotActive => "not_active",
            Self::NotFound => "not_found",
            Self::Forbidden => "forbidden",
            Self::Unavailable(_) => "unavailable",
        }
    }

    /// The HTTP status.
    pub fn status(&self) -> u16 {
        match self {
            Self::SignInFailed
            | Self::CodeFailed { .. }
            | Self::SignInExpired
            | Self::SetupFailed
            | Self::SetupExpired => 401,
            Self::ReauthFailed
            | Self::NotAMember
            | Self::HostOnly
            | Self::SelfChange
            | Self::Forbidden => 403,
            Self::NotFound => 404,
            Self::WrongStep
            | Self::SetupCodeSpent
            | Self::NoPassword
            | Self::SignInUnconfirmed
            | Self::EvalsAccount
            | Self::NotActive => 409,
            Self::Weak(_) => 422,
            Self::TooMany { .. } => 429,
            Self::Busy | Self::Unavailable(_) => 503,
        }
    }

    /// A plain sentence. Fixed text: never a value the person sent.
    pub fn message(&self) -> &'static str {
        match self {
            Self::SignInFailed => {
                "that sign-in name and password don't match an account that can sign in now"
            }
            Self::CodeFailed { .. } => "that code isn't right",
            Self::SignInExpired => "no sign-in in progress: start again with the password",
            Self::SetupFailed => "that sign-in name and setup code don't match a setup that can start now",
            Self::SetupExpired => "no setup in progress: start again with the setup code",
            Self::SetupCodeSpent => {
                "this setup code was used for as many tries as it allows: ask an admin for a new one"
            }
            Self::WrongStep => "do the setup's steps in order: password, authenticator, confirm",
            Self::Weak(w) => w.message(),
            Self::ReauthFailed => "the current password or the authenticator code isn't right",
            Self::NoPassword => "this person has no password set",
            Self::TooMany { .. } => "too many attempts; wait and try again",
            Self::Busy => "sign-in is busy; try again in a moment",
            Self::NotAMember => "this person's access to PM OS is turned off",
            Self::SignInUnconfirmed => "an admin must confirm this person's sign-in name first",
            Self::HostOnly => "an Admin's or Auditor's setup code comes from the host (pmos-admin users password-setup)",
            Self::SelfChange => "nobody issues a setup code for themselves",
            Self::EvalsAccount => "the evals account never has a password",
            Self::NotActive => "this person is deactivated",
            Self::NotFound => "no such user",
            Self::Forbidden => "only an active admin issues setup codes",
            Self::Unavailable(_) => "PM OS storage is unavailable",
        }
    }
}

pub type PwResult<T> = Result<T, PwError>;

/// What a password call is made with. `Debug` never shows it.
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Self(Zeroizing::new(s.into()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// The second step: an authenticator code, or one of the recovery codes.
#[derive(Debug)]
pub enum SecondStep {
    Code(Secret),
    Recovery(Secret),
}

/// A password step that passed: the pending token for the second step (hand it to the browser
/// once; it is stored hashed).
#[derive(Debug)]
pub struct Started {
    pub token: Secret,
    pub expires_at: DateTime<Utc>,
}

/// A finished sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedIn {
    pub handle: String,
    /// The credential's epoch: the session ends when it moves.
    pub epoch: i64,
    pub recovery_code_used: bool,
    pub recovery_codes_left: u32,
}

/// A setup's first step that passed.
#[derive(Debug)]
pub struct SetupStarted {
    pub token: Secret,
    pub handle: String,
    pub kind: CodeKind,
    pub expires_at: DateTime<Utc>,
}

/// The enrolment: shown once.
#[derive(Debug)]
pub struct Enrolment {
    pub uri: Secret,
    pub secret: Secret,
    /// The account name the app shows (the sign-in name).
    pub account: String,
}

/// A finished setup: the person's recovery codes (shown once) and the new epoch.
#[derive(Debug)]
pub struct SetupDone {
    pub handle: String,
    pub kind: CodeKind,
    pub epoch: i64,
    pub recovery_codes: Vec<Secret>,
}

/// An issued setup code (shown once).
#[derive(Debug)]
pub struct IssuedCode {
    pub code: Secret,
    pub handle: String,
    pub kind: CodeKind,
    pub expires_at: DateTime<Utc>,
}

/// The authenticator secret's binding: sealed for this person and this use only.
fn totp_aad(handle: &str) -> String {
    format!("pmos-vault|{handle}|password|totp")
}

/// The fixed words an audit detail of a password event is made of (see [`is_safe_detail`]).
pub const DETAIL_WORDS: [&str; 37] = [
    "dismissed",
    // The review of 8e8f8fe: refusals past a block's setup guesses, recorded thinned (with
    // `refused <n>`).
    "block_limit",
    // The re-check of eb24d28: an account reaching its ceiling (M2), a setup code that gave
    // all its work (H1). The gate review of 04c175f: a rehash put off while the hashing
    // allowance was full (GL2).
    "ceiling",
    "rehash_deferred",
    "spent",
    // An Auditor's view of a refused password step or setup's first step: one reason for all
    // (`app::admin`, L15).
    "sign_in_failed",
    "setup_failed",
    "password",
    "rehashed",
    "unknown_name",
    "no_password",
    "deactivated",
    "locked",
    "wrong_password",
    "stale",
    "authenticator",
    "recovery_code",
    "wrong_code",
    "expired",
    "setup",
    "reset",
    "change",
    "regenerated",
    "sign_in_unconfirmed",
    "host_only",
    "evals_account",
    "wrong_step",
    "reauth_failed",
    "weak_password",
    "forbidden",
    "self_change",
    "not_found",
    "not_active",
    "begin",
    "complete",
    "too_many_attempts",
    "malformed",
];

/// Is `detail` one of the password events' fixed formats: parts joined by `"; "`, each a
/// [`DETAIL_WORDS`] word, `host_user <name>`, `failures <n>`, `recovery_left <n>` or
/// `refused <n>`? Never a
/// value anyone typed.
pub fn is_safe_detail(detail: &str) -> bool {
    let count = |s: &str| !s.is_empty() && s.len() <= 6 && s.bytes().all(|b| b.is_ascii_digit());
    !detail.is_empty()
        && detail.split("; ").all(|part| {
            DETAIL_WORDS.contains(&part)
                || part
                    .strip_prefix("host_user ")
                    .is_some_and(|h| HostUser::parse(h).is_some())
                || part.strip_prefix("failures ").is_some_and(count)
                || part.strip_prefix("recovery_left ").is_some_and(count)
                || part.strip_prefix("refused ").is_some_and(count)
        })
}

/// One password audit event. `actor`: who (a handle, `host` or [`ANONYMOUS`]).
fn event(
    client: Client,
    actor: &str,
    action: &str,
    outcome: Outcome,
    target: Option<&str>,
    detail: &str,
    now: DateTime<Utc>,
) -> AuditEvent {
    debug_assert!(is_safe_detail(detail), "{detail}");
    AuditEvent {
        at: now,
        client,
        run_id: RUN.to_string(),
        user: actor.to_string(),
        skill: None,
        bot: None,
        tool: action.to_string(),
        outcome,
        sources: Vec::new(),
        source_titles: Default::default(),
        args_hash: None,
        output_ref: target.map(String::from),
        detail: Some(detail.to_string()),
        persona: None,
    }
}

/// A web event about `handle` (the person, proven or not).
fn web_event(
    actor: &str,
    action: &str,
    outcome: Outcome,
    handle: &str,
    detail: &str,
    now: DateTime<Utc>,
) -> AuditEvent {
    event(
        Client::Web,
        actor,
        action,
        outcome,
        Some(handle),
        detail,
        now,
    )
}

/// The lockout event when a failure started a wait.
fn lockout(handle: &str, counter: &limits::Counter, setup: bool, now: DateTime<Utc>) -> AuditEvent {
    let mut detail = format!("failures {}", counter.count.min(999_999));
    if setup {
        detail.push_str("; setup");
    }
    web_event(
        ANONYMOUS,
        "password.lockout",
        Outcome::Denied,
        handle,
        &detail,
        now,
    )
}

/// The events a counted sign-in failure leaves: the address's wait, and the account reaching
/// its ceiling (from then on, new addresses are rationed: M2).
fn failure_events(
    handle: &str,
    failed: limits::Failed,
    source: &limits::Counter,
    now: DateTime<Utc>,
) -> Vec<AuditEvent> {
    let mut out = Vec::new();
    if failed.started_wait {
        out.push(lockout(handle, source, false, now));
    }
    if failed.reached_ceiling {
        out.push(web_event(
            ANONYMOUS,
            "password.lockout",
            Outcome::Denied,
            handle,
            "ceiling",
            now,
        ));
    }
    out
}

/// Run `plan` on `handle`'s state (with the counters of the address `source`, a
/// [`limits::source_key`]) in one transaction; give back what it decided.
async fn tx<T: Send>(
    store: &dyn PasswordStore,
    handle: &str,
    actor: Option<&str>,
    source: Option<&str>,
    now: DateTime<Utc>,
    plan: impl FnOnce(&Snapshot) -> (Writes, T) + Send,
) -> PwResult<T> {
    let mut out: Option<T> = None;
    {
        let slot = &mut out;
        store
            .password_tx(
                handle,
                actor,
                source,
                now,
                Box::new(move |s: &Snapshot| {
                    let (w, t) = plan(s);
                    *slot = Some(t);
                    w
                }),
            )
            .await?;
    }
    out.ok_or_else(|| PwError::Unavailable("the plan didn't run".into()))
}

/// A sign-in name as stored: trimmed, lower-case; `None` when it can't be one.
fn sign_in_name(name: &str) -> Option<String> {
    let n = name.trim().to_lowercase();
    let ok = !n.is_empty() && n.len() <= 254 && n.contains('@') && !n.chars().any(char::is_control);
    ok.then_some(n)
}

/// A pending token of `purpose` in `s`, live at `now`.
fn live_pending<'a>(
    s: &'a Snapshot,
    token_hash: &str,
    purpose: Purpose,
    now: DateTime<Utc>,
) -> Option<&'a Pending> {
    s.pending(token_hash)
        .filter(|p| p.purpose == purpose && p.expires_at > now)
}

/// The setup in progress for `token_hash`: its pending token, still on its live, unexpired
/// code, for an active person.
fn live_setup<'a>(
    s: &'a Snapshot,
    token_hash: &str,
    now: DateTime<Utc>,
) -> Result<(&'a Pending, &'a store::SetupCode, &'a Person), PwError> {
    let p = live_pending(s, token_hash, Purpose::Setup, now).ok_or(PwError::SetupExpired)?;
    let code = s
        .setup_code
        .as_ref()
        .filter(|c| Some(c.id) == p.setup_code_id && c.expires_at > now)
        .ok_or(PwError::SetupExpired)?;
    let person = s.person.as_ref().ok_or(PwError::SetupExpired)?;
    if !person.active() {
        return Err(PwError::NotAMember);
    }
    Ok((p, code, person))
}

/// The flows, with the vault's keys (for the authenticator secret), the hashing parameters,
/// the hashing slots and the global limit.
pub struct PasswordService {
    keys: KeyRing,
    params: HashParams,
    /// A hash of a random password, verified against when there is no real one, so every
    /// refusal at the password step costs the same.
    dummy: String,
    slots: Arc<Semaphore>,
    /// How long a request waits for a hashing slot ([`HASH_WAIT_SECS`]; shorter in tests).
    hash_wait: std::time::Duration,
    global: GlobalLimit,
    /// Hashing work after an authentication, per account (H1, LA).
    work: AccountWork,
    /// Wrong setup codes per network block (G2).
    setup_guesses: BlockGuesses,
    /// Plain counts since start-up, for an operator (the tool server's `/readyz`).
    setup_wrong_guesses: AtomicU64,
    setup_block_refusals: AtomicU64,
    rehashes_deferred: AtomicU64,
}

impl std::fmt::Debug for PasswordService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PasswordService")
            .field("keys", &self.keys)
            .field("params", &self.params)
            .finish_non_exhaustive()
    }
}

impl PasswordService {
    /// With the current parameters ([`hash::CURRENT`]).
    pub fn new(keys: KeyRing) -> Result<Self, String> {
        Self::with_params(keys, hash::CURRENT)
    }

    /// With other parameters: tests (quick ones; or raising them to see a rehash).
    pub fn with_params(keys: KeyRing, params: HashParams) -> Result<Self, String> {
        let random = codes::new_token().map_err(String::from)?;
        let dummy = hash::hash(random.as_bytes(), params).map_err(String::from)?;
        Ok(Self {
            keys,
            params,
            dummy,
            slots: Arc::new(Semaphore::new(HASH_PERMITS)),
            hash_wait: std::time::Duration::from_secs(HASH_WAIT_SECS),
            global: GlobalLimit::default(),
            work: AccountWork::default(),
            setup_guesses: BlockGuesses::default(),
            setup_wrong_guesses: AtomicU64::new(0),
            setup_block_refusals: AtomicU64::new(0),
            rehashes_deferred: AtomicU64::new(0),
        })
    }

    /// With another global limit per minute and reserve (tests).
    pub fn with_global_limit(mut self, per_minute: usize, reserve: usize) -> Self {
        self.global = GlobalLimit::new(per_minute, reserve);
        self
    }

    /// With another global limit and shares (tests).
    pub fn with_global(mut self, global: GlobalLimit) -> Self {
        self.global = global;
        self
    }

    /// With another wait for a hashing slot (tests: the busy paths without a 10-second wait).
    pub fn with_hash_wait(mut self, wait: std::time::Duration) -> Self {
        self.hash_wait = wait;
        self
    }

    /// Hold every hashing slot until the permits are dropped (tests: every slot busy).
    pub async fn hold_hashing_slots(&self) -> Vec<tokio::sync::OwnedSemaphorePermit> {
        let mut held = Vec::new();
        for _ in 0..HASH_PERMITS {
            if let Ok(p) = self.slots.clone().acquire_owned().await {
                held.push(p);
            }
        }
        held
    }

    pub fn params(&self) -> HashParams {
        self.params
    }

    /// Wrong setup codes (and unknown names at a setup's first step) since start-up: a plain
    /// count for an operator, on the tool server's `/readyz` (G2). Each is in the audit log too.
    pub fn setup_wrong_guesses(&self) -> u64 {
        self.setup_wrong_guesses.load(Ordering::Relaxed)
    }

    /// Refusals of a setup's first step past its block's guesses since start-up (`429`, every
    /// one): a plain count for an operator, on `/readyz`. The audit log has them thinned, at
    /// most one row a minute per block (the review of 8e8f8fe, a Low).
    pub fn setup_block_refusals(&self) -> u64 {
        self.setup_block_refusals.load(Ordering::Relaxed)
    }

    /// Rehashes put off since start-up because the hashing allowance was full (GL2): each is
    /// done at a later correct sign-in, and recorded (`password; rehash_deferred`).
    pub fn rehashes_deferred(&self) -> u64 {
        self.rehashes_deferred.load(Ordering::Relaxed)
    }

    /// One hash from the global limit, drawn for `draw`'s block (and, for a password step from
    /// a known address, [`PasswordStore::source_known`], that address, which may use the
    /// reserve). Setup and the signed-in routes draw for the account's own block
    /// ([`limits::account_block`]) from the open allowance only.
    fn take_global(&self, now: DateTime<Utc>, draw: Draw<'_>) -> Result<(), u64> {
        self.global.take(now, draw).inspect_err(|_| {
            tracing::warn!(
                eligible = draw.reserve_for.is_some(),
                "password hashing over the tool server's limit"
            );
        })
    }

    /// One piece of hashing work for `handle` after an authentication, checked and counted
    /// (H1, LA); then one hash from the open allowance. A refusal by the open allowance gives
    /// the account's piece back: nothing was done.
    fn take_work(&self, handle: &str, now: DateTime<Utc>) -> PwResult<()> {
        self.work.take(handle, now).map_err(|retry_after| {
            tracing::warn!("password work for one account over its limit");
            PwError::TooMany { retry_after }
        })?;
        let block = limits::account_block(handle);
        self.take_global(now, Draw::open(&block))
            .map_err(|retry_after| {
                self.work.give_back(handle, now);
                PwError::TooMany { retry_after }
            })
    }

    /// Run `work` (Argon2id) on the blocking pool, in one of the [`HASH_PERMITS`] slots, waiting
    /// at most [`HASH_WAIT_SECS`] for one.
    async fn hashing<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> PwResult<T> {
        let slot = tokio::time::timeout(self.hash_wait, self.slots.clone().acquire_owned())
            .await
            .map_err(|_| PwError::Busy)?
            .map_err(|_| PwError::Busy)?;
        let out = tokio::task::spawn_blocking(move || {
            let _slot = slot;
            work()
        })
        .await
        .map_err(|_| PwError::Unavailable("a hashing task failed".into()))?;
        Ok(out)
    }

    async fn verify(&self, password: &Secret, stored: String) -> PwResult<bool> {
        let pw = Zeroizing::new(password.expose().as_bytes().to_vec());
        self.hashing(move || hash::verify(&pw, &stored)).await
    }

    async fn hash(&self, password: &Secret) -> PwResult<String> {
        let pw = Zeroizing::new(password.expose().as_bytes().to_vec());
        let params = self.params;
        self.hashing(move || hash::hash(&pw, params))
            .await?
            .map_err(|why| PwError::Unavailable(why.into()))
    }

    /// The same work as a real check, against nothing.
    async fn dummy_verify(&self, password: &Secret) -> PwResult<()> {
        self.verify(password, self.dummy.clone()).await.map(drop)
    }

    fn unseal_totp(&self, handle: &str, sealed: &str) -> PwResult<Zeroizing<Vec<u8>>> {
        let text = Zeroizing::new(self.keys.unseal_bound(sealed, &totp_aad(handle))?);
        totp::unbase32(&text)
            .ok_or_else(|| PwError::Unavailable("a stored secret is malformed".into()))
    }

    fn seal_totp(&self, handle: &str, secret: &[u8]) -> PwResult<String> {
        let text = Zeroizing::new(totp::base32(secret));
        Ok(self.keys.seal_bound(&text, &totp_aad(handle))?)
    }

    // --- Sign-in ---------------------------------------------------------------------------

    /// The password step, from the address `source` (pmos-web's view of the browser; `None`
    /// shares one count). Every refusal is [`PwError::SignInFailed`], after the same work;
    /// what happened is in the audit log (by reason, never by value). Only the global limit's
    /// own refusal differs (`429`), and whether an attempt may use its reserve is decided from
    /// the address alone, before the name is looked up, so an unknown name and a real account
    /// get the same answer under load too (M3). The one other `429` is an account whose
    /// correct password was already given [`limits::ACCOUNT_WORK`] times in the window: only
    /// whoever holds the password can cause it.
    pub async fn sign_in(
        &self,
        store: &dyn PasswordStore,
        name: &str,
        password: &Secret,
        source: Option<&str>,
        now: DateTime<Utc>,
    ) -> PwResult<Started> {
        let key = limits::source_key(source);
        let block = limits::block_key(source);
        // M3: from the address alone. The shared `unknown` key is never known.
        let eligible = limits::is_address(source)
            && store
                .source_known(&key, now - Duration::days(limits::KNOWN_SOURCE_DAYS))
                .await?;
        // G1: every hash of this step is counted under the address's block; a known address
        // may use the reserve, up to its own share and its block's.
        let draw = Draw {
            block: &block,
            reserve_for: eligible.then_some(key.as_str()),
        };
        let handle = match sign_in_name(name) {
            Some(n) => store.handle_by_sign_in_name(&n).await?,
            None => None,
        };
        let Some(handle) = handle else {
            self.take_global(now, draw)
                .map_err(|retry_after| PwError::TooMany { retry_after })?;
            self.dummy_verify(password).await?;
            store
                .password_events(vec![event(
                    Client::Web,
                    ANONYMOUS,
                    "password.sign_in",
                    Outcome::Denied,
                    None,
                    "unknown_name",
                    now,
                )])
                .await?;
            tracing::info!(reason = "unknown_name", "password sign-in refused");
            return Err(PwError::SignInFailed);
        };
        // Count the failure before hashing (taken back if the password is right), so guesses
        // in parallel can't slip past the threshold; refuse at once while this address waits
        // for this account (another address is not held up: PM1). The global hash is taken
        // here too, so a refusal by it counts nothing.
        enum Reserve {
            Refused(&'static str),
            Busy(u64),
            Check {
                stored: String,
                epoch: i64,
                /// The events this failure (taken back if the password is right) leaves.
                events: Vec<AuditEvent>,
            },
        }
        let h = handle.clone();
        let reserved = tx(store, &handle, None, Some(&key), now, move |s| {
            let mut w = Writes::default();
            let refuse = |reason: &'static str, mut w: Writes| {
                // A refusal costs a (dummy) hash too, from the same allowance.
                if let Err(retry_after) = self.take_global(now, draw) {
                    return (Writes::default(), Reserve::Busy(retry_after));
                }
                w.events.push(web_event(
                    ANONYMOUS,
                    "password.sign_in",
                    Outcome::Denied,
                    &h,
                    reason,
                    now,
                ));
                (w, Reserve::Refused(reason))
            };
            let Some(person) = s.person.as_ref() else {
                return refuse("unknown_name", w);
            };
            if !person.active() {
                return refuse("deactivated", w);
            }
            let Some(cred) = s.credential.clone().filter(Credential::is_set) else {
                return refuse("no_password", w);
            };
            if cred.held_for(person) {
                // An Admin's or Auditor's credential an admin issued in the web app (PM3).
                return refuse("host_only", w);
            }
            if limits::may_try(&s.source, now).is_err() {
                return refuse("locked", w);
            }
            // H1: a correct password already given this many times in the window. Only the
            // holder of the password fills it (wrong guesses never count here). Checked here,
            // before the hash, and taken, in one step, once the password proved right (GL1).
            // A full room answers `429` to every attempt on the account, right or wrong, as
            // `docs/password-sign-in.md` says (the review of 8e8f8fe weighed `401`): only the
            // password's holder fills it (wrong guesses never count; a busy check, here or in
            // `change`, gives its piece back), and answering `401` would mean either hashing
            // every attempt on a full room, which lets the holder spend the shared allowance
            // past `pmos-web`'s count (H1), or calling the holder's right password wrong.
            if let Err(retry_after) = self.work.check(&h, now) {
                return (w, Reserve::Busy(retry_after));
            }
            if let Err(retry_after) = self.take_global(now, draw) {
                return (w, Reserve::Busy(retry_after));
            }
            let mut next = cred.clone();
            let mut src = s.source;
            let failed = limits::fail_sign_in(&mut next.failed, &mut src, now);
            let events = failure_events(&h, failed, &src.sign_in, now);
            let stored = cred.password_hash.clone().unwrap_or_default();
            let epoch = cred.epoch;
            w.credential = Some(next);
            w.source = Some(src);
            (
                w,
                Reserve::Check {
                    stored,
                    epoch,
                    events,
                },
            )
        })
        .await?;
        let (stored, epoch, failure) = match reserved {
            Reserve::Busy(retry_after) => return Err(PwError::TooMany { retry_after }),
            Reserve::Refused(reason) => {
                self.dummy_verify(password).await?;
                tracing::info!(reason, "password sign-in refused");
                return Err(PwError::SignInFailed);
            }
            Reserve::Check {
                stored,
                epoch,
                events,
            } => (stored, epoch, events),
        };
        let right = match self.verify(password, stored.clone()).await {
            Ok(right) => right,
            Err(e) => {
                if e == PwError::Busy {
                    // G3: every slot was busy, so nothing was checked: the failure counted
                    // ahead goes back (its wait, if it started one, stays).
                    self.give_back_failure(store, &handle, &key, now).await;
                }
                return Err(e);
            }
        };
        if !right {
            let mut events = vec![web_event(
                ANONYMOUS,
                "password.sign_in",
                Outcome::Denied,
                &handle,
                "wrong_password",
                now,
            )];
            events.extend(failure);
            store.password_events(events).await?;
            tracing::info!(reason = "wrong_password", "password sign-in refused");
            return Err(PwError::SignInFailed);
        }
        // The right password. Raise its parameters now if they are below the current ones: a
        // hash like any other, from the same allowance (GL2). With no room (or no free slot)
        // it is put off, recorded, and done at a later correct sign-in.
        let rehashed = if hash::needs_rehash(&stored, self.params) {
            match self.take_global(now, draw) {
                Ok(()) => match self.hash(password).await {
                    Ok(h) => Some(h),
                    Err(PwError::Busy) => None,
                    Err(e) => return Err(e),
                },
                Err(_) => None,
            }
        } else {
            None
        };
        let deferred = rehashed.is_none() && hash::needs_rehash(&stored, self.params);
        if deferred {
            self.rehashes_deferred.fetch_add(1, Ordering::Relaxed);
            tracing::warn!("a password rehash was put off: the hashing allowance is full");
        }
        let token = codes::new_token().map_err(|e| PwError::Unavailable(e.into()))?;
        let token_hash = codes::token_hash(&token).ok_or(PwError::Unavailable("token".into()))?;
        let h = handle.clone();
        /// Why the last step refused a right password.
        enum Refused {
            Reason(&'static str),
            /// The account's room for correct password steps is full (GL1).
            Room(u64),
        }
        let started = tx(store, &handle, None, Some(&key), now, move |s| {
            let mut w = Writes::default();
            let person_ok = s.person.as_ref().is_some_and(Person::active);
            let Some(cred) = s.credential.clone().filter(|c| c.is_set()) else {
                return (w, Err(Refused::Reason("no_password")));
            };
            let mut next = cred.clone();
            next.failed.take_back();
            w.credential = Some(next.clone());
            let mut src = s.source;
            src.sign_in.take_back();
            w.source = Some(src);
            if s.person.as_ref().is_some_and(|p| cred.held_for(p)) {
                w.events.push(web_event(
                    ANONYMOUS,
                    "password.sign_in",
                    Outcome::Denied,
                    &h,
                    "host_only",
                    now,
                ));
                return (w, Err(Refused::Reason("host_only")));
            }
            if !person_ok {
                w.events.push(web_event(
                    ANONYMOUS,
                    "password.sign_in",
                    Outcome::Denied,
                    &h,
                    "deactivated",
                    now,
                ));
                return (w, Err(Refused::Reason("deactivated")));
            }
            if cred.epoch != epoch || cred.password_hash.as_deref() != Some(stored.as_str()) {
                // Reset or changed while the password was being checked.
                w.events.push(web_event(
                    ANONYMOUS,
                    "password.sign_in",
                    Outcome::Denied,
                    &h,
                    "stale",
                    now,
                ));
                return (w, Err(Refused::Reason("stale")));
            }
            // GL1: the account's room for correct password steps, checked and counted in one
            // step now that the password proved right, so correct steps in parallel can't pass
            // it together (each was only checked before its hash). Past it, nothing starts;
            // the failure counted ahead still goes back (the password was right).
            if let Err(retry_after) = self.work.take(&h, now) {
                return (w, Err(Refused::Room(retry_after)));
            }
            let mut detail = "password".to_string();
            if let Some(new_hash) = rehashed {
                next.password_hash = Some(new_hash);
                w.credential = Some(next);
                detail.push_str("; rehashed");
            } else if deferred {
                detail.push_str("; rehash_deferred");
            }
            // At most a few sign-ins in flight per person: the oldest go.
            let mut live: Vec<&Pending> = s
                .pending
                .iter()
                .filter(|p| p.purpose == Purpose::SignIn)
                .collect();
            live.sort_by_key(|p| p.created_at);
            while live.len() >= MAX_SIGN_IN_PENDING {
                w.delete_pending.push(live.remove(0).token_hash.clone());
            }
            w.put_pending.push(Pending::new(
                token_hash,
                Purpose::SignIn,
                None,
                epoch,
                now,
                now + SIGN_IN_PENDING,
            ));
            w.events.push(web_event(
                ANONYMOUS,
                "password.sign_in",
                Outcome::Ok,
                &h,
                &detail,
                now,
            ));
            (w, Ok(()))
        })
        .await?;
        match started {
            Ok(()) => Ok(Started {
                token: Secret(token),
                expires_at: now + SIGN_IN_PENDING,
            }),
            Err(Refused::Room(retry_after)) => {
                tracing::warn!("password work for one account over its limit");
                Err(PwError::TooMany { retry_after })
            }
            Err(Refused::Reason(reason)) => {
                tracing::info!(reason, "password sign-in refused");
                Err(PwError::SignInFailed)
            }
        }
    }

    /// Give back the failure [`Self::sign_in`] counted ahead of a check that never ran (G3):
    /// on the account and on the address, counts only (a wait stays). Best effort: a failure
    /// here leaves one failure counted, which is where the attempt stood before.
    async fn give_back_failure(
        &self,
        store: &dyn PasswordStore,
        handle: &str,
        key: &str,
        now: DateTime<Utc>,
    ) {
        let r = tx(store, handle, None, Some(key), now, |s| {
            let mut w = Writes::default();
            if let Some(mut cred) = s.credential.clone() {
                cred.failed.give_back();
                w.credential = Some(cred);
                let mut src = s.source;
                src.sign_in.give_back();
                w.source = Some(src);
            }
            (w, ())
        })
        .await;
        if r.is_err() {
            tracing::warn!("a sign-in failure counted ahead of a busy check wasn't given back");
        }
    }

    /// The second step, with the pending token from [`Self::sign_in`], from the address
    /// `source`: an authenticator code (each accepted once) or a recovery code (each used
    /// once). The token is checked first; no hash is computed, so the global limit isn't
    /// touched.
    pub async fn second_step(
        &self,
        store: &dyn PasswordStore,
        token: &str,
        step: &SecondStep,
        source: Option<&str>,
        now: DateTime<Utc>,
    ) -> PwResult<SignedIn> {
        let Some(token_hash) = codes::token_hash(token) else {
            return Err(PwError::SignInExpired);
        };
        let Some(handle) = store.handle_by_token(&token_hash).await? else {
            return Err(PwError::SignInExpired);
        };
        let key = limits::source_key(source);
        let address = limits::is_address(source);
        let h = handle.clone();
        tx(store, &handle, None, Some(&key), now, move |s| {
            let mut w = Writes::default();
            let ev = |outcome, detail: &str| {
                web_event(ANONYMOUS, "password.second_step", outcome, &h, detail, now)
            };
            let Some(p) = live_pending(s, &token_hash, Purpose::SignIn, now).cloned() else {
                // An expired sign-in token goes; another purpose's token is left alone.
                if s.pending(&token_hash)
                    .is_some_and(|p| p.purpose == Purpose::SignIn)
                {
                    w.delete_pending.push(token_hash.clone());
                    w.events.push(ev(Outcome::Denied, "expired"));
                }
                return (w, Err(PwError::SignInExpired));
            };
            if !s.person.as_ref().is_some_and(Person::active) {
                w.clear_pending = true;
                w.events.push(ev(Outcome::Denied, "deactivated"));
                return (w, Err(PwError::NotAMember));
            }
            let Some(cred) = s
                .credential
                .clone()
                .filter(|c| c.is_set() && c.epoch == p.epoch)
            else {
                w.delete_pending.push(p.token_hash.clone());
                w.events.push(ev(Outcome::Denied, "stale"));
                return (w, Err(PwError::SignInExpired));
            };
            if s.person.as_ref().is_some_and(|p| cred.held_for(p)) {
                // Promoted to Admin or Auditor since the password step (PM3).
                w.delete_pending.push(p.token_hash.clone());
                w.events.push(ev(Outcome::Denied, "host_only"));
                return (w, Err(PwError::SignInExpired));
            }
            if let Err(until) = limits::may_try(&s.source, now) {
                w.events.push(ev(Outcome::Denied, "locked"));
                return (
                    w,
                    Err(PwError::TooMany {
                        retry_after: limits::retry_after(until, now),
                    }),
                );
            }
            let mut next = cred.clone();
            let unused = s.recovery.iter().filter(|r| r.used_at.is_none());
            let matched: Option<(Option<i64>, Option<String>)> = match step {
                SecondStep::Code(code) => {
                    let secret = match self
                        .unseal_totp(&h, cred.totp_sealed.as_deref().unwrap_or_default())
                    {
                        Ok(s) => s,
                        Err(e) => return (Writes::default(), Err(e)),
                    };
                    totp::verify(&secret, code.expose(), now.timestamp(), cred.totp_last_step)
                        .map(|step| (Some(step), None))
                }
                SecondStep::Recovery(code) => codes::recovery_code_hash(&h, code.expose())
                    .and_then(|hash| {
                        unused
                            .clone()
                            .find(|r| codes::same_hash(&r.hash, &hash))
                            .map(|r| (None, Some(r.hash.clone())))
                    }),
            };
            let mut src = s.source;
            match matched {
                Some((step, recovery)) => {
                    // This address is the person's own now: its count goes back to zero and
                    // it stays known. The account-wide count is left to age out. Only a real
                    // address becomes known: never the shared key of attempts with none (M1).
                    src.sign_in.clear();
                    if address {
                        src.last_success_at = Some(now);
                    }
                    w.source = Some(src);
                    if let Some(step) = step {
                        next.totp_last_step = Some(step);
                        // A secret sealed with an older vault key is sealed again.
                        if !self
                            .keys
                            .sealed_with_active(next.totp_sealed.as_deref().unwrap_or_default())
                        {
                            if let Ok(secret) = self
                                .unseal_totp(&h, next.totp_sealed.as_deref().unwrap_or_default())
                            {
                                if let Ok(sealed) = self.seal_totp(&h, &secret) {
                                    next.totp_sealed = Some(sealed);
                                }
                            }
                        }
                    }
                    let left = unused.count() as u32 - u32::from(recovery.is_some());
                    let detail = match &recovery {
                        Some(_) => format!("recovery_code; recovery_left {left}"),
                        None => "authenticator".to_string(),
                    };
                    w.use_recovery = recovery.clone();
                    w.credential = Some(next);
                    w.delete_pending.push(p.token_hash.clone());
                    w.events.push(web_event(
                        &h,
                        "password.second_step",
                        Outcome::Ok,
                        &h,
                        &detail,
                        now,
                    ));
                    (
                        w,
                        Ok(SignedIn {
                            handle: h.clone(),
                            epoch: cred.epoch,
                            recovery_code_used: recovery.is_some(),
                            recovery_codes_left: left,
                        }),
                    )
                }
                None => {
                    let failed = limits::fail_sign_in(&mut next.failed, &mut src, now);
                    w.source = Some(src);
                    let mut p = p.clone();
                    p.attempts += 1;
                    let left = (SIGN_IN_TRIES - p.attempts).max(0);
                    if left == 0 {
                        w.delete_pending.push(p.token_hash.clone());
                    } else {
                        w.put_pending.push(p);
                    }
                    w.events.push(ev(Outcome::Denied, "wrong_code"));
                    w.events
                        .extend(failure_events(&h, failed, &src.sign_in, now));
                    w.credential = Some(next);
                    (
                        w,
                        Err(PwError::CodeFailed {
                            attempts_left: left,
                        }),
                    )
                }
            }
        })
        .await?
    }

    // --- Setup and reset --------------------------------------------------------------------

    /// The setup's first step, from the address `source`: the sign-in name and the setup code.
    /// Every refusal before the code matched is [`PwError::SetupFailed`]. No hash is computed
    /// (the code is a SHA-256 lookup), so the global limit isn't touched.
    pub async fn setup_begin(
        &self,
        store: &dyn PasswordStore,
        name: &str,
        code: &Secret,
        source: Option<&str>,
        now: DateTime<Utc>,
    ) -> PwResult<SetupStarted> {
        // G2: wrong guesses count against the address (per person, in the database) and its
        // block (here, before the name is looked up, so the answer says nothing about it),
        // never against anyone's code. The slot is taken first and given back unless the
        // attempt was a wrong guess.
        //
        // The review of 8e8f8fe: past the block's guesses the attempt is still checked, so a
        // flood of wrong guesses from the invitee's network can't refuse their right code. Over
        // the limit every other outcome before the code matched is `429` (unknown name, wrong,
        // expired or replaced code, a waiting address), whatever the name, and writes nothing:
        // the answer says nothing about the name, as before. What such an attempt can still
        // tell is whether its (name, code) pair is right, which takes a 100-bit code, and, by
        // timing, whether the name exists (one lookup for an unknown name; a lookup and a short
        // read for a real one), as below the limit. `pmos-web` counts each of these `429`s
        // against the address and the name (10 per name and address, 100 per address, per 15
        // minutes). Each is counted on `/readyz` and recorded, thinned: at most one audit row a
        // minute per block, with no target, standing for the refusals since the block's last
        // (the review of 8e8f8fe, a Low: before, none was recorded).
        let block = limits::block_key(source);
        let over = match self.setup_guesses.take(&block, now) {
            limits::Guess::Counted => None,
            limits::Guess::OverLimit(retry_after) => {
                tracing::warn!("setup-code guesses from one network block over the limit");
                Some(retry_after)
            }
        };
        let out = self
            .setup_begin_counted(store, name, code, source, over, now)
            .await;
        if over.is_none() && !matches!(out, Err(PwError::SetupFailed)) {
            self.setup_guesses.give_back(&block, now);
        }
        if over.is_some() && matches!(out, Err(PwError::TooMany { .. })) {
            self.setup_block_refusals.fetch_add(1, Ordering::Relaxed);
            if let Some(n) = self.setup_guesses.note_over_limit(&block, now) {
                store
                    .password_events(vec![event(
                        Client::Web,
                        ANONYMOUS,
                        "password.setup_begin",
                        Outcome::Denied,
                        None,
                        &format!("block_limit; refused {n}"),
                        now,
                    )])
                    .await?;
            }
        }
        out
    }

    /// The setup's first step; `over` is the block's `Retry-After` when it used its guesses
    /// (then a refusal before the code matched is `429` and writes nothing).
    async fn setup_begin_counted(
        &self,
        store: &dyn PasswordStore,
        name: &str,
        code: &Secret,
        source: Option<&str>,
        over: Option<u64>,
        now: DateTime<Utc>,
    ) -> PwResult<SetupStarted> {
        let key = limits::source_key(source);
        let handle = match sign_in_name(name) {
            Some(n) => store.handle_by_sign_in_name(&n).await?,
            None => None,
        };
        let over_limit = |retry_after: u64| PwError::TooMany { retry_after };
        let Some(handle) = handle else {
            if let Some(retry_after) = over {
                return Err(over_limit(retry_after));
            }
            self.setup_wrong_guesses.fetch_add(1, Ordering::Relaxed);
            store
                .password_events(vec![event(
                    Client::Web,
                    ANONYMOUS,
                    "password.setup_begin",
                    Outcome::Denied,
                    None,
                    "unknown_name",
                    now,
                )])
                .await?;
            return Err(PwError::SetupFailed);
        };
        let code_hash = codes::setup_code_hash(code.expose());
        let token = codes::new_token().map_err(|e| PwError::Unavailable(e.into()))?;
        let token_hash = codes::token_hash(&token).ok_or(PwError::Unavailable("token".into()))?;
        let h = handle.clone();
        let begun = tx(store, &handle, None, Some(&key), now, move |s| {
            let mut w = Writes::default();
            let ev = |outcome, detail: &str| {
                web_event(ANONYMOUS, "password.setup_begin", outcome, &h, detail, now)
            };
            let Some(person) = s.person.as_ref() else {
                return (w, Err(over.map_or(PwError::SetupFailed, over_limit)));
            };
            let cred = s.credential.clone().unwrap_or_default();
            if limits::may_try_setup(&s.source, now).is_err() {
                if let Some(retry_after) = over {
                    return (Writes::default(), Err(over_limit(retry_after)));
                }
                w.events.push(ev(Outcome::Denied, "locked"));
                return (w, Err(PwError::SetupFailed));
            }
            let live = s.setup_code.as_ref();
            let matched = match (live, &code_hash) {
                (Some(c), Some(hash)) => codes::same_hash(&c.hash, hash),
                _ => false,
            };
            let right = live.filter(|c| matched && c.expires_at > now);
            if let (None, Some(retry_after)) = (right, over) {
                // Over the block's guesses: only a right code goes on; nothing is written.
                return (Writes::default(), Err(over_limit(retry_after)));
            }
            let Some(c) = right else {
                let why = if matched { "expired" } else { "wrong_code" };
                let mut src = s.source;
                let started_wait = src.setup.fail(now);
                w.events.push(ev(Outcome::Denied, why));
                if started_wait {
                    w.events.push(lockout(&h, &src.setup, true, now));
                }
                // G2: a wrong guess never touches the person's code (or a setup in progress
                // with it): it counts against this address (here) and its block only. The
                // code keeps a plain count of the wrong guesses tried at its person while it
                // was live, which decides nothing.
                if !matched {
                    self.setup_wrong_guesses.fetch_add(1, Ordering::Relaxed);
                    if let Some(live) = live.filter(|c| c.expires_at > now) {
                        let mut code = live.clone();
                        code.wrong_guesses = code.wrong_guesses.saturating_add(1);
                        w.code_counts = Some(code.counts());
                    }
                }
                if h != EVALS {
                    w.source = Some(src);
                }
                return (w, Err(PwError::SetupFailed));
            };
            // The code is right: from here, a refusal can say why.
            let refusal = if !person.active() {
                Some((PwError::NotAMember, "deactivated"))
            } else if h == EVALS {
                Some((PwError::EvalsAccount, "evals_account"))
            } else if person.sign_in_name.is_none() || !person.sign_in_name_confirmed {
                Some((PwError::SignInUnconfirmed, "sign_in_unconfirmed"))
            } else if person.protected() && c.issued_via != Via::Host {
                Some((PwError::HostOnly, "host_only"))
            } else if c.spent() {
                // H1: the code gave all its tokens or password sets.
                Some((PwError::SetupCodeSpent, "spent"))
            } else {
                None
            };
            if let Some((e, detail)) = refusal {
                w.events.push(ev(Outcome::Denied, detail));
                return (w, Err(e));
            }
            let mut src = s.source;
            src.setup.clear();
            w.source = Some(src);
            w.credential = Some(cred.clone());
            w.delete_pending = s
                .pending
                .iter()
                .filter(|p| p.purpose == Purpose::Setup)
                .map(|p| p.token_hash.clone())
                .collect();
            let mut code = c.clone();
            code.begins = code.begins.saturating_add(1);
            w.code_counts = Some(code.counts());
            let expires_at = (now + SETUP_PENDING).min(c.expires_at);
            w.put_pending.push(Pending::new(
                token_hash,
                Purpose::Setup,
                Some(c.id),
                cred.epoch,
                now,
                expires_at,
            ));
            w.events
                .push(ev(Outcome::Ok, &format!("begin; {}", c.kind.as_str())));
            (w, Ok((c.kind, expires_at)))
        })
        .await?;
        let (kind, expires_at) = begun?;
        Ok(SetupStarted {
            token: Secret(token),
            handle,
            kind,
            expires_at,
        })
    }

    /// The setup's password. Checked against the rules, hashed, kept with the setup until it
    /// is confirmed. **Bounded** (the re-check of eb24d28, H1): each call is counted against
    /// the token ([`limits::SETUP_PASSWORD_SETS_PER_TOKEN`], then the token is spent), the code
    /// across its renewals ([`limits::SETUP_PASSWORD_SETS_PER_CODE`], then the code is spent)
    /// and the account ([`limits::ACCOUNT_WORK`]), before the hash, whatever the outcome; the
    /// hash comes from the global limit's open allowance only, never its reserve. The shared
    /// limits are taken first: a `429` from them, or a `503` because every hashing slot was
    /// busy, costs the token and the code nothing (the gate review of 04c175f, G3).
    pub async fn setup_password(
        &self,
        store: &dyn PasswordStore,
        token: &str,
        password: &Secret,
        now: DateTime<Utc>,
    ) -> PwResult<()> {
        // The token first (a cheap lookup): a made-up or spent one costs the global limit
        // nothing.
        let token_hash = codes::token_hash(token).ok_or(PwError::SetupExpired)?;
        let handle = store
            .handle_by_token(&token_hash)
            .await?
            .ok_or(PwError::SetupExpired)?;
        let th = token_hash.clone();
        let h = handle.clone();
        tx(store, &handle, None, None, now, move |s| {
            let mut w = Writes::default();
            let (p, c, person) = match live_setup(s, &th, now) {
                Ok(v) => v,
                Err(e) => return (w, Err(e)),
            };
            // The rules first: a password they refuse costs nothing and isn't counted.
            if let Err(weak) =
                hash::check_new(password.expose(), &h, person.sign_in_name.as_deref())
            {
                return (w, Err(PwError::Weak(weak)));
            }
            if p.password_sets >= limits::SETUP_PASSWORD_SETS_PER_TOKEN {
                w.delete_pending.push(p.token_hash.clone());
                return (w, Err(PwError::SetupExpired));
            }
            if c.spent() {
                w.delete_pending.push(p.token_hash.clone());
                return (w, Err(PwError::SetupCodeSpent));
            }
            // G3: the shared limits first (the account's work, the open allowance), so a
            // refusal by them (`429`) is written nowhere and costs the token and the code
            // nothing. Taken inside the person's lock, so the count below and the hash it
            // pays for are one decision.
            if let Err(e) = self.take_work(&h, now) {
                return (Writes::default(), Err(e));
            }
            // Counted now, before the hash, whatever comes of it (H1).
            let mut p = p.clone();
            p.password_sets += 1;
            w.put_pending.push(p);
            let mut code = c.clone();
            code.password_sets = code.password_sets.saturating_add(1);
            w.code_counts = Some(code.counts());
            (w, Ok(()))
        })
        .await??;
        let hashed = match self.hash(password).await {
            Ok(hashed) => hashed,
            Err(PwError::Busy) => {
                // G3: every hashing slot was busy, so no hash ran: the set goes back to the
                // token, the code and the account (`503` costs the invitee nothing). Only an
                // attempt that computed nothing is given back, so one code still pays for at
                // most its bounded number of hashes.
                self.work.give_back(&handle, now);
                self.give_back_password_set(store, &handle, &token_hash, now)
                    .await;
                return Err(PwError::Busy);
            }
            Err(e) => return Err(e),
        };
        tx(store, &handle, None, None, now, move |s| {
            let mut w = Writes::default();
            match live_setup(s, &token_hash, now) {
                Ok((p, _, _)) => {
                    let mut p = p.clone();
                    p.staged_hash = Some(hashed);
                    w.put_pending.push(p);
                    (w, Ok(()))
                }
                Err(e) => (w, Err(e)),
            }
        })
        .await?
    }

    /// Give back one password set counted for a setup whose hash never ran (G3): on the token
    /// and on its code. Best effort: a failure here leaves the set counted.
    async fn give_back_password_set(
        &self,
        store: &dyn PasswordStore,
        handle: &str,
        token_hash: &str,
        now: DateTime<Utc>,
    ) {
        let r = tx(store, handle, None, None, now, |s| {
            let mut w = Writes::default();
            if let Some(p) = s
                .pending(token_hash)
                .filter(|p| p.purpose == Purpose::Setup)
            {
                let mut p = p.clone();
                p.password_sets = (p.password_sets - 1).max(0);
                if let Some(code) = s
                    .setup_code
                    .as_ref()
                    .filter(|c| Some(c.id) == p.setup_code_id)
                {
                    let mut code = code.clone();
                    code.password_sets = (code.password_sets - 1).max(0);
                    w.code_counts = Some(code.counts());
                }
                w.put_pending.push(p);
            }
            (w, ())
        })
        .await;
        if r.is_err() {
            tracing::warn!("a setup's password set counted ahead of a busy hash wasn't given back");
        }
    }

    /// The setup's authenticator: a new secret (each call replaces the last), shown once.
    pub async fn setup_authenticator(
        &self,
        store: &dyn PasswordStore,
        token: &str,
        now: DateTime<Utc>,
    ) -> PwResult<Enrolment> {
        let token_hash = codes::token_hash(token).ok_or(PwError::SetupExpired)?;
        let handle = store
            .handle_by_token(&token_hash)
            .await?
            .ok_or(PwError::SetupExpired)?;
        let secret = totp::new_secret().map_err(|e| PwError::Unavailable(e.into()))?;
        let sealed = self.seal_totp(&handle, &secret)?;
        let account = tx(store, &handle, None, None, now, move |s| {
            let mut w = Writes::default();
            match live_setup(s, &token_hash, now) {
                Ok((p, _, _)) if p.enrolments >= limits::SETUP_ENROLMENTS_PER_TOKEN => {
                    // Bounded per token too: past it, start again with the code.
                    w.delete_pending.push(p.token_hash.clone());
                    (w, Err(PwError::SetupExpired))
                }
                Ok((p, _, person)) if p.staged_hash.is_some() => {
                    let mut p = p.clone();
                    p.staged_totp = Some(sealed);
                    p.enrolments += 1;
                    w.put_pending.push(p);
                    let account = person
                        .sign_in_name
                        .clone()
                        .unwrap_or_else(|| person.handle.clone());
                    (w, Ok(account))
                }
                Ok(_) => (w, Err(PwError::WrongStep)),
                Err(e) => (w, Err(e)),
            }
        })
        .await??;
        let b32 = Zeroizing::new(totp::base32(&secret));
        Ok(Enrolment {
            uri: Secret::new(totp::uri(&account, &b32)),
            secret: Secret(b32),
            account,
        })
    }

    /// The setup's last step, from the address `source`: a code from the authenticator just
    /// enrolled. The new password, authenticator and ten recovery codes replace whatever the
    /// person had, the setup code is spent, the credential's epoch moves (every session of
    /// theirs ends), and the codes are returned to be shown once. No hash is computed, so the
    /// global limit isn't touched; the token is checked first.
    pub async fn setup_confirm(
        &self,
        store: &dyn PasswordStore,
        token: &str,
        code: &Secret,
        source: Option<&str>,
        now: DateTime<Utc>,
    ) -> PwResult<SetupDone> {
        let key = limits::source_key(source);
        let address = limits::is_address(source);
        let token_hash = codes::token_hash(token).ok_or(PwError::SetupExpired)?;
        let handle = store
            .handle_by_token(&token_hash)
            .await?
            .ok_or(PwError::SetupExpired)?;
        let recovery = codes::new_recovery_codes().map_err(|e| PwError::Unavailable(e.into()))?;
        let hashes: Vec<String> = recovery
            .iter()
            .filter_map(|c| codes::recovery_code_hash(&handle, c))
            .collect();
        let h = handle.clone();
        let done = tx(store, &handle, None, Some(&key), now, move |s| {
            let mut w = Writes::default();
            let ev = |actor: &str, outcome, detail: &str| {
                web_event(actor, "password.setup_complete", outcome, &h, detail, now)
            };
            let (p, c, person) = match live_setup(s, &token_hash, now) {
                Ok(v) => v,
                Err(e) => return (w, Err(e)),
            };
            let (Some(staged_hash), Some(staged_totp)) = (&p.staged_hash, &p.staged_totp) else {
                return (w, Err(PwError::WrongStep));
            };
            // The rules again, in case the person's roles or name changed since the code.
            if person.protected() && c.issued_via != Via::Host {
                w.events.push(ev(ANONYMOUS, Outcome::Denied, "host_only"));
                return (w, Err(PwError::HostOnly));
            }
            if person.sign_in_name.is_none() || !person.sign_in_name_confirmed {
                w.events
                    .push(ev(ANONYMOUS, Outcome::Denied, "sign_in_unconfirmed"));
                return (w, Err(PwError::SignInUnconfirmed));
            }
            let mut cred = s.credential.clone().unwrap_or_default();
            if let Err(until) = limits::may_try_setup(&s.source, now) {
                w.events.push(ev(ANONYMOUS, Outcome::Denied, "locked"));
                return (
                    w,
                    Err(PwError::TooMany {
                        retry_after: limits::retry_after(until, now),
                    }),
                );
            }
            let secret = match self.unseal_totp(&h, staged_totp) {
                Ok(s) => s,
                Err(e) => return (Writes::default(), Err(e)),
            };
            let mut src = s.source;
            let Some(step) = totp::verify(&secret, code.expose(), now.timestamp(), None) else {
                cred.setup_failed.count_only(now);
                let started_wait = src.setup.fail(now);
                let mut p = p.clone();
                p.attempts += 1;
                let left = (SETUP_TRIES - p.attempts).max(0);
                if left == 0 {
                    w.delete_pending.push(p.token_hash.clone());
                } else {
                    w.put_pending.push(p);
                }
                w.events.push(ev(ANONYMOUS, Outcome::Denied, "wrong_code"));
                if started_wait {
                    w.events.push(lockout(&h, &src.setup, true, now));
                }
                w.credential = Some(cred);
                w.source = Some(src);
                return (
                    w,
                    Err(PwError::CodeFailed {
                        attempts_left: left,
                    }),
                );
            };
            let epoch = cred.epoch + 1;
            cred = Credential {
                password_hash: Some(staged_hash.clone()),
                totp_sealed: Some(staged_totp.clone()),
                totp_last_step: Some(step),
                epoch,
                failed: Default::default(),
                setup_failed: Default::default(),
                set_at: Some(now),
                // Who issued the code this credential is set with (PM3).
                issued_via: Some(c.issued_via),
                // A reset notice stays until the person dismisses it (PM2).
                reset_notice: cred.reset_notice.clone(),
            };
            w.credential = Some(cred);
            w.recovery = Some(hashes);
            w.use_code = Some(c.id);
            w.clear_pending = true;
            // Every address's counts start again; this one is the person's own now (only a
            // real address: never the shared key of attempts with none, M1).
            w.reset_sources = true;
            w.source = Some(SourceCounters {
                last_success_at: address.then_some(now),
                ..SourceCounters::default()
            });
            w.changes.push(RoleChange {
                id: 0,
                at: now,
                actor: h.clone(),
                via: Via::Web,
                host_user: None,
                target: h.clone(),
                change: ChangeKind::SetPassword,
                role: None,
                detail: Some(c.kind.as_str().to_string()),
            });
            w.events.push(ev(&h, Outcome::Ok, c.kind.as_str()));
            (w, Ok((c.kind, epoch)))
        })
        .await?;
        let (kind, epoch) = done?;
        Ok(SetupDone {
            handle,
            kind,
            epoch,
            recovery_codes: recovery.into_iter().map(Secret).collect(),
        })
    }

    // --- Signed in --------------------------------------------------------------------------

    /// A signed-in person changes their password: the current one and a fresh authenticator
    /// code. The epoch moves (their other sessions end). A wrong current password or code
    /// counts toward a wait of the person's own re-authentication ([`limits::session_key`]),
    /// apart from every address's.
    pub async fn change(
        &self,
        store: &dyn PasswordStore,
        handle: &str,
        current: &Secret,
        code: &Secret,
        new: &Secret,
        now: DateTime<Utc>,
    ) -> PwResult<i64> {
        let key = limits::session_key();
        let name = tx(store, handle, None, None, now, |s| {
            let r = match (&s.person, &s.credential) {
                (Some(p), _) if !p.active() => Err(PwError::NotAMember),
                (None, _) => Err(PwError::NotFound),
                (Some(_), c) if !c.as_ref().is_some_and(Credential::is_set) => {
                    Err(PwError::NoPassword)
                }
                (Some(p), _) => Ok(p.sign_in_name.clone()),
            };
            (Writes::default(), r)
        })
        .await??;
        if let Err(weak) = hash::check_new(new.expose(), handle, name.as_deref()) {
            return Err(PwError::Weak(weak));
        }
        // LA: a change costs two hashes (the current password, the new one). Both are counted
        // against the account's work and taken from the open allowance before either, so a
        // signed-in person can't spend what everyone shares.
        self.take_work(handle, now)?;
        let block = limits::account_block(handle);
        self.take_global(now, Draw::open(&block))
            .map_err(|retry_after| {
                // The second hash had no room: no work was done, so the piece goes back.
                self.work.give_back(handle, now);
                PwError::TooMany { retry_after }
            })?;
        let h = handle.to_string();
        let reserved = tx(store, handle, None, Some(&key), now, move |s| {
            let mut w = Writes::default();
            let Some(cred) = s.credential.clone().filter(Credential::is_set) else {
                return (w, Err(PwError::NoPassword));
            };
            if let Some(until) = s.source.sign_in.locked(now) {
                w.events.push(web_event(
                    &h,
                    "password.change",
                    Outcome::Denied,
                    &h,
                    "locked",
                    now,
                ));
                return (
                    w,
                    Err(PwError::TooMany {
                        retry_after: limits::retry_after(until, now),
                    }),
                );
            }
            let mut src = s.source;
            let started_wait = src.sign_in.fail(now);
            w.source = Some(src);
            (
                w,
                Ok((
                    cred.password_hash.clone().unwrap_or_default(),
                    cred.epoch,
                    started_wait,
                )),
            )
        })
        .await??;
        let (stored, epoch, started_wait) = reserved;
        // The review of 8e8f8fe: when every hashing slot is busy nothing was checked, so what
        // this call counted goes back: the piece of the account's work and the failure counted
        // ahead (a wait it started stays), as the password step and a setup's password do (G3).
        // Before, ten busy tries during someone else's flood filled the person's own room.
        let right = match self.verify(current, stored.clone()).await {
            Ok(right) => right,
            Err(PwError::Busy) => {
                self.give_back_change(store, handle, now).await;
                return Err(PwError::Busy);
            }
            Err(e) => return Err(e),
        };
        let new_hash = if right {
            match self.hash(new).await {
                Ok(h) => Some(h),
                Err(PwError::Busy) => {
                    // The current password was checked (one hash ran), and was right: the
                    // failure counted ahead goes back; the piece of work stays (work was done).
                    self.give_back_reauth_failure(store, handle, now).await;
                    return Err(PwError::Busy);
                }
                Err(e) => return Err(e),
            }
        } else {
            None
        };
        let h = handle.to_string();
        tx(store, handle, None, Some(&key), now, move |s| {
            let mut w = Writes::default();
            let deny = |mut w: Writes, detail: &str, failed: &limits::Counter| {
                w.events.push(web_event(
                    &h,
                    "password.change",
                    Outcome::Denied,
                    &h,
                    detail,
                    now,
                ));
                if started_wait {
                    w.events.push(lockout(&h, failed, false, now));
                }
                (w, Err(PwError::ReauthFailed))
            };
            let Some(cred) = s
                .credential
                .clone()
                .filter(|c| c.is_set() && c.epoch == epoch)
            else {
                return (w, Err(PwError::ReauthFailed));
            };
            let Some(new_hash) = new_hash else {
                return deny(w, "reauth_failed", &s.source.sign_in);
            };
            let secret = match self.unseal_totp(&h, cred.totp_sealed.as_deref().unwrap_or_default())
            {
                Ok(s) => s,
                Err(e) => return (Writes::default(), Err(e)),
            };
            let Some(step) =
                totp::verify(&secret, code.expose(), now.timestamp(), cred.totp_last_step)
            else {
                return deny(w, "reauth_failed", &s.source.sign_in);
            };
            let mut next = cred.clone();
            next.password_hash = Some(new_hash);
            next.totp_last_step = Some(step);
            next.epoch = cred.epoch + 1;
            next.set_at = Some(now);
            let epoch = next.epoch;
            w.credential = Some(next);
            w.source = Some(SourceCounters::default());
            w.clear_pending = true;
            w.events.push(web_event(
                &h,
                "password.change",
                Outcome::Ok,
                &h,
                "change",
                now,
            ));
            (w, Ok(epoch))
        })
        .await?
    }

    /// Give back what [`Self::change`] counted ahead of a check that never ran: the piece of
    /// the account's work and the re-authentication failure. Best effort, as
    /// [`Self::give_back_failure`].
    async fn give_back_change(&self, store: &dyn PasswordStore, handle: &str, now: DateTime<Utc>) {
        self.work.give_back(handle, now);
        self.give_back_reauth_failure(store, handle, now).await;
    }

    /// Give back one re-authentication failure counted ahead (a count only: a wait stays).
    async fn give_back_reauth_failure(
        &self,
        store: &dyn PasswordStore,
        handle: &str,
        now: DateTime<Utc>,
    ) {
        let key = limits::session_key();
        let r = tx(store, handle, None, Some(&key), now, |s| {
            let mut w = Writes::default();
            let mut src = s.source;
            src.sign_in.give_back();
            w.source = Some(src);
            (w, ())
        })
        .await;
        if r.is_err() {
            tracing::warn!(
                "a re-authentication failure counted ahead of a busy check wasn't given back"
            );
        }
    }

    /// A signed-in person replaces their recovery codes (an authenticator code proves it).
    pub async fn regenerate_recovery_codes(
        &self,
        store: &dyn PasswordStore,
        handle: &str,
        code: &Secret,
        now: DateTime<Utc>,
    ) -> PwResult<Vec<Secret>> {
        // LA: no Argon2 hash here (an authenticator code and ten SHA-256s), so nothing from the
        // global limit; counted against the account's work all the same.
        self.work.take(handle, now).map_err(|retry_after| {
            tracing::warn!("password work for one account over its limit");
            PwError::TooMany { retry_after }
        })?;
        let recovery = codes::new_recovery_codes().map_err(|e| PwError::Unavailable(e.into()))?;
        let hashes: Vec<String> = recovery
            .iter()
            .filter_map(|c| codes::recovery_code_hash(handle, c))
            .collect();
        let key = limits::session_key();
        let h = handle.to_string();
        tx(store, handle, None, Some(&key), now, move |s| {
            let mut w = Writes::default();
            let ev = |outcome, detail: &str| {
                web_event(&h, "password.recovery_codes", outcome, &h, detail, now)
            };
            match &s.person {
                None => return (w, Err(PwError::NotFound)),
                Some(p) if !p.active() => return (w, Err(PwError::NotAMember)),
                Some(_) => {}
            }
            let Some(cred) = s.credential.clone().filter(Credential::is_set) else {
                return (w, Err(PwError::NoPassword));
            };
            if let Some(until) = s.source.sign_in.locked(now) {
                w.events.push(ev(Outcome::Denied, "locked"));
                return (
                    w,
                    Err(PwError::TooMany {
                        retry_after: limits::retry_after(until, now),
                    }),
                );
            }
            let secret = match self.unseal_totp(&h, cred.totp_sealed.as_deref().unwrap_or_default())
            {
                Ok(s) => s,
                Err(e) => return (Writes::default(), Err(e)),
            };
            let mut next = cred.clone();
            let mut src = s.source;
            match totp::verify(&secret, code.expose(), now.timestamp(), cred.totp_last_step) {
                Some(step) => {
                    next.totp_last_step = Some(step);
                    w.credential = Some(next);
                    w.recovery = Some(hashes);
                    w.events.push(ev(Outcome::Ok, "regenerated"));
                    (w, Ok(()))
                }
                None => {
                    let started_wait = src.sign_in.fail(now);
                    w.events.push(ev(Outcome::Denied, "reauth_failed"));
                    if started_wait {
                        w.events.push(lockout(&h, &src.sign_in, false, now));
                    }
                    w.source = Some(src);
                    (w, Err(PwError::ReauthFailed))
                }
            }
        })
        .await??;
        Ok(recovery.into_iter().map(Secret).collect())
    }

    /// The person dismisses the notice of a reset someone issued for them (PM2). Nothing to
    /// dismiss is not an error.
    pub async fn dismiss_reset_notice(
        &self,
        store: &dyn PasswordStore,
        handle: &str,
        now: DateTime<Utc>,
    ) -> PwResult<()> {
        let h = handle.to_string();
        tx(store, handle, None, None, now, move |s| {
            let mut w = Writes::default();
            match &s.person {
                None => return (w, Err(PwError::NotFound)),
                Some(p) if !p.active() => return (w, Err(PwError::NotAMember)),
                Some(_) => {}
            }
            if let Some(mut cred) = s.credential.clone().filter(|c| c.reset_notice.is_some()) {
                cred.reset_notice = None;
                w.credential = Some(cred);
                // LF: recorded as a role change (who, when), so the admin overview shows the
                // reset as dismissed, by this account's own session, from the database by
                // time, whoever is holding the account.
                w.changes.push(RoleChange {
                    id: 0,
                    at: now,
                    actor: h.clone(),
                    via: Via::Web,
                    host_user: None,
                    target: h.clone(),
                    change: ChangeKind::DismissResetNotice,
                    role: None,
                    detail: Some("reset".into()),
                });
                w.events.push(web_event(
                    &h,
                    "password.reset_notice",
                    Outcome::Ok,
                    &h,
                    "dismissed",
                    now,
                ));
            }
            (w, Ok(()))
        })
        .await?
    }
}

/// Issue a setup (or reset) code for `target`: by an active Admin in the web app
/// ([`Actor::web`]; never for themselves, never for an Admin or Auditor) or on the host
/// ([`Actor::host`]; for anyone but `evals`). The live code, if any, is replaced; the code
/// lives [`SETUP_CODE_TTL`]. Recorded in `role_changes` (`issue_password_code`) and the audit
/// log, allowed or refused. The code is returned to be shown once; only its hash is stored.
pub async fn issue_setup_code(
    store: &dyn PasswordStore,
    actor: &Actor,
    target: &str,
    now: DateTime<Utc>,
) -> PwResult<IssuedCode> {
    let code = codes::new_setup_code().map_err(|e| PwError::Unavailable(e.into()))?;
    let code_hash =
        codes::setup_code_hash(&code).ok_or(PwError::Unavailable("setup code".into()))?;
    let web = actor.via == Via::Web;
    let client = if actor.via == Via::Host {
        Client::Host
    } else {
        Client::Web
    };
    let host_part = actor
        .host_user
        .as_ref()
        .map(|h| format!("host_user {}; ", h.as_str()))
        .unwrap_or_default();
    let acting = actor.clone();
    let t = target.to_string();
    let issued = tx(
        store,
        target,
        web.then_some(actor.name.as_str()),
        None,
        now,
        move |s| {
            let mut w = Writes::default();
            let ev = |outcome, detail: &str| {
                event(
                    client,
                    &acting.name,
                    "password.setup_code",
                    outcome,
                    Some(&t),
                    &format!("{host_part}{detail}"),
                    now,
                )
            };
            let not_admin = web && !s.actor.as_ref().is_some_and(Person::is_admin);
            let refusal = if !matches!(acting.via, Via::Web | Via::Host) || not_admin {
                Some(PwError::Forbidden)
            } else if web && acting.name == t {
                Some(PwError::SelfChange)
            } else {
                match &s.person {
                    None => Some(PwError::NotFound),
                    Some(_) if t == EVALS => Some(PwError::EvalsAccount),
                    Some(p) if !p.active() => Some(PwError::NotActive),
                    Some(p) if p.sign_in_name.is_none() || !p.sign_in_name_confirmed => {
                        Some(PwError::SignInUnconfirmed)
                    }
                    Some(p) if web && p.protected() => Some(PwError::HostOnly),
                    Some(_) => None,
                }
            };
            if let Some(e) = refusal {
                w.events.push(ev(Outcome::Denied, e.code()));
                return (w, Err(e));
            }
            let mut cred = s.credential.clone().unwrap_or_default();
            let kind = if cred.is_set() {
                CodeKind::Reset
            } else {
                CodeKind::Setup
            };
            cred.setup_failed.clear();
            w.reset_source_setup = true;
            if kind == CodeKind::Reset {
                // Never silent (PM2): the person sees who reset their sign-in and when, at
                // their next sign-in and on their security page, until they dismiss it. Their
                // old credential keeps working until the code is used.
                cred.reset_notice = Some(ResetNotice {
                    by: acting.name.clone(),
                    via: acting.via,
                    host_user: acting.host_user.as_ref().map(|h| h.as_str().to_string()),
                    at: now,
                });
            }
            w.credential = Some(cred);
            let expires_at = now + SETUP_CODE_TTL;
            w.issue_code = Some(NewSetupCode {
                hash: code_hash,
                kind,
                issued_by: acting.name.clone(),
                issued_via: acting.via,
                expires_at,
            });
            w.delete_pending = s
                .pending
                .iter()
                .filter(|p| p.purpose == Purpose::Setup)
                .map(|p| p.token_hash.clone())
                .collect();
            w.changes.push(RoleChange {
                id: 0,
                at: now,
                actor: acting.name.clone(),
                via: acting.via,
                host_user: acting.host_user.as_ref().map(|h| h.as_str().to_string()),
                target: t.clone(),
                change: ChangeKind::IssuePasswordCode,
                role: None,
                detail: Some(kind.as_str().to_string()),
            });
            w.events.push(ev(Outcome::Ok, kind.as_str()));
            (w, Ok((kind, expires_at)))
        },
    )
    .await?;
    let (kind, expires_at) = issued?;
    Ok(IssuedCode {
        code: Secret(code),
        handle: target.to_string(),
        kind,
        expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_details_are_fixed_words_and_counts() {
        for ok in [
            "password",
            "password; rehashed",
            "recovery_code; recovery_left 9",
            "failures 5; setup",
            "host_user raj; reset",
            "begin; setup",
        ] {
            assert!(is_safe_detail(ok), "{ok}");
        }
        for bad in [
            "",
            "ana@example.com",
            "failures lots",
            "host_user Raj",
            "password; hunter2hunter2",
            "123456",
        ] {
            assert!(!is_safe_detail(bad), "{bad}");
        }
    }

    #[test]
    fn errors_carry_no_value_and_a_fixed_status() {
        assert_eq!(PwError::SignInFailed.status(), 401);
        assert_eq!(PwError::TooMany { retry_after: 3 }.status(), 429);
        assert_eq!(PwError::Weak(Weak::Common).code(), "weak_password");
        assert_eq!(
            format!("{:?}", Secret::new("hunter2hunter2")),
            "Secret(<redacted>)"
        );
    }

    /// L12: no type that holds a hash, a sealed secret or a token's hash prints it.
    #[test]
    fn debug_never_prints_a_hash_or_a_sealed_secret() {
        let now = Utc::now();
        let (hash, sealed, token, staged, code, recovery) = (
            "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g",
            "v1.k1.c2VhbGVkLXNlY3JldA",
            "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0",
            "$argon2id$v=19$m=19456,t=2,p=1$c3RhZ2Vk$c3RhZ2VkaGFzaA",
            "aa11bb22cc33dd44ee55ff6600778899aa11bb22cc33dd44ee55ff6600778899",
            "1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd",
        );
        let cred = Credential {
            password_hash: Some(hash.into()),
            totp_sealed: Some(sealed.into()),
            totp_last_step: Some(1),
            epoch: 2,
            set_at: Some(now),
            issued_via: Some(Via::Host),
            ..Default::default()
        };
        let pending = Pending {
            token_hash: token.into(),
            purpose: Purpose::Setup,
            setup_code_id: Some(1),
            epoch: 2,
            created_at: now,
            expires_at: now,
            attempts: 0,
            staged_hash: Some(staged.into()),
            staged_totp: Some(sealed.into()),
            password_sets: 1,
            enrolments: 1,
        };
        let setup = store::SetupCode {
            id: 1,
            hash: code.into(),
            kind: CodeKind::Reset,
            issued_at: now,
            issued_by: "ada".into(),
            issued_via: Via::Web,
            expires_at: now,
            begins: 1,
            password_sets: 1,
            wrong_guesses: 0,
        };
        let new_code = NewSetupCode {
            hash: code.into(),
            kind: CodeKind::Setup,
            issued_by: "ada".into(),
            issued_via: Via::Web,
            expires_at: now,
        };
        let rec = store::RecoveryCode {
            hash: recovery.into(),
            used_at: None,
        };
        let writes = Writes {
            credential: Some(cred.clone()),
            recovery: Some(vec![recovery.into()]),
            use_recovery: Some(recovery.into()),
            issue_code: Some(new_code.clone()),
            put_pending: vec![pending.clone()],
            delete_pending: vec![token.into()],
            ..Default::default()
        };
        let snapshot = Snapshot {
            credential: Some(cred.clone()),
            recovery: vec![rec.clone()],
            setup_code: Some(setup.clone()),
            pending: vec![pending.clone()],
            ..Default::default()
        };
        let printed = [
            format!("{cred:?}"),
            format!("{pending:?}"),
            format!("{setup:?}"),
            format!("{new_code:?}"),
            format!("{rec:?}"),
            format!("{writes:?}"),
            format!("{snapshot:?}"),
            format!("{cred:#?}{writes:#?}{snapshot:#?}"),
        ]
        .join("\n");
        for secret in [
            hash, sealed, token, staged, code, recovery, "argon2", "v1.k1",
        ] {
            assert!(!printed.contains(secret), "Debug printed {secret}");
        }
        assert!(printed.contains("<redacted>") && printed.contains("epoch: 2"));
    }
}
