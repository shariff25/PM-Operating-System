//! The daily triage digest (`docs/improvement-loop-spec.md` §4). On weekdays at
//! `PMOS_TRIAGE_AT` (default 09:00) in `PMOS_DEFAULT_TZ`, each bot's quality owner gets one DM
//! listing the drafts that were rejected, or approved after an edit that changed at least
//! `PMOS_TRIAGE_MIN_EDIT` (default 0.3) of the text.
//!
//! - **Who gets it:** a bot's `owner` from the runtime's `GET /v1/bots`, one DM per owner for
//!   all their bots. A bot with no owner, or whose owner isn't in `team.yaml`, is logged and
//!   skipped.
//! - **The rows:** from the tool server's feedback ledger (`GET /internal/ledger`), which holds
//!   labels and metadata only, never message text. Each shows the tool, who it was for, the
//!   outcome and reason, the edit distance, and a link to the console's task page when
//!   `PMOS_CONSOLE_PUBLIC_URL` is set. The digest ends with the case-card prompt.
//! - **The window:** the drafts decided since the previous digest's time, so Monday's covers
//!   the weekend. The query reaches back a week before that, so a draft made earlier but
//!   decided in the window is still caught.
//! - **Nothing happened:** no message.
//! - **Once a day:** the date each owner was last sent a digest is kept in
//!   `<state dir>/triage.sent`, so a restart never sends a second one that day. A restart
//!   after the digest's time on a weekday sends that day's digest late, to owners who haven't
//!   had it.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Datelike, Days, NaiveDate, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::blocks::{escape, link};
use crate::bots::Bots;
use crate::cards::{self, reason_label};
use crate::runtime::{safe_id, BotInfo};
use crate::toolserver::{LedgerQuery, LEDGER_LIMIT};

pub const SENT_FILE: &str = "triage.sent";
pub const DEFAULT_AT: &str = "09:00";
pub const DEFAULT_MIN_EDIT: f64 = 0.3;
/// How far before the window the ledger query reaches, for drafts decided late.
const LOOKBACK: chrono::Duration = chrono::Duration::days(7);
/// The most rows one digest lists; the rest are counted.
const SHOWN_MAX: usize = 30;

pub const CASE_CARD: &str = "Worth keeping? Write a case card: `evals/case-card.md`";
pub const TRACE_REMINDER: &str =
    "_Slack text leaves the trace after 24 hours, so look at a task's trace today._";

#[derive(Debug, Clone, PartialEq)]
pub struct TriageConfig {
    /// The local time of day it is sent.
    pub at: NaiveTime,
    pub tz: Tz,
    /// Edited drafts are listed from this edit distance up.
    pub min_edit: f64,
    /// `PMOS_CONSOLE_PUBLIC_URL`, without a trailing `/`.
    pub console_url: Option<String>,
}

/// `PMOS_TRIAGE_AT`: unset for the default, `off` to turn the digest off, else `HH:MM`.
pub fn parse_at(value: Option<&str>) -> Result<Option<NaiveTime>, String> {
    let value = value.map(str::trim).filter(|v| !v.is_empty());
    match value {
        Some(v) if v.eq_ignore_ascii_case("off") => Ok(None),
        v => NaiveTime::parse_from_str(v.unwrap_or(DEFAULT_AT), "%H:%M")
            .map(Some)
            .map_err(|_| "PMOS_TRIAGE_AT must be a time such as 09:00, or off".to_string()),
    }
}

/// `PMOS_TRIAGE_MIN_EDIT`: unset for the default, else a number from 0 to 1.
pub fn parse_min_edit(value: Option<&str>) -> Result<f64, String> {
    let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(DEFAULT_MIN_EDIT);
    };
    v.parse::<f64>()
        .ok()
        .filter(|n| (0.0..=1.0).contains(n))
        .ok_or_else(|| "PMOS_TRIAGE_MIN_EDIT must be a number from 0 to 1, such as 0.3".into())
}

// ---- When ----

/// `at` on `date` in `tz`, as UTC. A time the clocks skip moves to the first minute after
/// the gap; a time that happens twice is the first.
pub fn local_time(date: NaiveDate, at: NaiveTime, tz: Tz) -> Option<DateTime<Utc>> {
    let mut local = date.and_time(at);
    for _ in 0..=180 {
        if let Some(t) = tz.from_local_datetime(&local).earliest() {
            return Some(t.with_timezone(&Utc));
        }
        local += chrono::Duration::minutes(1);
    }
    None
}

/// The digest's time on `date` (in `tz`), if `date` is a weekday.
fn slot_on(date: NaiveDate, at: NaiveTime, tz: Tz) -> Option<DateTime<Utc>> {
    if matches!(date.weekday(), Weekday::Sat | Weekday::Sun) {
        return None;
    }
    local_time(date, at, tz)
}

/// The latest digest time at or before `now`.
pub fn last_slot(now: DateTime<Utc>, at: NaiveTime, tz: Tz) -> Option<DateTime<Utc>> {
    let today = now.with_timezone(&tz).date_naive();
    (0..9)
        .filter_map(|back| today.checked_sub_days(Days::new(back)))
        .filter_map(|d| slot_on(d, at, tz))
        .find(|s| *s <= now)
}

/// The first digest time after `now`.
pub fn next_slot(now: DateTime<Utc>, at: NaiveTime, tz: Tz) -> Option<DateTime<Utc>> {
    let today = now.with_timezone(&tz).date_naive();
    (0..9)
        .filter_map(|ahead| today.checked_add_days(Days::new(ahead)))
        .filter_map(|d| slot_on(d, at, tz))
        .find(|s| *s > now)
}

/// The window a digest sent at `slot` covers: from the previous digest's time to `slot`.
pub fn window(slot: DateTime<Utc>, at: NaiveTime, tz: Tz) -> (DateTime<Utc>, DateTime<Utc>) {
    let since = last_slot(slot - chrono::Duration::seconds(1), at, tz)
        .unwrap_or(slot - chrono::Duration::days(1));
    (since, slot)
}

// ---- What ----

/// Rejected, or edited by at least `min_edit`.
pub fn qualifies(row: &Value, min_edit: f64) -> bool {
    match row["outcome"].as_str() {
        Some("rejected") => true,
        Some("edited") => row["edit_distance"].as_f64().is_some_and(|d| d >= min_edit),
        _ => false,
    }
}

/// When a row was decided (`decided_at`, else `created_at`).
fn decided_at(row: &Value) -> Option<DateTime<Utc>> {
    row["decided_at"]
        .as_str()
        .or(row["created_at"].as_str())
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc))
}

fn outcome_text(row: &Value) -> String {
    match row["outcome"].as_str().unwrap_or_default() {
        "rejected" => match row["reason"].as_str() {
            Some(code) => format!("rejected: {}", escape(reason_label(code).unwrap_or(code))),
            None => "rejected, no reason given".into(),
        },
        "edited" => {
            let pct = row["edit_distance"].as_f64().unwrap_or(0.0) * 100.0;
            format!("edited, {pct:.0}% changed")
        }
        other => escape(other),
    }
}

/// One row: the tool, who it was for, the outcome and reason or edit distance, and the
/// console link. Everything from the ledger is escaped.
pub fn row_line(row: &Value, console_url: Option<&str>) -> String {
    let mut line = format!(
        "• `{}` for {} · {}",
        escape(row["tool"].as_str().unwrap_or("?")),
        escape(row["user"].as_str().unwrap_or("?")),
        outcome_text(row)
    );
    if let (Some(base), Some(run)) = (console_url, row["run_id"].as_str()) {
        if safe_id(run) {
            line.push_str(" · ");
            line.push_str(&link(&format!("{base}/tasks/{run}"), "open the task"));
        }
    }
    line
}

/// One owner's digest: their bots' rows under each bot's name, newest first, then the
/// case-card prompt.
pub fn digest_message(
    groups: &[(BotInfo, Vec<Value>)],
    since: DateTime<Tz>,
    console_url: Option<&str>,
) -> (String, Value) {
    let total: usize = groups.iter().map(|(_, rows)| rows.len()).sum();
    let mut lines = vec![format!(
        "*Drafts worth a look:* {total} rejected or heavily edited since {}",
        since.format("%a %-d %b %H:%M %Z")
    )];
    let mut shown = 0;
    for (bot, rows) in groups {
        if shown == SHOWN_MAX {
            break;
        }
        lines.push(String::new());
        lines.push(format!(
            "*{}* (`{}`)",
            escape(&bot.display_name()),
            escape(&bot.name)
        ));
        for row in rows.iter().take(SHOWN_MAX - shown) {
            lines.push(row_line(row, console_url));
            shown += 1;
        }
    }
    if total > shown {
        lines.push(format!("_…and {} more in the ledger._", total - shown));
    }
    let mut blocks = cards::sections(&lines.join("\n"));
    blocks.push(json!({"type": "section", "text": {"type": "mrkdwn", "text": CASE_CARD}}));
    blocks
        .push(json!({"type": "context", "elements": [{"type": "mrkdwn", "text": TRACE_REMINDER}]}));
    (format!("{total} drafts to triage"), Value::Array(blocks))
}

// ---- The loop ----

pub struct Triage {
    bots: Bots,
    cfg: TriageConfig,
    sent_file: PathBuf,
    /// How long to wait before trying a failed digest again. Default 5 minutes.
    pub retry: Duration,
    /// How many times a digest is tried. Default 6.
    pub attempts: u32,
}

impl Triage {
    pub fn new(bots: Bots, cfg: TriageConfig, state_dir: impl AsRef<Path>) -> Self {
        Self {
            bots,
            cfg,
            sent_file: state_dir.as_ref().join(SENT_FILE),
            retry: Duration::from_secs(300),
            attempts: 6,
        }
    }

    /// Send each digest at its time until `shutdown`.
    pub async fn run(self, shutdown: CancellationToken) {
        if let Some(dir) = self.sent_file.parent() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                tracing::error!(error = %e, dir = %dir.display(), "can't create the Slack state directory; a restart could send a second digest");
            }
        }
        let (at, tz) = (self.cfg.at, self.cfg.tz);
        let now = Utc::now();
        // Started after today's time: today's digest, to whoever hasn't had it.
        if let Some(slot) = last_slot(now, at, tz)
            .filter(|s| s.with_timezone(&tz).date_naive() == now.with_timezone(&tz).date_naive())
        {
            self.send_with_retry(slot, &shutdown).await;
        }
        loop {
            let Some(next) = next_slot(Utc::now(), at, tz) else {
                tracing::error!("no time for the next triage digest; stopping it");
                return;
            };
            tracing::info!(next = %next, "next triage digest");
            // At most an hour at a time, so a suspended host or a clock change can't make
            // it late by more than that.
            while let Ok(left) = (next - Utc::now()).to_std() {
                if left.is_zero() {
                    break;
                }
                tokio::select! {
                    _ = shutdown.cancelled() => return,
                    _ = tokio::time::sleep(left.min(Duration::from_secs(3600))) => {}
                }
            }
            self.send_with_retry(next, &shutdown).await;
        }
    }

    async fn send_with_retry(&self, slot: DateTime<Utc>, shutdown: &CancellationToken) {
        for attempt in 1..=self.attempts {
            match self.send(slot).await {
                Ok(sent) => {
                    tracing::info!(sent, slot = %slot, "triage digest done");
                    return;
                }
                Err(e) if attempt < self.attempts => {
                    tracing::warn!(error = %e, attempt, "triage digest failed; trying again");
                    tokio::select! {
                        _ = shutdown.cancelled() => return,
                        _ = tokio::time::sleep(self.retry) => {}
                    }
                }
                Err(e) => tracing::error!(error = %e, slot = %slot, "triage digest gave up"),
            }
        }
    }

    /// The digest for the time `slot`: one DM per owner with rows, unless they already had
    /// one that day. Returns how many were sent, or an error if any DM failed: trying again
    /// sends only to the owners who didn't get theirs.
    pub async fn send(&self, slot: DateTime<Utc>) -> Result<usize, String> {
        let (at, tz) = (self.cfg.at, self.cfg.tz);
        let (since, until) = window(slot, at, tz);
        let day = slot.with_timezone(&tz).date_naive().to_string();
        let rows = self.rows(since, until).await?;
        if rows.is_empty() {
            tracing::info!("nothing to triage; no digest");
            return Ok(0);
        }
        let roster = self
            .bots
            .roster()
            .await
            .map_err(|e| format!("the bot list: {e}"))?;
        let team = self.bots.team();

        // owner → bot → rows, newest first.
        let mut owners: BTreeMap<String, BTreeMap<String, (BotInfo, Vec<Value>)>> = BTreeMap::new();
        let mut skipped: HashSet<String> = HashSet::new();
        for row in rows {
            let name = row["bot"].as_str().unwrap_or_default().to_string();
            let Some(bot) = roster.iter().find(|b| b.name == name) else {
                if skipped.insert(name.clone()) {
                    tracing::warn!(bot = %name, "ledger rows for a bot the runtime doesn't list; skipped");
                }
                continue;
            };
            let Some(owner) = bot.owner.as_deref().filter(|o| !o.is_empty()) else {
                if skipped.insert(name.clone()) {
                    tracing::warn!(bot = %name, "bot has no quality owner; its rows are skipped");
                }
                continue;
            };
            if team.by_handle(owner).is_none() {
                if skipped.insert(name.clone()) {
                    tracing::warn!(bot = %name, owner, "bot's owner isn't a team member; its rows are skipped");
                }
                continue;
            }
            owners
                .entry(owner.to_string())
                .or_default()
                .entry(name)
                .or_insert_with(|| (bot.clone(), Vec::new()))
                .1
                .push(row);
        }

        let (mut sent, mut failed) = (0, 0);
        for (owner, bots) in owners {
            let mut record = read_sent(&self.sent_file);
            if record.get(&owner) == Some(&day) {
                tracing::info!(owner, day, "triage digest already sent today");
                continue;
            }
            let Some(member) = team.by_handle(&owner) else {
                continue;
            };
            let groups: Vec<(BotInfo, Vec<Value>)> = bots.into_values().collect();
            let count: usize = groups.iter().map(|(_, r)| r.len()).sum();
            let message = digest_message(
                &groups,
                since.with_timezone(&tz),
                self.cfg.console_url.as_deref(),
            );
            // Recorded first, so a crash after the post can't send it twice; undone if the
            // post fails.
            let before = record.insert(owner.clone(), day.clone());
            save_sent(&self.sent_file, &record);
            if self
                .bots
                .post(&member.slack_id, None, None, message)
                .await
                .is_some()
            {
                tracing::info!(owner, rows = count, "triage digest sent");
                sent += 1;
            } else {
                let mut record = read_sent(&self.sent_file);
                match before {
                    Some(previous) => record.insert(owner.clone(), previous),
                    None => record.remove(&owner),
                };
                save_sent(&self.sent_file, &record);
                failed += 1;
            }
        }
        if failed > 0 {
            return Err(format!(
                "{failed} of {} digests couldn't be posted",
                sent + failed
            ));
        }
        Ok(sent)
    }

    /// The qualifying rows decided in `(since, until]`, newest first. The two queries ask the
    /// ledger for exactly what qualifies; the same checks are applied here as well.
    async fn rows(&self, since: DateTime<Utc>, until: DateTime<Utc>) -> Result<Vec<Value>, String> {
        let base = LedgerQuery {
            since: since - LOOKBACK,
            until,
            bot: None,
            outcomes: Vec::new(),
            min_edit: None,
            limit: LEDGER_LIMIT,
        };
        let ts = self.bots.toolserver();
        let rejected = ts
            .ledger(&LedgerQuery {
                outcomes: vec!["rejected"],
                ..base.clone()
            })
            .await
            .map_err(|e| format!("the ledger: {e}"))?;
        let edited = ts
            .ledger(&LedgerQuery {
                outcomes: vec!["edited"],
                min_edit: Some(self.cfg.min_edit),
                ..base
            })
            .await
            .map_err(|e| format!("the ledger: {e}"))?;
        if rejected.len() >= LEDGER_LIMIT || edited.len() >= LEDGER_LIMIT {
            tracing::warn!("the ledger returned its limit; the digest may miss older rows");
        }
        let mut ids = HashSet::new();
        let mut rows: Vec<(DateTime<Utc>, Value)> = rejected
            .into_iter()
            .chain(edited)
            .filter(|r| qualifies(r, self.cfg.min_edit))
            .filter_map(|r| decided_at(&r).map(|t| (t, r)))
            .filter(|(t, _)| *t > since && *t <= until)
            .filter(|(_, r)| ids.insert(r["id"].to_string()))
            .collect();
        rows.sort_by_key(|(t, _)| Reverse(*t));
        Ok(rows.into_iter().map(|(_, r)| r).collect())
    }
}

/// Each owner's handle and the local date they were last sent a digest.
fn read_sent(path: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Write then rename, so a crash never leaves half a file.
fn save_sent(path: &Path, record: &BTreeMap<String, String>) {
    let tmp = path.with_extension("tmp");
    let body = serde_json::to_string_pretty(record).unwrap_or_default();
    if let Err(e) = std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, path)) {
        tracing::error!(error = %e, "saving the triage digest's sent dates failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn nine() -> NaiveTime {
        NaiveTime::from_hms_opt(9, 0, 0).unwrap()
    }

    #[test]
    fn settings() {
        assert_eq!(parse_at(None), Ok(Some(nine())));
        assert_eq!(parse_at(Some(" ")), Ok(Some(nine())));
        assert_eq!(
            parse_at(Some("07:45")),
            Ok(NaiveTime::from_hms_opt(7, 45, 0))
        );
        assert_eq!(parse_at(Some("OFF")), Ok(None));
        assert!(parse_at(Some("9am")).is_err());
        assert!(parse_at(Some("25:00")).is_err());
        assert_eq!(parse_min_edit(None), Ok(0.3));
        assert_eq!(parse_min_edit(Some("0.5")), Ok(0.5));
        assert!(parse_min_edit(Some("1.5")).is_err());
        assert!(parse_min_edit(Some("lots")).is_err());
    }

    #[test]
    fn weekdays_at_the_local_time() {
        let la: Tz = "America/Los_Angeles".parse().unwrap();
        // Friday 13 Nov 2026, 09:30 in Los Angeles (PST, UTC-8).
        let fri = utc("2026-11-13T17:30:00Z");
        assert_eq!(
            last_slot(fri, nine(), la),
            Some(utc("2026-11-13T17:00:00Z"))
        );
        // The next is Monday: the weekend is skipped.
        assert_eq!(
            next_slot(fri, nine(), la),
            Some(utc("2026-11-16T17:00:00Z"))
        );
        let sat = utc("2026-11-14T20:00:00Z");
        assert_eq!(
            last_slot(sat, nine(), la),
            Some(utc("2026-11-13T17:00:00Z"))
        );
        assert_eq!(
            next_slot(sat, nine(), la),
            Some(utc("2026-11-16T17:00:00Z"))
        );
        // A minute before Monday's: Monday's is next, Friday's the last.
        let mon = utc("2026-11-16T16:59:00Z");
        assert_eq!(
            next_slot(mon, nine(), la),
            Some(utc("2026-11-16T17:00:00Z"))
        );
        assert_eq!(
            last_slot(mon, nine(), la),
            Some(utc("2026-11-13T17:00:00Z"))
        );
        // At the time itself: that's the last, and the next is tomorrow.
        let at = utc("2026-11-16T17:00:00Z");
        assert_eq!(last_slot(at, nine(), la), Some(at));
        assert_eq!(next_slot(at, nine(), la), Some(utc("2026-11-17T17:00:00Z")));
        // Monday's digest covers the weekend: since Friday's.
        assert_eq!(window(at, nine(), la), (utc("2026-11-13T17:00:00Z"), at));
        let tue = utc("2026-11-17T17:00:00Z");
        assert_eq!(window(tue, nine(), la), (at, tue));
    }

    #[test]
    fn weekdays_are_the_zones_own() {
        // Sunday 23:30 UTC is already Monday 08:30 in Tokyo: Monday's digest is 30 minutes off.
        let tokyo: Tz = "Asia/Tokyo".parse().unwrap();
        let sun = utc("2026-11-15T23:30:00Z");
        assert_eq!(
            next_slot(sun, nine(), tokyo),
            Some(utc("2026-11-16T00:00:00Z"))
        );
        // Friday 23:30 UTC is Saturday in Tokyo: nothing until Monday there.
        let fri = utc("2026-11-20T23:30:00Z");
        assert_eq!(
            next_slot(fri, nine(), tokyo),
            Some(utc("2026-11-23T00:00:00Z"))
        );
        assert_eq!(
            last_slot(fri, nine(), tokyo),
            Some(utc("2026-11-20T00:00:00Z"))
        );
        // Across a clock change the local time holds: 09:00 EDT, then 09:00 EST.
        let ny: Tz = "America/New_York".parse().unwrap();
        let before = utc("2026-10-30T14:00:00Z");
        assert_eq!(
            last_slot(before, nine(), ny),
            Some(utc("2026-10-30T13:00:00Z"))
        );
        assert_eq!(
            next_slot(before, nine(), ny),
            Some(utc("2026-11-02T14:00:00Z"))
        );
        // A time the clocks skip moves to just after the gap; a repeated one is the first.
        let gap = NaiveTime::from_hms_opt(2, 30, 0).unwrap();
        let spring = NaiveDate::from_ymd_opt(2026, 3, 8).unwrap();
        assert_eq!(
            local_time(spring, gap, ny),
            Some(utc("2026-03-08T07:00:00Z"))
        );
        let twice = NaiveTime::from_hms_opt(1, 30, 0).unwrap();
        let fall = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
        assert_eq!(
            local_time(fall, twice, ny),
            Some(utc("2026-11-01T05:30:00Z"))
        );
        // UTC.
        assert_eq!(
            next_slot(utc("2026-11-16T09:00:00Z"), nine(), Tz::UTC),
            Some(utc("2026-11-17T09:00:00Z"))
        );
    }

    #[test]
    fn rows_and_lines() {
        assert!(qualifies(&json!({"outcome": "rejected"}), 0.3));
        assert!(qualifies(
            &json!({"outcome": "edited", "edit_distance": 0.3}),
            0.3
        ));
        assert!(!qualifies(
            &json!({"outcome": "edited", "edit_distance": 0.29}),
            0.3
        ));
        assert!(!qualifies(
            &json!({"outcome": "edited", "edit_distance": null}),
            0.3
        ));
        assert!(!qualifies(
            &json!({"outcome": "approved", "edit_distance": 0.0}),
            0.0
        ));
        let row = json!({
            "tool": "jira.create_issue", "user": "ana", "outcome": "rejected",
            "reason": "wrong_target", "run_id": "tsk_1",
        });
        assert_eq!(
            row_line(&row, Some("https://console.example.com")),
            "• `jira.create_issue` for ana · rejected: Wrong place · <https://console.example.com/tasks/tsk_1|open the task>"
        );
        assert_eq!(
            row_line(&row, None),
            "• `jira.create_issue` for ana · rejected: Wrong place"
        );
        let odd = json!({
            "tool": "x.<b>", "user": "a&b", "outcome": "edited", "edit_distance": 0.456,
            "run_id": "../x",
        });
        assert_eq!(
            row_line(&odd, Some("https://console.example.com")),
            "• `x.&lt;b&gt;` for a&amp;b · edited, 46% changed",
            "escaped, and no link for an unsafe run id"
        );
        let none = json!({"tool": "t", "user": "u", "outcome": "rejected", "reason": null});
        assert!(row_line(&none, None).ends_with("rejected, no reason given"));
    }
}
