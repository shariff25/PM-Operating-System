//! Password sign-in's routes (`docs/web-api.md`, "Password sign-in"; `docs/password-sign-in.md`):
//! on with `PMOS_WEB_PASSWORD_LOGIN=1`, `404` otherwise.
//!
//! **The tool server decides; `pmos-web` relays.** It never stores or sees a password hash, an
//! authenticator secret or a recovery code's hash: it forwards what the person typed to the
//! tool server's `/internal/password/…` routes with its own token (and, for a signed-in
//! person, their user assertion) and acts on the decision. What it keeps itself:
//!
//! - **The tokens between steps** in two cookies the page can't read: `__Host-pmos-pending`
//!   (after a correct password, for the second step; five minutes) and `__Host-pmos-setup` (a
//!   setup in progress; fifteen minutes). `Secure; HttpOnly; SameSite=Strict; Path=/`.
//! - **The session**, exactly as every other sign-in makes it ([`super::start_session`]), with
//!   the credential's epoch: every request checks it against the user database, so a reset or
//!   a password change ends it within 10 seconds (at once in this server).
//! - **A per-address and per-subject limit** ([`SourceLimiter`], the review of 8e8f8fe, N2):
//!   at most [`SUBJECT_REFUSALS`] refused attempts from one address (an IPv4 address, or an
//!   IPv6 /64) about one [`Subject`] (a sign-in name; one pending or setup token; one signed-in
//!   person; or nothing readable) in any [`SOURCE_WINDOW_MINUTES`] minutes, then that subject
//!   answers `429 too_many_attempts` from there; and at most [`SOURCE_REFUSALS`] from the
//!   address about anything, then every password route from it does. A refusal for a full
//!   subject is not counted against the address, so a few people's mistakes behind one shared
//!   address (an office's NAT) never lock the others out: that takes refusals about at least
//!   ten different subjects. **Every** attempt that doesn't succeed counts: a wrong password, code or setup code, an
//!   unknown or expired token, a step out of order, a body that isn't the route's JSON. Only
//!   PM OS's own failures (a `5xx`) don't, and nor does a `429` from a limit the tool server
//!   shares (on the password step and a setup's password: the global allowance, its block
//!   shares, the account's room; the review of 8e8f8fe, M4). The check and the count are one
//!   step: each attempt takes its slot before the tool server is asked and gives it back if it
//!   succeeds.
//! - **The address** ([`source`]): the peer, or behind a proxy named in
//!   `PMOS_WEB_TRUSTED_PROXIES`, `X-Forwarded-For` read strictly from the right. With no
//!   trusted proxy (`PMOS_WEB_DIRECT=1`, a loopback bind), a password request that carries a
//!   forwarded header is refused: a proxy nobody named is in front. The address goes to the
//!   tool server too, whose per-account waits are per address.
//!
//! Every write takes the origin check and JSON (a login CSRF defence, as development sign-in
//! does); the signed-in ones take the session's CSRF token too. Bodies are at most
//! [`MAX_BODY`]. Refusals keep the tool server's code with `pmos-web`'s own sentence
//! ([`PASSWORD_CODES`]). Nothing here logs a password, a code, a token, a secret, a sign-in
//! name or an address.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::Mutex;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use pmos_contract::permissions::Permission;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{
    auth, bad_content_type, is_json, now, parse, parse_optional, start_session, ApiError,
    ApiResult, AppState, Signed,
};
use crate::upstream::UpstreamError;

/// Refused attempts from one address, about anything, before every password route from it
/// waits (N2: a ceiling well above what a busy office's typos come to; it was 20 for
/// everything, so twenty mistakes behind one NAT shut everyone there out for 15 minutes).
pub const SOURCE_REFUSALS: usize = 100;
/// Refused attempts from one address about one [`Subject`] before that subject waits from
/// there. A subject's refusals past it are answered `429` and not counted against the address,
/// so one subject adds at most this many to [`SOURCE_REFUSALS`].
pub const SUBJECT_REFUSALS: usize = 10;
/// The window those are counted in.
pub const SOURCE_WINDOW_MINUTES: i64 = 15;
/// Addresses tracked at once; past it the one seen longest ago goes.
pub const MAX_SOURCES: usize = 10_000;
/// (Address, subject) pairs tracked at once; past it the idle ones go, then the one seen
/// longest ago (the address's own count, which bounds spraying, is kept apart).
pub const MAX_SUBJECTS: usize = 50_000;
/// The largest body a password route reads (the tool server's limit too).
pub const MAX_BODY: usize = 4 * 1024;

/// The cookie that carries the pending token after a correct password.
pub const PENDING_COOKIE: &str = "__Host-pmos-pending";
/// The cookie that carries a setup in progress.
pub const SETUP_COOKIE: &str = "__Host-pmos-setup";
/// How long each cookie lives (the tool server's token lives as long, or less).
const PENDING_SECS: i64 = 5 * 60;
const SETUP_SECS: i64 = 15 * 60;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/auth/password/sign-in", post(sign_in))
        .route("/auth/password/second-step", post(second_step))
        .route("/auth/password/setup/begin", post(setup_begin))
        .route("/auth/password/setup/password", post(setup_password))
        .route(
            "/auth/password/setup/authenticator",
            post(setup_authenticator),
        )
        .route("/auth/password/setup/confirm", post(setup_confirm))
        .route("/auth/password/change", post(change))
        .route("/auth/password/recovery-codes", post(recovery_codes))
        .route("/auth/password/status", get(status))
        .route(
            "/auth/password/reset-notice/dismiss",
            post(dismiss_reset_notice),
        )
        .route(
            "/admin/users/{handle}/password-setup-code",
            post(issue_setup_code),
        )
        .layer(DefaultBodyLimit::max(MAX_BODY))
}

/// The tool server's codes on these routes, the status `pmos-web` answers with, and its own
/// sentence. Any other code is `502 upstream_unavailable`.
pub const PASSWORD_CODES: [(&str, u16, &str); 24] = [
    ("bad_request", 400, "That request wasn't in a shape PM OS takes."),
    ("sign_in_failed", 401, "That sign-in name and password didn't work. Check them and try again, or wait a few minutes."),
    ("code_failed", 401, "That code didn't work. Try the newest code from your authenticator app."),
    ("sign_in_expired", 401, "That sign-in took too long or was used up. Start again with your password."),
    ("setup_failed", 401, "That sign-in name and setup code didn't work. Check them, or ask an admin for a new code."),
    ("setup_expired", 401, "That setup took too long. Start again with your setup code."),
    ("reauth_failed", 403, "Your current password or the authenticator code wasn't right. Nothing changed."),
    ("not_a_member", 403, "Your access to PM OS is turned off. A platform owner can turn it back on."),
    ("forbidden", 403, "Only an active admin can make setup codes. Nothing changed."),
    ("host_only", 403, "An Admin's or Auditor's setup code comes from someone with host access (pmos-admin users password-setup). Nothing changed."),
    ("self_change", 403, "Nobody makes a setup code for themselves. Ask another admin. Nothing changed."),
    ("not_found", 404, "There's no such person in PM OS. Nothing changed."),
    ("wrong_step", 409, "Do the set-up steps in order: password, then authenticator, then the code."),
    ("setup_code_spent", 409, "That setup code was used for as many tries as it allows. Ask an admin for a new one."),
    ("no_password", 409, "You don't have a PM OS password yet. Nothing changed."),
    ("sign_in_unconfirmed", 409, "An admin needs to confirm this person's sign-in name first. Nothing changed."),
    ("evals_account", 409, "The evals account never has a password. Nothing changed."),
    ("not_active", 409, "That person is deactivated. Nothing changed."),
    ("weak_password", 422, "Choose another password: at least 12 characters, not your name or sign-in name, and not a common password."),
    ("too_many_attempts", 429, "Too many tries. Wait a few minutes, then try again."),
    ("not_configured", 501, "Password sign-in isn't set up on this PM OS. Nothing changed."),
    ("sign_in_busy", 503, "Sign-in is busy right now. Try again in a moment."),
    ("assertion_required", 503, "Password sign-in needs signed requests, and this PM OS isn't running with them. Nothing changed."),
    ("unavailable", 503, "PM OS storage isn't answering. Try again in a minute."),
];

// --- The per-address limit ---------------------------------------------------------------------

/// The key an address is counted under: an IPv4 address, or an IPv6 address's /64
/// (`2001:db8:1:2::/64`), so one host can't step through the addresses of its own network (L3).
/// `None` when the server has no peer address (tests that serve without it).
pub type SourceKey = Option<String>;

/// The /64 an IPv6 address is in, or the IPv4 address (an IPv4-mapped one as IPv4).
pub fn source_key(ip: Option<IpAddr>) -> SourceKey {
    ip.map(|ip| match canonical(ip) {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => {
            let s = v6.segments();
            format!("{}/64", Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
        }
    })
}

/// What an attempt is about, kept only as a SHA-256 (never the name or token itself): a
/// sign-in name (trimmed and lower-cased, as the tool server reads it; the password step and a
/// setup's first step share it), one pending or setup token, one signed-in person, or
/// [`Subject::none`] when the request carries nothing readable (a body that isn't the route's
/// JSON, no step cookie).
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Subject([u8; 32]);

impl std::fmt::Debug for Subject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Subject(..)")
    }
}

impl Subject {
    fn of(kind: &str, value: &str) -> Self {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"pmos-web-password-subject\0");
        h.update(kind.as_bytes());
        h.update(b"\0");
        h.update(value.as_bytes());
        Self(h.finalize().into())
    }

    /// A sign-in name, as the tool server reads it.
    pub fn name(sign_in_name: &str) -> Self {
        Self::of("name", &sign_in_name.trim().to_lowercase())
    }

    /// One step token (`pending` or `setup`).
    pub fn token(kind: &str, token: &str) -> Self {
        Self::of(kind, token)
    }

    /// One signed-in person (their `change` and `recovery-codes` calls).
    pub fn user(handle: &str) -> Self {
        Self::of("user", handle)
    }

    /// Nothing readable.
    pub fn none() -> Self {
        Self::of("none", "")
    }
}

/// One attempt's slots in the per-address and per-subject counts ([`SourceLimiter::begin`]).
pub struct Attempt {
    key: SourceKey,
    subject: Subject,
    at: DateTime<Utc>,
}

#[derive(Default)]
struct Seen {
    by_address: Recent<SourceKey>,
    by_subject: Recent<(SourceKey, Subject)>,
}

/// A key's place in [`Recent::order`]: its latest time and a tie-breaking sequence number.
type Place = (DateTime<Utc>, u64);

/// Each key's times in the window, and the keys in the order of their latest time, so making
/// room for a new key never scans them all under the lock (the 50,000-pair map did, review of
/// 2efb73f): the idle keys and the one seen longest ago sit at the ends of `order`.
struct Recent<K> {
    /// A key's times (as pushed; the latest is the last pushed, as `back()` gives it), and its
    /// place in `order`: that time and a tie-breaking sequence number. `None` only while its
    /// times are empty, when the key is in `empty` instead.
    times: HashMap<K, (VecDeque<DateTime<Utc>>, Option<Place>)>,
    order: BTreeMap<Place, K>,
    /// Keys whose times [`room`] emptied: idle, so the first to go.
    empty: HashSet<K>,
    next: u64,
}

impl<K> Default for Recent<K> {
    fn default() -> Self {
        Self {
            times: HashMap::new(),
            order: BTreeMap::new(),
            empty: HashSet::new(),
            next: 0,
        }
    }
}

impl<K: Clone + Eq + std::hash::Hash> Recent<K> {
    fn len(&self) -> usize {
        self.times.len()
    }

    fn contains(&self, key: &K) -> bool {
        self.times.contains_key(key)
    }

    /// Puts `key` back in `order` by its latest time, after its times changed.
    fn reindex(&mut self, key: &K) {
        let Some((times, place)) = self.times.get_mut(key) else {
            return;
        };
        if let Some(old) = place.take() {
            self.order.remove(&old);
        }
        match times.back().copied() {
            Some(last) => {
                self.next += 1;
                let new = (last, self.next);
                *place = Some(new);
                self.order.insert(new, key.clone());
                self.empty.remove(key);
            }
            None => {
                self.empty.insert(key.clone());
            }
        }
    }

    fn remove(&mut self, key: &K) {
        if let Some((_, place)) = self.times.remove(key) {
            if let Some(p) = place {
                self.order.remove(&p);
            }
            self.empty.remove(key);
        }
    }

    /// [`room`] for `key`'s times, when it has any.
    fn room(
        &mut self,
        key: &K,
        most: usize,
        now: DateTime<Utc>,
        window: Duration,
    ) -> Result<(), u64> {
        let Some((times, _)) = self.times.get_mut(key) else {
            return Ok(());
        };
        let r = room(times, most, now, window);
        self.reindex(key);
        r
    }

    fn push(&mut self, key: K, at: DateTime<Utc>) {
        self.times
            .entry(key.clone())
            .or_insert_with(|| (VecDeque::new(), None))
            .0
            .push_back(at);
        self.reindex(&key);
    }

    /// Takes back one time `at` of `key`'s (its latest such), and the key once it has none.
    fn give_back(&mut self, key: &K, at: DateTime<Utc>) {
        let Some((times, _)) = self.times.get_mut(key) else {
            return;
        };
        if let Some(i) = times.iter().rposition(|t| *t == at) {
            times.remove(i);
        }
        if times.is_empty() {
            self.remove(key);
        } else {
            self.reindex(key);
        }
    }

    /// Make room for a new key (at most `most` keys): when full, every idle key goes (no times
    /// left, or its latest time outside the window or after `now`), and if that frees nothing,
    /// the one whose latest time is oldest. The rule the scan applied, found at the ends of
    /// `order` instead: each key leaves `order` at most once per time it was put there.
    fn evict(&mut self, most: usize, now: DateTime<Utc>, window: Duration) {
        if self.len() < most {
            return;
        }
        for k in std::mem::take(&mut self.empty) {
            self.times.remove(&k);
        }
        while let Some((&(last, _), _)) = self.order.first_key_value() {
            if now - last < window {
                break;
            }
            if let Some((_, k)) = self.order.pop_first() {
                self.times.remove(&k);
            }
        }
        while let Some((&(last, _), _)) = self.order.last_key_value() {
            if last <= now {
                break;
            }
            if let Some((_, k)) = self.order.pop_last() {
                self.times.remove(&k);
            }
        }
        if self.len() >= most {
            if let Some((_, k)) = self.order.pop_first() {
                self.times.remove(&k);
            }
        }
    }
}

/// Refused attempts per address, and per address and subject, in a rolling window. In this
/// process only.
#[derive(Default)]
pub struct SourceLimiter {
    seen: Mutex<Seen>,
}

/// Times in the window at `now`; `Err(retry_after_secs)` when there are `most` of them.
fn room(
    times: &mut VecDeque<DateTime<Utc>>,
    most: usize,
    now: DateTime<Utc>,
    window: Duration,
) -> Result<(), u64> {
    times.retain(|t| now - *t < window && *t <= now);
    if times.len() >= most {
        let oldest = times.front().copied().unwrap_or(now);
        let wait = (oldest + window - now).num_seconds().max(1);
        return Err(u64::try_from(wait).unwrap_or(1));
    }
    Ok(())
}

impl SourceLimiter {
    fn window() -> Duration {
        Duration::minutes(SOURCE_WINDOW_MINUTES)
    }

    /// Check `source` and `subject` and take a slot in both for this attempt at `now`, in one
    /// step (L2): concurrent attempts can't pass either limit together. `Err(retry_after_secs)`
    /// when the address has used its [`SOURCE_REFUSALS`], or the subject its
    /// [`SUBJECT_REFUSALS`] from that address; then nothing is taken, so a full subject never
    /// adds to the address's count (N2). The slots count as a refusal unless the attempt is
    /// [`Self::settle`]d as not counted.
    pub fn begin(
        &self,
        source: &SourceKey,
        subject: &Subject,
        now: DateTime<Utc>,
    ) -> Result<Attempt, u64> {
        let window = Self::window();
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        let s = &mut *seen;
        s.by_address.room(source, SOURCE_REFUSALS, now, window)?;
        let pair = (source.clone(), subject.clone());
        s.by_subject.room(&pair, SUBJECT_REFUSALS, now, window)?;
        if !s.by_address.contains(source) {
            s.by_address.evict(MAX_SOURCES, now, window);
        }
        if !s.by_subject.contains(&pair) {
            s.by_subject.evict(MAX_SUBJECTS, now, window);
        }
        s.by_address.push(source.clone(), now);
        s.by_subject.push(pair, now);
        Ok(Attempt {
            key: source.clone(),
            subject: subject.clone(),
            at: now,
        })
    }

    /// The attempt's outcome: `counted` keeps its slots (a refusal); otherwise (it succeeded,
    /// or PM OS itself failed) both are given back.
    pub fn settle(&self, attempt: Attempt, counted: bool) {
        if counted {
            return;
        }
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        seen.by_address.give_back(&attempt.key, attempt.at);
        seen.by_subject
            .give_back(&(attempt.key, attempt.subject), attempt.at);
    }
}

/// An IPv4-mapped IPv6 address as IPv4; anything else as it is.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

/// One `X-Forwarded-For` entry, as raw bytes: an address, `ip:port`, `[ipv6]` or
/// `[ipv6]:port`. `None` for anything else (`unknown`, an obfuscated name, garbage, bytes that
/// aren't text).
fn forwarded_entry(entry: &[u8]) -> Option<IpAddr> {
    let e = std::str::from_utf8(entry).ok()?.trim();
    if let Some(rest) = e.strip_prefix('[') {
        let (inside, after) = rest.split_once(']')?;
        let port_ok = after.is_empty()
            || after
                .strip_prefix(':')
                .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
        return port_ok
            .then(|| inside.parse::<Ipv6Addr>().ok().map(IpAddr::V6))
            .flatten();
    }
    if let Ok(ip) = e.parse::<IpAddr>() {
        return Some(ip);
    }
    e.parse::<SocketAddr>().ok().map(|s| s.ip())
}

/// Why a password request's address can't be known (the re-check of eb24d28, M1): pmos-web
/// refuses it (`403 proxy_not_trusted`) rather than count the browser as some proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsourced {
    /// A forwarded header from a peer that isn't a trusted proxy while some are named: the
    /// setting is wrong or stale (the proxy's address moved), and every browser would share
    /// one count.
    UntrustedForwarder,
    /// A trusted proxy that named no client (no `X-Forwarded-For`, only proxies in it, or an
    /// unreadable entry right after the trusted hops): the address would be a proxy's.
    NoClient,
}

/// Where a request came from (L1, LI, M1): the peer; or, when the peer is a trusted proxy,
/// `X-Forwarded-For` read strictly from the right, every header in order, **entry by entry on
/// the raw bytes** (an entry that isn't text spoils only itself, not its header: nginx's
/// appending `$proxy_add_x_forwarded_for` puts the client's own claim on the left of the
/// address nginx saw). Each entry that is a trusted proxy is passed; the first that isn't, and
/// can be read, is the client. An entry that can't be read ends the walk (it is never
/// skipped), and so does running out of entries: what was read is then only trusted hops, so
/// the answer is [`Unsourced::NoClient`], never a proxy's own address. A peer that isn't a
/// trusted proxy is the client, unless it sends a forwarded header while proxies are named
/// ([`Unsourced::UntrustedForwarder`]). `Ok(None)` when the server has no peer address (tests
/// that serve without it): every such request shares one count, which the tool server never
/// takes for anyone's own address.
pub fn source(
    parts_peer: Option<SocketAddr>,
    headers: &HeaderMap,
    trusted: &[IpAddr],
) -> Result<Option<IpAddr>, Unsourced> {
    let Some(peer) = parts_peer.map(|p| canonical(p.ip())) else {
        return Ok(None);
    };
    let is_trusted = |ip: IpAddr| trusted.iter().any(|t| canonical(*t) == ip);
    if !is_trusted(peer) {
        if !trusted.is_empty() && forwarded(headers) {
            return Err(Unsourced::UntrustedForwarder);
        }
        return Ok(Some(peer));
    }
    let mut entries: Vec<Option<IpAddr>> = Vec::new();
    for v in headers.get_all("x-forwarded-for") {
        entries.extend(v.as_bytes().split(|b| *b == b',').map(forwarded_entry));
    }
    for entry in entries.into_iter().rev() {
        match entry.map(canonical) {
            Some(ip) if is_trusted(ip) => continue,
            Some(ip) => return Ok(Some(ip)),
            None => break,
        }
    }
    Err(Unsourced::NoClient)
}

/// Does the request carry a header a proxy adds? (With no trusted proxy, a password request
/// that does is refused: a proxy nobody named is in front, and every browser would share its
/// address.)
fn forwarded(headers: &HeaderMap) -> bool {
    ["x-forwarded-for", "forwarded", "x-real-ip"]
        .iter()
        .any(|h| headers.contains_key(*h))
}

// --- Cookies -----------------------------------------------------------------------------------

fn cookie(name: &str, value: &str, secs: i64) -> String {
    format!("{name}={value}; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age={secs}")
}

fn clear(name: &str) -> String {
    format!("{name}=; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=0")
}

/// A token cookie's value: 43 base64url characters, or nothing.
fn read_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(n, _)| *n == name)
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| {
            v.len() == 43
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
}

fn with_cookies(mut res: Response, cookies: &[String]) -> Response {
    for c in cookies {
        if let Ok(v) = HeaderValue::from_str(c) {
            res.headers_mut().append(header::SET_COOKIE, v);
        }
    }
    res
}

// --- Answers -----------------------------------------------------------------------------------

fn off() -> ApiError {
    ApiError::not_found()
}

/// The tool server's refusal, as `pmos-web` answers it, with `attempts_left`, `reason` or
/// `Retry-After` carried over.
fn refused(status: u16, body: &Value, retry_after: Option<HeaderValue>) -> Response {
    let code = body.get("code").and_then(Value::as_str).unwrap_or_default();
    let Some((code, status, sentence)) = PASSWORD_CODES.iter().find(|(c, ..)| *c == code) else {
        tracing::warn!(
            status,
            "the tool server refused a password call with an unknown code"
        );
        return ApiError::upstream(UpstreamError::Refused {
            service: "tool server",
            status,
            code: None,
            error: None,
            audience: None,
        })
        .into_response();
    };
    let mut out = json!({ "code": code, "error": sentence });
    if let Some(n) = body.get("attempts_left").and_then(Value::as_i64) {
        out["attempts_left"] = json!(n);
    }
    if let Some(r) = body
        .get("reason")
        .and_then(Value::as_str)
        .filter(|r| ["too_short", "too_long", "matches_name", "common"].contains(r))
    {
        out["reason"] = json!(r);
    }
    let mut res = (
        StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_REQUEST),
        Json(out),
    )
        .into_response();
    if let Some(v) = retry_after {
        res.headers_mut().insert(header::RETRY_AFTER, v);
    }
    res
}

fn too_many(retry_after: u64) -> Response {
    let mut res = refused(
        429,
        &json!({ "code": "too_many_attempts" }),
        HeaderValue::from_str(&retry_after.to_string()).ok(),
    );
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

/// What the tool server answered: its status, body and `Retry-After`.
struct Answer {
    status: u16,
    body: Value,
    retry_after: Option<HeaderValue>,
}

/// An unauthenticated call (no one is signed in yet). The tool server unreachable: fail
/// closed, `503 users_unavailable`.
async fn call(state: &AppState, segments: &[&str], body: &Value) -> Result<Answer, ApiError> {
    match state
        .0
        .toolserver
        .password_call(reqwest::Method::POST, segments, None, Some(body))
        .await
    {
        Ok((status, body, retry_after)) => Ok(Answer {
            status,
            body,
            retry_after,
        }),
        Err(e) => {
            tracing::warn!(error = ?e, "password sign-in: the tool server didn't answer");
            Err(super::users_unavailable())
        }
    }
}

/// An unauthenticated attempt in progress: the address it is counted against (sent to the tool
/// server as `source`), and its slot in the per-address count.
struct Try {
    from: SourceKey,
    attempt: Option<Attempt>,
}

impl Try {
    /// The address, for the tool server's per-account waits.
    fn source(&self) -> Value {
        json!(self.from)
    }

    /// The attempt succeeded, or PM OS failed: the slot goes back.
    fn not_counted(&mut self, state: &AppState) {
        if let Some(a) = self.attempt.take() {
            state.0.sources.settle(a, false);
        }
    }

    /// After the tool server answered: counted unless it succeeded (`2xx`), PM OS itself
    /// failed (`5xx`), or a limit the tool server shares refused it before any check (`429`
    /// on the password step: the global hashing allowance, its block shares, or the
    /// account's room; the review of 8e8f8fe, M4: before, a busy office's `429`s used up its
    /// address's twenty refusals and locked it out for 15 minutes). Every other refusal
    /// counts: a wrong guess, an unknown or expired token, a step out of order, a body it
    /// couldn't read (P1).
    fn answered(&mut self, state: &AppState, status: u16) {
        if (200..300).contains(&status) || status >= 500 || status == 429 {
            self.not_counted(state);
        }
    }

    /// After the tool server answered a step whose `429` is about this address's own guessing
    /// (the second step and a setup's confirmation: the address waits for that person; a
    /// setup's first step: the block used its guesses, and this one was checked and wrong):
    /// those count, like every other refusal.
    fn answered_guess(&mut self, state: &AppState, status: u16) {
        if (200..300).contains(&status) || status >= 500 {
            self.not_counted(state);
        }
    }

    /// For a step that does work whatever comes of it (a setup's password: a hash; its
    /// authenticator: a new secret): counted **even when it succeeds** (the re-check of
    /// eb24d28, H1); PM OS's own failure (`5xx`) and a refusal by a limit the tool server
    /// shares (`429`: the account's work or the open allowance, which did no work and cost
    /// the invitee nothing there, G3) give the slot back (the review of 8e8f8fe).
    fn answered_with_work(&mut self, state: &AppState, status: u16) {
        if status >= 500 || status == 429 {
            self.not_counted(state);
        }
    }
}

impl super::Inner {
    /// A request refused because its address can't be known with the proxy setting as it is
    /// (M1): counted for `/readyz`, logged at error level at most once a minute.
    fn note_untrusted_proxy(&self, why: Unsourced, now: DateTime<Utc>) {
        use std::sync::atomic::Ordering;
        let total = self
            .untrusted_proxy_refusals
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        let at = now.timestamp();
        // GL3: `/readyz` warns while this is recent.
        self.untrusted_proxy_last_at.store(at, Ordering::Relaxed);
        let last = self.untrusted_proxy_logged.load(Ordering::Relaxed);
        if at - last >= 60
            && self
                .untrusted_proxy_logged
                .compare_exchange(last, at, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            let what = match why {
                Unsourced::UntrustedForwarder => {
                    "a forwarded header from a peer not in PMOS_WEB_TRUSTED_PROXIES"
                }
                Unsourced::NoClient => "a trusted proxy that named no client in X-Forwarded-For",
            };
            tracing::error!(
                refused_since_start = total,
                "password sign-in refused: {what}. PMOS_WEB_TRUSTED_PROXIES is wrong or stale \
                 (in the deploy kit it must be the pmos network's gateway, PMOS_NETWORK_GATEWAY; \
                 deploy/pmos-admin up --web checks it), or the proxy doesn't set X-Forwarded-For \
                 (docs/password-sign-in.md)"
            );
        }
    }
}

/// An unauthenticated password route's checks: on; same origin; JSON; no forwarded header
/// unless a proxy is trusted; the address, and `subject` from it, not over their limits.
/// Takes the attempt's slots (they count unless given back), so every refusal from here on is
/// counted.
fn pre(
    state: &AppState,
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
    subject: &Subject,
) -> Result<Try, Box<Response>> {
    if !state.0.password_login {
        return Err(Box::new(off().into_response()));
    }
    if !auth::same_origin(headers, state.0.public_url.as_deref()) {
        return Err(Box::new(
            ApiError::new(
                StatusCode::FORBIDDEN,
                "bad_origin",
                "That request didn't come from PM OS.",
            )
            .into_response(),
        ));
    }
    // Login CSRF: a cross-site form can't send JSON.
    if !is_json(headers) {
        return Err(Box::new(bad_content_type().into_response()));
    }
    let from = address(state, headers, peer)?;
    match state.0.sources.begin(&from, subject, now(state)) {
        Ok(attempt) => Ok(Try {
            from,
            attempt: Some(attempt),
        }),
        Err(wait) => {
            tracing::warn!("password sign-in: an address, or a subject from it, over its limit");
            Err(Box::new(too_many(wait)))
        }
    }
}

/// What a step that carries `cookie` is about: its token, or nothing readable.
fn token_subject(headers: &HeaderMap, cookie: &str, kind: &str) -> Subject {
    read_cookie(headers, cookie).map_or_else(Subject::none, |t| Subject::token(kind, &t))
}

fn proxy_not_trusted() -> Box<Response> {
    Box::new(
        ApiError::new(
            StatusCode::FORBIDDEN,
            "proxy_not_trusted",
            "PM OS can't sign you in through this address. Tell an admin: the web app's proxy isn't set up.",
        )
        .into_response(),
    )
}

/// The address a password request is counted under, or its refusal (`403 proxy_not_trusted`):
/// a forwarded header with no trusted proxy (`PMOS_WEB_DIRECT=1`), or, with
/// `PMOS_WEB_TRUSTED_PROXIES` set, one from a peer not on it or a trusted proxy that names no
/// client (M1). The two with a list set are a wrong or stale setting: counted on `/readyz`
/// (`untrusted_proxy_refusals`) and logged at error level, at most once a minute with the
/// count, so an operator sees it. The tool server gets only an address resolved here, in the
/// body of a call made with pmos-web's own token; it never sees a proxy's address as anyone's.
fn address(
    state: &AppState,
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
) -> Result<SourceKey, Box<Response>> {
    if state.0.trusted_proxies.is_empty() && forwarded(headers) {
        tracing::warn!(
            "password sign-in refused: a forwarded header with no trusted proxy (set PMOS_WEB_TRUSTED_PROXIES)"
        );
        return Err(proxy_not_trusted());
    }
    match source(peer, headers, &state.0.trusted_proxies) {
        Ok(ip) => Ok(source_key(ip)),
        Err(why) => {
            state.0.note_untrusted_proxy(why, now(state));
            Err(proxy_not_trusted())
        }
    }
}

/// A signed-in password route that hashes or makes new secrets (`change`, `recovery-codes`):
/// counted against the caller's address, and the person from it, like every unauthenticated
/// attempt, whatever its outcome (the re-check of eb24d28, LA), so one session can't spend the
/// address's share.
fn count_signed(
    state: &AppState,
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
    handle: &str,
) -> Result<(), Box<Response>> {
    let from = address(state, headers, peer)?;
    match state
        .0
        .sources
        .begin(&from, &Subject::user(handle), now(state))
    {
        Ok(_kept) => Ok(()),
        Err(wait) => {
            tracing::warn!("password route: an address over its limit");
            Err(Box::new(too_many(wait)))
        }
    }
}

/// The peer's address, when the server was started with it (`main` serves with connect info;
/// a test that doesn't has none).
pub struct Peer(Option<SocketAddr>);

impl<S: Send + Sync> FromRequestParts<S> for Peer {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|c| c.0),
        ))
    }
}

// --- Sign-in -----------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignInBody {
    sign_in_name: String,
    password: String,
}

/// `POST /api/auth/password/sign-in {sign_in_name, password}`: the first step.
async fn sign_in(
    State(state): State<AppState>,
    Peer(peer): Peer,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let parsed: Result<SignInBody, _> = parse(&body);
    let subject = parsed
        .as_ref()
        .map_or_else(|_| Subject::none(), |b| Subject::name(&b.sign_in_name));
    let mut t = match pre(&state, &headers, peer, &subject) {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let b = match parsed {
        Ok(b) => b,
        Err(e) => return e.into_response(),
    };
    let a = match call(
        &state,
        &["sign-in"],
        &json!({"sign_in_name": b.sign_in_name, "password": b.password, "source": t.source()}),
    )
    .await
    {
        Ok(a) => a,
        Err(e) => {
            t.not_counted(&state);
            return e.into_response();
        }
    };
    t.answered(&state, a.status);
    if a.status != 200 {
        return refused(a.status, &a.body, a.retry_after);
    }
    let Some(token) = a.body.get("pending_token").and_then(Value::as_str) else {
        return bad_reply();
    };
    let res = Json(json!({
        "next": "second_step",
        "expires_at": a.body.get("expires_at"),
    }))
    .into_response();
    with_cookies(res, &[cookie(PENDING_COOKIE, token, PENDING_SECS)])
}

fn bad_reply() -> Response {
    ApiError::upstream(UpstreamError::Failed {
        service: "tool server",
        why: "sent a reply pmos-web can't read".into(),
    })
    .into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SecondStepBody {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    recovery_code: Option<String>,
}

/// The credential's epoch in the tool server's answer: required (L7). A password session
/// without one would never be checked against a reset, so none is started.
fn epoch_of(body: &Value) -> Result<i64, Box<Response>> {
    body.get("password_epoch")
        .and_then(Value::as_i64)
        .ok_or_else(|| {
            tracing::warn!("the tool server answered a password sign-in without an epoch");
            Box::new(bad_reply())
        })
}

/// Start the session for the tool server's `user` with `epoch`, ending `others` first when
/// asked (a setup or reset ends every session of theirs).
async fn session_for(
    state: &AppState,
    headers: &HeaderMap,
    user: &Value,
    epoch: i64,
    end_others: bool,
) -> Result<String, Box<Response>> {
    let Some(user) = pmos_directory::resolve::parse_user(user) else {
        return Err(Box::new(bad_reply()));
    };
    if !user.is_active() {
        return Err(Box::new(super::no_longer_a_member().into_response()));
    }
    if end_others {
        state
            .0
            .sessions
            .remove_user(&user.handle)
            .await
            .map_err(|e| Box::new(ApiError::store(e).into_response()))?;
    }
    state.0.directory.forget(&user.handle);
    start_session(state, headers, &user, "password", None, Some(epoch))
        .await
        .map_err(|e| Box::new(e.into_response()))
}

/// `POST /api/auth/password/second-step {code} | {recovery_code}`, with the pending cookie:
/// the session.
async fn second_step(
    State(state): State<AppState>,
    Peer(peer): Peer,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let subject = token_subject(&headers, PENDING_COOKIE, "pending");
    let mut t = match pre(&state, &headers, peer, &subject) {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let b: SecondStepBody = match parse(&body) {
        Ok(b) => b,
        Err(e) => return e.into_response(),
    };
    let mut req = match (b.code, b.recovery_code) {
        (Some(c), None) => json!({ "code": c }),
        (None, Some(r)) => json!({ "recovery_code": r }),
        _ => {
            return ApiError::bad_request("Send `code` or `recovery_code`, one of them.")
                .into_response()
        }
    };
    let Some(token) = read_cookie(&headers, PENDING_COOKIE) else {
        return with_cookies(
            refused(401, &json!({"code": "sign_in_expired"}), None),
            &[clear(PENDING_COOKIE)],
        );
    };
    req["pending_token"] = json!(token);
    req["source"] = t.source();
    let a = match call(&state, &["second-step"], &req).await {
        Ok(a) => a,
        Err(e) => {
            t.not_counted(&state);
            return e.into_response();
        }
    };
    t.answered_guess(&state, a.status);
    if a.status != 200 {
        let code = a
            .body
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let res = refused(a.status, &a.body, a.retry_after);
        let used_up = code == "sign_in_expired"
            || code == "not_a_member"
            || (code == "code_failed"
                && a.body.get("attempts_left").and_then(Value::as_i64) == Some(0));
        return if used_up {
            with_cookies(res, &[clear(PENDING_COOKIE)])
        } else {
            res
        };
    }
    let epoch = match epoch_of(&a.body) {
        Ok(e) => e,
        Err(r) => return *r,
    };
    let id = match session_for(&state, &headers, &a.body["user"], epoch, false).await {
        Ok(id) => id,
        Err(r) => return *r,
    };
    let res = Json(json!({
        "recovery_code_used": a.body.get("recovery_code_used").and_then(Value::as_bool).unwrap_or(false),
        "recovery_codes_left": a.body.get("recovery_codes_left").and_then(Value::as_u64).unwrap_or(0),
    }))
    .into_response();
    with_cookies(
        res,
        &[clear(PENDING_COOKIE), crate::session::set_cookie(&id)],
    )
}

// --- Setup -------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BeginBody {
    sign_in_name: String,
    setup_code: String,
}

/// `POST /api/auth/password/setup/begin {sign_in_name, setup_code}`.
async fn setup_begin(
    State(state): State<AppState>,
    Peer(peer): Peer,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let parsed: Result<BeginBody, _> = parse(&body);
    let subject = parsed
        .as_ref()
        .map_or_else(|_| Subject::none(), |b| Subject::name(&b.sign_in_name));
    let mut t = match pre(&state, &headers, peer, &subject) {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let b = match parsed {
        Ok(b) => b,
        Err(e) => return e.into_response(),
    };
    let a = match call(
        &state,
        &["setup", "begin"],
        &json!({"sign_in_name": b.sign_in_name, "setup_code": b.setup_code, "source": t.source()}),
    )
    .await
    {
        Ok(a) => a,
        Err(e) => {
            t.not_counted(&state);
            return e.into_response();
        }
    };
    t.answered_guess(&state, a.status);
    if a.status != 200 {
        return refused(a.status, &a.body, a.retry_after);
    }
    let Some(token) = a.body.get("setup_token").and_then(Value::as_str) else {
        return bad_reply();
    };
    let res = Json(json!({
        "handle": a.body.get("handle"),
        "kind": a.body.get("kind"),
        "expires_at": a.body.get("expires_at"),
        "password": {"min_length": 12, "max_length": 128},
    }))
    .into_response();
    with_cookies(res, &[cookie(SETUP_COOKIE, token, SETUP_SECS)])
}

/// The setup cookie, or `401 setup_expired`.
fn setup_token(headers: &HeaderMap) -> Result<String, Box<Response>> {
    read_cookie(headers, SETUP_COOKIE).ok_or_else(|| {
        Box::new(with_cookies(
            refused(401, &json!({"code": "setup_expired"}), None),
            &[clear(SETUP_COOKIE)],
        ))
    })
}

/// A setup step's refusal: `setup_expired` clears the cookie.
fn setup_refused(a: &Answer) -> Response {
    let res = refused(a.status, &a.body, a.retry_after.clone());
    let code = a
        .body
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let used_up = matches!(
        code,
        "setup_expired" | "setup_code_spent" | "not_a_member" | "host_only" | "sign_in_unconfirmed"
    ) || (code == "code_failed"
        && a.body.get("attempts_left").and_then(Value::as_i64) == Some(0));
    if used_up {
        with_cookies(res, &[clear(SETUP_COOKIE)])
    } else {
        res
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordBody {
    password: String,
}

/// `POST /api/auth/password/setup/password {password}`: `204`.
async fn setup_password(
    State(state): State<AppState>,
    Peer(peer): Peer,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let subject = token_subject(&headers, SETUP_COOKIE, "setup");
    let mut t = match pre(&state, &headers, peer, &subject) {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let b: PasswordBody = match parse(&body) {
        Ok(b) => b,
        Err(e) => return e.into_response(),
    };
    let token = match setup_token(&headers) {
        Ok(t) => t,
        Err(r) => return *r,
    };
    match call(
        &state,
        &["setup", "password"],
        &json!({"setup_token": token, "password": b.password}),
    )
    .await
    {
        Ok(a) => {
            // Every call hashes at the tool server: counted against the address even when it
            // succeeds (H1).
            t.answered_with_work(&state, a.status);
            if a.status == 200 {
                StatusCode::NO_CONTENT.into_response()
            } else {
                setup_refused(&a)
            }
        }
        Err(e) => {
            t.not_counted(&state);
            e.into_response()
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Empty {}

/// `POST /api/auth/password/setup/authenticator {}`: the enrolment, once.
async fn setup_authenticator(
    State(state): State<AppState>,
    Peer(peer): Peer,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let subject = token_subject(&headers, SETUP_COOKIE, "setup");
    let mut t = match pre(&state, &headers, peer, &subject) {
        Ok(t) => t,
        Err(r) => return *r,
    };
    if let Err(e) = parse_optional::<Empty>(&body) {
        return e.into_response();
    }
    let token = match setup_token(&headers) {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let answer = call(
        &state,
        &["setup", "authenticator"],
        &json!({ "setup_token": token }),
    )
    .await;
    match &answer {
        // Every call makes a new secret: counted even when it succeeds (H1).
        Ok(a) => t.answered_with_work(&state, a.status),
        Err(_) => t.not_counted(&state),
    }
    match answer {
        Ok(a) if a.status == 200 => {
            let pick = |k: &str| a.body.get(k).cloned().unwrap_or(Value::Null);
            Json(json!({
                "otpauth_uri": pick("otpauth_uri"),
                "secret": pick("secret"),
                "account": pick("account"),
                "issuer": pick("issuer"),
                "algorithm": pick("algorithm"),
                "digits": pick("digits"),
                "period": pick("period"),
            }))
            .into_response()
        }
        Ok(a) => setup_refused(&a),
        Err(e) => e.into_response(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeBody {
    code: String,
}

/// `POST /api/auth/password/setup/confirm {code}`: the recovery codes, once, and the session;
/// every other session of the person ends.
async fn setup_confirm(
    State(state): State<AppState>,
    Peer(peer): Peer,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let subject = token_subject(&headers, SETUP_COOKIE, "setup");
    let mut t = match pre(&state, &headers, peer, &subject) {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let b: CodeBody = match parse(&body) {
        Ok(b) => b,
        Err(e) => return e.into_response(),
    };
    let token = match setup_token(&headers) {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let a = match call(
        &state,
        &["setup", "confirm"],
        &json!({"setup_token": token, "code": b.code, "source": t.source()}),
    )
    .await
    {
        Ok(a) => a,
        Err(e) => {
            t.not_counted(&state);
            return e.into_response();
        }
    };
    t.answered_guess(&state, a.status);
    if a.status != 200 {
        return setup_refused(&a);
    }
    let Some(codes) = a
        .body
        .get("recovery_codes")
        .and_then(Value::as_array)
        .cloned()
    else {
        return bad_reply();
    };
    let epoch = match epoch_of(&a.body) {
        Ok(e) => e,
        Err(r) => return *r,
    };
    let id = match session_for(&state, &headers, &a.body["user"], epoch, true).await {
        Ok(id) => id,
        Err(r) => return *r,
    };
    let res = Json(json!({
        "kind": a.body.get("kind"),
        "recovery_codes": codes,
    }))
    .into_response();
    with_cookies(res, &[clear(SETUP_COOKIE), crate::session::set_cookie(&id)])
}

// --- Signed in ---------------------------------------------------------------------------------

/// A signed-in person's call, for them, with their assertion. A write whose answer was lost
/// is `502 unknown_outcome`.
async fn signed_call(
    state: &AppState,
    signed: &Signed,
    method: reqwest::Method,
    segments: &[&str],
    body: Option<&Value>,
) -> Result<Answer, Box<Response>> {
    let write = method != reqwest::Method::GET;
    match signed
        .toolserver(state)
        .password_call(method, segments, Some(signed.handle()), body)
        .await
    {
        Ok((status, body, retry_after)) => Ok(Answer {
            status,
            body,
            retry_after,
        }),
        Err(UpstreamError::Unknown { why, .. }) if write => {
            tracing::warn!(why = %why, "a password change's answer was lost");
            Err(Box::new(
                ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "unknown_outcome",
                    "PM OS can't tell whether that change was made. Sign in again to see.",
                )
                .into_response(),
            ))
        }
        Err(e) => Err(Box::new(ApiError::upstream(e).into_response())),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangeBody {
    current_password: String,
    code: String,
    new_password: String,
}

/// `POST /api/auth/password/change {current_password, code, new_password}`: `204` with a new
/// session cookie; every other session of the person ends.
async fn change(
    State(state): State<AppState>,
    Peer(peer): Peer,
    signed: Signed,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    if !state.0.password_login {
        return Err(off());
    }
    if let Err(r) = count_signed(&state, &headers, peer, signed.handle()) {
        return Ok(*r);
    }
    let b: ChangeBody = parse(&body)?;
    let req = json!({
        "current_password": b.current_password,
        "code": b.code,
        "new_password": b.new_password,
    });
    let a = match signed_call(
        &state,
        &signed,
        reqwest::Method::POST,
        &["change"],
        Some(&req),
    )
    .await
    {
        Ok(a) => a,
        Err(r) => return Ok(*r),
    };
    if a.status != 200 {
        return Ok(refused(a.status, &a.body, a.retry_after));
    }
    let epoch = match epoch_of(&a.body) {
        Ok(e) => e,
        Err(r) => return Ok(*r),
    };
    state
        .0
        .sessions
        .remove_user(signed.handle())
        .await
        .map_err(ApiError::store)?;
    // The cached answer holds the old epoch: ask afresh, so the new session isn't ended by it.
    state.0.directory.forget(signed.handle());
    let user = match state
        .0
        .directory
        .resolve(&pmos_directory::resolve::Lookup::handle(signed.handle()))
        .await
    {
        Ok(u) => u,
        Err(_) => return Err(super::users_unavailable()),
    };
    let id = start_session(&state, &headers, &user, "password", None, Some(epoch)).await?;
    Ok((
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, crate::session::set_cookie(&id))],
    )
        .into_response())
}

/// `POST /api/auth/password/recovery-codes {code}`: ten new recovery codes, once.
async fn recovery_codes(
    State(state): State<AppState>,
    Peer(peer): Peer,
    signed: Signed,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    if !state.0.password_login {
        return Err(off());
    }
    if let Err(r) = count_signed(&state, &headers, peer, signed.handle()) {
        return Ok(*r);
    }
    let b: CodeBody = parse(&body)?;
    let a = match signed_call(
        &state,
        &signed,
        reqwest::Method::POST,
        &["recovery-codes"],
        Some(&json!({ "code": b.code })),
    )
    .await
    {
        Ok(a) => a,
        Err(r) => return Ok(*r),
    };
    if a.status != 200 {
        return Ok(refused(a.status, &a.body, a.retry_after));
    }
    Ok(Json(json!({ "recovery_codes": a.body.get("recovery_codes") })).into_response())
}

/// `GET /api/auth/password/status`: the signed-in person's own.
async fn status(State(state): State<AppState>, signed: Signed) -> ApiResult {
    if !state.0.password_login {
        return Err(off());
    }
    let a = match signed_call(&state, &signed, reqwest::Method::GET, &["status"], None).await {
        Ok(a) => a,
        Err(r) => return Ok(*r),
    };
    if a.status != 200 {
        return Ok(refused(a.status, &a.body, a.retry_after));
    }
    let pick = |k: &str| a.body.get(k).and_then(Value::as_bool).unwrap_or(false);
    // A reset someone issued for this person (PM2): who (a handle, or `host`), their name,
    // and when. Re-picked field by field.
    let notice = a
        .body
        .get("reset_notice")
        .filter(|n| {
            n.get("by").is_some_and(Value::is_string) && n.get("at").is_some_and(Value::is_string)
        })
        .map(|n| {
            json!({
                "by": n["by"],
                "by_name": n.get("by_name").filter(|v| v.is_string()),
                "at": n["at"],
            })
        });
    Ok(Json(json!({
        "enabled": pick("enabled"),
        "password_set": pick("password_set"),
        "second_step_enrolled": pick("second_step_enrolled"),
        "recovery_codes_left": a.body.get("recovery_codes_left").and_then(Value::as_u64).unwrap_or(0),
        "needs_host_setup": pick("needs_host_setup"),
        "reset_notice": notice,
    }))
    .into_response())
}

/// `POST /api/auth/password/reset-notice/dismiss {}`: the signed-in person has read the notice
/// of a reset someone issued for them. `204`.
async fn dismiss_reset_notice(
    State(state): State<AppState>,
    signed: Signed,
    body: Bytes,
) -> ApiResult {
    if !state.0.password_login {
        return Err(off());
    }
    parse_optional::<Empty>(&body)?;
    let a = match signed_call(
        &state,
        &signed,
        reqwest::Method::POST,
        &["reset-notice", "dismiss"],
        Some(&json!({})),
    )
    .await
    {
        Ok(a) => a,
        Err(r) => return Ok(*r),
    };
    if !(200..300).contains(&a.status) {
        return Ok(refused(a.status, &a.body, a.retry_after));
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `POST /api/admin/users/{handle}/password-setup-code`: an admin makes a one-time setup (or
/// reset) code, shown once.
async fn issue_setup_code(
    State(state): State<AppState>,
    signed: Signed,
    Path(handle): Path<String>,
) -> ApiResult {
    if !state.0.password_login {
        return Err(off());
    }
    signed.need(Permission::ManageMembers).map_err(|_| {
        ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Only an active admin can make setup codes. Nothing changed.",
        )
    })?;
    if !pmos_contract::model::valid_handle(&handle) {
        return Err(ApiError::not_found());
    }
    let a = match signed_call(
        &state,
        &signed,
        reqwest::Method::POST,
        &["setup-codes"],
        Some(&json!({ "handle": handle })),
    )
    .await
    {
        Ok(a) => a,
        Err(r) => return Ok(*r),
    };
    if a.status != 201 {
        return Ok(refused(a.status, &a.body, a.retry_after));
    }
    tracing::info!(admin = %signed.handle(), target = %handle, "password setup code made");
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "setup_code": a.body.get("setup_code"),
            "handle": a.body.get("handle"),
            "kind": a.body.get("kind"),
            "expires_at": a.body.get("expires_at"),
        })),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-20T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + Duration::seconds(secs)
    }

    fn key(s: &str) -> SourceKey {
        source_key(Some(s.parse().unwrap()))
    }

    fn name(n: usize) -> Subject {
        Subject::name(&format!("p{n}@example.com"))
    }

    /// The map and scan [`Recent`] replaced, kept here to check it decides the same.
    #[derive(Default)]
    struct Scan(HashMap<u32, VecDeque<DateTime<Utc>>>);

    impl Scan {
        fn evict(&mut self, most: usize, now: DateTime<Utc>, window: Duration) {
            let map = &mut self.0;
            if map.len() < most {
                return;
            }
            map.retain(|_, t| t.back().is_some_and(|at| now - *at < window && *at <= now));
            if map.len() >= most {
                let stalest = map
                    .iter()
                    .min_by_key(|(_, t)| t.back().copied())
                    .map(|(k, _)| *k);
                if let Some(k) = stalest {
                    map.remove(&k);
                }
            }
        }
    }

    /// The review of 2efb73f: making room no longer scans every key, and decides exactly as
    /// the scan did: the same keys kept, with the same times, through attempts, refusals,
    /// give-backs and a clock that sometimes steps back.
    #[test]
    fn the_ordered_index_evicts_exactly_as_the_scan_did() {
        let window = Duration::minutes(SOURCE_WINDOW_MINUTES);
        let (most, cap) = (6, 3);
        let mut fast: Recent<u32> = Recent::default();
        let mut slow = Scan::default();
        let mut used = std::collections::BTreeSet::new();
        let mut pushed: Vec<(u32, DateTime<Utc>)> = Vec::new();
        // A small, fixed pseudo-random sequence (no clock, no dependency).
        let mut seed: u64 = 0x2efb_73f0_dead_beef;
        let mut rand = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let mut clock: i64 = 0;
        for step in 0..20_000 {
            // Mostly forward, sometimes far (past the window), sometimes a step back; never a
            // time used before, so no two keys tie (the scan broke ties arbitrarily).
            clock += match rand(20) {
                0 => -(rand(120) as i64) - 1,
                1 => 16 * 60 + rand(600) as i64,
                _ => rand(30) as i64 + 1,
            };
            while !used.insert(clock) {
                clock += 1;
            }
            let now = at(clock);
            if rand(5) == 0 && !pushed.is_empty() {
                let (k, t) = pushed.swap_remove(rand(pushed.len() as u64) as usize);
                fast.give_back(&k, t);
                if let Some(times) = slow.0.get_mut(&k) {
                    if let Some(i) = times.iter().rposition(|x| *x == t) {
                        times.remove(i);
                    }
                    if times.is_empty() {
                        slow.0.remove(&k);
                    }
                }
            } else {
                let k = rand(12) as u32;
                let a = fast.room(&k, cap, now, window);
                let b = match slow.0.get_mut(&k) {
                    Some(times) => room(times, cap, now, window),
                    None => Ok(()),
                };
                assert_eq!(a, b, "step {step}");
                if a.is_ok() {
                    assert_eq!(fast.contains(&k), slow.0.contains_key(&k));
                    if !fast.contains(&k) {
                        fast.evict(most, now, window);
                        slow.evict(most, now, window);
                    }
                    fast.push(k, now);
                    slow.0.entry(k).or_default().push_back(now);
                    pushed.push((k, now));
                }
            }
            let kept: HashMap<u32, VecDeque<DateTime<Utc>>> = fast
                .times
                .iter()
                .map(|(k, (t, _))| (*k, t.clone()))
                .collect();
            assert_eq!(kept, slow.0, "step {step}");
            assert!(fast.len() <= most);
            // The index holds exactly the keys with times.
            assert_eq!(
                fast.order.len() + fast.empty.len(),
                fast.len(),
                "step {step}"
            );
        }
    }

    /// With the pair map full (50,000), new pairs still go in, each making room without a
    /// scan, and the limits hold as before: the newest pairs are kept.
    #[test]
    fn a_full_pair_map_makes_room_for_new_pairs() {
        let l = SourceLimiter::default();
        let a = key("203.0.113.7");
        // Fill it directly: one refusal each, every one still in the window.
        {
            let mut seen = l.seen.lock().unwrap();
            for n in 0..MAX_SUBJECTS {
                seen.by_subject.push((a.clone(), name(n)), at(0));
            }
            assert_eq!(seen.by_subject.len(), MAX_SUBJECTS);
        }
        // From other addresses, so no address is full: each new pair pushes out an old one.
        for n in 0..20_000 {
            let from = key(&format!("198.51.{}.{}", n / 250, n % 250 + 1));
            let t = l
                .begin(&from, &name(MAX_SUBJECTS + n), at(1 + n as i64 / 1000))
                .expect("room");
            l.settle(t, true);
        }
        let seen = l.seen.lock().unwrap();
        assert_eq!(seen.by_subject.len(), MAX_SUBJECTS);
        let newest = (key("198.51.79.250"), name(MAX_SUBJECTS + 19_999));
        assert!(seen.by_subject.contains(&newest));
    }

    /// N2 (the review of 8e8f8fe): an address waits after a hundred refusals in fifteen
    /// minutes, about anything (before: twenty); one subject from it after ten. (Before, this
    /// test asserted the address waited after twenty refusals; it now asserts a hundred, with
    /// ten per subject.)
    #[test]
    fn an_address_waits_after_a_hundred_refusals_and_a_subject_after_ten() {
        let l = SourceLimiter::default();
        let (a, b) = (key("203.0.113.7"), key("203.0.113.8"));
        assert_eq!((SOURCE_REFUSALS, SUBJECT_REFUSALS), (100, 10));
        for i in 0..SOURCE_REFUSALS {
            let t = l.begin(&a, &name(i), at(i as i64)).expect("room");
            l.settle(t, true);
        }
        assert_eq!(l.begin(&a, &name(500), at(150)).err(), Some(15 * 60 - 150));
        assert!(
            l.begin(&b, &name(0), at(150)).is_ok(),
            "another address has its own count"
        );
        // The first refusal leaves the window after 15 minutes: one more try.
        assert!(l.begin(&a, &name(501), at(15 * 60)).is_ok());
        assert!(l.begin(&a, &name(502), at(15 * 60)).is_err());
        assert!(l.begin(&a, &name(503), at(15 * 60 + 20 * 60)).is_ok());

        // One subject: ten, then it waits from there; nothing past them reaches the address's
        // count, so the others there are not held up.
        let l = SourceLimiter::default();
        let ana = Subject::name(" Ana@Example.com ");
        for i in 0..SUBJECT_REFUSALS as i64 {
            let t = l.begin(&a, &ana, at(i)).expect("room");
            l.settle(t, true);
        }
        for i in 0..200 {
            assert_eq!(
                l.begin(&a, &Subject::name("ana@example.com"), at(20 + i))
                    .err(),
                Some(15 * 60 - 20 - i as u64),
                "the same name as the tool server reads it"
            );
        }
        assert!(l.begin(&b, &ana, at(30)).is_ok(), "from another address");
        for n in 0..(SOURCE_REFUSALS - SUBJECT_REFUSALS) {
            let t = l.begin(&a, &name(n), at(300)).expect("the others' room");
            l.settle(t, true);
        }
        assert!(
            l.begin(&a, &name(999), at(300)).is_err(),
            "now the address is full"
        );
    }

    /// L2: the check and the count are one step. A hundred attempts in flight at once from one
    /// address take every slot; the next is refused before it reaches the tool server, and a
    /// success gives its slot back. The same for one subject's ten.
    #[test]
    fn concurrent_attempts_cannot_pass_the_limit_together() {
        let l = SourceLimiter::default();
        let a = key("203.0.113.7");
        let held: Vec<Attempt> = (0..SOURCE_REFUSALS)
            .map(|n| l.begin(&a, &name(n), at(0)).expect("room"))
            .collect();
        assert!(
            l.begin(&a, &name(1000), at(0)).is_err(),
            "all hundred are in flight"
        );
        let mut held = held.into_iter();
        l.settle(held.next().unwrap(), false);
        assert!(
            l.begin(&a, &name(1000), at(1)).is_ok(),
            "a success gave its slot back"
        );
        assert!(l.begin(&a, &name(1001), at(1)).is_err());
        let l = SourceLimiter::default();
        let one = Subject::token("pending", &"p".repeat(43));
        let held: Vec<Attempt> = (0..SUBJECT_REFUSALS)
            .map(|_| l.begin(&a, &one, at(0)).expect("room"))
            .collect();
        assert!(l.begin(&a, &one, at(0)).is_err(), "all ten are in flight");
        l.settle(held.into_iter().next().unwrap(), false);
        assert!(l.begin(&a, &one, at(1)).is_ok());
        // Subjects of different kinds never share a count.
        assert_ne!(Subject::token("pending", "x"), Subject::token("setup", "x"));
        assert_ne!(Subject::name("x"), Subject::user("x"));
        assert_eq!(
            format!("{:?}", Subject::name("ana@example.com")),
            "Subject(..)"
        );
    }

    /// L3: IPv6 counts by /64; an IPv4-mapped address counts as IPv4.
    #[test]
    fn ipv6_addresses_count_by_their_64() {
        assert_eq!(
            key("2001:db8:1:2::1"),
            key("2001:db8:1:2:ffff:ffff:ffff:ffff")
        );
        assert_eq!(key("2001:db8:1:2::1").as_deref(), Some("2001:db8:1:2::/64"));
        assert_ne!(key("2001:db8:1:2::1"), key("2001:db8:1:3::1"));
        assert_eq!(key("::ffff:203.0.113.7"), key("203.0.113.7"));
        let l = SourceLimiter::default();
        for i in 0..SOURCE_REFUSALS {
            let t = l
                .begin(&key(&format!("2001:db8:1:2::{:x}", i + 1)), &name(i), at(0))
                .unwrap();
            l.settle(t, true);
        }
        assert!(l
            .begin(&key("2001:db8:1:2::abcd"), &name(1000), at(1))
            .is_err());
        assert!(l
            .begin(&key("2001:db8:1:99::1"), &name(1000), at(1))
            .is_ok());
    }

    fn xff(values: &[&'static str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for v in values {
            h.append("x-forwarded-for", HeaderValue::from_static(v));
        }
        h
    }

    #[test]
    fn the_address_behind_a_trusted_proxy_is_the_forwarded_one() {
        let peer: SocketAddr = "10.0.0.1:5000".parse().unwrap();
        let proxy: IpAddr = "10.0.0.1".parse().unwrap();
        let h = xff(&["198.51.100.9, 203.0.113.7, 10.0.0.1"]);
        // No proxies named (direct): the peer's own address (pmos-web refuses the forwarded
        // header itself before asking).
        assert_eq!(source(Some(peer), &h, &[]), Ok(Some(proxy)));
        // Trusted: the right-most address that isn't a proxy (the client's own claim to the
        // left of it is not believed).
        assert_eq!(
            source(Some(peer), &h, &[proxy]),
            Ok("203.0.113.7".parse().ok())
        );
        assert_eq!(source(None, &h, &[proxy]), Ok(None));
    }

    /// M1: with proxies named, a wrong or stale setting is loud, never one shared count. A
    /// forwarded header from a peer not on the list is refused (the proxy's address moved), and
    /// a trusted proxy that names no client is refused (the address would be the proxy's).
    #[test]
    fn a_wrong_or_stale_proxy_setting_is_refused_not_shared() {
        let proxy: IpAddr = "10.201.77.1".parse().unwrap();
        let moved: SocketAddr = "172.18.0.1:5000".parse().unwrap();
        let at_proxy: SocketAddr = "10.201.77.1:5000".parse().unwrap();
        assert_eq!(
            source(Some(moved), &xff(&["203.0.113.7"]), &[proxy]),
            Err(Unsourced::UntrustedForwarder)
        );
        let mut real_ip = HeaderMap::new();
        real_ip.insert("x-real-ip", HeaderValue::from_static("203.0.113.7"));
        assert_eq!(
            source(Some(moved), &real_ip, &[proxy]),
            Err(Unsourced::UntrustedForwarder)
        );
        // A peer that isn't the proxy and forwards nothing is counted by its own address.
        assert_eq!(
            source(Some(moved), &HeaderMap::new(), &[proxy]),
            Ok("172.18.0.1".parse().ok())
        );
        // The proxy itself, naming nobody: no header, or only proxies in it.
        assert_eq!(
            source(Some(at_proxy), &HeaderMap::new(), &[proxy]),
            Err(Unsourced::NoClient)
        );
        assert_eq!(
            source(Some(at_proxy), &xff(&["10.201.77.1"]), &[proxy]),
            Err(Unsourced::NoClient)
        );
    }

    /// L1: strictly from the right; `ip:port` and bracketed IPv6 read; an entry that can't be
    /// read ends the walk (never skipped), so a client can't slip its own value past it.
    #[test]
    fn forwarded_for_is_read_strictly_from_the_right() {
        let peer: SocketAddr = "10.0.0.1:5000".parse().unwrap();
        let proxy: IpAddr = "10.0.0.1".parse().unwrap();
        let inner: IpAddr = "10.0.0.2".parse().unwrap();
        let ip = |s: &str| Ok(s.parse::<IpAddr>().ok());
        // A proxy that writes the port.
        assert_eq!(
            source(Some(peer), &xff(&["203.0.113.7:51234"]), &[proxy]),
            ip("203.0.113.7")
        );
        assert_eq!(
            source(Some(peer), &xff(&["[2001:db8::7]:443"]), &[proxy]),
            ip("2001:db8::7")
        );
        assert_eq!(
            source(Some(peer), &xff(&["[2001:db8::7]"]), &[proxy]),
            ip("2001:db8::7")
        );
        // A client-written value to the left of something unreadable is never reached; what
        // was read is only trusted hops, so the request names no client and is refused (M1),
        // never counted as the proxy.
        assert_eq!(
            source(Some(peer), &xff(&["198.51.100.66, garbage"]), &[proxy]),
            Err(Unsourced::NoClient)
        );
        assert_eq!(
            source(
                Some(peer),
                &xff(&["198.51.100.66, unknown, 10.0.0.2"]),
                &[proxy, inner]
            ),
            Err(Unsourced::NoClient)
        );
        assert_eq!(
            source(
                Some(peer),
                &xff(&["198.51.100.66, [2001:db8::7]:x"]),
                &[proxy]
            ),
            Err(Unsourced::NoClient)
        );
        // Two proxies: both passed, the client is the next one left; the left-most claim isn't.
        assert_eq!(
            source(
                Some(peer),
                &xff(&["192.0.2.1, 203.0.113.9, 10.0.0.2"]),
                &[proxy, inner]
            ),
            ip("203.0.113.9")
        );
        // Several headers are one list, in order.
        assert_eq!(
            source(Some(peer), &xff(&["192.0.2.1", "203.0.113.9"]), &[proxy]),
            ip("203.0.113.9")
        );
        // Every entry a proxy: no client named.
        assert_eq!(
            source(Some(peer), &xff(&["10.0.0.2"]), &[proxy, inner]),
            Err(Unsourced::NoClient)
        );
        // An IPv4-mapped peer matches its IPv4 proxy entry.
        let mapped: SocketAddr = "[::ffff:10.0.0.1]:5000".parse().unwrap();
        assert_eq!(
            source(Some(mapped), &xff(&["203.0.113.7"]), &[proxy]),
            ip("203.0.113.7")
        );
    }

    /// LI: nginx's `proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for` appends the
    /// address it saw to whatever the client sent. A client that sends a value that isn't text
    /// (or isn't an address) spoils only its own entry: the address nginx appended, on the
    /// right, is still the client's, and the client is never counted as the proxy.
    #[test]
    fn a_malformed_client_entry_with_an_appending_proxy_is_its_own_entry() {
        let peer: SocketAddr = "10.201.77.1:5000".parse().unwrap();
        let proxy: IpAddr = "10.201.77.1".parse().unwrap();
        let client = Ok("203.0.113.7".parse().ok());
        let nginx = |client_sent: &[u8]| {
            let mut v = client_sent.to_vec();
            v.extend_from_slice(b", 203.0.113.7");
            let mut h = HeaderMap::new();
            h.insert(
                "x-forwarded-for",
                HeaderValue::from_bytes(&v).expect("a header value"),
            );
            h
        };
        for sent in [
            b"\xff\xfe".as_slice(),
            b"caf\xc3\xa9, \x80",
            b"unknown",
            b"198.51.100.66",
            b"[::1",
            b"1.2.3.4:99999",
            b"",
        ] {
            assert!(
                HeaderValue::from_bytes(sent).is_ok(),
                "{sent:?} is a header value a client can send"
            );
            assert_eq!(
                source(Some(peer), &nginx(sent), &[proxy]),
                client,
                "{sent:?}"
            );
        }
        // Without a client value nginx sends just the address.
        assert_eq!(source(Some(peer), &xff(&["203.0.113.7"]), &[proxy]), client);
    }

    #[test]
    fn the_cookies_are_host_only_http_only_and_strict() {
        assert_eq!(
            cookie(PENDING_COOKIE, "t", 300),
            "__Host-pmos-pending=t; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=300"
        );
        assert!(clear(SETUP_COOKIE).ends_with("Max-Age=0"));
        let mut h = HeaderMap::new();
        let v = "a".repeat(43);
        h.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("x=1; {SETUP_COOKIE}={v}")).unwrap(),
        );
        assert_eq!(read_cookie(&h, SETUP_COOKIE), Some(v));
        assert_eq!(read_cookie(&h, PENDING_COOKIE), None);
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("__Host-pmos-setup=short"),
        );
        assert_eq!(read_cookie(&h, SETUP_COOKIE), None);
    }
}
