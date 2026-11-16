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
//!
//! **What the PM is told about the target comes from ids, never from free text the bot
//! wrote.** The label is built from the target arguments only (space, page, project, issue
//! type, epic, channel), and a value that isn't shaped like an id reads "an unrecognised …"
//! so a bot can't put a sentence there. `target.url`, `expires_at` and `external` come
//! from the tool server's own fields; `sources` from its audit log.
//!
//! **`version`** is the SHA-256 of the tool and the canonical JSON of the arguments an
//! approve would send now (the item's arguments with the PM's saved edit applied). An
//! approve that names another version is refused (`409 stale`), so what is published is
//! what the PM saw.
//!
//! **Citations.** The bot's source markers (`[[src-3f9a1c]]`, `crates/contract/src/cite.rs`)
//! are resolved against the sources the tool server lists for the item ([`with_sources`]):
//! each one naming a listed source becomes `[[sN]]` and the source is `cited`; any other is
//! removed. An edited draft has none. The tool server removes markers when it writes, so
//! the `version` (the arguments as stored) is unaffected.

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use pmos_contract::cite;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::settings::{DraftEdit, PracticeDraft, StoredSuggestion, UserState};
use crate::upstream::{Approval, ApprovalSource};

/// The most sources a draft shows.
pub const MAX_SOURCES: usize = 20;
/// The longest source title shown.
pub const MAX_SOURCE_TITLE: usize = 200;

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
    /// `null` for a source with no link (a context library file, or Jira and Confluence
    /// when the PM has no linked Atlassian site).
    pub url: Option<String>,
    /// Always `null`: no source text is kept anywhere to quote (see `crates/web/README.md`).
    pub excerpt: Option<String>,
    /// The draft's body marks at least one sentence with this source (`[[s3]]`); see
    /// [`with_sources`]. False for a source the bot read but didn't cite.
    pub cited: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Draft {
    #[serde(flatten)]
    pub summary: DraftSummary,
    pub body: String,
    pub edited: bool,
    pub target: Target,
    /// What the bot read for the run that made the draft (the tool server's
    /// `GET /internal/approvals/{id}/sources`); empty when it can't say.
    pub sources: Vec<Source>,
    pub result_url: Option<String>,
    pub external: bool,
    /// What an approve would publish now; see the module docs.
    pub version: String,
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
            // A suggestion is the PM's to accept as their own text: no source markers.
            body: s.body.as_deref().map(cite::strip),
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
            format!("Update to page {}", ident(s(args, "page_id"), "page")),
            s(args, "body").into(),
        ),
        "jira.create_issue" => (s(args, "summary").into(), s(args, "description").into()),
        "slack.post_reply" => (
            format!("Reply in {}", ident(s(args, "channel"), "channel")),
            s(args, "text").into(),
        ),
        _ => (
            format!("Message to {}", ident(s(args, "channel"), "channel")),
            s(args, "text").into(),
        ),
    }
}

/// `value` if it is shaped like an id (letters, digits, `-`, `_`, `.`, `~`, at most 64),
/// else "an unrecognised `what`": a target label never carries free text.
fn ident(value: &str, what: &str) -> String {
    let ok = !value.is_empty()
        && value.chars().count() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~'));
    if ok {
        value.to_string()
    } else {
        format!("an unrecognised {what}")
    }
}

/// Issue types are words: letters, digits, spaces and `-`, at most 32.
fn issue_type(value: &str) -> String {
    let ok = !value.trim().is_empty()
        && value.chars().count() <= 32
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-'));
    if ok {
        value.trim().to_string()
    } else {
        "issue".to_string()
    }
}

/// An `https` link with nothing in it that could break out of an attribute.
pub fn safe_url(url: &str) -> bool {
    url.starts_with("https://")
        && url.len() > "https://".len()
        && url.len() <= 2048
        && !url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '<' | '>' | '\\'))
}

fn target(item: &Approval) -> Target {
    let (tool, args) = (item.tool.as_str(), &item.args);
    let (system, label) = match tool {
        "confluence.create_draft" => {
            let mut label = format!("Space {}", ident(s(args, "space"), "space"));
            if !s(args, "parent_id").is_empty() {
                label.push_str(&format!(
                    ", under page {}",
                    ident(s(args, "parent_id"), "page")
                ));
            }
            ("confluence", label)
        }
        "confluence.update_page" => (
            "confluence",
            format!("Page {}", ident(s(args, "page_id"), "page")),
        ),
        "jira.create_issue" => {
            let mut label = format!(
                "Project {}, a new {}",
                ident(s(args, "project"), "project"),
                issue_type(s(args, "issue_type"))
            );
            if !s(args, "epic").is_empty() {
                label.push_str(&format!(" under {}", ident(s(args, "epic"), "epic")));
            }
            ("jira", label)
        }
        "slack.post_reply" => (
            "slack",
            format!("A thread in {}", ident(s(args, "channel"), "channel")),
        ),
        _ => ("slack", ident(s(args, "channel"), "channel")),
    };
    Target {
        system,
        label,
        url: item.target_url.clone().filter(|u| safe_url(u)),
    }
}

/// The tool server's sources as the draft's: each with a link only if it is `https`, a
/// plain title of bounded length, no excerpt (none is kept anywhere), at most
/// [`MAX_SOURCES`], none cited (see [`with_sources`]).
pub fn sources(found: Vec<ApprovalSource>) -> Vec<Source> {
    listed(found).into_iter().map(|(s, _)| s).collect()
}

/// [`sources`], each with the ref the bot's markers name it by, if it is one.
fn listed(found: Vec<ApprovalSource>) -> Vec<(Source, Option<String>)> {
    found
        .into_iter()
        .filter(|s| !s.reference.is_empty())
        .take(MAX_SOURCES)
        .enumerate()
        .map(|(i, s)| {
            let cite = s
                .cite
                .as_deref()
                .filter(|c| cite::is_ref(c))
                .map(str::to_ascii_lowercase);
            let title: String = s
                .title
                .chars()
                .filter(|c| !c.is_control())
                .take(MAX_SOURCE_TITLE)
                .collect();
            let source = Source {
                id: format!("s{}", i + 1),
                system: ident(&s.system, "system"),
                title: if title.trim().is_empty() {
                    s.reference.chars().take(MAX_SOURCE_TITLE).collect()
                } else {
                    title
                },
                url: s.url.filter(|u| safe_url(u)),
                excerpt: None,
                cited: false,
            };
            (source, cite)
        })
        .collect()
}

/// What resolving a draft's source markers did, for a log line (counts only, no text).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Citations {
    /// Markers that name a listed source.
    pub kept: usize,
    /// Markers that name a source the run didn't read (or that isn't listed), and malformed
    /// ones.
    pub dropped: usize,
}

/// The draft with what the bot read for it (the tool server's sources), and its body's
/// source markers resolved against them (`docs/web-api.md`, Citations):
///
/// - Only a draft shown as the bot wrote it has citations. Each `[[src-…]]` marker that
///   names a listed source becomes `[[sN]]`, `sN` being that source's `id`, and the source
///   is `cited`. Any other marker is removed, cleanly ([`cite::render`]).
/// - A draft with the PM's saved edit (an edit, or an accepted suggestion) has none: its body
///   is the PM's text, which has no markers ([`draft`] removes any), and no source is
///   `cited`.
///
/// The `version` is untouched: it is what an approve would send, markers and all (the tool
/// server removes them when it writes).
pub fn with_sources(
    mut draft: Draft,
    item: &Approval,
    state: &UserState,
    found: Vec<ApprovalSource>,
) -> (Draft, Citations) {
    let mut listed = listed(found);
    let mut citations = Citations::default();
    if !state.edits.contains_key(&item.id) {
        let (_, raw) = title_and_body(&item.tool, &item.args);
        let rendered = cite::render(&display_body(&raw), |r| {
            let (source, _) = listed.iter_mut().find(|(_, c)| c.as_deref() == Some(r))?;
            source.cited = true;
            Some(format!("[[{}]]", source.id))
        });
        citations = Citations {
            kept: rendered.kept,
            dropped: rendered.dropped,
        };
        draft.body = rendered.text;
    }
    draft.sources = listed.into_iter().map(|(s, _)| s).collect();
    (draft, citations)
}

/// `value` with object keys sorted at every level, as the tool server hashes arguments.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for k in keys {
                out.insert(k.clone(), canonical(&map[k]));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

fn hex_sha256(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u64).to_be_bytes());
        h.update(p.as_bytes());
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The version of a write: its tool and its arguments.
pub fn args_version(tool: &str, args: &Value) -> String {
    let canonical = serde_json::to_string(&canonical(args)).unwrap_or_default();
    hex_sha256(&[tool, &canonical])
}

/// The arguments an approve of `item` would send now: with the PM's saved edit, if any.
pub fn effective_args(item: &Approval, state: &UserState) -> Value {
    match state.edits.get(&item.id) {
        Some(edit) => edited_args(&item.tool, &item.args, edit),
        None => item.args.clone(),
    }
}

/// The version of `item` as the PM sees it now.
pub fn version(item: &Approval, state: &UserState) -> String {
    args_version(&item.tool, &effective_args(item, state))
}

/// The version of a practice draft as the PM sees it now.
pub fn practice_version(p: &PracticeDraft, state: &UserState) -> String {
    let edit = state.edits.get(&p.id);
    let title = edit
        .and_then(|e| e.title.as_deref())
        .unwrap_or(PRACTICE_TITLE);
    let body = edit.map(|e| e.body.as_str()).unwrap_or(PRACTICE_BODY);
    hex_sha256(&["practice", &p.id, title, body])
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

/// When a Slack draft expires: the tool server's own `expires_at`, or for a tool server that
/// doesn't send it yet, its default of 24 hours after creation.
fn expires_at(item: &Approval) -> Option<String> {
    if !is_slack(&item.tool) {
        return None;
    }
    if let Some(at) = item
        .expires_at
        .as_deref()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
    {
        return Some(rfc3339(at.with_timezone(&Utc)));
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
    // A title is never cited: a marker in it is removed.
    let title = cite::strip(&title);
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

/// A body for display: if it has no real line break at all but does contain the two
/// characters `\` `n`, the model wrote its line breaks as JSON escapes (a whole draft on one
/// line reading "…launch.\n\nSource: PM-101"), so `\n` becomes a line break, `\t` a tab
/// and `\r\n` one line break. `\\` is left as it is (an escaped backslash, so `\\n`
/// stays as written). A body with any real line break is left alone: there the two
/// characters are more likely meant (a path, a regex, a code sample).
///
/// **Display only.** This changes what the PM reads, never what an approve sends: the
/// `version` and an approve without an edit use the item's own arguments, exactly as the
/// bot drafted them and as the tool server hashed them, so nothing is published that the
/// approval record doesn't hold. The trade-off is that an unedited approve publishes the
/// escapes the PM did not see. Saving an edit (any `PUT`) sends the text the PM saw, which
/// is then what they approve. Repairing on approve instead would publish text that differs
/// from the stored, hashed arguments without the PM's own edit, which we don't do.
pub fn display_body(body: &str) -> std::borrow::Cow<'_, str> {
    if body.contains('\n') || !body.contains("\\n") {
        return std::borrow::Cow::Borrowed(body);
    }
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('n') => {
                chars.next();
                out.push('\n');
            }
            Some('t') => {
                chars.next();
                out.push('\t');
            }
            Some('r') => {
                // `\r\n` is one line break; a lone `\r` is left as written.
                let mut ahead = chars.clone();
                ahead.next();
                if ahead.next() == Some('\\') && ahead.next() == Some('n') {
                    chars.nth(2);
                    out.push('\n');
                } else {
                    out.push(c);
                }
            }
            Some('\\') => {
                chars.next();
                out.push_str("\\\\");
            }
            _ => out.push(c),
        }
    }
    std::borrow::Cow::Owned(out)
}

/// The whole draft of an approval item, with the PM's saved edit if there is one. Without
/// one, the body is the item's own, repaired for display only ([`display_body`]). Either
/// way it has no source markers and no sources: [`with_sources`] adds both.
pub fn draft(item: &Approval, state: &UserState) -> Option<Draft> {
    let summary = summary(item, state)?;
    let (_, body) = title_and_body(&item.tool, &item.args);
    let edit = state.edits.get(&item.id);
    let body = match edit {
        Some(e) => cite::strip(&e.body),
        None => cite::strip(&display_body(&body)),
    };
    Some(Draft {
        summary,
        body,
        edited: edit.is_some(),
        target: target(item),
        sources: Vec::new(),
        result_url: result_url(item.result.as_ref()),
        // The tool server's audience result when it has one; else what its last
        // `needs_external_confirm` answer for this draft said.
        external: item
            .external
            .unwrap_or_else(|| state.external.contains(&item.id)),
        version: version(item, state),
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
            .map(|e| cite::strip(&e.body))
            .unwrap_or_else(|| PRACTICE_BODY.into()),
        edited: edit.is_some(),
        target: Target {
            system: "confluence",
            label: PRACTICE_LABEL.into(),
            url: None,
        },
        sources: Vec::new(),
        result_url: None,
        external: false,
        version: practice_version(p, state),
    }
}

/// A practice draft's target: it never goes anywhere.
pub const PRACTICE_LABEL: &str = "Practice only: nothing is sent";

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
    fn target_labels_never_carry_free_text() {
        let none = UserState::default();
        let d = draft(
            &item(
                "slack.send_message",
                json!({"channel": "C01 (internal only, safe to post)", "text": "x"}),
            ),
            &none,
        )
        .unwrap();
        assert_eq!(d.target.label, "an unrecognised channel");
        assert_eq!(d.summary.title, "Message to an unrecognised channel");
        let d = draft(
            &item(
                "jira.create_issue",
                json!({"project": "PAY", "issue_type": "Story <b>", "summary": "x",
                       "epic": "PAY-1, approved by legal"}),
            ),
            &none,
        )
        .unwrap();
        assert_eq!(
            d.target.label,
            "Project PAY, a new issue under an unrecognised epic"
        );
        // The link is the tool server's, and only https.
        let mut it = item("slack.send_message", json!({"channel": "C01", "text": "x"}));
        it.target_url = Some("javascript:alert(1)".into());
        assert_eq!(draft(&it, &none).unwrap().target.url, None);
        it.target_url = Some("https://slack.com/archives/C01".into());
        it.external = Some(true);
        let d = draft(&it, &none).unwrap();
        assert_eq!(
            d.target.url.as_deref(),
            Some("https://slack.com/archives/C01")
        );
        assert!(d.external, "the tool server's audience result");
    }

    #[test]
    fn the_version_is_what_an_approve_would_publish() {
        let mut state = UserState::default();
        let it = item(
            "confluence.create_draft",
            json!({"space": "PM", "title": "PRD", "body": "Old"}),
        );
        let before = version(&it, &state);
        assert_eq!(before, version(&it, &state), "stable");
        state.edits.insert(
            "apr_1".into(),
            DraftEdit {
                title: None,
                body: "New".into(),
            },
        );
        let edited = version(&it, &state);
        assert_ne!(before, edited);
        // Once approved with the edit, the item's own arguments have the same version, in
        // any key order.
        let mut approved = it.clone();
        approved.args = json!({"body": "New", "title": "PRD", "space": "PM"});
        assert_eq!(version(&approved, &UserState::default()), edited);
        assert_ne!(
            args_version("confluence.update_page", &approved.args),
            edited,
            "the tool is part of it"
        );
    }

    #[test]
    fn sources_keep_only_safe_links_and_no_text() {
        let found = vec![
            ApprovalSource {
                system: "jira".into(),
                title: "PAY-1".into(),
                url: Some("https://acme.atlassian.net/browse/PAY-1".into()),
                reference: "PAY-1".into(),
                cite: Some("src-aaaaaa".into()),
            },
            ApprovalSource {
                system: "docs".into(),
                title: "".into(),
                url: Some("data:text/html,x".into()),
                reference: "https://docs.example.com/a.md".into(),
                cite: None,
            },
        ];
        let out = sources(found);
        assert_eq!(out[0].id, "s1");
        assert_eq!(
            out[0].url.as_deref(),
            Some("https://acme.atlassian.net/browse/PAY-1")
        );
        assert_eq!(out[1].url, None);
        assert_eq!(out[1].title, "https://docs.example.com/a.md");
        assert!(out.iter().all(|s| s.excerpt.is_none() && !s.cited));
    }

    fn src(reference: &str, cite: Option<&str>) -> ApprovalSource {
        ApprovalSource {
            system: "jira".into(),
            title: reference.into(),
            url: None,
            reference: reference.into(),
            cite: cite.map(String::from),
        }
    }

    fn read_three() -> Vec<ApprovalSource> {
        vec![
            src("PAY-1", Some("src-aaaaaa")),
            src("PAY-2", Some("src-bbbbbb")),
            src("PAY-3", Some("src-cccccc")),
        ]
    }

    #[test]
    fn markers_become_citations_of_sources_the_task_read() {
        let none = UserState::default();
        let body = "# Plan [[src-aaaaaa]]\n\nGrace is 7 days [[src-cccccc]][[SRC-AAAAAA]].\n\
                    - Churn [[src-ffffff]] rises.\n\nUse `[[src-bbbbbb]]` in code.\n";
        let it = item(
            "confluence.create_draft",
            json!({"space": "PM", "title": "Plan [[src-aaaaaa]]", "body": body}),
        );
        let (d, c) = with_sources(draft(&it, &none).unwrap(), &it, &none, read_three());
        assert_eq!(
            d.body,
            "# Plan[[s1]]\n\nGrace is 7 days[[s3]][[s1]].\n- Churn rises.\n\nUse `[[src-bbbbbb]]` in code.\n"
        );
        assert_eq!(d.summary.title, "Plan", "titles are never cited");
        assert_eq!((c.kept, c.dropped), (3, 1));
        let cited: Vec<(&str, bool)> = d.sources.iter().map(|s| (s.id.as_str(), s.cited)).collect();
        assert_eq!(cited, [("s1", true), ("s2", false), ("s3", true)]);
        // The version is still what an approve sends: the item's own arguments.
        assert_eq!(d.version, args_version(&it.tool, &it.args));
        // The body without sources (as `draft` gives it, and as a change request sends it to
        // the bot) has no markers at all.
        assert_eq!(
            draft(&it, &none).unwrap().body,
            "# Plan\n\nGrace is 7 days.\n- Churn rises.\n\nUse `[[src-bbbbbb]]` in code.\n"
        );
    }

    #[test]
    fn a_draft_without_markers_or_refs_is_as_before() {
        let none = UserState::default();
        let it = item(
            "jira.create_issue",
            json!({"project": "PAY", "issue_type": "Story", "summary": "S", "description": "Plain."}),
        );
        let before = draft(&it, &none).unwrap();
        let (d, c) = with_sources(before.clone(), &it, &none, read_three());
        assert_eq!(d.body, "Plain.");
        assert_eq!(c, Citations::default());
        assert!(d.sources.iter().all(|s| !s.cited));
        assert_eq!(d.version, before.version);
        // An older tool server sends no refs: markers name nothing and are removed, and
        // the bot's own `[[s1]]` is never taken for a citation.
        let it = item(
            "slack.post_reply",
            json!({"channel": "C01", "thread_ts": "1.2", "text": "Yes [[src-aaaaaa]] and [[s1]]."}),
        );
        let old = vec![src("PAY-1", None), src("PAY-2", Some("not a ref"))];
        let (d, c) = with_sources(draft(&it, &none).unwrap(), &it, &none, old);
        assert_eq!(d.body, "Yes and.");
        assert_eq!((c.kept, c.dropped), (0, 2));
        assert!(d.sources.iter().all(|s| !s.cited));
        // No sources at all (the tool server couldn't say).
        let (d, _) = with_sources(draft(&it, &none).unwrap(), &it, &none, Vec::new());
        assert_eq!(d.body, "Yes and.");
        assert!(d.sources.is_empty());
    }

    #[test]
    fn an_edited_draft_has_no_citations() {
        let it = item(
            "confluence.create_draft",
            json!({"space": "PM", "title": "T", "body": "Claim [[src-aaaaaa]]."}),
        );
        let mut edited = UserState::default();
        edited.edits.insert(
            "apr_1".into(),
            DraftEdit {
                title: None,
                // Saved before markers were removed on save, or typed by the PM.
                body: "Claim, edited [[s1]] [[src-aaaaaa]].".into(),
            },
        );
        let (d, c) = with_sources(draft(&it, &edited).unwrap(), &it, &edited, read_three());
        assert_eq!(d.body, "Claim, edited.");
        assert!(d.edited);
        assert_eq!(c, Citations::default());
        assert!(d.sources.iter().all(|s| !s.cited));
        assert_eq!(d.sources.len(), 3, "still listed as read");
    }

    #[test]
    fn literal_line_breaks_are_repaired_for_display_only() {
        // One line, with the escapes the model wrote: shown with real line breaks.
        assert_eq!(
            display_body("We launch.\\n\\nSource: PM-101\\tok"),
            "We launch.\n\nSource: PM-101\tok"
        );
        assert_eq!(display_body("a\\r\\nb\\rc"), "a\nb\\rc");
        // An escaped backslash stays as written.
        assert_eq!(display_body("C:\\\\new \\n x"), "C:\\\\new \n x");
        // Any real line break: left alone, escapes and all.
        let mixed = "Use `\\n` to split.\nNext line";
        assert_eq!(display_body(mixed), mixed);
        assert_eq!(display_body("plain"), "plain");

        let none = UserState::default();
        let raw = "We launch.\\n\\nSource: PM-101";
        let it = item(
            "confluence.create_draft",
            json!({"space": "PM", "title": "PRD", "body": raw}),
        );
        let d = draft(&it, &none).unwrap();
        assert_eq!(d.body, "We launch.\n\nSource: PM-101");
        // What an approve sends, and the version, are the item's own arguments.
        assert_eq!(effective_args(&it, &none)["body"], raw);
        assert_eq!(d.version, args_version(&it.tool, &it.args));

        // Once the PM saves an edit, the edit is the body, and it is what is sent.
        let mut edited = UserState::default();
        edited.edits.insert(
            "apr_1".into(),
            DraftEdit {
                title: None,
                body: d.body.clone(),
            },
        );
        let d2 = draft(&it, &edited).unwrap();
        assert_eq!(d2.body, "We launch.\n\nSource: PM-101");
        assert_eq!(
            effective_args(&it, &edited)["body"],
            "We launch.\n\nSource: PM-101"
        );
        assert_ne!(d2.version, d.version);
        // An edit with literal escapes is the PM's own text: never rewritten.
        edited.edits.get_mut("apr_1").unwrap().body = raw.into();
        assert_eq!(draft(&it, &edited).unwrap().body, raw);
    }

    #[test]
    fn practice_ids_are_safe() {
        let p = new_practice(Utc::now());
        assert!(is_practice_id(&p.id));
        assert!(p.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    }
}
