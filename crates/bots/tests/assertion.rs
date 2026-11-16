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

#[tokio::test]
async fn the_console_signs_only_what_names_a_person() {
    let env = Env::memory_with_assertions(assertions(Mode::Require)).await;
    let console = Signer::console();
    // Read everything: no person, no assertion needed.
    let (s, v) = send(
        &env,
        Method::GET,
        "/v1/tasks?limit=5",
        "console",
        None,
        None,
        None,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = send(
        &env,
        Method::GET,
        "/v1/routines",
        "console",
        None,
        None,
        None,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    // One person's tasks: needs an assertion for them.
    let target = "/v1/tasks?limit=5&requested_by=ana";
    let (s, v) = send(&env, Method::GET, target, "console", None, None, None).await;
    refused(s, &v);
    let a = console.sign("ana", "GET", target, b"");
    let (s, v) = send(&env, Method::GET, target, "console", None, None, Some(&a)).await;
    assert_eq!(s, 200, "{v}");
    // The web key does not speak for the console.
    let a = Signer::web().sign("ana", "GET", target, b"");
    let (s, v) = send(&env, Method::GET, target, "console", None, None, Some(&a)).await;
    refused(s, &v);
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
