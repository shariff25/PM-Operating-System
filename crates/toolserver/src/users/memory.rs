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

use super::import::ImportRequest;
use super::rules::{self, denied_event, Rules};
use super::{
    audit_event, Actor, Applied, ChangeKind, Directory, Op, RoleChange, UserResult, UserStore,
    ROLE_CHANGES_RETENTION_DAYS,
};
use crate::audit::{AuditEvent, AuditSink, Client, Outcome};
use crate::store::StoreResult;

#[derive(Default)]
struct State {
    dir: Directory,
    changes: Vec<RoleChange>,
    next_id: i64,
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
        }
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

    async fn apply(&self, actor: &Actor, op: Op, now: DateTime<Utc>) -> UserResult<Applied> {
        // One write at a time, as the database's advisory lock does.
        let mut state = self.state.lock().await;
        let dir = state.directory(now);
        match rules::plan(&dir, actor, &op, now, &self.rules) {
            Ok(plan) => {
                // The events first: if the audit log can't take them, nothing changes.
                self.record(
                    plan.events
                        .iter()
                        .map(|e| audit_event(actor, e, now))
                        .collect(),
                )
                .await?;
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
        }])
        .await?;
        Ok(deleted)
    }
}
