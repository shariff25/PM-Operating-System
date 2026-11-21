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

/// A role in PM OS (ADM-1). What each role may do is [`crate::permissions`].
///
/// Users and their roles live in the PM OS user database (`docs/user-database-spec.md`); the
/// tool server stores the four roles there. This enum is the shared vocabulary for the
/// permission matrix, and the import of an old `team.yaml` (`pmos-admin users import`, the
/// only code that still parses that file) reads its roles with [`Role::parse_tolerant`]: a
/// well-formed name this build doesn't know becomes [`Role::Unknown`], which grants no
/// permission, so the import can name it and stop. A malformed name is an error there.
///
/// Deserializing a `Role` on its own, as an API body or a CLI argument does, is strict: an
/// unknown name is an error, because it is a typo. Use [`Role::parse`] for input.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Role {
    /// Product Manager: asks bots and decides their own approvals.
    Pm,
    /// Edits skills and bots by pull request; otherwise as `pm`.
    SkillAuthor,
    /// Platform owner: runs the Spark, reviews sandbox policies, approves new members.
    Admin,
    /// A PM who also sees other PMs' approvals and recommends a decision. The draft's owner
    /// still decides; a reviewer never decides someone else's approval.
    Reviewer,
    /// Reads audit metadata and exports it. Nothing else: no trace bodies, no prompt or
    /// Slack text, no asking bots.
    Auditor,
    /// A well-formed role name this build doesn't know (from an imported file). Grants
    /// nothing.
    Unknown(String),
}

impl Role {
    /// Every role this build knows, by name.
    pub const KNOWN: [&'static str; 5] = ["pm", "skill-author", "admin", "reviewer", "auditor"];

    /// The role's name.
    pub fn as_str(&self) -> &str {
        match self {
            Role::Pm => "pm",
            Role::SkillAuthor => "skill-author",
            Role::Admin => "admin",
            Role::Reviewer => "reviewer",
            Role::Auditor => "auditor",
            Role::Unknown(name) => name,
        }
    }

    /// A known role by its name. Strict: anything else is `None`.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "pm" => Some(Role::Pm),
            "skill-author" => Some(Role::SkillAuthor),
            "admin" => Some(Role::Admin),
            "reviewer" => Some(Role::Reviewer),
            "auditor" => Some(Role::Auditor),
            _ => None,
        }
    }

    /// A role as read from an imported file: a known role, else [`Role::Unknown`] if the name is
    /// well formed (lowercase letters, digits and single hyphens, at most 32), else `None`.
    pub fn parse_tolerant(name: &str) -> Option<Self> {
        Self::parse(name).or_else(|| {
            let well_formed = name.len() <= 32
                && name.starts_with(|c: char| c.is_ascii_lowercase())
                && !name.ends_with('-')
                && !name.contains("--")
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
            well_formed.then(|| Role::Unknown(name.to_string()))
        })
    }

    /// Whether this build knows the role.
    pub fn is_known(&self) -> bool {
        !matches!(self, Role::Unknown(_))
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for Role {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

/// Strict: only a known role name.
impl<'de> Deserialize<'de> for Role {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let name = String::deserialize(d)?;
        Role::parse(&name).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "unknown role `{name}`, expected one of {}",
                Role::KNOWN.join(", ")
            ))
        })
    }
}

/// A plausible email address: one `@`, something either side, a dot in the domain, no
/// spaces. Deliverability isn't checked; this catches typos.
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

/// An Entra ID object id: a GUID, 8-4-4-4-12 hex digits, in either case.
pub fn valid_entra_oid(oid: &str) -> bool {
    let parts: Vec<&str> = oid.trim().split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(p, n)| p.len() == n && p.chars().all(|c| c.is_ascii_hexdigit()))
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

    #[test]
    fn a_role_on_its_own_parses_strictly() {
        // API bodies and CLI arguments: an unknown name is a typo, not a newer file.
        assert!(serde_norway::from_str::<Role>("policy-steward").is_err());
        assert!(serde_norway::from_str::<Role>("admn").is_err());
        assert_eq!(Role::parse("admn"), None);
        assert_eq!(
            Role::parse_tolerant("admn"),
            Some(Role::Unknown("admn".into()))
        );
        assert_eq!(Role::parse_tolerant("Admin"), None);
    }
}
