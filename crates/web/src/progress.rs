//! A task's runtime events as the web app shows them (`docs/web-api.md`, Asking).
//!
//! The runtime's events (`GET /v1/tasks/{id}/events`, `docs/bot-runtime-spec.md` §3) are of
//! three kinds: `status` (bookkeeping such as "queued", "claimed by worker vm-16344",
//! "started on vm-16344"), `checklist` ("2 of 3 done") and `milestone` (the human-readable
//! steps: "Reading Jira", "Drafting in Confluence"). The app gets:
//!
//! - `events`: the milestones only, in the runtime's order, each text once (its first
//!   time). Status events never pass: they name workers and hosts.
//! - `progress`: `{done, total}` from the task's latest checklist, else from the latest
//!   `checklist` event's "N of M done"; `null` when there is none.
//! - `retrying`: the task is still queued or working after the runtime retried it: a worker
//!   claimed it more than once (`attempts`), or the runtime's retry status event is there.
//!   Nothing else signals a retry today: the gateway's own retries of a model call
//!   (`crates/gateway/src/retry.rs`) and the harness's retries are not reported to the
//!   runtime, so they can't be shown.

use serde::Serialize;
use serde_json::Value;

use crate::upstream::{Task, TaskEvent};

/// The runtime's status event when it puts an interrupted task back in the queue
/// (`RETRY_EVENT` in `crates/bots/src/model.rs`).
pub const RETRY_EVENT: &str = "interrupted by a restart; retrying";

/// The longest milestone text shown, in characters.
pub const MAX_MILESTONE: usize = 200;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EventOut {
    pub at: String,
    pub kind: &'static str,
    pub text: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
}

/// The milestones, in order, each text once; control characters removed, at most
/// [`MAX_MILESTONE`] characters.
pub fn milestones(events: &[TaskEvent]) -> Vec<EventOut> {
    let mut seen = std::collections::HashSet::new();
    events
        .iter()
        .filter(|e| e.kind == "milestone")
        .filter_map(|e| {
            let text: String = e
                .text
                .as_deref()?
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(MAX_MILESTONE)
                .collect();
            (!text.is_empty() && seen.insert(text.clone())).then(|| EventOut {
                at: e.at.clone(),
                kind: "milestone",
                text,
            })
        })
        .collect()
}

/// `{done, total}` of a checklist (`[{item, done}]`), or `None` when it has no items.
fn of_checklist(checklist: &Value) -> Option<Progress> {
    let items = checklist.as_array().filter(|a| !a.is_empty())?;
    let done = items
        .iter()
        .filter(|i| i.get("done").and_then(Value::as_bool) == Some(true))
        .count();
    Some(Progress {
        done,
        total: items.len(),
    })
}

/// "N of M done", as the runtime writes a checklist event without its own text.
fn of_text(text: &str) -> Option<Progress> {
    let rest = text.trim().strip_suffix(" done")?;
    let (done, total) = rest.split_once(" of ")?;
    let (done, total) = (done.trim().parse().ok()?, total.trim().parse().ok()?);
    (total > 0 && done <= total).then_some(Progress { done, total })
}

/// The task's progress: its own latest checklist, else the latest checklist event.
pub fn progress(task: &Task, events: &[TaskEvent]) -> Option<Progress> {
    of_checklist(&task.checklist).or_else(|| {
        events
            .iter()
            .rev()
            .find(|e| e.kind == "checklist")
            .and_then(|e| of_text(e.text.as_deref()?))
    })
}

/// Is the task still running after the runtime retried it?
pub fn retrying(task: &Task, events: &[TaskEvent]) -> bool {
    matches!(task.status.as_str(), "queued" | "working")
        && (task.attempts > 1
            || events
                .iter()
                .any(|e| e.kind == "status" && e.text.as_deref() == Some(RETRY_EVENT)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(kind: &str, text: &str) -> TaskEvent {
        TaskEvent {
            at: "2026-11-16T10:00:00Z".into(),
            kind: kind.into(),
            text: Some(text.into()),
        }
    }

    fn task(status: &str, attempts: u32, checklist: Value) -> Task {
        serde_json::from_value(json!({
            "id": "tsk_1", "bot": "pm-ana", "requested_by": "ana", "status": status,
            "attempts": attempts, "checklist": checklist,
        }))
        .unwrap()
    }

    #[test]
    fn only_milestones_once_each_in_order() {
        let events = vec![
            ev("status", "queued"),
            ev("status", "claimed by worker vm-16344"),
            ev("status", "started on vm-16344"),
            ev("milestone", "Working on it"),
            ev("checklist", "0 of 1 done"),
            ev("milestone", "Reading Jira"),
            ev("milestone", "Reading Jira"),
            ev("milestone", "  "),
            ev("milestone", "Drafting\nin Confluence"),
            ev("checklist", "2 of 3 done"),
            ev("milestone", "Working on it"),
        ];
        let texts: Vec<String> = milestones(&events).into_iter().map(|e| e.text).collect();
        assert_eq!(
            texts,
            ["Working on it", "Reading Jira", "Drafting in Confluence"]
        );
        let shown = serde_json::to_string(&milestones(&events)).unwrap();
        assert!(!shown.contains("vm-16344") && !shown.contains("of 3 done"));
        let long = ev("milestone", &"x".repeat(500));
        assert_eq!(milestones(&[long])[0].text.chars().count(), MAX_MILESTONE);
    }

    #[test]
    fn progress_comes_from_the_latest_checklist() {
        let events = vec![
            ev("checklist", "0 of 1 done"),
            ev("checklist", "2 of 3 done"),
        ];
        let t = task(
            "working",
            1,
            json!([{"item": "a", "done": true}, {"item": "b", "done": false}]),
        );
        assert_eq!(
            progress(&t, &events),
            Some(Progress { done: 1, total: 2 }),
            "the task's own checklist first"
        );
        let t = task("working", 1, Value::Null);
        assert_eq!(progress(&t, &events), Some(Progress { done: 2, total: 3 }));
        assert_eq!(progress(&t, &[]), None);
        assert_eq!(progress(&task("working", 1, json!([])), &[]), None);
        assert_eq!(
            progress(&t, &[ev("checklist", "Searched the docs")]),
            None,
            "a checklist event with its own wording says no numbers"
        );
        assert_eq!(progress(&t, &[ev("checklist", "4 of 3 done")]), None);
    }

    #[test]
    fn retrying_only_while_running_after_a_retry() {
        let retry = vec![ev("status", RETRY_EVENT)];
        assert!(retrying(&task("queued", 1, Value::Null), &retry));
        assert!(retrying(&task("working", 2, Value::Null), &[]));
        assert!(!retrying(&task("working", 1, Value::Null), &[]));
        assert!(!retrying(&task("done", 2, Value::Null), &retry));
        assert!(!retrying(&task("failed", 2, Value::Null), &retry));
    }
}
