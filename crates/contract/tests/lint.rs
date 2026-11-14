//! Integration tests: build a valid content tree, break one thing, check the lint catches it.

use std::fs;
use std::path::Path;

use chrono::NaiveDate;
use pmos_contract::{lint, LintOptions, Severity};
use tempfile::TempDir;

const TODAY: &str = "2026-11-14";

fn today() -> NaiveDate {
    TODAY.parse().unwrap()
}

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn skill(name: &str, meta: &str) -> String {
    format!("---\nname: {name}\ndescription: Does a thing.\nmetadata:\n{meta}---\n\n# /{name}\n\nSteps.\n")
}

const GOOD_META: &str = "  owner: \"@pm\"\n  version: \"0.1.0\"\n  model: \"default\"\n  data-class: \"internal\"\n  tools: \"context.search jira.create_issue\"\n";

fn context(owner: &str, reviewed: &str) -> String {
    format!("---\nowner: \"{owner}\"\nlast-reviewed: {reviewed}\nsensitivity: internal\n---\n\n# Body\n")
}

/// A minimal content tree that passes with no diagnostics.
fn valid_tree() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(
        root,
        "tools.yaml",
        "tools:\n  - name: context.search\n    system: context\n    kind: read\n  - name: jira.create_issue\n    system: jira\n    kind: write\n",
    );
    write(root, "master/AGENTS.md", "# Team\n\nRules.\n");
    write(root, "skills/ask/SKILL.md", &skill("ask", GOOD_META));
    write(root, "context/README.md", "# Context\n");
    write(root, "context/products/a.md", &context("@pm", "2026-11-01"));
    write(
        root,
        "reviewers/eng.md",
        "---\nname: Eng\nlens: Feasibility\nflags:\n  - Vague requirements\n---\n\nRubric.\n",
    );
    write(
        root,
        "templates/prd.md",
        "---\nowner: \"@pm\"\nversion: \"0.1.0\"\n---\n\n# PRD\n",
    );
    dir
}

fn run(dir: &TempDir) -> pmos_contract::Report {
    lint(dir.path(), &LintOptions::new(today()))
}

fn assert_code(report: &pmos_contract::Report, code: &str, severity: Severity) {
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.code == code && d.severity == severity),
        "expected {severity:?} `{code}`, got {:#?}",
        report.diagnostics
    );
}

#[test]
fn valid_tree_is_clean() {
    let dir = valid_tree();
    let report = run(&dir);
    assert!(report.diagnostics.is_empty(), "{:#?}", report.diagnostics);
    assert_eq!(report.skills.len(), 1);
    let ask = &report.skills[0];
    assert_eq!(ask.tools, vec!["context.search", "jira.create_issue"]);
    assert_eq!(ask.surfaces, vec!["slack", "web", "cli"]);
}

#[test]
fn missing_root() {
    let report = lint(Path::new("/nonexistent/pmos"), &LintOptions::new(today()));
    assert_eq!(report.codes(), vec!["root-missing"]);
}

#[test]
fn missing_tools_and_master() {
    let dir = valid_tree();
    fs::remove_file(dir.path().join("tools.yaml")).unwrap();
    fs::remove_file(dir.path().join("master/AGENTS.md")).unwrap();
    let report = run(&dir);
    assert_code(&report, "tools-missing", Severity::Error);
    assert_code(&report, "master-missing", Severity::Error);
}

#[test]
fn duplicate_and_misnamed_tools() {
    let dir = valid_tree();
    write(
        dir.path(),
        "tools.yaml",
        "tools:\n  - name: context.search\n    system: context\n    kind: read\n  - name: context.search\n    system: context\n    kind: read\n  - name: slack.post\n    system: jira\n    kind: write\n",
    );
    let report = run(&dir);
    assert_code(&report, "tool-duplicate", Severity::Error);
    assert_code(&report, "tool-name", Severity::Error);
}

#[test]
fn harness_name_collision() {
    let dir = valid_tree();
    write(
        dir.path(),
        "tools.yaml",
        "tools:\n  - name: context.search\n    system: context\n    kind: read\n  - name: jira.create_issue\n    system: jira\n    kind: write\n  - name: a.b_c\n    system: a\n    kind: read\n  - name: a_b.c\n    system: a_b\n    kind: read\n",
    );
    assert_code(&run(&dir), "tool-harness-collision", Severity::Error);
}

#[test]
fn harness_names_from_registry() {
    let registry = pmos_contract::ToolRegistry::from_yaml(
        "tools:\n  - {name: jira.create_issue, system: jira, kind: write}\n  - {name: confluence.read, system: confluence, kind: read}\n",
    )
    .unwrap();
    let names: Vec<_> = registry.by_harness_name().unwrap().into_keys().collect();
    assert_eq!(names, vec!["confluence_read", "jira_create_issue"]);
    assert_eq!(
        pmos_contract::model::harness_tool_name("slack.post_reply"),
        "slack_post_reply"
    );
}

#[test]
fn unknown_tool_kind_is_a_parse_error() {
    let dir = valid_tree();
    write(
        dir.path(),
        "tools.yaml",
        "tools:\n  - name: context.search\n    system: context\n    kind: delete\n",
    );
    assert_code(&run(&dir), "tools-parse", Severity::Error);
}

#[test]
fn skill_unknown_tool() {
    let dir = valid_tree();
    let meta = GOOD_META.replace("jira.create_issue", "jira.delete_everything");
    write(dir.path(), "skills/ask/SKILL.md", &skill("ask", &meta));
    let report = run(&dir);
    assert_code(&report, "skill-unknown-tool", Severity::Error);
    assert!(report.skills.is_empty());
}

#[test]
fn skill_name_must_match_directory() {
    let dir = valid_tree();
    write(
        dir.path(),
        "skills/ask/SKILL.md",
        &skill("ask-me", GOOD_META),
    );
    assert_code(&run(&dir), "skill-name", Severity::Error);
}

#[test]
fn skill_name_must_be_lowercase() {
    let dir = valid_tree();
    write(dir.path(), "skills/Ask/SKILL.md", &skill("Ask", GOOD_META));
    assert_code(&run(&dir), "skill-name", Severity::Error);
}

#[test]
fn skill_missing_metadata() {
    let dir = valid_tree();
    let meta = GOOD_META.replace("  data-class: \"internal\"\n", "");
    write(dir.path(), "skills/ask/SKILL.md", &skill("ask", &meta));
    assert_code(&run(&dir), "skill-metadata-missing", Severity::Error);
}

#[test]
fn skill_metadata_must_be_strings() {
    let dir = valid_tree();
    let meta = GOOD_META.replace("version: \"0.1.0\"", "version: 1");
    write(dir.path(), "skills/ask/SKILL.md", &skill("ask", &meta));
    assert_code(&run(&dir), "skill-metadata-type", Severity::Error);
}

#[test]
fn skill_bad_enums_and_version() {
    let dir = valid_tree();
    let meta = GOOD_META
        .replace("\"default\"", "\"frontier\"")
        .replace("\"internal\"", "\"secret\"")
        .replace("\"0.1.0\"", "\"0.1\"")
        + "  surfaces: \"slack email\"\n  ships-in: \"someday\"\n";
    write(dir.path(), "skills/ask/SKILL.md", &skill("ask", &meta));
    let report = run(&dir);
    for code in [
        "skill-model",
        "skill-data-class",
        "skill-version",
        "skill-surface",
        "skill-phase",
    ] {
        assert_code(&report, code, Severity::Error);
    }
}

#[test]
fn skill_without_skill_md() {
    let dir = valid_tree();
    fs::create_dir_all(dir.path().join("skills/empty")).unwrap();
    assert_code(&run(&dir), "skill-missing-file", Severity::Error);
}

#[test]
fn skill_without_front_matter() {
    let dir = valid_tree();
    write(
        dir.path(),
        "skills/ask/SKILL.md",
        "# /ask\n\nNo front matter.\n",
    );
    assert_code(&run(&dir), "front-matter", Severity::Error);
}

#[test]
fn context_stale_is_a_warning() {
    let dir = valid_tree();
    write(
        dir.path(),
        "context/products/a.md",
        &context("@pm", "2026-08-01"),
    );
    let report = run(&dir);
    assert_code(&report, "context-stale", Severity::Warning);
    assert!(!report.has_errors());
}

#[test]
fn context_exactly_ninety_days_is_not_stale() {
    let dir = valid_tree();
    write(
        dir.path(),
        "context/products/a.md",
        &context("@pm", "2026-08-16"),
    );
    assert!(run(&dir).diagnostics.is_empty());
}

#[test]
fn context_future_date_is_an_error() {
    let dir = valid_tree();
    write(
        dir.path(),
        "context/products/a.md",
        &context("@pm", "2027-01-01"),
    );
    assert_code(&run(&dir), "context-future-date", Severity::Error);
}

#[test]
fn context_unknown_domain() {
    let dir = valid_tree();
    write(
        dir.path(),
        "context/rumors/a.md",
        &context("@pm", "2026-11-01"),
    );
    write(
        dir.path(),
        "context/loose.md",
        &context("@pm", "2026-11-01"),
    );
    let report = run(&dir);
    assert_eq!(
        report
            .diagnostics
            .iter()
            .filter(|d| d.code == "context-domain")
            .count(),
        2
    );
}

#[test]
fn context_missing_field_and_typo() {
    let dir = valid_tree();
    write(
        dir.path(),
        "context/products/a.md",
        "---\nowner: \"@pm\"\nsensitivity: internal\n---\n\nBody\n",
    );
    write(
        dir.path(),
        "context/products/b.md",
        "---\nowner: \"@pm\"\nlast_reviewed: 2026-11-01\nlast-reviewed: 2026-11-01\nsensitivity: internal\n---\n\nBody\n",
    );
    let report = run(&dir);
    let parse_errors: Vec<_> = report
        .diagnostics
        .iter()
        .filter(|d| d.code == "context-parse")
        .map(|d| d.path.as_str())
        .collect();
    assert_eq!(
        parse_errors,
        vec!["context/products/a.md", "context/products/b.md"]
    );
}

#[test]
fn context_bad_sensitivity() {
    let dir = valid_tree();
    write(
        dir.path(),
        "context/products/a.md",
        "---\nowner: \"@pm\"\nlast-reviewed: 2026-11-01\nsensitivity: top-secret\n---\n\nBody\n",
    );
    assert_code(&run(&dir), "context-parse", Severity::Error);
}

#[test]
fn owner_todo_warns_and_empty_errors() {
    let dir = valid_tree();
    write(
        dir.path(),
        "context/products/a.md",
        &context("TODO", "2026-11-01"),
    );
    write(
        dir.path(),
        "context/products/b.md",
        &context(" ", "2026-11-01"),
    );
    let report = run(&dir);
    assert_code(&report, "owner-unassigned", Severity::Warning);
    assert_code(&report, "owner-missing", Severity::Error);
}

#[test]
fn reviewer_needs_flags_and_rubric() {
    let dir = valid_tree();
    write(
        dir.path(),
        "reviewers/eng.md",
        "---\nname: Eng\nlens: Feasibility\nflags: []\n---\n\n",
    );
    let report = run(&dir);
    assert_code(&report, "reviewer-flags", Severity::Error);
    assert_code(&report, "reviewer-body-empty", Severity::Error);
}

#[test]
fn template_bad_version() {
    let dir = valid_tree();
    write(
        dir.path(),
        "templates/prd.md",
        "---\nowner: \"@pm\"\nversion: \"one\"\n---\n\n# PRD\n",
    );
    assert_code(&run(&dir), "template-version", Severity::Error);
}

#[test]
fn readme_files_are_ignored() {
    let dir = valid_tree();
    write(dir.path(), "reviewers/README.md", "# Reviewers\n");
    write(dir.path(), "templates/README.md", "# Templates\n");
    write(dir.path(), "context/products/README.md", "# Products\n");
    assert!(run(&dir).diagnostics.is_empty());
}

/// The repo's own seed content must pass with no errors.
#[test]
fn repo_content_passes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
    let report = lint(&root, &LintOptions::new(today()));
    let errors: Vec<_> = report.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let names: Vec<_> = report.skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["ask", "docs", "meeting-notes", "prd-draft"]);
}

#[test]
fn team_file_rules() {
    let dir = valid_tree();
    write(
        dir.path(),
        "team.yaml",
        "members:\n  - {handle: ana, slack-id: U0ANA1, roles: [pm, skill-author]}\n  - {handle: Ana, slack-id: U0ANA1, roles: []}\n  - {handle: bo, slack-id: x123, roles: [admin]}\n  - {handle: bo, slack-id: U0BO, roles: [pm]}\n",
    );
    std::fs::create_dir_all(dir.path().join("master/people/cy")).unwrap();
    let report = run(&dir);
    for code in [
        "team-handle",
        "team-duplicate",
        "team-slack-id",
        "team-roles",
    ] {
        assert_code(&report, code, Severity::Error);
    }
    assert_code(&report, "people-unknown", Severity::Warning);
    // Both duplicate kinds are reported: the Slack id and the handle.
    assert_eq!(
        report
            .codes()
            .iter()
            .filter(|c| **c == "team-duplicate")
            .count(),
        2
    );
}

#[test]
fn team_file_parses_and_looks_up_by_slack_id() {
    let team = pmos_contract::model::TeamFile::from_yaml(
        "members:\n  - {handle: ana, slack-id: U0ANA1, roles: [pm]}\n",
    )
    .unwrap();
    assert_eq!(team.by_slack_id("U0ANA1").unwrap().handle, "ana");
    assert!(team.by_slack_id("U0BO").is_none());
    assert!(pmos_contract::model::TeamFile::from_yaml(
        "members:\n  - {handle: a, slack-id: U1, roles: [owner]}\n"
    )
    .is_err());
    let dir = valid_tree();
    write(dir.path(), "team.yaml", "members: [\n");
    assert_code(&run(&dir), "team-parse", Severity::Error);
}
