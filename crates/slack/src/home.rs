//! The App Home (`docs/slack-mentions-and-home-spec.md` §4): one view with three sections,
//! switched by buttons.
//!
//! - **Inbox:** the member's pending drafts (`GET /internal/approvals?user=`), the 10 oldest
//!   first, each with a tick box. **Approve selected** and **Reject selected** decide the
//!   ticked ones, one at a time, through the same decision call as the cards; the view then
//!   says how each one ended. Editing, Snooze and confirming an external channel stay on the
//!   card in the DM.
//! - **Skills:** the member's level ("You're set to the *Staff* level …", left out when the
//!   runtime doesn't answer), then the skills with the `slack` surface that the member's
//!   personal bot's role lists (the Docs bot's without one): "Start here" with three suggested
//!   skills, then the rest grouped by the job they do (`GROUPS`; a group over six skills has
//!   **Show all in …**).
//! - **Accounts:** Jira and Confluence, Slack and GitHub, each Connected (**Disconnect**) or
//!   Not connected (**Connect**), and the memory repo.
//!
//! Every action republishes the view (`views.publish`); nothing here uses `chat.update`. The
//! section shown is kept in the view's `private_metadata` (`{"tab": …}`), and the process
//! remembers who has opened the Home so a new or closed approval refreshes it. A notice
//! (what a batch decision did) lives only in the publish that shows it.
//!
//! Everything from a draft, a skill or another service is escaped before it goes in a block.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use pmos_contract::lint::{lint, LintOptions};
use pmos_contract::model::{valid_handle, Skill};
use pmos_contract::onboarding::personal_bot_name;
use serde_json::{json, Value};

use crate::blocks::{escape, truncate};
use crate::bots::{Bots, DEFAULT_BOT};
use crate::bounded::Bounded;
use crate::cards;
use crate::commands::FollowUp;
use crate::member_cards::valid_repo;
use crate::runtime::Runtime;
use crate::toolserver::{approval_id_ok, ApiError, Seen, PROVIDERS};

/// Action ids in the Home, and the tick boxes' block-id prefix.
pub mod action {
    pub const TAB_INBOX: &str = "home_tab_inbox";
    pub const TAB_SKILLS: &str = "home_tab_skills";
    pub const TAB_ACCOUNTS: &str = "home_tab_accounts";
    /// "Show all in <group>": the value is the group's name.
    pub const SKILLS_GROUP: &str = "home_skills_group";
    pub const APPROVE: &str = "home_approve";
    pub const REJECT: &str = "home_reject";
    pub const REFRESH: &str = "home_refresh";
    /// A draft's tick box. A tick fires `block_actions`; it is acked and ignored.
    pub const PICK: &str = "home_pick";
    /// The tick box's block id is this prefix and the approval id.
    pub const PICK_BLOCK: &str = "pick_";
    /// **Disconnect** is this prefix and the provider: `home_unlink_slack`.
    pub const UNLINK_PREFIX: &str = "home_unlink_";
    /// **Connect** (a link button) is this prefix and the provider; its click is ignored.
    pub const CONNECT_PREFIX: &str = "home_connect_";
}

pub const NOT_A_MEMBER: &str = "PM OS is for the product team. DM me to ask for access.";
pub const TICK_FIRST: &str = "Tick the drafts first.";
const FOOTER: &str = "To edit or snooze a draft, use its card in our DM.";
const SKILLS_HOW: &str =
    "DM me and ask in your own words, or name a skill: `strategy-doc: our plan for small teams`";

/// Slack's limits: 100 blocks in a Home view, 3,000 characters in a section.
const MAX_BLOCKS: usize = 100;
const SECTION_MAX: usize = 2900;
/// How many drafts the Inbox lists.
const INBOX_ROWS: usize = 10;
const PREVIEW_MAX: usize = 200;
const SKILL_LINE_MAX: usize = 150;
/// How long the skills catalog is reused.
const CATALOG_TTL: Duration = Duration::from_secs(60);

/// The groups the Skills section shows, in order, and the `track`s that land in each. A skill
/// goes to the group of its `track`, unless `GROUP_OVERRIDES` names it. A skill with neither
/// goes to `MORE`.
pub const GROUPS: [(&str, &[&str]); 7] = [
    ("Everyday", &["Everyday"]),
    (
        "Understand and decide",
        &[
            "Product sense",
            "Customer and market",
            "Strategy and decisions",
        ],
    ),
    (
        "Write a document",
        &["Spec and review", "AI products", "Company bets"],
    ),
    (
        "Plan and prioritise",
        &["Metrics and experiments", "Leading across teams"],
    ),
    ("Launch", &["Delivery and launch"]),
    ("Review and coach", &[]),
    ("Communicate", &[]),
];
/// The group for skills that fit none of the above.
pub const MORE: &str = "More";
/// Skills whose job differs from their track's usual one.
const GROUP_OVERRIDES: [(&str, &str); 11] = [
    ("prioritize", "Plan and prioritise"),
    ("pre-mortem", "Review and coach"),
    ("review", "Review and coach"),
    ("experiment-readout", "Review and coach"),
    ("business-review", "Review and coach"),
    ("coach-feedback", "Review and coach"),
    ("status-update", "Communicate"),
    ("exec-brief", "Communicate"),
    ("alignment-plan", "Communicate"),
    ("partner-brief", "Communicate"),
    ("platform-strategy", "Plan and prioritise"),
];
/// How many skills a group lists before "Show all in <group>".
const GROUP_PREVIEW: usize = 6;

/// "Start here": the skills a new PM gets the most from, in the order they are offered, with
/// what you get and what to type. The first three that are in the PM's catalog are shown, so
/// the later ones are stand-ins when an earlier one is missing. The rule: work almost every
/// PM does in their first week. Edit this list to change the suggestions.
pub const START_HERE: [(&str, &str, &str); 6] = [
    (
        "ask",
        "A sourced answer to a question about our products, customers or past decisions.",
        "ask: what did we decide about pricing for small teams?",
    ),
    (
        "meeting-notes",
        "Decisions, owners and follow-ups from a call, ready for you to approve.",
        "meeting-notes: the notes from today's customer call",
    ),
    (
        "status-update",
        "A one-page update for stakeholders, drawn from Jira and Confluence.",
        "status-update: this week for the checkout project",
    ),
    (
        "prd-draft",
        "A first draft of a PRD from your brief, with sources.",
        "prd-draft: bulk export for admins",
    ),
    (
        "problem-framing",
        "A clear problem statement before anyone picks a solution.",
        "problem-framing: customers keep asking for dark mode",
    ),
    (
        "slack-reply",
        "Draft replies to the questions waiting for you in Slack.",
        "slack-reply: catch me up on what I missed",
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Inbox,
    Skills,
    Accounts,
}

impl Tab {
    pub fn code(self) -> &'static str {
        match self {
            Tab::Inbox => "inbox",
            Tab::Skills => "skills",
            Tab::Accounts => "accounts",
        }
    }

    pub fn parse(code: &str) -> Option<Self> {
        match code {
            "inbox" => Some(Tab::Inbox),
            "skills" => Some(Tab::Skills),
            "accounts" => Some(Tab::Accounts),
            _ => None,
        }
    }

    /// The section a view's `private_metadata` names, or the Inbox.
    pub fn of_metadata(metadata: Option<&str>) -> Self {
        metadata
            .and_then(|m| serde_json::from_str::<Value>(m).ok())
            .and_then(|m| m["tab"].as_str().and_then(Tab::parse))
            .unwrap_or(Tab::Inbox)
    }
}

/// What the Home remembers: who has opened it (and on which section), and the skills
/// catalog for a minute.
pub struct Home {
    viewers: Mutex<Bounded<String, Tab>>,
    catalog: Mutex<Option<(Instant, Vec<Skill>)>>,
    /// The Skills group a viewer opened with "Show all in …", by Slack user id.
    expanded: Mutex<Bounded<String, String>>,
}

impl Default for Home {
    fn default() -> Self {
        Self {
            viewers: Mutex::new(Bounded::new(2000)),
            catalog: Mutex::new(None),
            expanded: Mutex::new(Bounded::new(2000)),
        }
    }
}

fn section(text: &str) -> Value {
    json!({"type": "section", "text": {"type": "mrkdwn", "text": truncate(text, SECTION_MAX)}})
}

fn context(text: &str) -> Value {
    json!({"type": "context", "elements": [{"type": "mrkdwn", "text": truncate(text, SECTION_MAX)}]})
}

fn header(text: &str) -> Value {
    json!({"type": "header", "text": {"type": "plain_text", "text": truncate(text, 140)}})
}

fn button(text: &str, action_id: &str, value: &str, style: Option<&str>) -> Value {
    let mut b = json!({
        "type": "button",
        "text": {"type": "plain_text", "text": truncate(text, 70)},
        "action_id": action_id,
        "value": value,
    });
    if let Some(style) = style {
        b["style"] = json!(style);
    }
    b
}

fn confirm(title: &str, text: &str, yes: &str, no: &str) -> Value {
    json!({
        "title": {"type": "plain_text", "text": truncate(title, 95)},
        "text": {"type": "plain_text", "text": truncate(text, 295)},
        "confirm": {"type": "plain_text", "text": yes},
        "deny": {"type": "plain_text", "text": no},
    })
}

/// The whole view: the header, the section buttons, a notice if there is one, the section.
pub fn view(tab: Tab, notice: Option<&str>, body: Vec<Value>) -> Value {
    let tabs = [
        ("Inbox", action::TAB_INBOX, Tab::Inbox),
        ("Skills", action::TAB_SKILLS, Tab::Skills),
        ("Accounts", action::TAB_ACCOUNTS, Tab::Accounts),
    ];
    let elements: Vec<Value> = tabs
        .iter()
        .map(|(label, id, t)| button(label, id, t.code(), (*t == tab).then_some("primary")))
        .collect();
    let mut blocks = vec![
        header("PM OS"),
        json!({"type": "actions", "block_id": "home_tabs", "elements": elements}),
    ];
    if let Some(notice) = notice.filter(|n| !n.trim().is_empty()) {
        blocks.push(section(notice));
    }
    blocks.extend(body);
    if blocks.len() > MAX_BLOCKS {
        blocks.truncate(MAX_BLOCKS - 1);
        blocks.push(context("_There's more than fits here._"));
    }
    json!({
        "type": "home",
        "private_metadata": json!({"tab": tab.code()}).to_string(),
        "blocks": blocks,
    })
}

/// The view for someone outside `team.yaml`.
pub fn not_a_member_view() -> Value {
    json!({
        "type": "home",
        "private_metadata": json!({"tab": Tab::Inbox.code()}).to_string(),
        "blocks": [header("PM OS"), section(NOT_A_MEMBER)],
    })
}

// ---- Inbox ----

/// "a Jira issue in `PM`", "Reply drafted for <#C…>": what a draft is and where it goes, in
/// the cards' words, as safe `mrkdwn`.
pub fn item_title(item: &Value) -> String {
    let tool = item["tool"].as_str().unwrap_or("a write");
    let args = &item["args"];
    let code = |key: &str| {
        args[key]
            .as_str()
            .filter(|v| {
                !v.is_empty()
                    && v.len() <= 64
                    && v.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_~".contains(c))
            })
            .map(|v| format!(" in `{v}`"))
            .unwrap_or_default()
    };
    match tool {
        cards::REPLY_TOOL => cards::reply_title(item),
        "slack.send_message" => {
            let to = args["channel"].as_str().unwrap_or_default();
            let place = if pmos_contract::model::valid_slack_id(to) {
                format!("<@{to}>")
            } else {
                cards::reply_place(item)
            };
            format!("{} to {place}", escape(&cards::tool_label(tool)))
        }
        "jira.create_issue" => format!("{}{}", escape(&cards::tool_label(tool)), code("project")),
        "confluence.create_draft" => {
            format!("{}{}", escape(&cards::tool_label(tool)), code("space"))
        }
        _ => escape(&cards::tool_label(tool)),
    }
}

/// "just now", "5 minutes ago", "3 hours ago", "2 days ago".
pub fn ago(created_at: Option<&str>, now: DateTime<Utc>) -> Option<String> {
    let at = DateTime::parse_from_rfc3339(created_at?).ok()?;
    let mins = now.signed_duration_since(at).num_minutes().max(0);
    let plural = |n: i64, unit: &str| {
        if n == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{n} {unit}s ago")
        }
    };
    Some(match mins {
        0 => "just now".into(),
        1..=59 => plural(mins, "minute"),
        60..=1439 => plural(mins / 60, "hour"),
        _ => plural(mins / 1440, "day"),
    })
}

/// The bot that drafted an item: its `bot`, or a `bot:<name>` skill.
fn drafted_by(item: &Value) -> Option<&str> {
    item["bot"]
        .as_str()
        .or_else(|| item["skill"].as_str().and_then(|s| s.strip_prefix("bot:")))
        .filter(|b| !b.is_empty())
}

/// The draft's text, on one line and cut to 200 characters, escaped.
fn preview(item: &Value) -> Option<String> {
    let (_, text) = cards::editable_fields(item)
        .into_iter()
        .find(|(f, v)| matches!(*f, "text" | "body" | "description") && !v.trim().is_empty())
        .or_else(|| cards::editable_fields(item).into_iter().next())?;
    if text == "[expired]" {
        return None;
    }
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(escape(&truncate(&one_line, PREVIEW_MAX)))
}

/// One draft's row, with its tick box.
pub fn inbox_row(item: &Value, display: &dyn Fn(&str) -> String, now: DateTime<Utc>) -> Value {
    let id = item["id"].as_str().unwrap_or_default();
    let mut meta = Vec::new();
    if let Some(bot) = drafted_by(item) {
        meta.push(format!("by {}", escape(&display(bot))));
    }
    if let Some(ago) = ago(item["created_at"].as_str(), now) {
        meta.push(ago);
    }
    let mut text = format!("*{}*", item_title(item));
    if !meta.is_empty() {
        text.push('\n');
        text.push_str(&meta.join(" · "));
    }
    if let Some(p) = preview(item) {
        text.push_str("\n> ");
        text.push_str(&p);
    }
    let mut row = section(&text);
    // The block id carries the start of the args hash of what this row showed, so **Approve
    // selected** approves exactly that (R5): `pick_<id>:<hash>`.
    row["block_id"] = json!(match crate::cards::shown_hash(item) {
        Some(hash) => format!("{}{id}:{hash}", action::PICK_BLOCK),
        None => format!("{}{id}", action::PICK_BLOCK),
    });
    row["accessory"] = json!({
        "type": "checkboxes",
        "action_id": action::PICK,
        "options": [{"text": {"type": "plain_text", "text": "Select"}, "value": id}],
    });
    row
}

/// The Inbox: the count, the buttons, the 10 oldest drafts and the footer.
pub fn inbox(
    items: &[Value],
    total: usize,
    display: &dyn Fn(&str) -> String,
    now: DateTime<Utc>,
) -> Vec<Value> {
    let refresh = button("Refresh", action::REFRESH, "refresh", None);
    let rows: Vec<&Value> = items
        .iter()
        .filter(|i| i["id"].as_str().is_some_and(approval_id_ok))
        .take(INBOX_ROWS)
        .collect();
    if rows.is_empty() {
        return vec![
            section("Nothing is waiting for you."),
            json!({"type": "actions", "block_id": "home_inbox", "elements": [refresh]}),
            context(FOOTER),
        ];
    }
    let total = total.max(rows.len());
    let top = if total == 1 {
        "*1 draft waiting for you*".to_string()
    } else {
        format!("*{total} drafts waiting for you*")
    };
    let mut approve = button(
        "Approve selected",
        action::APPROVE,
        "approve",
        Some("primary"),
    );
    approve["confirm"] = confirm(
        "Approve selected",
        "Approve the selected drafts? Each one is created or posted as you.",
        "Approve",
        "Cancel",
    );
    let mut blocks = vec![
        section(&top),
        json!({"type": "actions", "block_id": "home_inbox", "elements": [
            approve,
            button("Reject selected", action::REJECT, "reject", Some("danger")),
            refresh,
        ]}),
    ];
    blocks.extend(rows.iter().map(|i| inbox_row(i, display, now)));
    if total > rows.len() {
        blocks.push(context(&format!(
            "and {} more; they appear as you clear these.",
            total - rows.len()
        )));
    }
    blocks.push(context(FOOTER));
    blocks
}

/// One ticked draft: its approval id and the start of the args hash its row showed (`None`
/// for a row from before rows carried it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticked {
    pub id: String,
    pub shown: Option<String>,
}

/// The approval ids ticked in a Home view's `state.values`, each checked, with what each row
/// showed.
pub fn ticked(state_values: &Value) -> Vec<Ticked> {
    let mut out: Vec<Ticked> = Vec::new();
    let Some(blocks) = state_values.as_object() else {
        return out;
    };
    for (block_id, actions) in blocks {
        let Some(rest) = block_id.strip_prefix(action::PICK_BLOCK) else {
            continue;
        };
        let (id, shown) = match rest.split_once(':') {
            Some((id, hash))
                if hash.len() == crate::cards::SHOWN_HASH_LEN
                    && hash.chars().all(|c| c.is_ascii_hexdigit()) =>
            {
                (id, Some(hash.to_string()))
            }
            Some(_) => continue,
            None => (rest, None),
        };
        let picked = actions[action::PICK]["selected_options"]
            .as_array()
            .is_some_and(|o| o.iter().any(|o| o["value"] == id));
        if picked && approval_id_ok(id) && !out.iter().any(|t| t.id == id) {
            out.push(Ticked {
                id: id.to_string(),
                shown,
            });
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// How each decided draft ended, counted for the notice.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub approved: usize,
    pub rejected: usize,
    pub blocked: usize,
    pub confirm: usize,
    pub closed: usize,
    /// Drafts whose text changed since the Home showed them (or whose row didn't say what it
    /// showed): skipped, nothing approved.
    pub changed: usize,
    /// One reason per failed draft.
    pub failed: Vec<String>,
}

impl Tally {
    /// Count one decision's outcome.
    pub fn add(&mut self, outcome: &Result<Value, ApiError>) {
        match outcome {
            Ok(item) => match item["status"].as_str().unwrap_or_default() {
                "approved" | "executing" => self.approved += 1,
                "rejected" => self.rejected += 1,
                "failed" => self.failed.push(
                    item["result"]["error"]
                        .as_str()
                        .unwrap_or("the write failed")
                        .to_string(),
                ),
                _ => self.closed += 1,
            },
            Err(ApiError::Unavailable) => self
                .failed
                .push("PM OS couldn't reach the tool server".into()),
            Err(ApiError::Refused { code, .. }) if code == "audience_blocked" => self.blocked += 1,
            Err(ApiError::Refused { code, .. }) if code == "needs_external_confirm" => {
                self.confirm += 1
            }
            Err(ApiError::Refused { code, .. }) if code == "stale" => self.changed += 1,
            Err(ApiError::Refused { code, status, .. }) if code == "expired" || *status == 409 => {
                self.closed += 1
            }
            Err(ApiError::Refused { code, .. }) if code == "audience_unavailable" => self
                .failed
                .push("PM OS couldn't check who is in the channel".into()),
            Err(ApiError::Refused { error, .. }) => self.failed.push(if error.is_empty() {
                "refused".into()
            } else {
                error.clone()
            }),
        }
    }

    /// The notice: one line per kind of outcome, in plain words.
    pub fn notice(&self) -> String {
        let mut lines = Vec::new();
        if self.approved > 0 {
            lines.push(format!(
                ":white_check_mark: {} approved or posted.",
                self.approved
            ));
        }
        if self.rejected > 0 {
            lines.push(format!(":no_entry_sign: {} rejected.", self.rejected));
        }
        if self.blocked > 0 {
            lines.push(format!(
                ":no_entry: {} blocked: built from restricted sources, and people in the channel \
                 aren't on the product team. Reject it, or answer in the thread yourself.",
                self.blocked
            ));
        }
        if self.confirm > 0 {
            let verb = if self.confirm == 1 { "needs" } else { "need" };
            lines.push(format!(
                ":warning: {} {verb} your confirmation: it goes to a channel shared outside the \
                 company. Confirm on its card in our DM.",
                self.confirm
            ));
        }
        if self.changed > 0 {
            lines.push(format!(
                ":pencil2: {} changed since you looked, so {} not approved. Open {} to review.",
                self.changed,
                if self.changed == 1 {
                    "it was"
                } else {
                    "they were"
                },
                if self.changed == 1 { "it" } else { "each" },
            ));
        }
        if self.closed > 0 {
            lines.push(format!(
                ":hourglass: {} expired, or already decided.",
                self.closed
            ));
        }
        if !self.failed.is_empty() {
            let mut reasons: Vec<String> = Vec::new();
            for r in &self.failed {
                let r = escape(&truncate(r, 200));
                if !reasons.contains(&r) {
                    reasons.push(r);
                }
            }
            reasons.truncate(3);
            lines.push(format!(
                ":x: {} failed: {}.",
                self.failed.len(),
                reasons.join("; ")
            ));
        }
        lines.join("\n")
    }
}

// ---- Skills ----

/// The group a skill belongs to: its override, else its track's group, else "More".
pub fn group_of(name: &str, track: Option<&str>) -> &'static str {
    if let Some((_, g)) = GROUP_OVERRIDES.iter().find(|(n, _)| *n == name) {
        return g;
    }
    let track = track.map(str::trim).unwrap_or_default();
    GROUPS
        .iter()
        .find(|(_, tracks)| tracks.iter().any(|t| t.eq_ignore_ascii_case(track)))
        .map_or(MORE, |(g, _)| g)
}

/// Whether `group` is one the Skills section can show.
fn known_group(group: &str) -> bool {
    group == MORE || GROUPS.iter().any(|(g, _)| *g == group)
}

/// A description's first sentence, cut to 150 characters.
pub fn first_sentence(description: &str) -> String {
    let d = description.trim();
    let end = d
        .match_indices(". ")
        .next()
        .map(|(i, _)| i + 1)
        .unwrap_or(d.len());
    truncate(&d[..end], SKILL_LINE_MAX)
}

/// The Skills section: the PM's level (when the runtime said), how to use them, "Start here"
/// with three suggestions, then the rest by the job they do. A group over `GROUP_PREVIEW`
/// skills lists the first few and a **Show all in <group>** button, unless it is `expanded`.
/// `allowed` is the role's skills; `None` when they couldn't be loaded (every Slack skill is
/// shown, with a note).
pub fn skills(
    catalog: &[(String, String, Option<String>)],
    allowed: Option<&[String]>,
    level: Option<&str>,
    expanded: Option<&str>,
) -> Vec<Value> {
    let mut blocks = Vec::new();
    if let Some(level) = level {
        blocks.push(section(&crate::level::home_line(level)));
    }
    blocks.push(section(SKILLS_HOW));
    if allowed.is_none() {
        blocks.push(context(
            "PM OS couldn't load which skills your bot has, so this lists every skill for Slack.",
        ));
    }
    let mut shown: Vec<&(String, String, Option<String>)> = catalog
        .iter()
        .filter(|(name, _, _)| allowed.is_none_or(|a| a.iter().any(|s| s == name)))
        .collect();
    if shown.is_empty() {
        blocks.push(section("No skills are available to your bot in Slack yet."));
        return blocks;
    }
    shown.sort_by(|a, b| a.0.cmp(&b.0));

    let start: Vec<String> = START_HERE
        .iter()
        .filter(|(name, _, _)| shown.iter().any(|(n, _, _)| n == name))
        .take(3)
        .map(|(name, gets, example)| format!("*{name}*: {gets} Try `{example}`"))
        .collect();
    if !start.is_empty() {
        blocks.push(header("Start here"));
        blocks.extend(cards::sections(&start.join("\n")));
    }

    let group_names = GROUPS.iter().map(|(g, _)| *g).chain([MORE]);
    for group in group_names {
        let members: Vec<&&(String, String, Option<String>)> = shown
            .iter()
            .filter(|(name, _, track)| group_of(name, track.as_deref()) == group)
            .collect();
        if members.is_empty() {
            continue;
        }
        let open = expanded == Some(group);
        let limit = if open { members.len() } else { GROUP_PREVIEW };
        let lines: Vec<String> = members
            .iter()
            .take(limit)
            .map(|(name, description, _)| {
                format!(
                    "*{}* {}",
                    escape(name),
                    escape(&first_sentence(description))
                )
            })
            .collect();
        blocks.push(header(group));
        blocks.extend(cards::sections(&lines.join("\n")));
        if members.len() > limit {
            blocks.push(json!({"type": "actions", "elements": [
                button(
                    &format!("Show all in {group} ({})", members.len()),
                    action::SKILLS_GROUP,
                    group,
                    None,
                ),
            ]}));
        }
    }
    blocks
}

// ---- Accounts ----

struct Account {
    provider: &'static str,
    name: &'static str,
    uses: &'static str,
    stops: &'static str,
}

const ACCOUNTS: [Account; 3] = [
    Account {
        provider: "atlassian",
        name: "Jira and Confluence",
        uses: "Your bot searches and reads Jira and Confluence as you, and drafts issues and \
               pages for you to approve.",
        stops: "Searching, reading and drafting in Jira and Confluence stop until you connect \
                again.",
    },
    Account {
        provider: "slack",
        name: "Slack",
        uses: "Your bot reads threads and searches Slack as you, and posts the replies you \
               approve, as you.",
        stops: "The reply assistant, tagging and thread reading stop until you connect again.",
    },
    Account {
        provider: "github",
        name: "GitHub",
        uses: "Your bot keeps its notes in your memory repo.",
        stops: "Your bot can't read or save its notes until you connect again.",
    },
];

/// The name of a provider's account: `atlassian` → "Jira and Confluence".
pub fn account_name(provider: &str) -> &'static str {
    ACCOUNTS
        .iter()
        .find(|a| a.provider == provider)
        .map_or("that account", |a| a.name)
}

/// The Accounts section, from the onboarding answer and a **Connect** link for each account
/// that isn't linked (`None` if one couldn't be made).
pub fn accounts(state: &Value, links: &[(&str, Option<String>)]) -> Vec<Value> {
    let mut blocks = Vec::new();
    for account in &ACCOUNTS {
        let linked = &state["linked"][account.provider];
        if account.provider == "github" && linked.is_null() {
            continue;
        }
        let mut block = if linked.as_bool() == Some(true) {
            let mut b = section(&format!(
                ":white_check_mark: *{}* · Connected\n{}",
                account.name, account.uses
            ));
            let mut disconnect = button(
                "Disconnect",
                &format!("{}{}", action::UNLINK_PREFIX, account.provider),
                account.provider,
                Some("danger"),
            );
            disconnect["confirm"] = confirm(
                &format!("Disconnect {}?", account.name),
                account.stops,
                "Disconnect",
                "Keep it",
            );
            b["accessory"] = disconnect;
            b
        } else {
            let url = links
                .iter()
                .find(|(p, _)| *p == account.provider)
                .and_then(|(_, u)| u.as_deref());
            match url {
                Some(url) => {
                    let mut b = section(&format!(
                        "*{}* · Not connected\n{}",
                        account.name, account.uses
                    ));
                    b["accessory"] = json!({
                        "type": "button",
                        "text": {"type": "plain_text", "text": "Connect"},
                        "action_id": format!("{}{}", action::CONNECT_PREFIX, account.provider),
                        "url": url,
                        "style": "primary",
                    });
                    b
                }
                None => section(&format!(
                    "*{}* · Not connected\n{} PM OS couldn't make the link right now. Tap \
                     *Refresh* in a minute.",
                    account.name, account.uses
                )),
            }
        };
        block["block_id"] = json!(format!("account_{}", account.provider));
        blocks.push(block);
    }
    if let Some(repo) = state["memory_repo"]
        .as_str()
        .filter(|r| valid_repo(r) && !r.contains('`'))
    {
        blocks.push(section(&format!("*Memory repo:* `{repo}`")));
    }
    blocks.push(
        json!({"type": "actions", "block_id": "home_accounts", "elements": [
            button("Refresh", action::REFRESH, "refresh", None),
        ]}),
    );
    blocks.push(context(
        "A Connect link lasts 10 minutes; Refresh makes new ones.",
    ));
    blocks
}

// ---- Wiring ----

impl Bots {
    /// `app_home_opened` on the Home tab: publish the view, on the section it was left on.
    pub(crate) fn home_opened(&self, payload: &Value) -> Option<FollowUp> {
        let event = &payload["event"];
        if event["tab"] != "home" {
            return None;
        }
        let user = event["user"]
            .as_str()
            .filter(|u| pmos_contract::model::valid_slack_id(u))?;
        let fallback = format!(
            "home:{user}:{}",
            event["event_ts"].as_str().unwrap_or_default()
        );
        if !self.first_event(payload, fallback) {
            return None;
        }
        let tab = Tab::of_metadata(event["view"]["private_metadata"].as_str());
        let (this, user) = (self.clone(), user.to_string());
        Some(Box::pin(async move {
            // The first-time offer, if their account is now linked (flag off: no-op).
            this.onboarding_offer_for_slack_user(&user).await;
            this.publish_home(&user, tab, None).await
        }))
    }

    /// A click in the Home. Section buttons, **Refresh**, the batch decisions and
    /// **Disconnect** republish; tick boxes and **Connect** links do nothing more.
    pub(crate) fn home_action(&self, payload: &Value) -> (Option<Value>, Option<FollowUp>) {
        let Some(user) = payload["user"]["id"]
            .as_str()
            .filter(|u| pmos_contract::model::valid_slack_id(u))
            .map(String::from)
        else {
            return (None, None);
        };
        let current = Tab::of_metadata(payload["view"]["private_metadata"].as_str());
        let action_id = payload["actions"][0]["action_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let this = self.clone();
        let tab = match action_id.as_str() {
            action::TAB_INBOX => Some(Tab::Inbox),
            action::TAB_SKILLS => Some(Tab::Skills),
            action::TAB_ACCOUNTS => Some(Tab::Accounts),
            action::REFRESH => Some(current),
            action::SKILLS_GROUP => Some(Tab::Skills),
            _ => None,
        };
        let group = (action_id == action::SKILLS_GROUP)
            .then(|| payload["actions"][0]["value"].as_str())
            .flatten()
            .filter(|g| known_group(g))
            .map(String::from);
        {
            let mut expanded = self
                .home_state()
                .expanded
                .lock()
                .expect("home expanded lock");
            if let Some(group) = &group {
                expanded.insert(user.clone(), group.clone());
            } else if tab.is_some() && action_id != action::REFRESH {
                expanded.remove(&user);
            }
        }
        let work: FollowUp = if let Some(tab) = tab {
            Box::pin(async move { this.publish_home(&user, tab, None).await })
        } else if action_id == action::APPROVE || action_id == action::REJECT {
            let decision = if action_id == action::APPROVE {
                "approve"
            } else {
                "reject"
            };
            let ids = ticked(&payload["view"]["state"]["values"]);
            Box::pin(async move { this.decide_ticked(&user, decision, ids).await })
        } else if let Some(provider) = action_id
            .strip_prefix(action::UNLINK_PREFIX)
            .and_then(|p| PROVIDERS.iter().find(|k| **k == p).copied())
        {
            Box::pin(async move { this.unlink_account(&user, provider, current).await })
        } else {
            // A tick, a Connect link, or something unknown: acked, nothing else.
            return (None, None);
        };
        (None, Some(work))
    }

    /// Republish `handle`'s Home if they have opened it, on the section they left it on.
    /// Best effort and in the background: the notifier never waits for it.
    pub fn refresh_home(&self, handle: &str) {
        let Some(slack_id) = self.team().by_handle(handle).map(|m| m.slack_id.clone()) else {
            return;
        };
        let tab = self
            .home_state()
            .viewers
            .lock()
            .expect("home viewers lock")
            .get(&slack_id)
            .copied();
        if let Some(tab) = tab {
            let this = self.clone();
            tokio::spawn(async move { this.publish_home(&slack_id, tab, None).await });
        }
    }

    /// Build and publish the Home for a Slack user.
    async fn publish_home(&self, user: &str, tab: Tab, notice: Option<String>) {
        let view = match self.team().by_slack_id(user).map(|m| m.handle.clone()) {
            None => not_a_member_view(),
            Some(handle) => {
                self.home_state()
                    .viewers
                    .lock()
                    .expect("home viewers lock")
                    .insert(user.to_string(), tab);
                let body = match tab {
                    Tab::Inbox => self.inbox_body(&handle).await,
                    Tab::Skills => {
                        let open = self
                            .home_state()
                            .expanded
                            .lock()
                            .expect("home expanded lock")
                            .get(&user.to_string())
                            .cloned();
                        self.skills_body(&handle, open.as_deref()).await
                    }
                    Tab::Accounts => self.accounts_body(&handle).await,
                };
                view(tab, notice.as_deref(), body)
            }
        };
        if let Err(e) = self.slack().publish_view(user, view).await {
            tracing::warn!(error = %e, "views.publish failed");
        }
    }

    async fn inbox_body(&self, handle: &str) -> Vec<Value> {
        match self.toolserver().pending_approvals(handle).await {
            Ok((items, total)) => {
                let roster = self.roster().await.unwrap_or_default();
                let display = |bot: &str| {
                    roster
                        .iter()
                        .find(|b| b.name == bot)
                        .map(|b| b.display_name())
                        .unwrap_or_else(|| crate::runtime::display_name(bot))
                };
                inbox(&items, total, &display, Utc::now())
            }
            Err(e) => {
                tracing::warn!(error = %e, "pending approvals unavailable for the Home");
                vec![
                    section(
                        "PM OS couldn't load your drafts right now. Tap *Refresh* in a minute.",
                    ),
                    json!({"type": "actions", "block_id": "home_inbox", "elements": [
                        button("Refresh", action::REFRESH, "refresh", None),
                    ]}),
                ]
            }
        }
    }

    /// The skills catalog from the content repo, reused for a minute.
    async fn catalog(&self) -> Vec<Skill> {
        if let Some((at, skills)) = &*self.home_state().catalog.lock().expect("catalog lock") {
            if at.elapsed() < CATALOG_TTL {
                return skills.clone();
            }
        }
        let root: PathBuf = self.content_root();
        let linted = tokio::task::spawn_blocking(move || {
            lint(&root, &LintOptions::new(Utc::now().date_naive())).skills
        })
        .await;
        match linted {
            Ok(skills) => {
                *self.home_state().catalog.lock().expect("catalog lock") =
                    Some((Instant::now(), skills.clone()));
                skills
            }
            Err(e) => {
                tracing::warn!(error = %e, "reading the skills catalog failed");
                Vec::new()
            }
        }
    }

    async fn skills_body(&self, handle: &str, expanded: Option<&str>) -> Vec<Value> {
        let catalog: Vec<(String, String, Option<String>)> = self
            .catalog()
            .await
            .into_iter()
            .filter(|s| s.surfaces.iter().any(|x| x == "slack"))
            .map(|s| (s.name, s.description, s.track))
            .collect();
        let personal = personal_bot_name(handle);
        let roster = self.roster().await.unwrap_or_default();
        let bot = if roster.iter().any(|b| b.name == personal) {
            personal
        } else {
            DEFAULT_BOT.to_string()
        };
        let allowed = match self.runtime().bot(&bot).await {
            Ok(detail) => Some(Runtime::role_skills(&detail)),
            Err(e) => {
                tracing::warn!(error = %e, bot, "bot details unavailable for the Home");
                None
            }
        };
        let level = match self.runtime().level(handle).await {
            Ok(level) => Some(level),
            Err(e) => {
                tracing::warn!(error = %e, "the PM's level is unavailable for the Home");
                None
            }
        };
        skills(&catalog, allowed.as_deref(), level.as_deref(), expanded)
    }

    async fn accounts_body(&self, handle: &str) -> Vec<Value> {
        let state = match self.toolserver().onboarding(handle).await {
            Ok(state) => state,
            Err(e) => {
                tracing::warn!(error = %e, "onboarding state unavailable for the Home");
                return vec![
                    section(
                        "PM OS couldn't load your accounts right now. Tap *Refresh* in a minute.",
                    ),
                    json!({"type": "actions", "block_id": "home_accounts", "elements": [
                        button("Refresh", action::REFRESH, "refresh", None),
                    ]}),
                ];
            }
        };
        let mut links = Vec::new();
        for provider in PROVIDERS {
            let linked = &state["linked"][provider];
            if linked.as_bool() == Some(true) || (provider == "github" && linked.is_null()) {
                continue;
            }
            let url = match self.toolserver().oauth_start(provider, handle).await {
                Ok(url) => Some(url),
                Err(e) => {
                    tracing::warn!(error = %e, provider, "no Connect link for the Home");
                    None
                }
            };
            links.push((provider, url));
        }
        accounts(&state, &links)
    }

    /// **Approve selected** / **Reject selected**: decide each ticked draft, then republish
    /// the Inbox with how each one ended.
    async fn decide_ticked(&self, user: &str, decision: &str, ids: Vec<Ticked>) {
        let Some(handle) = self.team().by_slack_id(user).map(|m| m.handle.clone()) else {
            return self.publish_home(user, Tab::Inbox, None).await;
        };
        if ids.is_empty() {
            return self
                .publish_home(user, Tab::Inbox, Some(TICK_FIRST.into()))
                .await;
        }
        let mut tally = Tally::default();
        for Ticked { id, shown } in &ids {
            // An approve sends what the row showed (R5); the tool server refuses it as
            // `stale` if the draft changed since. A row that doesn't say (an App Home view
            // from before this release) is skipped: nothing is approved unseen.
            let seen = match (decision, shown.as_deref()) {
                ("approve", None) => {
                    tracing::info!(approval = %id, "an App Home row without its args hash; skipped");
                    tally.changed += 1;
                    continue;
                }
                ("approve", Some(hash)) => Seen::hash(hash),
                _ => Seen::default(),
            };
            let outcome = self
                .toolserver()
                .decide(id, &handle, decision, None, None, seen)
                .await;
            if let Err(e) = &outcome {
                tracing::info!(approval = %id, error = %e, "a Home decision didn't go through");
            }
            tally.add(&outcome);
        }
        tracing::info!(
            user = handle,
            decision,
            n = ids.len(),
            "drafts decided from the Home"
        );
        self.publish_home(user, Tab::Inbox, Some(tally.notice()))
            .await;
    }

    /// **Disconnect**: unlink the account, then republish with what happened.
    async fn unlink_account(&self, user: &str, provider: &'static str, tab: Tab) {
        let Some(handle) = self
            .team()
            .by_slack_id(user)
            .map(|m| m.handle.clone())
            .filter(|h| valid_handle(h))
        else {
            return self.publish_home(user, tab, None).await;
        };
        let name = account_name(provider);
        let notice = match self.toolserver().unlink(&handle, provider).await {
            Ok(()) => {
                tracing::info!(user = handle, provider, "account unlinked from the Home");
                format!("Disconnected {name}.")
            }
            // A second click, or a disconnect from elsewhere, got there first.
            Err(ApiError::Refused {
                status: 404, error, ..
            }) if error.contains("no such linked account") => {
                format!("{name} was already disconnected.")
            }
            Err(ApiError::Unavailable) => format!(
                ":warning: Couldn't disconnect {name}: PM OS couldn't reach the tool server."
            ),
            Err(ApiError::Refused { error, .. }) => format!(
                ":warning: Couldn't disconnect {name}: {}",
                escape(&truncate(
                    if error.is_empty() { "refused" } else { &error },
                    300
                ))
            ),
        };
        self.publish_home(user, Tab::Accounts, Some(notice)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_round_trip() {
        assert_eq!(Tab::of_metadata(Some(r#"{"tab":"skills"}"#)), Tab::Skills);
        assert_eq!(Tab::of_metadata(Some("nope")), Tab::Inbox);
        assert_eq!(Tab::of_metadata(None), Tab::Inbox);
        let v = view(Tab::Accounts, Some("Hi"), vec![]);
        assert_eq!(v["private_metadata"], r#"{"tab":"accounts"}"#);
        let tabs = &v["blocks"][1]["elements"];
        assert_eq!(tabs[2]["style"], "primary");
        assert!(tabs[0].get("style").is_none());
        assert_eq!(v["blocks"][2]["text"]["text"], "Hi");
        let many = view(Tab::Inbox, None, vec![section("x"); 300]);
        assert_eq!(many["blocks"].as_array().unwrap().len(), MAX_BLOCKS);
    }

    #[test]
    fn rows_say_what_and_where_escaped() {
        let now: DateTime<Utc> = "2026-11-15T12:00:00Z".parse().unwrap();
        let item = json!({
            "id": "apr_1", "user": "ana", "bot": "pm-ana", "tool": "slack.post_reply",
            "status": "pending", "created_at": "2026-11-15T09:00:00Z",
            "args": {"channel": "C0LAUNCH", "thread_ts": "1.2", "text": "Yes <!here> &\n".to_string() + &"x".repeat(400)},
        });
        let row = inbox_row(&item, &crate::runtime::display_name, now);
        let text = row["text"]["text"].as_str().unwrap();
        assert!(text.starts_with("*Reply drafted for <#C0LAUNCH>*\nby PM Ana · 3 hours ago\n> Yes &lt;!here&gt; &amp; x"), "{text}");
        assert!(text.chars().count() < 300);
        assert_eq!(row["block_id"], "pick_apr_1");
        assert_eq!(row["accessory"]["options"][0]["value"], "apr_1");
        let jira = json!({"tool": "jira.create_issue", "args": {"project": "PM", "summary": "s"}});
        assert_eq!(item_title(&jira), "Create a Jira issue in `PM`");
        let odd = json!({"tool": "confluence.create_draft", "args": {"space": "a`b"}});
        assert_eq!(item_title(&odd), "Create a Confluence draft");
        let msg = json!({"tool": "slack.send_message", "args": {"channel": "U0BOB"}});
        assert_eq!(item_title(&msg), "Send a Slack message as you to <@U0BOB>");
        assert_eq!(
            ago(Some("2026-11-13T12:00:00Z"), now).as_deref(),
            Some("2 days ago")
        );
        assert_eq!(
            ago(Some("2026-11-15T11:59:30Z"), now).as_deref(),
            Some("just now")
        );
    }

    #[test]
    fn the_inbox_caps_at_ten() {
        let now = Utc::now();
        let items: Vec<Value> = (0..12)
            .map(|i| json!({"id": format!("apr_{i}"), "tool": "jira.create_issue", "args": {"summary": "s"}}))
            .collect();
        let blocks = inbox(&items, 14, &crate::runtime::display_name, now);
        let s = Value::Array(blocks.clone()).to_string();
        assert!(s.contains("*14 drafts waiting for you*"));
        assert!(s.contains("and 4 more; they appear as you clear these."));
        assert_eq!(
            blocks
                .iter()
                .filter(|b| b["block_id"]
                    .as_str()
                    .is_some_and(|b| b.starts_with("pick_")))
                .count(),
            10
        );
        assert!(s.contains("Each one is created or posted as you."));
        let empty = Value::Array(inbox(&[], 0, &crate::runtime::display_name, now)).to_string();
        assert!(empty.contains("Nothing is waiting for you."));
        assert!(!empty.contains(action::APPROVE));
    }

    #[test]
    fn ticks_are_read_and_checked() {
        let values = json!({
            "pick_apr_1": {"home_pick": {"type": "checkboxes", "selected_options": [{"value": "apr_1"}]}},
            "pick_apr_2": {"home_pick": {"type": "checkboxes", "selected_options": []}},
            "pick_../x": {"home_pick": {"type": "checkboxes", "selected_options": [{"value": "../x"}]}},
            "pick_apr_3": {"home_pick": {"type": "checkboxes", "selected_options": [{"value": "apr_9"}]}},
            "pick_apr_4:0123456789abcdef": {"home_pick": {"type": "checkboxes", "selected_options": [{"value": "apr_4"}]}},
            "pick_apr_5:nothex": {"home_pick": {"type": "checkboxes", "selected_options": [{"value": "apr_5"}]}},
        });
        assert_eq!(
            ticked(&values),
            [
                Ticked {
                    id: "apr_1".into(),
                    shown: None
                },
                Ticked {
                    id: "apr_4".into(),
                    shown: Some("0123456789abcdef".into())
                },
            ]
        );
        assert!(ticked(&json!(null)).is_empty());
    }

    /// R5: a row carries the start of the args hash it showed, and a stale or unversioned
    /// draft is counted as changed, not approved.
    #[test]
    fn rows_carry_what_they_showed_and_changes_are_counted() {
        let item = json!({"id": "apr_1", "tool": "jira.create_issue", "status": "pending",
                          "args": {"summary": "x"}, "args_hash": "0123456789abcdef".repeat(4)});
        let row = inbox_row(&item, &|b: &str| b.to_string(), Utc::now());
        assert_eq!(row["block_id"], "pick_apr_1:0123456789abcdef");
        let mut t = Tally::default();
        t.add(&Ok(json!({"status": "approved"})));
        t.add(&Ok(json!({"status": "approved"})));
        t.add(&Err(ApiError::Refused {
            status: 409,
            code: "stale".into(),
            error: "changed".into(),
            body: Value::Null,
        }));
        let n = t.notice();
        assert!(n.contains("2 approved or posted."), "{n}");
        assert!(
            n.contains("1 changed since you looked, so it was not approved. Open it to review."),
            "{n}"
        );
        assert!(!n.contains("expired"));
    }

    #[test]
    fn notices_count_each_outcome() {
        let refused = |code: &str, status: u16| -> Result<Value, ApiError> {
            Err(ApiError::Refused {
                status,
                code: code.into(),
                error: "nope".into(),
                body: Value::Null,
            })
        };
        let mut t = Tally::default();
        t.add(&Ok(json!({"status": "approved"})));
        t.add(&Ok(json!({"status": "approved"})));
        t.add(&refused("audience_blocked", 409));
        t.add(&refused("needs_external_confirm", 409));
        t.add(&refused("expired", 409));
        t.add(&Err(ApiError::Unavailable));
        t.add(&Ok(
            json!({"status": "failed", "result": {"error": "Jira said <no>"}}),
        ));
        let n = t.notice();
        assert!(n.contains(":white_check_mark: 2 approved or posted."));
        assert!(n.contains("1 blocked: built from restricted sources"));
        assert!(n.contains(
            "1 needs your confirmation: it goes to a channel shared outside the company"
        ));
        assert!(n.contains("1 expired, or already decided."));
        assert!(n.contains("2 failed: PM OS couldn't reach the tool server; Jira said &lt;no&gt;."));
        assert!(!n.contains("rejected"));
    }

    fn entry(n: &str, t: Option<&str>) -> (String, String, Option<String>) {
        (
            n.to_string(),
            format!("Does {n} <well>. And more."),
            t.map(String::from),
        )
    }

    fn texts(blocks: &[Value]) -> String {
        Value::Array(blocks.to_vec()).to_string()
    }

    fn headers(blocks: &[Value]) -> Vec<&str> {
        blocks
            .iter()
            .filter(|b| b["type"] == "header")
            .map(|b| b["text"]["text"].as_str().unwrap())
            .collect()
    }

    /// The real skills' names and tracks, plus invented ones to reach 47.
    fn full_catalog() -> Vec<(String, String, Option<String>)> {
        let mut out = Vec::new();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content/skills");
        for dir in std::fs::read_dir(root).unwrap() {
            let file = dir.unwrap().path().join("SKILL.md");
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let name = file
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string();
            let track = text
                .lines()
                .find_map(|l| l.trim().strip_prefix("track:"))
                .map(|t| t.trim().trim_matches('"').to_string());
            out.push(entry(&name, track.as_deref()));
        }
        let mut i = 0;
        while out.len() < 47 {
            out.push(entry(&format!("extra-{i}"), (i % 2 == 0).then_some("Odd")));
            i += 1;
        }
        out
    }

    #[test]
    fn skills_start_here_shows_three_suggestions() {
        let catalog = full_catalog();
        let blocks = skills(&catalog, None, None, None);
        assert_eq!(headers(&blocks)[0], "Start here");
        let s = texts(&blocks);
        for (name, _, example) in START_HERE.iter().take(3) {
            assert!(s.contains(&format!("*{name}*: ")), "{name}");
            assert!(s.contains(&format!("Try `{example}`")), "{name}");
        }
        assert!(!s.contains("*prd-draft*: "), "only three are suggested");
        let at = blocks.iter().position(|b| b["type"] == "header").unwrap();
        let start = blocks[at + 1]["text"]["text"].as_str().unwrap();
        assert_eq!(start.lines().count(), 3);
    }

    #[test]
    fn skills_start_here_falls_back_when_one_is_missing() {
        let catalog = full_catalog();
        let allowed: Vec<String> = catalog
            .iter()
            .map(|c| c.0.clone())
            .filter(|n| n != "meeting-notes")
            .collect();
        let s = texts(&skills(&catalog, Some(&allowed), None, None));
        assert!(s.contains("*ask*: ") && s.contains("*status-update*: "));
        assert!(s.contains("*prd-draft*: "), "the next one steps in");
        assert!(!s.contains("*meeting-notes*"));
        // Nothing suggested is available: no "Start here" at all.
        let only = vec!["decision-doc".to_string()];
        let blocks = skills(&catalog, Some(&only), None, None);
        assert!(!headers(&blocks).contains(&"Start here"));
    }

    #[test]
    fn skills_groups_cover_every_skill_once() {
        let catalog = full_catalog();
        assert_eq!(catalog.len(), 47);
        let all_open = |blocks: &[Value]| {
            let mut seen: Vec<String> = Vec::new();
            let mut in_group = false;
            for b in blocks {
                if b["type"] == "header" {
                    in_group = b["text"]["text"] != "Start here";
                } else if in_group && b["type"] == "section" {
                    for line in b["text"]["text"].as_str().unwrap().lines() {
                        seen.push(
                            line.trim_start_matches('*')
                                .split('*')
                                .next()
                                .unwrap()
                                .into(),
                        );
                    }
                }
            }
            seen
        };
        // Collapsed: nothing is listed twice and the block count is well under the limit.
        let collapsed = skills(&catalog, None, None, None);
        assert!(collapsed.len() + 4 <= MAX_BLOCKS, "{}", collapsed.len());
        let seen = all_open(&collapsed);
        let mut dup = seen.clone();
        dup.sort();
        dup.dedup();
        assert_eq!(dup.len(), seen.len(), "a skill appears twice");
        // Opening each group in turn lists every skill, exactly once across the groups.
        let mut listed: Vec<String> = Vec::new();
        for group in GROUPS.iter().map(|(g, _)| *g).chain([MORE]) {
            let blocks = skills(&catalog, None, None, Some(group));
            assert!(blocks.len() + 4 <= MAX_BLOCKS);
            let in_group: Vec<String> = all_open(&blocks)
                .into_iter()
                .filter(|n| {
                    let (_, _, t) = catalog.iter().find(|c| &c.0 == n).unwrap();
                    group_of(n, t.as_deref()) == group
                })
                .collect();
            listed.extend(in_group);
        }
        listed.sort();
        let mut names: Vec<String> = catalog.iter().map(|c| c.0.clone()).collect();
        names.sort();
        assert_eq!(listed, names);
        // The rules.
        assert_eq!(group_of("no-such-skill", None), "More");
        assert_eq!(group_of("x", Some("Odd")), "More");
        assert_eq!(group_of("x", Some("everyday")), "Everyday");
        assert_eq!(
            group_of("status-update", Some("Delivery and launch")),
            "Communicate"
        );
        for (_, g) in GROUP_OVERRIDES {
            assert!(GROUPS.iter().any(|(n, _)| *n == g), "{g}");
        }
    }

    #[test]
    fn skills_long_groups_show_a_button_and_open() {
        let catalog = full_catalog();
        let blocks = skills(&catalog, None, None, None);
        let buttons: Vec<&Value> = blocks
            .iter()
            .filter(|b| b["type"] == "actions")
            .map(|b| &b["elements"][0])
            .collect();
        assert!(!buttons.is_empty());
        for b in &buttons {
            assert_eq!(b["action_id"], action::SKILLS_GROUP);
            let label = b["text"]["text"].as_str().unwrap();
            assert!(label.starts_with("Show all in "));
            assert!(known_group(b["value"].as_str().unwrap()));
        }
        let first = buttons[0]["value"].as_str().unwrap();
        let open = skills(&catalog, None, None, Some(first));
        assert_eq!(
            open.iter().filter(|b| b["type"] == "actions").count(),
            buttons.len() - 1,
            "the opened group has no button"
        );
    }

    #[test]
    fn skills_keep_level_note_and_empty_cases() {
        let catalog = vec![entry("ask", Some("Everyday")), entry("zeta", Some("Zany"))];
        let allowed = vec!["ask".to_string(), "zeta".to_string()];
        let blocks = skills(&catalog, Some(&allowed), Some("staff"), None);
        assert_eq!(
            blocks[0]["text"]["text"],
            "You're set to the *Staff* level. Change it by sending me `level …`."
        );
        assert_eq!(headers(&blocks), ["Start here", "Everyday", "More"]);
        let s = texts(&blocks);
        assert!(s.contains("*zeta* Does zeta &lt;well&gt;."));
        assert!(!s.contains("And more"));
        assert_eq!(first_sentence(&"word ".repeat(100)).chars().count(), 151);
        let none = skills(&catalog, Some(&[]), None, None);
        assert!(texts(&none).contains("No skills are available to your bot in Slack yet."));
        let note = skills(&catalog, None, None, None);
        assert!(texts(&note).contains("couldn't load which skills"));
        assert!(!texts(&note).contains("level"));
    }

    #[test]
    fn inbox_and_accounts_do_not_get_skills_blocks() {
        let s = texts(&inbox(&[], 0, &|b| b.to_string(), Utc::now()));
        assert!(!s.contains("Start here") && !s.contains(action::SKILLS_GROUP));
        let state = json!({"linked": {"slack": true}});
        assert!(!texts(&accounts(&state, &[])).contains("Start here"));
    }

    #[test]
    fn accounts_connect_and_disconnect() {
        let state = json!({"linked": {"atlassian": false, "slack": true}, "memory_repo": "org/pm-os-memory-ana"});
        let links = [(
            "atlassian",
            Some("https://pmos.example.com/oauth/a".to_string()),
        )];
        let s = Value::Array(accounts(&state, &links)).to_string();
        assert!(s.contains("https://pmos.example.com/oauth/a"));
        assert!(s.contains("home_unlink_slack"));
        assert!(s.contains(
            "The reply assistant, tagging and thread reading stop until you connect again."
        ));
        assert!(!s.contains("GitHub"), "not in the answer, not shown");
        assert!(s.contains("`org/pm-os-memory-ana`"));
    }
}
