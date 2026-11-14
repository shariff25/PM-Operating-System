//! Loading bots from the content repo through the lint: the seeded tree loads, and a tree
//! with a lint error stops the runtime from starting.

use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use pmos_bots::catalog::Catalog;
use pmos_contract::bots::{ActionSetting, BotScope};

fn content() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content")
}

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 14).unwrap()
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn the_seeded_content_loads() {
    let catalog = Catalog::load(&content(), today()).expect("the seeded content lints clean");
    let docs = catalog.bot("docs").expect("the team bot `docs` is seeded");
    assert_eq!(docs.scope, BotScope::Team);
    assert_eq!(docs.role, "docs-helper");
    let detail = catalog.detail("docs").unwrap();
    assert!(
        detail.tools.iter().all(|t| t.starts_with("docs.")),
        "the Docs bot only reads the docs: {:?}",
        detail.tools
    );
    assert!(detail.actions.is_empty(), "and writes nothing");
    assert!(catalog.summaries().iter().any(|b| b.name == "docs"));
}

#[test]
fn a_personal_bot_gets_its_roles_write_settings() {
    let dir = tempfile::tempdir().unwrap();
    copy_dir(&content(), dir.path());
    let bot = dir.path().join("bots/pm-ana");
    std::fs::create_dir_all(&bot).unwrap();
    std::fs::write(
        bot.join("BOT.md"),
        "---\nname: pm-ana\nrole: pm-assistant\nscope: personal\nowner: \"@ana\"\n\
         description: Ana's assistant.\nactions:\n  slack.send_message: never\n---\n\nHelp Ana.\n",
    )
    .unwrap();
    let catalog = Catalog::load(dir.path(), today()).expect("loads");
    let detail = catalog.detail("pm-ana").unwrap();
    assert_eq!(detail.bot.owner, "ana");
    assert_eq!(
        detail.actions.get("slack.send_message"),
        Some(&ActionSetting::Never)
    );
    assert_eq!(
        detail.actions.get("jira.create_issue"),
        Some(&ActionSetting::Approve)
    );
    assert!(catalog
        .may_use("ana", catalog.bot("pm-ana").unwrap())
        .is_ok());
    assert!(catalog
        .may_use("ben", catalog.bot("pm-ana").unwrap())
        .is_err());
}

#[test]
fn a_tree_with_lint_errors_refuses_to_load() {
    let dir = tempfile::tempdir().unwrap();
    copy_dir(&content(), dir.path());
    let bot = dir.path().join("bots/broken");
    std::fs::create_dir_all(&bot).unwrap();
    std::fs::write(
        bot.join("BOT.md"),
        "---\nname: broken\nrole: no-such-role\nscope: team\nowner: \"@ana\"\ndescription: x\n---\n\nx\n",
    )
    .unwrap();
    let err = Catalog::load(dir.path(), today()).err().expect("refused");
    assert!(err.contains("lint errors"), "{err}");
    assert!(err.contains("bots/broken/BOT.md"), "{err}");
}
