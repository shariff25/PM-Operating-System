//! The tool server's view of the user database for its own decisions (slice U3,
//! `docs/user-database-spec.md` §2): who may have a session, decide an approval, link an
//! account, read the admin screens, approve a join, and who counts as on the product team for
//! a restricted draft.
//!
//! **Fresh for decisions.** [`People::directory`] reads the store every time. Only an open MCP
//! session's re-check on each call goes through a cache ([`People::cached`], at most
//! [`SESSION_RECHECK`] old), and every write through [`People::apply`] empties it, so a change
//! made here is seen at once; a host command (another process) within 5 seconds. A
//! deactivation also ends the person's sessions in its own transaction.
//!
//! **Who may use PM OS** ([`may_use`]): an active user holding `pm`, `reviewer` or `admin`.
//! An auditor-only person reads metadata in the web app and is never a member here: no MCP
//! session, no approval, no account link, and an outsider to a restricted draft (§6).
//!
//! **No open directory in release binaries.** With fake connectors and no seed, nobody may
//! use PM OS: the dev and eval stacks load a seed (`PMOS_USERS_SEED`). An open directory, where
//! any valid handle may have a session, exists for tests only (the `test-support` feature).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration as StdDuration, Instant};

use chrono::Utc;
use pmos_contract::model::Role;
use pmos_contract::permissions::{self, Permission};

use super::{Actor, Applied, Directory, IdentityKind, Op, User, UserResult, UserRole, UserStore};
use crate::session::{is_bot_identity, valid_handle};
use crate::store::StoreResult;

/// How old a directory an open MCP session's re-check may use (§2: "5-second in-process
/// cache").
pub const SESSION_RECHECK: StdDuration = StdDuration::from_secs(5);

/// May `u` use PM OS (ask bots, have a session, decide their own drafts, link accounts)? An
/// active person holding `pm`, `reviewer` or `admin`. Also who counts as on the product team
/// for a restricted draft's audience (§6 "Audience insiders").
pub fn may_use(u: &User) -> bool {
    u.active() && (u.has(UserRole::Pm) || u.has(UserRole::Reviewer) || u.has(UserRole::Admin))
}

/// `u`'s roles in the permission matrix's terms.
pub fn contract_roles(u: &User) -> Vec<Role> {
    u.roles
        .keys()
        .map(|r| match r {
            UserRole::Admin => Role::Admin,
            UserRole::Pm => Role::Pm,
            UserRole::Reviewer => Role::Reviewer,
            UserRole::Auditor => Role::Auditor,
        })
        .collect()
}

/// Does `u` hold `p`? Never for a deactivated person (their kept roles grant nothing).
pub fn permits(u: &User, p: Permission) -> bool {
    u.active() && permissions::allows(&contract_roles(u), p)
}

/// The directory the tool server decides from.
pub struct People {
    store: Arc<dyn UserStore>,
    open: bool,
    cache: Mutex<Option<(Instant, Arc<Directory>)>>,
    misses: ResolveMisses,
}

impl People {
    /// Enforced: only [`may_use`] people may use PM OS.
    pub fn new(store: Arc<dyn UserStore>) -> Self {
        Self {
            store,
            open: false,
            cache: Mutex::new(None),
            misses: ResolveMisses::default(),
        }
    }

    /// Tests only: any valid handle may have a session, decide its own drafts and link
    /// accounts, unless the directory lists it and it may not. Everything else (admins,
    /// audiences, the admin reads) still reads the directory.
    #[cfg(any(test, feature = "test-support"))]
    pub fn open(store: Arc<dyn UserStore>) -> Self {
        Self {
            open: true,
            ..Self::new(store)
        }
    }

    /// Whether the directory decides who may use PM OS (always, outside tests).
    pub fn enforced(&self) -> bool {
        !self.open
    }

    pub fn store(&self) -> &Arc<dyn UserStore> {
        &self.store
    }

    /// The resolve-miss counter behind the overview's warning (§2).
    pub fn resolve_misses(&self) -> &ResolveMisses {
        &self.misses
    }

    /// The directory now, read from the store (no cache): for every decision.
    pub async fn directory(&self) -> StoreResult<Arc<Directory>> {
        let dir = Arc::new(self.store.snapshot(Utc::now()).await?);
        *self.cache.lock().expect("directory cache") = Some((Instant::now(), dir.clone()));
        Ok(dir)
    }

    /// The directory at most [`SESSION_RECHECK`] old: an open MCP session's re-check only.
    pub async fn cached(&self) -> StoreResult<Arc<Directory>> {
        if let Some((at, dir)) = self.cache.lock().expect("directory cache").as_ref() {
            if at.elapsed() < SESSION_RECHECK {
                return Ok(dir.clone());
            }
        }
        self.directory().await
    }

    /// The last directory read, however old, without reading the store: for display only
    /// (a direct message's other person on a draft), never for a decision.
    pub fn snapshot(&self) -> Option<Arc<Directory>> {
        self.cache
            .lock()
            .expect("directory cache")
            .as_ref()
            .map(|(_, d)| d.clone())
    }

    /// Forget the cached directory: the next re-check reads the store.
    pub fn invalidate(&self) {
        *self.cache.lock().expect("directory cache") = None;
    }

    /// Apply a user write, then forget the cache so the change is seen at once.
    pub async fn apply(
        &self,
        actor: &Actor,
        op: Op,
        now: chrono::DateTime<Utc>,
    ) -> UserResult<Applied> {
        let result = self.store.apply(actor, op, now).await;
        self.invalidate();
        result
    }

    fn allows_in(&self, dir: &Directory, handle: &str) -> bool {
        if is_bot_identity(handle) || !valid_handle(handle) {
            return false;
        }
        match dir.get(handle) {
            Some(u) => may_use(u),
            None => self.open,
        }
    }

    /// May `handle` use PM OS now? Never a team bot's service identity.
    pub async fn allows(&self, handle: &str) -> StoreResult<bool> {
        Ok(self.allows_in(&*self.directory().await?, handle))
    }

    /// [`People::allows`] on a directory at most [`SESSION_RECHECK`] old.
    pub async fn allows_cached(&self, handle: &str) -> StoreResult<bool> {
        Ok(self.allows_in(&*self.cached().await?, handle))
    }

    /// The person with this handle, active or not. Never a team bot's service identity.
    pub async fn user(&self, handle: &str) -> StoreResult<Option<User>> {
        if is_bot_identity(handle) {
            return Ok(None);
        }
        Ok(self.directory().await?.get(handle).cloned())
    }

    /// Is `handle` an active admin now?
    pub async fn is_admin(&self, handle: &str) -> StoreResult<bool> {
        Ok(self
            .user(handle)
            .await?
            .is_some_and(|u| u.active() && u.has(UserRole::Admin)))
    }

    /// The person whose Slack identity is `slack_id`, active or not.
    pub async fn by_slack_id(&self, slack_id: &str) -> StoreResult<Option<User>> {
        Ok(by_slack_id(&*self.directory().await?, slack_id).cloned())
    }
}

/// The person whose Slack identity is `slack_id`, active or not.
pub fn by_slack_id<'a>(dir: &'a Directory, slack_id: &str) -> Option<&'a User> {
    dir.users.values().find(|u| {
        u.identity(IdentityKind::Slack)
            .is_some_and(|i| i.subject == slack_id)
    })
}

/// Misses of `GET /internal/users/resolve` (slice U2), counted per client over the last hour.
/// More than [`ResolveMisses::WARN_PER_HOUR`] from one client raises a warning on the admin
/// overview (§2). The route itself rate-limits misses; this only remembers them.
#[derive(Default)]
pub struct ResolveMisses {
    by_client: Mutex<std::collections::BTreeMap<String, VecDeque<Instant>>>,
}

impl ResolveMisses {
    /// More misses than this in an hour from one client warn the admins.
    pub const WARN_PER_HOUR: usize = 100;
    const WINDOW: StdDuration = StdDuration::from_secs(3600);

    /// Count one miss from `client` (a token's client name, never a value looked up).
    pub fn record(&self, client: &str) {
        let mut all = self.by_client.lock().expect("resolve misses");
        let q = all.entry(client.to_string()).or_default();
        q.push_back(Instant::now());
        // Keep a little more than the threshold: enough to know it was passed.
        while q.len() > Self::WARN_PER_HOUR + 1 {
            q.pop_front();
        }
    }

    /// The clients past the threshold within the last hour.
    pub fn warning(&self) -> Vec<String> {
        let mut all = self.by_client.lock().expect("resolve misses");
        let mut out = Vec::new();
        for (client, q) in all.iter_mut() {
            while q.front().is_some_and(|t| t.elapsed() > Self::WINDOW) {
                q.pop_front();
            }
            if q.len() > Self::WARN_PER_HOUR {
                out.push(client.clone());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::users::{MemoryUsers, NewUser};

    async fn people(open: bool) -> People {
        let store: Arc<dyn UserStore> = Arc::new(MemoryUsers::default());
        let host = Actor::host(super::super::HostUser::parse("ops").unwrap());
        store
            .apply(
                &host,
                Op::BootstrapAdmin {
                    user: NewUser {
                        handle: "raj".into(),
                        slack_id: Some("U0RAJ".into()),
                        ..Default::default()
                    },
                    entra: None,
                },
                Utc::now(),
            )
            .await
            .unwrap();
        store
            .apply(
                &host,
                Op::CreateUser {
                    user: NewUser {
                        handle: "aud".into(),
                        roles: [UserRole::Auditor].into(),
                        ..Default::default()
                    },
                },
                Utc::now(),
            )
            .await
            .unwrap();
        if open {
            People::open(store)
        } else {
            People::new(store)
        }
    }

    #[tokio::test]
    async fn only_active_pms_reviewers_and_admins_use_pm_os() {
        let p = people(false).await;
        assert!(p.allows("raj").await.unwrap());
        assert!(!p.allows("aud").await.unwrap(), "auditor-only");
        assert!(!p.allows("eve").await.unwrap(), "not a user");
        assert!(!p.allows("bots.docs").await.unwrap());
        assert!(p.is_admin("raj").await.unwrap());
        assert_eq!(
            p.by_slack_id("U0RAJ").await.unwrap().map(|u| u.handle),
            Some("raj".into())
        );
        let open = people(true).await;
        assert!(open.allows("eve").await.unwrap(), "open: anyone not listed");
        assert!(!open.allows("aud").await.unwrap(), "listed and may not");
        assert!(!open.allows("bots.docs").await.unwrap());
    }

    #[test]
    fn misses_warn_past_the_threshold_per_client() {
        let m = ResolveMisses::default();
        for _ in 0..ResolveMisses::WARN_PER_HOUR {
            m.record("web");
        }
        assert!(m.warning().is_empty());
        m.record("web");
        assert_eq!(m.warning(), vec!["web".to_string()]);
    }
}
