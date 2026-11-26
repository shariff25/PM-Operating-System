//! The witness: a store the customer controls, outside the Ledger's database, that
//! checkpoints are written to and never rewritten (spec §6.2).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum WitnessError {
    /// A different checkpoint is already there under that number. Something rewrote history.
    #[error("the witness holds a different checkpoint {0}")]
    Conflict(u64),
    #[error("witness: {0}")]
    Io(String),
}

pub trait Witness: Send + Sync {
    /// Write checkpoint `seq`. Writing the same bytes again is fine; different bytes are a
    /// conflict and nothing is changed.
    fn put(&self, seq: u64, bytes: &[u8]) -> Result<(), WitnessError>;
    /// Every checkpoint held, in order of number.
    fn all(&self) -> Result<Vec<(u64, Vec<u8>)>, WitnessError>;
}

/// A directory of `checkpoint-<number>.json` files, for a test stack. A directory is only as
/// write-once as its permissions; a real deployment uses object storage with a lock.
pub struct DirWitness {
    dir: PathBuf,
}

impl DirWitness {
    pub fn new(dir: impl AsRef<Path>) -> Self {
        DirWitness {
            dir: dir.as_ref().to_path_buf(),
        }
    }

    fn path(&self, seq: u64) -> PathBuf {
        self.dir.join(format!("checkpoint-{seq:020}.json"))
    }
}

fn io(e: std::io::Error) -> WitnessError {
    WitnessError::Io(e.to_string())
}

impl Witness for DirWitness {
    fn put(&self, seq: u64, bytes: &[u8]) -> Result<(), WitnessError> {
        fs::create_dir_all(&self.dir).map_err(io)?;
        let path = self.path(seq);
        // Written whole under another name, synced, then linked into place. A link never
        // replaces a file, and a crash can leave a stray temporary but never a short
        // checkpoint (review finding GR-13).
        let tmp = self
            .dir
            .join(format!(".tmp-{seq:020}-{}", std::process::id()));
        let _ = fs::remove_file(&tmp);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(io)?;
        file.write_all(bytes).map_err(io)?;
        let mut perms = file.metadata().map_err(io)?.permissions();
        perms.set_readonly(true);
        fs::set_permissions(&tmp, perms).map_err(io)?;
        file.sync_all().map_err(io)?;
        drop(file);
        let linked = fs::hard_link(&tmp, &path);
        let _ = fs::remove_file(&tmp);
        match linked {
            Ok(()) => {
                fs::File::open(&self.dir)
                    .and_then(|d| d.sync_all())
                    .map_err(io)?;
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if fs::read(&path).map_err(io)? == bytes {
                    Ok(())
                } else {
                    Err(WitnessError::Conflict(seq))
                }
            }
            Err(e) => Err(io(e)),
        }
    }

    fn all(&self) -> Result<Vec<(u64, Vec<u8>)>, WitnessError> {
        let mut out = Vec::new();
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(io(e)),
        };
        for entry in entries {
            let entry = entry.map_err(io)?;
            let name = entry.file_name().to_string_lossy().to_string();
            // Only the exact name `put` writes: twenty digits. "checkpoint-1.json" or
            // "checkpoint-+1.json" beside the real file is not a second checkpoint 1.
            let Some(seq) = name
                .strip_prefix("checkpoint-")
                .and_then(|s| s.strip_suffix(".json"))
                .filter(|s| s.len() == 20 && s.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|s| s.parse::<u64>().ok())
            else {
                continue;
            };
            out.push((seq, fs::read(entry.path()).map_err(io)?));
        }
        out.sort_by_key(|(seq, _)| *seq);
        Ok(out)
    }
}
