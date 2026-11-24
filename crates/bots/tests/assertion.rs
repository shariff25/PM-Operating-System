//! User assertions from the `web` and `console` clients (`docs/user-assertion.md`): what each
//! mode accepts and refuses, and that the Slack client is never affected.

mod common;

use common::signing::*;
use common::*;
use pmos_assertion::unix_now;
use pmos_bots::assertion::Mode;
use reqwest::Method;
use serde_json::{json, Value};

fn refused(status: u16, body: &Value) {
    assert_eq!(status, 401, "{body}");
    assert_eq!(body["code"], "bad_assertion", "{body}");
}

#[tokio::test]
async fn a_valid_assertion_is_accepted_in_verify_and_require() {
    for mode in [Mode::Verify, Mode::Require] {
        let env = Env::memory_with_assertions(assertions(mode)).await;
        let web = Signer::web();
        let body = web_task("ana");
        let a = web.sign("ana", "POST", "/v1/tasks", &body);
        let (s, task) = send(
            &env,
            Method::POST,
            "/v1/tasks",
            "web",
            Some("ana"),
            Some(&body),
            Some(&a),
        )
        .await;
        assert_eq!(s, 201, "{mode:?}: {task}");
        // The subject is the PM even without X-Pmos-User.
        let target = "/v1/tasks?requested_by=ana&limit=5";
        let a = web.sign("ana", "GET", target, b"");
        let (s, list) = send(&env, Method::GET, target, "web", None, None, Some(&a)).await;
        assert_eq!(s, 200, "{list}");
        assert_eq!(list.as_array().unwrap().len(), 1);
        let id = task["id"].as_str().unwrap();
        let path = format!("/v1/tasks/{id}");
        let a = web.sign("ana", "GET", &path, b"");
        let (s, _) = send(&env, Method::GET, &path, "web", Some("ana"), None, Some(&a)).await;
        assert_eq!(s, 200);
        env.finish().await;
    }
}

#[tokio::test]
async fn a_missing_assertion_is_refused_in_require_and_accepted_in_verify_and_off() {
    let body = web_task("ana");
    for (mode, want) in [(Mode::Off, 201), (Mode::Verify, 201), (Mode::Require, 401)] {
        let env = Env::memory_with_assertions(assertions(mode)).await;
        let (s, v) = send(
            &env,
            Method::POST,
            "/v1/tasks",
            "web",
            Some("ana"),
            Some(&body),
            None,
        )
        .await;
        assert_eq!(s, want, "{mode:?}: {v}");
        if want == 401 {
            refused(s, &v);
        }
        env.finish().await;
    }
}

#[tokio::test]
async fn off_ignores_even_a_bad_assertion() {
    let env = Env::memory_with_assertions(assertions(Mode::Off)).await;
    let body = web_task("ana");
    let (s, v) = send(
        &env,
        Method::POST,
        "/v1/tasks",
        "web",
        Some("ana"),
        Some(&body),
        Some("v=1;junk"),
    )
    .await;
    assert_eq!(s, 201, "{v}");
    env.finish().await;
}

#[tokio::test]
async fn bad_assertions_are_refused_alike() {
    for mode in [Mode::Verify, Mode::Require] {
        let env = Env::memory_with_assertions(assertions(mode)).await;
        let web = Signer::web();
        let body = web_task("ana");
        let now = unix_now();
        let post = |a: String| {
            let env = &env;
            let body = body.clone();
            async move {
                send(
                    env,
                    Method::POST,
                    "/v1/tasks",
                    "web",
                    Some("ana"),
                    Some(&body),
                    Some(&a),
                )
                .await
            }
        };

        // Expired: signed two minutes ago for 60 seconds.
        let (s, v) = post(web.sign_at("ana", "POST", "/v1/tasks", &body, now - 120)).await;
        refused(s, &v);
        // Wrong audience: a tool server assertion.
        let ts = Signer {
            audience: "toolserver",
            ..Signer::web()
        };
        let (s, v) = post(ts.sign("ana", "POST", "/v1/tasks", &body)).await;
        refused(s, &v);
        // Wrong issuer: the console's assertion presented with the web token.
        let (s, v) = post(Signer::console().sign("ana", "POST", "/v1/tasks", &body)).await;
        refused(s, &v);
        // Wrong issuer: signed with the web key but claiming to be the console.
        let lying = Signer {
            issuer: "console",
            ..Signer::web()
        };
        let (s, v) = post(lying.sign("ana", "POST", "/v1/tasks", &body)).await;
        refused(s, &v);
        // Tampered: the subject changed after signing.
        let good = web.sign("ana", "POST", "/v1/tasks", &body);
        let (s, v) = post(good.replace("sub=ana", "sub=ben")).await;
        refused(s, &v);
        // Request mismatch: signed for another body, and for another path.
        let (s, v) = post(web.sign("ana", "POST", "/v1/tasks", &web_task("ana")[1..])).await;
        refused(s, &v);
        let (s, v) = post(web.sign("ana", "POST", "/v1/routines", &body)).await;
        refused(s, &v);
        // Not an assertion at all.
        let (s, v) = post("hello".into()).await;
        refused(s, &v);
        // Replayed: accepted once, refused the second time.
        let once = web.sign("ana", "POST", "/v1/tasks", &body);
        let (s, v) = post(once.clone()).await;
        assert_eq!(s, 201, "{v}");
        let (s, v) = post(once).await;
        refused(s, &v);
        env.finish().await;
    }
}

#[tokio::test]
async fn the_subject_must_be_every_person_the_request_names() {
    let env = Env::memory_with_assertions(assertions(Mode::Require)).await;
    let web = Signer::web();
    // X-Pmos-User names someone else.
    let body = web_task("ben");
    let a = web.sign("ana", "POST", "/v1/tasks", &body);
    let (s, v) = send(
        &env,
        Method::POST,
        "/v1/tasks",
        "web",
        Some("ben"),
        Some(&body),
        Some(&a),
    )
    .await;
    refused(s, &v);
    // A filter names someone else.
    let target = "/v1/tasks?requested_by=ben";
    let a = web.sign("ana", "GET", target, b"");
    let (s, v) = send(
        &env,
        Method::GET,
        target,
        "web",
        Some("ana"),
        None,
        Some(&a),
    )
    .await;
    refused(s, &v);
    // The settings path names someone else.
    let path = "/v1/members/ben/settings";
    let a = web.sign("ana", "GET", path, b"");
    let (s, v) = send(&env, Method::GET, path, "web", None, None, Some(&a)).await;
    refused(s, &v);
    // The body names someone else: the web client's own rule still refuses it.
    let a = web.sign("ana", "POST", "/v1/tasks", &body);
    let (s, v) = send(
        &env,
        Method::POST,
        "/v1/tasks",
        "web",
        None,
        Some(&body),
        Some(&a),
    )
    .await;
    assert_eq!(s, 403, "{v}");
    env.finish().await;
}

/// Under `require`, the console reads one task's detail, events, trace or drafts only signed
/// for a viewer who is that task's PM or an active admin in the user directory. Its list of
/// every task (metadata) still needs none. In `verify`, an unsigned read still works.
#[tokio::test]
async fn the_console_reads_one_task_only_for_its_pm_or_an_admin() {
    let env = Env::memory_with_catalog_and_assertions(catalog(), assertions(Mode::Require)).await;
    let id = env.ask("What's on the roadmap?").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let console = Signer::console();
    for target in [
        format!("/v1/tasks/{id}"),
        format!("/v1/tasks/{id}/events"),
        format!("/v1/tasks/{id}/trace"),
        format!("/v1/tasks/{id}/drafts"),
    ] {
        let (s, v) = send(&env, Method::GET, &target, "console", None, None, None).await;
        refused(s, &v);
        for (viewer, want) in [("ana", 200), ("ben", 404), ("cy", 200)] {
            let a = console.sign(viewer, "GET", &target, b"");
            let (s, v) = send(&env, Method::GET, &target, "console", None, None, Some(&a)).await;
            assert_eq!(s, want, "{viewer} {target}: {v}");
        }
        // The assertion's own roles don't make an admin.
        let claims = pmos_assertion::Claims::new(
            "console",
            "bots",
            "ben",
            &[pmos_contract::model::Role::Admin],
            "ses_test",
        )
        .unwrap();
        let req = pmos_assertion::RequestBinding::new("GET", &target, b"").unwrap();
        let a = console
            .ring
            .sign(&claims, unix_now(), 60, Some(&req))
            .unwrap();
        let (s, _) = send(&env, Method::GET, &target, "console", None, None, Some(&a)).await;
        assert_eq!(s, 404, "{target}");
    }
    // Under `require` the list needs a viewer too (R1): unsigned, it is refused.
    let (s, v) = send(&env, Method::GET, "/v1/tasks", "console", None, None, None).await;
    refused(s, &v);
    env.finish().await;

    let env = Env::memory_with_assertions(assertions(Mode::Verify)).await;
    let id = env.ask("q").await["id"].as_str().unwrap().to_string();
    let (s, v) = send(
        &env,
        Method::GET,
        &format!("/v1/tasks/{id}"),
        "console",
        None,
        None,
        None,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    env.finish().await;
}

/// Under `require`, every console call to the runtime is signed for its viewer (R1): the bot
/// list, the task and routine lists included.
#[tokio::test]
async fn the_console_signs_every_call_under_require() {
    let env = Env::memory_with_assertions(assertions(Mode::Require)).await;
    let console = Signer::console();
    for target in [
        "/v1/bots",
        "/v1/bots/docs",
        "/v1/tasks?limit=5",
        "/v1/routines",
    ] {
        let (s, v) = send(&env, Method::GET, target, "console", None, None, None).await;
        refused(s, &v);
        let a = console.sign("ana", "GET", target, b"");
        let (s, v) = send(&env, Method::GET, target, "console", None, None, Some(&a)).await;
        assert_eq!(s, 200, "{target}: {v}");
        // The web key does not speak for the console.
        let a = Signer::web().sign("ana", "GET", target, b"");
        let (s, v) = send(&env, Method::GET, target, "console", None, None, Some(&a)).await;
        refused(s, &v);
    }
    // One person's tasks name them: the viewer may name only themselves.
    let target = "/v1/tasks?limit=5&requested_by=ben";
    let a = console.sign("ana", "GET", target, b"");
    let (s, v) = send(&env, Method::GET, target, "console", None, None, Some(&a)).await;
    refused(s, &v);
    env.finish().await;
}

fn routine_draft(owner: &str, prompt: &str) -> Value {
    json!({"draft": {"bot": "docs", "owner": owner,
        "schedule": {"cron": "0 8 * * *", "tz": "UTC"}, "prompt": prompt,
        "output": {"kind": "dm"}, "approval": "approve"}})
}

/// R1: a non-admin viewer lists only their own tasks and routines, in full; an admin (in the
/// user directory) lists everyone's, with the ask, the result and the prompt of
/// anyone else's left out. Cancel and routine changes: the owner or an admin, signed, and
/// recorded as that person.
#[tokio::test]
async fn console_lists_are_the_viewers_own_or_metadata_for_an_admin() {
    let env = Env::memory_with_catalog_and_assertions(catalog(), assertions(Mode::Require)).await;
    let console = Signer::console();
    let ana_task = env.ask("Ana's private question").await;
    let (s, ben_task) = env
        .post(
            "/v1/tasks",
            "slack",
            Some("ben"),
            json!({"bot": "docs", "requested_by": "ben", "text": "Ben's private question",
                   "source": "slack", "slack": {"channel": "D0BEN", "thread_ts": "1731.0002"}}),
        )
        .await;
    assert_eq!(s, 201, "{ben_task}");
    for owner in ["ana", "ben"] {
        let (s, r) = env
            .post(
                "/v1/routines",
                "slack",
                Some(owner),
                routine_draft(owner, &format!("{owner}'s secret prompt")),
            )
            .await;
        assert_eq!(s, 201, "{r}");
    }
    let get = |viewer: &'static str, target: &'static str| {
        let env = &env;
        let console = &console;
        async move {
            let a = console.sign(viewer, "GET", target, b"");
            let (s, v) = send(env, Method::GET, target, "console", None, None, Some(&a)).await;
            assert_eq!(s, 200, "{viewer} {target}: {v}");
            v.as_array().unwrap().clone()
        }
    };

    // ana: her own task only, in full.
    let tasks = get("ana", "/v1/tasks").await;
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["requested_by"], "ana");
    assert_eq!(tasks[0]["text"], "Ana's private question");
    assert!(tasks[0].get("hidden").is_none());
    // cy, an admin: both, ben's and ana's as metadata (not cy's own).
    let tasks = get("cy", "/v1/tasks").await;
    assert_eq!(tasks.len(), 2);
    for t in &tasks {
        assert_eq!(t["text"], "", "{t}");
        assert_eq!(t["result"], Value::Null);
        assert_eq!(t["hidden"], true);
        assert!(t["id"].is_string() && t["bot"] == "docs" && t["status"] == "queued");
        assert!(t["created_at"].is_string() && t["requested_by"].is_string());
        assert!(t.get("slack").is_none(), "{t}");
    }
    // ben, not an admin, sees only his own.
    let tasks = get("ben", "/v1/tasks").await;
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["requested_by"], "ben");

    // Routines alike.
    let routines = get("ana", "/v1/routines").await;
    assert_eq!(routines.len(), 1);
    assert_eq!(routines[0]["prompt"], "ana's secret prompt");
    let routines = get("cy", "/v1/routines").await;
    assert_eq!(routines.len(), 2);
    assert!(routines
        .iter()
        .all(|r| r["prompt"] == "" && r["hidden"] == true));
    let ana_routine = routines.iter().find(|r| r["owner"] == "ana").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Routine changes: not ben on ana's; cy, an admin, may.
    let path = format!("/v1/routines/{ana_routine}");
    let body = serde_json::to_vec(&json!({"enabled": false})).unwrap();
    for (viewer, want) in [("ben", 404), ("cy", 200), ("ana", 200)] {
        let a = console.sign(viewer, "PATCH", &path, &body);
        let (s, v) = send(
            &env,
            Method::PATCH,
            &path,
            "console",
            None,
            Some(&body),
            Some(&a),
        )
        .await;
        assert_eq!(s, want, "{viewer}: {v}");
    }

    // Cancel: ben can't cancel ana's task; ana can, and it is recorded as her.
    let ana_id = ana_task["id"].as_str().unwrap();
    let path = format!("/v1/tasks/{ana_id}/cancel");
    let (s, v) = send(&env, Method::POST, &path, "console", None, None, None).await;
    refused(s, &v);
    let a = console.sign("ben", "POST", &path, b"");
    let (s, _) = send(&env, Method::POST, &path, "console", None, None, Some(&a)).await;
    assert_eq!(s, 404);
    let a = console.sign("ana", "POST", &path, b"");
    let (s, v) = send(&env, Method::POST, &path, "console", None, None, Some(&a)).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["status"], "cancelled");
    let events = format!("/v1/tasks/{ana_id}/events");
    let a = console.sign("ana", "GET", &events, b"");
    let (_, ev) = send(&env, Method::GET, &events, "console", None, None, Some(&a)).await;
    assert!(
        ev.as_array()
            .unwrap()
            .iter()
            .any(|e| e["text"] == "cancelled by ana"),
        "{ev}"
    );
    // cy, an admin, cancels ben's, recorded as cy.
    let ben_id = ben_task["id"].as_str().unwrap();
    let path = format!("/v1/tasks/{ben_id}/cancel");
    let a = console.sign("cy", "POST", &path, b"");
    let (s, _) = send(&env, Method::POST, &path, "console", None, None, Some(&a)).await;
    assert_eq!(s, 200);
    let events = format!("/v1/tasks/{ben_id}/events");
    let a = console.sign("cy", "GET", &events, b"");
    let (_, ev) = send(&env, Method::GET, &events, "console", None, None, Some(&a)).await;
    assert!(ev
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["text"] == "cancelled by cy"));
    env.finish().await;
}

/// Without a viewer (`off`, or an unsigned call in `verify`), the console's lists are
/// metadata for everyone: no ask, result or prompt reaches it on its token alone.
#[tokio::test]
async fn an_unsigned_console_list_is_metadata_in_every_mode() {
    for mode in [Mode::Off, Mode::Verify] {
        let env = Env::memory_with_assertions(assertions(mode)).await;
        env.ask("Ana's private question").await;
        let (s, r) = env
            .post(
                "/v1/routines",
                "slack",
                Some("ana"),
                routine_draft("ana", "secret"),
            )
            .await;
        assert_eq!(s, 201, "{r}");
        let (_, tasks) = env.get("/v1/tasks", "console", None).await;
        assert_eq!(tasks.as_array().unwrap().len(), 1);
        assert_eq!(tasks[0]["text"], "", "{mode:?}");
        assert_eq!(tasks[0]["hidden"], true);
        let (_, routines) = env.get("/v1/routines", "console", None).await;
        assert_eq!(routines[0]["prompt"], "", "{mode:?}");
        env.finish().await;
    }
}

/// R14: every runtime route × method × client token, without and with a user assertion,
/// under `require`: exactly the allowed set gets past auth (not `401`/`403`); a `web` call
/// that names its PM and every console call need the assertion (`401` without); the Slack,
/// worker and evals clients are never checked.
#[tokio::test]
async fn every_route_client_and_assertion_under_require() {
    let env = Env::memory_with_assertions(assertions(Mode::Require)).await;
    let tid = env.ask("Q").await["id"].as_str().unwrap().to_string();
    let (_, r) = env
        .post(
            "/v1/routines",
            "slack",
            Some("ana"),
            routine_draft("ana", "p"),
        )
        .await;
    let rid = r["id"].as_str().unwrap().to_string();

    // (method, path, body, clients allowed once past the assertion check)
    let rows: Vec<(Method, String, Value, &[&str])> = vec![
        (
            Method::GET,
            "/v1/bots".into(),
            json!(null),
            &["slack", "web", "worker", "console", "evals"],
        ),
        (
            Method::GET,
            "/v1/bots/docs".into(),
            json!(null),
            &["slack", "web", "worker", "console", "evals"],
        ),
        (
            Method::POST,
            "/v1/tasks".into(),
            json!({"bot": "docs", "requested_by": "ben", "text": "t", "source": "eval"}),
            &["evals"],
        ),
        (
            Method::GET,
            "/v1/tasks".into(),
            json!(null),
            &["slack", "web", "console", "evals"],
        ),
        (
            Method::GET,
            format!("/v1/tasks/{tid}"),
            json!(null),
            &["slack", "web", "console", "evals"],
        ),
        (
            Method::GET,
            format!("/v1/tasks/{tid}/events"),
            json!(null),
            &["slack", "web", "console", "evals"],
        ),
        (
            Method::GET,
            format!("/v1/tasks/{tid}/trace"),
            json!(null),
            &["console"],
        ),
        (
            Method::GET,
            format!("/v1/tasks/{tid}/drafts"),
            json!(null),
            &["console", "evals"],
        ),
        (
            Method::POST,
            "/v1/worker/claim".into(),
            json!({"worker": "w1"}),
            &["worker"],
        ),
        (
            Method::POST,
            format!("/v1/worker/tasks/{tid}/heartbeat"),
            json!({}),
            &["worker"],
        ),
        (
            Method::POST,
            format!("/v1/worker/tasks/{tid}/events"),
            json!({"kind": "status", "text": "x"}),
            &["worker"],
        ),
        (
            Method::POST,
            format!("/v1/worker/tasks/{tid}/trace"),
            json!({"entries": []}),
            &["worker"],
        ),
        (
            Method::POST,
            format!("/v1/worker/tasks/{tid}/complete"),
            json!({"result": "r"}),
            &["worker"],
        ),
        (
            Method::POST,
            format!("/v1/worker/tasks/{tid}/fail"),
            json!({"reason": "r"}),
            &["worker"],
        ),
        (
            Method::GET,
            "/v1/notifications".into(),
            json!(null),
            &["slack"],
        ),
        (
            Method::GET,
            "/v1/members/ana/settings".into(),
            json!(null),
            &["slack", "web", "console"],
        ),
        (
            Method::PUT,
            "/v1/members/ana/settings".into(),
            json!({"level": "senior"}),
            &["slack", "web"],
        ),
        (
            Method::POST,
            "/v1/routines/parse".into(),
            json!({"bot": "docs", "owner": "ana", "text": "t", "tz": "UTC"}),
            &["slack", "web"],
        ),
        (
            Method::POST,
            "/v1/routines".into(),
            json!({"draft": {}}),
            &["slack", "web"],
        ),
        (
            Method::GET,
            "/v1/routines".into(),
            json!(null),
            &["slack", "web", "console"],
        ),
        (
            Method::PATCH,
            format!("/v1/routines/{rid}"),
            json!({"enabled": true}),
            &["slack", "web", "console"],
        ),
        (
            Method::DELETE,
            format!("/v1/routines/{rid}"),
            json!(null),
            &["slack", "web", "console"],
        ),
        // Signed, the console cancels (its viewer's own task, or any for an admin). Evals may
        // cancel eval tasks only: this one is a 404 for it.
        (
            Method::POST,
            format!("/v1/tasks/{tid}/cancel"),
            json!({}),
            &["slack", "web", "console", "evals"],
        ),
    ];
    for (method, path, body, allowed) in &rows {
        let bytes: Vec<u8> = if body.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(body).unwrap()
        };
        for client in ["slack", "web", "worker", "console", "evals", "none"] {
            for signed in [false, true] {
                let user = matches!(client, "slack" | "web").then_some("ana");
                let assertion = match (signed, client) {
                    (true, "console") => {
                        Some(Signer::console().sign("ana", method.as_str(), path, &bytes))
                    }
                    (true, "web") => Some(Signer::web().sign("ana", method.as_str(), path, &bytes)),
                    // Every other client's assertion is ignored: send one anyway.
                    (true, _) => Some(Signer::web().sign("ana", method.as_str(), path, &bytes)),
                    (false, _) => None,
                };
                let (status, v) = send(
                    &env,
                    method.clone(),
                    path,
                    client,
                    user,
                    (!bytes.is_empty()).then_some(bytes.as_slice()),
                    assertion.as_deref(),
                )
                .await;
                let what = format!("{method} {path} as {client}, signed: {signed}: {v}");
                match client {
                    "none" => assert_eq!(status, 401, "{what}"),
                    "web" | "console" if !signed => {
                        assert_eq!(status, 401, "{what}");
                        assert_eq!(v["code"], "bad_assertion", "{what}");
                    }
                    _ if allowed.contains(&client) => {
                        assert!(status != 401 && status != 403, "{what}")
                    }
                    _ => assert_eq!(status, 403, "{what}"),
                }
            }
        }
    }
    env.finish().await;
}

#[tokio::test]
async fn slack_is_unaffected_in_every_mode() {
    for mode in [Mode::Off, Mode::Verify, Mode::Require] {
        let env = Env::memory_with_assertions(assertions(mode)).await;
        let body = serde_json::to_vec(&json!({"bot": "docs", "requested_by": "ana", "text": "hi",
            "source": "slack", "slack": {"channel": "D0ANA", "thread_ts": "1.2"}}))
        .unwrap();
        // No assertion, and a junk one: both ignored.
        for a in [None, Some("v=1;junk")] {
            let (s, v) = send(
                &env,
                Method::POST,
                "/v1/tasks",
                "slack",
                Some("ana"),
                Some(&body),
                a,
            )
            .await;
            assert_eq!(s, 201, "{mode:?}: {v}");
            let (s, v) = send(
                &env,
                Method::GET,
                "/v1/tasks?requested_by=ana",
                "slack",
                Some("ana"),
                None,
                a,
            )
            .await;
            assert_eq!(s, 200, "{mode:?}: {v}");
        }
        // Evals and workers are not checked either.
        let (s, _) = send(
            &env,
            Method::GET,
            "/v1/tasks",
            "evals",
            None,
            None,
            Some("junk"),
        )
        .await;
        assert_eq!(s, 200);
        env.finish().await;
    }
}
