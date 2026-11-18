//! The tools and skills the server serves, loaded from the content repo at start-up.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use chrono::NaiveDate;
use pmos_contract::bots::{Bot, Role};
use pmos_contract::model::harness_tool_name;
use pmos_contract::{lint, LintOptions, Skill, ToolKind, ToolRegistry};
use serde::Serialize;
use serde_json::Value;

use crate::schemas;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolSpec {
    /// Registry name, such as `jira.create_issue`.
    pub name: String,
    /// MCP and harness name, such as `jira_create_issue`.
    pub harness_name: String,
    pub system: String,
    pub kind: ToolKind,
    pub description: String,
    pub input_schema: Value,
}

impl ToolSpec {
    /// The action part of the name, such as `create_issue`.
    pub fn action(&self) -> &str {
        self.name
            .split_once('.')
            .map(|(_, action)| action)
            .unwrap_or(&self.name)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("content does not pass `pmos lint`:\n{0}")]
    Lint(String),
    #[error("cannot read tools.yaml: {0}")]
    Registry(String),
    #[error("tool `{0}` is in tools.yaml but the tool server has no input schema for it")]
    MissingSchema(String),
}

/// The named bots and their roles (`content/bots/`, `content/roles/`). Loaded at start-up
/// with the skills, and reloaded on `POST /internal/reindex`.
#[derive(Debug, Clone, Default)]
pub struct Roster {
    roles: BTreeMap<String, Arc<Role>>,
    bots: BTreeMap<String, Arc<Bot>>,
}

impl Roster {
    /// The roles and bots of a lint report that passed.
    pub fn from_report(report: &pmos_contract::Report) -> Self {
        let roster = Self {
            roles: report
                .roles
                .iter()
                .map(|r| (r.name.clone(), Arc::new(r.clone())))
                .collect(),
            bots: report
                .bots
                .iter()
                .map(|b| (b.name.clone(), Arc::new(b.clone())))
                .collect(),
        };
        for bot in roster.bots.values() {
            let reads = &bot.reads;
            let bad = reads
                .jira_projects
                .iter()
                .chain(&reads.confluence_spaces)
                .filter(|k| !crate::connectors::scope::valid_key(k))
                .chain(
                    reads
                        .slack_channels
                        .iter()
                        .filter(|c| !crate::connectors::scope::valid_channel(c)),
                );
            for entry in bad {
                tracing::warn!(bot = %bot.name, entry = %entry, "read scope entry isn't a valid key or channel id; reads in that system are refused");
            }
        }
        roster
    }

    /// A bot and its role. `None` if either is missing.
    pub fn bot(&self, name: &str) -> Option<(Arc<Bot>, Arc<Role>)> {
        let bot = self.bots.get(name)?;
        let role = self.roles.get(&bot.role)?;
        Some((bot.clone(), role.clone()))
    }

    pub fn bots(&self) -> impl Iterator<Item = &Bot> {
        self.bots.values().map(|b| b.as_ref())
    }

    pub fn roles(&self) -> impl Iterator<Item = &Role> {
        self.roles.values().map(|r| r.as_ref())
    }
}

#[derive(Debug, Clone)]
pub struct Catalog {
    tools: BTreeMap<String, ToolSpec>,
    skills: BTreeMap<String, Skill>,
    roster: Roster,
    content_sha: Option<String>,
}

/// The commit the content checkout at `content_root` is on (`git rev-parse HEAD`), or
/// `None` outside a git checkout. The ledger records it with every draft.
pub fn content_head(content_root: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(content_root)
        .args(["rev-parse", "HEAD"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let head = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let valid = out.status.success()
        && (40..=64).contains(&head.len())
        && head.chars().all(|c| c.is_ascii_hexdigit());
    valid.then_some(head)
}

impl Catalog {
    pub fn load(content_root: &Path, today: NaiveDate) -> Result<Self, CatalogError> {
        let report = lint(content_root, &LintOptions::for_service(today));
        if report.has_errors() {
            let lines: Vec<String> = report
                .errors()
                .map(|d| format!("  {}: {}: {}", d.code, d.path, d.message))
                .collect();
            return Err(CatalogError::Lint(lines.join("\n")));
        }
        let text = std::fs::read_to_string(content_root.join("tools.yaml"))
            .map_err(|e| CatalogError::Registry(e.to_string()))?;
        let registry =
            ToolRegistry::from_yaml(&text).map_err(|e| CatalogError::Registry(e.to_string()))?;

        let mut tools = BTreeMap::new();
        for tool in &registry.tools {
            let input_schema = schemas::input_schema(&tool.name)
                .ok_or_else(|| CatalogError::MissingSchema(tool.name.clone()))?;
            let harness_name = harness_tool_name(&tool.name);
            tools.insert(
                harness_name.clone(),
                ToolSpec {
                    name: tool.name.clone(),
                    harness_name,
                    system: tool.system.clone(),
                    kind: tool.kind,
                    description: tool.description.clone().unwrap_or_default(),
                    input_schema,
                },
            );
        }
        let roster = Roster::from_report(&report);
        let skills = report
            .skills
            .into_iter()
            .map(|s| (s.name.clone(), s))
            .collect();
        Ok(Self {
            tools,
            skills,
            roster,
            content_sha: content_head(content_root),
        })
    }

    /// The content repo's `HEAD` when this catalog was loaded; `None` outside a git
    /// checkout. [`crate::ToolService`] keeps the live value, which reindexing refreshes.
    pub fn content_sha(&self) -> Option<&str> {
        self.content_sha.as_deref()
    }

    /// The bots and roles as they were at start-up. [`crate::ToolService`] keeps the live
    /// copy, which `POST /internal/reindex` replaces.
    pub fn roster(&self) -> &Roster {
        &self.roster
    }

    pub fn skill(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    pub fn skills(&self) -> impl Iterator<Item = &Skill> {
        self.skills.values()
    }

    /// A tool by its harness name.
    pub fn tool(&self, harness_name: &str) -> Option<&ToolSpec> {
        self.tools.get(harness_name)
    }

    pub fn tools(&self) -> impl Iterator<Item = &ToolSpec> {
        self.tools.values()
    }

    /// The tools a skill declared, in harness-name order.
    pub fn tools_for(&self, skill: &Skill) -> Vec<&ToolSpec> {
        self.tools
            .values()
            .filter(|t| skill.tools.contains(&t.name))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn today() -> NaiveDate {
        "2026-11-14".parse().unwrap()
    }

    fn repo_content() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content")
    }

    #[test]
    fn loads_repo_content() {
        let catalog = Catalog::load(&repo_content(), today()).unwrap();
        let prd = catalog.skill("prd-draft").unwrap();
        let names: Vec<_> = catalog
            .tools_for(prd)
            .iter()
            .map(|t| t.harness_name.as_str())
            .collect();
        assert!(names.contains(&"confluence_create_draft"));
        assert!(!names.contains(&"slack_post_reply"));
        let create = catalog.tool("jira_create_issue").unwrap();
        assert_eq!(create.action(), "create_issue");
        assert_eq!(create.kind, ToolKind::Write);

        let (bot, role) = catalog.roster().bot("docs").unwrap();
        assert_eq!(role.name, "docs-helper");
        assert_eq!(bot.role, "docs-helper");
        assert!(catalog.roster().bot("nobody").is_none());
        assert!(catalog.roster().roles().any(|r| r.name == "pm-assistant"));
    }

    #[test]
    fn content_head_is_the_checkout_commit() {
        // The repo's own content is in a git checkout (or a worktree of one).
        let head = content_head(&repo_content()).expect("content is in a git checkout");
        assert!(head.len() >= 40 && head.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(
            Catalog::load(&repo_content(), today())
                .unwrap()
                .content_sha(),
            Some(head.as_str())
        );
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(content_head(dir.path()), None);
    }

    #[test]
    fn refuses_content_with_lint_errors() {
        let dir = tempfile::tempdir().unwrap();
        let err = Catalog::load(dir.path(), today()).unwrap_err();
        assert!(matches!(err, CatalogError::Lint(_)));
    }
}
