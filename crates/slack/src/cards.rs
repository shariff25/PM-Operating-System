//! Block Kit for the bots: progress and answers in a task's thread, approval cards and the
//! Edit modal (D7), and routines (D8).
//!
//! Every piece of dynamic text is escaped for `mrkdwn`, and only `https://` links are kept.

use serde_json::{json, Value};

use crate::blocks::{escape, link, truncate};

/// Fields an approval card's **Edit** modal offers, when the item's arguments have them.
pub const EDITABLE: [&str; 5] = ["summary", "description", "title", "body", "text"];

/// Slack caps section text at 3,000 characters and a message at 50 blocks.
const SECTION_MAX: usize = 2900;
const MAX_BLOCKS: usize = 45;
/// Slack caps a text input's value at 3,000 characters.
const INPUT_MAX: usize = 3000;

/// Action ids the bot puts on buttons, and the modal's callback id.
pub mod action {
    pub const APPROVE: &str = "apr_approve";
    pub const EDIT: &str = "apr_edit";
    pub const REJECT: &str = "apr_reject";
    /// A reason button is this prefix and the reason's code: `apr_reason_tone`.
    pub const REASON_PREFIX: &str = "apr_reason_";
    /// Back from the reasons to the card's own buttons.
    pub const BACK: &str = "apr_back";
    pub const EDIT_MODAL: &str = "apr_edit_modal";
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

/// A bot's answer or message, from markdown.
pub fn answer(markdown: &str) -> (String, Value) {
    (
        fallback(markdown),
        Value::Array(sections(&mrkdwn(markdown))),
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

/// The arguments of a write, one line per field: the editable ones first.
pub fn args_summary(args: &Value) -> String {
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

fn approval_buttons(item: &Value) -> Value {
    let id = item["id"].as_str().unwrap_or_default();
    let mut elements = vec![button("Approve", action::APPROVE, id, Some("primary"))];
    if !editable_fields(item).is_empty() {
        elements.push(button("Edit", action::EDIT, id, None));
    }
    elements.push(button("Reject", action::REJECT, id, Some("danger")));
    json!({"type": "actions", "block_id": format!("approval:{id}"), "elements": elements})
}

/// A pending approval card: the tool, its arguments, and **Approve**, **Edit**, **Reject**.
pub fn approval_card(item: &Value) -> (String, Value) {
    let tool = item["tool"].as_str().unwrap_or("a write");
    let mut blocks = approval_head(item);
    blocks.push(approval_buttons(item));
    (
        format!("Approval needed: {}", escape(&tool_label(tool))),
        Value::Array(blocks),
    )
}

/// The card after **Reject**: why, as six one-tap reasons, and **Back** to the card's own
/// buttons. Nothing is decided until a reason is chosen.
pub fn approval_reasons(item: &Value) -> (String, Value) {
    let id = item["id"].as_str().unwrap_or_default();
    let mut elements: Vec<Value> = REASONS
        .iter()
        .map(|(code, label)| button(label, &format!("{}{code}", action::REASON_PREFIX), id, None))
        .collect();
    elements.push(button("Back", action::BACK, id, None));
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
pub fn approval_retry(item: &Value, error: &str) -> (String, Value) {
    let (text, blocks) = approval_card(item);
    let mut blocks = blocks.as_array().cloned().unwrap_or_default();
    blocks.push(context(&format!(":warning: {}", escape(error))));
    (text, Value::Array(blocks))
}

/// "Approved by Ana — <link>", "Rejected by Ana", or what went wrong, from the decided item.
pub fn decision_line(item: &Value, decided_by: &str, edited: bool) -> String {
    let who = escape(&person(item["decided_by"].as_str().unwrap_or(decided_by)));
    let verb = if edited {
        "Edited and approved"
    } else {
        "Approved"
    };
    match item["status"].as_str().unwrap_or_default() {
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

/// The editable string fields this item has, with their values.
pub fn editable_fields(item: &Value) -> Vec<(&'static str, String)> {
    EDITABLE
        .iter()
        .filter_map(|f| item["args"][*f].as_str().map(|v| (*f, v.to_string())))
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
    Some(json!({
        "type": "modal",
        "callback_id": action::EDIT_MODAL,
        "title": {"type": "plain_text", "text": "Edit before approving"},
        "submit": {"type": "plain_text", "text": "Approve"},
        "close": {"type": "plain_text", "text": "Cancel"},
        "private_metadata": metadata.to_string(),
        "blocks": blocks,
    }))
}

/// The item's arguments with the modal's edited fields put in. Only editable string fields
/// the item already had can change.
pub fn edited_args(args: &Value, state_values: &Value) -> Value {
    let mut out = args.clone();
    for field in EDITABLE {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_becomes_safe_mrkdwn() {
        let md = "# Ports\nDSM needs **443** <script> & more. See [the docs](https://support.fortanix.com/x) or [bad](javascript:alert(1)).\n- one\n```\na < b\n```";
        let out = mrkdwn(md);
        assert!(out.starts_with("*Ports*\n"));
        assert!(out.contains("*443* &lt;script&gt; &amp; more"));
        assert!(out.contains("<https://support.fortanix.com/x|the docs>"));
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
        let (_, blocks) = approval_card(&item);
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

    #[test]
    fn reject_reasons() {
        let item = json!({
            "id": "apr_1", "user": "ana", "tool": "jira.create_issue",
            "args": {"summary": "Fix <login>"}, "status": "pending",
        });
        let (_, blocks) = approval_reasons(&item);
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
