//! Loading bots from the content repo through the lint: the seeded tree loads, a tree with
//! a lint error stops the runtime from starting, and a content change is picked up without a
//! restart unless it fails the lint (onboarding spec §5).

mod common;

use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use common::Env;
use pmos_bots::catalog::{fingerprint, Catalog, Reload, Reloader};
use pmos_contract::bots::{ActionSetting, BotScope};
use pmos_contract::onboarding::{bot_path, render_personal_bot};
use serde_json::{json, Value};

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

fn names(bots: &Value) -> Vec<String> {
    bots.as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn a_content_change_is_picked_up_and_a_bad_one_is_not() {
    let dir = tempfile::tempdir().unwrap();
    copy_dir(&content(), dir.path());
    let mut reloader = Reloader::new(dir.path());
    let env = Env::memory_with(Catalog::load(dir.path(), today()).expect("loads")).await;
    assert_eq!(reloader.check(&env.rt, today()).await, Reload::Unchanged);
    let (status, bots) = env.get("/v1/bots", "slack", Some("ana")).await;
    assert_eq!(status, 200);
    assert_eq!(names(&bots), ["docs"]);

    // A task claimed before the change keeps the bot it was claimed with.
    let task = env.ask("What ports does Acme Vault need?").await;
    let (status, claim) = env.claim().await;
    assert_eq!(status, 200, "{claim}");
    assert_eq!(claim["bot"]["name"], "docs");

    // The member approved in Slack: the tool server commits their bot, the checkout pulls.
    // The docs bot goes in the same change.
    let bot_md = dir.path().join(bot_path("pm-ana"));
    std::fs::create_dir_all(bot_md.parent().unwrap()).unwrap();
    std::fs::write(&bot_md, render_personal_bot("ana")).unwrap();
    std::fs::remove_dir_all(dir.path().join("bots/docs")).unwrap();
    assert_eq!(
        reloader.check(&env.rt, today()).await,
        Reload::Reloaded { bots: 1 }
    );
    let (_, bots) = env.get("/v1/bots", "slack", Some("ana")).await;
    assert_eq!(names(&bots), ["pm-ana"], "no restart needed");
    let (status, t) = env
        .post(
            "/v1/tasks",
            "slack",
            Some("ana"),
            json!({"bot": "pm-ana", "requested_by": "ana", "text": "Draft a PRD", "source": "slack"}),
        )
        .await;
    assert_eq!(status, 201, "{t}");
    // The docs task finishes although its bot is gone.
    let tid = task["id"].as_str().unwrap();
    let (status, done) = env
        .post(
            &format!("/v1/worker/tasks/{tid}/complete"),
            "worker",
            None,
            json!({"result": "Port 443."}),
        )
        .await;
    assert_eq!(status, 200, "{done}");
    assert_eq!(done["status"], "done");

    // A change that fails the lint keeps the catalog that is loaded, and is reported once.
    let broken = dir.path().join("bots/broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(
        broken.join("BOT.md"),
        "---\nname: broken\nrole: no-such-role\nscope: team\nowner: \"@ana\"\ndescription: x\n---\n\nx\n",
    )
    .unwrap();
    match reloader.check(&env.rt, today()).await {
        Reload::Kept(e) => assert!(e.contains("bots/broken/BOT.md"), "{e}"),
        other => panic!("expected the old catalog kept, got {other:?}"),
    }
    let (_, bots) = env.get("/v1/bots", "slack", Some("ana")).await;
    assert_eq!(names(&bots), ["pm-ana"]);
    assert_eq!(reloader.check(&env.rt, today()).await, Reload::Unchanged);

    // Fixing it loads again.
    std::fs::remove_dir_all(&broken).unwrap();
    assert_eq!(
        reloader.check(&env.rt, today()).await,
        Reload::Reloaded { bots: 1 }
    );
    env.finish().await;
}

#[test]
fn the_fingerprint_follows_the_watched_files_only() {
    let dir = tempfile::tempdir().unwrap();
    copy_dir(&content(), dir.path());
    let before = fingerprint(dir.path());
    assert_eq!(fingerprint(dir.path()), before);
    std::fs::write(dir.path().join("README-extra.md"), "not watched").unwrap();
    assert_eq!(
        fingerprint(dir.path()),
        before,
        "only the catalog's files count"
    );
    // A team.yaml decides nothing (slice U7), so it isn't watched either.
    std::fs::write(dir.path().join("team.yaml"), "members: []\n").unwrap();
    assert_eq!(fingerprint(dir.path()), before);
    std::fs::write(dir.path().join("tools.yaml"), "tools: []\n").unwrap();
    assert_ne!(fingerprint(dir.path()), before);
}
