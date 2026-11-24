//! One usage event per request: who asked, which role and model, how long it queued and ran,
//! and the tokens used. Never prompts or answers. Under `local` every upstream is a model
//! server on PM OS's host; a remote upstream runs only as `hosted-test`, with fake data. Each
//! event names the upstream (its host, or for a labelled role its kind) so that can be checked.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::scheduler::Priority;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UsageEvent {
    pub at: DateTime<Utc>,
    pub client: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    pub endpoint: &'static str,
    pub role: String,
    pub model: String,
    /// For a labelled role only: the SHA-256 of the real model id (`config::Role::model_sha256`)
    /// beside its label in `model`, so an auditor can tell which model served a past call
    /// (by hashing the ids the config has held) without the log naming it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_sha256: Option<String>,
    pub upstream: String,
    pub priority: Priority,
    pub stream: bool,
    pub stub: bool,
    pub status: u16,
    /// Which upstream attempt this line describes (1 for the first).
    pub attempt: u32,
    /// Attempts made in all by this line; on the call's final line, the total.
    pub attempts: u32,
    /// True on a line for an attempt that failed and will be retried; the call's final line
    /// is the one without it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub retrying: bool,
    pub queue_ms: u64,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub workarounds: Vec<&'static str>,
    /// Set on a line for a call the gateway refused before it reached a model server: why
    /// (a client naming a person it may not name, `403`; a client or person with too many
    /// calls waiting, `429`). Such a line has `attempts: 0`; on a `403` line `user` is the
    /// person the client tried to name, not one it was allowed to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refused: Option<&'static str>,
    /// On a refusal summary line only ([`UsageLog::record_refusal`]): how many more refusals
    /// of this client, with this status, the line stands for since its last line, none of
    /// which got a line of its own. The rest of the line is the last of them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refused_count: Option<u64>,
    /// On a refusal summary line only: the distinct people the held refusals named, in the
    /// order first seen, at most [`REFUSAL_USERS`] of them (the Low on refusal summaries,
    /// review at 6bb806f: before, a summary kept only the last one).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub refused_users: Vec<String>,
    /// On a refusal summary line only, when some were left out of `refused_users`: how many
    /// of the held refusals named someone not in it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refused_users_more: Option<u64>,
    /// On a `403` line only: the person key of the handle in `user` (its spelled form,
    /// `config::person_key`), the form every other line's `user` has, so a refusal can be
    /// matched to that person's calls (`ana.k` is `ana-k`; the Low on 403 lines, review at
    /// c78b5fb). On a summary line, the last refusal's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_key: Option<String>,
    /// The 1-based place in the role's route of the model this line's attempt went to (slice
    /// MP3, `docs/model-providers-spec.md` §4.9): 1 for a one-hop role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hop: Option<u32>,
    /// True when that model isn't the role's primary.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub fallback: bool,
    /// On a routed role's failed attempt: why it failed (`connect`, `timeout`, `rate_limit`,
    /// `quota_exhausted`, `overloaded`, `server_error`, `model_unavailable`, `auth`,
    /// `redirect`, `bad_answer`, `first_token`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    /// On a routed call's final line: the models of its route it didn't try, and why.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<Skipped>,
    /// Under `hosted-test` only: the SHA-256 of the upstream's normalised URL, so two test
    /// upstreams can be told apart without naming either.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_sha256: Option<String>,
}

/// A model of a route that a call didn't try (§4.9): its label and why (`disabled`,
/// `circuit`, `feature`, `key`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Skipped {
    pub label: String,
    pub why: &'static str,
}

/// How long a client's refusals of one status are counted into one summary line, after the
/// first is recorded on its own (the Low on refusal lines, review at 2544d01).
pub const REFUSAL_WINDOW: Duration = Duration::from_secs(60);

/// The most distinct people one refusal summary line names.
pub const REFUSAL_USERS: usize = 8;

/// The refusals of one client with one status since its last line.
struct Window {
    opened: Instant,
    held: u64,
    last: Option<UsageEvent>,
    /// Distinct people the held refusals named, at most [`REFUSAL_USERS`].
    users: Vec<String>,
    /// Held refusals that named someone not in `users`, once it was full.
    more: u64,
}

impl Window {
    fn new(opened: Instant) -> Self {
        Self {
            opened,
            held: 0,
            last: None,
            users: Vec::new(),
            more: 0,
        }
    }

    /// Counts a refusal into the window, without a line of its own.
    fn hold(&mut self, event: UsageEvent) {
        self.held += 1;
        if let Some(u) = event.user.as_ref().filter(|u| !self.users.contains(u)) {
            if self.users.len() < REFUSAL_USERS {
                self.users.push(u.clone());
            } else {
                self.more += 1;
            }
        }
        self.last = Some(event);
    }

    /// The summary line for the refusals held, if any.
    fn summary(&mut self) -> Option<UsageEvent> {
        let mut last = self.last.take()?;
        last.refused_count = Some(self.held);
        last.refused_users = std::mem::take(&mut self.users);
        last.refused_users_more = (self.more > 0).then_some(self.more);
        self.held = 0;
        self.more = 0;
        Some(last)
    }
}

/// Recent events in memory, plus an optional JSON-lines file for shipping off-box.
pub struct UsageLog {
    file: Option<Mutex<File>>,
    recent: Mutex<Vec<UsageEvent>>,
    /// Refusals held for a summary line, by client and status.
    refusals: Mutex<HashMap<(String, u16), Window>>,
}

const KEEP: usize = 1000;

impl UsageLog {
    pub fn new(path: Option<&Path>) -> std::io::Result<Self> {
        let file = match path {
            Some(p) => Some(Mutex::new(
                OpenOptions::new().create(true).append(true).open(p)?,
            )),
            None => None,
        };
        Ok(Self {
            file,
            recent: Mutex::new(Vec::new()),
            refusals: Mutex::new(HashMap::new()),
        })
    }

    /// Records a call refused before any try (`403`, `429`) without letting a looping token
    /// flood the file, the service log or [`UsageLog::recent`]: a client's first refusal of a
    /// status is recorded at once; those that follow within [`REFUSAL_WINDOW`] are counted, and
    /// one summary line (the last of them, with `refused_count`) is written when the window
    /// ends: by [`UsageLog::flush_refusals`] (the router calls it every few seconds), by the
    /// next refusal after it, or at shutdown. So each client writes at most about two refusal
    /// lines of each status a minute, and every refusal is counted. `true`: this one got a
    /// line of its own.
    pub fn record_refusal(&self, event: UsageEvent) -> bool {
        self.refusal_at(event, Instant::now())
    }

    fn refusal_at(&self, event: UsageEvent, now: Instant) -> bool {
        let key = (event.client.clone(), event.status);
        let mut map = self.refusals.lock().expect("refusals lock");
        let summary = match map.get_mut(&key) {
            Some(w) if now.saturating_duration_since(w.opened) < REFUSAL_WINDOW => {
                w.hold(event);
                return false;
            }
            Some(w) => {
                let s = w.summary();
                w.opened = now;
                s
            }
            None => {
                map.insert(key, Window::new(now));
                None
            }
        };
        drop(map);
        if let Some(s) = summary {
            self.record(s);
        }
        self.record(event);
        true
    }

    /// Writes the summary line of every refusal window that has ended, and forgets it, so a
    /// client's next refusal is recorded at once again.
    pub fn flush_refusals(&self) {
        self.flush_at(Some(Instant::now()));
    }

    /// Writes every held refusal's summary line now, ended or not (at shutdown).
    pub fn flush_all_refusals(&self) {
        self.flush_at(None);
    }

    fn flush_at(&self, now: Option<Instant>) {
        let mut lines = Vec::new();
        self.refusals.lock().expect("refusals lock").retain(|_, w| {
            let ended = now.is_none_or(|n| n.saturating_duration_since(w.opened) >= REFUSAL_WINDOW);
            if ended {
                lines.extend(w.summary());
            }
            !ended
        });
        for line in lines {
            self.record(line);
        }
    }

    pub fn record(&self, event: UsageEvent) {
        tracing::info!(
            client = %event.client,
            user = event.user.as_deref().unwrap_or("-"),
            role = %event.role,
            status = event.status,
            attempt = event.attempt,
            retrying = event.retrying,
            queue_ms = event.queue_ms,
            latency_ms = event.latency_ms,
            "model call"
        );
        if let Some(file) = &self.file {
            let line = serde_json::to_string(&event).expect("usage event serializes");
            let mut f = file.lock().expect("usage log lock");
            if let Err(e) = writeln!(f, "{line}") {
                tracing::error!(error = %e, "usage log write failed");
            }
        }
        let mut recent = self.recent.lock().expect("usage lock");
        if recent.len() >= KEEP {
            recent.remove(0);
        }
        recent.push(event);
    }

    pub fn recent(&self) -> Vec<UsageEvent> {
        self.recent.lock().expect("usage lock").clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn refusal(client: &str, status: u16, user: &str) -> UsageEvent {
        UsageEvent {
            at: Utc::now(),
            client: client.into(),
            user: Some(user.into()),
            run: None,
            endpoint: "chat/completions",
            role: "default".into(),
            model: "m".into(),
            model_sha256: None,
            upstream: "vllm".into(),
            priority: Priority::Personal,
            stream: false,
            stub: false,
            status,
            attempt: 0,
            attempts: 0,
            retrying: false,
            queue_ms: 0,
            latency_ms: 0,
            prompt_tokens: None,
            completion_tokens: None,
            workarounds: Vec::new(),
            refused: Some("why"),
            refused_count: None,
            refused_users: Vec::new(),
            refused_users_more: None,
            user_key: None,
            hop: None,
            fallback: false,
            reason: None,
            skipped: Vec::new(),
            upstream_sha256: None,
        }
    }

    #[test]
    fn a_summary_names_up_to_eight_distinct_people_and_counts_the_rest() {
        let log = UsageLog::new(None).unwrap();
        let t0 = Instant::now();
        assert!(log.refusal_at(refusal("bot-docs", 429, "first"), t0));
        // 30 people, each refused three times, after the first line.
        for round in 0..3u64 {
            for p in 0..30u64 {
                let at = t0 + Duration::from_millis(round * 100 + p);
                assert!(!log.refusal_at(refusal("bot-docs", 429, &format!("p{p}")), at));
            }
        }
        log.flush_at(Some(t0 + REFUSAL_WINDOW));
        let recent = log.recent();
        let summary = recent.last().unwrap();
        assert_eq!(summary.refused_count, Some(90));
        let want: Vec<String> = (0..REFUSAL_USERS).map(|p| format!("p{p}")).collect();
        assert_eq!(summary.refused_users, want, "distinct, first seen, capped");
        assert_eq!(
            summary.refused_users_more,
            Some(3 * (30 - REFUSAL_USERS as u64))
        );
        assert_eq!(
            summary.user.as_deref(),
            Some("p29"),
            "the line is the last refusal"
        );
        // A summary of refusals that named few people names each once, and nothing more.
        let t1 = t0 + Duration::from_secs(300);
        assert!(log.refusal_at(refusal("evals", 403, "a"), t1));
        for p in ["b", "a", "b", "c"] {
            assert!(!log.refusal_at(refusal("evals", 403, p), t1));
        }
        log.flush_all_refusals();
        let summary = log.recent().pop().unwrap();
        assert_eq!(summary.refused_users, ["b", "a", "c"]);
        assert_eq!(summary.refused_users_more, None);
        // A line of its own carries neither field.
        let text = serde_json::to_string(&log.recent()[0]).unwrap();
        assert!(!text.contains("refused_users"), "{text}");
    }

    #[test]
    fn a_looping_token_gets_its_first_refusal_and_then_a_count_a_minute() {
        let dir = std::env::temp_dir().join(format!("pmos-usage-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("usage.jsonl");
        let _ = std::fs::remove_file(&path);
        let log = UsageLog::new(Some(&path)).unwrap();
        let t0 = Instant::now();
        // A token refused 10 000 times in its first minute: one line, then nothing more.
        assert!(log.refusal_at(refusal("bot-pm-ana", 403, "bo"), t0));
        for i in 1..10_000u64 {
            let at = t0 + Duration::from_millis(i * 5);
            assert!(!log.refusal_at(refusal("bot-pm-ana", 403, "cy"), at));
        }
        // Another client, and another status of the same client, keep their own first line.
        assert!(log.refusal_at(refusal("bot-docs", 429, "ana"), t0));
        assert!(log.refusal_at(refusal("bot-pm-ana", 429, "ana"), t0));
        assert_eq!(log.recent().len(), 3);
        // Before the minute is up, nothing is flushed.
        log.flush_at(Some(t0 + Duration::from_secs(59)));
        assert_eq!(log.recent().len(), 3);
        // After it, one summary line counts the 9 999 held, and names the last person tried.
        log.flush_at(Some(t0 + REFUSAL_WINDOW));
        let recent = log.recent();
        assert_eq!(recent.len(), 4, "{recent:?}");
        let summary = &recent[3];
        assert_eq!(summary.client, "bot-pm-ana");
        assert_eq!(summary.status, 403);
        assert_eq!(summary.refused_count, Some(9_999));
        assert_eq!(summary.user.as_deref(), Some("cy"));
        // The first refusal is still there, as it was.
        assert_eq!(recent[0].user.as_deref(), Some("bo"));
        assert_eq!(recent[0].refused_count, None);
        // After a quiet window, the next refusal is recorded at once again.
        assert!(log.refusal_at(
            refusal("bot-pm-ana", 403, "di"),
            t0 + REFUSAL_WINDOW + Duration::from_secs(1)
        ));
        // A refusal after a window ended unflushed writes that window's summary first.
        let t1 = t0 + Duration::from_secs(200);
        assert!(log.refusal_at(refusal("evals", 429, "x"), t1));
        assert!(!log.refusal_at(refusal("evals", 429, "x"), t1));
        assert!(log.refusal_at(refusal("evals", 429, "x"), t1 + REFUSAL_WINDOW));
        let recent = log.recent();
        let evals: Vec<_> = recent.iter().filter(|e| e.client == "evals").collect();
        assert_eq!(
            evals.iter().map(|e| e.refused_count).collect::<Vec<_>>(),
            vec![None, Some(1), None]
        );
        // At shutdown every held count is written.
        assert!(!log.refusal_at(refusal("evals", 429, "x"), t1 + REFUSAL_WINDOW));
        log.flush_all_refusals();
        assert_eq!(log.recent().last().unwrap().refused_count, Some(1));
        // The file holds the same lines as the recent list, no more.
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), log.recent().len());
        assert!(text.contains("\"refused_count\":9999"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
