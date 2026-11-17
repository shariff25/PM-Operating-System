//! Who may use PM OS: the people in `content/team.yaml`, until SSO replaces it (Phase 1).
//!
//! With live connectors the list is enforced: sessions are created only for members, and a
//! linked account must belong to the member it is linked for. With fake connectors (dev and
//! tests) it is not enforced, so the empty seed file doesn't lock anyone out.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use pmos_contract::model::{TeamFile, TeamMember};

use crate::session::BOT_IDENTITY_PREFIX;

pub struct Team {
    path: Option<PathBuf>,
    file: RwLock<TeamFile>,
    enforced: bool,
}

impl Team {
    /// Not enforced: anyone with a valid handle may have a session (dev and tests).
    pub fn open() -> Arc<Self> {
        Arc::new(Self {
            path: None,
            file: RwLock::new(TeamFile::default()),
            enforced: false,
        })
    }

    /// Enforced, with a fixed member list (tests).
    pub fn of(members: Vec<TeamMember>) -> Arc<Self> {
        Arc::new(Self {
            path: None,
            file: RwLock::new(TeamFile { members }),
            enforced: true,
        })
    }

    /// `<content_root>/team.yaml`, enforced or not.
    pub fn load(content_root: &Path, enforced: bool) -> Result<Arc<Self>, String> {
        let file = TeamFile::load(content_root)?;
        if enforced && file.members.is_empty() {
            tracing::warn!("team.yaml lists nobody: no one can use PM OS until it does");
        }
        Ok(Arc::new(Self {
            path: Some(content_root.to_path_buf()),
            file: RwLock::new(file),
            enforced,
        }))
    }

    /// Re-read `team.yaml` after a merge. On error the old list stays.
    pub fn reload(&self) -> Result<(), String> {
        if let Some(root) = &self.path {
            let file = TeamFile::load(root)?;
            *self.file.write().expect("team lock") = file;
        }
        Ok(())
    }

    /// Use `file` as the member list now: the `team.yaml` just committed to the content repo,
    /// before the checkout is pulled. The next reload reads the checkout again.
    pub fn replace(&self, file: TeamFile) {
        *self.file.write().expect("team lock") = file;
    }

    pub fn enforced(&self) -> bool {
        self.enforced
    }

    /// The whole member list as loaded, for onboarding (`onboarding.rs`).
    pub fn file(&self) -> TeamFile {
        self.file.read().expect("team lock").clone()
    }

    /// The member with this Slack id.
    pub fn by_slack_id(&self, slack_id: &str) -> Option<TeamMember> {
        self.file
            .read()
            .expect("team lock")
            .by_slack_id(slack_id)
            .cloned()
            .filter(|m| !m.handle.starts_with(BOT_IDENTITY_PREFIX))
    }

    /// The member with this handle. A team bot's service identity (`bots.<name>`) is never a
    /// member, even if `team.yaml` lists it.
    pub fn member(&self, handle: &str) -> Option<TeamMember> {
        if handle.starts_with(BOT_IDENTITY_PREFIX) {
            return None;
        }
        self.file
            .read()
            .expect("team lock")
            .by_handle(handle)
            .cloned()
    }

    /// May `handle` use PM OS? Never a team bot's service identity, enforced or not.
    pub fn allows(&self, handle: &str) -> bool {
        !handle.starts_with(BOT_IDENTITY_PREFIX)
            && (!self.enforced || self.member(handle).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pmos_contract::model::Role;

    fn member(handle: &str) -> TeamMember {
        TeamMember {
            handle: handle.into(),
            slack_id: "U0TEST".into(),
            email: None,
            github: None,
            entra_oid: None,
            roles: vec![Role::Pm],
        }
    }

    #[test]
    fn bot_identities_are_never_members() {
        let team = Team::of(vec![member("ana"), member("bots.docs")]);
        assert!(team.allows("ana"));
        assert!(!team.allows("eve"));
        assert!(team.member("bots.docs").is_none(), "listed, but never a PM");
        assert!(!team.allows("bots.docs"));
        let open = Team::open();
        assert!(open.allows("eve"));
        assert!(!open.allows("bots.docs"));
    }
}
