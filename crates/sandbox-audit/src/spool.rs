//! The spool (`docs/openshell-prover-and-sandbox-audit-spec.md` §B.2): before the collector
//! answers OpenShell, each batch is written to its own file here and synced, and it stays until
//! the tool server answers `200`. Bounded (64 MiB by default): when a request's batches don't fit,
//! nothing of it is written and OpenShell is answered `503`, so it retries, drops and reports the
//! gap itself. Files are `<number>.json`, oldest first; a batch the tool server refuses (`400`:
//! a bug or a forgery, never retried) moves to `rejected/`, and the hole it leaves in the
//! sequence is recorded by the tool server as a gap.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::wire::Batch;

pub const DEFAULT_MAX_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug)]
pub struct Spool {
    dir: PathBuf,
    max: u64,
    next: u64,
}

fn number(p: &Path) -> Option<u64> {
    p.file_name()?.to_str()?.strip_suffix(".json")?.parse().ok()
}

impl Spool {
    pub fn open(dir: &Path, max: u64) -> std::io::Result<Self> {
        fs::create_dir_all(dir)?;
        let next = Self::files(dir)?
            .last()
            .and_then(|p| number(p))
            .map_or(1, |n| n + 1);
        Ok(Self {
            dir: dir.to_path_buf(),
            max,
            next,
        })
    }

    fn files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
        let mut found: Vec<(u64, PathBuf)> = fs::read_dir(dir)?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter_map(|p| number(&p).map(|n| (n, p)))
            .collect();
        found.sort();
        Ok(found.into_iter().map(|(_, p)| p).collect())
    }

    pub fn used(&self) -> std::io::Result<u64> {
        Ok(Self::files(&self.dir)?
            .iter()
            .filter_map(|p| fs::metadata(p).ok())
            .map(|m| m.len())
            .sum())
    }

    /// Do batches of `bytes` in total fit?
    pub fn fits(&self, bytes: u64) -> std::io::Result<bool> {
        Ok(self.used()? + bytes <= self.max)
    }

    /// Write one batch (already serialized), synced, under the next number.
    pub fn push(&mut self, body: &[u8]) -> std::io::Result<()> {
        let final_path = self.dir.join(format!("{:012}.json", self.next));
        let tmp = self.dir.join(format!(".{:012}.tmp", self.next));
        {
            let mut f = OpenOptions::new().create_new(true).write(true).open(&tmp)?;
            f.write_all(body)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &final_path)?;
        File::open(&self.dir)?.sync_all()?;
        self.next += 1;
        Ok(())
    }

    /// The oldest batch waiting, if any.
    pub fn oldest(&self) -> std::io::Result<Option<(PathBuf, Batch)>> {
        let Some(p) = Self::files(&self.dir)?.into_iter().next() else {
            return Ok(None);
        };
        let text = fs::read(&p)?;
        match serde_json::from_slice::<Batch>(&text) {
            Ok(b) => Ok(Some((p, b))),
            Err(_) => {
                // Unreadable (a torn write can't happen: files are renamed in whole). Keep it
                // aside rather than lose it silently.
                self.reject(&p)?;
                self.oldest()
            }
        }
    }

    pub fn remove(&self, p: &Path) -> std::io::Result<()> {
        fs::remove_file(p)?;
        File::open(&self.dir)?.sync_all()
    }

    pub fn reject(&self, p: &Path) -> std::io::Result<()> {
        let dir = self.dir.join("rejected");
        fs::create_dir_all(&dir)?;
        fs::rename(p, dir.join(p.file_name().unwrap_or_default()))
    }

    pub fn len(&self) -> std::io::Result<usize> {
        Ok(Self::files(&self.dir)?.len())
    }

    pub fn is_empty(&self) -> std::io::Result<bool> {
        Ok(self.len()? == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(run: &str) -> Vec<u8> {
        serde_json::to_vec(&Batch {
            collector_run: run.into(),
            events: vec![],
        })
        .unwrap()
    }

    #[test]
    fn batches_are_kept_in_order_until_removed_and_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Spool::open(dir.path(), 1 << 20).unwrap();
        s.push(&body("run-aaaaaaa1")).unwrap();
        s.push(&body("run-aaaaaaa2")).unwrap();
        let s = Spool::open(dir.path(), 1 << 20).unwrap();
        let (p, b) = s.oldest().unwrap().unwrap();
        assert_eq!(b.collector_run, "run-aaaaaaa1");
        s.remove(&p).unwrap();
        assert_eq!(s.oldest().unwrap().unwrap().1.collector_run, "run-aaaaaaa2");
        let mut s = Spool::open(dir.path(), 1 << 20).unwrap();
        s.push(&body("run-aaaaaaa3")).unwrap();
        assert_eq!(s.len().unwrap(), 2);
    }

    #[test]
    fn a_full_spool_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Spool::open(dir.path(), 100).unwrap();
        let b = body("run-aaaaaaa1");
        assert!(s.fits(b.len() as u64).unwrap());
        s.push(&b).unwrap();
        assert!(!s.fits(100).unwrap());
    }
}
