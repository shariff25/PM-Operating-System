//! Who may use the bots comes from the user directory (`docs/user-database-spec.md` §2, slice
//! U4a), never from `team.yaml`: roles (an Auditor can't ask), status (a deactivated person
//! can't ask, and their routines pause), and the age of the snapshot at each of its bounds
//! (30 seconds to allow, 5 minutes to queue an ask or decide a due routine). A tool server
//! outage only ever narrows what anyone may do.

#[macro_use]
mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Duration as Span;
use common::signing::*;
use common::*;
use pmos_bots::assertion::Mode;
use pmos_bots::catalog::Catalog;
use pmos_bots::model::{Output, Routine, Schedule, Source, Task, TaskStatus};
use pmos_bots::store::TaskFilter;
use pmos_directory::snapshot::{user, ALLOW_AGE, DUE_ROUTINE_AGE, QUEUE_ASK_AGE};
use reqwest::Method;
use serde_json::{json, Value};

const MS: Duration = Duration::from_millis(1);

fn ask_body(bot: &str, who: &str) -> Value {
    json!({"bot": bot, "requested_by": who, "text": "hello", "source": "slack"})
}

async fn ask_as(env: &Env, bot: &str, who: &str) -> (u16, Value) {
    env.post("/v1/tasks", "slack", Some(who), ask_body(bot, who))
        .await
}

async fn routine_tasks(env: &Env) -> Vec<Task> {
    env.store
        .tasks(&TaskFilter {
            source: Some(Source::Routine),
            limit: 100,
            ..TaskFilter::default()
        })
        .await
        .unwrap()
}

/// A daily routine of `owner`'s on `bot`, due at `due`.
async fn routine(env: &Env, bot: &str, owner: &str, due: chrono::DateTime<chrono::Utc>) -> String {
    let r = Routine {
        id: pmos_bots::model::new_routine_id(),
        bot: bot.into(),
        owner: owner.into(),
        enabled: true,
        schedule: Schedule {
            cron: "0 8 * * *".into(),
            tz: "UTC".into(),
        },
        prompt: "What changed?".into(),
        output: Output::dm(),
        approval: "approve".into(),
        next_run_at: Some(due),
        last_run_at: None,
        created_at: due - Span::days(1),
        updated_at: due - Span::days(1),
    };
    env.store.create_routine(&r).await.unwrap();
    r.id
}

// ----- roles ------------------------------------------------------------------------------

#[tokio::test]
async fn an_auditor_only_member_cannot_ask() {
    let env = Env::memory().await;
    env.set_user(user("aud", &["auditor"])).await;
    for (bot, source) in [("docs", "slack"), ("docs", "cli")] {
        let (status, body) = env
            .post(
                "/v1/tasks",
                "slack",
                Some("aud"),
                json!({"bot": bot, "requested_by": "aud", "text": "hi", "source": source}),
            )
            .await;
        assert_eq!(status, 403, "{body}");
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("no role that may ask"),
            "{body}"
        );
    }
    let (status, _) = env
        .post(
            "/v1/tasks",
            "web",
            Some("aud"),
            json!({"bot": "docs", "requested_by": "aud", "text": "hi", "source": "web"}),
        )
        .await;
    assert_eq!(status, 403, "the web client is held to the same rule");
    // No level to set either.
    let (status, _) = env
        .put(
            "/v1/members/aud/settings",
            "slack",
            Some("aud"),
            json!({"level": "staff"}),
        )
        .await;
    assert_eq!(status, 403);
    // An auditor who is also a PM asks as a PM; a reviewer and an admin-only person ask too.
    env.set_user(user("aud", &["auditor", "pm"])).await;
    env.set_user(user("rev", &["reviewer"])).await;
    env.set_user(user("adm", &["admin"])).await;
    for who in ["aud", "rev", "adm"] {
        let (status, body) = ask_as(&env, "docs", who).await;
        assert_eq!(status, 201, "{who}: {body}");
    }
    env.finish().await;
}

#[tokio::test]
async fn an_auditor_only_member_is_not_a_console_admin_and_admin_needs_active() {
    let env = Env::memory().await;
    env.set_user(user("aud", &["auditor"])).await;
    env.set_user(deactivated("old-admin", &["pm", "admin"]))
        .await;
    assert!(env.rt.is_admin("cy"));
    assert!(!env.rt.is_admin("aud"));
    assert!(!env.rt.is_admin("old-admin"), "a deactivated admin is none");
    assert!(!env.rt.is_admin("eve"));
    env.finish().await;
}

// ----- deactivation -------------------------------------------------------------------------

async fn a_deactivated_users_ask_is_refused_and_routines_pause(env: &Env) {
    let now = chrono::Utc::now();
    let later = now + Span::days(1);
    let team = routine(env, "docs", "ana", later).await;
    let personal = routine(env, "pm-ana", "ana", later).await;
    let bens = routine(env, "docs", "ben", later).await;
    let (status, _) = ask_as(env, "pm-ana", "ana").await;
    assert_eq!(status, 201);

    env.set_user(deactivated("ana", &["pm"])).await;
    for bot in ["docs", "pm-ana"] {
        let (status, body) = ask_as(env, bot, "ana").await;
        assert_eq!(status, 403, "{body}");
        assert!(
            body["error"].as_str().unwrap().contains("deactivated"),
            "{body}"
        );
    }
    let (status, _) = env
        .put(
            "/v1/members/ana/settings",
            "slack",
            Some("ana"),
            json!({"level": "staff"}),
        )
        .await;
    assert_eq!(status, 403);

    // The snapshot shows it: her routines pause now, not when they next fall due.
    assert_eq!(
        env.rt.pause_routines_of_inactive_owners(now).await.unwrap(),
        2
    );
    for id in [&team, &personal] {
        let r = env.rt.routine(id).await.unwrap();
        assert!(!r.enabled, "{id}");
        assert_eq!(r.next_run_at, None);
    }
    assert!(
        env.rt.routine(&bens).await.unwrap().enabled,
        "ben's runs on"
    );
    let tasks = routine_tasks(env).await;
    assert_eq!(tasks.len(), 2);
    assert!(tasks.iter().all(|t| t.status == TaskStatus::Failed
        && t.error
            .as_deref()
            .unwrap()
            .starts_with("Your routine is paused:")
        && t.error.as_deref().unwrap().contains("deactivated")));
    // Nothing is claimable from them, and a second pass changes nothing.
    assert_eq!(
        env.rt.pause_routines_of_inactive_owners(now).await.unwrap(),
        0
    );
    let docs = ["docs".to_string()];
    let claimed = env.rt.claim_now("w1", Some(&docs[..]), now).await;
    assert!(claimed.unwrap().is_none());

    // The 30-second tick does it too, for someone who loses the right to ask.
    let bo = routine(env, "docs", "ben", later + Span::days(1)).await;
    env.set_user(user("ben", &["auditor"])).await;
    env.rt.tick(now).await;
    assert!(!env.rt.routine(&bens).await.unwrap().enabled);
    assert!(!env.rt.routine(&bo).await.unwrap().enabled);
}

// ----- the age of the snapshot ----------------------------------------------------------------

#[tokio::test]
async fn an_ask_queues_on_a_snapshot_up_to_five_minutes_old_and_no_older() {
    let env = Env::memory().await;
    env.people.set_down(true);
    env.clock.advance(ALLOW_AGE + MS);
    let (status, body) = ask_as(&env, "docs", "ana").await;
    assert_eq!(status, 201, "31 s old: still queued: {body}");
    env.clock.advance(QUEUE_ASK_AGE - ALLOW_AGE - MS);
    let (status, _) = ask_as(&env, "docs", "ana").await;
    assert_eq!(status, 201, "exactly 5 minutes old: queued");
    env.clock.advance(MS);
    let (status, body) = ask_as(&env, "docs", "ana").await;
    assert_eq!(status, 503, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("can't check"),
        "{body}"
    );
    // Even a refusal it could have made earlier is now a 503, not a guess.
    let (status, _) = ask_as(&env, "docs", "eve").await;
    assert_eq!(status, 503);
    // Back up: answered again.
    env.people.set_down(false);
    env.refresh().await;
    let (status, _) = ask_as(&env, "docs", "ana").await;
    assert_eq!(status, 201);
    env.finish().await;
}

#[tokio::test]
async fn what_allows_needs_a_snapshot_at_most_30_seconds_old() {
    let env = Env::memory().await;
    env.gw.reply.lock().unwrap().clear();
    env.people.set_down(true);
    env.clock.advance(ALLOW_AGE);
    let level = json!({"level": "staff"});
    let (status, body) = env
        .put(
            "/v1/members/ana/settings",
            "slack",
            Some("ana"),
            level.clone(),
        )
        .await;
    assert_eq!(status, 200, "exactly 30 s: {body}");
    env.clock.advance(MS);
    let (status, body) = env
        .put("/v1/members/ana/settings", "slack", Some("ana"), level)
        .await;
    assert_eq!(status, 503, "{body}");
    // Parsing a routine calls the model as the owner: refused before the model is asked.
    let (status, body) = env
        .post(
            "/v1/routines/parse",
            "slack",
            Some("ana"),
            json!({"bot": "docs", "owner": "ana", "text": "every morning at 8", "tz": "UTC"}),
        )
        .await;
    assert_eq!(status, 503, "{body}");
    assert!(env.gw.seen.lock().unwrap().is_empty());
    env.finish().await;
}

#[tokio::test]
async fn a_304_keeps_the_snapshot_fresh() {
    let env = Env::memory().await;
    // Ten polls ten seconds apart, each answered 304: 100 seconds on, the snapshot is fresh.
    for _ in 0..10 {
        env.clock.advance(Duration::from_secs(10));
        env.refresh().await;
    }
    assert!(env.rt.is_admin("cy"));
    let (status, _) = env
        .put(
            "/v1/members/ana/settings",
            "slack",
            Some("ana"),
            json!({"level": "staff"}),
        )
        .await;
    assert_eq!(status, 200);
    env.finish().await;
}

#[tokio::test]
async fn on_a_stale_snapshot_a_console_admin_sees_their_own_tasks_only() {
    let env = Env::memory_with_assertions(assertions(Mode::Require)).await;
    let anas = env.ask("What's on the roadmap?").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, _) = ask_as(&env, "docs", "cy").await;
    assert_eq!(status, 201);
    let console = Signer::console();
    let read = |path: String| {
        let a = console.sign("cy", "GET", &path, b"");
        let env = &env;
        async move { send(env, Method::GET, &path, "console", None, None, Some(&a)).await }
    };
    let one = format!("/v1/tasks/{anas}");

    env.people.set_down(true);
    env.clock.advance(ALLOW_AGE);
    let (status, _) = read(one.clone()).await;
    assert_eq!(status, 200, "30 s old: cy is still an admin");
    let (_, list) = read("/v1/tasks".into()).await;
    assert_eq!(list.as_array().unwrap().len(), 2);

    env.clock.advance(MS);
    for path in [one.clone(), format!("{one}/events"), format!("{one}/trace")] {
        let (status, body) = read(path.clone()).await;
        assert_eq!(status, 404, "{path}: {body}");
    }
    let (_, list) = read("/v1/tasks".into()).await;
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 1, "own tasks only: {list:?}");
    assert_eq!(list[0]["requested_by"], "cy");
    let (_, routines) = read("/v1/routines".into()).await;
    assert!(routines.as_array().unwrap().is_empty());
    // A cancel of ana's task is refused the same way.
    let path = format!("/v1/tasks/{anas}/cancel");
    let a = console.sign("cy", "POST", &path, b"");
    let (status, _) = send(&env, Method::POST, &path, "console", None, None, Some(&a)).await;
    assert_eq!(status, 404);

    // Fresh again: an admin again.
    env.people.set_down(false);
    env.refresh().await;
    let (status, _) = read(one).await;
    assert_eq!(status, 200);
    env.finish().await;
}

async fn a_due_routine_is_skipped_on_a_snapshot_older_than_five_minutes(env: &Env) {
    let due = pmos_bots::model::now();
    let id = routine(env, "docs", "ana", due).await;
    env.people.set_down(true);

    // Exactly 5 minutes: it runs.
    env.clock.advance(DUE_ROUTINE_AGE);
    assert_eq!(env.rt.run_due_routines(due).await.unwrap(), 1);
    let ran = env.rt.routine(&id).await.unwrap();
    assert!(ran.enabled);
    let next = ran.next_run_at.unwrap();
    assert_eq!(routine_tasks(env).await.len(), 1);

    // Past 5 minutes, at its next slot: skipped, not paused, and its next run scheduled.
    env.clock.advance(MS);
    assert_eq!(env.rt.run_due_routines(next).await.unwrap(), 0);
    let skipped = env.rt.routine(&id).await.unwrap();
    assert!(skipped.enabled, "a skip never pauses");
    assert_eq!(skipped.next_run_at, Some(next + Span::days(1)));
    assert_eq!(skipped.last_run_at, ran.last_run_at, "no run recorded");
    assert_eq!(routine_tasks(env).await.len(), 1, "nothing queued");

    // Deactivated meanwhile, but a snapshot that old pauses nothing either ...
    env.people.set_down(false);
    env.people.upsert(deactivated("ana", &["pm"]));
    env.people.set_down(true);
    assert_eq!(
        env.rt
            .pause_routines_of_inactive_owners(next)
            .await
            .unwrap(),
        0
    );
    assert!(env.rt.routine(&id).await.unwrap().enabled);
    // ... until the directory answers and shows it.
    env.people.set_down(false);
    env.refresh().await;
    assert_eq!(
        env.rt
            .pause_routines_of_inactive_owners(next)
            .await
            .unwrap(),
        1
    );
    assert!(!env.rt.routine(&id).await.unwrap().enabled);
}

// ----- an outage never widens access ------------------------------------------------------------

#[tokio::test]
async fn a_tool_server_outage_never_widens_access() {
    let env = Env::memory().await;
    env.set_user(deactivated("ben", &["pm"])).await;
    env.people.set_down(true);
    // Whatever the directory says while it can't be read is never seen: ben stays refused,
    // eve stays nobody, and nobody becomes an admin.
    env.people.set_users(vec![
        user("ana", &["pm", "admin"]),
        user("ben", &["pm", "admin"]),
        user("eve", &["pm", "admin"]),
    ]);
    for _ in 0..4 {
        env.clock.advance(Duration::from_secs(10));
        assert!(env.rt.people().refresh().await.is_err());
    }
    for who in ["ben", "eve"] {
        let (status, _) = ask_as(&env, "docs", who).await;
        assert_eq!(status, 403, "{who}");
    }
    for who in ["ana", "ben", "eve"] {
        assert!(!env.rt.is_admin(who), "{who}");
    }
    assert!(!env.rt.is_admin("cy"), "40 s of outage: no admin at all");
    // Past 5 minutes nobody may queue, those once allowed included.
    env.clock.advance(QUEUE_ASK_AGE);
    for who in ["ana", "ben", "eve"] {
        let (status, _) = ask_as(&env, "docs", who).await;
        assert_eq!(status, 503, "{who}");
    }
    env.finish().await;
}

#[tokio::test]
async fn nobody_may_ask_before_the_first_snapshot() {
    use pmos_directory::snapshot::{FakeClock, SnapshotClient, StaticFetcher};
    use std::sync::Arc;
    let fetcher = Arc::new(StaticFetcher::new(directory()));
    fetcher.set_down(true);
    let people = SnapshotClient::new(fetcher, Arc::new(FakeClock::default()));
    assert!(people.refresh().await.is_err());
    assert!(people.for_queueing_ask().is_none());
    assert!(people.fresh().is_none());
}

// ----- no team.yaml -------------------------------------------------------------------------------

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

fn content() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content")
}

/// A hostile `team.yaml` lists `mallory` as an admin with ana's Slack id; the directory doesn't
/// have her. She can't ask and isn't an admin, and ana is unaffected. Then the same with no
/// `team.yaml` at all.
#[tokio::test]
async fn a_hostile_team_yaml_decides_nothing() {
    let dir = tempfile::tempdir().unwrap();
    copy_dir(&content(), dir.path());
    std::fs::write(
        dir.path().join("team.yaml"),
        "members:\n  - {handle: mallory, slack-id: U0ANA, email: ana@example.com, roles: [pm, admin]}\n",
    )
    .unwrap();
    let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 17).unwrap();
    let env = Env::memory_with(Catalog::load(dir.path(), today).expect("loads")).await;
    let (status, body) = ask_as(&env, "docs", "mallory").await;
    assert_eq!(status, 403, "{body}");
    assert!(!env.rt.is_admin("mallory"));
    let (status, _) = env
        .put(
            "/v1/members/mallory/settings",
            "slack",
            Some("mallory"),
            json!({"level": "staff"}),
        )
        .await;
    assert_eq!(status, 403);
    let (status, body) = ask_as(&env, "docs", "ana").await;
    assert_eq!(status, 201, "{body}");
    env.finish().await;

    std::fs::remove_file(dir.path().join("team.yaml")).unwrap();
    let env = Env::memory_with(Catalog::load(dir.path(), today).expect("loads without it")).await;
    let (status, body) = ask_as(&env, "docs", "ana").await;
    assert_eq!(status, 201, "{body}");
    assert!(env.rt.is_admin("cy"));
    env.finish().await;
}

/// No decision path in the runtime reads the team file: its parser and lookups are gone from
/// the runtime's code. The one mention left is the reloader's watch list, because the lint
/// (in `pmos-contract`) still checks bot owners against it until U7.
#[test]
fn the_runtime_reads_no_team_file() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut mentions = Vec::new();
    for entry in std::fs::read_dir(&src).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or_default();
            for banned in [
                "TeamFile",
                "TeamMember",
                "by_handle(",
                "team::",
                "load_team",
            ] {
                assert!(
                    !code.contains(banned),
                    "{}:{}: `{banned}` in the runtime's code",
                    path.display(),
                    n + 1
                );
            }
            if code.contains("team.yaml") {
                mentions.push(format!("{}: {}", path.display(), line.trim()));
            }
        }
    }
    assert_eq!(mentions.len(), 1, "{mentions:#?}");
    assert!(
        mentions[0].contains("catalog.rs: const WATCHED"),
        "{mentions:#?}"
    );
}

both_envs!(
    a_deactivated_users_ask_is_refused_and_routines_pause,
    a_due_routine_is_skipped_on_a_snapshot_older_than_five_minutes,
);
