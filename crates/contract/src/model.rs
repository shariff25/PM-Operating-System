//! Typed forms of the PM OS content files. See `docs/contract.md`.

use std::collections::BTreeMap;
use std::fmt;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// Data class of a context file or of the data a skill handles (PRD: Security, data classes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sensitivity {
    Public,
    Internal,
    Restricted,
}

impl Sensitivity {
    pub const ALL: [&'static str; 3] = ["public", "internal", "restricted"];

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "public" => Some(Self::Public),
            "internal" => Some(Self::Internal),
            "restricted" => Some(Self::Restricted),
            _ => None,
        }
    }
}

impl fmt::Display for Sensitivity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Public => "public",
            Self::Internal => "internal",
            Self::Restricted => "restricted",
        })
    }
}

/// What a tool may change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolKind {
    /// Reads only, under the user's own permissions.
    Read,
    /// Changes PM OS's own state only, such as the user's memory.
    Internal,
    /// Writes outside PM OS (Jira, Confluence, Slack). Every call needs a human approval
    /// per action (OUT-4): the harness pauses on it and the tool server queues it.
    Write,
}

/// One entry of `content/tools.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolDef {
    pub name: String,
    pub system: String,
    pub kind: ToolKind,
    #[serde(default)]
    pub description: Option<String>,
}

/// `content/tools.yaml`: every tool the tool server exposes. A skill may only declare tools
/// listed here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolRegistry {
    pub tools: Vec<ToolDef>,
}

impl ToolRegistry {
    /// Parse `tools.yaml`.
    pub fn from_yaml(text: &str) -> Result<Self, serde_norway::Error> {
        serde_norway::from_str(text)
    }

    pub fn get(&self, name: &str) -> Option<&ToolDef> {
        self.tools.iter().find(|t| t.name == name)
    }

    pub fn write_tools(&self) -> impl Iterator<Item = &ToolDef> {
        self.tools.iter().filter(|t| t.kind == ToolKind::Write)
    }

    /// Map of harness tool name to tool. Errors name the first pair that collides.
    pub fn by_harness_name(&self) -> Result<BTreeMap<String, &ToolDef>, String> {
        let mut map: BTreeMap<String, &ToolDef> = BTreeMap::new();
        for tool in &self.tools {
            let name = harness_tool_name(&tool.name);
            if let Some(existing) = map.get(&name) {
                return Err(format!(
                    "tools `{}` and `{}` both map to harness name `{name}`",
                    existing.name, tool.name
                ));
            }
            map.insert(name, tool);
        }
        Ok(map)
    }
}

/// The name a tool has in MCP and inside a harness. Registry names use dots
/// (`jira.create_issue`); OpenAI-style function names allow only letters, digits, `_` and
/// `-`, so dots become `_`. The TypeScript harness applies the same rule
/// (`harness/src/registry.ts`).
pub fn harness_tool_name(name: &str) -> String {
    name.replace('.', "_")
}

/// Front matter of a `SKILL.md`, as the Agent Skills spec defines it. PM OS fields live in
/// `metadata` so any spec-compliant harness (Deep Agents, Hermes) loads the same file.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SkillFrontMatter {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub compatibility: Option<String>,
    #[serde(default)]
    pub allowed_tools: Option<String>,
    #[serde(default)]
    pub metadata: BTreeMap<String, serde_norway::Value>,
}

/// Which gateway model role a skill is pinned to. The gateway maps roles to concrete
/// open-weight models, so the bake-off can change a model without touching skills.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelRole {
    /// Drafting and tool calls (Qwen3.6-35B-A3B at launch).
    Default,
    /// Extraction and triage (Nemotron 3 Nano 30B-A3B at launch).
    Extraction,
    /// The PM assistant's drafting and critique (the `default` model at launch; the bake-off
    /// may point it at a larger one).
    Reasoning,
}

impl ModelRole {
    pub const ALL: [&'static str; 3] = ["default", "extraction", "reasoning"];

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "default" => Some(Self::Default),
            "extraction" => Some(Self::Extraction),
            "reasoning" => Some(Self::Reasoning),
            _ => None,
        }
    }
}

pub const SURFACES: [&str; 3] = ["slack", "web", "cli"];
pub const PHASES: [&str; 4] = ["phase-0", "pilot", "team-launch", "later"];

/// A validated skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub owner: String,
    pub version: String,
    pub model: ModelRole,
    pub data_class: Sensitivity,
    pub tools: Vec<String>,
    pub surfaces: Vec<String>,
    pub ships_in: Option<String>,
    /// The catalog heading the skill is listed under, such as "Everyday" (`metadata.track`).
    pub track: Option<String>,
}

/// Domains of the context library (CTX-1), plus `exemplars` for past PRDs and plans (CTX-4).
pub const CONTEXT_DOMAINS: [&str; 9] = [
    "products",
    "personas",
    "competitors",
    "partners",
    "stakeholders",
    "style",
    "glossary",
    "decisions",
    "exemplars",
];

/// Front matter of a context library file (CTX-2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ContextFrontMatter {
    #[serde(default)]
    pub title: Option<String>,
    pub owner: String,
    pub last_reviewed: NaiveDate,
    pub sensitivity: Sensitivity,
}

/// Parse a context file's front matter (the YAML between the `---` lines).
pub fn parse_context_front_matter(yaml: &str) -> Result<ContextFrontMatter, serde_norway::Error> {
    serde_norway::from_str(yaml)
}

/// Front matter of a reviewer persona (REV-2). The body is the rubric.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ReviewerFrontMatter {
    pub name: String,
    pub lens: String,
    pub flags: Vec<String>,
    #[serde(default)]
    pub owner: Option<String>,
}

/// Front matter of a template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct TemplateFrontMatter {
    #[serde(default)]
    pub title: Option<String>,
    pub owner: String,
    pub version: String,
}

/// Front matter of a playbook (`content/playbooks/<name>.md`): the craft guidance a skill
/// reads before it drafts. The body is the playbook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PlaybookFrontMatter {
    #[serde(default)]
    pub title: Option<String>,
    pub owner: String,
    pub version: String,
}

/// A role in PM OS (ADM-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    Pm,
    SkillAuthor,
    /// Platform owner: runs the Spark, reviews sandbox policies, approves new members.
    Admin,
}

impl Role {
    /// The name as written in `team.yaml`.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Pm => "pm",
            Role::SkillAuthor => "skill-author",
            Role::Admin => "admin",
        }
    }
}

impl TeamMember {
    pub fn is_admin(&self) -> bool {
        self.roles.contains(&Role::Admin)
    }
}

/// One person who may use PM OS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct TeamMember {
    /// PM OS handle: lowercase letters, digits, `.`, `_` and `-`.
    pub handle: String,
    /// Slack member id, such as `U012AB3CD`.
    pub slack_id: String,
    /// Atlassian account email. Linking Jira and Confluence checks the signed-in account
    /// has this email, so a forwarded link can't attach someone else's account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// GitHub login in your GitHub org. Optional: linking GitHub checks this login when it is
    /// set, and otherwise a verified email on the GitHub account matching `email`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<String>,
    pub roles: Vec<Role>,
}

/// `content/team.yaml`: who may use PM OS, until SSO replaces it (Phase 1).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamFile {
    #[serde(default)]
    pub members: Vec<TeamMember>,
}

impl TeamFile {
    pub fn from_yaml(text: &str) -> Result<Self, serde_norway::Error> {
        serde_norway::from_str(text)
    }

    /// The member with this Slack id.
    pub fn by_slack_id(&self, slack_id: &str) -> Option<&TeamMember> {
        self.members.iter().find(|m| m.slack_id == slack_id)
    }

    /// The platform owners: members with the `admin` role.
    pub fn admins(&self) -> impl Iterator<Item = &TeamMember> {
        self.members.iter().filter(|m| m.is_admin())
    }

    /// The member with this PM OS handle.
    pub fn by_handle(&self, handle: &str) -> Option<&TeamMember> {
        self.members.iter().find(|m| m.handle == handle)
    }

    /// `<content_root>/team.yaml`. A missing file is an empty team; a file that doesn't parse
    /// is an error, so a typo never silently opens or closes access.
    pub fn load(content_root: &std::path::Path) -> Result<Self, String> {
        let path = content_root.join("team.yaml");
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::from_yaml(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }
}

/// A plausible email address: one `@`, something either side, a dot in the domain, no
/// spaces. Deliverability isn't checked; this catches typos in `team.yaml`.
pub fn valid_email(email: &str) -> bool {
    match email.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && !domain.contains('@')
                && !email.chars().any(char::is_whitespace)
        }
        None => false,
    }
}

/// A valid PM OS handle (the tool server and harness apply the same rule).
pub fn valid_handle(handle: &str) -> bool {
    let mut chars = handle.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
        && handle.len() <= 64
}

/// A Slack member id: `U` or `W`, then uppercase letters and digits.
pub fn valid_slack_id(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some('U' | 'W'))
        && id.len() >= 3
        && id.len() <= 32
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_roles_parse() {
        for name in ModelRole::ALL {
            let role = ModelRole::parse(name).expect(name);
            let yaml = serde_norway::to_string(&role).unwrap();
            assert_eq!(yaml.trim(), name);
        }
        assert_eq!(ModelRole::parse("reasoning"), Some(ModelRole::Reasoning));
        assert_eq!(ModelRole::parse("Reasoning"), None);
        assert_eq!(ModelRole::parse("large"), None);
    }
}
