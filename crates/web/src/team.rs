//! `team.yaml`, read once and kept until it changes.
//!
//! Every signed-in request checks the PM is still a member, so the file is consulted on
//! every request; parsing it each time is wasted work. [`TeamCache::load`] keeps the parsed
//! file and reads it again when its modification time or size changes (one `stat` per
//! request), and in any case once it is older than [`MAX_AGE`], which covers an edit that
//! keeps both (a same-size write within the file system's timestamp granularity).
//!
//! **The bound:** a member removed from `team.yaml` loses access on their next request
//! after the file changes on disk, and never later than [`MAX_AGE`] after it.
//!
//! A file that can't be read or parsed is an error at once, never an old copy served: the
//! membership check must not keep someone in after a broken edit took the file away.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use pmos_contract::model::TeamFile;

/// The longest a parsed copy is used without reading the file again.
pub const MAX_AGE: Duration = Duration::from_secs(5);

struct Cached {
    team: Arc<TeamFile>,
    modified: Option<SystemTime>,
    len: u64,
    read_at: Instant,
}

/// `team.yaml` under a content root, parsed once and reloaded on change.
pub struct TeamCache {
    root: PathBuf,
    cached: Mutex<Option<Cached>>,
}

impl std::fmt::Debug for TeamCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TeamCache")
            .field("root", &self.root)
            .finish()
    }
}

impl TeamCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            cached: Mutex::new(None),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The team file as it is on disk now (see the module docs for when it is re-read).
    pub fn load(&self) -> Result<Arc<TeamFile>, String> {
        let path = self.root.join("team.yaml");
        let meta = std::fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let (modified, len) = (meta.modified().ok(), meta.len());
        {
            let cached = self.cached.lock().expect("team cache lock");
            if let Some(c) = cached.as_ref() {
                if c.modified.is_some()
                    && c.modified == modified
                    && c.len == len
                    && c.read_at.elapsed() < MAX_AGE
                {
                    return Ok(c.team.clone());
                }
            }
        }
        let team = Arc::new(TeamFile::load(&self.root)?);
        *self.cached.lock().expect("team cache lock") = Some(Cached {
            team: team.clone(),
            modified,
            len,
            read_at: Instant::now(),
        });
        Ok(team)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: &str = "members:\n  - {handle: ana, slack-id: U0ANA, roles: [pm]}\n";
    const TWO: &str = "members:\n  - {handle: ana, slack-id: U0ANA, roles: [pm]}\n  - {handle: ben, slack-id: U0BEN, roles: [pm]}\n";

    #[test]
    fn keeps_the_parsed_file_until_it_changes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("team.yaml"), ONE).unwrap();
        let cache = TeamCache::new(dir.path());
        let a = cache.load().unwrap();
        let b = cache.load().unwrap();
        assert!(Arc::ptr_eq(&a, &b), "the same parse, not read again");
        // A change of size is seen at once.
        std::fs::write(dir.path().join("team.yaml"), TWO).unwrap();
        assert_eq!(cache.load().unwrap().members.len(), 2);
        // Removed again: gone on the next load.
        std::fs::write(dir.path().join("team.yaml"), ONE).unwrap();
        assert!(cache.load().unwrap().by_handle("ben").is_none());
        // A broken or missing file is an error, never the old copy.
        std::fs::write(dir.path().join("team.yaml"), "members: [").unwrap();
        assert!(cache.load().is_err());
        std::fs::remove_file(dir.path().join("team.yaml")).unwrap();
        assert!(cache.load().is_err());
    }
}
