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
    pub fn get(&self, name: &str) -> Option<&ToolDef> {
        self.tools.iter().find(|t| t.name == name)
    }

    pub fn write_tools(&self) -> impl Iterator<Item = &ToolDef> {
        self.tools.iter().filter(|t| t.kind == ToolKind::Write)
    }
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
}

impl ModelRole {
    pub const ALL: [&'static str; 2] = ["default", "extraction"];

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "default" => Some(Self::Default),
            "extraction" => Some(Self::Extraction),
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
