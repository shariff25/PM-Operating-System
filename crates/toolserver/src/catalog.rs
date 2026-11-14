//! The tools and skills the server serves, loaded from the content repo at start-up.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::NaiveDate;
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

#[derive(Debug, Clone)]
pub struct Catalog {
    tools: BTreeMap<String, ToolSpec>,
    skills: BTreeMap<String, Skill>,
}

impl Catalog {
    pub fn load(content_root: &Path, today: NaiveDate) -> Result<Self, CatalogError> {
        let report = lint(content_root, &LintOptions::new(today));
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
        let skills = report
            .skills
            .into_iter()
            .map(|s| (s.name.clone(), s))
            .collect();
        Ok(Self { tools, skills })
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
    }

    #[test]
    fn refuses_content_with_lint_errors() {
        let dir = tempfile::tempdir().unwrap();
        let err = Catalog::load(dir.path(), today()).unwrap_err();
        assert!(matches!(err, CatalogError::Lint(_)));
    }
}
