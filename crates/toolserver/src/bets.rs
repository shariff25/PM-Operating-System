//! The bets register (`docs/staff-principal-spec.md` §5): one note per bet in the PM's own
//! memory, key `bet-<key>`, so a bet lives wherever their memory does (their private GitHub
//! repo when memory is on GitHub).
//!
//! The tool server owns the note's format: it parses and renders it, and the model never
//! writes the whole note. `bets.record` sends only what changes; on approval the server
//! merges it into the current note ([`merge`]) and saves it. The note is plain markdown a PM
//! can read and edit by hand:
//!
//! ```markdown
//! # Bet: SSO wins the enterprise tier
//!
//! - Key: sso-enterprise
//! - Status: open
//! - Bet: SAML SSO in the console moves enterprise trials to paid.
//! - Metric: Enterprise trial-to-paid conversion
//! - Riskiest assumption: SSO is the blocker, not price
//! - Test: Offer SSO to 20 trials in Q1 and compare conversion
//! - Opened: 2026-11-15
//! - Review by: 2027-02-01
//!
//! ## Evidence
//!
//! - 2026-11-10: Four of six lost deals named SSO (source: CRM export, October)
//!
//! ## Decisions
//!
//! - 2026-11-15: Fund the SSO epic for Q1 (source: PM-101)
//! ```
//!
//! Parsing is tolerant: labels match whatever their case, bold or bullet, and any line it
//! doesn't recognise is kept (rendered again under `## Other notes`) and reported as
//! `unparsed`, never dropped and never a panic.

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{json, Value};

use crate::schemas::{is_bet_key, is_date};

/// A bet's statuses; a new bet starts `open`.
pub const STATUSES: [&str; 4] = ["open", "confirmed", "refuted", "dropped"];
/// Memory keys of bets start with this.
pub const NOTE_PREFIX: &str = "bet-";
/// The most characters in one field or one entry's text.
pub const MAX_FIELD_CHARS: u64 = 500;
/// The most evidence entries one `bets.record` adds.
pub const MAX_EVIDENCE_PER_CALL: u64 = 5;
/// The memory limit for one note, in characters: past it, the oldest evidence is dropped.
pub const MAX_NOTE_CHARS: usize = 4000;

const OTHER_NOTES: &str = "## Other notes";

/// One dated entry in a bet's evidence or decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub date: String,
    pub text: String,
    pub source: Option<String>,
}

impl Entry {
    fn json(&self) -> Value {
        let mut v = json!({"date": self.date, "text": self.text});
        if let Some(s) = &self.source {
            v["source"] = json!(s);
        }
        v
    }

    fn render(&self) -> String {
        match &self.source {
            Some(s) => format!("- {}: {} (source: {s})", self.date, self.text),
            None => format!("- {}: {}", self.date, self.text),
        }
    }

    /// `- 2026-11-10: text (source: s)`, or `None` if the line isn't one.
    fn parse(line: &str) -> Option<Self> {
        let rest = bullet(line)?;
        let date = rest.get(..10).filter(|d| is_date(d))?;
        let text = rest[10..].strip_prefix(':')?.trim();
        let (text, source) = match text
            .strip_suffix(')')
            .and_then(|t| t.rsplit_once(" (source: "))
        {
            Some((t, s)) if !s.trim().is_empty() => (t.trim(), Some(s.trim().to_string())),
            _ => (text, None),
        };
        Some(Self {
            date: date.to_string(),
            text: text.to_string(),
            source,
        })
    }
}

/// One bet, as its note holds it. Text fields are empty when a hand-edited note lacks them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bet {
    pub key: String,
    pub title: String,
    pub status: String,
    pub bet: String,
    pub metric: String,
    pub riskiest_assumption: String,
    pub test: String,
    pub opened: String,
    pub review_by: String,
    pub evidence: Vec<Entry>,
    pub decisions: Vec<Entry>,
    /// How many of the oldest evidence entries were dropped to keep the note within
    /// [`MAX_NOTE_CHARS`].
    pub evidence_dropped: u64,
    /// Lines the parser didn't recognise, kept as they were.
    pub unparsed: Vec<String>,
}

/// The text after a list bullet (`- ` or `* `), or the line itself without one.
fn bullet(line: &str) -> Option<&str> {
    let t = line.trim();
    let t = t
        .strip_prefix("- ")
        .or_else(|| t.strip_prefix("* "))
        .unwrap_or(t);
    Some(t.trim()).filter(|t| !t.is_empty())
}

/// `- **Review by:** 2027-02-01` → (`review by`, `2027-02-01`).
fn labelled(line: &str) -> Option<(String, String)> {
    let t = bullet(line)?.replace("**", "");
    let (label, value) = t.split_once(':')?;
    let label = label.trim().to_lowercase().replace(['_', '-'], " ");
    if label.is_empty() || label.len() > 40 {
        return None;
    }
    Some((label, value.trim().to_string()))
}

/// Collapse a value onto one line, so it can't break the note's layout.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(PartialEq)]
enum Section {
    Fields,
    Evidence,
    Decisions,
    Other,
}

impl Bet {
    /// Parse the note for `key` (the bet's key, without `bet-`). Never fails: what it doesn't
    /// recognise goes into [`Bet::unparsed`].
    pub fn parse(key: &str, text: &str) -> Self {
        let mut bet = Bet {
            key: key.to_string(),
            ..Bet::default()
        };
        let mut section = Section::Fields;
        let mut seen_title = false;
        for raw in text.lines() {
            let line = raw.trim_end();
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if section != Section::Other {
                if let Some(h) = trimmed.strip_prefix("## ") {
                    match h.trim().to_lowercase().as_str() {
                        "evidence" => {
                            section = Section::Evidence;
                            continue;
                        }
                        "decisions" | "decision" => {
                            section = Section::Decisions;
                            continue;
                        }
                        _ => {
                            // Everything from an unknown section on is kept as it is.
                            section = Section::Other;
                            bet.unparsed.push(line.to_string());
                            continue;
                        }
                    }
                }
            }
            match section {
                Section::Other => bet.unparsed.push(line.to_string()),
                Section::Evidence | Section::Decisions => match Entry::parse(trimmed) {
                    Some(e) if section == Section::Evidence => bet.evidence.push(e),
                    Some(e) => bet.decisions.push(e),
                    None => bet.unparsed.push(line.to_string()),
                },
                Section::Fields => {
                    if let (false, Some(h)) = (seen_title, trimmed.strip_prefix("# ")) {
                        let h = h.trim();
                        bet.title = h
                            .strip_prefix("Bet:")
                            .map(str::trim)
                            .unwrap_or(h)
                            .to_string();
                        seen_title = true;
                        continue;
                    }
                    if !bet.set_field(trimmed) {
                        bet.unparsed.push(line.to_string());
                    }
                }
            }
        }
        bet
    }

    /// A `Label: value` line in the fields block. False if it isn't one.
    fn set_field(&mut self, line: &str) -> bool {
        let Some((label, value)) = labelled(line) else {
            return false;
        };
        match label.as_str() {
            // The note's key is the bet's key; this line is for people.
            "key" => {}
            "title" => self.title = value,
            "status" => self.status = value.to_lowercase(),
            "bet" => self.bet = value,
            "metric" => self.metric = value,
            "riskiest assumption" => self.riskiest_assumption = value,
            "test" => self.test = value,
            "opened" => self.opened = value,
            "review by" => self.review_by = value,
            "earlier evidence dropped" | "evidence dropped" => match value.parse() {
                Ok(n) => self.evidence_dropped = n,
                Err(_) => return false,
            },
            _ => return false,
        }
        true
    }

    /// The note's text.
    pub fn render(&self) -> String {
        let mut out = format!("# Bet: {}\n\n", self.title);
        let mut field = |label: &str, value: &str| {
            if !value.is_empty() {
                out.push_str(&format!("- {label}: {value}\n"));
            }
        };
        field("Key", &self.key);
        field("Status", &self.status);
        field("Bet", &self.bet);
        field("Metric", &self.metric);
        field("Riskiest assumption", &self.riskiest_assumption);
        field("Test", &self.test);
        field("Opened", &self.opened);
        field("Review by", &self.review_by);
        if self.evidence_dropped > 0 {
            field(
                "Earlier evidence dropped",
                &self.evidence_dropped.to_string(),
            );
        }
        for (heading, entries) in [("Evidence", &self.evidence), ("Decisions", &self.decisions)] {
            out.push_str(&format!("\n## {heading}\n\n"));
            for e in entries {
                out.push_str(&e.render());
                out.push('\n');
            }
        }
        if !self.unparsed.is_empty() {
            out.push('\n');
            if !self.unparsed[0].trim_start().starts_with("## ") {
                out.push_str(OTHER_NOTES);
                out.push_str("\n\n");
            }
            for line in &self.unparsed {
                out.push_str(line);
                out.push('\n');
            }
        }
        out
    }

    /// The bet as the tools return it. Empty fields are `null`.
    pub fn json(&self, updated_at: DateTime<Utc>) -> Value {
        let opt = |s: &str| if s.is_empty() { Value::Null } else { json!(s) };
        let mut v = json!({
            "key": self.key,
            "title": opt(&self.title),
            "status": opt(&self.status),
            "bet": opt(&self.bet),
            "metric": opt(&self.metric),
            "riskiest_assumption": opt(&self.riskiest_assumption),
            "test": opt(&self.test),
            "opened": opt(&self.opened),
            "review_by": opt(&self.review_by),
            "evidence": self.evidence.iter().map(Entry::json).collect::<Vec<_>>(),
            "decisions": self.decisions.iter().map(Entry::json).collect::<Vec<_>>(),
            "evidence_dropped": self.evidence_dropped,
            "updated_at": updated_at,
        });
        if !self.unparsed.is_empty() {
            v["unparsed"] = json!(self.unparsed);
        }
        v
    }

    /// The line `bets.list` shows.
    pub fn summary(&self, updated_at: DateTime<Utc>) -> Value {
        let opt = |s: &str| if s.is_empty() { Value::Null } else { json!(s) };
        json!({
            "key": self.key,
            "title": opt(&self.title),
            "status": opt(&self.status),
            "review_by": opt(&self.review_by),
            "updated_at": updated_at,
        })
    }
}

/// The memory key for a bet's key.
pub fn note_key(key: &str) -> String {
    format!("{NOTE_PREFIX}{key}")
}

/// The bet's key for a memory key, if it is a bet's.
pub fn bet_key(note_key: &str) -> Option<&str> {
    note_key.strip_prefix(NOTE_PREFIX).filter(|k| is_bet_key(k))
}

/// Soonest review first; bets with no valid review date last; then by key.
pub fn sort_for_review(bets: &mut [(Bet, DateTime<Utc>)]) {
    bets.sort_by(|(a, _), (b, _)| {
        let date = |b: &Bet| NaiveDate::parse_from_str(&b.review_by, "%Y-%m-%d").ok();
        match (date(a), date(b)) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
        .then_with(|| a.key.cmp(&b.key))
    });
}

fn entry(v: &Value, path: &str) -> Result<Entry, String> {
    let obj = v
        .as_object()
        .ok_or_else(|| format!("`{path}` must be an object: {{date, text, source?}}"))?;
    if let Some(extra) = obj
        .keys()
        .find(|k| !matches!(k.as_str(), "date" | "text" | "source"))
    {
        return Err(format!("unknown field `{path}.{extra}`"));
    }
    let date = v["date"]
        .as_str()
        .filter(|d| is_date(d))
        .ok_or_else(|| format!("`{path}.date` must be a date written YYYY-MM-DD"))?;
    let text = field(v, "text", path)?.ok_or_else(|| format!("`{path}.text` is required"))?;
    let source = field(v, "source", path)?;
    Ok(Entry {
        date: date.to_string(),
        text,
        source,
    })
}

/// An optional text field: one line, not empty, at most [`MAX_FIELD_CHARS`].
fn field(v: &Value, key: &str, path: &str) -> Result<Option<String>, String> {
    let name = if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    };
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            let s = one_line(s);
            if s.is_empty() {
                Err(format!(
                    "`{name}` is empty; leave it out to keep what the bet has"
                ))
            } else if s.chars().count() as u64 > MAX_FIELD_CHARS {
                Err(format!(
                    "`{name}` is longer than {MAX_FIELD_CHARS} characters"
                ))
            } else {
                Ok(Some(s))
            }
        }
        Some(_) => Err(format!("`{name}` must be a string")),
    }
}

/// Apply one `bets.record` call to the bet as it is now (`None`: no such bet yet), as of
/// `today`. Returns the bet to save and whether it is new. An `Err` is an invalid call, said
/// so the model or the PM can fix it.
///
/// - A new bet needs `title`, `bet`, `metric`, `riskiest_assumption` and `review_by`; it
///   starts `open`, and `opened` is today.
/// - On an existing bet the fields given replace the old ones; evidence and the decision
///   are appended.
/// - If the note would pass [`MAX_NOTE_CHARS`], the oldest evidence is dropped until it fits,
///   and the count is kept in the note.
pub fn merge(current: Option<Bet>, args: &Value, today: NaiveDate) -> Result<(Bet, bool), String> {
    let obj = args.as_object().ok_or("arguments must be an object")?;
    const ALLOWED: [&str; 10] = [
        "key",
        "title",
        "status",
        "bet",
        "metric",
        "riskiest_assumption",
        "test",
        "review_by",
        "evidence",
        "decision",
    ];
    if let Some(extra) = obj.keys().find(|k| !ALLOWED.contains(&k.as_str())) {
        return Err(format!("unknown argument `{extra}`"));
    }
    let key = args["key"]
        .as_str()
        .filter(|k| is_bet_key(k))
        .ok_or("`key` must be 1 to 60 lowercase letters, digits or hyphens")?;
    let status = match args.get("status") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_str()
                .filter(|s| STATUSES.contains(s))
                .ok_or_else(|| format!("`status` must be one of {}", STATUSES.join(", ")))?,
        ),
    };
    let review_by = match args.get("review_by") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_str()
                .filter(|d| is_date(d))
                .ok_or("`review_by` must be a date written YYYY-MM-DD")?
                .to_string(),
        ),
    };
    let title = field(args, "title", "")?;
    let bet_text = field(args, "bet", "")?;
    let metric = field(args, "metric", "")?;
    let riskiest = field(args, "riskiest_assumption", "")?;
    let test = field(args, "test", "")?;
    let evidence = match args.get("evidence") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => {
            if items.len() as u64 > MAX_EVIDENCE_PER_CALL {
                return Err(format!(
                    "`evidence` has at most {MAX_EVIDENCE_PER_CALL} entries per call (got {})",
                    items.len()
                ));
            }
            items
                .iter()
                .enumerate()
                .map(|(i, e)| entry(e, &format!("evidence[{i}]")))
                .collect::<Result<_, _>>()?
        }
        Some(_) => return Err("`evidence` must be a list of {date, text, source?}".into()),
    };
    let decision = match args.get("decision") {
        None | Some(Value::Null) => None,
        Some(v) => Some(entry(v, "decision")?),
    };

    let created = current.is_none();
    let mut bet = match current {
        Some(bet) => {
            let changes = [
                title.is_some(),
                bet_text.is_some(),
                metric.is_some(),
                riskiest.is_some(),
                test.is_some(),
                status.is_some(),
                review_by.is_some(),
                !evidence.is_empty(),
                decision.is_some(),
            ];
            if !changes.contains(&true) {
                return Err(format!(
                    "nothing to record for bet `{key}`: give the fields that change, new evidence or a decision"
                ));
            }
            bet
        }
        None => {
            let missing: Vec<&str> = [
                ("title", title.is_none()),
                ("bet", bet_text.is_none()),
                ("metric", metric.is_none()),
                ("riskiest_assumption", riskiest.is_none()),
                ("review_by", review_by.is_none()),
            ]
            .into_iter()
            .filter(|(_, m)| *m)
            .map(|(f, _)| f)
            .collect();
            if !missing.is_empty() {
                return Err(format!(
                    "there is no bet `{key}` yet, and a new bet needs {}",
                    missing
                        .iter()
                        .map(|f| format!("`{f}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if status.is_some_and(|s| s != "open") {
                return Err(format!(
                    "a new bet starts `open`; record it first, then change its status with the evidence that supports it (got `{}`)",
                    status.unwrap_or_default()
                ));
            }
            Bet {
                key: key.to_string(),
                status: "open".into(),
                opened: today.format("%Y-%m-%d").to_string(),
                ..Bet::default()
            }
        }
    };
    bet.key = key.to_string();
    for (slot, value) in [
        (&mut bet.title, title),
        (&mut bet.bet, bet_text),
        (&mut bet.metric, metric),
        (&mut bet.riskiest_assumption, riskiest),
        (&mut bet.test, test),
        (&mut bet.review_by, review_by),
        (&mut bet.status, status.map(String::from)),
    ] {
        if let Some(v) = value {
            *slot = v;
        }
    }
    bet.evidence.extend(evidence);
    bet.decisions.extend(decision);
    while bet.render().chars().count() > MAX_NOTE_CHARS {
        if bet.evidence.is_empty() {
            return Err(format!(
                "bet `{key}` would be longer than the memory limit of {MAX_NOTE_CHARS} characters even with no evidence; shorten its fields, or ask the PM to tidy the note by hand"
            ));
        }
        bet.evidence.remove(0);
        bet.evidence_dropped += 1;
    }
    Ok((bet, created))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 15).unwrap()
    }

    fn new_args() -> Value {
        json!({
            "key": "sso-enterprise",
            "title": "SSO wins the enterprise tier",
            "bet": "SAML SSO in the console moves enterprise trials to paid.",
            "metric": "Enterprise trial-to-paid conversion",
            "riskiest_assumption": "SSO is the blocker, not price",
            "test": "Offer SSO to 20 trials in Q1 and compare conversion",
            "review_by": "2027-02-01",
            "evidence": [{"date": "2026-11-10", "text": "Four of six lost deals named SSO", "source": "CRM export, October"}],
            "decision": {"date": "2026-11-15", "text": "Fund the SSO epic for Q1", "source": "PM-101"}
        })
    }

    #[test]
    fn a_new_bet_renders_and_round_trips() {
        let (bet, created) = merge(None, &new_args(), today()).unwrap();
        assert!(created);
        assert_eq!(bet.status, "open");
        assert_eq!(bet.opened, "2026-11-15");
        let text = bet.render();
        assert_eq!(
            text,
            "# Bet: SSO wins the enterprise tier\n\n- Key: sso-enterprise\n- Status: open\n- Bet: SAML SSO in the console moves enterprise trials to paid.\n- Metric: Enterprise trial-to-paid conversion\n- Riskiest assumption: SSO is the blocker, not price\n- Test: Offer SSO to 20 trials in Q1 and compare conversion\n- Opened: 2026-11-15\n- Review by: 2027-02-01\n\n## Evidence\n\n- 2026-11-10: Four of six lost deals named SSO (source: CRM export, October)\n\n## Decisions\n\n- 2026-11-15: Fund the SSO epic for Q1 (source: PM-101)\n"
        );
        let back = Bet::parse("sso-enterprise", &text);
        assert_eq!(back, bet);
        assert_eq!(back.render(), text);
    }

    #[test]
    fn a_new_bet_needs_its_fields_and_starts_open() {
        let mut args = new_args();
        args.as_object_mut().unwrap().remove("metric");
        args.as_object_mut().unwrap().remove("review_by");
        let err = merge(None, &args, today()).unwrap_err();
        assert!(err.contains("`metric`, `review_by`"), "{err}");
        let mut args = new_args();
        args["status"] = json!("confirmed");
        assert!(merge(None, &args, today())
            .unwrap_err()
            .contains("starts `open`"));
        args["status"] = json!("open");
        assert!(merge(None, &args, today()).is_ok());
    }

    #[test]
    fn an_update_replaces_fields_and_appends() {
        let (bet, _) = merge(None, &new_args(), today()).unwrap();
        let later = NaiveDate::from_ymd_opt(2027, 2, 1).unwrap();
        let (bet, created) = merge(
            Some(bet),
            &json!({
                "key": "sso-enterprise", "status": "confirmed", "review_by": "2027-05-01",
                "evidence": [{"date": "2027-01-30", "text": "Trials with SSO converted at 31% against 18%", "source": "CONF-77"}],
            }),
            later,
        )
        .unwrap();
        assert!(!created);
        assert_eq!(bet.status, "confirmed");
        assert_eq!(bet.opened, "2026-11-15", "opened doesn't move");
        assert_eq!(bet.review_by, "2027-05-01");
        assert_eq!(bet.metric, "Enterprise trial-to-paid conversion", "kept");
        assert_eq!(bet.evidence.len(), 2);
        assert_eq!(
            bet.evidence[0].date, "2026-11-10",
            "earlier evidence survives"
        );
        assert_eq!(bet.decisions.len(), 1);
        let err = merge(Some(bet), &json!({"key": "sso-enterprise"}), later).unwrap_err();
        assert!(err.contains("nothing to record"), "{err}");
    }

    #[test]
    fn a_full_note_drops_its_oldest_evidence() {
        let (mut bet, _) = merge(None, &new_args(), today()).unwrap();
        for round in 0..12 {
            let evidence: Vec<Value> = (0..5)
                .map(|i| json!({"date": "2026-12-01", "text": format!("round {round} item {i}: {}", "x".repeat(60)), "source": "CONF-1"}))
                .collect();
            bet = merge(
                Some(bet),
                &json!({"key": "sso-enterprise", "evidence": evidence}),
                today(),
            )
            .unwrap()
            .0;
            assert!(bet.render().chars().count() <= MAX_NOTE_CHARS);
        }
        assert!(bet.evidence_dropped > 0);
        assert_eq!(
            bet.evidence.last().unwrap().text.split(':').next(),
            Some("round 11 item 4")
        );
        assert!(
            !bet.evidence.iter().any(|e| e.text.contains("lost deals")),
            "the oldest went first"
        );
        let text = bet.render();
        assert!(text.contains(&format!(
            "- Earlier evidence dropped: {}\n",
            bet.evidence_dropped
        )));
        assert_eq!(Bet::parse("sso-enterprise", &text), bet);
    }

    #[test]
    fn bad_calls_are_refused() {
        let (bet, _) = merge(None, &new_args(), today()).unwrap();
        for args in [
            json!({"key": "Bad Key", "title": "x"}),
            json!({"key": "sso-enterprise", "status": "won"}),
            json!({"key": "sso-enterprise", "review_by": "2027-02-30"}),
            json!({"key": "sso-enterprise", "title": "   "}),
            json!({"key": "sso-enterprise", "title": "x".repeat(501)}),
            json!({"key": "sso-enterprise", "evidence": [{"date": "soon", "text": "x"}]}),
            json!({"key": "sso-enterprise", "evidence": [{"date": "2026-11-01"}]}),
            json!({"key": "sso-enterprise", "evidence": vec![json!({"date": "2026-11-01", "text": "x"}); 6]}),
            json!({"key": "sso-enterprise", "decision": {"date": "2026-11-01", "text": "x", "by": "me"}}),
            json!({"key": "sso-enterprise", "note": "the whole note"}),
        ] {
            assert!(merge(Some(bet.clone()), &args, today()).is_err(), "{args}");
        }
    }

    #[test]
    fn a_hand_edited_note_parses_and_keeps_what_it_doesnt_know() {
        let text = "# SSO wins the enterprise tier\n\n* **Status:** Open\n- **Review by:** 2027-02-01\n- Metric: conversion\n- Owner: Ana\nA paragraph Ana wrote.\n\n## Evidence\n\n- 2026-11-10: Lost deals named SSO (source: CRM)\n- a note without a date\n- 2026-11-12: No source here\n\n## Context\n\nWhy we care.\n- 2026-11-01: not evidence\n";
        let bet = Bet::parse("sso-enterprise", text);
        assert_eq!(bet.title, "SSO wins the enterprise tier");
        assert_eq!(bet.status, "open");
        assert_eq!(bet.review_by, "2027-02-01");
        assert_eq!(bet.metric, "conversion");
        assert_eq!(bet.evidence.len(), 2);
        assert_eq!(bet.evidence[1].source, None);
        assert_eq!(
            bet.unparsed,
            vec![
                "- Owner: Ana",
                "A paragraph Ana wrote.",
                "- a note without a date",
                "## Context",
                "Why we care.",
                "- 2026-11-01: not evidence",
            ]
        );
        // Rendered again, nothing is lost, and it is stable from then on.
        let once = bet.render();
        for line in &bet.unparsed {
            assert!(once.contains(line.as_str()), "{line}");
        }
        let again = Bet::parse("sso-enterprise", &once);
        assert_eq!(
            again.render(),
            Bet::parse("sso-enterprise", &again.render()).render()
        );
        assert_eq!(again.evidence, bet.evidence);
        // Nothing at all, or rubbish, is not a panic.
        for junk in [
            "",
            "\n\n",
            "## Evidence",
            "- : :",
            "#",
            "# Bet:",
            "- Earlier evidence dropped: many",
            "é: ü",
        ] {
            let _ = Bet::parse("x", junk).render();
        }
    }

    #[test]
    fn ordering_for_review() {
        let t = Utc::now();
        let bet = |key: &str, review: &str| {
            (
                Bet {
                    key: key.into(),
                    review_by: review.into(),
                    ..Bet::default()
                },
                t,
            )
        };
        let mut bets = vec![
            bet("c", "2027-03-01"),
            bet("a", ""),
            bet("b", "2027-01-01"),
            bet("d", "2027-01-01"),
        ];
        sort_for_review(&mut bets);
        let keys: Vec<_> = bets.iter().map(|(b, _)| b.key.as_str()).collect();
        assert_eq!(keys, vec!["b", "d", "c", "a"]);
        assert_eq!(bet_key("bet-sso"), Some("sso"));
        assert_eq!(bet_key("areas"), None);
        assert_eq!(note_key("sso"), "bet-sso");
    }
}
