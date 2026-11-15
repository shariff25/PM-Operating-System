//! Keeping the content checkout current (`docs/onboarding-spec.md` §3). Every
//! `PMOS_CONTENT_SYNC_SECS`, and right after a member commit, the tool server runs
//! `git pull --ff-only` in the repo that holds `PMOS_CONTENT_ROOT`. If HEAD is no longer the
//! commit last indexed, it reloads like `POST /internal/reindex`, with the same lint gate.
//!
//! The checkout's own git setup is used (such as a deploy key on the Spark): the tool server
//! holds no git credentials. Failures are logged and don't stop the server. `pmos-slack` and
//! `pmos-bots` read the same checkout, so they see a change as soon as the pull lands.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

use crate::context::ReindexReport;
use crate::service::ToolService;

/// The longest one git command may take.
const GIT_TIMEOUT: Duration = Duration::from_secs(120);

/// What one sync did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReport {
    /// HEAD after the pull.
    pub head: String,
    /// Whether the pull moved HEAD.
    pub moved: bool,
    /// Set when the content was reloaded.
    pub reindexed: Option<ReindexReport>,
}

pub struct ContentSync {
    content_root: PathBuf,
    svc: Arc<ToolService>,
    wake: Notify,
    /// The commit whose content is loaded. Held for a whole sync, so syncs run one at a time.
    indexed: Mutex<Option<String>>,
}

async fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        // Never wait on a prompt for credentials: fail instead.
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(GIT_TIMEOUT, cmd.output())
        .await
        .map_err(|_| format!("git {} took longer than {GIT_TIMEOUT:?}", args.join(" ")))?
        .map_err(|e| format!("running git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

impl ContentSync {
    /// The content at `content_root` is taken to be loaded at its current HEAD, as it is
    /// once the server has started.
    pub fn new(content_root: impl Into<PathBuf>, svc: Arc<ToolService>) -> Arc<Self> {
        Arc::new(Self {
            content_root: content_root.into(),
            svc,
            wake: Notify::new(),
            indexed: Mutex::new(None),
        })
    }

    /// Ask the running loop to sync now.
    pub fn trigger(&self) {
        self.wake.notify_one();
    }

    /// Pull, and reload if HEAD isn't the commit last loaded.
    pub async fn sync_once(&self, now: DateTime<Utc>) -> Result<SyncReport, String> {
        let mut indexed = self.indexed.lock().await;
        let root = PathBuf::from(git(&self.content_root, &["rev-parse", "--show-toplevel"]).await?);
        let before = git(&root, &["rev-parse", "HEAD"]).await?;
        indexed.get_or_insert_with(|| before.clone());
        git(&root, &["pull", "--ff-only", "--quiet"]).await?;
        let head = git(&root, &["rev-parse", "HEAD"]).await?;
        let moved = head != before;
        if indexed.as_deref() == Some(head.as_str()) {
            return Ok(SyncReport {
                head,
                moved,
                reindexed: None,
            });
        }
        let report = self
            .svc
            .reindex(&self.content_root, now)
            .await
            .map_err(|e| format!("content at {head} not loaded: {e}"))?;
        tracing::info!(
            head = %head,
            indexed = report.indexed,
            removed = report.removed,
            "content synced and reindexed"
        );
        *indexed = Some(head.clone());
        Ok(SyncReport {
            head,
            moved,
            reindexed: Some(report),
        })
    }

    /// Sync every `every`, and whenever [`ContentSync::trigger`] is called, until `cancel`.
    pub fn spawn(self: Arc<Self>, every: Duration, cancel: CancellationToken) {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // The first tick is immediate, and the content was just loaded.
            tick.tick().await;
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tick.tick() => {}
                    _ = self.wake.notified() => {}
                }
                if let Err(e) = self.sync_once(Utc::now()).await {
                    tracing::error!(error = %e, "content sync failed");
                }
            }
        });
    }
}
