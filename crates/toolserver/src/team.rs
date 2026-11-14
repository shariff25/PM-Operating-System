//! Who may use PM OS: the people in `content/team.yaml`, until SSO replaces it (Phase 1).
//!
//! With live connectors the list is enforced: sessions are created only for members, and a
//! linked account must belong to the member it is linked for. With fake connectors (dev and
//! tests) it is not enforced, so the empty seed file doesn't lock anyone out.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use pmos_contract::model::{TeamFile, TeamMember};

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

    pub fn enforced(&self) -> bool {
        self.enforced
    }

    pub fn member(&self, handle: &str) -> Option<TeamMember> {
        self.file
            .read()
            .expect("team lock")
            .by_handle(handle)
            .cloned()
    }

    /// May `handle` use PM OS?
    pub fn allows(&self, handle: &str) -> bool {
        !self.enforced || self.member(handle).is_some()
    }
}
