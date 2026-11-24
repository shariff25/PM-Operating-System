//! One key per upstream, each in its own file (`docs/model-providers-spec.md` §4.11, slice MP2).
//!
//! An upstream with `key-file:` sends only the key in that file, and only to itself. The
//! gateway reads the file, never an environment variable or the config, and re-reads it when
//! its size or modification time changes (looked at no more than once a second) or on
//! `SIGHUP` ([`KeyStore::reload`]). A file that is missing, unreadable, empty, or readable by
//! anyone but its owner is unusable: the upstream is sent nothing until it is fixed, and the
//! gateway keeps serving everything else. Neither the key nor the file's path is ever logged.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

/// How often a key file is looked at, at most.
pub const RECHECK: Duration = Duration::from_secs(1);

/// Why a key file can't be used. The words are the log's; none names the key or the path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unusable {
    Missing,
    Unreadable,
    Empty,
    /// Group or others may read it (mode wider than `600`).
    TooOpen,
}

impl Unusable {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Unreadable => "unreadable",
            Self::Empty => "empty",
            Self::TooOpen => "readable by group or others",
        }
    }
}

struct Cached {
    key: Result<String, Unusable>,
    size: u64,
    modified: Option<SystemTime>,
    checked: Instant,
    generation: u64,
}

/// The upstreams' keys, by key file.
#[derive(Default)]
pub struct KeyStore {
    files: Mutex<HashMap<PathBuf, Cached>>,
    /// Bumped by [`KeyStore::reload`]: every file is read afresh on its next use.
    generation: AtomicU64,
}

impl std::fmt::Debug for KeyStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KeyStore(<redacted>)")
    }
}

impl KeyStore {
    /// `SIGHUP`: read every key file afresh on its next use.
    pub fn reload(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    /// The key in `path`, or why it can't be used.
    pub fn get(&self, path: &Path) -> Result<String, Unusable> {
        let generation = self.generation.load(Ordering::SeqCst);
        let mut files = self.files.lock().expect("key store lock");
        if let Some(c) = files.get(path) {
            if c.generation == generation && c.checked.elapsed() < RECHECK {
                return c.key.clone();
            }
        }
        let meta = std::fs::metadata(path);
        let (size, modified) = match &meta {
            Ok(m) => (m.len(), m.modified().ok()),
            Err(_) => (0, None),
        };
        if let Some(c) = files.get_mut(path) {
            if c.generation == generation && c.size == size && c.modified == modified {
                c.checked = Instant::now();
                return c.key.clone();
            }
        }
        let key = read_key(path, meta);
        let before = files.get(path).map(|c| c.key.clone());
        match (&before, &key) {
            (Some(Ok(_)) | None, Err(why)) => {
                tracing::warn!(
                    reason = why.as_str(),
                    "an upstream's key file can't be used; that upstream is sent nothing"
                );
            }
            (Some(Err(_)), Ok(_)) => tracing::info!("an upstream's key file is usable again"),
            (Some(Ok(a)), Ok(b)) if a != b => {
                tracing::info!("an upstream's key changed; using the new one")
            }
            _ => {}
        }
        files.insert(
            path.to_path_buf(),
            Cached {
                key: key.clone(),
                size,
                modified,
                checked: Instant::now(),
                generation,
            },
        );
        key
    }
}

fn read_key(path: &Path, meta: std::io::Result<std::fs::Metadata>) -> Result<String, Unusable> {
    let meta = match meta {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Unusable::Missing),
        Err(_) => return Err(Unusable::Unreadable),
    };
    if !meta.is_file() {
        return Err(Unusable::Unreadable);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(Unusable::TooOpen);
        }
    }
    let text = std::fs::read_to_string(path).map_err(|_| Unusable::Unreadable)?;
    let key = text.trim();
    if key.is_empty() {
        return Err(Unusable::Empty);
    }
    Ok(key.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn write(path: &Path, text: &str, mode: u32) {
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn reads_rereads_and_refuses_unusable_files() {
        let dir = std::env::temp_dir().join(format!("pmos-keys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("upstream-a.key");
        let store = KeyStore::default();
        assert_eq!(store.get(&path), Err(Unusable::Missing));
        write(&path, "key-one\n", 0o600);
        // Within the recheck period the cached answer stands; a reload reads it afresh.
        store.reload();
        assert_eq!(store.get(&path).unwrap(), "key-one");
        write(&path, "key-two-longer\n", 0o600);
        store.reload();
        assert_eq!(store.get(&path).unwrap(), "key-two-longer");
        // A change of size is picked up within a second, without a reload.
        write(&path, "key-three-longer-still\n", 0o600);
        std::thread::sleep(RECHECK + Duration::from_millis(50));
        assert_eq!(store.get(&path).unwrap(), "key-three-longer-still");
        write(&path, "  \n", 0o600);
        store.reload();
        assert_eq!(store.get(&path), Err(Unusable::Empty));
        write(&path, "key-four", 0o644);
        store.reload();
        assert_eq!(store.get(&path), Err(Unusable::TooOpen));
        assert!(!format!("{store:?}").contains("key-"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
