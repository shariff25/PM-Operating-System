//! The bot marketplace's catalog: **personas** (`docs/bot-marketplace-spec.md` §2).
//!
//! In the product Chief, Sprout and Echo are "bots"; in code they are personas, because a
//! [`crate::bots::Bot`] is a sandboxed bot with its own policy and a marketplace bot is not one.
//! A persona is a role a PM's own personal bot (`pm-<handle>`) runs as, chosen per ask. Its
//! file is `content/personas/<key>/PERSONA.md`.
//!
//! A persona can never do more than the personal role ([`PERSONAL_ROLE`]): the lint refuses a
//! persona whose role has a tool, a data class or a write setting looser than the personal
//! role's (`persona-exceeds-base`, `persona-data-class`, `persona-action`), and the tool server
//! intersects again per session. The policy generator gives every personal bot of the personal
//! role the skill folders of every persona (read-only uploads), and nothing else.
//!
//! What a persona reads and may draft is **derived** from its role's tools here, never written
//! in the file, so content can't claim a bot reads or writes something it can't.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::bots::{ActionSetting, Role};
use crate::model::{ToolKind, ToolRegistry};

pub use crate::onboarding::PERSONAL_ROLE;

/// The mascot drawings the web app has. New art is code, so a new key needs a code change
/// here and in `web/src/components/mascots/`.
pub const MASCOTS: [&str; 4] = ["nudge", "chief", "sprout", "echo"];

/// The data sources a persona may say it needs (`needs[].source`).
pub const NEED_SOURCES: [&str; 2] = ["analytics", "crm"];

/// The always-on persona's key: today's generalist, on every PM's team.
pub const ALWAYS_ON: &str = "nudge";

/// The most personas the catalog may hold.
pub const MAX_PERSONAS: usize = 16;

/// Text limits, in characters.
pub const MAX_DISPLAY_NAME: usize = 24;
pub const MAX_TITLE: usize = 40;
pub const MAX_SUMMARY: usize = 160;
pub const MAX_EXAMPLE: usize = 80;
pub const MAX_EXAMPLES: usize = 6;
pub const MAX_NEED_LABEL: usize = 40;
pub const MAX_NEED_WITHOUT: usize = 160;
/// The largest `order`.
pub const MAX_ORDER: i64 = 1000;

/// What a write tool drafts, as the Bots page says it ("What it may draft"). A write tool a
/// persona's role has but that is missing here is a lint error (`persona-write-label`), so a
/// new write tool is never shown unlabelled.
pub const WRITE_LABELS: [(&str, &str); 6] = [
    ("jira.create_issue", "Jira issues"),
    ("confluence.create_draft", "Confluence pages"),
    ("confluence.update_page", "Confluence page updates"),
    ("slack.post_reply", "Slack replies"),
    ("slack.send_message", "Slack messages"),
    ("bets.record", "bets register entries"),
];

/// The line every customer-facing draft of the Outbound PM bot (Echo) carries until Legal's
/// outbound policy exists (the owner's decision of 2026-11-21, `docs/bot-decisions.md`, note on
/// D10 and D12). Echo's `PERSONA.md` names it as its `draft-notice`; the tool server puts it at
/// the top of every page body such a persona drafts ([`with_draft_notice`]), whatever the
/// model wrote. No extra approval step goes with it.
pub const INTERNAL_REVIEW_NOTICE: &str =
    "Draft for internal review. Not approved for external use.";

/// The longest `draft-notice`.
pub const MAX_DRAFT_NOTICE: usize = 160;

/// The write tools whose drafts are pages read beyond the team: a persona's `draft-notice`
/// goes at the top of their `body`. Jira issues and Slack replies are internal work and don't
/// carry it.
pub const NOTICE_TOOLS: [&str; 2] = ["confluence.create_draft", "confluence.update_page"];

/// The role sections a persona role carries **verbatim** from the personal role
/// (`docs/bot-marketplace-spec.md` §3): the lint refuses a persona role whose copy differs in
/// any character, and a personal role that lacks one (`persona-role-sections`, an error). A
/// persona's own limits go in a section of another name.
pub const STANDING_SECTIONS: [&str; 5] = [
    "Untrusted data",
    "Hard limits",
    "Source markers in drafts",
    "Look before you ask",
    "Point of view",
];

/// Names no persona may take as its key or display name, compared ignoring case and anything
/// but letters and digits (`persona-reserved`): PM OS itself, its four roles and their words,
/// the systems it talks to, and words Slack treats specially. A display name is shown in Slack
/// as "Name: …", so one of these would read as PM OS, a person's role or a system speaking.
pub const RESERVED_NAMES: [&str; 30] = [
    "PM OS",
    "PMOS",
    "PM",
    "Admin",
    "Administrator",
    "Product Manager",
    "Reviewer",
    "Auditor",
    "Owner",
    "Platform owner",
    "System",
    "Security",
    "Legal",
    "Support",
    "Help",
    "Bot",
    "Assistant",
    "Evals",
    "Anonymous",
    "Slack",
    "Slackbot",
    "Jira",
    "Confluence",
    "GitHub",
    "OpenShell",
    "Claude",
    "Anthropic",
    "Everyone",
    "Channel",
    "Here",
];

/// Lowercase letters and digits only: how names are compared.
fn fold_name(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The reserved name ([`RESERVED_NAMES`]) `s` reads as, if any.
pub fn reserved_name(s: &str) -> Option<&'static str> {
    let me = fold_name(s);
    RESERVED_NAMES.iter().copied().find(|r| fold_name(r) == me)
}

/// Whether `s` reads as the person's name or handle `name` (compared as [`reserved_name`]
/// compares): a persona must not be named like a PM OS user, whose name Slack and the web app
/// show in the same place.
pub fn reads_as(s: &str, name: &str) -> bool {
    let (a, b) = (fold_name(s), fold_name(name));
    !a.is_empty() && a == b
}

/// The sections of a role body, by `## ` heading: `(heading, text)`, the text running from the
/// heading line to the line before the next `## ` heading, trailing whitespace trimmed.
pub fn role_sections(body: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in body.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            out.push((h.trim().to_string(), format!("{line}\n")));
        } else if let Some((_, text)) = out.last_mut() {
            text.push_str(line);
            text.push('\n');
        }
    }
    for (_, text) in &mut out {
        let keep = text.trim_end().len();
        text.truncate(keep);
    }
    out
}

/// The team's standing context, which comes before any persona's text.
pub const MASTER_CONTEXT: &str = "master/AGENTS.md";

/// The content files (paths relative to the content root) whose change alters what a
/// marketplace bot is told, so a change to one needs a second platform owner's review, as a
/// network change does (`policies/README.md`, "Bot instructions"): the team's standing context
/// ([`MASTER_CONTEXT`]), each persona's file, the role body of every role a persona runs as,
/// the personal role included (its standing sections are copied into every other), the
/// `SKILL.md` of every skill those roles hold (a persona runs its role's skills), and the
/// `BOT.md` of every personal bot (a persona runs on the PM's own bot, which keeps its body;
/// gate-fix-4). `roles` and `bots` are the catalog's; a role it doesn't have adds no skills.
pub fn instruction_paths<'a>(
    personas: impl IntoIterator<Item = &'a Persona>,
    roles: &[&Role],
    bots: &[&crate::bots::Bot],
) -> BTreeSet<String> {
    let mut out = BTreeSet::from([MASTER_CONTEXT.to_string()]);
    let mut persona_roles = BTreeSet::from([PERSONAL_ROLE.to_string()]);
    for p in personas {
        out.insert(format!("personas/{}/PERSONA.md", p.name));
        out.insert(format!("roles/{}/ROLE.md", p.role));
        persona_roles.insert(p.role.clone());
    }
    for role in roles.iter().filter(|r| persona_roles.contains(&r.name)) {
        out.insert(format!("roles/{}/ROLE.md", role.name));
        for skill in &role.skills {
            out.insert(format!("skills/{skill}/SKILL.md"));
        }
    }
    for bot in bots
        .iter()
        .filter(|b| b.scope == crate::bots::BotScope::Personal)
    {
        out.insert(format!("bots/{}/BOT.md", bot.name));
    }
    out
}

/// Whether a change to content file `rel` changes a marketplace bot's instructions: one of
/// `known` ([`instruction_paths`]), any file under `personas/` (a new persona), or a new
/// personal bot's `BOT.md` (`bots/pm-<handle>/BOT.md`).
pub fn is_bot_instructions(rel: &str, known: &BTreeSet<String>) -> bool {
    known.contains(rel)
        || rel.starts_with("personas/")
        || (rel.starts_with("bots/pm-") && rel.ends_with("/BOT.md"))
}

/// `args` with `notice` as the first line of the page body, for a tool in [`NOTICE_TOOLS`]
/// whose `body` doesn't already **start** with it (leading whitespace aside): a copy further
/// down, where a reader may never get to it, doesn't count, and the notice is put first
/// anyway. Anything else is returned unchanged.
pub fn with_draft_notice(tool: &str, args: &serde_json::Value, notice: &str) -> serde_json::Value {
    let mut out = args.clone();
    if !NOTICE_TOOLS.contains(&tool) {
        return out;
    }
    if let Some(body) = out.get_mut("body") {
        if let Some(text) = body.as_str() {
            if !text.trim_start().starts_with(notice) {
                *body = serde_json::Value::String(format!("{notice}\n\n{text}"));
            }
        }
    }
    out
}

/// Systems whose read tools are not shown as sources ("Reads"): PM OS's own context, memory,
/// statistics and bets register.
pub const NOT_SOURCES: [&str; 4] = ["context", "memory", "stats", "bets"];

/// The order sources are shown in; any other system follows, alphabetically.
const SOURCE_ORDER: [&str; 4] = ["jira", "confluence", "slack", "docs"];

/// The label of a write tool, if it has one.
pub fn write_label(tool: &str) -> Option<&'static str> {
    WRITE_LABELS
        .iter()
        .find(|(t, _)| *t == tool)
        .map(|(_, label)| *label)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct NeedFrontMatter {
    pub source: String,
    pub label: String,
    pub without: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct PersonaFrontMatter {
    pub name: String,
    pub display_name: String,
    pub title: String,
    pub role: String,
    pub mascot: String,
    pub owner: String,
    pub version: String,
    #[serde(default)]
    pub offered: bool,
    #[serde(default)]
    pub always_on: bool,
    /// Offered as a Preview ("not yet benchmarked") before its eval category meets its bar
    /// (the owner's exception to D10, 2026-11-21).
    #[serde(default)]
    pub preview: bool,
    /// The content's claim that every eval category naming this persona met its bar two weeks
    /// in a row (D10). `pmos lint` checks the claim against `evals/reports`
    /// (`persona-benchmark`); the services can't read the reports, so for them a persona is a
    /// Preview unless it says this ([`Persona::preview`]: fail safe).
    #[serde(default)]
    pub benchmarked: bool,
    /// A line every page it drafts carries ([`NOTICE_TOOLS`]), such as Echo's
    /// [`INTERNAL_REVIEW_NOTICE`].
    #[serde(default)]
    pub draft_notice: Option<String>,
    pub order: i64,
    pub summary: String,
    pub examples: Vec<String>,
    /// Space-separated skill names; every skill of the role when absent.
    #[serde(default)]
    pub can_do: Option<String>,
    #[serde(default)]
    pub needs: Vec<NeedFrontMatter>,
}

/// A data source the persona would use but PM OS may not have.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Need {
    /// One of [`NEED_SOURCES`].
    pub source: String,
    /// "an analytics source": the badge reads "Needs <label>".
    pub label: String,
    /// What the bot does without it.
    pub without: String,
    /// Whether the tool registry has a read tool of this system that the persona's role has
    /// ([`need_met`]).
    pub met: bool,
}

/// A validated persona: one entry of the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Persona {
    /// The key in every API, table and audit event (equals its folder).
    pub name: String,
    pub display_name: String,
    pub title: String,
    /// The role it runs as.
    pub role: String,
    /// One of [`MASCOTS`].
    pub mascot: String,
    /// The owner's handle, without the `@`.
    pub owner: String,
    pub version: String,
    /// Whether PMs see it and may use it.
    pub offered: bool,
    /// Nudge: on every team, never removable.
    pub always_on: bool,
    /// A Preview: offered before its eval category has met its bar, shown as "Preview: not
    /// yet benchmarked" (the owner's exception to D10, 2026-11-21). **Worked out, fail safe**
    /// ([`is_preview`]): every persona but the always-on one is a Preview unless its file says
    /// `benchmarked: true` (a claim `pmos lint` verifies against the eval reports) and not
    /// `preview: true`. A file that leaves both out, or content whose lint never ran, still
    /// shows the label: no service needs the eval reports to fail safe.
    pub preview: bool,
    /// The file's `benchmarked: true`: the claim the lint verifies.
    pub benchmarked: bool,
    /// The line the tool server puts at the top of every page this persona drafts
    /// ([`with_draft_notice`]); `None` for most.
    pub draft_notice: Option<String>,
    pub order: u32,
    pub summary: String,
    pub examples: Vec<String>,
    /// "What it can do": skills of its role.
    pub can_do: Vec<String>,
    pub needs: Vec<Need>,
    /// Derived: the systems its role's read tools read ([`reads`]).
    pub reads: Vec<String>,
    /// Derived: what it may draft ([`drafts`]).
    pub drafts: Vec<String>,
}

/// Whether a persona is shown as a Preview: never the always-on persona; any other unless it
/// is `benchmarked` and not marked `preview` (see [`Persona::preview`]).
pub fn is_preview(always_on: bool, preview: bool, benchmarked: bool) -> bool {
    !always_on && (preview || !benchmarked)
}

/// The stricter of two settings (`never` beats `approve`, which beats `auto`).
pub fn stricter(a: ActionSetting, b: ActionSetting) -> ActionSetting {
    a.max(b)
}

/// A role's own setting for a write tool: its `actions` entry, else `approve`.
pub fn role_setting(role: &Role, tool: &str) -> ActionSetting {
    role.actions
        .get(tool)
        .copied()
        .unwrap_or(ActionSetting::Approve)
}

/// A **persona** role's own setting for a write tool: its `actions` entry, else `never`. A
/// persona role lists every write it may draft (`persona-action-unlisted`); there is no
/// fallback to `approve`, so a skill that later declares a new write tool gives the persona
/// nothing until someone decides its setting in the role.
pub fn persona_role_setting(role: &Role, tool: &str) -> ActionSetting {
    role.actions
        .get(tool)
        .copied()
        .unwrap_or(ActionSetting::Never)
}

/// A persona's effective setting for a write tool before any bot override: the stricter of
/// its role's ([`persona_role_setting`]) and the personal role's.
pub fn effective_setting(persona_role: &Role, base: &Role, tool: &str) -> ActionSetting {
    stricter(
        persona_role_setting(persona_role, tool),
        role_setting(base, tool),
    )
}

/// The systems a set of tools reads, as sources: Jira, Confluence, Slack and Docs first (in
/// that order), then any other system alphabetically; PM OS's own systems
/// ([`NOT_SOURCES`]) are left out.
pub fn reads(tools: &BTreeSet<String>, registry: &ToolRegistry) -> Vec<String> {
    let systems: BTreeSet<&str> = tools
        .iter()
        .filter_map(|t| registry.get(t))
        .filter(|t| t.kind == ToolKind::Read && !NOT_SOURCES.contains(&t.system.as_str()))
        .map(|t| t.system.as_str())
        .collect();
    let mut out: Vec<String> = SOURCE_ORDER
        .iter()
        .filter(|s| systems.contains(*s))
        .map(|s| s.to_string())
        .collect();
    out.extend(
        systems
            .iter()
            .filter(|s| !SOURCE_ORDER.contains(*s))
            .map(|s| s.to_string()),
    );
    out
}

/// What a persona may draft: the labels of its role's write tools whose effective setting
/// ([`effective_setting`]) is `approve`, in [`WRITE_LABELS`] order. A write tool with no label
/// is left out here; the lint refuses it (`persona-write-label`).
pub fn drafts(
    tools: &BTreeSet<String>,
    registry: &ToolRegistry,
    persona_role: &Role,
    base: &Role,
) -> Vec<String> {
    WRITE_LABELS
        .iter()
        .filter(|(tool, _)| {
            tools.contains(*tool)
                && registry
                    .get(tool)
                    .is_some_and(|t| t.kind == ToolKind::Write)
                && effective_setting(persona_role, base, tool) == ActionSetting::Approve
        })
        .map(|(_, label)| label.to_string())
        .collect()
}

/// A need is met when the registry has at least one `read` tool whose `system` is the need's
/// source **and** that tool is one of the persona's role tools.
pub fn need_met(source: &str, tools: &BTreeSet<String>, registry: &ToolRegistry) -> bool {
    registry
        .tools
        .iter()
        .any(|t| t.kind == ToolKind::Read && t.system == source && tools.contains(&t.name))
}

/// `display-name`: 1 to [`MAX_DISPLAY_NAME`] ASCII letters, digits and single spaces, starting
/// and ending with a letter or digit. It is also the Slack prefix (`Chief: …`).
pub fn check_display_name(s: &str) -> Result<(), String> {
    let n = s.chars().count();
    if n == 0 || n > MAX_DISPLAY_NAME {
        return Err(format!(
            "display-name must be 1 to {MAX_DISPLAY_NAME} characters"
        ));
    }
    let ok = s.chars().all(|c| c.is_ascii_alphanumeric() || c == ' ')
        && !s.starts_with(' ')
        && !s.ends_with(' ')
        && !s.contains("  ");
    if !ok {
        return Err(format!(
            "display-name `{s}` may use only letters, digits and single spaces"
        ));
    }
    Ok(())
}

/// Shown text (`title`, `summary`, `examples`, `needs[].label`, `needs[].without`): 1 to
/// `max` characters on one line, not blank, with no `<`, `>` or control characters.
pub fn check_text(field: &str, s: &str, max: usize) -> Result<(), String> {
    if s.trim().is_empty() {
        return Err(format!("{field} is empty"));
    }
    if s.chars().count() > max {
        return Err(format!("{field} is longer than {max} characters"));
    }
    if s.chars().any(|c| c == '<' || c == '>' || c.is_control()) {
        return Err(format!(
            "{field} may not contain `<`, `>`, line breaks or other control characters"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ModelRole, Sensitivity, ToolDef};
    use std::collections::BTreeMap;

    fn registry() -> ToolRegistry {
        let t = |name: &str, system: &str, kind| ToolDef {
            name: name.into(),
            system: system.into(),
            kind,
            description: None,
        };
        ToolRegistry {
            tools: vec![
                t("context.search", "context", ToolKind::Read),
                t("jira.search", "jira", ToolKind::Read),
                t("docs.search", "docs", ToolKind::Read),
                t("analytics.query", "analytics", ToolKind::Read),
                t("jira.create_issue", "jira", ToolKind::Write),
                t("slack.send_message", "slack", ToolKind::Write),
                t("memory.update", "memory", ToolKind::Internal),
            ],
        }
    }

    fn role(actions: &[(&str, ActionSetting)]) -> Role {
        Role {
            name: "r".into(),
            description: "d".into(),
            owner: "pm".into(),
            version: "0.1.0".into(),
            skills: vec![],
            data_class: Sensitivity::Internal,
            model: ModelRole::Default,
            actions: actions
                .iter()
                .map(|(t, s)| (t.to_string(), *s))
                .collect::<BTreeMap<_, _>>(),
            instructions: String::new(),
        }
    }

    fn set(tools: &[&str]) -> BTreeSet<String> {
        tools.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn reads_are_sources_in_a_fixed_order_without_pm_os_systems() {
        let tools = set(&[
            "analytics.query",
            "context.search",
            "docs.search",
            "jira.search",
            "jira.create_issue",
        ]);
        assert_eq!(reads(&tools, &registry()), ["jira", "docs", "analytics"]);
    }

    #[test]
    fn drafts_are_the_approve_writes_labelled() {
        use ActionSetting::*;
        let tools = set(&["jira.create_issue", "slack.send_message", "jira.search"]);
        let base = role(&[]);
        let both = role(&[
            ("jira.create_issue", Approve),
            ("slack.send_message", Approve),
        ]);
        assert_eq!(
            drafts(&tools, &registry(), &both, &base),
            ["Jira issues", "Slack messages"]
        );
        // `never` in the persona's role or in the base hides it.
        let p = role(&[
            ("jira.create_issue", Approve),
            ("slack.send_message", Never),
        ]);
        assert_eq!(drafts(&tools, &registry(), &p, &base), ["Jira issues"]);
        let base = role(&[("jira.create_issue", Never)]);
        assert_eq!(
            drafts(&tools, &registry(), &both, &base),
            ["Slack messages"]
        );
        // A write the persona's role doesn't list is `never`: no fallback to approve.
        let base = role(&[]);
        let unlisted = role(&[("jira.create_issue", Approve)]);
        assert_eq!(
            drafts(&tools, &registry(), &unlisted, &base),
            ["Jira issues"]
        );
        assert_eq!(persona_role_setting(&unlisted, "slack.send_message"), Never);
        assert_eq!(role_setting(&unlisted, "slack.send_message"), Approve);
    }

    #[test]
    fn the_stricter_setting_wins() {
        use ActionSetting::*;
        assert_eq!(stricter(Approve, Never), Never);
        assert_eq!(stricter(Never, Approve), Never);
        assert_eq!(stricter(Auto, Approve), Approve);
        let base = role(&[("jira.create_issue", Never)]);
        assert_eq!(
            effective_setting(&role(&[]), &base, "jira.create_issue"),
            Never
        );
    }

    #[test]
    fn a_persona_is_a_preview_unless_it_claims_a_benchmark() {
        // (always_on, preview, benchmarked)
        assert!(!is_preview(true, false, false), "Nudge never");
        assert!(is_preview(false, false, false), "no claim: a Preview");
        assert!(is_preview(false, true, false));
        assert!(is_preview(false, true, true), "both: the label wins");
        assert!(!is_preview(false, false, true));
    }

    #[test]
    fn a_need_is_met_only_by_a_read_tool_of_the_role() {
        let r = registry();
        assert!(need_met("analytics", &set(&["analytics.query"]), &r));
        assert!(!need_met("analytics", &set(&["jira.search"]), &r));
        assert!(!need_met("crm", &set(&["analytics.query"]), &r));
    }

    #[test]
    fn every_write_tool_in_the_repository_has_a_label() {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content/tools.yaml"),
        )
        .unwrap();
        let registry = ToolRegistry::from_yaml(&text).unwrap();
        for tool in registry.write_tools() {
            assert!(write_label(&tool.name).is_some(), "{}", tool.name);
        }
    }

    #[test]
    fn text_rules() {
        assert!(check_display_name("Chief").is_ok());
        assert!(check_display_name("Chief 2").is_ok());
        assert!(check_display_name(&"x".repeat(24)).is_ok());
        for bad in [
            "",
            " Chief",
            "Chief ",
            "Chi  ef",
            "Chief!",
            "Chéf",
            &"x".repeat(25),
        ] {
            assert!(check_display_name(bad).is_err(), "{bad:?}");
        }
        assert!(check_text("title", "Chief of Staff", 40).is_ok());
        for bad in ["", "  ", "a<b", "a>b", "two\nlines", "tab\there"] {
            assert!(check_text("title", bad, 40).is_err(), "{bad:?}");
        }
        assert!(check_text("title", &"x".repeat(41), 40).is_err());
    }

    #[test]
    fn reserved_names_match_ignoring_case_and_spacing() {
        for name in [
            "PM OS",
            "pmos",
            "Pm-Os",
            "admin",
            "ADMIN",
            "Product Manager",
            "slackbot",
        ] {
            assert!(reserved_name(name).is_some(), "{name}");
        }
        for name in ["Chief", "Sprout", "Echo", "Nudge", "Admiral", "Pmo"] {
            assert_eq!(reserved_name(name), None, "{name}");
        }
        assert!(reads_as("Ana Lopez", "ana-lopez"));
        assert!(!reads_as("Chief", "ana"));
        assert!(!reads_as("", ""));
    }

    #[test]
    fn sections_split_on_level_two_headings() {
        let body = "# Role\n\nIntro.\n\n## Untrusted data\nOne.\nTwo.\n\n## Hard limits\n- No.\n\n### Sub\nStill limits.\n";
        let s = role_sections(body);
        assert_eq!(s.len(), 2);
        assert_eq!(
            s[0],
            (
                "Untrusted data".into(),
                "## Untrusted data\nOne.\nTwo.".into()
            )
        );
        assert_eq!(
            s[1],
            (
                "Hard limits".into(),
                "## Hard limits\n- No.\n\n### Sub\nStill limits.".into()
            )
        );
    }

    #[test]
    fn bot_instructions_are_the_master_context_personas_and_their_roles() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
        let report = crate::lint(&root, &crate::LintOptions::new(chrono::NaiveDate::MIN));
        let roles: Vec<&Role> = report.roles.iter().collect();
        let bots: Vec<&crate::bots::Bot> = report.bots.iter().collect();
        let known = instruction_paths(&report.personas, &roles, &bots);
        for p in [
            "master/AGENTS.md",
            "roles/pm-assistant/ROLE.md",
            "roles/chief-of-staff/ROLE.md",
            "roles/growth-pm/ROLE.md",
            "roles/outbound-pm/ROLE.md",
            "personas/echo/PERSONA.md",
            // A persona runs its role's skills (gate-fix-4).
            "skills/ask/SKILL.md",
            "skills/battlecard/SKILL.md",
            "skills/daily-brief/SKILL.md",
        ] {
            assert!(is_bot_instructions(p, &known), "{p}");
        }
        assert!(is_bot_instructions("personas/newbot/PERSONA.md", &known));
        // A personal bot's BOT.md: a persona runs on it and keeps its body.
        for b in report
            .bots
            .iter()
            .filter(|b| b.scope == crate::bots::BotScope::Personal)
        {
            assert!(is_bot_instructions(
                &format!("bots/{}/BOT.md", b.name),
                &known
            ));
        }
        assert!(is_bot_instructions("bots/pm-newpm/BOT.md", &known));
        for p in [
            "roles/docs-helper/ROLE.md",
            "context/products/x.md",
            // Only the team bot `docs` runs it: no persona does.
            "skills/docs/SKILL.md",
            "bots/docs/BOT.md",
        ] {
            assert!(!is_bot_instructions(p, &known), "{p}");
        }
    }

    #[test]
    fn the_notice_heads_every_page_body_once() {
        let n = INTERNAL_REVIEW_NOTICE;
        let page = serde_json::json!({"space": "PM", "title": "4.2", "body": "## New\nThings."});
        let noted = with_draft_notice("confluence.create_draft", &page, n);
        assert_eq!(noted["body"], format!("{n}\n\n## New\nThings."));
        assert_eq!(noted["title"], "4.2");
        // Already there: unchanged, so it is never doubled.
        assert_eq!(
            with_draft_notice("confluence.create_draft", &noted, n),
            noted
        );
        let update = serde_json::json!({"page_id": "1", "version": 2, "body": "x"});
        assert_eq!(
            with_draft_notice("confluence.update_page", &update, n)["body"],
            format!("{n}\n\nx")
        );
        // A copy at the bottom doesn't count: the notice goes first.
        let bottom = serde_json::json!({"body": format!("Things.\n\n{n}")});
        assert_eq!(
            with_draft_notice("confluence.create_draft", &bottom, n)["body"],
            format!("{n}\n\nThings.\n\n{n}")
        );
        // Not a page: unchanged.
        let issue = serde_json::json!({"project": "PM", "summary": "s", "description": "d"});
        assert_eq!(with_draft_notice("jira.create_issue", &issue, n), issue);
    }
}
