//! The feedback ledger (`docs/improvement-loop-spec.md` §1): one row per approval item, so
//! one per drafted outbound action, recording what became of it.
//!
//! The approval stores write a row when they create an item and update it when the item is
//! decided and when its write finishes, in the same operation (in Postgres, the same
//! transaction), so a row is never missing. Rows hold labels and metadata only, never the
//! draft's text or anything from Slack.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Days a pending row waits before the ledger reports it as `expired`
/// (`PMOS_LEDGER_EXPIRE_DAYS`).
pub const DEFAULT_EXPIRE_DAYS: i64 = 7;
/// The most rows one `GET /internal/ledger` returns.
pub const MAX_LIMIT: usize = 1000;
/// Each side of an edit distance is cut to this many characters first, so one decision
/// never costs more than a bounded amount of work: a longer text is compared on its first
/// this many characters. The Postgres store computes it outside its transaction.
pub const EDIT_TEXT_CAP: usize = 10_000;

/// What became of a draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerOutcome {
    /// Waiting for its PM, or approved and still being written.
    Pending,
    /// Approved unchanged, and written.
    Approved,
    /// Approved after an edit, and written.
    Edited,
    Rejected,
    /// Approved, but the write failed or its outcome is unknown.
    Failed,
    /// Never decided. Stored for a Slack draft that expired after
    /// `PMOS_SLACK_DRAFT_TTL_HOURS`; for any other tool, worked out when the row is read, for
    /// a row pending longer than `PMOS_LEDGER_EXPIRE_DAYS`.
    Expired,
}

impl LedgerOutcome {
    pub const ALL: [Self; 6] = [
        Self::Pending,
        Self::Approved,
        Self::Edited,
        Self::Rejected,
        Self::Failed,
        Self::Expired,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Edited => "edited",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
            Self::Expired => "expired",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|o| o.as_str() == s)
    }

    /// Decided, or expired: a row with a label (phase 0's coverage gate).
    pub fn is_labelled(self) -> bool {
        self != Self::Pending
    }
}

/// Why a PM rejected a draft. Optional, and only with a rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    WrongFacts,
    WrongTarget,
    Tone,
    Incomplete,
    ShouldNotDraft,
    Other,
}

impl Reason {
    pub const ALL: [Self; 6] = [
        Self::WrongFacts,
        Self::WrongTarget,
        Self::Tone,
        Self::Incomplete,
        Self::ShouldNotDraft,
        Self::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::WrongFacts => "wrong_facts",
            Self::WrongTarget => "wrong_target",
            Self::Tone => "tone",
            Self::Incomplete => "incomplete",
            Self::ShouldNotDraft => "should_not_draft",
            Self::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == s)
    }
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One ledger row, as `GET /internal/ledger` returns it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LedgerRow {
    /// The approval item's id.
    pub id: String,
    /// The task id (`tsk_…`) or skill run id.
    pub run_id: String,
    /// The PM the draft is for.
    pub user: String,
    pub bot: Option<String>,
    pub skill: Option<String>,
    /// Registry name, such as `jira.create_issue`.
    pub tool: String,
    /// The content repo's `HEAD` when the item was created; `None` outside a git checkout.
    pub content_sha: Option<String>,
    /// The drafting role's gateway alias, such as `pmos/default`.
    pub model_alias: Option<String>,
    pub outcome: LedgerOutcome,
    /// `0.0`–`1.0` once approved; `None` while pending and when rejected.
    pub edit_distance: Option<f64>,
    pub reason: Option<Reason>,
    pub created_at: DateTime<Utc>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decided_by: Option<String>,
}

impl LedgerRow {
    /// When the row last changed: its decision, or its creation while undecided. The
    /// ledger's time filters and order use this.
    pub fn activity_at(&self) -> DateTime<Utc> {
        self.decided_at.unwrap_or(self.created_at)
    }

    /// The row as reported: a stored `pending` row that was never decided and was created
    /// before `expire_before` reads as `expired`. A stored `expired` row reads as it is.
    pub fn reported(mut self, expire_before: DateTime<Utc>) -> Self {
        if self.outcome == LedgerOutcome::Pending
            && self.decided_at.is_none()
            && self.created_at < expire_before
        {
            self.outcome = LedgerOutcome::Expired;
        }
        self
    }
}

/// A ledger read. Times match [`LedgerRow::activity_at`]: `since` inclusive, `until`
/// exclusive.
#[derive(Debug, Clone, PartialEq)]
pub struct LedgerQuery {
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub bot: Option<String>,
    /// Reported outcomes to keep; empty keeps all.
    pub outcomes: Vec<LedgerOutcome>,
    /// Drop rows whose edit distance is below this. Rows with no edit distance (pending,
    /// rejected) are kept, so `outcome=rejected,edited&min_edit=0.3` reads "rejected, or
    /// edited by at least 0.3".
    pub min_edit: Option<f64>,
    /// `None` for every match.
    pub limit: Option<usize>,
    /// Pending rows created before this are reported as `expired`.
    pub expire_before: DateTime<Utc>,
}

impl LedgerQuery {
    /// Every row, with pending rows created before `expire_before` reported as expired.
    pub fn all(expire_before: DateTime<Utc>) -> Self {
        Self {
            since: None,
            until: None,
            bot: None,
            outcomes: Vec::new(),
            min_edit: None,
            limit: None,
            expire_before,
        }
    }

    /// Does a reported row pass the filters (all but `limit`)?
    pub fn matches(&self, row: &LedgerRow) -> bool {
        let at = row.activity_at();
        self.since.is_none_or(|s| at >= s)
            && self.until.is_none_or(|u| at < u)
            && self
                .bot
                .as_deref()
                .is_none_or(|b| row.bot.as_deref() == Some(b))
            && (self.outcomes.is_empty() || self.outcomes.contains(&row.outcome))
            && match (self.min_edit, row.edit_distance) {
                (Some(min), Some(d)) => d >= min,
                _ => true,
            }
    }
}

/// The text an edit distance is measured on: the item's string arguments, at any depth,
/// in key order (arrays in their own order), joined with newlines. A reply's top-level
/// `message_ts` is left out: it is an id, never edited, not draft text.
pub fn draft_text(args: &Value) -> String {
    fn walk(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::String(s) => out.push(s.clone()),
            Value::Array(items) => items.iter().for_each(|v| walk(v, out)),
            Value::Object(map) => {
                let sorted: BTreeMap<&String, &Value> = map.iter().collect();
                sorted.into_values().for_each(|v| walk(v, out));
            }
            _ => {}
        }
    }
    let mut parts = Vec::new();
    match args {
        Value::Object(map) => {
            let sorted: BTreeMap<&String, &Value> =
                map.iter().filter(|(k, _)| *k != "message_ts").collect();
            sorted.into_values().for_each(|v| walk(v, &mut parts));
        }
        other => walk(other, &mut parts),
    }
    parts.join("\n")
}

/// Normalized character-level Levenshtein distance: the edits needed to turn `a` into `b`,
/// divided by the longer one's length. `0.0` for equal texts, `1.0` for nothing in common.
/// Each side is cut to [`EDIT_TEXT_CAP`] characters first, and a shared prefix and suffix
/// are skipped, so small edits to long drafts stay cheap.
pub fn edit_distance(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().take(EDIT_TEXT_CAP).collect();
    let b: Vec<char> = b.chars().take(EDIT_TEXT_CAP).collect();
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 0.0;
    }
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let (a, b) = (&a[prefix..], &b[prefix..]);
    let suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (a, b) = (&a[..a.len() - suffix], &b[..b.len() - suffix]);
    // Two rows of the usual table, over the shorter side.
    let (long, short) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let mut prev: Vec<usize> = (0..=short.len()).collect();
    let mut row = vec![0; short.len() + 1];
    for (i, lc) in long.iter().enumerate() {
        row[0] = i + 1;
        for (j, sc) in short.iter().enumerate() {
            let substitute = prev[j] + usize::from(lc != sc);
            row[j + 1] = substitute.min(prev[j + 1] + 1).min(row[j] + 1);
        }
        std::mem::swap(&mut prev, &mut row);
    }
    prev[short.len()] as f64 / longest as f64
}

/// The edit distance between an item's original and final arguments.
pub fn args_edit_distance(original: &Value, edited: &Value) -> f64 {
    edit_distance(&draft_text(original), &draft_text(edited))
}

/// One bot (or skill) and tool in a ledger summary.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SummaryGroup {
    pub bot: Option<String>,
    pub skill: Option<String>,
    pub tool: String,
    pub rows: usize,
    /// Rows by reported outcome; every outcome is listed.
    pub counts: BTreeMap<&'static str, usize>,
    /// The share of rows that are decided or expired.
    pub label_coverage: f64,
    /// Over the rows that have an edit distance; `None` if none do.
    pub median_edit_distance: Option<f64>,
    /// Rejections by reason; `none` counts rejections without one.
    pub reasons: BTreeMap<&'static str, usize>,
}

/// `GET /internal/ledger/summary`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    pub since: Option<DateTime<Utc>>,
    pub rows: usize,
    /// Over every row in the summary.
    pub label_coverage: f64,
    pub groups: Vec<SummaryGroup>,
}

fn coverage(labelled: usize, rows: usize) -> f64 {
    if rows == 0 {
        0.0
    } else {
        labelled as f64 / rows as f64
    }
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    })
}

/// Summarize reported rows per bot (or skill) and tool, in that order.
pub fn summarize(since: Option<DateTime<Utc>>, rows: &[LedgerRow]) -> Summary {
    type Key = (Option<String>, Option<String>, String);
    let mut groups: BTreeMap<Key, Vec<&LedgerRow>> = BTreeMap::new();
    for row in rows {
        groups
            .entry((row.bot.clone(), row.skill.clone(), row.tool.clone()))
            .or_default()
            .push(row);
    }
    let groups = groups
        .into_iter()
        .map(|((bot, skill, tool), rows)| {
            let mut counts: BTreeMap<&'static str, usize> =
                LedgerOutcome::ALL.iter().map(|o| (o.as_str(), 0)).collect();
            let mut reasons: BTreeMap<&'static str, usize> = BTreeMap::new();
            for row in &rows {
                *counts.entry(row.outcome.as_str()).or_default() += 1;
                if row.outcome == LedgerOutcome::Rejected {
                    let key = row.reason.map(Reason::as_str).unwrap_or("none");
                    *reasons.entry(key).or_default() += 1;
                }
            }
            let labelled = rows.iter().filter(|r| r.outcome.is_labelled()).count();
            SummaryGroup {
                bot,
                skill,
                tool,
                rows: rows.len(),
                counts,
                label_coverage: coverage(labelled, rows.len()),
                median_edit_distance: median(rows.iter().filter_map(|r| r.edit_distance).collect()),
                reasons,
            }
        })
        .collect();
    let labelled = rows.iter().filter(|r| r.outcome.is_labelled()).count();
    Summary {
        since,
        rows: rows.len(),
        label_coverage: coverage(labelled, rows.len()),
        groups,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn distances() {
        assert_eq!(edit_distance("", ""), 0.0);
        assert_eq!(edit_distance("same", "same"), 0.0);
        assert_eq!(edit_distance("abc", ""), 1.0);
        assert_eq!(edit_distance("", "abc"), 1.0);
        assert_eq!(edit_distance("abc", "xyz"), 1.0);
        // kitten → sitting: 3 edits over 7 characters.
        assert!((edit_distance("kitten", "sitting") - 3.0 / 7.0).abs() < 1e-9);
        assert!((edit_distance("flaw", "lawn") - 0.5).abs() < 1e-9);
        // Characters, not bytes.
        assert!((edit_distance("café", "cafe") - 0.25).abs() < 1e-9);
        // Symmetric.
        assert_eq!(
            edit_distance("Fix the login bug", "Fix login bugs"),
            edit_distance("Fix login bugs", "Fix the login bug")
        );
    }

    #[test]
    fn long_texts_are_capped_and_stay_cheap() {
        let a = "x".repeat(EDIT_TEXT_CAP + 5_000);
        let mut b = a.clone();
        b.insert(10, 'y');
        let d = edit_distance(&a, &b);
        assert!(d > 0.0 && d < 0.001, "{d}");
        // Past the cap, differences don't count.
        let c = format!("{}{}", "x".repeat(EDIT_TEXT_CAP), "tail");
        assert_eq!(edit_distance(&"x".repeat(EDIT_TEXT_CAP), &c), 0.0);
        // The cap is 10,000 characters of each text, and the result stays from 0 to 1.
        let ten_k = "a".repeat(10_000);
        assert_eq!(
            edit_distance(&ten_k, &format!("{ten_k}{}", "b".repeat(50_000))),
            0.0
        );
        assert_eq!(edit_distance(&ten_k, &"b".repeat(60_000)), 1.0);
    }

    #[test]
    fn draft_text_is_string_arguments_in_key_order() {
        let args = json!({
            "summary": "Login fails",
            "project": "PMOS",
            "priority": 2,
            "fields": {"b": "two", "a": "one"},
            "labels": ["x", "y"],
        });
        assert_eq!(draft_text(&args), "one\ntwo\nx\ny\nPMOS\nLogin fails");
        let reordered = json!({
            "project": "PMOS",
            "labels": ["x", "y"],
            "fields": {"a": "one", "b": "two"},
            "summary": "Login fails",
            "priority": 3,
        });
        assert_eq!(args_edit_distance(&args, &reordered), 0.0);
        // A reply's tagged message is an id, not draft text.
        let tagged =
            json!({"channel": "C1", "thread_ts": "1.1", "message_ts": "1.2", "text": "Yes."});
        assert_eq!(draft_text(&tagged), "C1\nYes.\n1.1");
    }

    #[test]
    fn names_round_trip() {
        for o in LedgerOutcome::ALL {
            assert_eq!(LedgerOutcome::parse(o.as_str()), Some(o));
            assert_eq!(serde_json::to_value(o).unwrap(), json!(o.as_str()));
        }
        for r in Reason::ALL {
            assert_eq!(Reason::parse(r.as_str()), Some(r));
            assert_eq!(serde_json::to_value(r).unwrap(), json!(r.as_str()));
        }
        assert_eq!(Reason::parse("rude"), None);
        assert_eq!(LedgerOutcome::parse("executing"), None);
    }

    fn row(id: &str, outcome: LedgerOutcome, distance: Option<f64>) -> LedgerRow {
        let at: DateTime<Utc> = "2026-11-14T10:00:00Z".parse().unwrap();
        LedgerRow {
            id: id.into(),
            run_id: "tsk_1".into(),
            user: "ana".into(),
            bot: Some("docs".into()),
            skill: None,
            tool: "jira.create_issue".into(),
            content_sha: None,
            model_alias: Some("pmos/default".into()),
            outcome,
            edit_distance: distance,
            reason: None,
            created_at: at,
            decided_at: (outcome != LedgerOutcome::Pending).then_some(at),
            decided_by: None,
        }
    }

    #[test]
    fn pending_rows_expire_when_read() {
        let r = row("a", LedgerOutcome::Pending, None);
        let created = r.created_at;
        assert_eq!(r.clone().reported(created).outcome, LedgerOutcome::Pending);
        let later = created + chrono::Duration::seconds(1);
        assert_eq!(r.clone().reported(later).outcome, LedgerOutcome::Expired);
        // Decided rows (a write still running) never expire.
        let mut running = r;
        running.decided_at = Some(created);
        assert_eq!(running.reported(later).outcome, LedgerOutcome::Pending);
    }

    #[test]
    fn min_edit_keeps_rows_without_a_distance() {
        let q = LedgerQuery {
            min_edit: Some(0.3),
            ..LedgerQuery::all(Utc::now())
        };
        assert!(q.matches(&row("r", LedgerOutcome::Rejected, None)));
        assert!(q.matches(&row("e", LedgerOutcome::Edited, Some(0.3))));
        assert!(!q.matches(&row("e", LedgerOutcome::Edited, Some(0.29))));
        assert!(!q.matches(&row("a", LedgerOutcome::Approved, Some(0.0))));
    }

    #[test]
    fn summaries_count_coverage_medians_and_reasons() {
        let mut rejected = row("r1", LedgerOutcome::Rejected, None);
        rejected.reason = Some(Reason::Tone);
        let rows = vec![
            row("a1", LedgerOutcome::Approved, Some(0.0)),
            row("e1", LedgerOutcome::Edited, Some(0.2)),
            row("e2", LedgerOutcome::Edited, Some(0.6)),
            rejected,
            row("r2", LedgerOutcome::Rejected, None),
            row("p1", LedgerOutcome::Pending, None),
            row("x1", LedgerOutcome::Expired, None),
            row("f1", LedgerOutcome::Failed, Some(0.0)),
        ];
        let s = summarize(None, &rows);
        assert_eq!(s.rows, 8);
        assert!((s.label_coverage - 7.0 / 8.0).abs() < 1e-9);
        assert_eq!(s.groups.len(), 1);
        let g = &s.groups[0];
        assert_eq!(g.counts["edited"], 2);
        assert_eq!(g.counts["expired"], 1);
        assert_eq!(g.counts["pending"], 1);
        // 0.0, 0.0, 0.2, 0.6.
        assert_eq!(g.median_edit_distance, Some(0.1));
        assert_eq!(g.reasons["tone"], 1);
        assert_eq!(g.reasons["none"], 1);
        assert_eq!(summarize(None, &[]).label_coverage, 0.0);
    }
}
