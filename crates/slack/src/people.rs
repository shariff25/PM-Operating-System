//! Who someone in Slack is, from the user database (`docs/user-database-spec.md` §2, slice
//! U4b): a snapshot of the tool server's `GET /internal/directory`, read with the Slack bot
//! token (`PMOS_BOT_TOKEN`), whose projection carries each user's Slack id. The Slack service
//! never reads `content/team.yaml`, and never falls back to it.
//!
//! **Bounds** (the spec's "When the store is unavailable", Slack row):
//!
//! | What | Snapshot at most | Past it |
//! | --- | --- | --- |
//! | Slack id → handle, and whether that person may use PM OS ([`People::identify`], [`People::of_handle`]) | 5 minutes (`for_slack_identity`) | [`Identity::CantCheck`]: "PM OS can't check who you are right now" |
//! | Anything only an admin may do: onboarding decisions, `requests`, `remove` ([`People::admin`]), and who gets a join card | 30 seconds (`fresh`) | Refused; nothing is sent to the tool server |
//! | The target of `remove` ([`People::target`]) | 30 seconds | Refused |
//! | Proactive messages: nudges, the triage digest ([`People::members`]) | 30 seconds | Nothing is sent this time |
//!
//! Everything a PM asks for in Slack runs through the runtime or the tool server, which
//! decide again from the database (the runtime on its own ≤ 30 s snapshot, the tool server at
//! once), so a 5-minute-old identity can only ever *route* a request, never grant one.
//!
//! **Who may use PM OS** in Slack: an active user holding `pm`, `reviewer` or `admin` (the
//! `AskBot` permission), as in the tool server and the runtime. An Auditor-only person is not
//! treated as a PM; a deactivated person is refused, and a status this build doesn't know
//! grants nothing.

use std::sync::Arc;

use pmos_contract::permissions::Permission;
use pmos_directory::snapshot::{DirectoryUser, Snapshot, SnapshotClient, UserStatus};

use crate::access_copy;

/// Who a Slack user (or a handle) is, for a decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    /// An active user who may use PM OS.
    Member(Member),
    /// Nobody in the directory: offered **Request access**.
    Stranger,
    /// A deactivated user: their access was turned off. No **Request access**.
    Deactivated,
    /// Active, but holding no role that uses PM OS in Slack (an Auditor only), or a status
    /// this build doesn't know.
    NoAccess,
    /// No snapshot within the bound: PM OS can't check who they are right now.
    CantCheck,
}

impl Identity {
    pub fn member(&self) -> Option<&Member> {
        match self {
            Identity::Member(m) => Some(m),
            _ => None,
        }
    }

    pub fn into_member(self) -> Option<Member> {
        match self {
            Identity::Member(m) => Some(m),
            _ => None,
        }
    }

    /// A short word for logs (never a name or an id).
    pub fn kind(&self) -> &'static str {
        match self {
            Identity::Member(_) => "member",
            Identity::Stranger => "stranger",
            Identity::Deactivated => "deactivated",
            Identity::NoAccess => "no_access",
            Identity::CantCheck => "cant_check",
        }
    }
}

/// A user who may use PM OS, as the snapshot that identified them says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub handle: String,
    /// Untrusted text from Slack: render with [`crate::blocks::name`].
    pub display_name: String,
    /// Their Slack id, if the directory has one.
    pub slack_id: Option<String>,
    /// Holds `admin` in the snapshot that identified them. Only a hint for reading a DM as a
    /// command: anything an admin may do checks [`People::admin`] on a fresh snapshot.
    pub admin: bool,
}

/// What [`People::admin`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminCheck {
    /// An active admin, on a snapshot at most 30 seconds old.
    Admin(Member),
    /// Not an active admin.
    No,
    /// No snapshot at most 30 seconds old: refuse, and say PM OS can't check.
    CantCheck,
}

/// The target of `remove`, on a fresh snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub handle: String,
    pub display_name: String,
    pub slack_id: Option<String>,
    pub active: bool,
    /// Holds `admin` or `auditor`: the Slack bot client can't deactivate them.
    pub protected: bool,
}

/// What [`People::target`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetCheck {
    Found(Target),
    /// Nobody has that handle.
    Nobody,
    /// No snapshot at most 30 seconds old.
    CantCheck,
}

impl TargetCheck {
    #[cfg(test)]
    fn unwrap(self) -> Target {
        match self {
            TargetCheck::Found(t) => t,
            other => panic!("no target: {other:?}"),
        }
    }
}

fn classify(user: Option<&DirectoryUser>) -> Identity {
    let Some(u) = user else {
        return Identity::Stranger;
    };
    match u.status {
        UserStatus::Deactivated => Identity::Deactivated,
        UserStatus::Unknown => Identity::NoAccess,
        UserStatus::Active if u.allows(Permission::AskBot) => Identity::Member(Member {
            handle: u.handle.clone(),
            display_name: u.display_name.clone(),
            slack_id: u.slack_id.clone(),
            admin: u.is_admin(),
        }),
        UserStatus::Active => Identity::NoAccess,
    }
}

/// What to tell someone who may not use PM OS: `stranger` for someone the directory doesn't
/// have (each surface words that its own way), the shared copy otherwise.
pub fn refusal(identity: &Identity, stranger: &'static str) -> &'static str {
    match identity {
        Identity::Deactivated => access_copy::DEACTIVATED,
        Identity::NoAccess => access_copy::NO_ACCESS,
        Identity::CantCheck => access_copy::CANT_CHECK,
        Identity::Stranger | Identity::Member(_) => stranger,
    }
}

/// The directory as the Slack service reads it. Cheap to clone; share one per process.
#[derive(Clone)]
pub struct People {
    client: Arc<SnapshotClient>,
}

impl People {
    pub fn new(client: Arc<SnapshotClient>) -> Self {
        Self { client }
    }

    /// The snapshot client (to poll, or to refresh after a change made through PM OS).
    pub fn client(&self) -> &Arc<SnapshotClient> {
        &self.client
    }

    /// Poll now, after a change this service asked the tool server for (an approval or a
    /// deactivation), so it shows at once rather than on the next poll. A failure keeps the
    /// snapshot as it was.
    pub async fn refresh(&self) {
        if let Err(e) = self.client.refresh().await {
            tracing::debug!(error = %e, "couldn't refresh the user directory after a change");
        }
    }

    /// The Slack user `slack_id`, on a snapshot at most 5 minutes old.
    pub fn identify(&self, slack_id: &str) -> Identity {
        match self.client.for_slack_identity() {
            Some(s) => classify(s.by_slack_id(slack_id)),
            None => Identity::CantCheck,
        }
    }

    /// The user `handle` (to send them a message), on a snapshot at most 5 minutes old.
    pub fn of_handle(&self, handle: &str) -> Identity {
        match self.client.for_slack_identity() {
            Some(s) => classify(s.user(handle)),
            None => Identity::CantCheck,
        }
    }

    /// Is the Slack user `slack_id` an active admin, on a snapshot at most 30 seconds old?
    pub fn admin(&self, slack_id: &str) -> AdminCheck {
        let Some(s) = self.client.fresh() else {
            return AdminCheck::CantCheck;
        };
        match classify(s.by_slack_id(slack_id)) {
            Identity::Member(m) if m.admin => AdminCheck::Admin(m),
            _ => AdminCheck::No,
        }
    }

    /// Every user who may use PM OS, on a snapshot at most 30 seconds old; `None` past it.
    pub fn members(&self) -> Option<Vec<Member>> {
        self.client.fresh().map(|s| members_of(&s))
    }

    /// Every active admin with a Slack id, on a snapshot at most 30 seconds old.
    pub fn admins(&self) -> Option<Vec<Member>> {
        self.members().map(|ms| {
            ms.into_iter()
                .filter(|m| m.admin && m.slack_id.is_some())
                .collect()
        })
    }

    /// The user `handle` as `remove` sees them, on a snapshot at most 30 seconds old.
    pub fn target(&self, handle: &str) -> TargetCheck {
        let Some(s) = self.client.fresh() else {
            return TargetCheck::CantCheck;
        };
        match s.user(handle) {
            Some(u) => TargetCheck::Found(Target {
                handle: u.handle.clone(),
                display_name: u.display_name.clone(),
                slack_id: u.slack_id.clone(),
                active: u.is_active(),
                protected: u.roles.iter().any(|r| r == "admin" || r == "auditor"),
            }),
            None => TargetCheck::Nobody,
        }
    }

    /// The handle of whoever has the Slack id `slack_id` (any status), on a snapshot at most
    /// 5 minutes old: how `remove <@U…>` names its target.
    pub fn handle_of(&self, slack_id: &str) -> Option<String> {
        self.client
            .for_slack_identity()
            .and_then(|s| s.by_slack_id(slack_id).map(|u| u.handle.clone()))
    }

    /// Is `handle` a user (any status) in a snapshot at most 30 seconds old? `None` past it.
    /// Handles are never reused, so a deactivated person's handle is taken too.
    pub fn handle_taken(&self, handle: &str) -> Option<bool> {
        self.client.fresh().map(|s| s.user(handle).is_some())
    }
}

fn members_of(s: &Snapshot) -> Vec<Member> {
    s.users
        .values()
        .filter_map(|u| classify(Some(u)).into_member())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pmos_directory::snapshot::{user, FakeClock, StaticFetcher, ALLOW_AGE, SLACK_IDENTITY_AGE};
    use std::time::Duration;

    fn with_slack(handle: &str, slack: &str, roles: &[&str]) -> DirectoryUser {
        DirectoryUser {
            slack_id: Some(slack.into()),
            ..user(handle, roles)
        }
    }

    async fn people(users: Vec<DirectoryUser>) -> (People, Arc<StaticFetcher>, Arc<FakeClock>) {
        let f = Arc::new(StaticFetcher::new(users));
        let clock = Arc::new(FakeClock::default());
        let client = Arc::new(SnapshotClient::new(f.clone(), clock.clone()));
        client.refresh().await.unwrap();
        (People::new(client), f, clock)
    }

    #[tokio::test]
    async fn who_may_use_pm_os() {
        let gone = DirectoryUser {
            status: UserStatus::Deactivated,
            ..with_slack("gone", "U0GONE", &["pm", "admin"])
        };
        let (p, _, _) = people(vec![
            with_slack("ana", "U0ANA", &["pm", "admin"]),
            with_slack("rev", "U0REV", &["reviewer"]),
            with_slack("aud", "U0AUD", &["auditor"]),
            with_slack("odd", "U0ODD", &["policy-steward"]),
            gone,
        ])
        .await;
        let ana = p.identify("U0ANA").into_member().unwrap();
        assert_eq!(ana.handle, "ana");
        assert!(ana.admin);
        assert!(p.identify("U0REV").member().is_some());
        assert_eq!(
            p.identify("U0AUD"),
            Identity::NoAccess,
            "an Auditor isn't a PM"
        );
        assert_eq!(p.identify("U0ODD"), Identity::NoAccess);
        assert_eq!(p.identify("U0GONE"), Identity::Deactivated);
        assert_eq!(p.identify("U0NOBODY"), Identity::Stranger);
        assert!(matches!(p.admin("U0ANA"), AdminCheck::Admin(_)));
        assert_eq!(
            p.admin("U0GONE"),
            AdminCheck::No,
            "kept roles grant nothing"
        );
        assert_eq!(p.admin("U0REV"), AdminCheck::No);
        let handles: Vec<String> = p.members().unwrap().into_iter().map(|m| m.handle).collect();
        assert_eq!(handles, ["ana", "rev"]);
        assert_eq!(p.admins().unwrap().len(), 1);
        assert!(p.target("ana").unwrap().protected);
        assert!(p.target("aud").unwrap().protected);
        assert!(!p.target("rev").unwrap().protected);
        assert!(!p.target("gone").unwrap().active);
        assert_eq!(p.target("nobody"), TargetCheck::Nobody);
        assert_eq!(p.handle_taken("gone"), Some(true));
    }

    #[tokio::test]
    async fn thirty_seconds_for_admin_five_minutes_for_identity() {
        let (p, f, clock) = people(vec![with_slack("ana", "U0ANA", &["pm", "admin"])]).await;
        f.set_down(true);
        clock.advance(ALLOW_AGE);
        assert!(
            matches!(p.admin("U0ANA"), AdminCheck::Admin(_)),
            "30 s holds"
        );
        clock.advance(Duration::from_millis(1));
        assert_eq!(p.admin("U0ANA"), AdminCheck::CantCheck);
        assert!(p.members().is_none());
        assert!(p.admins().is_none());
        assert_eq!(p.target("ana"), TargetCheck::CantCheck);
        assert_eq!(p.handle_taken("ana"), None);
        assert!(
            p.identify("U0ANA").member().is_some(),
            "identity holds 5 min"
        );
        clock.advance(SLACK_IDENTITY_AGE - ALLOW_AGE - Duration::from_millis(1));
        assert!(p.of_handle("ana").member().is_some());
        clock.advance(Duration::from_millis(1));
        assert_eq!(p.identify("U0ANA"), Identity::CantCheck);
        assert_eq!(p.of_handle("ana"), Identity::CantCheck);
        assert_eq!(p.identify("U0NOBODY"), Identity::CantCheck);
    }
}
