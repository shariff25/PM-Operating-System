//! The in-memory [`UserStore`]: tests, `PMOS_DEV=1` without a database, and the dev seed.
//!
//! Every write goes through the same plan as Postgres, and the planned directory is checked
//! against the same invariants (`rules::apply_writes` for the tables' keys and checks,
//! `rules::check_invariants` for the trigger) before it replaces the current one, under one
//! lock, so concurrent writes are serialized as the database's advisory lock serializes them.
//! The shared tests in `tests/users.rs` run the same cases against both stores.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use tokio::sync::Mutex;

use std::collections::BTreeMap;

use super::import::ImportRequest;
use super::link::{self, EntraLink, LinkError, LinkRefusal, LinkResult, Linked, Planned};
use super::rules::{self, denied_event, Effect, Rules};
use super::{
    audit_event, Actor, Applied, ChangeKind, Directory, Op, RoleChange, UserResult, UserStore,
    ROLE_CHANGES_RETENTION_DAYS,
};
use crate::audit::{AuditEvent, AuditSink, Client, Outcome};
use crate::password::limits::{Counter, SourceCounters};
use crate::password::store::{
    Credential, PasswordStatus, PasswordStore, Pending, Person, Plan, Purpose, RecoveryCode,
    SetupCode, Snapshot, Writes,
};
use crate::store::StoreResult;

#[derive(Default)]
struct State {
    dir: Directory,
    changes: Vec<RoleChange>,
    next_id: i64,
    /// Password sign-in (`crate::password`), under the same lock as the directory.
    pw: Passwords,
}

/// One setup code as stored, with its owner and end.
#[derive(Debug, Clone)]
struct StoredCode {
    handle: String,
    code: SetupCode,
    used_at: Option<DateTime<Utc>>,
    revoked_at: Option<DateTime<Utc>>,
    /// Void after too many wrong guesses (migration 0023).
    voided_at: Option<DateTime<Utc>>,
}

/// Password sign-in's tables, in memory.
#[derive(Default)]
struct Passwords {
    credentials: BTreeMap<String, Credential>,
    recovery: BTreeMap<String, Vec<RecoveryCode>>,
    codes: Vec<StoredCode>,
    /// By token hash: the owner and the token.
    pending: BTreeMap<String, (String, Pending)>,
    next_code_id: i64,
    /// By (handle, source key): one person's counters for one address.
    sources: BTreeMap<(String, String), SourceCounters>,
    /// The ID tokens already used for a first Entra link, by key, until when (M4; in
    /// Postgres, `entra_used_tokens`).
    used_tokens: BTreeMap<String, DateTime<Utc>>,
}

impl Passwords {
    fn live_code(&self, handle: &str) -> Option<&SetupCode> {
        self.codes
            .iter()
            .find(|c| c.handle == handle && c.used_at.is_none() && c.revoked_at.is_none())
            .map(|c| &c.code)
    }

    /// Everything about `handle` goes (the deactivation grace is over).
    fn forget(&mut self, handle: &str) {
        self.credentials.remove(handle);
        self.recovery.remove(handle);
        self.codes.retain(|c| c.handle != handle);
        self.pending.retain(|_, (h, _)| h != handle);
        self.sources.retain(|(h, _), _| h != handle);
    }
}

fn person_of(u: &super::User) -> Person {
    Person {
        handle: u.handle.clone(),
        status: u.status,
        roles: u.role_set(),
        sign_in_name: u.sign_in_name.clone(),
        sign_in_name_confirmed: u.sign_in_name_confirmed_at.is_some(),
    }
}

/// What the database's checks on the password tables refuse (migration 0020), checked the
/// same way here before anything is written.
fn check_writes(handle: &str, exists: bool, w: &Writes, now: DateTime<Utc>) -> StoreResult<()> {
    let bad = |why: &str| {
        Err(crate::store::StoreError(format!(
            "password write refused: {why}"
        )))
    };
    let writes_rows = w.credential.is_some()
        || w.recovery.is_some()
        || w.issue_code.is_some()
        || w.source.is_some()
        || !w.put_pending.is_empty();
    if writes_rows && (!exists || handle == super::EVALS) {
        return bad("no such user, or the evals account");
    }
    if let Some(c) = &w.credential {
        if c.password_hash.is_some() != c.totp_sealed.is_some()
            || c.password_hash.is_some() != c.set_at.is_some()
        {
            return bad("a password without its second step");
        }
        if c.password_hash
            .as_ref()
            .is_some_and(|h| !h.starts_with("$argon2id$"))
        {
            return bad("not an Argon2id hash");
        }
    }
    if let Some(code) = &w.issue_code {
        if code.expires_at <= now || code.expires_at > now + chrono::Duration::hours(24) {
            return bad("a setup code lives at most 24 hours");
        }
    }
    for p in &w.put_pending {
        let setup = p.purpose == Purpose::Setup;
        if p.expires_at <= p.created_at
            || p.expires_at > p.created_at + chrono::Duration::minutes(15)
            || setup != p.setup_code_id.is_some()
            || (!setup && (p.staged_hash.is_some() || p.staged_totp.is_some()))
        {
            return bad("a pending token's shape");
        }
    }
    Ok(())
}

impl State {
    fn directory(&self, now: DateTime<Utc>) -> Directory {
        let mut dir = self.dir.clone();
        let since = now - super::deactivation_window();
        dir.capped_deactivations = self
            .changes
            .iter()
            .filter(|c| c.change == ChangeKind::Deactivate && c.via.capped())
            .filter(|c| c.at > since && c.at <= now)
            .map(|c| c.at)
            .collect();
        dir
    }
}

pub struct MemoryUsers {
    state: Mutex<State>,
    rules: Rules,
    audit: Option<Arc<dyn AuditSink>>,
    linked: Option<LinkedStores>,
}

/// The stores a deactivation and a reactivation reach beyond the users tables (§6). In
/// Postgres they are the same database and change in the same transaction; here they change
/// right after the directory, under the same lock.
#[derive(Clone)]
pub struct LinkedStores {
    pub sessions: Arc<dyn crate::session::SessionRepo>,
    pub credentials: Arc<dyn crate::vault::CredentialStore>,
    pub approvals: Arc<dyn crate::approvals::ApprovalRepo>,
}

impl Default for MemoryUsers {
    fn default() -> Self {
        Self::new(Rules::default())
    }
}

impl MemoryUsers {
    pub fn new(rules: Rules) -> Self {
        let mut state = State::default();
        // As the migration seeds `directory_version`.
        state.dir.version = 1;
        Self {
            state: Mutex::new(state),
            rules,
            audit: None,
            linked: None,
        }
    }

    /// Run deactivations' and reactivations' effects on these stores.
    pub fn with_linked(mut self, linked: LinkedStores) -> Self {
        self.linked = Some(linked);
        self
    }

    async fn run(&self, effects: &[Effect], now: DateTime<Utc>) -> StoreResult<()> {
        let Some(l) = &self.linked else {
            return Ok(());
        };
        for effect in effects {
            match effect {
                Effect::RevokeSessions(h) => {
                    l.sessions.revoke_user(h).await?;
                }
                Effect::SuspendCredentials(h) => {
                    l.credentials.suspend(h, now).await?;
                }
                Effect::RestoreCredentials(h) => {
                    l.credentials.restore(h).await?;
                }
                Effect::DeleteSuspendedCredentials(h) => {
                    l.credentials.delete_suspended(h).await?;
                }
                Effect::CloseHeldDrafts(h) => {
                    l.approvals.close_for_owner(h, now).await?;
                }
                Effect::DeleteCredential(h, provider) => {
                    l.credentials.delete(h, *provider).await?;
                }
            }
        }
        Ok(())
    }

    /// Record each write's audit events here too.
    pub fn with_audit(mut self, audit: Arc<dyn AuditSink>) -> Self {
        self.audit = Some(audit);
        self
    }

    /// A store holding the dev seed (`PMOS_USERS_SEED`): `req` applied as a bootstrap import
    /// by the `import` actor, with no host user. The same rules apply as to any import: an
    /// unknown role, a duplicate key, a seed with users but no active admin are refused.
    pub async fn seeded(
        req: ImportRequest,
        rules: Rules,
        audit: Option<Arc<dyn AuditSink>>,
    ) -> Result<Self, String> {
        let mut store = Self::new(rules);
        store.audit = audit;
        store
            .apply(&Actor::import(None), Op::Import(Box::new(req)), Utc::now())
            .await
            .map_err(|e| format!("PMOS_USERS_SEED: {e}"))?;
        Ok(store)
    }

    async fn record(&self, events: Vec<AuditEvent>) -> StoreResult<()> {
        if let Some(audit) = &self.audit {
            for e in events {
                audit.record(e).await?;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl UserStore for MemoryUsers {
    fn rules(&self) -> Rules {
        self.rules
    }

    async fn directory(&self, now: DateTime<Utc>) -> StoreResult<Directory> {
        Ok(self.state.lock().await.directory(now))
    }

    async fn apply_expecting(
        &self,
        actor: &Actor,
        op: Op,
        now: DateTime<Utc>,
        expected: Option<i64>,
    ) -> UserResult<Applied> {
        // One write at a time, as the database's advisory lock does.
        let mut state = self.state.lock().await;
        let dir = state.directory(now);
        match rules::plan_expecting(&dir, actor, &op, now, &self.rules, expected) {
            Ok(plan) => {
                // The events first: if the audit log can't take them, nothing changes.
                self.record(
                    plan.events
                        .iter()
                        .map(|e| audit_event(actor, e, now))
                        .collect(),
                )
                .await?;
                // The effects next, still under the lock: a failure leaves the directory as
                // it was (an effect already run is idempotent on a retry).
                self.run(&plan.effects, now).await?;
                for effect in &plan.effects {
                    if let Effect::DeleteSuspendedCredentials(h) = effect {
                        state.pw.forget(h);
                    }
                }
                let mut next = plan.next;
                next.capped_deactivations.clear();
                state.dir = next;
                for mut change in plan.changes {
                    state.next_id += 1;
                    change.id = state.next_id;
                    state.changes.push(change);
                }
                Ok(Applied { lines: plan.lines })
            }
            Err(e) => {
                drop(state);
                if let Some(refusal) = e.refusal() {
                    self.record(vec![audit_event(
                        actor,
                        &denied_event(actor, &op, refusal),
                        now,
                    )])
                    .await?;
                }
                Err(e)
            }
        }
    }

    async fn role_changes(
        &self,
        target: Option<&str>,
        limit: usize,
    ) -> StoreResult<Vec<RoleChange>> {
        let state = self.state.lock().await;
        Ok(state
            .changes
            .iter()
            .rev()
            .filter(|c| target.is_none_or(|t| c.target == t))
            .take(limit)
            .cloned()
            .collect())
    }

    async fn purge_role_changes(&self, now: DateTime<Utc>) -> StoreResult<u64> {
        let mut state = self.state.lock().await;
        let cutoff = now - Duration::days(ROLE_CHANGES_RETENTION_DAYS);
        let gone: Vec<String> = state
            .dir
            .users
            .values()
            .filter(|u| !u.active() && u.deactivated_at.is_some_and(|at| at < cutoff))
            .map(|u| u.handle.clone())
            .collect();
        let before = state.changes.len();
        state
            .changes
            .retain(|c| c.target == "*" || !gone.contains(&c.target));
        let deleted = (before - state.changes.len()) as u64;
        drop(state);
        self.record(vec![AuditEvent {
            at: now,
            client: Client::System,
            run_id: "retention".into(),
            user: crate::audit::PURGE_ACTOR.into(),
            skill: None,
            bot: None,
            tool: "users.role_changes_purge".into(),
            outcome: Outcome::Ok,
            sources: vec![],
            source_titles: Default::default(),
            args_hash: None,
            output_ref: None,
            detail: Some(format!("purged {deleted} role changes")),
            persona: None,
        }])
        .await?;
        Ok(deleted)
    }

    async fn link_entra(&self, link: &EntraLink, now: DateTime<Utc>) -> LinkResult {
        self.first_entra_link(link, now).await
    }

    async fn first_token_use(
        &self,
        key: &str,
        expires_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> StoreResult<bool> {
        let mut state = self.state.lock().await;
        let used = &mut state.pw.used_tokens;
        used.retain(|_, until| *until > now);
        if used.contains_key(key) {
            return Ok(false);
        }
        used.insert(key.to_string(), expires_at);
        Ok(true)
    }

    fn passwords(&self) -> &dyn PasswordStore {
        self
    }
}

// --- Password sign-in (crate::password) -------------------------------------------------------

#[async_trait]
impl PasswordStore for MemoryUsers {
    async fn source_known(&self, source_key: &str, since: DateTime<Utc>) -> StoreResult<bool> {
        let state = self.state.lock().await;
        Ok(state
            .pw
            .sources
            .iter()
            .any(|((_, k), s)| k == source_key && s.last_success_at.is_some_and(|at| at > since)))
    }

    async fn password_overview(
        &self,
        since: DateTime<Utc>,
    ) -> StoreResult<crate::password::store::PasswordOverview> {
        let state = self.state.lock().await;
        let resets = state
            .changes
            .iter()
            .filter(|c| {
                c.change == ChangeKind::IssuePasswordCode
                    && c.detail.as_deref() == Some("reset")
                    && c.at >= since
            })
            .map(|c| (c.target.clone(), c.actor.clone(), c.at))
            .collect();
        let dismissals: Vec<(String, DateTime<Utc>)> = state
            .changes
            .iter()
            .filter(|c| c.change == ChangeKind::DismissResetNotice && c.at >= since)
            .map(|c| (c.target.clone(), c.at))
            .collect();
        let mut voided: Vec<crate::password::store::VoidedCode> = state
            .pw
            .codes
            .iter()
            .filter_map(|c| {
                c.voided_at
                    .filter(|at| *at >= since)
                    .map(|at| crate::password::store::VoidedCode {
                        handle: c.handle.clone(),
                        at,
                        wrong_guesses: c.code.wrong_guesses,
                    })
            })
            .collect();
        voided.sort_by_key(|v| std::cmp::Reverse(v.at));
        Ok(crate::password::store::PasswordOverview {
            resets: crate::password::store::pair_resets(resets, &dismissals),
            voided,
        })
    }

    async fn handle_by_sign_in_name(&self, name: &str) -> StoreResult<Option<String>> {
        let state = self.state.lock().await;
        Ok(state
            .dir
            .users
            .values()
            .find(|u| u.sign_in_name.as_deref() == Some(name))
            .map(|u| u.handle.clone()))
    }

    async fn handle_by_token(&self, token_hash: &str) -> StoreResult<Option<String>> {
        let state = self.state.lock().await;
        Ok(state.pw.pending.get(token_hash).map(|(h, _)| h.clone()))
    }

    async fn password_tx(
        &self,
        handle: &str,
        actor: Option<&str>,
        source: Option<&str>,
        now: DateTime<Utc>,
        plan: Plan<'_>,
    ) -> StoreResult<()> {
        // One person at a time, as the database's lock does; here the one lock covers all.
        let mut state = self.state.lock().await;
        let source_row = source.map(|k| (handle.to_string(), k.to_string()));
        let snapshot = Snapshot {
            source: source_row
                .as_ref()
                .and_then(|k| state.pw.sources.get(k).copied())
                .unwrap_or_default(),
            person: state.dir.get(handle).map(person_of),
            actor: actor.and_then(|a| state.dir.get(a)).map(person_of),
            credential: state.pw.credentials.get(handle).cloned(),
            recovery: state.pw.recovery.get(handle).cloned().unwrap_or_default(),
            setup_code: state.pw.live_code(handle).cloned(),
            pending: state
                .pw
                .pending
                .values()
                .filter(|(h, _)| h == handle)
                .map(|(_, p)| p.clone())
                .collect(),
        };
        let w = plan(&snapshot);
        check_writes(handle, snapshot.person.is_some(), &w, now)?;
        // The events first: if the audit log can't take them, nothing changes.
        self.record(w.events.clone()).await?;
        let pw = &mut state.pw;
        if let Some(c) = w.credential {
            pw.credentials.insert(handle.to_string(), c);
        }
        for ((h, _), s) in pw.sources.iter_mut() {
            if h != handle {
                continue;
            }
            if w.reset_sources {
                s.sign_in = Counter::default();
            }
            if w.reset_sources || w.reset_source_setup {
                s.setup = Counter::default();
            }
        }
        if let (Some(s), Some(k)) = (w.source, source_row) {
            pw.sources.insert(k, s);
        }
        if let Some(hashes) = w.recovery {
            pw.recovery.insert(
                handle.to_string(),
                hashes
                    .into_iter()
                    .map(|hash| RecoveryCode {
                        hash,
                        used_at: None,
                    })
                    .collect(),
            );
        }
        if let Some(used) = w.use_recovery {
            if let Some(codes) = pw.recovery.get_mut(handle) {
                for c in codes.iter_mut().filter(|c| c.hash == used) {
                    c.used_at.get_or_insert(now);
                }
            }
        }
        if let Some(new) = w.issue_code {
            for c in pw
                .codes
                .iter_mut()
                .filter(|c| c.handle == handle && c.used_at.is_none() && c.revoked_at.is_none())
            {
                c.revoked_at = Some(now);
            }
            if pw.codes.iter().any(|c| c.code.hash == new.hash) {
                return Err(crate::store::StoreError("a setup code collided".into()));
            }
            pw.next_code_id += 1;
            pw.codes.push(StoredCode {
                handle: handle.to_string(),
                code: SetupCode {
                    id: pw.next_code_id,
                    hash: new.hash,
                    kind: new.kind,
                    issued_at: now,
                    issued_by: new.issued_by,
                    issued_via: new.issued_via,
                    expires_at: new.expires_at,
                    begins: 0,
                    password_sets: 0,
                    wrong_guesses: 0,
                },
                used_at: None,
                revoked_at: None,
                voided_at: None,
            });
        }
        if let Some(counts) = w.code_counts {
            for c in pw.codes.iter_mut().filter(|c| {
                c.code.id == counts.id
                    && c.handle == handle
                    && c.used_at.is_none()
                    && c.revoked_at.is_none()
            }) {
                c.code.begins = counts.begins;
                c.code.password_sets = counts.password_sets;
                c.code.wrong_guesses = counts.wrong_guesses;
                if counts.void {
                    c.revoked_at = Some(now);
                    c.voided_at = Some(now);
                }
            }
        }
        if let Some(id) = w.use_code {
            for c in pw
                .codes
                .iter_mut()
                .filter(|c| c.code.id == id && c.handle == handle)
            {
                c.used_at.get_or_insert(now);
            }
            // A token on a code that is gone goes with it.
            pw.pending
                .retain(|_, (_, p)| p.setup_code_id.is_none_or(|code| code != id));
        }
        if w.clear_pending {
            pw.pending.retain(|_, (h, _)| h != handle);
        }
        for t in &w.delete_pending {
            if pw.pending.get(t).is_some_and(|(h, _)| h == handle) {
                pw.pending.remove(t);
            }
        }
        for p in w.put_pending {
            if pw
                .pending
                .get(&p.token_hash)
                .is_some_and(|(h, _)| h != handle)
            {
                return Err(crate::store::StoreError("a token collided".into()));
            }
            pw.pending
                .insert(p.token_hash.clone(), (handle.to_string(), p));
        }
        for mut change in w.changes {
            state.next_id += 1;
            change.id = state.next_id;
            state.changes.push(change);
        }
        Ok(())
    }

    async fn password_events(&self, events: Vec<AuditEvent>) -> StoreResult<()> {
        self.record(events).await
    }

    async fn password_statuses(
        &self,
        now: DateTime<Utc>,
    ) -> StoreResult<BTreeMap<String, PasswordStatus>> {
        let state = self.state.lock().await;
        let mut out = BTreeMap::new();
        let handles: std::collections::BTreeSet<&String> = state
            .pw
            .credentials
            .keys()
            .chain(state.pw.codes.iter().map(|c| &c.handle))
            .collect();
        for h in handles {
            out.insert(h.clone(), status_of(&state, h, now));
        }
        Ok(out)
    }

    async fn password_status(
        &self,
        handle: &str,
        now: DateTime<Utc>,
    ) -> StoreResult<PasswordStatus> {
        let state = self.state.lock().await;
        Ok(status_of(&state, handle, now))
    }

    async fn prune_passwords(&self, now: DateTime<Utc>) -> StoreResult<u64> {
        let mut state = self.state.lock().await;
        let pw = &mut state.pw;
        let before = pw.pending.len() + pw.codes.len() + pw.sources.len();
        pw.pending.retain(|_, (_, p)| p.expires_at > now);
        // A void code stays for the overview's window.
        let kept = chrono::Duration::days(crate::password::store::VOIDED_KEPT_DAYS);
        pw.codes
            .retain(|c| c.code.expires_at > now || c.voided_at.is_some_and(|at| at > now - kept));
        pw.used_tokens.retain(|_, until| *until > now);
        pw.sources.retain(|_, s| !s.is_idle(now));
        Ok((before - pw.pending.len() - pw.codes.len() - pw.sources.len()) as u64)
    }
}

fn status_of(state: &State, handle: &str, now: DateTime<Utc>) -> PasswordStatus {
    let pw = &state.pw;
    let person = state.dir.get(handle).map(person_of);
    PasswordStatus::of(
        person.as_ref(),
        pw.credentials.get(handle),
        pw.live_code(handle).is_some_and(|c| c.expires_at > now),
        pw.recovery.get(handle).map_or(0, |r| {
            r.iter().filter(|c| c.used_at.is_none()).count() as u32
        }),
    )
}

// --- The first Entra link (slice U2) ---------------------------------------------------------

impl MemoryUsers {
    /// As in Postgres: planned under the one lock, the write checked against the tables' keys
    /// (`rules::apply_writes`: one person per key, one key of each kind per person) and the
    /// invariants, the audit event recorded first, then committed with the version bumped.
    async fn first_entra_link(&self, link: &EntraLink, now: DateTime<Utc>) -> LinkResult {
        let mut state = self.state.lock().await;
        let dir = state.directory(now);
        let refuse = |why: LinkRefusal, dir: &Directory| {
            let (actor, event) = link::refused_event(Some(dir), &link.handle, why);
            link::audit(&actor, &event, now)
        };
        let plan = match link::plan(&dir, link, now) {
            Ok(Planned::Same(user)) => {
                return Ok(Linked {
                    user: *user,
                    first_link: false,
                })
            }
            Ok(Planned::Link(plan)) => plan,
            Err(why) => {
                drop(state);
                self.record(vec![refuse(why, &dir)]).await?;
                return Err(LinkError::Refused(why));
            }
        };
        let mut next = dir.clone();
        let checked =
            rules::apply_writes(&mut next, std::slice::from_ref(&plan.write)).and_then(|()| {
                rules::check_invariants(
                    &next,
                    &rules::Touched::People([plan.user.handle.clone()].into()),
                    now,
                    &self.rules,
                )
            });
        if let Err(e) = checked {
            drop(state);
            let why = match e.refusal() {
                Some(super::Refusal::EvalsAccount) => LinkRefusal::EvalsAccount,
                Some(_) => LinkRefusal::Taken,
                None => return Err(LinkError::Store(crate::store::StoreError(e.to_string()))),
            };
            self.record(vec![refuse(why, &dir)]).await?;
            return Err(LinkError::Refused(why));
        }
        self.record(vec![audit_event(&plan.actor, &plan.event, now)])
            .await?;
        next.capped_deactivations.clear();
        next.version += 1;
        state.dir = next;
        let mut change = plan.change.clone();
        state.next_id += 1;
        change.id = state.next_id;
        state.changes.push(change);
        Ok(Linked {
            user: plan.user.clone(),
            first_link: true,
        })
    }
}
