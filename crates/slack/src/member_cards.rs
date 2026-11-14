//! Block Kit for onboarding (`docs/onboarding-spec.md` §4): the **Request access** note, the
//! platform owners' request cards, the setup checklist, the memory-repo modal and the
//! removal confirmation.
//!
//! Names, emails and errors come from people or other services, so every one is escaped for
//! `mrkdwn`; handles and commit shas are checked before they go in a code span.

use pmos_contract::model::{valid_handle, valid_slack_id};
use pmos_contract::onboarding::personal_bot_name;
use serde_json::{json, Value};

use crate::blocks::{escape, truncate};
use crate::cards::person;

/// Action ids of the onboarding buttons, and the callback ids of its modals.
pub mod action {
    pub const REQUEST_ACCESS: &str = "pmos_request_access";
    pub const APPROVE: &str = "pmos_member_approve";
    pub const EDIT: &str = "pmos_member_edit";
    pub const REJECT: &str = "pmos_member_reject";
    pub const HANDLE_MODAL: &str = "pmos_member_handle";
    pub const REMOVE_CONFIRM: &str = "pmos_member_remove_confirm";
    pub const REMOVE_CANCEL: &str = "pmos_member_remove_cancel";
    pub const SETUP_REFRESH: &str = "pmos_setup_refresh";
    pub const SETUP_MEMORY: &str = "pmos_setup_memory";
    pub const MEMORY_MODAL: &str = "pmos_memory_repo";
    /// The prefix of the **Connect** link buttons. Slack still sends a click for a link
    /// button; it is acknowledged and nothing else happens.
    pub const SETUP_LINK: &str = "pmos_setup_link_";
    pub const CONNECT_GITHUB: &str = "pmos_connect_github";
}

/// The input's block id in the handle modal and in the memory-repo modal.
pub const HANDLE_BLOCK: &str = "handle";
pub const REPO_BLOCK: &str = "repo";

/// What someone outside `team.yaml` is told, with the button.
pub const NOT_A_MEMBER: &str = "PM OS is for the Acme product team.";

const SECTION_MAX: usize = 2900;

fn section(text: &str) -> Value {
    json!({"type": "section", "text": {"type": "mrkdwn", "text": truncate(text, SECTION_MAX)}})
}

fn context(text: &str) -> Value {
    json!({"type": "context", "elements": [{"type": "mrkdwn", "text": truncate(text, SECTION_MAX)}]})
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

/// A button that opens `url` in the browser.
fn link_button(text: &str, action_id: &str, url: &str) -> Value {
    json!({
        "type": "button",
        "text": {"type": "plain_text", "text": text},
        "action_id": action_id,
        "url": url,
    })
}

/// A handle, sha or repo in a code span, or `fallback` if it isn't safe to show as one.
fn code(text: &str, ok: bool, fallback: &str) -> String {
    if ok && !text.is_empty() && !text.contains('`') {
        format!("`{}`", escape(text))
    } else {
        fallback.to_string()
    }
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or_default()
}

/// The first 7 characters of a commit sha, if it is one.
pub fn short_sha(sha: &str) -> Option<String> {
    (sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit())).then(|| sha[..7].to_string())
}

/// The note for someone outside `team.yaml`, with **Request access**.
pub fn request_access_note() -> (String, Value) {
    (
        NOT_A_MEMBER.to_string(),
        json!([
            section(&format!(
                "{NOT_A_MEMBER} If you're on it, ask for access and a platform owner will \
                 review it."
            )),
            {"type": "actions", "block_id": "request_access", "elements": [
                button("Request access", action::REQUEST_ACCESS, "request", Some("primary")),
            ]},
        ]),
    )
}

/// The same note as a slash command's ephemeral acknowledgement.
pub fn request_access_ack() -> Value {
    let (text, blocks) = request_access_note();
    json!({"response_type": "ephemeral", "text": text, "blocks": blocks})
}

/// Who asked: name, email, Slack mention and the proposed handle.
fn request_head(request: &Value) -> Vec<Value> {
    let name = str_of(request, "name");
    let name = if name.trim().is_empty() {
        "Someone".to_string()
    } else {
        escape(&truncate(name, 100))
    };
    let slack_id = str_of(request, "slack_id");
    let mention = if valid_slack_id(slack_id) {
        format!(" (<@{slack_id}>)")
    } else {
        String::new()
    };
    let handle = str_of(request, "proposed_handle");
    vec![
        section(&format!(
            "*Access request:* {name}{mention}\n*Email:* {}\n*Handle:* {}",
            escape(&truncate(str_of(request, "email"), 200)),
            code(handle, valid_handle(handle), "_none proposed_"),
        )),
        context("Approving adds them to `team.yaml` and gives them a personal bot."),
    ]
}

/// A pending request, for a platform owner: **Approve**, **Approve with another handle** and
/// **Reject**, each carrying the request id.
pub fn request_card(request: &Value) -> (String, Value) {
    let id = str_of(request, "id");
    let mut blocks = request_head(request);
    blocks.push(json!({
        "type": "actions",
        "block_id": format!("member_request:{id}"),
        "elements": [
            button("Approve", action::APPROVE, id, Some("primary")),
            button("Approve with another handle", action::EDIT, id, None),
            button("Reject", action::REJECT, id, Some("danger")),
        ],
    }));
    (
        format!(
            "Access request from {}",
            escape(&truncate(str_of(request, "name"), 100))
        ),
        Value::Array(blocks),
    )
}

/// The card once decided: who asked, then `line` in place of the buttons.
pub fn request_closed(request: &Value, line: &str) -> (String, Value) {
    let mut blocks = request_head(request);
    blocks.push(section(line));
    (truncate(line, 300), Value::Array(blocks))
}

/// "✅ Approved by Ana: `ben` added (commit `abc1234`)", "Rejected by Ana", or what went
/// wrong, from the request as the tool server returned it.
pub fn decision_line(request: &Value, decided_by: &str) -> String {
    let who = escape(&person(
        request["decided_by"]
            .as_str()
            .filter(|d| !d.is_empty())
            .unwrap_or(decided_by),
    ));
    let handle = str_of(request, "handle");
    let handle = code(handle, valid_handle(handle), "the new member");
    match str_of(request, "status") {
        "approved" => {
            let commit = short_sha(str_of(request, "commit"))
                .map(|s| format!(" (commit `{s}`)"))
                .unwrap_or_default();
            format!(":white_check_mark: Approved by {who}: {handle} added{commit}")
        }
        "rejected" => format!(":no_entry_sign: Rejected by {who}"),
        "failed" => format!(
            ":warning: Approved by {who}, but adding them failed: {}\nThe request is closed; \
             they can ask again.",
            escape(&truncate(
                request["error"].as_str().unwrap_or("unknown error"),
                500
            ))
        ),
        "pending" => "Still waiting for a decision.".to_string(),
        other => format!("Status: {}", escape(other)),
    }
}

/// The modal for **Approve with another handle**, prefilled with the proposed one.
pub fn handle_modal(request: &Value, channel: &str, ts: &str) -> Value {
    let metadata = json!({"id": request["id"], "channel": channel, "ts": ts});
    let mut input = json!({
        "type": "plain_text_input",
        "action_id": "value",
        "max_length": 24,
    });
    let proposed = str_of(request, "proposed_handle");
    if valid_handle(proposed) {
        input["initial_value"] = json!(proposed);
    }
    json!({
        "type": "modal",
        "callback_id": action::HANDLE_MODAL,
        "title": {"type": "plain_text", "text": "Approve access"},
        "submit": {"type": "plain_text", "text": "Approve"},
        "close": {"type": "plain_text", "text": "Cancel"},
        "private_metadata": metadata.to_string(),
        "blocks": [
            section(&format!(
                "Approve {} with this handle.",
                escape(&truncate(str_of(request, "name"), 100))
            )),
            {
                "type": "input",
                "block_id": HANDLE_BLOCK,
                "label": {"type": "plain_text", "text": "Handle"},
                "hint": {"type": "plain_text", "text": "Lowercase letters, digits and hyphens, up to 24 characters. Their bot is pm-<handle>."},
                "element": input,
            },
        ],
    })
}

/// What a member has set up, for the checklist.
#[derive(Debug, Clone, Default)]
pub struct Setup {
    pub handle: String,
    /// `atlassian`, `slack`, `github`: linked, or the **Connect** link if one could be made.
    pub links: Vec<(&'static str, bool, Option<String>)>,
    pub memory_repo: Option<String>,
    pub bot_ready: bool,
}

impl Setup {
    pub fn done(&self) -> bool {
        self.links.iter().all(|(_, linked, _)| *linked)
            && self.memory_repo.is_some()
            && self.bot_ready
    }
}

const STEP_LABELS: [(&str, &str); 3] = [
    ("atlassian", "*Jira and Confluence*"),
    ("slack", "*Slack* (so drafts post as you)"),
    ("github", "*GitHub* (for your memory)"),
];

/// The setup checklist, with **Connect** links, **Set memory repo** and **Refresh**.
pub fn checklist(setup: &Setup) -> (String, Value) {
    let mut blocks = vec![section("*Set up PM OS*")];
    let mut n = 0;
    for (provider, label) in STEP_LABELS {
        n += 1;
        let (linked, url) = setup
            .links
            .iter()
            .find(|(p, _, _)| *p == provider)
            .map(|(_, l, u)| (*l, u.clone()))
            .unwrap_or((false, None));
        let mut block = if linked {
            section(&format!("{n}. :white_check_mark: {label}"))
        } else if url.is_some() {
            section(&format!("{n}. {label}"))
        } else {
            section(&format!(
                "{n}. {label}: PM OS couldn't make the link right now. Tap *Refresh* in a \
                 minute."
            ))
        };
        if let (false, Some(url)) = (linked, url) {
            block["accessory"] = link_button(
                "Connect",
                &format!("{}{provider}", action::SETUP_LINK),
                &url,
            );
        }
        blocks.push(block);
    }
    let memory = match &setup.memory_repo {
        Some(repo) => section(&format!(
            "4. :white_check_mark: *Memory repo:* {}",
            code(repo, valid_repo(repo), "set")
        )),
        None => {
            let mut b = section("4. *Memory repo*: a private GitHub repo for your bot's notes");
            b["accessory"] = button("Set memory repo", action::SETUP_MEMORY, "memory", None);
            b
        }
    };
    blocks.push(memory);
    let bot = personal_bot_name(&setup.handle);
    let bot = code(&bot, valid_handle(&bot), "your bot");
    blocks.push(if setup.bot_ready {
        section(&format!("5. :white_check_mark: *Your bot* {bot}"))
    } else {
        section(&format!(
            "5. *Your bot* {bot}: being set up (a minute or two)"
        ))
    });
    let trailer = if setup.done() {
        ":tada: *All set.* DM me anything and your bot will take it."
    } else {
        "Tap *Refresh* after each step."
    };
    blocks.push(json!({
        "type": "actions",
        "block_id": "setup",
        "elements": [button("Refresh", action::SETUP_REFRESH, "refresh", None)],
    }));
    blocks.push(context(trailer));
    let text = if setup.done() {
        "PM OS setup: all set".to_string()
    } else {
        "PM OS setup checklist".to_string()
    };
    (text, Value::Array(blocks))
}

/// `org/name` as GitHub allows them: letters, digits, `-`, `_` and `.`, at most 100 each.
pub fn valid_repo(repo: &str) -> bool {
    let ok = |s: &str| {
        !s.is_empty()
            && s.len() <= 100
            && s != "."
            && s != ".."
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    matches!(repo.split_once('/'), Some((org, name)) if ok(org) && ok(name))
}

/// The memory-repo modal, prefilled with `<org>/pm-os-memory-<handle>` when the org is
/// configured. `channel` and `ts` are the checklist's, to refresh it after.
pub fn memory_modal(handle: &str, org: Option<&str>, channel: &str, ts: &str) -> Value {
    let suggested = format!("pm-os-memory-{handle}");
    let mut input = json!({
        "type": "plain_text_input",
        "action_id": "value",
        "max_length": 201,
    });
    match org.filter(|o| valid_repo(&format!("{o}/x"))) {
        Some(org) => input["initial_value"] = json!(format!("{org}/{suggested}")),
        None => {
            input["placeholder"] = json!({"type": "plain_text", "text": format!("org/{suggested}")})
        }
    }
    json!({
        "type": "modal",
        "callback_id": action::MEMORY_MODAL,
        "title": {"type": "plain_text", "text": "Memory repo"},
        "submit": {"type": "plain_text", "text": "Save"},
        "close": {"type": "plain_text", "text": "Cancel"},
        "private_metadata": json!({"channel": channel, "ts": ts}).to_string(),
        "blocks": [
            section("Your bot keeps its notes about your work in this repo, one file per note."),
            {
                "type": "input",
                "block_id": REPO_BLOCK,
                "label": {"type": "plain_text", "text": "Repo (org/name)"},
                "element": input,
            },
            context("Create it in the Acme org as a *private* repo first."),
        ],
    })
}

/// A memory-repo refusal as a plain sentence, for the modal's field error.
pub fn memory_error(code: Option<&str>, error: &str, org: Option<&str>) -> String {
    let org = org.unwrap_or("Acme");
    match code {
        Some("not_in_org") => format!("That repo isn't in the {org} GitHub org. Create it there."),
        Some("not_private") => {
            "That repo is public. Make it private, or create a private one.".into()
        }
        Some("archived") => "That repo is archived. Unarchive it or pick another.".into(),
        Some("no_push") => "Your GitHub account can't push to that repo. Ask for write \
                            access, or use a repo you created."
            .into(),
        Some("not_found") => "GitHub can't find that repo, or your account can't see it. \
                              Check the name."
            .into(),
        Some("github_not_linked") => "Connect GitHub first (step 3), then set the repo.".into(),
        _ if !error.trim().is_empty() => truncate(error, 300),
        _ => "PM OS couldn't save that. Try again.".into(),
    }
}

/// The confirmation for `remove <handle>`.
pub fn remove_confirm(handle: &str, slack_id: &str) -> (String, Value) {
    let mention = if valid_slack_id(slack_id) {
        format!(" (<@{slack_id}>)")
    } else {
        String::new()
    };
    let shown = code(handle, valid_handle(handle), "this member");
    (
        format!("Remove {} from PM OS?", escape(handle)),
        json!([
            section(&format!(
                "*Remove {shown}{mention} from PM OS?*\nThis commits `team.yaml` without them, \
                 deletes their bot, unlinks their accounts and forgets their memory repo. The \
                 repo itself stays theirs."
            )),
            {"type": "actions", "block_id": "member_remove", "elements": [
                button("Remove", action::REMOVE_CONFIRM, handle, Some("danger")),
                button("Cancel", action::REMOVE_CANCEL, handle, None),
            ]},
        ]),
    )
}

/// The removal card with a line in place of its buttons.
pub fn remove_closed(handle: &str, line: &str) -> (String, Value) {
    let shown = code(handle, valid_handle(handle), "this member");
    (
        truncate(line, 300),
        json!([
            section(&format!("*Remove {shown} from PM OS*")),
            section(line)
        ]),
    )
}

/// The removal card with its buttons back and a warning.
pub fn remove_retry(handle: &str, slack_id: &str, warning: &str) -> (String, Value) {
    let (text, blocks) = remove_confirm(handle, slack_id);
    let mut blocks = blocks.as_array().cloned().unwrap_or_default();
    blocks.push(context(&format!(":warning: {}", escape(warning))));
    (text, Value::Array(blocks))
}

/// "Removed `ben` (commit `abc1234`)".
pub fn removed_line(handle: &str, by: &str, commit: &str) -> String {
    let commit = short_sha(commit)
        .map(|s| format!(" (commit `{s}`)"))
        .unwrap_or_default();
    format!(
        ":white_check_mark: Removed {} by {}{commit}",
        code(handle, valid_handle(handle), "the member"),
        escape(&person(by))
    )
}

/// The welcome DM for a new member.
pub fn welcome(name: &str, handle: &str, by: &str) -> (String, Value) {
    let first = name.split_whitespace().next().unwrap_or_default();
    let greeting = if first.is_empty() {
        "Welcome to PM OS!".to_string()
    } else {
        format!("Welcome to PM OS, {}!", escape(&truncate(first, 60)))
    };
    let text = format!(
        "{greeting} {} approved you as {}. A few steps and your own bot is ready; the checklist \
         is below. Say `setup` any time to see it again.",
        escape(&person(by)),
        code(handle, valid_handle(handle), "a member"),
    );
    (truncate(&text, 300), json!([section(&text)]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> Value {
        json!({
            "id": "0d3f6c1e-0000-4000-8000-000000000001", "slack_id": "U0BEN",
            "email": "ben<x>@acme.com", "name": "Ben *Bold* <script>",
            "proposed_handle": "ben", "status": "pending",
        })
    }

    #[test]
    fn request_cards_escape_and_carry_the_id() {
        let (_, blocks) = request_card(&request());
        let s = blocks.to_string();
        assert!(s.contains("Ben *Bold* &lt;script&gt;"), "{s}");
        assert!(s.contains("ben&lt;x&gt;@acme.com"));
        assert!(s.contains("(<@U0BEN>)"));
        assert!(s.contains("`ben`"));
        for a in [action::APPROVE, action::EDIT, action::REJECT] {
            assert!(s.contains(a));
        }
        assert_eq!(
            blocks[2]["elements"][0]["value"],
            "0d3f6c1e-0000-4000-8000-000000000001"
        );
        // A bad Slack id is not made into a mention, a bad handle not into code.
        let mut odd = request();
        odd["slack_id"] = json!("<!channel>");
        odd["proposed_handle"] = json!("x`y");
        let s = request_card(&odd).1.to_string();
        assert!(!s.contains("<!channel>"));
        assert!(s.contains("_none proposed_"));
    }

    #[test]
    fn decision_lines() {
        let mut r = request();
        r["status"] = json!("approved");
        r["handle"] = json!("ben");
        r["decided_by"] = json!("ana");
        r["commit"] = json!("abc1234def5678");
        assert_eq!(
            decision_line(&r, "x"),
            ":white_check_mark: Approved by Ana: `ben` added (commit `abc1234`)"
        );
        r["status"] = json!("rejected");
        assert_eq!(decision_line(&r, "x"), ":no_entry_sign: Rejected by Ana");
        r["status"] = json!("failed");
        r["error"] = json!("GitHub said <no>");
        assert!(decision_line(&r, "x").contains("GitHub said &lt;no&gt;"));
    }

    #[test]
    fn the_checklist_in_each_state() {
        let mut setup = Setup {
            handle: "ben".into(),
            links: vec![
                ("atlassian", true, None),
                (
                    "slack",
                    false,
                    Some("https://pmos.example.com/oauth/slack?s=1".into()),
                ),
                ("github", false, None),
            ],
            memory_repo: None,
            bot_ready: false,
        };
        let s = checklist(&setup).1.to_string();
        assert!(s.contains("1. :white_check_mark: *Jira and Confluence*"));
        assert!(s.contains("https://pmos.example.com/oauth/slack?s=1"));
        assert!(s.contains("couldn't make the link"));
        assert!(s.contains(action::SETUP_MEMORY));
        assert!(s.contains("being set up (a minute or two)"));
        assert!(s.contains(action::SETUP_REFRESH));
        assert!(!s.contains("All set"));
        setup.links = PROVIDERS_LINKED.to_vec();
        setup.memory_repo = Some("acme/pm-os-memory-ben".into());
        setup.bot_ready = true;
        let s = checklist(&setup).1.to_string();
        assert!(s.contains("`acme/pm-os-memory-ben`"));
        assert!(s.contains("5. :white_check_mark: *Your bot* `pm-ben`"));
        assert!(s.contains("All set"));
        assert!(!s.contains(action::SETUP_MEMORY));
    }

    const PROVIDERS_LINKED: [(&str, bool, Option<String>); 3] = [
        ("atlassian", true, None),
        ("slack", true, None),
        ("github", true, None),
    ];

    #[test]
    fn memory_modal_and_errors() {
        let m = memory_modal("ben", Some("acme"), "D1", "1.0");
        assert_eq!(
            m["blocks"][1]["element"]["initial_value"],
            "acme/pm-os-memory-ben"
        );
        assert!(m.to_string().contains("as a *private* repo first"));
        let m = memory_modal("ben", None, "D1", "1.0");
        assert!(m["blocks"][1]["element"]["initial_value"].is_null());
        assert!(valid_repo("acme/pm-os-memory-ben"));
        assert!(!valid_repo("acme"));
        assert!(!valid_repo("a/b/c"));
        assert!(!valid_repo("a/.."));
        assert!(memory_error(Some("not_private"), "", None).contains("public"));
        assert!(memory_error(Some("not_in_org"), "", Some("acme")).contains("acme"));
        assert_eq!(memory_error(None, "odd", None), "odd");
    }
}
