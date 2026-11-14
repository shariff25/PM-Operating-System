//! The content sync loop (`docs/onboarding-spec.md` §3) against a real git setup: a bare
//! remote, a clone where changes are pushed, and the clone the tool server reads.

mod common;

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use pmos_contract::onboarding::render_personal_bot;
use pmos_toolserver::catalog::Catalog;
use pmos_toolserver::connectors::fake::FakeConnector;
use pmos_toolserver::connectors::Connector;
use pmos_toolserver::content_sync::ContentSync;
use pmos_toolserver::{Stores, ToolService};
use tokio_util::sync::CancellationToken;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn copy_dir(from: &Path, to: &Path) {
    for entry in walkdir::WalkDir::new(from) {
        let entry = entry.unwrap();
        let dest = to.join(entry.path().strip_prefix(from).unwrap());
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&dest).unwrap();
        } else {
            std::fs::copy(entry.path(), &dest).unwrap();
        }
    }
}

/// Write `files` in the work clone, commit and push.
fn push(work: &Path, files: &[(&str, Option<&str>)], message: &str) {
    for (rel, text) in files {
        let path = work.join(rel);
        match text {
            Some(text) => {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, text).unwrap();
            }
            None => std::fs::remove_file(&path).unwrap(),
        }
    }
    git(work, &["add", "-A"]);
    git(work, &["commit", "-q", "-m", message]);
    git(work, &["push", "-q", "origin", "HEAD:main"]);
}

#[tokio::test]
async fn the_sync_pulls_and_reindexes_and_keeps_old_content_on_a_lint_failure() {
    let dir = tempfile::tempdir().unwrap();
    let remote = dir.path().join("remote.git");
    let work = dir.path().join("work");
    let checkout = dir.path().join("checkout");
    std::fs::create_dir_all(&remote).unwrap();
    git(&remote, &["init", "-q", "--bare", "--initial-branch=main"]);
    git(
        dir.path(),
        &["clone", "-q", remote.to_str().unwrap(), "work"],
    );
    git(&work, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    let (_fixture, fixture_root) = common::content_tree();
    copy_dir(&fixture_root, &work.join("content"));
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "content"]);
    git(&work, &["push", "-q", "origin", "HEAD:main"]);
    git(
        dir.path(),
        &["clone", "-q", remote.to_str().unwrap(), "checkout"],
    );

    // The tool server reads `checkout/content`, in a subfolder of the repo.
    let root = checkout.join("content");
    let catalog = Catalog::load(&root, Utc::now().date_naive()).unwrap();
    let (stores, _audit) = Stores::in_memory(&root);
    let connectors: Vec<Arc<dyn Connector>> = vec![
        Arc::new(FakeConnector::jira()),
        Arc::new(FakeConnector::confluence()),
        Arc::new(FakeConnector::slack()),
        Arc::new(FakeConnector::docs()),
    ];
    let svc = Arc::new(ToolService::new(catalog, stores, connectors).unwrap());
    let sync = ContentSync::new(&root, svc.clone());

    // Nothing new.
    let report = sync.sync_once(Utc::now()).await.unwrap();
    assert!(!report.moved);
    assert!(report.reindexed.is_none());

    // A new member's bot lands: pulled and loaded.
    push(
        &work,
        &[(
            "content/bots/pm-zed/BOT.md",
            Some(&render_personal_bot("zed")),
        )],
        "Add zed",
    );
    assert!(svc.roster().bot("pm-zed").is_none());
    let report = sync.sync_once(Utc::now()).await.unwrap();
    assert!(report.moved);
    assert!(report.reindexed.is_some());
    assert!(svc.roster().bot("pm-zed").is_some());
    assert!(checkout.join("content/bots/pm-zed/BOT.md").is_file());
    let again = sync.sync_once(Utc::now()).await.unwrap();
    assert!(!again.moved && again.reindexed.is_none(), "loaded once");

    // Content that fails the lint is pulled but not loaded, and is tried again each time.
    push(
        &work,
        &[(
            "content/bots/broken/BOT.md",
            Some("---\nname: Broken\n---\n"),
        )],
        "Break it",
    );
    let err = sync.sync_once(Utc::now()).await.unwrap_err();
    assert!(err.contains("not loaded"), "{err}");
    assert!(
        sync.sync_once(Utc::now()).await.is_err(),
        "still not loaded"
    );
    assert!(
        svc.roster().bot("pm-zed").is_some(),
        "the old content stays"
    );
    push(&work, &[("content/bots/broken/BOT.md", None)], "Fix it");
    assert!(sync
        .sync_once(Utc::now())
        .await
        .unwrap()
        .reindexed
        .is_some());

    // The loop syncs when triggered, as after a member commit.
    let cancel = CancellationToken::new();
    sync.clone()
        .spawn(Duration::from_secs(3600), cancel.clone());
    push(
        &work,
        &[(
            "content/bots/pm-yan/BOT.md",
            Some(&render_personal_bot("yan")),
        )],
        "Add yan",
    );
    sync.trigger();
    let mut loaded = false;
    for _ in 0..100 {
        if svc.roster().bot("pm-yan").is_some() {
            loaded = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    cancel.cancel();
    assert!(loaded, "the triggered sync loaded the new bot");
}

#[tokio::test]
async fn a_checkout_outside_git_fails_without_panicking() {
    let (_dir, root) = common::content_tree();
    let catalog = Catalog::load(&root, Utc::now().date_naive()).unwrap();
    let (stores, _audit) = Stores::in_memory(&root);
    let svc = Arc::new(
        ToolService::new(
            catalog,
            stores,
            vec![
                Arc::new(FakeConnector::jira()) as Arc<dyn Connector>,
                Arc::new(FakeConnector::confluence()),
                Arc::new(FakeConnector::slack()),
                Arc::new(FakeConnector::docs()),
            ],
        )
        .unwrap(),
    );
    let err = ContentSync::new(&root, svc)
        .sync_once(Utc::now())
        .await
        .unwrap_err();
    assert!(err.contains("git"), "{err}");
}
