//! Volume (`docs/openshell-prover-and-sandbox-audit-spec.md` §B.4): every deny is recorded, one
//! row each up to 100 per sandbox per minute; past that they are held and counted, and one
//! `deny_summary` carries the count and the distinct rules when the minute ends, so a looping
//! sandbox can't flood the log and no deny goes uncounted. Allows are counted: one
//! `allow_summary` per sandbox, rule, destination host and port, and method, per 5-minute
//! window, with no path. Records OpenShell delivers twice (its export is at least once) are
//! skipped by `metadata.uid` while it is remembered (the newest 100,000); the tool server also
//! deduplicates denies. Unparsed records are counted per flush with their OpenShell version, and
//! a version the format wasn't verified on is reported once.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use chrono::{DateTime, Utc};

use crate::ocsf::{Decision, Kept, Record};
use crate::wire::{Kind, Note, WireEvent};

pub const DENY_ROWS_PER_MINUTE: u32 = 100;
pub const ALLOW_WINDOW_SECS: i64 = 300;
pub const SUMMARY_RULES: usize = 20;
pub const REMEMBERED_UIDS: usize = 100_000;

#[derive(Debug, Clone, Default)]
struct DenyMinute {
    minute: i64,
    rows: u32,
    held: u64,
    rules: BTreeSet<String>,
    sandbox: Option<String>,
    sandbox_uid: Option<String>,
    last_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct AllowKey {
    sandbox: Option<String>,
    sandbox_uid: Option<String>,
    rule: Option<String>,
    host: Option<String>,
    port: Option<u32>,
    method: Option<String>,
}

#[derive(Debug, Clone)]
struct AllowWindow {
    start: i64,
    count: u64,
    last_at: Option<DateTime<Utc>>,
    version: Option<String>,
}

/// The collector's counting state. Cloned before a request is taken in, and put back if the
/// request can't be spooled, so a record OpenShell sends again isn't lost or counted twice.
#[derive(Debug, Clone, Default)]
pub struct Limiter {
    denies: BTreeMap<String, DenyMinute>,
    allows: BTreeMap<AllowKey, AllowWindow>,
    uids: HashSet<String>,
    uid_order: VecDeque<String>,
    unparsed: BTreeMap<Option<String>, u64>,
    verified: Option<String>,
    reported_versions: BTreeSet<String>,
}

fn minute_of(t: DateTime<Utc>) -> i64 {
    t.timestamp().div_euclid(60)
}

fn window_of(t: DateTime<Utc>) -> i64 {
    t.timestamp().div_euclid(ALLOW_WINDOW_SECS) * ALLOW_WINDOW_SECS
}

fn sandbox_key(k: &Kept) -> String {
    k.sandbox
        .clone()
        .or_else(|| k.sandbox_uid.clone())
        .unwrap_or_else(|| "?".into())
}

fn event(kind: Kind, k: &Kept, now: DateTime<Utc>) -> WireEvent {
    WireEvent {
        seq: 0,
        kind: Some(kind),
        at: Some(k.at.unwrap_or(now).min(now)),
        sandbox: k.sandbox.clone(),
        sandbox_uid: k.sandbox_uid.clone(),
        rule: k.rule.clone(),
        reason: k.reason.map(String::from),
        dest_host: k.host.clone(),
        dest_ip: k.ip.clone(),
        dest_port: k.port,
        http_method: k.method.clone(),
        http_path: k.path.clone(),
        process: k.process.clone(),
        os_event_uid: k.uid.clone(),
        os_correlation_uid: k.correlation_uid.clone(),
        os_version: k.version.clone(),
        policy_hash: k.policy_hash.clone(),
        ..WireEvent::default()
    }
}

impl Limiter {
    /// `verified`: the OpenShell version the format was last verified on
    /// (`OPENSHELL_FORMAT_VERIFIED` in `deploy/openshell-prover.lock`); `None` is none yet.
    pub fn new(verified: Option<String>) -> Self {
        Self {
            verified: verified.filter(|v| !v.is_empty()),
            ..Self::default()
        }
    }

    fn seen(&mut self, uid: &Option<String>) -> bool {
        let Some(uid) = uid else {
            return false;
        };
        if self.uids.contains(uid) {
            return true;
        }
        self.uids.insert(uid.clone());
        self.uid_order.push_back(uid.clone());
        while self.uid_order.len() > REMEMBERED_UIDS {
            if let Some(old) = self.uid_order.pop_front() {
                self.uids.remove(&old);
            }
        }
        false
    }

    fn version_note(
        &mut self,
        version: &Option<String>,
        now: DateTime<Utc>,
        out: &mut Vec<WireEvent>,
    ) {
        let Some(v) = version else {
            return;
        };
        if self.verified.as_deref() == Some(v.as_str()) || self.reported_versions.contains(v) {
            return;
        }
        self.reported_versions.insert(v.clone());
        out.push(WireEvent {
            kind: Some(Kind::Unparsed),
            at: Some(now),
            os_version: Some(v.clone()),
            note: Some(Note::FormatUnverified),
            ..WireEvent::default()
        });
    }

    /// One classified record: what to send now (summaries come from [`Limiter::flush`]).
    pub fn take(&mut self, record: Record, now: DateTime<Utc>) -> Vec<WireEvent> {
        let mut out = Vec::new();
        match record {
            Record::Ignored => {}
            Record::Unparsed { version } => {
                *self.unparsed.entry(version).or_insert(0) += 1;
            }
            Record::Gap {
                dropped,
                since,
                sandbox_uid,
            } => out.push(WireEvent {
                kind: Some(Kind::SourceGap),
                at: Some(now),
                sandbox_uid,
                count: Some(dropped),
                since: since.map(|s| s.min(now)),
                ..WireEvent::default()
            }),
            Record::Decision(d, k) => {
                self.version_note(&k.version, now, &mut out);
                if self.seen(&k.uid) {
                    return out;
                }
                match d {
                    Decision::Deny => self.deny(&k, now, &mut out),
                    Decision::Allow => self.allow(&k, now, &mut out),
                    Decision::Finding => out.push(event(Kind::Finding, &k, now)),
                    Decision::PolicyLoaded => out.push(event(Kind::PolicyLoaded, &k, now)),
                }
            }
        }
        out
    }

    fn deny(&mut self, k: &Kept, now: DateTime<Utc>, out: &mut Vec<WireEvent>) {
        let key = sandbox_key(k);
        let minute = minute_of(now);
        let entry = self.denies.entry(key).or_default();
        if entry.minute != minute {
            if let Some(s) = summary_of(entry, now) {
                out.push(s);
            }
            *entry = DenyMinute {
                minute,
                sandbox: k.sandbox.clone(),
                sandbox_uid: k.sandbox_uid.clone(),
                ..DenyMinute::default()
            };
        }
        if entry.rows < DENY_ROWS_PER_MINUTE {
            entry.rows += 1;
            out.push(event(Kind::Deny, k, now));
        } else {
            entry.held += 1;
            entry.last_at = k.at.or(Some(now));
            if entry.rules.len() < SUMMARY_RULES {
                entry
                    .rules
                    .insert(k.rule.clone().unwrap_or_else(|| "-".into()));
            }
        }
    }

    fn allow(&mut self, k: &Kept, now: DateTime<Utc>, out: &mut Vec<WireEvent>) {
        let key = AllowKey {
            sandbox: k.sandbox.clone(),
            sandbox_uid: k.sandbox_uid.clone(),
            rule: k.rule.clone(),
            host: k.host.clone(),
            port: k.port,
            method: k.method.clone(),
        };
        let start = window_of(now);
        if let Some(w) = self.allows.get(&key) {
            if w.start != start {
                let w = self.allows.remove(&key).expect("present");
                out.push(allow_summary(&key, &w, now));
            }
        }
        let w = self.allows.entry(key).or_insert(AllowWindow {
            start,
            count: 0,
            last_at: None,
            version: k.version.clone(),
        });
        w.count += 1;
        w.last_at = k.at.or(Some(now));
    }

    /// End every minute and window that is over at `now` (or all of them with `all`, at
    /// shutdown), and the unparsed counts: what to send.
    pub fn flush(&mut self, now: DateTime<Utc>, all: bool) -> Vec<WireEvent> {
        let mut out = Vec::new();
        let minute = minute_of(now);
        let mut done = Vec::new();
        for (key, d) in self.denies.iter_mut() {
            if all || d.minute < minute {
                if let Some(s) = summary_of(d, now) {
                    out.push(s);
                }
                done.push(key.clone());
            }
        }
        for k in done {
            self.denies.remove(&k);
        }
        let start = window_of(now);
        let over: Vec<AllowKey> = self
            .allows
            .iter()
            .filter(|(_, w)| all || w.start < start)
            .map(|(k, _)| k.clone())
            .collect();
        for k in over {
            if let Some(w) = self.allows.remove(&k) {
                out.push(allow_summary(&k, &w, now));
            }
        }
        for (version, count) in std::mem::take(&mut self.unparsed) {
            out.push(WireEvent {
                kind: Some(Kind::Unparsed),
                at: Some(now),
                os_version: version,
                count: Some(count),
                ..WireEvent::default()
            });
        }
        out
    }
}

fn summary_of(d: &DenyMinute, now: DateTime<Utc>) -> Option<WireEvent> {
    (d.held > 0).then(|| WireEvent {
        kind: Some(Kind::DenySummary),
        at: Some(d.last_at.unwrap_or(now).min(now)),
        sandbox: d.sandbox.clone(),
        sandbox_uid: d.sandbox_uid.clone(),
        count: Some(d.held),
        rules: d.rules.iter().cloned().collect(),
        ..WireEvent::default()
    })
}

fn allow_summary(k: &AllowKey, w: &AllowWindow, now: DateTime<Utc>) -> WireEvent {
    WireEvent {
        kind: Some(Kind::AllowSummary),
        at: Some(w.last_at.unwrap_or(now).min(now)),
        sandbox: k.sandbox.clone(),
        sandbox_uid: k.sandbox_uid.clone(),
        rule: k.rule.clone(),
        dest_host: k.host.clone(),
        dest_port: k.port,
        http_method: k.method.clone(),
        count: Some(w.count),
        os_version: w.version.clone(),
        ..WireEvent::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn t(sec: i64) -> DateTime<Utc> {
        "2026-11-24T10:00:00Z".parse::<DateTime<Utc>>().unwrap() + Duration::seconds(sec)
    }

    fn deny(uid: usize) -> Record {
        Record::Decision(
            Decision::Deny,
            Box::new(Kept {
                uid: Some(format!("u{uid}")),
                sandbox: Some("pmos-docs".into()),
                rule: Some(if uid % 2 == 0 {
                    "-".into()
                } else {
                    "pmos_gateway".into()
                }),
                host: Some("httpbin.org".into()),
                port: Some(443),
                version: Some("0.1.9".into()),
                ..Kept::default()
            }),
        )
    }

    #[test]
    fn a_thousand_denies_in_a_minute_are_a_hundred_rows_and_one_summary() {
        let mut l = Limiter::new(Some("0.1.9".into()));
        let mut rows = 0;
        for i in 0..1000 {
            rows += l
                .take(deny(i), t(5))
                .iter()
                .filter(|e| e.kind == Some(Kind::Deny))
                .count();
        }
        assert_eq!(rows, 100);
        assert!(l.flush(t(30), false).is_empty(), "the minute isn't over");
        let out = l.flush(t(61), false);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, Some(Kind::DenySummary));
        assert_eq!(out[0].count, Some(900));
        assert_eq!(out[0].rules, ["-", "pmos_gateway"]);
        // The next minute starts again at 100 rows.
        assert_eq!(l.take(deny(5000), t(62))[0].kind, Some(Kind::Deny));
    }

    #[test]
    fn allows_are_summed_per_window_and_key_without_a_path() {
        let mut l = Limiter::new(Some("0.1.9".into()));
        let allow = |uid: usize, path: &str| {
            Record::Decision(
                Decision::Allow,
                Box::new(Kept {
                    uid: Some(format!("a{uid}")),
                    sandbox: Some("pmos-docs".into()),
                    rule: Some("pmos_gateway".into()),
                    host: Some("gateway.pmos.internal".into()),
                    port: Some(8740),
                    method: Some("POST".into()),
                    path: Some(path.into()),
                    ..Kept::default()
                }),
            )
        };
        for i in 0..50 {
            assert!(l.take(allow(i, "/v1/chat/completions"), t(10)).is_empty());
        }
        // Redelivered: skipped.
        assert!(l.take(allow(3, "/v1/chat/completions"), t(11)).is_empty());
        let out = l.flush(t(301), false);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, Some(Kind::AllowSummary));
        assert_eq!(out[0].count, Some(50));
        assert_eq!(out[0].http_path, None);
    }

    #[test]
    fn an_unverified_version_is_reported_once_and_unparsed_records_are_counted() {
        let mut l = Limiter::new(None);
        let first = l.take(deny(1), t(0));
        assert_eq!(first[0].note, Some(Note::FormatUnverified));
        assert_eq!(first[0].os_version.as_deref(), Some("0.1.9"));
        assert!(l.take(deny(2), t(0)).iter().all(|e| e.note.is_none()));
        for _ in 0..3 {
            l.take(
                Record::Unparsed {
                    version: Some("9.9".into()),
                },
                t(0),
            );
        }
        let out = l.flush(t(1), false);
        assert!(out
            .iter()
            .any(|e| e.kind == Some(Kind::Unparsed) && e.count == Some(3)));
    }

    #[test]
    fn duplicates_of_a_deny_are_one_row() {
        let mut l = Limiter::new(Some("0.1.9".into()));
        assert_eq!(l.take(deny(1), t(0)).len(), 1);
        assert!(l.take(deny(1), t(1)).is_empty());
    }
}
