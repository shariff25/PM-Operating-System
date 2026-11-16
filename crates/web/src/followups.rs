//! Follow-ups: what a PM is waiting to answer, and what they are waiting on, plus the day's
//! "what changed" items (`docs/daily-brief.md`). A small store, not a log.
//!
//! - A follow-up holds a one-line summary, never message text. Slack-derived summaries
//!   expire after 24 hours; every other one after a week. Both are purged, not just hidden.
//! - Everything is keyed by the PM's handle: no read or write takes anyone else's.
//! - The id is derived (`fu_` plus 16 hex digits of a hash of handle, kind, system and url),
//!   so the producer sending the same follow-up again refreshes it instead of making a copy,
//!   and a dismissed one stays dismissed until it expires.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use crate::session::StoreError;

/// How long a follow-up from Slack is kept.
pub const SLACK_TTL_HOURS: i64 = 24;
/// How long any other follow-up is kept.
pub const TTL_DAYS: i64 = 7;
/// How long the day's `changed` items are kept.
pub const CHANGED_TTL_HOURS: i64 = 24;

/// At most this many items in each list.
pub const MAX_ITEMS: usize = 50;
pub const MAX_SYSTEM: usize = 32;
pub const MAX_WHO: usize = 80;
pub const MAX_SUMMARY: usize = 200;
pub const MAX_URL: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    WaitingOnYou,
    YouWaitOn,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WaitingOnYou => "waiting_on_you",
            Self::YouWaitOn => "you_wait_on",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "waiting_on_you" => Some(Self::WaitingOnYou),
            "you_wait_on" => Some(Self::YouWaitOn),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowUp {
    pub id: String,
    pub kind: Kind,
    pub system: String,
    pub who: String,
    pub summary: String,
    pub url: String,
    pub created: DateTime<Utc>,
    pub expires: DateTime<Utc>,
    pub dismissed: bool,
}

/// What the producer sends for one follow-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFollowUp {
    pub kind: Kind,
    pub system: String,
    pub who: String,
    pub summary: String,
    pub url: String,
    /// When the thing began, if the producer knows; else now.
    pub created: Option<DateTime<Utc>>,
}

impl NewFollowUp {
    pub fn id(&self, handle: &str) -> String {
        followup_id(handle, self.kind, &self.system, &self.url)
    }
}

/// One thing that changed since yesterday.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changed {
    pub system: String,
    pub title: String,
    pub summary: String,
    pub url: String,
}

/// The day's `changed` items for one PM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Day {
    pub date: NaiveDate,
    pub changed: Vec<Changed>,
    pub updated: DateTime<Utc>,
}

pub fn followup_id(handle: &str, kind: Kind, system: &str, url: &str) -> String {
    let mut h = Sha256::new();
    for part in [handle, kind.as_str(), system, url] {
        h.update(part.as_bytes());
        h.update([0]);
    }
    let digest = h.finalize();
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("fu_{hex}")
}

/// When a follow-up from `system` created or refreshed at `now` goes.
pub fn expiry(system: &str, now: DateTime<Utc>) -> DateTime<Utc> {
    if system == "slack" {
        now + Duration::hours(SLACK_TTL_HOURS)
    } else {
        now + Duration::days(TTL_DAYS)
    }
}

/// Where follow-ups live. Every method takes the PM's handle and touches only their rows.
#[async_trait]
pub trait FollowUpStore: Send + Sync {
    /// The PM's open follow-ups (not dismissed, not expired at `now`), oldest first.
    async fn list(&self, handle: &str, now: DateTime<Utc>) -> Result<Vec<FollowUp>, StoreError>;

    /// One open follow-up of this PM; `None` when it is not theirs, dismissed, expired or
    /// unknown.
    async fn open(
        &self,
        handle: &str,
        id: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<FollowUp>, StoreError>;

    /// Dismiss one. `true` when the PM has such a follow-up that hasn't expired, whether or
    /// not it was already dismissed (so a repeat is a no-op); `false` otherwise.
    async fn dismiss(&self, handle: &str, id: &str, now: DateTime<Utc>)
        -> Result<bool, StoreError>;

    /// Replace the PM's follow-ups and the day's `changed` items. A follow-up already held
    /// (same id) keeps its `created` and `dismissed` and gets a fresh expiry; others are
    /// added; any the producer no longer lists is removed.
    async fn replace(
        &self,
        handle: &str,
        date: NaiveDate,
        changed: Vec<Changed>,
        followups: Vec<NewFollowUp>,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError>;

    /// The day's `changed` items, `None` when nothing is stored or it has expired.
    async fn day(&self, handle: &str, now: DateTime<Utc>) -> Result<Option<Day>, StoreError>;

    /// Delete everything expired; the number of rows removed.
    async fn purge(&self, now: DateTime<Utc>) -> Result<u64, StoreError>;
}

#[derive(Default)]
struct PmRows {
    followups: Vec<FollowUp>,
    day: Option<Day>,
}

#[derive(Default)]
pub struct MemoryFollowUpStore {
    pms: Mutex<HashMap<String, PmRows>>,
}

impl MemoryFollowUpStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl FollowUpStore for MemoryFollowUpStore {
    async fn list(&self, handle: &str, now: DateTime<Utc>) -> Result<Vec<FollowUp>, StoreError> {
        let pms = self.pms.lock().expect("followups lock");
        let mut out: Vec<FollowUp> = pms
            .get(handle)
            .map(|p| {
                p.followups
                    .iter()
                    .filter(|f| !f.dismissed && f.expires > now)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by(|a, b| a.created.cmp(&b.created).then(a.id.cmp(&b.id)));
        Ok(out)
    }

    async fn open(
        &self,
        handle: &str,
        id: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<FollowUp>, StoreError> {
        Ok(self
            .pms
            .lock()
            .expect("followups lock")
            .get(handle)
            .and_then(|p| {
                p.followups
                    .iter()
                    .find(|f| f.id == id && !f.dismissed && f.expires > now)
                    .cloned()
            }))
    }

    async fn dismiss(
        &self,
        handle: &str,
        id: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        let mut pms = self.pms.lock().expect("followups lock");
        let found = pms.get_mut(handle).and_then(|p| {
            p.followups
                .iter_mut()
                .find(|f| f.id == id && f.expires > now)
        });
        Ok(match found {
            Some(f) => {
                f.dismissed = true;
                true
            }
            None => false,
        })
    }

    async fn replace(
        &self,
        handle: &str,
        date: NaiveDate,
        changed: Vec<Changed>,
        followups: Vec<NewFollowUp>,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut pms = self.pms.lock().expect("followups lock");
        let pm = pms.entry(handle.to_string()).or_default();
        let old = std::mem::take(&mut pm.followups);
        for new in followups {
            let id = new.id(handle);
            if pm.followups.iter().any(|f| f.id == id) {
                continue;
            }
            let before = old.iter().find(|f| f.id == id);
            pm.followups.push(FollowUp {
                id,
                kind: new.kind,
                created: before.map(|f| f.created).or(new.created).unwrap_or(now),
                expires: expiry(&new.system, now),
                dismissed: before.is_some_and(|f| f.dismissed),
                system: new.system,
                who: new.who,
                summary: new.summary,
                url: new.url,
            });
        }
        pm.day = Some(Day {
            date,
            changed,
            updated: now,
        });
        Ok(())
    }

    async fn day(&self, handle: &str, now: DateTime<Utc>) -> Result<Option<Day>, StoreError> {
        Ok(self
            .pms
            .lock()
            .expect("followups lock")
            .get(handle)
            .and_then(|p| p.day.clone())
            .filter(|d| d.updated + Duration::hours(CHANGED_TTL_HOURS) > now))
    }

    async fn purge(&self, now: DateTime<Utc>) -> Result<u64, StoreError> {
        let mut removed = 0;
        for pm in self.pms.lock().expect("followups lock").values_mut() {
            let before = pm.followups.len();
            pm.followups.retain(|f| f.expires > now);
            removed += (before - pm.followups.len()) as u64;
            if pm
                .day
                .as_ref()
                .is_some_and(|d| d.updated + Duration::hours(CHANGED_TTL_HOURS) <= now)
            {
                pm.day = None;
                removed += 1;
            }
        }
        Ok(removed)
    }
}

/// Whether the daily brief is on, and the token the producer sends.
#[derive(Clone, Default)]
pub struct BriefConfig {
    /// `PMOS_WEB_BRIEF=1`.
    pub enabled: bool,
    /// `PMOS_WEB_BRIEF_TOKEN`: what `PUT /internal/brief/{handle}` must present.
    pub token: Option<String>,
}

impl std::fmt::Debug for BriefConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BriefConfig")
            .field("enabled", &self.enabled)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(h: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-16T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + Duration::hours(h)
    }

    #[test]
    fn ids_are_stable_and_depend_on_the_owner() {
        let a = followup_id("ana", Kind::WaitingOnYou, "slack", "https://x/1");
        assert_eq!(
            a,
            followup_id("ana", Kind::WaitingOnYou, "slack", "https://x/1")
        );
        assert_ne!(
            a,
            followup_id("ben", Kind::WaitingOnYou, "slack", "https://x/1")
        );
        assert_ne!(
            a,
            followup_id("ana", Kind::YouWaitOn, "slack", "https://x/1")
        );
        assert!(a.starts_with("fu_") && a.len() == 19);
    }

    #[test]
    fn slack_expires_in_a_day_the_rest_in_a_week() {
        assert_eq!(expiry("slack", t(0)), t(24));
        assert_eq!(expiry("jira", t(0)), t(24 * 7));
    }
}
