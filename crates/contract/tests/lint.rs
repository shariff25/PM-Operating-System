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
            "battlecard",
            "bets-register",
            "business-review",
            "call-synth",
            "coach-feedback",
            "competitive-brief",
            "daily-brief",
            "decision-doc",
            "docs",
            "eval-plan",
            "exec-brief",
            "experiment-backlog",
            "experiment-design",
            "experiment-readout",
            "funnel-diagnosis",
            "investment-case",
            "jira-draft",
            "launch-checklist",
            "meeting-notes",
            "meeting-prep",
            "metrics-tree",
            "okr-draft",
            "opportunity-sizing",
            "partner-brief",
            "platform-strategy",
            "poc-plan",
            "portfolio-review",
            "positioning-brief",
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
            "week-review",
        ]
    );

    // The bot marketplace's catalog (docs/bot-marketplace-spec.md §3): Nudge, then the three
    // specialists, each with exactly the skills the spec gives it and the writes it names.
    let personas: Vec<_> = report.personas.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(personas, vec!["nudge", "chief", "sprout", "echo"]);
    assert!(report.personas.iter().all(|p| p.offered));
    let role = |name: &str| report.roles.iter().find(|r| r.name == name).unwrap();
    let skills = |name: &str| -> Vec<&str> {
        let mut v: Vec<&str> = role(name).skills.iter().map(String::as_str).collect();
        v.sort_unstable();
        v
    };
    let sorted = |s: &str| -> Vec<String> {
        let mut v: Vec<String> = s.split(' ').map(str::to_owned).collect();
        v.sort_unstable();
        v
    };
    assert_eq!(
        skills("chief-of-staff"),
        sorted("ask daily-brief meeting-prep week-review meeting-notes status-update exec-brief decision-doc okr-draft slack-reply jira-draft")
    );
    assert_eq!(
        skills("growth-pm"),
        sorted("ask problem-framing metrics-tree funnel-diagnosis experiment-design experiment-readout experiment-backlog opportunity-sizing pricing-proposal jira-draft")
    );
    assert_eq!(
        skills("outbound-pm"),
        sorted("ask release-notes launch-checklist positioning-brief battlecard competitive-brief call-synth partner-brief advisory-draft poc-plan jira-draft")
    );
    let drafts = |name: &str| -> Vec<String> {
        let p = report.personas.iter().find(|p| p.name == name).unwrap();
        let mut d = p.drafts.clone();
        d.sort_unstable();
        d
    };
    // Chief never starts a Slack conversation; Sprout and Echo have no Slack write at all.
    assert_eq!(
        drafts("chief"),
        vec!["Confluence pages", "Jira issues", "Slack replies"]
    );
    assert_eq!(drafts("sprout"), vec!["Confluence pages", "Jira issues"]);
    assert_eq!(drafts("echo"), vec!["Confluence pages", "Jira issues"]);
    // Only Sprout needs a source, and PM OS has no analytics tool yet.
    for p in &report.personas {
        let expected = usize::from(p.name == "sprout");
        assert_eq!(p.needs.len(), expected, "{}", p.name);
        assert!(p.needs.iter().all(|n| n.source == "analytics" && !n.met));
    }
    // Each specialist's role carries the base role's three standing sections verbatim
    // (spec §3; the lint has no shared include).
    let base = &role("pm-assistant").instructions;
    let standing = &base[base.find("## Source markers in drafts").unwrap()..];
    for name in ["chief-of-staff", "growth-pm", "outbound-pm"] {
        assert!(
            role(name).instructions.contains(standing.trim_end()),
            "{name} is missing the standing sections"
        );
    }
}

/// Slice U7: `team.yaml` is retired. One anywhere in the content tree is an error for
/// `pmos lint` (and CI), pointing to the import; a service loading content gets the same
/// finding as a warning, so a leftover file never stops it. The file isn't read: one that
/// doesn't parse gets the same single finding, and nothing else in the report changes.
#[test]
fn a_team_file_is_retired() {
    let dir = valid_tree();
    let clean = run(&dir);
    write(
        dir.path(),
        "team.yaml",
        "members:\n  - {handle: mallory, slack-id: U0ANA, email: ana@example.com, roles: [pm, admin]}\n",
    );
    let report = run(&dir);
    assert_code(&report, "team-file-retired", Severity::Error);
    assert_eq!(report.errors().count(), 1, "{:#?}", report.diagnostics);
    let d = report.errors().next().unwrap();
    assert_eq!(d.path, "team.yaml");
    assert!(
        d.message.contains("pmos-admin users import"),
        "{}",
        d.message
    );
    assert_eq!(report.skills, clean.skills);
    assert_eq!(report.bots, clean.bots);

    // Not parsed: garbage gets the same one finding.
    write(dir.path(), "team.yaml", "members: [\n");
    assert_eq!(run(&dir).codes(), vec!["team-file-retired"]);

    // Anywhere in the tree.
    std::fs::remove_file(dir.path().join("team.yaml")).unwrap();
    write(dir.path(), "context/products/team.yaml", "members: []\n");
    let report = run(&dir);
    assert_code(&report, "team-file-retired", Severity::Error);
    assert_eq!(
        report.errors().next().unwrap().path,
        "context/products/team.yaml"
    );

    // A service loading the content: a warning, the content still loads.
    let report = lint(dir.path(), &LintOptions::for_service(today()));
    assert_code(&report, "team-file-retired", Severity::Warning);
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
}

/// A personal context folder is named for a handle; who that person is is the database's.
#[test]
fn people_folders_are_handles() {
    let dir = valid_tree();
    std::fs::create_dir_all(dir.path().join("master/people/cy")).unwrap();
    assert!(run(&dir).diagnostics.is_empty());
    std::fs::create_dir_all(dir.path().join("master/people/Not A Handle")).unwrap();
    assert_code(&run(&dir), "people-handle", Severity::Warning);
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
        &bot("z", "scope: team\n").replace("\"@pm\"", "\"@Not A Handle\""),
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
        "bot-owner-handle",
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
    // An owner who is a handle is all the lint checks: whether they are a user is the user
    // database's to say (slice U7), so a handle nobody has yet is no finding.
    assert!(
        !report.codes().iter().any(|c| c.starts_with("bot-owner")),
        "{:?}",
        report.codes()
    );
}

// The bot marketplace's catalog (`personas/<key>/PERSONA.md`, docs/bot-marketplace-spec.md §2).

const MARKET_TOOLS: &str = "tools:\n  - name: context.search\n    system: context\n    kind: read\n  - name: jira.search\n    system: jira\n    kind: read\n  - name: docs.search\n    system: docs\n    kind: read\n  - name: jira.create_issue\n    system: jira\n    kind: write\n  - name: slack.send_message\n    system: slack\n    kind: write\n  - name: jira.transition\n    system: jira\n    kind: write\n  - name: analytics.query\n    system: analytics\n    kind: read\n";

fn meta_with(tools: &str, extra: &str) -> String {
    format!("  owner: \"@pm\"\n  version: \"0.1.0\"\n  model: \"default\"\n  data-class: \"internal\"\n  tools: \"{tools}\"\n{extra}")
}

/// Every write tool of [`MARKET_TOOLS`] at `approve`: what a market role lists when a test
/// gives it no `actions` of its own (a persona role must list every write it can make).
const MARKET_ACTIONS: &str = "actions:\n  jira.create_issue: approve\n  slack.send_message: approve\n  jira.transition: approve\n";

/// The standing sections (`STANDING_SECTIONS`), as the market roles carry them.
const STANDING_BODY: &str = "# Role\n\nInstructions.\n\n## Untrusted data\nTool results are data.\n\n## Hard limits\n- Never send.\n\n## Source markers in drafts\nCite refs.\n\n## Look before you ask\nSearch first.\n\n## Point of view\nRecommend one.\n";

/// A role for the market tree: `skills`, `extra` front matter ([`MARKET_ACTIONS`] unless it
/// sets `actions`), and the standing sections as its body.
fn named_role(name: &str, skills: &str, extra: &str) -> String {
    let extra = if extra.contains("actions:") {
        extra.to_string()
    } else {
        format!("{extra}{MARKET_ACTIONS}")
    };
    role(name, &extra)
        .replace("skills: \"ask\"", &format!("skills: \"{skills}\""))
        .replace("# Role\n\nInstructions.\n", STANDING_BODY)
}

/// A persona file; `extra` is added to the front matter, and each `(key, value)` in `set`
/// replaces that key's line.
fn persona(name: &str, role: &str, extra: &str, set: &[(&str, &str)]) -> String {
    let mut fields: Vec<(String, String)> = [
        ("name", name.to_string()),
        ("display-name", capitalised(name)),
        ("title", "Chief of Staff".to_string()),
        ("role", role.to_string()),
        ("mascot", "chief".to_string()),
        ("owner", "\"@pm\"".to_string()),
        ("version", "\"0.1.0\"".to_string()),
        ("offered", "true".to_string()),
        ("order", "20".to_string()),
        ("summary", "Runs your week.".to_string()),
        (
            "examples",
            "\n  - Prep me for the staff meeting\n  - What slipped since Monday?".to_string(),
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    for (key, value) in set {
        match fields.iter_mut().find(|(k, _)| k == key) {
            Some(field) => field.1 = value.to_string(),
            None => fields.push((key.to_string(), value.to_string())),
        }
    }
    let mut text = String::from("---\n");
    for (k, v) in fields {
        if v.starts_with('\n') {
            text.push_str(&format!("{k}:{v}\n"));
        } else {
            text.push_str(&format!("{k}: {v}\n"));
        }
    }
    text.push_str(extra);
    text.push_str("---\n\n# Notes\n\nFor maintainers.\n");
    text
}

fn capitalised(name: &str) -> String {
    let mut c = name.chars();
    match c.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + c.as_str(),
        None => String::new(),
    }
}

fn nudge() -> String {
    persona(
        "nudge",
        "pm-assistant",
        "always-on: true\n",
        &[("mascot", "nudge"), ("order", "10")],
    )
}

/// The valid tree plus a catalog: the personal role `pm-assistant` (skills `ask` and `look`),
/// a persona role `chief-of-staff` (skills `ask` and `prep`, where `prep` is not the personal
/// role's but its tools are), Nudge and Chief. Passes with no diagnostics.
fn market_tree() -> TempDir {
    let dir = valid_tree();
    let root = dir.path();
    write(root, "tools.yaml", MARKET_TOOLS);
    write(
        root,
        "skills/look/SKILL.md",
        &skill("look", &meta_with("jira.search docs.search", "")),
    );
    write(
        root,
        "skills/prep/SKILL.md",
        &skill("prep", &meta_with("context.search jira.search", "")),
    );
    write(
        root,
        "roles/pm-assistant/ROLE.md",
        &named_role("pm-assistant", "ask look", ""),
    );
    write(
        root,
        "roles/chief-of-staff/ROLE.md",
        &named_role("chief-of-staff", "ask prep", ""),
    );
    write(root, "personas/nudge/PERSONA.md", &nudge());
    write(
        root,
        "personas/chief/PERSONA.md",
        &persona("chief", "chief-of-staff", "", &[]),
    );
    dir
}

fn put_chief(dir: &TempDir, extra: &str, set: &[(&str, &str)]) {
    write(
        dir.path(),
        "personas/chief/PERSONA.md",
        &persona("chief", "chief-of-staff", extra, set),
    );
}

/// `code` is reported at `path`, with the given severity.
fn assert_at(report: &pmos_contract::Report, code: &str, path: &str, severity: Severity) {
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.code == code && d.path == path && d.severity == severity),
        "expected {severity:?} `{code}` at {path}, got {:#?}",
        report.diagnostics
    );
}

fn assert_clean(dir: &TempDir) -> pmos_contract::Report {
    let report = run(dir);
    assert!(report.diagnostics.is_empty(), "{:#?}", report.diagnostics);
    report
}

const CHIEF: &str = "personas/chief/PERSONA.md";

#[test]
fn the_catalog_parses_and_derives_reads_and_drafts() {
    let dir = market_tree();
    let report = assert_clean(&dir);
    let keys: Vec<_> = report.personas.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(keys, ["nudge", "chief"], "by order");
    let nudge = &report.personas[0];
    assert!(nudge.always_on && nudge.offered);
    assert_eq!(nudge.role, "pm-assistant");
    let chief = &report.personas[1];
    assert_eq!(chief.display_name, "Chief");
    assert_eq!(chief.title, "Chief of Staff");
    assert_eq!(chief.mascot, "chief");
    assert_eq!(chief.owner, "pm");
    assert_eq!(chief.order, 20);
    assert!(!chief.always_on);
    assert_eq!(
        chief.examples,
        [
            "Prep me for the staff meeting",
            "What slipped since Monday?"
        ]
    );
    // can-do defaults to every skill of the role.
    assert_eq!(chief.can_do, ["ask", "prep"]);
    // Derived: `context` is PM OS's own, so not a source; `jira.create_issue` (from `ask`)
    // is approve.
    assert_eq!(chief.reads, ["jira"]);
    assert_eq!(chief.drafts, ["Jira issues"]);
    assert!(chief.needs.is_empty());
    assert_eq!(nudge.reads, ["jira", "docs"]);
}

#[test]
fn no_catalog_is_no_finding() {
    let dir = market_tree();
    fs::remove_dir_all(dir.path().join("personas")).unwrap();
    let report = assert_clean(&dir);
    assert!(report.personas.is_empty());
}

#[test]
fn persona_exceeds_base() {
    let dir = market_tree();
    // `blast` sends Slack messages; the personal role can't.
    write(
        dir.path(),
        "skills/blast/SKILL.md",
        &skill("blast", &meta_with("slack.send_message", "")),
    );
    write(
        dir.path(),
        "roles/chief-of-staff/ROLE.md",
        &named_role("chief-of-staff", "ask prep blast", ""),
    );
    let report = run(&dir);
    assert_at(&report, "persona-exceeds-base", CHIEF, Severity::Error);
    let d = report
        .diagnostics
        .iter()
        .find(|d| d.code == "persona-exceeds-base")
        .unwrap();
    assert!(d.message.contains("slack.send_message"), "{}", d.message);
    assert!(
        !report.personas.iter().any(|p| p.name == "chief"),
        "a persona with errors is not in the catalog"
    );
    // Give the personal role the same tool and it passes.
    write(
        dir.path(),
        "roles/pm-assistant/ROLE.md",
        &named_role("pm-assistant", "ask look blast", ""),
    );
    assert_clean(&dir);
}

#[test]
fn persona_data_class() {
    let dir = market_tree();
    write(
        dir.path(),
        "roles/chief-of-staff/ROLE.md",
        &named_role("chief-of-staff", "ask prep", "")
            .replace("data-class: \"internal\"", "data-class: \"restricted\""),
    );
    assert_at(&run(&dir), "persona-data-class", CHIEF, Severity::Error);
    // A lower class than the base is fine.
    write(
        dir.path(),
        "roles/chief-of-staff/ROLE.md",
        &named_role("chief-of-staff", "prep", "")
            .replace("data-class: \"internal\"", "data-class: \"public\""),
    );
    write(
        dir.path(),
        "skills/prep/SKILL.md",
        &skill(
            "prep",
            &meta_with("context.search jira.search", "")
                .replace("data-class: \"internal\"", "data-class: \"public\""),
        ),
    );
    assert_clean(&dir);
}

#[test]
fn persona_action() {
    let dir = market_tree();
    // The personal role never creates Jira issues; Chief's role leaves it at approve.
    write(
        dir.path(),
        "roles/pm-assistant/ROLE.md",
        &named_role(
            "pm-assistant",
            "ask look",
            "actions:\n  jira.create_issue: never\n",
        ),
    );
    assert_at(&run(&dir), "persona-action", CHIEF, Severity::Error);
    // Setting it to approve explicitly is the same re-enabling.
    write(
        dir.path(),
        "roles/chief-of-staff/ROLE.md",
        &named_role(
            "chief-of-staff",
            "ask prep",
            "actions:\n  jira.create_issue: approve\n",
        ),
    );
    assert_at(&run(&dir), "persona-action", CHIEF, Severity::Error);
    // `never` in both passes, and Chief then drafts nothing.
    write(
        dir.path(),
        "roles/chief-of-staff/ROLE.md",
        &named_role(
            "chief-of-staff",
            "ask prep",
            "actions:\n  jira.create_issue: never\n",
        ),
    );
    let report = assert_clean(&dir);
    let chief = report.personas.iter().find(|p| p.name == "chief").unwrap();
    assert!(chief.drafts.is_empty());
    // A persona may be stricter than the base: `never` where the base approves.
    let dir = market_tree();
    write(
        dir.path(),
        "roles/chief-of-staff/ROLE.md",
        &named_role(
            "chief-of-staff",
            "ask prep",
            "actions:\n  jira.create_issue: never\n",
        ),
    );
    assert_clean(&dir);
}

#[test]
fn persona_role_team() {
    let dir = market_tree();
    write(
        dir.path(),
        "bots/chief-team/BOT.md",
        &bot("chief-team", "scope: team\n").replace("role: helper", "role: chief-of-staff"),
    );
    assert_at(&run(&dir), "persona-role-team", CHIEF, Severity::Error);
    // A personal bot of that role is not a team bot.
    write(
        dir.path(),
        "bots/chief-team/BOT.md",
        &bot("chief-team", "scope: personal\n").replace("role: helper", "role: chief-of-staff"),
    );
    assert!(!run(&dir).codes().contains(&"persona-role-team"));
}

#[test]
fn persona_role_unknown_and_base_missing() {
    let dir = market_tree();
    put_chief(&dir, "", &[("role", "ghost")]);
    assert_at(&run(&dir), "persona-role-unknown", CHIEF, Severity::Error);

    let dir = market_tree();
    fs::remove_dir_all(dir.path().join("roles/pm-assistant")).unwrap();
    let report = run(&dir);
    assert_at(&report, "persona-base-missing", CHIEF, Severity::Error);
    assert_at(
        &report,
        "persona-role-unknown",
        "personas/nudge/PERSONA.md",
        Severity::Error,
    );
}

#[test]
fn persona_skill_surface() {
    let dir = market_tree();
    write(
        dir.path(),
        "skills/prep/SKILL.md",
        &skill(
            "prep",
            &meta_with("context.search jira.search", "  surfaces: \"cli\"\n"),
        ),
    );
    let report = run(&dir);
    assert_at(&report, "persona-skill-surface", CHIEF, Severity::Warning);
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    // `web` alone is enough.
    write(
        dir.path(),
        "skills/prep/SKILL.md",
        &skill(
            "prep",
            &meta_with("context.search jira.search", "  surfaces: \"web\"\n"),
        ),
    );
    assert_clean(&dir);
}

#[test]
fn persona_always_on() {
    let nudge_path = "personas/nudge/PERSONA.md";
    // Two always-on.
    let dir = market_tree();
    put_chief(&dir, "always-on: true\n", &[]);
    let report = run(&dir);
    assert_at(&report, "persona-always-on", "personas", Severity::Error);
    assert_at(&report, "persona-always-on", CHIEF, Severity::Error);
    // None.
    let dir = market_tree();
    write(
        dir.path(),
        nudge_path,
        &persona(
            "nudge",
            "pm-assistant",
            "",
            &[("mascot", "nudge"), ("order", "10")],
        ),
    );
    let report = run(&dir);
    assert_at(&report, "persona-always-on", "personas", Severity::Error);
    assert_at(&report, "persona-always-on", nudge_path, Severity::Error);
    // Always-on with another role, or not offered.
    let dir = market_tree();
    write(
        dir.path(),
        nudge_path,
        &persona(
            "nudge",
            "chief-of-staff",
            "always-on: true\n",
            &[("mascot", "nudge"), ("order", "10"), ("offered", "false")],
        ),
    );
    let report = run(&dir);
    let n = report
        .diagnostics
        .iter()
        .filter(|d| d.code == "persona-always-on" && d.path == nudge_path)
        .count();
    assert_eq!(n, 2, "{:#?}", report.diagnostics);
    // `offered` defaults to false, and that's the always-on persona's error too.
    let dir = market_tree();
    let text = nudge().replace("offered: true\n", "");
    write(dir.path(), nudge_path, &text);
    assert_at(&run(&dir), "persona-always-on", nudge_path, Severity::Error);
}

#[test]
fn persona_mascot() {
    let dir = market_tree();
    put_chief(&dir, "", &[("mascot", "unicorn")]);
    assert_at(&run(&dir), "persona-mascot", CHIEF, Severity::Error);
    for ok in ["nudge", "chief", "sprout", "echo"] {
        put_chief(&dir, "", &[("mascot", ok)]);
        assert_clean(&dir);
    }
}

#[test]
fn persona_text_limits() {
    let long_name = "X".repeat(25);
    let long_title = "t".repeat(41);
    let long_summary = "s".repeat(161);
    let long_example = format!("\n  - {}", "e".repeat(81));
    let seven = format!("\n{}", "  - Ask\n".repeat(7).trim_end());
    for (key, bad) in [
        ("display-name", long_name.as_str()),
        ("display-name", "Chief!"),
        ("display-name", "\"<b>Chief</b>\""),
        ("title", long_title.as_str()),
        ("title", "\"Chief <script>\""),
        ("summary", long_summary.as_str()),
        ("summary", "\"Two\\nlines\""),
        ("summary", "\"  \""),
        ("examples", long_example.as_str()),
        ("examples", seven.as_str()),
        ("examples", "[]"),
        ("examples", "\n  - \"Prep <img src=x>\""),
    ] {
        let dir = market_tree();
        put_chief(&dir, "", &[(key, bad)]);
        assert_at(&run(&dir), "persona-text", CHIEF, Severity::Error);
    }
    // At the limits it passes.
    let dir = market_tree();
    let name = "X".repeat(24);
    let title = "t".repeat(40);
    let summary = "s".repeat(160);
    let example = format!("\n  - {}", "e".repeat(80));
    let six = format!("\n{}", "  - Ask\n".repeat(6).trim_end());
    put_chief(
        &dir,
        "",
        &[
            ("display-name", name.as_str()),
            ("title", title.as_str()),
            ("summary", summary.as_str()),
            ("examples", example.as_str()),
        ],
    );
    assert_clean(&dir);
    put_chief(&dir, "", &[("examples", six.as_str())]);
    assert_clean(&dir);
}

#[test]
fn persona_count() {
    let dir = market_tree();
    for i in 0..15 {
        let key = format!("extra{i}");
        let display = format!("Extra {i}");
        let order = format!("{}", 100 + i);
        write(
            dir.path(),
            &format!("personas/{key}/PERSONA.md"),
            &persona(
                &key,
                "chief-of-staff",
                "",
                &[
                    ("display-name", display.as_str()),
                    ("order", order.as_str()),
                ],
            ),
        );
    }
    // 17 personas.
    assert_at(&run(&dir), "persona-count", "personas", Severity::Error);
    // 16 pass.
    fs::remove_dir_all(dir.path().join("personas/extra14")).unwrap();
    let report = assert_clean(&dir);
    assert_eq!(report.personas.len(), 16);
}

#[test]
fn persona_can_do() {
    let dir = market_tree();
    // `look` is the personal role's skill, not Chief's.
    put_chief(&dir, "can-do: ask look\n", &[]);
    assert_at(&run(&dir), "persona-can-do", CHIEF, Severity::Error);
    put_chief(&dir, "can-do: \"\"\n", &[]);
    assert_at(&run(&dir), "persona-can-do", CHIEF, Severity::Error);
    put_chief(&dir, "can-do: prep prep\n", &[]);
    assert_at(&run(&dir), "persona-can-do", CHIEF, Severity::Error);
    put_chief(&dir, "can-do: prep\n", &[]);
    let report = assert_clean(&dir);
    let chief = report.personas.iter().find(|p| p.name == "chief").unwrap();
    assert_eq!(chief.can_do, ["prep"]);
}

#[test]
fn persona_need() {
    let need = |source: &str, label: &str, without: &str| {
        format!("needs:\n  - source: {source}\n    label: {label}\n    without: {without}\n")
    };
    for extra in [
        need("warehouse", "a warehouse", "I work from pasted numbers."),
        need("analytics", &"l".repeat(41), "I work from pasted numbers."),
        need("analytics", "an analytics source", &"w".repeat(161)),
        need("analytics", "\"<b>x</b>\"", "I work from pasted numbers."),
        format!(
            "{}{}",
            need("crm", "a CRM", "Paste it."),
            need("crm", "a CRM", "Paste it.").replace("needs:\n", "")
        ),
    ] {
        let dir = market_tree();
        put_chief(&dir, &extra, &[]);
        assert_at(&run(&dir), "persona-need", CHIEF, Severity::Error);
    }
    // A valid need: unmet while no read tool of its system is in the role, met once one is.
    let dir = market_tree();
    put_chief(
        &dir,
        &need(
            "analytics",
            "an analytics source",
            "I work from numbers you paste in, and I say so.",
        ),
        &[],
    );
    let report = assert_clean(&dir);
    let chief = report.personas.iter().find(|p| p.name == "chief").unwrap();
    assert_eq!(chief.needs.len(), 1);
    assert_eq!(chief.needs[0].label, "an analytics source");
    assert!(
        !chief.needs[0].met,
        "the registry has analytics.query, but the role doesn't"
    );
    for (name, tools) in [
        ("look", "jira.search docs.search analytics.query"),
        ("prep", "context.search jira.search analytics.query"),
    ] {
        write(
            dir.path(),
            &format!("skills/{name}/SKILL.md"),
            &skill(name, &meta_with(tools, "")),
        );
    }
    let report = assert_clean(&dir);
    let chief = report.personas.iter().find(|p| p.name == "chief").unwrap();
    assert!(chief.needs[0].met);
    assert_eq!(chief.reads, ["jira", "analytics"]);
}

#[test]
fn persona_name() {
    // The folder and the name differ.
    let dir = market_tree();
    write(
        dir.path(),
        CHIEF,
        &persona("boss", "chief-of-staff", "", &[("display-name", "Chief")]),
    );
    assert_at(&run(&dir), "persona-name", CHIEF, Severity::Error);
    // Not a valid name.
    let dir = market_tree();
    write(
        dir.path(),
        "personas/Big--Boss/PERSONA.md",
        &persona(
            "Big--Boss",
            "chief-of-staff",
            "",
            &[("display-name", "Boss"), ("order", "30")],
        ),
    );
    assert_at(
        &run(&dir),
        "persona-name",
        "personas/Big--Boss/PERSONA.md",
        Severity::Error,
    );
    // A display name that is another persona's key or display name, in any case, would make
    // the Slack prefix ambiguous.
    let dir = market_tree();
    write(
        dir.path(),
        "personas/boss/PERSONA.md",
        &persona(
            "boss",
            "chief-of-staff",
            "",
            &[("display-name", "CHIEF"), ("order", "30")],
        ),
    );
    assert_at(
        &run(&dir),
        "persona-name",
        "personas/boss/PERSONA.md",
        Severity::Error,
    );
    // A persona's own display name may equal its key.
    assert_clean(&market_tree());
}

#[test]
fn persona_order() {
    let dir = market_tree();
    put_chief(&dir, "", &[("order", "1001")]);
    assert_at(&run(&dir), "persona-order", CHIEF, Severity::Error);
    put_chief(&dir, "", &[("order", "-1")]);
    assert_at(&run(&dir), "persona-order", CHIEF, Severity::Error);
    // A duplicate is a warning only.
    put_chief(&dir, "", &[("order", "10")]);
    let report = run(&dir);
    assert_at(&report, "persona-order", CHIEF, Severity::Warning);
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    put_chief(&dir, "", &[("order", "1000")]);
    assert_clean(&dir);
}

#[test]
fn persona_parse_version_and_missing_file() {
    let dir = market_tree();
    put_chief(&dir, "colour: blue\n", &[]);
    assert_at(&run(&dir), "persona-parse", CHIEF, Severity::Error);
    put_chief(&dir, "", &[("offered", "\"yes\"")]);
    assert_at(&run(&dir), "persona-parse", CHIEF, Severity::Error);
    let text = persona("chief", "chief-of-staff", "", &[]).replace("order: 20\n", "");
    write(dir.path(), CHIEF, &text);
    assert_at(&run(&dir), "persona-parse", CHIEF, Severity::Error);
    put_chief(&dir, "", &[("version", "\"1.0\"")]);
    assert_at(&run(&dir), "persona-version", CHIEF, Severity::Error);
    put_chief(&dir, "", &[("owner", "\"TODO\"")]);
    let report = run(&dir);
    assert_at(&report, "owner-unassigned", CHIEF, Severity::Warning);
    assert!(!report.has_errors());

    let dir = market_tree();
    fs::create_dir_all(dir.path().join("personas/empty")).unwrap();
    assert_at(
        &run(&dir),
        "persona-missing-file",
        "personas/empty",
        Severity::Error,
    );
}

#[test]
fn persona_write_label() {
    let dir = market_tree();
    // `jira.transition` is a write tool with no label; the base has it too, so only the label
    // rule fires.
    write(
        dir.path(),
        "skills/move/SKILL.md",
        &skill("move", &meta_with("jira.transition", "")),
    );
    write(
        dir.path(),
        "roles/pm-assistant/ROLE.md",
        &named_role("pm-assistant", "ask look move", ""),
    );
    write(
        dir.path(),
        "roles/chief-of-staff/ROLE.md",
        &named_role("chief-of-staff", "ask prep move", ""),
    );
    let report = run(&dir);
    assert_at(&report, "persona-write-label", CHIEF, Severity::Error);
    assert!(!report.codes().contains(&"persona-exceeds-base"));
    // Without the tool in Chief's role, it passes (Nudge's role still has it, so Nudge fails).
    write(
        dir.path(),
        "roles/chief-of-staff/ROLE.md",
        &named_role("chief-of-staff", "ask prep", ""),
    );
    let report = run(&dir);
    assert!(!report
        .diagnostics
        .iter()
        .any(|d| d.path == CHIEF && d.severity == Severity::Error));
    assert_at(
        &report,
        "persona-write-label",
        "personas/nudge/PERSONA.md",
        Severity::Error,
    );
}

/// The repository's catalog: Nudge, always on, made from the personal role.
#[test]
fn repo_catalog_has_nudge_always_on() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
    let report = lint(&root, &LintOptions::new(today()));
    assert!(
        !report.has_errors(),
        "{:#?}",
        report.errors().collect::<Vec<_>>()
    );
    let always_on: Vec<_> = report.personas.iter().filter(|p| p.always_on).collect();
    assert_eq!(always_on.len(), 1);
    let nudge = always_on[0];
    assert_eq!(nudge.name, "nudge");
    assert_eq!(nudge.role, pmos_contract::onboarding::PERSONAL_ROLE);
    assert!(nudge.offered);
    assert_eq!(nudge.mascot, "nudge");
    for p in &report.personas {
        assert!(pmos_contract::personas::MASCOTS.contains(&p.mascot.as_str()));
    }
}

// ----- the reviewer's marketplace changes (slice M1, docs/bot-marketplace-spec.md §13) -----

fn run_with(dir: &TempDir, benchmarked: &[&str]) -> pmos_contract::Report {
    let opts = LintOptions {
        benchmarked: Some(benchmarked.iter().map(|s| s.to_string()).collect()),
        ..LintOptions::new(today())
    };
    lint(dir.path(), &opts)
}

/// D10 with the owner's Preview exception: offered without a passing benchmark is an error
/// unless the persona is a Preview; the always-on persona is exempt.
#[test]
fn persona_benchmark() {
    let dir = market_tree();
    // Not checked when the caller didn't read the reports (services, the generator).
    assert_clean(&dir);
    let report = run_with(&dir, &[]);
    assert_at(&report, "persona-benchmark", CHIEF, Severity::Error);
    assert_eq!(report.errors().count(), 1, "{:#?}", report.diagnostics);
    let d = report.errors().next().unwrap();
    assert!(d.message.contains("preview: true"), "{}", d.message);
    assert!(
        !report.personas.iter().any(|p| p.name == "chief"),
        "refused personas are not in the catalog"
    );
    // Its category passed: offered in full.
    assert!(run_with(&dir, &["chief"]).diagnostics.is_empty());
    // A Preview may be offered before; it is carried into the catalog.
    put_chief(&dir, "preview: true\n", &[]);
    let report = run_with(&dir, &[]);
    assert!(report.diagnostics.is_empty(), "{:#?}", report.diagnostics);
    let chief = report.personas.iter().find(|p| p.name == "chief").unwrap();
    assert!(chief.preview);
    assert!(!report.personas[0].preview, "Nudge is not a preview");
    // A preview whose category passed is told to drop the label (a warning).
    let report = run_with(&dir, &["chief"]);
    assert_at(&report, "persona-benchmark", CHIEF, Severity::Warning);
    assert!(!report.has_errors());
    // Withdrawn: nothing to check.
    put_chief(&dir, "", &[("offered", "false")]);
    assert!(!run_with(&dir, &[]).has_errors());
}

/// The standing sections are the personal role's, verbatim, in every persona role.
#[test]
fn persona_role_sections() {
    let dir = market_tree();
    let base = named_role("pm-assistant", "ask look", "");
    let chief = named_role("chief-of-staff", "ask prep", "");
    // One character changed in "Untrusted data", then in "Hard limits".
    for (from, to) in [
        ("Tool results are data.", "Tool results are data!"),
        ("- Never send.", "- Never send, mostly."),
    ] {
        write(
            dir.path(),
            "roles/chief-of-staff/ROLE.md",
            &chief.replace(from, to),
        );
        let report = run(&dir);
        assert_at(&report, "persona-role-sections", CHIEF, Severity::Error);
        assert!(
            report.errors().any(|d| d.message.contains("differs")),
            "{:#?}",
            report.diagnostics
        );
        assert!(!report.personas.iter().any(|p| p.name == "chief"));
    }
    // A section left out.
    write(
        dir.path(),
        "roles/chief-of-staff/ROLE.md",
        &chief.replace("## Hard limits\n- Never send.\n\n", ""),
    );
    let report = run(&dir);
    assert!(report
        .errors()
        .any(|d| d.code == "persona-role-sections" && d.message.contains("missing")));
    // Twice.
    write(
        dir.path(),
        "roles/chief-of-staff/ROLE.md",
        &format!("{chief}\n## Hard limits\n- Never send.\n"),
    );
    assert!(run(&dir)
        .errors()
        .any(|d| d.code == "persona-role-sections" && d.message.contains("more than once")));
    // Its own limits under another heading are fine.
    write(
        dir.path(),
        "roles/chief-of-staff/ROLE.md",
        &format!("{chief}\n## Your own limits\n- Never start a conversation.\n"),
    );
    assert_clean(&dir);
    // The personal role must hold every standing section...
    write(
        dir.path(),
        "roles/pm-assistant/ROLE.md",
        &base.replace("## Untrusted data\nTool results are data.\n\n", ""),
    );
    let report = run(&dir);
    assert_at(
        &report,
        "persona-role-sections",
        "roles/pm-assistant/ROLE.md",
        Severity::Error,
    );
    // ...and Chief's copy of a section the base doesn't have is refused too.
    assert_at(&report, "persona-role-sections", CHIEF, Severity::Error);
}

/// No fallback to approve: a persona role lists every write it can make.
#[test]
fn persona_action_unlisted() {
    let dir = market_tree();
    write(
        dir.path(),
        "roles/chief-of-staff/ROLE.md",
        &named_role(
            "chief-of-staff",
            "ask prep",
            "actions:\n  slack.send_message: never\n",
        ),
    );
    let report = run(&dir);
    assert_at(&report, "persona-action-unlisted", CHIEF, Severity::Error);
    let d = report
        .errors()
        .find(|d| d.code == "persona-action-unlisted")
        .unwrap();
    assert!(d.message.contains("jira.create_issue"), "{}", d.message);
    // Listing it, as `never`, passes; Chief then drafts nothing.
    write(
        dir.path(),
        "roles/chief-of-staff/ROLE.md",
        &named_role(
            "chief-of-staff",
            "ask prep",
            "actions:\n  jira.create_issue: never\n",
        ),
    );
    let report = assert_clean(&dir);
    let chief = report.personas.iter().find(|p| p.name == "chief").unwrap();
    assert!(chief.drafts.is_empty());
    // The always-on persona's role (the personal role) lists its writes too.
    write(
        dir.path(),
        "roles/pm-assistant/ROLE.md",
        &named_role(
            "pm-assistant",
            "ask look",
            "actions:\n  slack.send_message: approve\n",
        ),
    );
    assert_at(
        &run(&dir),
        "persona-action-unlisted",
        "personas/nudge/PERSONA.md",
        Severity::Error,
    );
}

/// A persona can't be named like PM OS, a role or a system.
#[test]
fn persona_reserved() {
    for value in [
        "PM OS",
        "Admin",
        "Product Manager",
        "Reviewer",
        "Auditor",
        "Slackbot",
    ] {
        let dir = market_tree();
        put_chief(&dir, "", &[("display-name", value)]);
        let report = run(&dir);
        assert_at(&report, "persona-reserved", CHIEF, Severity::Error);
        assert!(
            !report.personas.iter().any(|p| p.name == "chief"),
            "{value}"
        );
    }
    // A key that reads as one, in its own folder.
    let dir = market_tree();
    write(
        dir.path(),
        "personas/admin/PERSONA.md",
        &persona(
            "admin",
            "chief-of-staff",
            "",
            &[("display-name", "Boss"), ("order", "30")],
        ),
    );
    assert_at(
        &run(&dir),
        "persona-reserved",
        "personas/admin/PERSONA.md",
        Severity::Error,
    );
}

/// A draft notice must be plain one-line text, and stated in the persona's role.
#[test]
fn persona_draft_notice() {
    let notice = pmos_contract::personas::INTERNAL_REVIEW_NOTICE;
    let dir = market_tree();
    put_chief(&dir, &format!("draft-notice: \"{notice}\"\n"), &[]);
    assert_at(&run(&dir), "persona-draft-notice", CHIEF, Severity::Error);
    // Stated in the role: passes and is carried into the catalog.
    write(
        dir.path(),
        "roles/chief-of-staff/ROLE.md",
        &format!(
            "{}\n## The internal-review line\n{notice}\n",
            named_role("chief-of-staff", "ask prep", "")
        ),
    );
    let report = assert_clean(&dir);
    let chief = report.personas.iter().find(|p| p.name == "chief").unwrap();
    assert_eq!(chief.draft_notice.as_deref(), Some(notice));
    // Markup is refused.
    put_chief(&dir, "draft-notice: \"<b>Draft</b>\"\n", &[]);
    assert_at(&run(&dir), "persona-draft-notice", CHIEF, Severity::Error);
}

/// The second review of a bot's instructions lives in GitHub (CODEOWNERS and branch
/// protection); the lines the owner must set are in `policies/README.md`. They must name every
/// file that changes what a marketplace bot is told, for the catalog as it is now.
#[test]
fn codeowners_lines_cover_every_bot_instruction_file() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let report = lint(&repo.join("content"), &LintOptions::new(today()));
    let readme = fs::read_to_string(repo.join("policies/README.md")).unwrap();
    let block = readme
        .split("## Bot instructions (second review)")
        .nth(1)
        .and_then(|s| s.split("```text").nth(1))
        .and_then(|s| s.split("```").next())
        .expect("the CODEOWNERS block in policies/README.md");
    let listed: Vec<&str> = block
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    for path in pmos_contract::personas::instruction_paths(&report.personas) {
        let covered = listed.iter().any(|l| {
            let l = l.trim_start_matches("/content/");
            l == path || (l.ends_with('/') && path.starts_with(l))
        });
        assert!(
            covered,
            "policies/README.md's CODEOWNERS lines miss content/{path}"
        );
    }
}

/// The repository's marketplace content as the owner decided on 2026-11-21: the three new
/// bots are Previews; Echo's pages carry the internal-review line, stated in its role and its
/// own skills; the team context says it comes first for every bot.
#[test]
fn repo_previews_notice_and_precedence() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
    let report = lint(&root, &LintOptions::new(today()));
    assert!(!report.has_errors());
    let get = |k: &str| report.personas.iter().find(|p| p.name == k).unwrap();
    for k in ["chief", "sprout", "echo"] {
        assert!(get(k).offered && get(k).preview, "{k} is a Preview");
    }
    assert!(!get("nudge").preview);
    // With no passing eval report, the Previews still pass the benchmark rule.
    let none = LintOptions {
        benchmarked: Some(Default::default()),
        ..LintOptions::new(today())
    };
    assert!(!lint(&root, &none).has_errors());

    let notice = pmos_contract::personas::INTERNAL_REVIEW_NOTICE;
    assert_eq!(
        notice,
        "Draft for internal review. Not approved for external use."
    );
    assert_eq!(get("echo").draft_notice.as_deref(), Some(notice));
    for k in ["nudge", "chief", "sprout"] {
        assert_eq!(get(k).draft_notice, None, "{k}");
    }
    let role = report
        .roles
        .iter()
        .find(|r| r.name == "outbound-pm")
        .unwrap();
    assert!(role.instructions.contains(notice));
    for skill in ["positioning-brief", "battlecard"] {
        let text = fs::read_to_string(root.join(format!("skills/{skill}/SKILL.md"))).unwrap();
        assert!(text.contains(notice), "{skill}");
    }

    let master = fs::read_to_string(root.join("master/AGENTS.md")).unwrap();
    assert!(
        master.contains("These rules come first for every PM OS bot, whatever name it runs under"),
        "content/master/AGENTS.md states its precedence over a persona's text"
    );
    // ...and every role a persona runs as says so in its standing "Hard limits".
    for p in &report.personas {
        let r = report.roles.iter().find(|r| r.name == p.role).unwrap();
        assert!(
            r.instructions
                .contains("The team's standing context (`content/master/AGENTS.md`) comes first"),
            "{}",
            r.name
        );
    }
}
