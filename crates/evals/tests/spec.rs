//! Category and task validation, and the checked-in categories.

mod common;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use common::write_category;
use pmos_evals::config::Config;
use pmos_evals::judge::load_judges;
use pmos_evals::spec::{category_names, load_category};
use pmos_evals::taxonomy::Taxonomy;

fn evals_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals")
}

#[test]
fn a_valid_category_loads() {
    let dir = tempfile::tempdir().unwrap();
    write_category(dir.path(), "cat", "docs", 0.8, &[
        ("b-task", "id: b-task\nask: \"Two?\"\nexpect:\n  approvals: 0\n".into()),
        ("a-task", "id: a-task\nask: \"One?\"\nexpect:\n  must_cite: [https://support.fortanix.com/docs/x]\n  must_include: [one]\n".into()),
    ]);
    let c = load_category(dir.path(), "cat").unwrap();
    assert_eq!(
        (c.name.as_str(), c.bot.as_str(), c.pass_bar),
        ("cat", "docs", Some(0.8))
    );
    assert_eq!(c.repeats, 1);
    let ids: Vec<&str> = c.tasks.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["a-task", "b-task"]);
    assert_eq!(c.tasks[1].expect.approvals, Some(0));
    assert_eq!(category_names(dir.path()).unwrap(), ["cat"]);
}

#[test]
fn every_problem_is_reported_with_its_file() {
    let dir = tempfile::tempdir().unwrap();
    write_category(dir.path(), "cat", "Docs Bot", 1.5, &[
        ("wrong-name", "id: other\nask: \"x\"\nexpect:\n  must_include: [a]\n".into()),
        ("relative", "id: relative\nask: \"x\"\nexpect:\n  must_cite: [/docs/page]\n".into()),
        ("empty", "id: empty\nask: \" \"\nexpect: {}\n".into()),
        ("both", "id: both\nask: \"x\"\nexpect:\n  must_include: [Docker-Hub]\n  must_not_include: [docker hub]\n".into()),
        ("unknown", "id: unknown\nask: \"x\"\nexpect:\n  must_mention: [a]\n".into()),
        ("blank", "id: blank\nask: \"x\"\nexpect:\n  must_include: [\"**\"]\n".into()),
    ]);
    // The same id twice, through a .yml file.
    std::fs::write(
        dir.path().join("categories/cat/tasks/both.yml"),
        "id: both\nask: \"x\"\nexpect:\n  approvals: 1\n",
    )
    .unwrap();
    let errors = load_category(dir.path(), "cat").unwrap_err().0;
    let has = |file: &str, text: &str| {
        assert!(
            errors.iter().any(|e| e.contains(file) && e.contains(text)),
            "no error on {file} with `{text}` in {errors:#?}"
        )
    };
    has("category.yaml", "`bot` `Docs Bot` is not a bot name");
    has("category.yaml", "`pass_bar` is 1.5");
    has("wrong-name.yaml", "id `other` must match the file name");
    has(
        "relative.yaml",
        "must_cite `/docs/page` is not an absolute http(s) URL",
    );
    has("empty.yaml", "`ask` is empty");
    has("empty.yaml", "`expect` has no checks");
    has(
        "both.yaml",
        "`docker hub` is in both must_include and must_not_include",
    );
    has("unknown.yaml", "unknown field `must_mention`");
    has("blank.yaml", "must_include has an empty entry");
    has("both.yml", "id `both` is used twice");
    assert_eq!(errors.len(), 10, "{errors:#?}");
    let shown = load_category(dir.path(), "cat").unwrap_err().to_string();
    assert!(
        shown.starts_with("10 problems in the eval files:\n  - "),
        "{shown}"
    );
}

#[test]
fn category_file_problems() {
    let dir = tempfile::tempdir().unwrap();
    let err = load_category(dir.path(), "missing")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("can't read") && err.contains("is `missing` a category"),
        "{err}"
    );
    let err = load_category(dir.path(), "Bad_Name")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("lowercase letters, digits and hyphens"),
        "{err}"
    );

    write_category(dir.path(), "cat", "docs", 0.5, &[]);
    std::fs::write(
        dir.path().join("categories/cat/category.yaml"),
        "name: dog\nbot: docs\npass_bar: 0.5\ndescription: \"\"\nowner: me\n",
    )
    .unwrap();
    let err = load_category(dir.path(), "cat").unwrap_err().to_string();
    assert!(err.contains("unknown field `owner`"), "{err}");
    std::fs::write(
        dir.path().join("categories/cat/category.yaml"),
        "name: dog\nbot: docs\npass_bar: 0.5\ndescription: \"\"\n",
    )
    .unwrap();
    let errors = load_category(dir.path(), "cat").unwrap_err().0;
    assert!(
        errors
            .iter()
            .any(|e| e.contains("`name` is `dog` but the directory is `cat`")),
        "{errors:?}"
    );
    assert!(
        errors.iter().any(|e| e.contains("`description` is empty")),
        "{errors:?}"
    );
    assert!(errors.iter().any(|e| e.contains("no tasks")), "{errors:?}");
}

#[test]
fn config_is_validated() {
    let dir = tempfile::tempdir().unwrap();
    assert!(Config::load(dir.path()).unwrap_err().contains("can't read"));
    std::fs::write(
        dir.path().join("config.yaml"),
        common::CONFIG
            .replace("seconds: 120", "seconds: 0")
            .replace("source: test plan", "source: \"\""),
    )
    .unwrap();
    let err = Config::load(dir.path()).unwrap_err();
    assert!(
        err.contains("latency_targets.queue_p90.seconds must be more than 0"),
        "{err}"
    );
    assert!(
        err.contains("latency_targets.queue_p90.source is empty"),
        "{err}"
    );
}

#[test]
fn the_checked_in_evals_are_valid() {
    let root = evals_root();
    let config = Config::load(&root).unwrap();
    assert!(config
        .latency_targets
        .queue_p90
        .source
        .contains("implementation-plan"));
    for name in category_names(&root).unwrap() {
        load_category(&root, &name).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

/// docs-qa: 20 answerable tasks for the Docs bot, each citing one support.fortanix.com page as
/// the bot cites it (the page URL without `.md`), spread across the products; and at least
/// three the docs don't answer, which the bot must decline. Each task runs three times.
#[test]
fn docs_qa_has_twenty_questions_across_the_products() {
    let c = load_category(&evals_root(), "docs-qa").unwrap();
    assert_eq!(c.bot, "docs");
    assert_eq!(c.repeats, 3);
    let (declines, answerable): (Vec<_>, Vec<_>) =
        c.tasks.iter().partition(|t| t.expect.must_decline);
    assert_eq!(answerable.len(), 20);
    assert!(declines.len() >= 3, "{}", declines.len());
    for t in &declines {
        assert!(t.id.starts_with("decline-"), "{}", t.id);
        assert!(t.expect.must_cite.is_empty(), "{}", t.id);
        assert_eq!(t.expect.approvals, Some(0), "{}", t.id);
        assert!(t
            .tags
            .contains(&"model/answered-beyond-sources".to_string()));
    }
    let mut asks = HashSet::new();
    let mut products: HashMap<&str, usize> = HashMap::new();
    for t in answerable {
        assert!(
            asks.insert(t.ask.clone()),
            "{}: the same question twice",
            t.id
        );
        assert_eq!(t.expect.must_cite.len(), 1, "{}", t.id);
        let url = &t.expect.must_cite[0];
        assert!(
            url.starts_with("https://support.fortanix.com/docs/")
                || url.starts_with("https://support.fortanix.com/apidocs/"),
            "{}: {url}",
            t.id
        );
        assert!(
            !url.ends_with(".md") && !url.ends_with('/'),
            "{}: {url}",
            t.id
        );
        assert!(!t.expect.must_include.is_empty(), "{}", t.id);
        assert!(
            t.expect.must_not_include.contains(&"<think>".to_string()),
            "{}",
            t.id
        );
        assert_eq!(t.expect.approvals, Some(0), "{}", t.id);
        let product = t.id.split('-').next().unwrap();
        *products.entry(product).or_default() += 1;
    }
    for p in ["dsm", "ccm", "confidential", "key", "armor", "api"] {
        assert!(
            products.get(p).copied().unwrap_or(0) >= 2,
            "{p}: {products:?}"
        );
    }
}

#[test]
fn new_fields_are_validated() {
    let dir = tempfile::tempdir().unwrap();
    write_category(dir.path(), "cat", "personal", 0.5, &[
        ("cites", "id: cites\nask: \"x\"\nexpect:\n  must_decline: true\n  must_cite: [https://a.example/x]\n".into()),
        ("bad-drafts", "id: bad-drafts\nask: \"x\"\nexpect:\n  drafts:\n    - tool: Jira\n      require_fields: [\"\"]\n      allowed: {project: []}\n      must_include: [\"**\"]\n    - tool: slack.send_message\n      count: 0\n      must_include: [hi]\n    - tool: slack.send_message\n".into()),
        ("blank-cluster", "id: blank-cluster\nask: \"x\"\ncluster: \" \"\nexpect:\n  must_decline: true\n".into()),
    ]);
    std::fs::write(
        dir.path().join("categories/cat/category.yaml"),
        "name: cat\nbot: personal\nrepeats: 0\ndescription: d\n",
    )
    .unwrap();
    let errors = load_category(dir.path(), "cat").unwrap_err().0;
    let has = |file: &str, text: &str| {
        assert!(
            errors.iter().any(|e| e.contains(file) && e.contains(text)),
            "no error on {file} with `{text}` in {errors:#?}"
        )
    };
    has("category.yaml", "`repeats` is 0");
    has("cites.yaml", "a `must_decline` task has no `must_cite`");
    has(
        "bad-drafts.yaml",
        "drafts tool `Jira` is not a registry name",
    );
    has("bad-drafts.yaml", "require_fields has an empty entry");
    has("bad-drafts.yaml", "allowed `project` lists no values");
    has(
        "bad-drafts.yaml",
        "drafts `Jira`: must_include has an empty entry",
    );
    has("bad-drafts.yaml", "`count: 0` means no drafts");
    has("bad-drafts.yaml", "drafts has `slack.send_message` twice");
    has("blank-cluster.yaml", "`cluster` is empty");
    assert_eq!(errors.len(), 9, "{errors:#?}");
}

#[test]
fn bar_and_repeats_are_optional() {
    let dir = tempfile::tempdir().unwrap();
    write_category(dir.path(), "cat", "personal", 0.5, &[(
        "t",
        "id: t\nask: \"x\"\nexpect:\n  drafts:\n    - tool: jira.create_issue\n      allowed: {project: [PM]}\n".into(),
    )]);
    std::fs::write(
        dir.path().join("categories/cat/category.yaml"),
        "name: cat\nbot: personal\ndescription: d\n",
    )
    .unwrap();
    let c = load_category(dir.path(), "cat").unwrap();
    assert_eq!((c.pass_bar, c.repeats), (None, 1));
    assert_eq!(c.tasks[0].expect.drafts[0].allowed["project"], ["PM"]);
}

/// The seed categories: at least three tasks each for the personal bot, no bar, every task
/// with drafts checks on write tools in content/tools.yaml, and the seed marker. (slack-reply
/// also has the reply assistant's triage tasks.)
#[test]
fn the_seed_categories_are_ready_to_report() {
    let root = evals_root();
    let content = root.join("../content");
    let registry = pmos_contract::model::ToolRegistry::from_yaml(
        &std::fs::read_to_string(content.join("tools.yaml")).unwrap(),
    )
    .unwrap();
    for name in ["jira-draft", "confluence-draft", "slack-reply"] {
        let c = load_category(&root, name).unwrap();
        assert_eq!(c.bot, "personal", "{name}");
        assert_eq!(c.pass_bar, None, "{name}");
        assert!(c.tasks.len() >= 3, "{name}: {} tasks", c.tasks.len());
        let header =
            std::fs::read_to_string(root.join("categories").join(name).join("category.yaml"))
                .unwrap();
        assert!(
            header.starts_with("# seed: replace with case cards"),
            "{name}"
        );
        for t in &c.tasks {
            assert!(!t.expect.drafts.is_empty(), "{name}/{}", t.id);
        }
        assert_eq!(
            pmos_evals::spec::check_draft_tools(&root, &c, &registry),
            Vec::<String>::new()
        );
    }
}

#[test]
fn drafts_tools_must_be_write_tools() {
    let dir = tempfile::tempdir().unwrap();
    write_category(dir.path(), "cat", "personal", 0.5, &[(
        "t",
        "id: t\nask: \"x\"\nexpect:\n  drafts:\n    - tool: jira.search\n    - tool: jira.delete_issue\n    - tool: jira.create_issue\n".into(),
    )]);
    let c = load_category(dir.path(), "cat").unwrap();
    let registry = pmos_contract::model::ToolRegistry::from_yaml(
        "tools:\n  - {name: jira.search, system: jira, kind: read}\n  - {name: jira.create_issue, system: jira, kind: write}\n",
    )
    .unwrap();
    let errors = pmos_evals::spec::check_draft_tools(dir.path(), &c, &registry);
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(errors[0].contains("`jira.search` is not a write tool"));
    assert!(errors[1].contains("`jira.delete_issue`"));
}

#[test]
fn the_checked_in_taxonomy_is_valid_and_has_the_spec_examples() {
    let t = Taxonomy::load(&evals_root()).unwrap();
    for sub in pmos_evals::tag::SUB_TAGS {
        assert!(t.contains(sub), "{sub}");
    }
    for example in [
        "tool/wrong-confluence-space",
        "model/missing-citation",
        "model/answered-beyond-sources",
        "harness/approval-count",
    ] {
        assert!(
            t.describe(example).is_some_and(|d| !d.is_empty()),
            "{example}"
        );
    }
    // Every tag a checked-in task or judge uses is listed.
    let root = evals_root();
    for name in category_names(&root).unwrap() {
        for task in load_category(&root, &name).unwrap().tasks {
            for tag in &task.tags {
                assert!(t.contains(tag), "{name}/{}: {tag}", task.id);
            }
        }
    }
    for j in load_judges(&root).unwrap() {
        assert!(t.contains(&j.failure_mode), "{}", j.failure_mode);
    }
}

#[test]
fn taxonomy_problems_are_reported() {
    let errors = Taxonomy::parse(
        "model:\n  description: d\n  tags:\n    Bad_Name: x\n    empty: \"\"\nweather:\n  description: d\ntool:\n  description: \"\"\n",
        "taxonomy.yaml",
    )
    .unwrap_err();
    let has = |text: &str| {
        assert!(
            errors.iter().any(|e| e.contains(text)),
            "no `{text}` in {errors:#?}"
        )
    };
    has("`weather` is not a bottleneck");
    has("`policy` is missing");
    has("`tool` has no description");
    has("`model/Bad_Name`: a sub-tag is lowercase");
    has("`model/empty` has no description");
    has("`model/missing-citation` is missing; the automatic tagger emits it");
    assert!(Taxonomy::parse("model: [", "taxonomy.yaml").is_err());
}

#[test]
fn the_checked_in_judge_is_advisory_with_empty_labels() {
    let root = evals_root();
    let judges = load_judges(&root).unwrap();
    let j = judges
        .iter()
        .find(|j| j.name == "faithful-to-cited-page")
        .expect("the example judge");
    assert_eq!(j.alias, "pmos/extraction");
    assert_eq!(j.applies_to, ["docs-qa"]);
    assert!(j.needs_sources);
    assert!(pmos_evals::judge::load_labels(&root, j).unwrap().is_empty());
    let cal = pmos_evals::judge::load_calibration(&root, j).unwrap();
    let status = pmos_evals::judge::status(j, cal.as_ref());
    assert!(!status.blocking, "{}", status.reason);
}

#[test]
fn judge_files_are_validated() {
    let dir = tempfile::tempdir().unwrap();
    let judges = dir.path().join("judges");
    std::fs::create_dir_all(&judges).unwrap();
    std::fs::write(
        judges.join("j.yaml"),
        "name: other\nfailure_mode: nope\nquestion: \" \"\nalias: gpt-4\napplies_to: []\nlabels: missing.jsonl\n",
    )
    .unwrap();
    std::fs::write(
        judges.join("k.yaml"),
        "name: k\nfailure_mode: model/x\nquestion: q?\napplies_to: [cat]\nlabels: k.jsonl\n",
    )
    .unwrap();
    std::fs::write(
        judges.join("k.jsonl"),
        "{\"input\": \"a\", \"output\": \"b\"}\n",
    )
    .unwrap();
    let errors = load_judges(dir.path()).unwrap_err();
    let has = |text: &str| {
        assert!(
            errors.iter().any(|e| e.contains(text)),
            "no `{text}` in {errors:#?}"
        )
    };
    has("`name` `other` must be lowercase letters, digits and hyphens, and match the file name");
    has("`failure_mode` `nope` is a taxonomy sub-tag");
    has("`question` is empty");
    has("`alias` `gpt-4` is a gateway alias");
    has("`applies_to` names no category");
    has("missing.jsonl: can't read");
    has("k.jsonl:1: not a label");
    // The default alias, and a missing directory is no judges.
    std::fs::write(judges.join("k.jsonl"), "").unwrap();
    std::fs::remove_file(judges.join("j.yaml")).unwrap();
    let ok = load_judges(dir.path()).unwrap();
    assert_eq!(ok[0].alias, "pmos/extraction");
    assert!(load_judges(&dir.path().join("nowhere")).unwrap().is_empty());
}

#[test]
fn affected_categories_follow_bot_role_and_skills() {
    let root = evals_root();
    let content = root.join("../content");
    let affected = |paths: &[&str]| {
        let paths: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
        pmos_evals::affected::affected(&root, &content, "content", "evals", &paths).unwrap()
    };
    let all = category_names(&root).unwrap();
    assert_eq!(affected(&["evals/categories/docs-qa/category.yaml"]), all);
    assert_eq!(affected(&["deploy/gateway.yaml"]), all);
    assert_eq!(affected(&["content/skills/docs/SKILL.md"]), ["docs-qa"]);
    assert_eq!(
        affected(&["content/roles/docs-helper/ROLE.md"]),
        ["docs-qa"]
    );
    assert_eq!(affected(&["content/bots/docs/BOT.md"]), ["docs-qa"]);
    // Personal bots: pm-<requested_by>, made from pm-assistant and its skills.
    let personal = ["confluence-draft", "jira-draft", "slack-reply"];
    assert_eq!(affected(&["content/roles/pm-assistant/ROLE.md"]), personal);
    assert_eq!(affected(&["./content/skills/prd-draft/SKILL.md"]), personal);
    assert_eq!(affected(&["content/bots/pm-evals/BOT.md"]), personal);
    assert!(affected(&["content/bots/pm-ana/BOT.md"]).is_empty());
    assert!(affected(&[
        "README.md",
        "content/context/x.md",
        "crates/evals/src/run.rs"
    ])
    .is_empty());
    // A skill directory's name must match whole: `ask` doesn't match `asked`.
    assert!(affected(&["content/skills/asked/SKILL.md"]).is_empty());
}
