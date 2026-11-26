//! The gate's own durable copy of every receipt it has signed (spec §4.5).
//!
//! A receipt is on disk and synced before the gate goes on. The spool is the gate's proof of
//! what it wrote: it is trimmed only when a witnessed checkpoint covers a receipt, never on
//! the Ledger's word alone (review finding GR-10).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use gov_core::receipt::ChainHead;
use gov_core::{Digest, Envelope};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum SpoolError {
    #[error("spool: {0}")]
    Io(String),
    #[error("the spool holds something that is not what the gate wrote")]
    Corrupt,
}

/// The most alarming answers kept. A Ledger gone wrong cannot fill the gate's disk with them.
pub const MAX_ALARMS: usize = 64;

fn io(e: std::io::Error) -> SpoolError {
    SpoolError::Io(e.to_string())
}

/// One receipt as the spool holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spooled {
    pub seq: u64,
    pub hash: Digest,
    /// The time written in the receipt.
    pub at: i64,
    /// For an intent: no outcome receipt has been written for it yet.
    pub open_intent: bool,
    pub envelope: Envelope,
}

/// Where the chain stands, kept apart from the receipts so that trimming them loses nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpoolState {
    pub head: Option<ChainHead>,
    /// The time in the last receipt. The next is never earlier.
    pub last_at: i64,
    /// The Ledger has said, under its signature, that it holds the chain this far.
    pub acked_through: u64,
    /// A witnessed checkpoint covers the chain this far; receipts up to here are trimmed.
    pub covered_through: u64,
    /// Calls that passed through with no receipt, since this spool began. Kept here so the
    /// count outlives a restart (review finding GR-73).
    #[serde(default)]
    pub not_recorded: u64,
}

pub trait Spool: Send {
    fn state(&self) -> Result<SpoolState, SpoolError>;
    /// Add a receipt and move the state to it. Durable before it returns.
    fn append(&mut self, receipt: &Spooled, state: &SpoolState) -> Result<(), SpoolError>;
    /// Mark an intent as answered.
    fn close_intent(&mut self, seq: u64) -> Result<(), SpoolError>;
    fn set_state(&mut self, state: &SpoolState) -> Result<(), SpoolError>;
    /// Every receipt still held, in order.
    /// Every receipt held. Reads the whole spool: for opening a gate, not for each call.
    fn held(&self) -> Result<Vec<Spooled>, SpoolError>;
    /// One receipt, by number.
    fn get(&self, seq: u64) -> Result<Option<Spooled>, SpoolError>;
    /// Drop one receipt. Dropping one that is not there is no error.
    fn remove(&mut self, seq: u64) -> Result<(), SpoolError>;
    /// Drop receipts up to and including `seq`.
    fn trim(&mut self, seq: u64) -> Result<(), SpoolError>;
    /// Keep the witnessed checkpoint the spool was last trimmed on, as read from the witness.
    fn keep_checkpoint(&mut self, bytes: &[u8]) -> Result<(), SpoolError>;
    fn kept_checkpoint(&self) -> Result<Option<Vec<u8>>, SpoolError>;
    /// Keep the Ledger's signed answer that raised an alarm, as evidence, under the receipt
    /// number it speaks of. Each is kept, not only the last (review finding GR-80), up to
    /// [`MAX_ALARMS`]: past that the earliest ones are the ones that stay.
    fn keep_alarm(&mut self, seq: u64, bytes: &[u8]) -> Result<(), SpoolError>;
    /// The kept alarms, by receipt number.
    fn kept_alarms(&self) -> Result<Vec<(u64, Vec<u8>)>, SpoolError>;
}

/// A spool in a directory: one file per receipt and one for the state, each written whole
/// under another name, synced, and renamed into place.
pub struct FileSpool {
    dir: PathBuf,
}

impl FileSpool {
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, SpoolError> {
        fs::create_dir_all(dir.as_ref()).map_err(io)?;
        Ok(FileSpool {
            dir: dir.as_ref().to_path_buf(),
        })
    }

    fn receipt_path(&self, seq: u64) -> PathBuf {
        self.dir.join(format!("receipt-{seq:020}.json"))
    }

    fn read_optional(&self, name: &str) -> Result<Option<Vec<u8>>, SpoolError> {
        match fs::read(self.dir.join(name)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io(e)),
        }
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> Result<(), SpoolError> {
        let tmp = path.with_extension("tmp");
        let mut file = fs::File::create(&tmp).map_err(io)?;
        file.write_all(bytes).map_err(io)?;
        file.sync_all().map_err(io)?;
        drop(file);
        fs::rename(&tmp, path).map_err(io)?;
        fs::File::open(&self.dir)
            .and_then(|d| d.sync_all())
            .map_err(io)
    }
}

impl Spool for FileSpool {
    fn state(&self) -> Result<SpoolState, SpoolError> {
        match fs::read(self.dir.join("state.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| SpoolError::Corrupt),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SpoolState::default()),
            Err(e) => Err(io(e)),
        }
    }

    fn append(&mut self, receipt: &Spooled, state: &SpoolState) -> Result<(), SpoolError> {
        // The receipt first: a crash between the two leaves a receipt the state does not know
        // of, which `Gate::open` finds and adopts. The other order could lose a receipt the
        // state already counted.
        let bytes = serde_json::to_vec(receipt).map_err(|_| SpoolError::Corrupt)?;
        self.write(&self.receipt_path(receipt.seq), &bytes)?;
        self.set_state(state)
    }

    fn close_intent(&mut self, seq: u64) -> Result<(), SpoolError> {
        let path = self.receipt_path(seq);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            // Already trimmed: nothing left to mark.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(io(e)),
        };
        let mut held: Spooled = serde_json::from_slice(&bytes).map_err(|_| SpoolError::Corrupt)?;
        held.open_intent = false;
        self.write(
            &path,
            &serde_json::to_vec(&held).map_err(|_| SpoolError::Corrupt)?,
        )
    }

    fn set_state(&mut self, state: &SpoolState) -> Result<(), SpoolError> {
        let bytes = serde_json::to_vec(state).map_err(|_| SpoolError::Corrupt)?;
        self.write(&self.dir.join("state.json"), &bytes)
    }

    fn held(&self) -> Result<Vec<Spooled>, SpoolError> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.dir).map_err(io)? {
            let entry = entry.map_err(io)?;
            let name = entry.file_name().to_string_lossy().to_string();
            let is_receipt = name
                .strip_prefix("receipt-")
                .and_then(|s| s.strip_suffix(".json"))
                .is_some_and(|s| s.len() == 20 && s.bytes().all(|b| b.is_ascii_digit()));
            if !is_receipt {
                continue;
            }
            let bytes = fs::read(entry.path()).map_err(io)?;
            out.push(serde_json::from_slice::<Spooled>(&bytes).map_err(|_| SpoolError::Corrupt)?);
        }
        out.sort_by_key(|r| r.seq);
        Ok(out)
    }

    fn get(&self, seq: u64) -> Result<Option<Spooled>, SpoolError> {
        match fs::read(self.receipt_path(seq)) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| SpoolError::Corrupt),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io(e)),
        }
    }

    fn remove(&mut self, seq: u64) -> Result<(), SpoolError> {
        match fs::remove_file(self.receipt_path(seq)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io(e)),
        }
    }

    fn trim(&mut self, seq: u64) -> Result<(), SpoolError> {
        // By name alone: no receipt is read to decide whether it goes.
        for entry in fs::read_dir(&self.dir).map_err(io)? {
            let name = entry.map_err(io)?.file_name();
            let number = name
                .to_str()
                .and_then(|n| n.strip_prefix("receipt-"))
                .and_then(|n| n.strip_suffix(".json"))
                .filter(|n| n.len() == 20 && n.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|n| n.parse::<u64>().ok());
            if number.is_some_and(|n| n <= seq) {
                fs::remove_file(self.dir.join(&name)).map_err(io)?;
            }
        }
        Ok(())
    }

    fn keep_checkpoint(&mut self, bytes: &[u8]) -> Result<(), SpoolError> {
        self.write(&self.dir.join("covered-by.json"), bytes)
    }

    fn kept_checkpoint(&self) -> Result<Option<Vec<u8>>, SpoolError> {
        self.read_optional("covered-by.json")
    }

    fn keep_alarm(&mut self, seq: u64, bytes: &[u8]) -> Result<(), SpoolError> {
        let path = self.dir.join(format!("alarm-{seq:020}.json"));
        if !path.exists() && self.kept_alarms()?.len() >= MAX_ALARMS {
            return Ok(());
        }
        self.write(&path, bytes)
    }

    fn kept_alarms(&self) -> Result<Vec<(u64, Vec<u8>)>, SpoolError> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.dir).map_err(io)? {
            let name = entry.map_err(io)?.file_name();
            let seq = name
                .to_str()
                .and_then(|n| n.strip_prefix("alarm-"))
                .and_then(|n| n.strip_suffix(".json"))
                .filter(|n| n.len() == 20)
                .and_then(|n| n.parse::<u64>().ok());
            if let Some(seq) = seq {
                out.push((seq, fs::read(self.dir.join(&name)).map_err(io)?));
            }
        }
        out.sort_by_key(|(seq, _)| *seq);
        Ok(out)
    }
}

/// Reads checkpoints from the witness store itself. A gate trims its spool only on what it
/// reads here, never on a checkpoint someone hands it (review finding GR-49).
pub trait WitnessReader: Send + Sync {
    /// Every checkpoint in the witness: its number and its bytes.
    fn checkpoints(&self) -> Result<Vec<(u64, Vec<u8>)>, String>;
    /// The newest `limit` checkpoints, oldest of them first. The witness only grows, and a
    /// gate needs its newest end alone (review finding GR-82).
    fn newest(&self, limit: usize) -> Result<Vec<(u64, Vec<u8>)>, String> {
        let mut all = self.checkpoints()?;
        Ok(all.split_off(all.len().saturating_sub(limit)))
    }
}

/// How many of the witness's newest checkpoints a gate reads. If none of them verifies,
/// nothing is trimmed and they are reported.
pub const WITNESS_WINDOW: usize = 16;

/// What was read from the witness. Only [`WitnessRead::from`] makes one, so a gate cannot be
/// handed checkpoints that did not come from a witness store (review finding GR-49).
pub struct WitnessRead(pub(crate) Vec<(u64, Vec<u8>)>);

impl WitnessRead {
    pub fn from(witness: &dyn WitnessReader) -> Result<Self, String> {
        witness.newest(WITNESS_WINDOW).map(WitnessRead)
    }
}

/// The directory witness, read directly: `checkpoint-<twenty digits>.json`.
pub struct DirWitnessReader {
    dir: PathBuf,
}

impl DirWitnessReader {
    pub fn new(dir: impl AsRef<Path>) -> Self {
        DirWitnessReader {
            dir: dir.as_ref().to_path_buf(),
        }
    }
}

impl DirWitnessReader {
    /// The numbers of the checkpoints in the directory, in order. No file is opened.
    fn numbers(&self) -> Result<Vec<u64>, String> {
        let mut out = Vec::new();
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.to_string()),
        };
        for entry in entries {
            let name = entry.map_err(|e| e.to_string())?.file_name();
            let seq = name
                .to_str()
                .and_then(|s| s.strip_prefix("checkpoint-"))
                .and_then(|s| s.strip_suffix(".json"))
                .filter(|s| s.len() == 20 && s.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|s| s.parse::<u64>().ok());
            if let Some(seq) = seq {
                out.push(seq);
            }
        }
        out.sort_unstable();
        Ok(out)
    }

    fn read(&self, numbers: &[u64]) -> Result<Vec<(u64, Vec<u8>)>, String> {
        numbers
            .iter()
            .map(|seq| {
                fs::read(self.dir.join(format!("checkpoint-{seq:020}.json")))
                    .map(|bytes| (*seq, bytes))
                    .map_err(|e| e.to_string())
            })
            .collect()
    }
}

impl WitnessReader for DirWitnessReader {
    fn checkpoints(&self) -> Result<Vec<(u64, Vec<u8>)>, String> {
        self.read(&self.numbers()?)
    }

    fn newest(&self, limit: usize) -> Result<Vec<(u64, Vec<u8>)>, String> {
        let numbers = self.numbers()?;
        self.read(&numbers[numbers.len().saturating_sub(limit)..])
    }
}
