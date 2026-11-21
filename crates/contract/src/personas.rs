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

/// A persona's effective setting for a write tool before any bot override: the stricter of
/// its role's and the personal role's.
pub fn effective_setting(persona_role: &Role, base: &Role, tool: &str) -> ActionSetting {
    stricter(role_setting(persona_role, tool), role_setting(base, tool))
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
        let tools = set(&["jira.create_issue", "slack.send_message", "jira.search"]);
        let base = role(&[]);
        assert_eq!(
            drafts(&tools, &registry(), &role(&[]), &base),
            ["Jira issues", "Slack messages"]
        );
        // `never` in the persona's role or in the base hides it.
        let p = role(&[("slack.send_message", ActionSetting::Never)]);
        assert_eq!(drafts(&tools, &registry(), &p, &base), ["Jira issues"]);
        let base = role(&[("jira.create_issue", ActionSetting::Never)]);
        assert_eq!(
            drafts(&tools, &registry(), &role(&[]), &base),
            ["Slack messages"]
        );
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
}
