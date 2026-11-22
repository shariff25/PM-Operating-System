//! Clean, isolated attempts: a fresh tool server for every attempt.
//!
//! Every attempt asks as one handle (`evals`), and the tool server keeps what an attempt left
//! behind: a pending draft, a memory, a recorded bet. The tool server allows one pending
//! `slack.post_reply` per person and message, so without isolation one attempt's draft makes
//! every later attempt on that message get `already_drafted` and fail. Nobody decides eval
//! drafts, and no route lets the runner clear them (decided 2026-11-22: a fresh tool server per
//! attempt, not a new route).
//!
//! So the runner works in **lanes**. A lane is one bot runtime with its own workers and its own
//! tool server, all in memory. Before each attempt the runner takes a free lane and runs the
//! **fresh command** for it (`<program> <lane>`, lanes numbered from 1), which replaces that
//! lane's tool server with a new process (empty approvals, memory and records; the same seed
//! and content) and exits 0 once it answers. Only then does the attempt start on the lane's
//! runtime, and no other attempt uses the lane until it has been graded. Attempts run in
//! parallel up to the number of lanes.
//!
//! The test stack (`scripts/claude-test-stack.sh`, `PMOS_TEST_EVALS_LANES`) starts the lanes
//! and writes the command; `docs/evals.md`, "Isolated attempts", has the details.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::client::Runtime;

/// Replaces lane `n`'s tool server with a fresh one, and returns once it answers; an error
/// says why it couldn't.
pub type FreshFn =
    Arc<dyn Fn(usize) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync>;

/// How long the fresh command may take before the attempt is given up on.
pub const FRESH_TIMEOUT: Duration = Duration::from_secs(120);

/// The lanes and how each gets a fresh tool server.
#[derive(Clone)]
pub struct Isolation {
    /// One runtime per lane; lane `n` (from 1) is `lanes[n - 1]`.
    pub lanes: Vec<Runtime>,
    /// The lanes' runtime URLs, for the report.
    pub urls: Vec<String>,
    pub fresh: FreshFn,
    /// What the report says about it ("`scripts/…` per attempt").
    pub how: String,
}

impl Isolation {
    /// What the report records: how many lanes, and that each attempt had a fresh tool server.
    pub fn describe(&self) -> String {
        format!(
            "a fresh tool server for every attempt ({}), {} lane{}",
            self.how,
            self.lanes.len(),
            if self.lanes.len() == 1 { "" } else { "s" }
        )
    }

    /// A pool that hands each attempt a lane of its own.
    pub fn pool(&self) -> Lanes {
        Lanes {
            free: Arc::new(Mutex::new((1..=self.lanes.len()).rev().collect())),
            permits: Arc::new(Semaphore::new(self.lanes.len().max(1))),
        }
    }
}

/// Free lanes: an attempt holds one from [`Lanes::take`] until the [`Lane`] is dropped.
#[derive(Clone)]
pub struct Lanes {
    free: Arc<Mutex<Vec<usize>>>,
    permits: Arc<Semaphore>,
}

/// A lane held by one attempt; it goes back to the pool when dropped.
pub struct Lane {
    pub n: usize,
    free: Arc<Mutex<Vec<usize>>>,
    _permit: OwnedSemaphorePermit,
}

impl Lanes {
    /// Wait for a free lane.
    pub async fn take(&self) -> Lane {
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .expect("the semaphore is never closed");
        let n = self
            .free
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .pop()
            .expect("a permit always has a free lane");
        Lane {
            n,
            free: self.free.clone(),
            _permit: permit,
        }
    }
}

impl Drop for Lane {
    fn drop(&mut self) {
        self.free
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(self.n);
    }
}

/// The fresh command: run `program <lane>` with no shell, and wait for it to exit (at most
/// [`FRESH_TIMEOUT`]). Exit 0 means the lane has a fresh tool server. Its stdout is discarded;
/// the last lines of its stderr go into the error. Nothing is passed to it but the lane
/// number, so no token travels on its command line.
pub fn command(program: PathBuf) -> FreshFn {
    Arc::new(move |lane: usize| {
        let program = program.clone();
        Box::pin(async move {
            let child = tokio::process::Command::new(&program)
                .arg(lane.to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .map_err(|e| format!("couldn't run {}: {e}", program.display()))?;
            let out = match tokio::time::timeout(FRESH_TIMEOUT, child.wait_with_output()).await {
                Ok(r) => r.map_err(|e| format!("{}: {e}", program.display()))?,
                Err(_) => {
                    return Err(format!(
                        "{} {lane} took longer than {} s",
                        program.display(),
                        FRESH_TIMEOUT.as_secs()
                    ))
                }
            };
            if out.status.success() {
                return Ok(());
            }
            let err = String::from_utf8_lossy(&out.stderr);
            let tail: Vec<&str> = err.lines().rev().take(3).collect();
            let tail: Vec<&str> = tail.into_iter().rev().collect();
            Err(format!(
                "{} {lane} exited with {}{}",
                program.display(),
                out.status,
                if tail.is_empty() {
                    String::new()
                } else {
                    format!(": {}", tail.join(" / "))
                }
            ))
        })
    })
}
