//! Approval items shown as drafts (`docs/web-api.md`, Drafts).
//!
//! A draft's `kind`, `title`, `body` and `target` come from the item's `tool` and `args`, in
//! the shapes the tool server's write tools take (`crates/toolserver/src/schemas.rs`):
//!
//! | Tool | `kind` | `title` | `body` | Target |
//! | --- | --- | --- | --- | --- |
//! | `confluence.create_draft` `{space, title, body, parent_id?}` | `confluence_page` | `title` | `body` | Space `space`, under page `parent_id` |
//! | `confluence.update_page` `{page_id, body, version_comment?}` | `confluence_page` | "Update to page `page_id`" (the item has no title) | `body` | Page `page_id` |
//! | `jira.create_issue` `{project, issue_type, summary, description?, epic?}` | `jira_issue` | `summary` | `description` | Project `project`, a `issue_type`, under `epic` |
//! | `slack.post_reply` `{channel, thread_ts, text, message_ts?}` | `slack_reply` | "Reply in `channel`" | `text` | A thread in `channel` |
//! | `slack.send_message` `{channel, text}` | `slack_message` | "Message to `channel`" | `text` | `channel` |
//!
//! Other write tools (such as `bets.record`) are not drafts in this contract and are left
//! out. A PM's edit maps back onto the same fields ([`edited_args`]).

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::{json, Value};

use crate::settings::{DraftEdit, PracticeDraft, StoredSuggestion, UserState};
use crate::upstream::Approval;

/// Slack drafts expire this long after they are made (the tool server's default
/// `PMOS_SLACK_DRAFT_TTL_HOURS`; it doesn't report its setting).
pub const SLACK_TTL_HOURS: i64 = 24;

/// How many decided drafts the list shows.
pub const RECENT_DECIDED: usize = 20;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DraftSummary {
    pub id: String,
    pub kind: &'static str,
    pub title: String,
    pub bot: String,
    pub status: &'static str,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub task_id: Option<String>,
    pub practice: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Target {
    pub system: &'static str,
    pub label: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Source {
    pub id: String,
    pub system: String,
    pub title: String,
    pub url: String,
    pub excerpt: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Draft {
    #[serde(flatten)]
    pub summary: DraftSummary,
    pub body: String,
    pub edited: bool,
    pub target: Target,
    /// Always empty today: see `crates/web/README.md` ("Sources").
    pub sources: Vec<Source>,
    pub result_url: Option<String>,
    pub external: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Suggestion {
    pub status: String,
    pub instruction: String,
    pub body: Option<String>,
    pub error: Option<String>,
}

impl From<&StoredSuggestion> for Suggestion {
    fn from(s: &StoredSuggestion) -> Self {
        Self {
            status: s.status.clone(),
            instruction: s.instruction.clone(),
            body: s.body.clone(),
            error: s.error.clone(),
        }
    }
}

/// The draft kind of a write tool, or `None` for one that isn't a draft here.
pub fn kind(tool: &str) -> Option<&'static str> {
    match tool {
        "confluence.create_draft" | "confluence.update_page" => Some("confluence_page"),
        "jira.create_issue" => Some("jira_issue"),
        "slack.post_reply" => Some("slack_reply"),
        "slack.send_message" => Some("slack_message"),
        _ => None,
    }
}

fn is_slack(tool: &str) -> bool {
    tool.starts_with("slack.")
}

/// The tool server's item status as a draft status. `executing` (approved, the write is
/// running) reads as `approved`, with no `result_url` yet.
pub fn status(item_status: &str) -> &'static str {
    match item_status {
        "pending" => "pending",
        "executing" | "approved" => "approved",
        "rejected" => "rejected",
        "expired" => "expired",
        _ => "failed",
    }
}

fn s<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or_default()
}

pub fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn title_and_body(tool: &str, args: &Value) -> (String, String) {
    match tool {
        "confluence.create_draft" => (s(args, "title").into(), s(args, "body").into()),
        "confluence.update_page" => (
            format!("Update to page {}", s(args, "page_id")),
            s(args, "body").into(),
        ),
        "jira.create_issue" => (s(args, "summary").into(), s(args, "description").into()),
        "slack.post_reply" => (
            format!("Reply in {}", s(args, "channel")),
            s(args, "text").into(),
        ),
        _ => (
            format!("Message to {}", s(args, "channel")),
            s(args, "text").into(),
        ),
    }
}

fn target(tool: &str, args: &Value) -> Target {
    let (system, label) = match tool {
        "confluence.create_draft" => {
            let mut label = format!("Space {}", s(args, "space"));
            if !s(args, "parent_id").is_empty() {
                label.push_str(&format!(", under page {}", s(args, "parent_id")));
            }
            ("confluence", label)
        }
        "confluence.update_page" => ("confluence", format!("Page {}", s(args, "page_id"))),
        "jira.create_issue" => {
            let mut label = format!(
                "Project {}, a new {}",
                s(args, "project"),
                s(args, "issue_type")
            );
            if !s(args, "epic").is_empty() {
                label.push_str(&format!(" under {}", s(args, "epic")));
            }
            ("jira", label)
        }
        "slack.post_reply" => ("slack", format!("A thread in {}", s(args, "channel"))),
        _ => ("slack", s(args, "channel").to_string()),
    };
    Target {
        system,
        label,
        url: None,
    }
}

/// The item's arguments with the PM's edit applied, for decision `edit`.
pub fn edited_args(tool: &str, args: &Value, edit: &DraftEdit) -> Value {
    let mut args = args.clone();
    if !args.is_object() {
        args = json!({});
    }
    let (title_key, body_key) = match tool {
        "confluence.create_draft" => (Some("title"), "body"),
        "confluence.update_page" => (None, "body"),
        "jira.create_issue" => (Some("summary"), "description"),
        _ => (None, "text"),
    };
    args[body_key] = json!(edit.body);
    if let (Some(key), Some(title)) = (title_key, edit.title.as_ref()) {
        args[key] = json!(title);
    }
    args
}

/// Whether `tool`'s draft has a title the PM can change.
pub fn has_title(tool: &str) -> bool {
    matches!(tool, "confluence.create_draft" | "jira.create_issue")
}

/// The link to what an approved write made: the `url` the tool server stored as its result.
pub fn result_url(result: Option<&Value>) -> Option<String> {
    let url = result?.get("url")?.as_str()?;
    (url.starts_with("https://") || url.starts_with("http://")).then(|| url.to_string())
}

fn expires_at(item: &Approval) -> Option<String> {
    if !is_slack(&item.tool) {
        return None;
    }
    DateTime::parse_from_rfc3339(&item.created_at)
        .ok()
        .map(|t| rfc3339(t.with_timezone(&Utc) + Duration::hours(SLACK_TTL_HOURS)))
}

/// The summary of an approval item, or `None` for a tool that isn't a draft.
pub fn summary(item: &Approval, state: &UserState) -> Option<DraftSummary> {
    let kind = kind(&item.tool)?;
    let (mut title, _) = title_and_body(&item.tool, &item.args);
    if let Some(t) = state.edits.get(&item.id).and_then(|e| e.title.as_ref()) {
        if has_title(&item.tool) {
            title = t.clone();
        }
    }
    Some(DraftSummary {
        id: item.id.clone(),
        kind,
        title,
        bot: item
            .bot
            .clone()
            .or_else(|| item.skill.clone())
            .unwrap_or_default(),
        status: status(&item.status),
        created_at: item.created_at.clone(),
        expires_at: expires_at(item),
        task_id: Some(item.run_id.clone()).filter(|r| r.starts_with("tsk_")),
        practice: false,
    })
}

/// The whole draft of an approval item, with the PM's saved edit if there is one.
pub fn draft(item: &Approval, state: &UserState) -> Option<Draft> {
    let summary = summary(item, state)?;
    let (_, mut body) = title_and_body(&item.tool, &item.args);
    let edit = state.edits.get(&item.id);
    if let Some(e) = edit {
        body = e.body.clone();
    }
    Some(Draft {
        summary,
        body,
        edited: edit.is_some(),
        target: target(&item.tool, &item.args),
        sources: Vec::new(),
        result_url: result_url(item.result.as_ref()),
        external: state.external.contains(&item.id),
    })
}

// The practice draft.

pub const PRACTICE_PREFIX: &str = "prc_";

pub fn is_practice_id(id: &str) -> bool {
    id.starts_with(PRACTICE_PREFIX)
}

pub const PRACTICE_TITLE: &str = "Practice: a one-page PRD";

pub const PRACTICE_BODY: &str = "# Practice: a one-page PRD\n\n\
This is a practice draft. Nothing you do here is sent anywhere: approving it creates no \
page, and rejecting it tells no one.\n\n\
## Problem\n\nPMs lose track of which drafts their bot prepared and which they already \
decided.\n\n\
## Proposal\n\nShow every pending draft as a document, with what the bot read to write it.\n\n\
## Risks\n\n- A PM approves without reading. Mitigation: show the target before Approve.\n\
- Drafts pile up. Mitigation: a daily nudge.\n\n\
Try editing this text, asking for a change, then approving or rejecting it.\n";

/// What a practice suggestion says: the practice draft is never sent to a bot.
pub const PRACTICE_SUGGESTION_NOTE: &str =
    "\n\n_(Practice: on a real draft, your bot rewrites the text here.)_\n";

pub fn new_practice(now: DateTime<Utc>) -> PracticeDraft {
    PracticeDraft {
        id: format!("{PRACTICE_PREFIX}{}", &crate::session::random_token()[..22])
            .replace(|c: char| !(c.is_ascii_alphanumeric() || c == '_'), "x"),
        status: "pending".into(),
        created_at: rfc3339(now),
        decided_at: None,
    }
}

pub fn practice_draft(p: &PracticeDraft, bot: &str, state: &UserState) -> Draft {
    let edit = state.edits.get(&p.id);
    let status = match p.status.as_str() {
        "approved" => "approved",
        "rejected" => "rejected",
        _ => "pending",
    };
    Draft {
        summary: DraftSummary {
            id: p.id.clone(),
            kind: "confluence_page",
            title: edit
                .and_then(|e| e.title.clone())
                .unwrap_or_else(|| PRACTICE_TITLE.into()),
            bot: bot.into(),
            status,
            created_at: p.created_at.clone(),
            expires_at: None,
            task_id: None,
            practice: true,
        },
        body: edit
            .map(|e| e.body.clone())
            .unwrap_or_else(|| PRACTICE_BODY.into()),
        edited: edit.is_some(),
        target: Target {
            system: "confluence",
            label: "Practice only: nothing is sent".into(),
            url: None,
        },
        sources: Vec::new(),
        result_url: None,
        external: false,
    }
}

/// What the PM's bot is asked when they request a change. It must answer with the text only
/// and write nothing: the backend reads the task's result as the revised body.
pub fn revision_prompt(kind: &str, title: &str, body: &str, instruction: &str) -> String {
    format!(
        "Revise a draft for me. Do not create, update, post or send anything: call no write \
         tool and make no new draft or approval. Answer with the revised text only, in \
         markdown, with no introduction, no comment and no code fence around it.\n\n\
         The change I want: {instruction}\n\n\
         The draft is a {kind} titled \"{title}\". Its current text is between the markers.\n\n\
         <<<DRAFT\n{body}\nDRAFT>>>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(tool: &str, args: Value) -> Approval {
        serde_json::from_value(json!({
            "id": "apr_1", "user": "ana", "bot": "pm-ana", "run_id": "tsk_1", "tool": tool,
            "args": args, "status": "pending", "created_at": "2026-11-16T09:00:00Z"
        }))
        .unwrap()
    }

    #[test]
    fn maps_each_write_tool() {
        let none = UserState::default();
        let d = draft(
            &item(
                "confluence.create_draft",
                json!({"space": "PM", "title": "PRD", "body": "# Hi", "parent_id": "42"}),
            ),
            &none,
        )
        .unwrap();
        assert_eq!(
            (d.summary.kind, d.summary.title.as_str(), d.body.as_str()),
            ("confluence_page", "PRD", "# Hi")
        );
        assert_eq!(d.target.label, "Space PM, under page 42");
        assert_eq!(d.summary.expires_at, None);
        assert_eq!(d.summary.task_id.as_deref(), Some("tsk_1"));

        let d = draft(
            &item(
                "jira.create_issue",
                json!({"project": "PAY", "issue_type": "Story", "summary": "Grace", "description": "Text"}),
            ),
            &none,
        )
        .unwrap();
        assert_eq!((d.summary.kind, d.target.system), ("jira_issue", "jira"));
        assert_eq!(d.body, "Text");

        let d = draft(
            &item(
                "slack.post_reply",
                json!({"channel": "C01", "thread_ts": "1.2", "text": "Hello"}),
            ),
            &none,
        )
        .unwrap();
        assert_eq!(d.summary.kind, "slack_reply");
        assert_eq!(
            d.summary.expires_at.as_deref(),
            Some("2026-11-17T09:00:00Z")
        );
        let d = draft(
            &item(
                "slack.send_message",
                json!({"channel": "U02", "text": "Hi"}),
            ),
            &none,
        )
        .unwrap();
        assert_eq!((d.summary.kind, d.body.as_str()), ("slack_message", "Hi"));
        assert!(draft(&item("bets.record", json!({})), &none).is_none());
    }

    #[test]
    fn edits_go_back_into_the_same_fields() {
        let edit = DraftEdit {
            title: Some("New".into()),
            body: "Edited".into(),
        };
        let args = json!({"project": "PAY", "issue_type": "Story", "summary": "Old"});
        assert_eq!(
            edited_args("jira.create_issue", &args, &edit),
            json!({"project": "PAY", "issue_type": "Story", "summary": "New", "description": "Edited"})
        );
        let args = json!({"channel": "C01", "thread_ts": "1.2", "text": "Old"});
        assert_eq!(
            edited_args("slack.post_reply", &args, &edit),
            json!({"channel": "C01", "thread_ts": "1.2", "text": "Edited"})
        );
        let args = json!({"page_id": "9", "body": "Old"});
        assert_eq!(
            edited_args("confluence.update_page", &args, &edit),
            json!({"page_id": "9", "body": "Edited"})
        );
    }

    #[test]
    fn reads_the_result_link_and_statuses() {
        assert_eq!(
            result_url(Some(
                &json!({"url": "https://acme.atlassian.net/browse/PAY-1"})
            )),
            Some("https://acme.atlassian.net/browse/PAY-1".into())
        );
        assert_eq!(result_url(Some(&json!({"url": "javascript:x"}))), None);
        assert_eq!(result_url(Some(&json!({"error": "no"}))), None);
        assert_eq!(status("executing"), "approved");
        assert_eq!(status("failed"), "failed");
    }

    #[test]
    fn practice_ids_are_safe() {
        let p = new_practice(Utc::now());
        assert!(is_practice_id(&p.id));
        assert!(p.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    }
}
