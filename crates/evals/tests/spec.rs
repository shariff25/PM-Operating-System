//! Category and task validation, and the checked-in categories.

mod common;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use common::write_category;
use pmos_evals::config::Config;
use pmos_evals::spec::{category_names, load_category};

fn evals_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals")
}

#[test]
fn a_valid_category_loads() {
    let dir = tempfile::tempdir().unwrap();
    write_category(dir.path(), "cat", "docs", 0.8, &[
        ("b-task", "id: b-task\nask: \"Two?\"\nexpect:\n  approvals: 0\n".into()),
        ("a-task", "id: a-task\nask: \"One?\"\nexpect:\n  must_cite: [https://support.acme.com/docs/x]\n  must_include: [one]\n".into()),
    ]);
    let c = load_category(dir.path(), "cat").unwrap();
    assert_eq!(
        (c.name.as_str(), c.bot.as_str(), c.pass_bar),
        ("cat", "docs", 0.8)
    );
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

/// docs-qa: 20 tasks for the Docs bot, each citing one support.acme.com page as the bot
/// cites it (the page URL without `.md`), spread across the products.
#[test]
fn docs_qa_has_twenty_questions_across_the_products() {
    let c = load_category(&evals_root(), "docs-qa").unwrap();
    assert_eq!(c.bot, "docs");
    assert_eq!(c.tasks.len(), 20);
    let mut asks = HashSet::new();
    let mut products: HashMap<&str, usize> = HashMap::new();
    for t in &c.tasks {
        assert!(
            asks.insert(t.ask.clone()),
            "{}: the same question twice",
            t.id
        );
        assert_eq!(t.expect.must_cite.len(), 1, "{}", t.id);
        let url = &t.expect.must_cite[0];
        assert!(
            url.starts_with("https://support.acme.com/docs/")
                || url.starts_with("https://support.acme.com/apidocs/"),
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
