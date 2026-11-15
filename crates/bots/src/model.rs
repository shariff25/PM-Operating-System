//! The runtime's records: tasks, their events and traces, notifications and routines. The
//! JSON shapes are the ones in `docs/bot-runtime-spec.md` §3; other services build against
//! them, so a field is only ever added, never renamed.

use chrono::{DateTime, SubsecRound, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How long a claim holds a task before the runtime assumes its worker is gone (D2).
pub const LEASE_SECONDS: i64 = 120;
/// A task is claimed at most this many times; a second interrupted run fails it.
pub const MAX_ATTEMPTS: i32 = 2;

pub const RETRY_EVENT: &str = "interrupted by a restart; retrying";
pub const GAVE_UP_REASON: &str = "interrupted twice by restarts; ask again";

/// Now, to the microsecond: Postgres keeps microseconds, so the memory store does too and
/// both stores answer the same.
pub fn now() -> DateTime<Utc> {
    Utc::now().trunc_subsecs(6)
}

/// `tsk_<32 hex>`.
pub fn new_task_id() -> String {
    format!("tsk_{}", uuid::Uuid::new_v4().simple())
}

/// `rtn_<32 hex>`.
pub fn new_routine_id() -> String {
    format!("rtn_{}", uuid::Uuid::new_v4().simple())
}

macro_rules! text_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident = $text:literal),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $($(#[$vmeta])* #[serde(rename = $text)] $variant),*
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),*];

            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $text),* }
            }

            pub fn parse(s: &str) -> Option<Self> {
                match s { $($text => Some($name::$variant),)* _ => None }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

text_enum!(
    /// Where a task came from.
    Source {
        Slack = "slack",
        Routine = "routine",
        Cli = "cli",
        Eval = "eval",
    }
);

text_enum!(
    /// The gateway queue a task's model calls join (`X-Pmos-Priority`).
    Priority {
        /// Someone is waiting: Slack and the CLI.
        Interactive = "interactive",
        /// Routines.
        Scheduled = "scheduled",
        /// Evals and background work.
        Personal = "personal",
    }
);

impl Priority {
    /// Claim order: interactive work first.
    pub fn rank(self) -> i16 {
        match self {
            Self::Interactive => 0,
            Self::Scheduled => 1,
            Self::Personal => 2,
        }
    }
}

impl Source {
    /// Each source has one priority (spec §3, Task).
    pub fn priority(self) -> Priority {
        match self {
            Self::Slack | Self::Cli => Priority::Interactive,
            Self::Routine => Priority::Scheduled,
            Self::Eval => Priority::Personal,
        }
    }
}

text_enum!(
    TaskStatus {
        Queued = "queued",
        Working = "working",
        NeedsApproval = "needs_approval",
        Done = "done",
        Failed = "failed",
        Cancelled = "cancelled",
    }
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChecklistItem {
    pub item: String,
    pub done: bool,
}

/// Where in Slack a task was asked, so replies land in that thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlackRef {
    pub channel: String,
    pub thread_ts: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub bot: String,
    pub requested_by: String,
    pub source: Source,
    pub priority: Priority,
    pub text: String,
    pub status: TaskStatus,
    pub checklist: Vec<ChecklistItem>,
    pub result: Option<String>,
    pub error: Option<String>,
    pub approvals: Vec<String>,
    pub slack: Option<SlackRef>,
    pub routine_id: Option<String>,
    pub attempts: i32,
    /// The worker holding the current claim, while `working`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    /// When the current claim lapses unless the worker heartbeats.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Task {
    /// A new queued task.
    pub fn queued(
        bot: &str,
        requested_by: &str,
        source: Source,
        text: &str,
        slack: Option<SlackRef>,
        routine_id: Option<String>,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            id: new_task_id(),
            bot: bot.to_string(),
            requested_by: requested_by.to_string(),
            source,
            priority: source.priority(),
            text: text.to_string(),
            status: TaskStatus::Queued,
            checklist: Vec::new(),
            result: None,
            error: None,
            approvals: Vec::new(),
            slack,
            routine_id,
            attempts: 0,
            worker: None,
            lease_until: None,
            created_at: now,
            updated_at: now,
        }
    }
}

/// One line of a task's progress (`GET /v1/tasks/{id}/events`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub at: DateTime<Utc>,
    pub kind: String,
    pub text: String,
}

text_enum!(
    TraceKind {
        Prompt = "prompt",
        ModelReply = "model_reply",
        ToolCall = "tool_call",
        ToolResult = "tool_result",
        PolicyDenial = "policy_denial",
    }
);

/// One entry of a task's full trace (D6), for the console.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceEntry {
    pub at: DateTime<Utc>,
    pub kind: TraceKind,
    pub body: Value,
}

/// The body a Slack tool result is left with after 24 hours (SLK-8).
pub fn redacted_body() -> Value {
    serde_json::json!({"redacted": "slack content removed after 24 h"})
}

/// The tool a trace body is about, from its `tool` (or `name`) field, so a Slack result can
/// be found again for redaction.
pub fn trace_tool(body: &Value) -> Option<String> {
    body.get("tool")
        .or_else(|| body.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Is this a tool from the Slack connector? Registry names use dots (`slack.read_thread`),
/// harness names underscores (`slack_read_thread`).
pub fn is_slack_tool(tool: &str) -> bool {
    tool.starts_with("slack.") || tool.starts_with("slack_")
}

text_enum!(
    NotificationKind {
        Milestone = "milestone",
        Done = "done",
        Failed = "failed",
        NeedsApproval = "needs_approval",
        ApprovalClosed = "approval_closed",
    }
);

/// How one approval item ended, for an `approval_closed` notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalOutcome {
    pub id: String,
    pub tool: Option<String>,
    /// `approved`, `rejected`, `failed`, `expired` (a Slack draft nobody decided within a
    /// day), or `missing` when the tool server no longer has it.
    pub status: String,
    pub decided_by: Option<String>,
}

/// What a store transition should announce. The store fills in the task's fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewNotification {
    pub kind: NotificationKind,
    pub text: String,
    pub approvals: Vec<String>,
    pub outcomes: Vec<ApprovalOutcome>,
}

impl NewNotification {
    pub fn new(kind: NotificationKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
            approvals: Vec::new(),
            outcomes: Vec::new(),
        }
    }
}

/// One item of the Slack client's notification feed, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notification {
    /// `ntf_<seq>`.
    pub id: String,
    /// Position in the feed; the cursor is the last one seen.
    #[serde(skip)]
    pub seq: i64,
    pub task_id: String,
    pub bot: String,
    pub requested_by: String,
    pub slack: Option<SlackRef>,
    pub kind: NotificationKind,
    pub text: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub approvals: Vec<String>,
    /// For `approval_closed`: how each item ended.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub outcomes: Vec<ApprovalOutcome>,
    pub at: DateTime<Utc>,
}

// ----- routines -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    /// Five fields: minute, hour, day of month, month, day of week.
    pub cron: String,
    /// An IANA time zone, such as `Europe/London`.
    pub tz: String,
}

/// Where a routine's result goes. v1 has one kind: a DM to the owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    pub kind: String,
}

impl Output {
    pub fn dm() -> Self {
        Self { kind: "dm".into() }
    }
}

/// A routine before it is saved: what `/v1/routines/parse` returns and `POST /v1/routines`
/// takes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Draft {
    pub bot: String,
    pub owner: String,
    pub schedule: Schedule,
    pub prompt: String,
    pub output: Output,
    pub approval: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Routine {
    pub id: String,
    pub bot: String,
    pub owner: String,
    pub enabled: bool,
    pub schedule: Schedule,
    pub prompt: String,
    pub output: Output,
    pub approval: String,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_have_the_spec_shape() {
        let id = new_task_id();
        assert_eq!(id.len(), 4 + 32);
        assert!(id.starts_with("tsk_"));
        assert!(id[4..].chars().all(|c| c.is_ascii_hexdigit()));
        assert!(new_routine_id().starts_with("rtn_"));
    }

    #[test]
    fn sources_set_the_priority() {
        assert_eq!(Source::Slack.priority(), Priority::Interactive);
        assert_eq!(Source::Cli.priority(), Priority::Interactive);
        assert_eq!(Source::Routine.priority(), Priority::Scheduled);
        assert_eq!(Source::Eval.priority(), Priority::Personal);
        assert_eq!(
            serde_json::to_value(TaskStatus::NeedsApproval).unwrap(),
            "needs_approval"
        );
        assert_eq!(TaskStatus::parse("working"), Some(TaskStatus::Working));
    }

    #[test]
    fn slack_tools_are_recognised_in_both_spellings() {
        assert!(is_slack_tool("slack.read_thread"));
        assert!(is_slack_tool("slack_search"));
        assert!(!is_slack_tool("jira.search"));
        assert_eq!(
            trace_tool(&serde_json::json!({"tool": "slack.search", "result": []})).as_deref(),
            Some("slack.search")
        );
        assert_eq!(trace_tool(&serde_json::json!({"text": "x"})), None);
    }
}
