//! Block Kit for the bots: progress and answers in a task's thread, approval cards and the
//! Edit modal (D7), and routines (D8).
//!
//! Every piece of dynamic text is escaped for `mrkdwn`, and only `https://` links are kept.

use serde_json::{json, Value};

use crate::blocks::{escape, link, truncate};
use crate::runtime::safe_id;

/// Fields an approval card's **Edit** modal offers, when the item's arguments have them:
/// what a draft says, never where it goes.
pub const EDITABLE: [&str; 5] = ["summary", "description", "title", "body", "text"];

/// The arguments that say where a draft goes. An edit from the bot must leave them as drafted
/// (the tool server refuses one that changes them with `422`), so the Edit modal never offers
/// them and [`edited_args`] never changes them.
pub const TARGETS: [&str; 13] = [
    "channel",
    "thread_ts",
    "message_ts",
    "space",
    "parent_id",
    "page_id",
    "version",
    "project",
    "issue_type",
    "epic",
    "parent",
    "key",
    "repo",
];

/// What the card says when the tool server refuses an edit with `422`: it changed where the
/// draft goes.
pub const TARGET_CHANGED: &str = "An edit can change what a draft says, not where it goes, so \
     nothing was sent. Approve the draft as it is, or reject it and ask again.";

/// Slack caps section text at 3,000 characters and a message at 50 blocks.
const SECTION_MAX: usize = 2900;
const MAX_BLOCKS: usize = 45;
/// Slack caps a text input's value at 3,000 characters.
const INPUT_MAX: usize = 3000;

/// Action ids the bot puts on buttons, and the modal's callback id.
pub mod action {
    pub const APPROVE: &str = "apr_approve";
    /// **Open in workspace**: a URL button to the web app's draft page. Slack sends no handler
    /// for it, and a click is acknowledged with nothing.
    pub const OPEN_WEB: &str = "apr_open_web";
    pub const EDIT: &str = "apr_edit";
    pub const REJECT: &str = "apr_reject";
    /// A reason button is this prefix and the reason's code: `apr_reason_tone`.
    pub const REASON_PREFIX: &str = "apr_reason_";
    /// Back from the reasons to the card's own buttons.
    pub const BACK: &str = "apr_back";
    pub const EDIT_MODAL: &str = "apr_edit_modal";
    /// **Snooze** on a reply card: its choices in place.
    pub const SNOOZE: &str = "apr_snooze";
    /// A snooze choice is this prefix and the choice's code: `apr_snooze_4h`.
    pub const SNOOZE_PREFIX: &str = "apr_snooze_";
    /// **Post to external channel**: the decision again, with `confirm_external`.
    pub const POST_EXTERNAL: &str = "apr_post_external";
    pub const ROUTINE_CREATE: &str = "rtn_create";
    pub const ROUTINE_CANCEL: &str = "rtn_cancel";
    pub const ROUTINE_PAUSE: &str = "rtn_pause";
    pub const ROUTINE_RESUME: &str = "rtn_resume";
    pub const ROUTINE_DELETE: &str = "rtn_delete";
}

/// Why a draft was rejected: the code the tool server's ledger stores, and the button's
/// label (`docs/improvement-loop-spec.md` §1 and §4).
pub const REASONS: [(&str, &str); 6] = [
    ("wrong_facts", "Wrong facts"),
    ("wrong_target", "Wrong place"),
    ("tone", "Tone"),
    ("incomplete", "Incomplete"),
    ("should_not_draft", "Shouldn't have drafted"),
    ("other", "Other"),
];

/// The reason code a button's action id carries, if it is one of [`REASONS`].
pub fn reason_of_action(action_id: &str) -> Option<&'static str> {
    let code = action_id.strip_prefix(action::REASON_PREFIX)?;
    REASONS.iter().find(|(c, _)| *c == code).map(|(c, _)| *c)
}

/// The label of a reason code: `tone` → "Tone". `None` for anything else.
pub fn reason_label(code: &str) -> Option<&'static str> {
    REASONS.iter().find(|(c, _)| *c == code).map(|(_, l)| *l)
}

fn section(text: &str) -> Value {
    json!({"type": "section", "text": {"type": "mrkdwn", "text": text}})
}

fn context(text: &str) -> Value {
    json!({"type": "context", "elements": [{"type": "mrkdwn", "text": text}]})
}

fn button(text: &str, action_id: &str, value: &str, style: Option<&str>) -> Value {
    let mut b = json!({
        "type": "button",
        "text": {"type": "plain_text", "text": text},
        "action_id": action_id,
        "value": value,
    });
    if let Some(style) = style {
        b["style"] = json!(style);
    }
    b
}

/// The plain-text `text` of a message: what notifications and screen readers show.
fn fallback(text: &str) -> String {
    escape(&truncate(text, 300))
}

/// `ana` → `Ana`, for "Approved by Ana".
pub fn person(handle: &str) -> String {
    let mut c = handle.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

/// Markdown as the bots write it, as Slack `mrkdwn`: headings and `**bold**` become bold,
/// `- ` bullets become `•`, `[label](https://…)` becomes a link. Everything else is escaped.
pub fn mrkdwn(markdown: &str) -> String {
    let mut out = Vec::new();
    let mut in_code = false;
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            out.push("```".to_string());
            continue;
        }
        if in_code {
            out.push(escape(line));
            continue;
        }
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
            out.push(format!(
                "*{}*",
                inline(trimmed[hashes..].trim()).replace('*', "")
            ));
            continue;
        }
        if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            let indent = line.len() - trimmed.len();
            out.push(format!("{}• {}", " ".repeat(indent), inline(rest)));
            continue;
        }
        out.push(inline(line));
    }
    out.join("\n")
}

fn bold(escaped: &str) -> String {
    escaped.replace("**", "*")
}

/// One line: escape it, and turn `[label](url)` into a Slack link (https only).
fn inline(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        if let Some(close) = rest[open..].find("](") {
            let label_end = open + close;
            let url_start = label_end + 2;
            if let Some(url_len) = rest[url_start..].find(')') {
                let label = &rest[open + 1..label_end];
                let url = &rest[url_start..url_start + url_len];
                if !label.contains('[') && !url.contains(char::is_whitespace) {
                    out.push_str(&bold(&escape(&rest[..open])));
                    out.push_str(&link(url, label));
                    rest = &rest[url_start + url_len + 1..];
                    continue;
                }
            }
        }
        out.push_str(&bold(&escape(&rest[..=open])));
        rest = &rest[open + 1..];
    }
    out.push_str(&bold(&escape(rest)));
    out
}

/// `mrkdwn` text as section blocks, split at line breaks to fit Slack's limits.
pub fn sections(text: &str) -> Vec<Value> {
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in text.split('\n') {
        let line = if line.chars().count() > SECTION_MAX {
            truncate(line, SECTION_MAX - 1)
        } else {
            line.to_string()
        };
        if !current.is_empty() && current.chars().count() + line.chars().count() + 1 > SECTION_MAX {
            chunks.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(&line);
    }
    if !current.trim().is_empty() {
        chunks.push(current);
    }
    if chunks.is_empty() {
        chunks.push("_(no text)_".into());
    }
    let cut = chunks.len() > MAX_BLOCKS;
    let mut blocks: Vec<Value> = chunks.iter().take(MAX_BLOCKS).map(|c| section(c)).collect();
    if cut {
        blocks.push(context(
            "_This answer was cut short for Slack; the full text is in the console._",
        ));
    }
    blocks
}

/// A bot's answer or message, from markdown. Source markers (`[[src-…]]`, for drafts in the
/// web app) are removed.
pub fn answer(markdown: &str) -> (String, Value) {
    let markdown = pmos_contract::cite::strip(markdown);
    (
        fallback(&markdown),
        Value::Array(sections(&mrkdwn(&markdown))),
    )
}

/// A short message that is already safe `mrkdwn`.
pub fn note(mrkdwn_text: &str) -> (String, Value) {
    (
        truncate(mrkdwn_text, 300),
        json!([section(&truncate(mrkdwn_text, SECTION_MAX))]),
    )
}

/// Where a task's run stands, for its progress message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Running,
    Done,
    Waiting,
    Failed,
}

/// The progress message: one per task, edited in place as milestones arrive (D6).
pub fn progress(milestones: &[String], stage: Stage) -> (String, Value) {
    let last = milestones.len().saturating_sub(1);
    let lines: Vec<String> = milestones
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let mark = match (i == last, stage) {
                (true, Stage::Running) => ":hourglass_flowing_sand:",
                (true, Stage::Failed) => ":x:",
                _ => ":white_check_mark:",
            };
            format!("{mark} {}", escape(&truncate(m, 200)))
        })
        .collect();
    let mut blocks = sections(&lines.join("\n"));
    match stage {
        Stage::Waiting => blocks.push(context("Waiting for your approval below.")),
        Stage::Failed => blocks.push(context("Stopped.")),
        Stage::Done | Stage::Running => {}
    }
    let text = milestones
        .last()
        .map(|m| fallback(m))
        .unwrap_or_else(|| "Working on it".into());
    (text, Value::Array(blocks))
}

/// A readable name for a write tool.
pub fn tool_label(tool: &str) -> String {
    match tool {
        "jira.create_issue" => "Create a Jira issue".into(),
        "confluence.create_draft" => "Create a Confluence draft".into(),
        "confluence.update_page" => "Update a Confluence page".into(),
        "slack.post_reply" => "Post a Slack reply as you".into(),
        "slack.send_message" => "Send a Slack message as you".into(),
        other => format!("Run {other}"),
    }
}

/// The arguments of a write, one line per field: the editable ones first. Text is shown as
/// it will be written: without source markers (`[[src-…]]`), which the tool server removes
/// when it writes.
pub fn args_summary(args: &Value) -> String {
    let args = &pmos_contract::cite::strip_json(args);
    let Some(obj) = args.as_object() else {
        return escape(&truncate(&args.to_string(), 500));
    };
    let mut keys: Vec<&String> = obj.keys().collect();
    keys.sort_by_key(|k| {
        (
            EDITABLE
                .iter()
                .position(|e| e == k)
                .unwrap_or(EDITABLE.len()),
            k.to_string(),
        )
    });
    let mut lines = Vec::new();
    for key in keys.into_iter().take(12) {
        let label = person(&key.replace('_', " "));
        let value = match &obj[key] {
            Value::String(s) => {
                let max = if matches!(key.as_str(), "body" | "description" | "text") {
                    600
                } else {
                    200
                };
                truncate(s, max)
            }
            other => truncate(&other.to_string(), 200),
        };
        if value.contains('\n') {
            let quoted: Vec<String> = value.lines().map(|l| format!("> {}", escape(l))).collect();
            lines.push(format!("*{}:*\n{}", escape(&label), quoted.join("\n")));
        } else {
            lines.push(format!("*{}:* {}", escape(&label), escape(&value)));
        }
    }
    if obj.len() > 12 {
        lines.push(format!("_and {} more fields_", obj.len() - 12));
    }
    truncate(&lines.join("\n"), SECTION_MAX - 200)
}

fn approval_head(item: &Value) -> Vec<Value> {
    if is_reply(item) {
        return reply_head(item);
    }
    let tool = item["tool"].as_str().unwrap_or("a write");
    vec![
        section(&format!(
            "*{}*\n{}",
            escape(&tool_label(tool)),
            args_summary(&item["args"])
        )),
        context(&format!(
            "`{}` · for {} · nothing is sent until you approve",
            escape(tool),
            escape(&person(item["user"].as_str().unwrap_or("you")))
        )),
    ]
}

/// The Confluence and Jira drafts get **Open in workspace** when the web app's address is
/// set (`PMOS_WEB_PUBLIC_URL`, an `https://` origin with no trailing `/`). Slack replies and
/// messages do not, and neither does a card when the address is unset or the id is not safe
/// to put in a URL.
fn open_web_button(item: &Value, web: Option<&str>) -> Option<Value> {
    let web = web?;
    let tool = item["tool"].as_str().unwrap_or_default();
    let id = item["id"].as_str().unwrap_or_default();
    if !(tool.starts_with("confluence.") || tool.starts_with("jira.")) || !safe_id(id) {
        return None;
    }
    Some(json!({
        "type": "button",
        "text": {"type": "plain_text", "text": "Open in workspace"},
        "url": format!("{web}/drafts/{id}"),
        "action_id": action::OPEN_WEB,
    }))
}

fn approval_buttons(item: &Value, web: Option<&str>) -> Value {
    let id = item["id"].as_str().unwrap_or_default();
    let mut elements = if is_reply(item) {
        vec![button("Post as you", action::APPROVE, id, Some("primary"))]
    } else {
        vec![button("Approve", action::APPROVE, id, Some("primary"))]
    };
    if !editable_fields(item).is_empty() {
        elements.push(button("Edit", action::EDIT, id, None));
    }
    if is_reply(item) {
        elements.push(button("Snooze", action::SNOOZE, id, None));
    }
    elements.push(button("Reject", action::REJECT, id, Some("danger")));
    elements.extend(open_web_button(item, web));
    json!({"type": "actions", "block_id": format!("approval:{id}"), "elements": elements})
}

/// A pending approval card: the tool, its arguments, and **Approve**, **Edit**, **Reject**.
/// A reply draft gets the reply card instead: **Post as you**, **Edit**, **Snooze** and
/// **Reject**.
pub fn approval_card(item: &Value, web: Option<&str>) -> (String, Value) {
    let tool = item["tool"].as_str().unwrap_or("a write");
    let mut blocks = approval_head(item);
    blocks.push(approval_buttons(item, web));
    let text = if is_reply(item) {
        format!("{}: approval needed", reply_title(item))
    } else {
        format!("Approval needed: {}", escape(&tool_label(tool)))
    };
    (text, Value::Array(blocks))
}

/// The card after **Reject**: why, as six one-tap reasons, and **Back** to the card's own
/// buttons. Nothing is decided until a reason is chosen.
pub fn approval_reasons(item: &Value, web: Option<&str>) -> (String, Value) {
    let id = item["id"].as_str().unwrap_or_default();
    let mut elements: Vec<Value> = REASONS
        .iter()
        .map(|(code, label)| button(label, &format!("{}{code}", action::REASON_PREFIX), id, None))
        .collect();
    elements.push(button("Back", action::BACK, id, None));
    elements.extend(open_web_button(item, web));
    let mut blocks = approval_head(item);
    blocks.push(section("*Why reject it?* Nothing is sent either way."));
    blocks
        .push(json!({"type": "actions", "block_id": format!("reject:{id}"), "elements": elements}));
    ("Why reject it?".into(), Value::Array(blocks))
}

/// The card with a line under it instead of the buttons: decided, or being decided.
pub fn approval_closed(item: &Value, line: &str) -> (String, Value) {
    let mut blocks = approval_head(item);
    blocks.push(section(line));
    (truncate(line, 300), Value::Array(blocks))
}

/// The card with its buttons back and an error under them, to try again.
pub fn approval_retry(item: &Value, error: &str, web: Option<&str>) -> (String, Value) {
    let (text, blocks) = approval_card(item, web);
    let mut blocks = blocks.as_array().cloned().unwrap_or_default();
    blocks.push(context(&format!(":warning: {}", escape(error))));
    (text, Value::Array(blocks))
}

/// "Approved by Ana — <link>", "Rejected by Ana", or what went wrong, from the decided item.
pub fn decision_line(item: &Value, decided_by: &str, edited: bool) -> String {
    let who = escape(&person(item["decided_by"].as_str().unwrap_or(decided_by)));
    let verb = match (edited, is_reply(item)) {
        (true, true) => "Edited and posted",
        (false, true) => "Posted",
        (true, false) => "Edited and approved",
        (false, false) => "Approved",
    };
    match item["status"].as_str().unwrap_or_default() {
        "approved" if is_reply(item) => {
            match item["result"]["url"]
                .as_str()
                .filter(|u| u.starts_with("https://"))
            {
                Some(url) => format!(
                    ":white_check_mark: {verb} by {who} — {}",
                    link(url, "see the reply")
                ),
                None => format!(":white_check_mark: {verb} by {who}"),
            }
        }
        "expired" => EXPIRED_LINE.to_string(),
        "approved" => {
            let result = &item["result"];
            match result["url"].as_str().filter(|u| u.starts_with("https://")) {
                Some(url) => {
                    let label = result["id"].as_str().unwrap_or("Open it");
                    format!(":white_check_mark: {verb} by {who} — {}", link(url, label))
                }
                None => format!(":white_check_mark: {verb} by {who}"),
            }
        }
        "rejected" => match item["reason"].as_str().and_then(reason_label) {
            Some(reason) => format!(":no_entry_sign: Rejected by {who}: {}", escape(reason)),
            None => format!(":no_entry_sign: Rejected by {who}"),
        },
        "failed" => format!(
            ":warning: {verb} by {who}, but the write failed: {}",
            escape(&truncate(
                item["result"]["error"].as_str().unwrap_or("unknown error"),
                500
            ))
        ),
        "executing" => format!(":hourglass_flowing_sand: {verb} by {who}; writing…"),
        other => format!("Status: {}", escape(other)),
    }
}

/// The editable string fields this item has, with their values, without source markers: the
/// PM edits the text as it will be written. Never a target ([`TARGETS`]).
pub fn editable_fields(item: &Value) -> Vec<(&'static str, String)> {
    EDITABLE
        .iter()
        .filter(|f| !TARGETS.contains(f))
        .filter_map(|f| {
            item["args"][*f]
                .as_str()
                .map(|v| (*f, pmos_contract::cite::strip(v)))
        })
        .collect()
}

/// The **Edit** modal, prefilled. `None` if nothing in the item can be edited in Slack.
pub fn edit_modal(item: &Value, channel: &str, message_ts: &str) -> Option<Value> {
    let fields = editable_fields(item);
    let mut blocks = vec![context(&format!(
        "{} · approving saves your edit",
        escape(&tool_label(item["tool"].as_str().unwrap_or("a write")))
    ))];
    let mut inputs = 0;
    for (field, value) in &fields {
        if value.chars().count() > INPUT_MAX {
            blocks.push(context(&format!(
                "_{}_ is too long to edit in Slack; it is kept as drafted.",
                escape(&person(field))
            )));
            continue;
        }
        inputs += 1;
        blocks.push(json!({
            "type": "input",
            "block_id": format!("field:{field}"),
            "label": {"type": "plain_text", "text": person(field)},
            "element": {
                "type": "plain_text_input",
                "action_id": "value",
                "multiline": matches!(*field, "body" | "description" | "text"),
                "initial_value": value,
                "max_length": INPUT_MAX,
            },
        }));
    }
    if inputs == 0 {
        return None;
    }
    let metadata = json!({
        "id": item["id"],
        "channel": channel,
        "ts": message_ts,
    });
    let (title, submit) = if is_reply(item) {
        ("Edit before posting", "Post as you")
    } else {
        ("Edit before approving", "Approve")
    };
    Some(json!({
        "type": "modal",
        "callback_id": action::EDIT_MODAL,
        "title": {"type": "plain_text", "text": title},
        "submit": {"type": "plain_text", "text": submit},
        "close": {"type": "plain_text", "text": "Cancel"},
        "private_metadata": metadata.to_string(),
        "blocks": blocks,
    }))
}

/// The item's arguments with the modal's edited fields put in. Only editable string fields
/// the item already had can change; a target ([`TARGETS`]) is always kept as drafted, even if
/// a submitted view names it.
pub fn edited_args(args: &Value, state_values: &Value) -> Value {
    let mut out = args.clone();
    for field in EDITABLE.iter().filter(|f| !TARGETS.contains(f)) {
        let field = *field;
        if out[field].is_string() {
            if let Some(v) = state_values[format!("field:{field}")]["value"]["value"].as_str() {
                out[field] = json!(v);
            }
        }
    }
    out
}

/// The fields the modal submitted empty, as Slack's `response_action: errors` map.
pub fn empty_fields(state_values: &Value) -> serde_json::Map<String, Value> {
    let mut errors = serde_json::Map::new();
    if let Some(obj) = state_values.as_object() {
        for (block_id, v) in obj {
            if block_id.starts_with("field:")
                && v["value"]["value"]
                    .as_str()
                    .is_none_or(|s| s.trim().is_empty())
            {
                errors.insert(block_id.clone(), json!("This can't be empty."));
            }
        }
    }
    errors
}

// ---- The reply card (`docs/slack-reply-spec.md` §5) ----

/// The tool whose approval items get the reply card.
pub const REPLY_TOOL: &str = "slack.post_reply";

/// What an expired draft's card says.
pub const EXPIRED_LINE: &str = ":hourglass: Expired after 24 hours; nothing was posted.";

pub fn is_reply(item: &Value) -> bool {
    item["tool"] == REPLY_TOOL
}

/// A Slack id that can go in `<#…>` or `<@…>`: capital letters and digits only.
fn slack_id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 24
        && id
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// A message timestamp: digits, a dot, digits.
fn ts_ok(ts: &str) -> bool {
    ts.split_once('.').is_some_and(|(a, b)| {
        !a.is_empty()
            && !b.is_empty()
            && a.len() + b.len() <= 24
            && a.chars().chain(b.chars()).all(|c| c.is_ascii_digit())
    })
}

/// Where a reply goes, as `mrkdwn`: the channel as a Slack mention (Slack shows its name to
/// whoever can see it, so the bot never has to read the channel), or "your DM", with the
/// other person when the item says who.
pub fn reply_place(item: &Value) -> String {
    let channel = item["args"]["channel"].as_str().unwrap_or_default();
    if channel.starts_with('D') {
        match item["audience"]["dm_with"]
            .as_str()
            .filter(|u| slack_id_ok(u))
        {
            Some(user) => format!("your DM with <@{user}>"),
            None => "your DM".into(),
        }
    } else if slack_id_ok(channel) {
        format!("<#{channel}>")
    } else {
        "a Slack channel".into()
    }
}

/// "Reply drafted for #launch", as `mrkdwn`.
pub fn reply_title(item: &Value) -> String {
    format!("Reply drafted for {}", reply_place(item))
}

/// The thread's link: `https://slack.com/archives/<channel>/p<thread_ts without the dot>`,
/// the format the tool server's Slack connector gives its own results. Slack opens it in the
/// workspace for anyone who can see the channel. It is built here rather than asked of
/// `chat.getPermalink`, which the bot token can't answer for channels the bot isn't in.
pub fn thread_link(channel: &str, thread_ts: &str) -> Option<String> {
    (slack_id_ok(channel) && ts_ok(thread_ts)).then(|| {
        format!(
            "https://slack.com/archives/{channel}/p{}",
            thread_ts.replace('.', "")
        )
    })
}

/// A reply draft as safe `mrkdwn`, as Slack will show it once posted: `<https://…|label>`
/// and `[label](https://…)` links are kept (https only), as are `<@U…>` and `<#C…>`
/// mentions; anything else in angle brackets, `<!here>` included, is escaped.
pub fn reply_text(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        out.push_str(&inline(&rest[..open]));
        let Some(close) = rest[open..].find('>') else {
            rest = &rest[open..];
            break;
        };
        let inner = &rest[open + 1..open + close];
        let (target, label) = match inner.split_once('|') {
            Some((t, l)) => (t, Some(l)),
            None => (inner, None),
        };
        let kept = if target.starts_with("https://") && !target.contains(char::is_whitespace) {
            Some(link(target, label.unwrap_or(target)))
        } else if let Some(id) = target.strip_prefix('@').filter(|id| slack_id_ok(id)) {
            Some(format!("<@{id}>"))
        } else {
            target
                .strip_prefix('#')
                .filter(|id| slack_id_ok(id))
                .map(|id| format!("<#{id}>"))
        };
        match kept {
            Some(k) => out.push_str(&k),
            None => out.push_str(&escape(&rest[open..=open + close])),
        }
        rest = &rest[open + close + 1..];
    }
    out.push_str(&inline(rest));
    out
}

/// "Visible to 14 people", "External channel": what the item knows of who will see it.
fn audience_parts(audience: &Value) -> Vec<String> {
    let mut parts = Vec::new();
    if let Some(n) = audience["members"].as_u64() {
        parts.push(if n == 1 {
            "Visible to 1 person".into()
        } else {
            format!("Visible to {n} people")
        });
    }
    if audience["external"].as_bool() == Some(true) {
        parts.push("External channel".into());
    }
    parts
}

/// The reply card's head: where it goes with the thread's link, the draft, and who will see
/// it. The question's text is never on the card: it isn't in the item, and nothing here
/// reads the thread.
fn reply_head(item: &Value) -> Vec<Value> {
    let args = &item["args"];
    let mut title = format!("*{}*", reply_title(item));
    if let Some(url) = thread_link(
        args["channel"].as_str().unwrap_or_default(),
        args["thread_ts"].as_str().unwrap_or_default(),
    ) {
        title.push_str(" · ");
        title.push_str(&link(&url, "open the thread"));
    }
    let mut blocks = vec![section(&title)];
    let text = pmos_contract::cite::strip(args["text"].as_str().unwrap_or_default());
    if item["status"] != "expired" && !text.trim().is_empty() && text != "[expired]" {
        let quoted: Vec<String> = reply_text(&truncate(&text, 2500))
            .lines()
            .map(|l| format!("> {l}"))
            .collect();
        blocks.extend(sections(&quoted.join("\n")));
    }
    let mut meta = vec![
        format!(
            "for {}",
            escape(&person(item["user"].as_str().unwrap_or("you")))
        ),
        "posts as you, marked “Drafted with PM OS”".to_string(),
    ];
    if let Some(s) = item["sources_sensitivity"]
        .as_str()
        .filter(|s| matches!(*s, "public" | "internal" | "restricted"))
    {
        meta.push(format!("built from {s} sources"));
    }
    meta.extend(audience_parts(&item["audience"]));
    blocks.push(context(&meta.join(" · ")));
    blocks
}

/// The reply card's title and one line, with no draft and no buttons: snoozed, or posted
/// again below.
pub fn reply_note(item: &Value, line: &str) -> (String, Value) {
    let args = &item["args"];
    let mut title = format!("*{}*", reply_title(item));
    if let Some(url) = thread_link(
        args["channel"].as_str().unwrap_or_default(),
        args["thread_ts"].as_str().unwrap_or_default(),
    ) {
        title.push_str(" · ");
        title.push_str(&link(&url, "open the thread"));
    }
    (truncate(line, 300), json!([section(&title), section(line)]))
}

/// The `audience_blocked` answer: why, in plain words, with only **Reject**. Editing can't
/// unblock it: the draft was built from restricted sources whatever its text now says, so
/// the PM answers in the thread themselves if it still needs an answer.
/// `outsiders` is how many members aren't in `team.yaml`; with none, the channel is
/// externally shared.
pub fn reply_blocked(item: &Value, outsiders: Option<u64>) -> (String, Value) {
    let id = item["id"].as_str().unwrap_or_default();
    let place = reply_place(item);
    let why = match outsiders {
        Some(1) => format!("1 person in {place} isn't on the product team"),
        Some(n) if n > 1 => format!("{n} people in {place} aren't on the product team"),
        _ => format!("{place} is shared outside the company"),
    };
    let line = format!(
        ":no_entry: This draft was built from restricted sources and {why}, so PM OS won't \
         post it there. Reject it, and answer in the thread yourself if it still needs a reply."
    );
    let elements = vec![button("Reject", action::REJECT, id, Some("danger"))];
    let mut blocks = approval_head(item);
    blocks.push(section(&line));
    blocks.push(
        json!({"type": "actions", "block_id": format!("blocked:{id}"), "elements": elements}),
    );
    (truncate(&line, 300), Value::Array(blocks))
}

/// The `needs_external_confirm` answer: **Post to external channel** sends the decision
/// again with `confirm_external`. After an edit, its value says so (`<id>:edit`), so the
/// edited text is what posts.
pub fn reply_confirm_external(item: &Value, edited: bool) -> (String, Value) {
    let id = item["id"].as_str().unwrap_or_default();
    let place = reply_place(item);
    let line = format!(
        ":warning: {place} is an external channel (Slack Connect): people outside the company \
         will see this reply. Post it there anyway?"
    );
    let value = if edited {
        format!("{id}:edit")
    } else {
        id.to_string()
    };
    let mut elements = vec![button(
        "Post to external channel",
        action::POST_EXTERNAL,
        &value,
        Some("primary"),
    )];
    if !editable_fields(item).is_empty() {
        elements.push(button("Edit", action::EDIT, id, None));
    }
    elements.push(button("Reject", action::REJECT, id, Some("danger")));
    let mut shown = item.clone();
    shown["audience"]["external"] = json!(true);
    let mut blocks = approval_head(&shown);
    blocks.push(section(&line));
    blocks.push(
        json!({"type": "actions", "block_id": format!("external:{id}"), "elements": elements}),
    );
    (truncate(&line, 300), Value::Array(blocks))
}

/// **Snooze**'s choices in place, each `(code, label)`, and **Back**. With none left before
/// the draft expires, it says so.
pub fn snooze_choices(item: &Value, choices: &[(&str, &str)], expires: &str) -> (String, Value) {
    let id = item["id"].as_str().unwrap_or_default();
    let mut elements: Vec<Value> = choices
        .iter()
        .map(|(code, label)| button(label, &format!("{}{code}", action::SNOOZE_PREFIX), id, None))
        .collect();
    elements.push(button("Back", action::BACK, id, None));
    let line = if choices.is_empty() {
        format!(
            "*Too late to snooze:* the draft expires {}. Post, edit or reject it.",
            escape(expires)
        )
    } else {
        format!(
            "*Snooze until when?* The card comes back here then. The draft expires {}.",
            escape(expires)
        )
    };
    let mut blocks = approval_head(item);
    blocks.push(section(&line));
    blocks
        .push(json!({"type": "actions", "block_id": format!("snooze:{id}"), "elements": elements}));
    ("Snooze until when?".into(), Value::Array(blocks))
}

/// A routine read back for a one-tap confirm (D8).
pub fn routine_readback(summary: &str, key: &str) -> (String, Value) {
    (
        fallback(summary),
        json!([
            section(&escape(&truncate(summary, SECTION_MAX))),
            {"type": "actions", "block_id": "routine_draft", "elements": [
                button("Create", action::ROUTINE_CREATE, key, Some("primary")),
                button("Cancel", action::ROUTINE_CANCEL, key, None),
            ]},
        ]),
    )
}

/// How a routine runs, in words.
fn schedule(routine: &Value) -> String {
    if let Some(s) = routine["summary"].as_str() {
        return escape(&truncate(s, 300));
    }
    let cron = routine["schedule"]["cron"].as_str().unwrap_or("?");
    let tz = routine["schedule"]["tz"].as_str().unwrap_or("UTC");
    format!("`{}` ({})", escape(cron), escape(tz))
}

/// "my routines": each with **Pause** or **Resume**, and **Delete**.
pub fn routine_list(routines: &[Value], display: impl Fn(&str) -> String) -> (String, Value) {
    if routines.is_empty() {
        return note(
            "You have no routines. Start one with a message like “every weekday at 8am, \
             summarize #launch”.",
        );
    }
    let shown = routines.iter().take(15);
    let mut blocks = vec![section(&format!("*Your routines* ({})", routines.len()))];
    for r in shown {
        let id = r["id"].as_str().unwrap_or_default();
        let enabled = r["enabled"].as_bool().unwrap_or(true);
        let mut text = format!(
            "*{}* · {}{}",
            escape(&display(r["bot"].as_str().unwrap_or("?"))),
            schedule(r),
            if enabled { "" } else { " · _paused_" }
        );
        if let Some(prompt) = r["prompt"].as_str() {
            text.push('\n');
            text.push_str(&escape(&truncate(prompt, 200)));
        }
        if let Some(next) = r["next_run_at"].as_str().filter(|_| enabled) {
            text.push_str(&format!("\n_Next run: {}_", escape(next)));
        }
        blocks.push(section(&text));
        let toggle = if enabled {
            button("Pause", action::ROUTINE_PAUSE, id, None)
        } else {
            button("Resume", action::ROUTINE_RESUME, id, Some("primary"))
        };
        let mut delete = button("Delete", action::ROUTINE_DELETE, id, Some("danger"));
        delete["confirm"] = json!({
            "title": {"type": "plain_text", "text": "Delete this routine?"},
            "text": {"type": "plain_text", "text": "It stops running. This can't be undone."},
            "confirm": {"type": "plain_text", "text": "Delete"},
            "deny": {"type": "plain_text", "text": "Keep it"},
        });
        blocks.push(json!({
            "type": "actions",
            "block_id": format!("routine:{id}"),
            "elements": [toggle, delete],
        }));
    }
    if routines.len() > 15 {
        blocks.push(context(&format!(
            "_{} more are in the console._",
            routines.len() - 15
        )));
    }
    (
        format!("You have {} routines", routines.len()),
        Value::Array(blocks),
    )
}

// ---- The first-time flow (`PMOS_ONBOARDING_V2=1`, `onboard_copy.rs`) ----

/// A starter button's action id is this prefix and the starter's id.
pub const ONBOARD_START_PREFIX: &str = "pmos_onboard_start_";
/// The welcome's one button: a link, so Slack sends no handler for a click.
pub const ONBOARD_CONNECT: &str = "pmos_onboard_connect";

/// The welcome: two short lines and one **Connect Jira and Confluence** button. Without a
/// link, the lines and a plain sentence say what to do instead.
pub fn onboard_hello(first_name: &str, url: Option<&str>) -> (String, Value) {
    let first = truncate(first_name.trim(), 60);
    let greeting = if first.is_empty() {
        crate::onboard_copy::HELLO_NO_NAME.to_string()
    } else {
        crate::onboard_copy::HELLO.replace("{name}", &escape(&first))
    };
    match url.filter(|u| u.starts_with("https://") || u.starts_with("http://")) {
        Some(url) => (
            truncate(&greeting, 300),
            json!([
                section(&greeting),
                {"type": "actions", "block_id": "onboard_hello", "elements": [{
                    "type": "button", "style": "primary",
                    "text": {"type": "plain_text", "text": crate::onboard_copy::CONNECT_BUTTON},
                    "action_id": ONBOARD_CONNECT, "url": url,
                }]},
            ]),
        ),
        None => {
            let text = format!("{greeting}\n{}", crate::onboard_copy::NO_LINK);
            (truncate(&text, 300), json!([section(&text)]))
        }
    }
}

/// The offer after the account is linked: three buttons, each starting a task.
pub fn onboard_offer() -> (String, Value) {
    let elements: Vec<Value> = crate::onboard_copy::STARTERS
        .iter()
        .map(|s| {
            button(
                s.label,
                &format!("{ONBOARD_START_PREFIX}{}", s.id),
                s.id,
                None,
            )
        })
        .collect();
    (
        crate::onboard_copy::OFFER.to_string(),
        json!([
            section(crate::onboard_copy::OFFER),
            {"type": "actions", "block_id": "onboard_offer", "elements": elements},
        ]),
    )
}

/// The offer after a starter was tapped: no buttons left.
pub fn onboard_started(label: &str) -> (String, Value) {
    note(&crate::onboard_copy::STARTED.replace("{label}", &escape(label)))
}

/// The one closing line.
pub fn onboard_closing() -> (String, Value) {
    note(crate::onboard_copy::CLOSING)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_becomes_safe_mrkdwn() {
        let md = "# Ports\nAcme Vault needs **443** <script> & more. See [the docs](https://docs.example.com/x) or [bad](javascript:alert(1)).\n- one\n```\na < b\n```";
        let out = mrkdwn(md);
        assert!(out.starts_with("*Ports*\n"));
        assert!(out.contains("*443* &lt;script&gt; &amp; more"));
        assert!(out.contains("<https://docs.example.com/x|the docs>"));
        assert!(!out.contains("javascript"), "{out}");
        assert!(out.contains("• one"));
        assert!(out.contains("a &lt; b"));
        // A stray bracket is just text.
        assert_eq!(mrkdwn("a [b c"), "a [b c");
    }

    #[test]
    fn long_answers_are_split() {
        let long = "word ".repeat(2000);
        let blocks = sections(&format!("{long}\n{long}\nend"));
        assert!(blocks.len() >= 3);
        for b in &blocks {
            assert!(b["text"]["text"].as_str().unwrap().chars().count() <= SECTION_MAX);
        }
        assert_eq!(sections("").len(), 1, "never an empty section");
    }

    #[test]
    fn approval_cards() {
        let item = json!({
            "id": "apr_1", "user": "ana", "tool": "confluence.create_draft",
            "args": {"space": "PM", "title": "Launch <plan>", "body": "line 1\nline 2"},
            "status": "pending",
        });
        let (_, blocks) = approval_card(&item, None);
        let s = blocks.to_string();
        assert!(s.contains("Create a Confluence draft"));
        assert!(s.contains("*Title:* Launch &lt;plan&gt;"), "{s}");
        assert!(s.contains("> line 2"));
        for a in [action::APPROVE, action::EDIT, action::REJECT] {
            assert!(s.contains(a));
        }
        let modal = edit_modal(&item, "D1", "1.0").unwrap();
        assert_eq!(
            modal["blocks"].as_array().unwrap().len(),
            3,
            "title and body"
        );
        let edited = edited_args(
            &item["args"],
            &json!({"field:title": {"value": {"value": "New"}}, "field:space": {"value": {"value": "X"}}}),
        );
        assert_eq!(edited["title"], "New");
        assert_eq!(edited["space"], "PM", "only editable fields change");
        assert!(edit_modal(&json!({"args": {"key": "X"}}), "D1", "1").is_none());

        let mut done = item.clone();
        done["status"] = json!("approved");
        done["decided_by"] = json!("ana");
        done["result"] = json!({"id": "CONF-1", "url": "https://wiki.example.com/p/1"});
        assert_eq!(
            decision_line(&done, "ana", false),
            ":white_check_mark: Approved by Ana — <https://wiki.example.com/p/1|CONF-1>"
        );
        done["status"] = json!("rejected");
        assert!(decision_line(&done, "ana", false).contains("Rejected by Ana"));
        done["reason"] = json!("should_not_draft");
        assert_eq!(
            decision_line(&done, "ana", false),
            ":no_entry_sign: Rejected by Ana: Shouldn't have drafted"
        );
    }

    /// S2: the Edit modal never offers where a draft goes, for any write tool, and a forged
    /// submission naming a target doesn't change it.
    #[test]
    fn edits_never_change_where_a_draft_goes() {
        assert!(
            EDITABLE.iter().all(|f| !TARGETS.contains(f)),
            "no field is both editable and a target"
        );
        let drafts = [
            json!({"id": "apr_1", "tool": "slack.post_reply", "status": "pending",
                   "args": {"channel": "C0A", "thread_ts": "1.1", "text": "hi"}}),
            json!({"id": "apr_2", "tool": "slack.send_message", "status": "pending",
                   "args": {"channel": "C0A", "message_ts": "2.2", "text": "hi"}}),
            json!({"id": "apr_3", "tool": "confluence.create_draft", "status": "pending",
                   "args": {"space": "PM", "parent_id": "12", "title": "T", "body": "B"}}),
            json!({"id": "apr_4", "tool": "confluence.update_page", "status": "pending",
                   "args": {"page_id": "34", "version": 3, "title": "T", "body": "B"}}),
            json!({"id": "apr_5", "tool": "jira.create_issue", "status": "pending",
                   "args": {"project": "PM", "issue_type": "Task", "epic": "PM-1",
                            "summary": "S", "description": "D"}}),
            json!({"id": "apr_6", "tool": "bets.record", "status": "pending",
                   "args": {"key": "bet-1", "text": "We bet"}}),
        ];
        for item in &drafts {
            let offered: Vec<&str> = editable_fields(item).iter().map(|(f, _)| *f).collect();
            assert!(!offered.is_empty(), "{item}");
            for f in &offered {
                assert!(!TARGETS.contains(f), "{f} offered for {}", item["tool"]);
            }
            let modal = edit_modal(item, "D1", "1.0").unwrap().to_string();
            for t in TARGETS {
                assert!(
                    !modal.contains(&format!("field:{t}\"")),
                    "{t} in the modal for {}",
                    item["tool"]
                );
            }
            let mut forged = json!({});
            for (k, v) in item["args"].as_object().unwrap() {
                let value = if v.is_string() { "CHANGED" } else { "9" };
                forged[format!("field:{k}")] = json!({"value": {"value": value}});
            }
            let edited = edited_args(&item["args"], &forged);
            for t in TARGETS {
                assert_eq!(edited[t], item["args"][t], "{t} kept for {}", item["tool"]);
            }
        }
    }

    fn reply_item() -> Value {
        json!({
            "id": "apr_r1", "user": "ana", "tool": "slack.post_reply", "status": "pending",
            "args": {"channel": "C0LAUNCH", "thread_ts": "1731500000.000100",
                     "text": "Yes, SSO ships in 5.1 <!here> & see <https://docs.example.com/sso|the SSO guide> or [notes](https://wiki.example.com/n). Ask <@U0BOB>."},
            "sources_sensitivity": "internal",
            "audience": {"members": 14, "external": true},
            "created_at": "2026-11-15T09:00:00Z",
        })
    }

    #[test]
    fn reply_cards() {
        let item = reply_item();
        let (text, blocks) = approval_card(&item, None);
        assert_eq!(text, "Reply drafted for <#C0LAUNCH>: approval needed");
        let s = blocks.to_string();
        assert!(s.contains("*Reply drafted for <#C0LAUNCH>* · <https://slack.com/archives/C0LAUNCH/p1731500000000100|open the thread>"), "{s}");
        // The draft, quoted, its links kept, and everything else escaped.
        assert!(s.contains("> Yes, SSO ships in 5.1 &lt;!here&gt; &amp; see <https://docs.example.com/sso|the SSO guide> or <https://wiki.example.com/n|notes>. Ask <@U0BOB>."), "{s}");
        assert!(s.contains("Visible to 14 people · External channel"));
        assert!(s.contains("built from internal sources"));
        assert!(s.contains("marked “Drafted with PM OS”"));
        let labels: Vec<&str> = blocks.as_array().unwrap().last().unwrap()["elements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["text"]["text"].as_str().unwrap())
            .collect();
        assert_eq!(labels, ["Post as you", "Edit", "Snooze", "Reject"]);
        // Not the generic card's argument list.
        assert!(!s.contains("*Thread ts:*") && !s.contains("*Channel:*"));

        // A DM, and channel ids that aren't ids.
        let mut dm = item.clone();
        dm["args"]["channel"] = json!("D0ANA");
        assert_eq!(reply_title(&dm), "Reply drafted for your DM");
        dm["audience"]["dm_with"] = json!("U0BOB");
        assert_eq!(reply_title(&dm), "Reply drafted for your DM with <@U0BOB>");
        let mut odd = item.clone();
        odd["args"]["channel"] = json!("C1>|<!here");
        assert_eq!(reply_title(&odd), "Reply drafted for a Slack channel");
        assert_eq!(thread_link("C1", "../x"), None);

        // Refusals.
        let (_, blocked) = reply_blocked(&item, Some(3));
        let s = blocked.to_string();
        assert!(s.contains(
            "This draft was built from restricted sources and 3 people in <#C0LAUNCH> aren't on the product team"
        ));
        assert!(s.contains(action::REJECT));
        assert!(!s.contains(action::EDIT), "an edit can't unblock it");
        assert!(!s.contains(action::APPROVE) && !s.contains(action::SNOOZE));
        let (_, shared) = reply_blocked(&item, None);
        assert!(shared
            .to_string()
            .contains("<#C0LAUNCH> is shared outside the company"));
        let (_, confirm) = reply_confirm_external(&item, false);
        assert!(confirm.to_string().contains("Post to external channel"));
        let (_, confirm_edit) = reply_confirm_external(&item, true);
        assert!(confirm_edit
            .to_string()
            .contains("\"value\":\"apr_r1:edit\""));

        // Decided and expired.
        let mut done = item.clone();
        done["status"] = json!("approved");
        done["decided_by"] = json!("ana");
        done["result"] = json!({"url": "https://slack.com/archives/C0LAUNCH/p1731500000000200"});
        assert_eq!(
            decision_line(&done, "ana", true),
            ":white_check_mark: Edited and posted by Ana — <https://slack.com/archives/C0LAUNCH/p1731500000000200|see the reply>"
        );
        let mut expired = item.clone();
        expired["status"] = json!("expired");
        expired["args"]["text"] = json!("[expired]");
        assert_eq!(decision_line(&expired, "ana", false), EXPIRED_LINE);
        let (_, closed) = approval_closed(&expired, EXPIRED_LINE);
        assert!(
            !closed.to_string().contains("[expired]"),
            "no draft once expired"
        );

        // The editor posts.
        let modal = edit_modal(&item, "D0ANA", "1.0").unwrap();
        assert_eq!(modal["submit"]["text"], "Post as you");
    }

    /// Source markers are for the web app's draft view: a card shows, and its Edit modal
    /// offers, the text as it will be written, without them.
    #[test]
    fn cards_show_drafts_without_source_markers() {
        let page = json!({
            "id": "apr_1", "user": "ana", "tool": "confluence.create_draft",
            "args": {"space": "PM", "title": "Plan [[src-3f9a1c]]",
                     "body": "Grace is 7 days [[src-3f9a1c]][[src-0b77d2]].\nUse `[[src-3f9a1c]]`."},
            "status": "pending"
        });
        let mut reply = reply_item();
        reply["args"]["text"] = json!("Yes, SSO ships in 4.2 [[src-3f9a1c]]. Ask <@U0BOB> [[s2]].");
        for item in [&page, &reply] {
            let (_, blocks) = approval_card(item, None);
            let shown = blocks.to_string();
            assert!(!shown.contains("[[src-3f9a1c]]."), "{shown}");
            assert!(!shown.contains("[[src-0b77d2]]") && !shown.contains("[[s2]]"));
            let modal = edit_modal(item, "C1", "1.2").unwrap().to_string();
            assert!(!modal.contains("[[src-0b77d2]]") && !modal.contains("[[s2]]"));
        }
        let (_, blocks) = approval_card(&page, None);
        let shown = blocks.to_string();
        assert!(shown.contains("Grace is 7 days."), "{shown}");
        assert!(shown.contains("Plan"));
        // Code keeps what it says.
        assert!(shown.contains("`[[src-3f9a1c]]`"), "{shown}");
        let (_, blocks) = approval_card(&reply, None);
        assert!(blocks
            .to_string()
            .contains("Yes, SSO ships in 4.2. Ask <@U0BOB>."));
        assert_eq!(
            editable_fields(&reply),
            vec![("text", "Yes, SSO ships in 4.2. Ask <@U0BOB>.".to_string())]
        );
        let (text, blocks) = answer("Done [[src-3f9a1c]].");
        assert_eq!(text, "Done.");
        assert!(!blocks.to_string().contains("src-"));
    }

    #[test]
    fn reply_text_keeps_only_safe_markup() {
        assert_eq!(reply_text("a < b"), "a &lt; b");
        assert_eq!(
            reply_text("<javascript:x|y> <#C1> <#c1>"),
            "&lt;javascript:x|y&gt; <#C1> &lt;#c1&gt;"
        );
        assert_eq!(
            reply_text("<https://x.com/a>"),
            "<https://x.com/a|https://x.com/a>"
        );
        assert_eq!(reply_text("<!channel>"), "&lt;!channel&gt;");
    }

    #[test]
    fn open_in_workspace_button_for_confluence_and_jira_drafts() {
        const WEB: &str = "https://pm.example.com";
        let confluence = json!({
            "id": "apr_1", "user": "ana", "tool": "confluence.create_draft",
            "args": {"space": "PM", "title": "Plan", "body": "x"}, "status": "pending",
        });
        let jira = json!({
            "id": "apr_2", "user": "ana", "tool": "jira.create_issue",
            "args": {"project": "PM", "summary": "s"}, "status": "pending",
        });
        let slack = json!({
            "id": "apr_r1", "user": "ana", "tool": "slack.post_reply", "status": "pending",
            "args": {"channel": "C0LAUNCH", "text": "hi"}, "bot": "pm-ana",
        });
        let button_of = |item: &Value, web: Option<&str>| -> Option<Value> {
            let (_, blocks) = approval_card(item, web);
            blocks
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|b| b["elements"].as_array().cloned().unwrap_or_default())
                .find(|e| e["action_id"] == action::OPEN_WEB)
        };
        for (item, id) in [(&confluence, "apr_1"), (&jira, "apr_2")] {
            let b = button_of(item, Some(WEB)).expect("button when configured");
            assert_eq!(b["text"]["text"], "Open in workspace");
            assert_eq!(b["url"], format!("{WEB}/drafts/{id}"));
            assert!(
                button_of(item, None).is_none(),
                "absent when not configured"
            );
        }
        assert!(
            button_of(&slack, Some(WEB)).is_none(),
            "no button for Slack drafts"
        );
        let (_, reasons) = approval_reasons(&confluence, Some(WEB));
        assert!(reasons
            .to_string()
            .contains("https://pm.example.com/drafts/apr_1"));
        let unsafe_id = json!({
            "id": "../apr", "tool": "jira.create_issue", "args": {"summary": "s"},
            "status": "pending",
        });
        assert!(
            button_of(&unsafe_id, Some(WEB)).is_none(),
            "unsafe id is never linked"
        );
    }

    #[test]
    fn reject_reasons() {
        let item = json!({
            "id": "apr_1", "user": "ana", "tool": "jira.create_issue",
            "args": {"summary": "Fix <login>"}, "status": "pending",
        });
        let (_, blocks) = approval_reasons(&item, None);
        let actions = blocks.as_array().unwrap().last().unwrap();
        let ids: Vec<&str> = actions["elements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["action_id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            [
                "apr_reason_wrong_facts",
                "apr_reason_wrong_target",
                "apr_reason_tone",
                "apr_reason_incomplete",
                "apr_reason_should_not_draft",
                "apr_reason_other",
                action::BACK,
            ]
        );
        assert!(actions["elements"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["value"] == "apr_1"));
        assert!(blocks.to_string().contains("*Summary:* Fix &lt;login&gt;"));
        assert_eq!(reason_of_action("apr_reason_tone"), Some("tone"));
        assert_eq!(reason_of_action("apr_reason_bogus"), None);
        assert_eq!(reason_of_action("apr_reject"), None);
    }

    #[test]
    fn progress_marks_the_current_milestone() {
        let ms = vec!["Working on it".to_string(), "Searching <docs>".to_string()];
        let (_, running) = progress(&ms, Stage::Running);
        let s = running.to_string();
        assert!(s.contains(":white_check_mark: Working on it"));
        assert!(s.contains(":hourglass_flowing_sand: Searching &lt;docs&gt;"));
        let (_, failed) = progress(&ms, Stage::Failed);
        assert!(failed.to_string().contains(":x: Searching"));
    }

    #[test]
    fn routine_lists() {
        let rs = vec![
            json!({"id": "rtn_1", "bot": "pm-ana", "enabled": true, "schedule": {"cron": "0 8 * * 1-5", "tz": "Europe/London"}, "prompt": "Summarize #launch"}),
            json!({"id": "rtn_2", "bot": "docs", "enabled": false, "schedule": {"cron": "0 9 * * 1", "tz": "UTC"}, "prompt": "x"}),
        ];
        let (_, blocks) = routine_list(&rs, crate::runtime::display_name);
        let s = blocks.to_string();
        assert!(s.contains("*PM Ana* · `0 8 * * 1-5` (Europe/London)"));
        assert!(s.contains(action::ROUTINE_PAUSE) && s.contains(action::ROUTINE_RESUME));
        assert!(s.contains("_paused_"));
        assert!(routine_list(&[], |b| b.to_string())
            .1
            .to_string()
            .contains("no routines"));
    }
}
