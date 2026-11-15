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
    assert_eq!(ask.track, None, "track is optional");
}

#[test]
fn skill_track_is_parsed_and_checked() {
    let dir = valid_tree();
    let meta = format!("{GOOD_META}  track: \"Strategy and decisions\"\n");
    write(dir.path(), "skills/ask/SKILL.md", &skill("ask", &meta));
    let report = run(&dir);
    assert!(report.diagnostics.is_empty(), "{:#?}", report.diagnostics);
    assert_eq!(
        report.skills[0].track.as_deref(),
        Some("Strategy and decisions")
    );

    // 40 characters is the most.
    let meta = format!("{GOOD_META}  track: \"{}\"\n", "x".repeat(40));
    write(dir.path(), "skills/ask/SKILL.md", &skill("ask", &meta));
    assert!(run(&dir).diagnostics.is_empty());

    for bad in ["", "   ", &"x".repeat(41)] {
        let meta = format!("{GOOD_META}  track: \"{bad}\"\n");
        write(dir.path(), "skills/ask/SKILL.md", &skill("ask", &meta));
        let report = run(&dir);
        assert_code(&report, "skill-track", Severity::Error);
        assert!(report.skills.is_empty(), "{bad:?}");
    }
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
fn playbooks_are_optional_and_checked() {
    let dir = valid_tree();
    write(
        dir.path(),
        "playbooks/bar.md",
        "---\ntitle: The bar\nowner: \"@pm\"\nversion: \"0.1.0\"\n---\n\n# The bar\n\n1. Problem first.\n",
    );
    write(dir.path(), "playbooks/README.md", "# Playbooks\n");
    assert!(
        run(&dir).diagnostics.is_empty(),
        "{:#?}",
        run(&dir).diagnostics
    );
}

#[test]
fn playbook_needs_owner_version_and_body() {
    let dir = valid_tree();
    // No version: the front matter does not parse.
    write(
        dir.path(),
        "playbooks/strategy.md",
        "---\ntitle: Strategy\nowner: \"@pm\"\n---\n\n# Strategy\n",
    );
    // No body.
    write(
        dir.path(),
        "playbooks/bar.md",
        "---\nowner: \"@pm\"\nversion: \"0.1.0\"\n---\n\n",
    );
    // An unassigned owner is a warning, as for templates.
    write(
        dir.path(),
        "playbooks/ai-products.md",
        "---\nowner: \"TODO\"\nversion: \"0.1.0\"\n---\n\n# AI products\n",
    );
    let report = run(&dir);
    assert_code(&report, "playbook-parse", Severity::Error);
    assert_code(&report, "playbook-body-empty", Severity::Error);
    assert_code(&report, "owner-unassigned", Severity::Warning);
}

#[test]
fn skill_may_use_the_reasoning_model() {
    let dir = valid_tree();
    let meta = GOOD_META.replace("\"default\"", "\"reasoning\"");
    write(dir.path(), "skills/ask/SKILL.md", &skill("ask", &meta));
    let report = run(&dir);
    assert!(report.diagnostics.is_empty(), "{:#?}", report.diagnostics);
    assert_eq!(
        report.skills[0].model,
        pmos_contract::model::ModelRole::Reasoning
    );
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
    assert_eq!(
        names,
        vec![
            "advisory-draft",
            "ai-feature-spec",
            "alignment-plan",
            "ask",
            "bets-register",
            "business-review",
            "call-synth",
            "coach-feedback",
            "competitive-brief",
            "decision-doc",
            "docs",
            "eval-plan",
            "exec-brief",
            "experiment-design",
            "experiment-readout",
            "investment-case",
            "jira-draft",
            "launch-checklist",
            "meeting-notes",
            "metrics-tree",
            "okr-draft",
            "opportunity-sizing",
            "partner-brief",
            "platform-strategy",
            "poc-plan",
            "portfolio-review",
            "prd-draft",
            "pre-mortem",
            "pricing-proposal",
            "prioritize",
            "problem-framing",
            "product-critique",
            "release-notes",
            "review",
            "roadmap-tradeoffs",
            "slack-reply",
            "status-update",
            "strategy-doc",
            "vision-doc",
        ]
    );
}

#[test]
fn team_file_rules() {
    let dir = valid_tree();
    write(
        dir.path(),
        "team.yaml",
        "members:\n  - {handle: ana, slack-id: U0ANA1, roles: [pm, skill-author]}\n  - {handle: Ana, slack-id: U0ANA1, roles: []}\n  - {handle: bo, slack-id: x123, roles: [admin]}\n  - {handle: bo, slack-id: U0BO, roles: [pm]}\n  - {handle: dee, slack-id: U0DEE, email: not-an-email, roles: [pm]}\n",
    );
    std::fs::create_dir_all(dir.path().join("master/people/cy")).unwrap();
    let report = run(&dir);
    for code in [
        "team-handle",
        "team-duplicate",
        "team-slack-id",
        "team-roles",
        "team-email",
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
    assert_eq!(team.by_handle("ana").unwrap().slack_id, "U0ANA1");
    assert!(team.by_handle("ana").unwrap().email.is_none());
    assert!(pmos_contract::model::TeamFile::from_yaml(
        "members:\n  - {handle: a, slack-id: U1, roles: [owner]}\n"
    )
    .is_err());
    let dir = valid_tree();
    write(dir.path(), "team.yaml", "members: [\n");
    assert_code(&run(&dir), "team-parse", Severity::Error);
}

fn role(name: &str, extra: &str) -> String {
    format!("---\nname: {name}\ndescription: Does a job.\nowner: \"@pm\"\nversion: \"0.1.0\"\nskills: \"ask\"\ndata-class: \"internal\"\nmodel: \"default\"\n{extra}---\n\n# Role\n\nInstructions.\n")
}

fn bot(name: &str, extra: &str) -> String {
    format!("---\nname: {name}\nrole: helper\nowner: \"@pm\"\ndescription: A bot.\n{extra}---\n\n# Bot\n\nJob.\n")
}

#[test]
fn roles_and_bots_parse_and_resolve() {
    let dir = valid_tree();
    write(
        dir.path(),
        "team.yaml",
        "members:\n  - {handle: pm, slack-id: U0PM, roles: [pm]}\n",
    );
    write(
        dir.path(),
        "roles/helper/ROLE.md",
        &role("helper", "actions:\n  jira.create_issue: never\n"),
    );
    write(
        dir.path(),
        "bots/ana-bot/BOT.md",
        &bot("ana-bot", "scope: personal\n"),
    );
    write(
        dir.path(),
        "bots/team-bot/BOT.md",
        &bot(
            "team-bot",
            "scope: team\nreads:\n  jira-projects: [PM]\nactions:\n  jira.create_issue: approve\n",
        ),
    );
    let report = run(&dir);
    assert!(
        report.errors().next().is_none(),
        "{:#?}",
        report.diagnostics
    );
    assert_eq!(report.roles.len(), 1);
    assert_eq!(report.bots.len(), 2);
    let role = &report.roles[0];
    let personal = report.bots.iter().find(|b| b.name == "ana-bot").unwrap();
    let team = report.bots.iter().find(|b| b.name == "team-bot").unwrap();
    assert_eq!(personal.owner, "pm");
    use pmos_contract::bots::ActionSetting;
    assert_eq!(
        personal.action_setting(role, "jira.create_issue"),
        ActionSetting::Never
    );
    assert_eq!(
        team.action_setting(role, "jira.create_issue"),
        ActionSetting::Approve
    );
    assert_eq!(team.reads.jira_projects, vec!["PM"]);
    assert_eq!(
        pmos_contract::bots::role_tools(role, &report.skills),
        ["context.search", "jira.create_issue"]
            .map(String::from)
            .into()
    );
}

#[test]
fn role_and_bot_rules() {
    let dir = valid_tree();
    write(
        dir.path(),
        "team.yaml",
        "members:\n  - {handle: pm, slack-id: U0PM, roles: [pm]}\n",
    );
    // auto is refused in v1; actions must name write tools; skills must exist and fit the class.
    write(
        dir.path(),
        "roles/helper/ROLE.md",
        &role(
            "helper",
            "actions:\n  jira.create_issue: auto\n  context.search: approve\n  nope.tool: never\n",
        )
        .replace("skills: \"ask\"", "skills: \"ask ghost\""),
    );
    write(
        dir.path(),
        "roles/wide/ROLE.md",
        &role("wide", "").replace("data-class: \"internal\"", "data-class: \"public\""),
    );
    write(dir.path(), "roles/Bad/ROLE.md", &role("Bad", ""));
    write(
        dir.path(),
        "bots/x/BOT.md",
        &bot("x", "scope: personal\nreads:\n  jira-projects: [PM]\n"),
    );
    write(
        dir.path(),
        "bots/y/BOT.md",
        &bot("y", "scope: team\n").replace("role: helper", "role: missing"),
    );
    write(
        dir.path(),
        "bots/z/BOT.md",
        &bot("z", "scope: team\n").replace("\"@pm\"", "\"@stranger\""),
    );
    write(dir.path(), "bots/w/BOT.md", &bot("other", "scope: team\n"));
    let report = run(&dir);
    for code in [
        "action-auto",
        "action-not-write",
        "action-unknown-tool",
        "role-skill-unknown",
        "role-skill-data-class",
        "role-name",
        "bot-reads-personal",
        "bot-role-unknown",
        "bot-owner-unknown",
        "bot-name",
    ] {
        assert_code(&report, code, Severity::Error);
    }
    // A team bot with no `reads` over a role that reads Jira gets a warning, not an error.
    let dir = valid_tree();
    write(dir.path(), "roles/helper/ROLE.md", &role("helper", ""));
    write(dir.path(), "bots/t/BOT.md", &bot("t", "scope: team\n"));
    let report = run(&dir);
    assert_code(&report, "bot-reads-empty", Severity::Warning);
    // team.yaml lists nobody yet: an unknown owner only warns.
    assert_code(&report, "bot-owner-unknown", Severity::Warning);
}
