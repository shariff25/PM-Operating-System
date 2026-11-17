//! The audit retention job (`docs/audit-retention.md`): once a day, audit events older than
//! `PMOS_AUDIT_RETENTION_DAYS` (90 by default, never fewer) are deleted, with no archive.
//!
//! The job only asks: in Postgres every rule is the database's own (migration 0013). The
//! purge function refuses a retention under 90 days, deletes only rows older than its cutoff,
//! records each batch in the audit log in the same transaction, and takes an advisory lock so
//! two replicas don't purge at once; the append-only trigger lets a DELETE through only from
//! that function and only for a row older than 90 days. The in-memory sink keeps the same
//! rules for tests.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;

use crate::audit::{AuditSink, Purged};
use crate::store::StoreResult;

/// The events one batch deletes at most.
pub const BATCH: u32 = 10_000;

/// The batches one run makes at most; what is left waits for the next day's run.
pub const MAX_BATCHES: usize = 100;

/// How often the job runs.
pub const EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// The first run comes this long after start-up, plus up to [`JITTER`], so replicas started
/// together don't all try at once and a crash loop doesn't purge on every start.
pub const FIRST_AFTER: Duration = Duration::from_secs(5 * 60);

/// The most random delay added to [`FIRST_AFTER`].
pub const JITTER: Duration = Duration::from_secs(5 * 60);

/// What one run did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RunReport {
    /// Events deleted, over every batch.
    pub deleted: u64,
    /// Batches made (each one recorded in the audit log).
    pub batches: usize,
    /// Another purge held the lock, so this run stopped there.
    pub busy: bool,
    /// Eligible events are left after [`MAX_BATCHES`]: the next run goes on.
    pub more: bool,
}

/// One run: batches of at most `batch` until nothing eligible is left, another purge holds
/// the lock, or [`MAX_BATCHES`]. `None` when the audit sink can't purge (no database).
pub async fn run(
    audit: &dyn AuditSink,
    retention_days: u32,
    batch: u32,
    now: DateTime<Utc>,
) -> StoreResult<Option<RunReport>> {
    let mut report = RunReport::default();
    for _ in 0..MAX_BATCHES {
        match audit.purge(retention_days, batch, now).await? {
            None => return Ok(None),
            Some(Purged::Busy) => {
                report.busy = true;
                return Ok(Some(report));
            }
            Some(Purged::Ran(done)) => {
                report.batches += 1;
                report.deleted += done.deleted;
                report.more = done.more;
                if !done.more {
                    return Ok(Some(report));
                }
            }
        }
    }
    Ok(Some(report))
}

/// A delay from zero up to (not including) `max`, from a fresh random UUID.
fn jitter(max: Duration) -> Duration {
    let millis = max.as_millis().max(1);
    Duration::from_millis((uuid::Uuid::new_v4().as_u128() % millis) as u64)
}

/// Run the job [`FIRST_AFTER`] (plus jitter) after start-up and then every [`EVERY`], until
/// `cancel`. A failed run is logged and tried again at the next tick.
pub fn spawn(
    audit: Arc<dyn AuditSink>,
    retention_days: u32,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let first = FIRST_AFTER + jitter(JITTER);
    tokio::spawn(async move {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(first) => {}
        }
        let mut tick = tokio::time::interval(EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tick.tick() => {}
            }
            let purge = run(audit.as_ref(), retention_days, BATCH, Utc::now());
            let result = tokio::select! {
                _ = cancel.cancelled() => return,
                r = purge => r,
            };
            match result {
                Ok(Some(r)) => tracing::info!(
                    retention_days,
                    deleted = r.deleted,
                    batches = r.batches,
                    busy = r.busy,
                    more = r.more,
                    "audit retention purge"
                ),
                Ok(None) => {
                    tracing::info!("audit retention: this audit log can't be purged");
                    return;
                }
                Err(e) => tracing::error!(error = %e, "audit retention purge failed"),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_stays_under_its_bound() {
        for _ in 0..200 {
            assert!(jitter(JITTER) < JITTER);
        }
        assert_eq!(jitter(Duration::ZERO), Duration::ZERO);
    }
}
