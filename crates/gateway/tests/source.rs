//! Slice H4 (`docs/openshell-any-host-spec.md` §3.6): the model source. Which configs each
//! source starts with, the source on every answer, callers naming the source they expect (X4),
//! neutral error bodies under `hosted-test` (V3), no redirects, the 30-second readiness cache
//! for a remote test upstream, and a production gateway's pinned model server names.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use pmos_contract::model_source::ModelSource;
use pmos_gateway::config::{Client, Config, GatewayClass, Models};
use pmos_gateway::usage::UsageLog;
use pmos_gateway::{router, GatewayConfig, Timeouts};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "gateway-source-test-token-0123456789";
const MARKER: &str = "acct-7781-vendorname-sk-masked";

async fn serve(app: Router) -> (String, CancellationToken) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let cancel = CancellationToken::new();
    let ct = cancel.clone();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { ct.cancelled_owned().await })
            .await
            .unwrap();
    });
    (base, cancel)
}

/// A mock upstream that counts the calls it gets and answers as `mode` says.
#[derive(Clone)]
struct Mock {
    calls: Arc<AtomicUsize>,
    probes: Arc<AtomicUsize>,
    mode: &'static str,
    redirect_to: String,
}

async fn mock_chat(State(m): State<Mock>, body: Bytes) -> Response {
    m.calls.fetch_add(1, Ordering::SeqCst);
    let req: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    match m.mode {
        "refuse" => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": {"message": format!("bad key for {MARKER}")}})),
        )
            .into_response(),
        "redirect" => (
            StatusCode::TEMPORARY_REDIRECT,
            [("location", m.redirect_to.clone())],
            format!("go to {MARKER}"),
        )
            .into_response(),
        _ if req["stream"] == json!(true) => (
            [("content-type", "text/event-stream")],
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n",
        )
            .into_response(),
        _ => Json(json!({"id": "x", "object": "chat.completion", "model": "m",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}}]}))
        .into_response(),
    }
}

async fn start_mock(mode: &'static str, redirect_to: String) -> (String, Mock, CancellationToken) {
    let m = Mock {
        calls: Arc::default(),
        probes: Arc::default(),
        mode,
        redirect_to,
    };
    let app = Router::new()
        .route(
            "/v1/models",
            get(|State(m): State<Mock>| async move {
                m.probes.fetch_add(1, Ordering::SeqCst);
                Json(json!({"data": []}))
            }),
        )
        .route("/v1/chat/completions", post(mock_chat))
        .with_state(m.clone());
    let (base, stop) = serve(app).await;
    (format!("{base}/v1"), m, stop)
}

struct Gw {
    base: String,
    stop: CancellationToken,
}

async fn gateway(yaml: &str, source: ModelSource, stub: bool, ready_cache: Duration) -> Gw {
    let models = Models::parse(yaml, true).unwrap();
    let (base, stop) = serve(router(GatewayConfig {
        models,
        clients: vec![Client {
            name: "runtime".into(),
            token: TOKEN.into(),
        }],
        usage: Arc::new(UsageLog::new(None).unwrap()),
        stub,
        workarounds_on: true,
        anthropic_key: String::new(),
        remote_key: "remote-key-for-tests".into(),
        retry: pmos_gateway::retry::RetryPolicy {
            max_attempts: 1,
            ..Default::default()
        },
        timeouts: Timeouts {
            ready_cache,
            ..Default::default()
        },
        model_source: source,
    }))
    .await;
    Gw { base, stop }
}

fn local_yaml(upstream: &str) -> String {
    format!("upstreams: {{a: '{upstream}'}}\nroles: {{default: {{upstream: a, model: m}}}}\n")
}

fn remote_yaml(upstream: &str) -> String {
    format!(
        "upstreams: {{r: 'openai+{upstream}'}}\n\
         roles: {{default: {{upstream: r, model: secret-model-id, label: hosted-model-a}}}}\n"
    )
}

fn call(gw: &Gw, expected: Option<&str>, stream: bool) -> reqwest::RequestBuilder {
    let mut b = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", gw.base))
        .bearer_auth(TOKEN)
        .json(&json!({"model": "pmos/default", "stream": stream,
                      "messages": [{"role": "user", "content": "hi"}]}));
    if let Some(s) = expected {
        b = b.header("X-Pmos-Model-Source", s);
    }
    b
}

fn source_of(res: &reqwest::Response) -> String {
    res.headers()
        .get("x-pmos-model-source")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("<none>")
        .to_string()
}

/// X4 (r7): a call without the header is `400 model_source_required`, one naming another
/// source `409 model_source_mismatch`, and neither reaches the upstream: whole and streamed,
/// and with the stub on too. Every answer, refusals included, says the gateway's source.
#[tokio::test]
async fn callers_must_name_the_source_and_a_mismatch_never_leaves_the_gateway() {
    let (up, mock, stop_up) = start_mock("ok", String::new()).await;
    let gw = gateway(&local_yaml(&up), ModelSource::Local, false, Duration::ZERO).await;
    for stream in [false, true] {
        let res = call(&gw, None, stream).send().await.unwrap();
        assert_eq!(res.status(), 400);
        assert_eq!(source_of(&res), "local");
        let v: Value = res.json().await.unwrap();
        assert_eq!(v["error"]["type"], "model_source_required");
        for other in ["hosted-test", "stub", "hosted:owner-hosted", "", "LOCAL"] {
            let res = call(&gw, Some(other), stream).send().await.unwrap();
            assert_eq!(res.status(), 409, "{other}");
            assert_eq!(source_of(&res), "local");
            let v: Value = res.json().await.unwrap();
            assert_eq!(v["error"]["type"], "model_source_mismatch");
        }
        assert_eq!(
            mock.calls.load(Ordering::SeqCst),
            0,
            "nothing went upstream"
        );
    }
    // The right source passes, whole and streamed, and the answer says it.
    let res = call(&gw, Some("local"), false).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(source_of(&res), "local");
    let res = call(&gw, Some("local"), true).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(source_of(&res), "local");
    assert!(res.text().await.unwrap().contains("[DONE]"));
    assert_eq!(mock.calls.load(Ordering::SeqCst), 2);
    gw.stop.cancel();

    // The stub: it reports `stub`, and still refuses a call that doesn't name it.
    let stub = gateway(&local_yaml(&up), ModelSource::Local, true, Duration::ZERO).await;
    let res = call(&stub, None, false).send().await.unwrap();
    assert_eq!(res.status(), 400);
    assert_eq!(source_of(&res), "stub");
    let res = call(&stub, Some("local"), true).send().await.unwrap();
    assert_eq!(res.status(), 409);
    let res = call(&stub, Some("stub"), false).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(source_of(&res), "stub");
    stub.stop.cancel();
    stop_up.cancel();
}

/// Every answer carries `x-pmos-model-source`: readiness, liveness, an unauthenticated call,
/// the roles list; and `/readyz` and `/v1/pmos/roles` say it in their bodies.
#[tokio::test]
async fn every_answer_and_both_status_routes_say_the_source() {
    let (up, _mock, stop_up) = start_mock("ok", String::new()).await;
    let yaml = remote_yaml(&up);
    let gw = gateway(&yaml, ModelSource::HostedTest, false, Duration::ZERO).await;
    let http = reqwest::Client::new();
    for path in ["/healthz", "/readyz", "/v1/models", "/v1/pmos/roles"] {
        let res = http.get(format!("{}{path}", gw.base)).send().await.unwrap();
        assert_eq!(source_of(&res), "hosted-test", "{path} without a token");
    }
    let ready: Value = http
        .get(format!("{}/readyz", gw.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ready["model_source"], "hosted-test");
    let roles: Value = http
        .get(format!("{}/v1/pmos/roles", gw.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(roles["model_source"], "hosted-test");
    assert!(!roles.to_string().contains("secret-model-id"), "{roles}");
    gw.stop.cancel();
    let stub = gateway(&yaml, ModelSource::HostedTest, true, Duration::ZERO).await;
    let ready: Value = http
        .get(format!("{}/readyz", stub.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ready["model_source"], "stub");
    stub.stop.cancel();
    stop_up.cancel();
}

/// V3 (r6): under `hosted-test` a provider's error body never reaches the caller, only
/// `{type, status}` and a fixed message, for an unlabelled role too.
#[tokio::test]
async fn hosted_test_replaces_every_upstream_error_body() {
    let (up, mock, stop_up) = start_mock("refuse", String::new()).await;
    // Unlabelled on purpose: the replacement doesn't rest on the label.
    let yaml = format!(
        "upstreams: {{r: 'openai+{up}'}}\nroles: {{default: {{upstream: r, model: secret-model-id}}}}\n"
    );
    let gw = gateway(&yaml, ModelSource::HostedTest, false, Duration::ZERO).await;
    for stream in [false, true] {
        let res = call(&gw, Some("hosted-test"), stream).send().await.unwrap();
        assert_eq!(res.status(), 401);
        assert_eq!(source_of(&res), "hosted-test");
        let text = res.text().await.unwrap();
        assert!(!text.contains(MARKER), "{text}");
        assert!(!text.contains(&up), "{text}");
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["error"]["status"], 401);
        assert_eq!(v["error"]["type"], "auth");
    }
    assert_eq!(mock.calls.load(Ordering::SeqCst), 2);
    gw.stop.cancel();
    // Under `local` an unlabelled role's own error still reaches the caller (it is PM OS's own
    // model server, whose messages help).
    let gw = gateway(&local_yaml(&up), ModelSource::Local, false, Duration::ZERO).await;
    let text = call(&gw, Some("local"), false)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(text.contains(MARKER), "{text}");
    gw.stop.cancel();
    stop_up.cancel();
}

/// No redirect is followed by either client (streamed or whole): a `307` to a second server
/// is a `502` for that call, the second server sees nothing, and neither the `Location` nor
/// the body reaches the caller.
#[tokio::test]
async fn neither_client_follows_a_redirect() {
    let (second, second_mock, stop_second) = start_mock("ok", String::new()).await;
    let (up, mock, stop_up) = start_mock("redirect", format!("{second}/chat/completions")).await;
    for (source, yaml, name) in [
        (ModelSource::Local, local_yaml(&up), "local"),
        (ModelSource::HostedTest, remote_yaml(&up), "hosted-test"),
    ] {
        let gw = gateway(&yaml, source, false, Duration::ZERO).await;
        for stream in [false, true] {
            let res = call(&gw, Some(name), stream).send().await.unwrap();
            assert_eq!(res.status(), 502, "{name} stream {stream}");
            assert!(res.headers().get("location").is_none());
            let text = res.text().await.unwrap();
            assert!(!text.contains(MARKER) && !text.contains(&second), "{text}");
        }
        gw.stop.cancel();
    }
    assert_eq!(mock.calls.load(Ordering::SeqCst), 4);
    assert_eq!(
        second_mock.calls.load(Ordering::SeqCst),
        0,
        "nothing followed"
    );
    stop_up.cancel();
    stop_second.cancel();
}

/// `/readyz` probes a remote test upstream (with its key) at most once per 30 seconds,
/// however many callers ask, even when configured to probe every time.
#[tokio::test]
async fn readiness_probes_a_remote_test_upstream_at_most_every_30_seconds() {
    let (up, mock, stop_up) = start_mock("ok", String::new()).await;
    let gw = gateway(
        &remote_yaml(&up),
        ModelSource::HostedTest,
        false,
        Duration::ZERO,
    )
    .await;
    let http = reqwest::Client::new();
    let mut burst = Vec::new();
    for _ in 0..20 {
        burst.push(http.get(format!("{}/readyz", gw.base)).send());
    }
    for res in futures_util::future::join_all(burst).await {
        assert_eq!(res.unwrap().status(), 200);
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _ = http
        .get(format!("{}/readyz", gw.base))
        .send()
        .await
        .unwrap();
    assert_eq!(mock.probes.load(Ordering::SeqCst), 1, "one probe in all");
    gw.stop.cancel();
    // A local model server keeps the configured cache (here: none).
    let gw = gateway(&local_yaml(&up), ModelSource::Local, false, Duration::ZERO).await;
    for _ in 0..3 {
        let _ = http
            .get(format!("{}/readyz", gw.base))
            .send()
            .await
            .unwrap();
    }
    assert_eq!(mock.probes.load(Ordering::SeqCst), 4);
    gw.stop.cancel();
    stop_up.cancel();
}

/// A production gateway reaches its model servers only by their pinned names, resolved by its
/// own resolver at every connection: here the name doesn't resolve to its pinned address (it
/// doesn't resolve at all), so the call is a `502` naming no address.
#[tokio::test]
async fn a_production_gateway_refuses_a_name_not_answered_by_its_pinned_address() {
    let yaml = "class: production\nupstreams: {a: 'http://vllm-default.pmos.internal:8000/v1'}\n\
                roles: {default: {upstream: a, model: m}}\n";
    let gw = gateway(yaml, ModelSource::Local, false, Duration::ZERO).await;
    let res = call(&gw, Some("local"), false).send().await.unwrap();
    assert_eq!(res.status(), 502);
    let text = res.text().await.unwrap();
    assert!(!text.contains("10.201.77"), "{text}");
    let ready = reqwest::get(format!("{}/readyz", gw.base)).await.unwrap();
    assert_eq!(ready.status(), 503);
    gw.stop.cancel();
}

fn config(pairs: &[(&str, &str)]) -> Result<Config, String> {
    let token = format!("runtime={TOKEN}");
    let pairs: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Config::from_lookup(move |k| {
        if k == "PMOS_GATEWAY_TOKENS" {
            return Some(token.clone());
        }
        pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
    })
}

/// Which configs each source starts with (spec §6, "The service refusals").
#[test]
fn each_source_starts_only_with_its_own_kind_of_config() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let production = std::fs::read_to_string(root.join("deploy/gateway.yaml")).unwrap();
    let production = Models::parse(&production, false).unwrap();
    assert_eq!(production.class, GatewayClass::Production);
    let test_local = Models::from_yaml(&local_yaml("http://127.0.0.1:1/v1")).unwrap();
    let remote_labelled = Models::parse(&remote_yaml("https://api.example.test/v1"), true).unwrap();
    let remote_unlabelled = Models::parse(
        "upstreams: {r: 'openai+https://api.example.test/v1'}\nroles: {default: {upstream: r, model: m}}\n",
        true,
    )
    .unwrap();

    // PMOS_MODEL_SOURCE is required, with no default, and `hosted` isn't built.
    for missing in [vec![], vec![("PMOS_MODEL_SOURCE", "")]] {
        let err = config(&missing).unwrap_err();
        assert!(err.contains("PMOS_MODEL_SOURCE is required"), "{err}");
    }
    let err = config(&[("PMOS_MODEL_SOURCE", "hosted")]).unwrap_err();
    assert!(err.contains("not built yet"), "{err}");

    let local = |extra: &[(&str, &str)]| {
        let mut p = vec![("PMOS_MODEL_SOURCE", "local")];
        p.extend_from_slice(extra);
        config(&p).unwrap()
    };
    // `local`: production config only, never CLOUD_TEST; the stub may be on.
    assert!(local(&[]).check_source(&production).is_ok());
    assert!(local(&[("PMOS_GATEWAY_STUB", "1")])
        .check_source(&production)
        .is_ok());
    let err = local(&[]).check_source(&test_local).unwrap_err();
    assert!(err.contains("class: production"), "{err}");
    let err = local(&[("PMOS_GATEWAY_STUB", "1")])
        .check_source(&test_local)
        .unwrap_err();
    assert!(err.contains("class: production"), "{err}");
    let err = local(&[("PMOS_GATEWAY_CLOUD_TEST", "1")])
        .check_source(&production)
        .unwrap_err();
    assert!(err.contains("PMOS_GATEWAY_CLOUD_TEST"), "{err}");
    assert_eq!(local(&[]).served_source(), "local");
    assert_eq!(local(&[("PMOS_GATEWAY_STUB", "1")]).served_source(), "stub");

    let hosted_test = |extra: &[(&str, &str)]| {
        let mut p = vec![("PMOS_MODEL_SOURCE", "hosted-test")];
        p.extend_from_slice(extra);
        config(&p).unwrap()
    };
    // `hosted-test`: a test config, CLOUD_TEST unless the stub is on, a label on every role.
    assert!(hosted_test(&[("PMOS_GATEWAY_CLOUD_TEST", "1")])
        .check_source(&remote_labelled)
        .is_ok());
    let err = hosted_test(&[]).check_source(&remote_labelled).unwrap_err();
    assert!(err.contains("PMOS_GATEWAY_CLOUD_TEST=1"), "{err}");
    assert!(hosted_test(&[("PMOS_GATEWAY_STUB", "1")])
        .check_source(&remote_labelled)
        .is_ok());
    let err = hosted_test(&[("PMOS_GATEWAY_CLOUD_TEST", "1")])
        .check_source(&remote_unlabelled)
        .unwrap_err();
    assert!(err.contains("no `label:`"), "{err}");
    let err = hosted_test(&[("PMOS_GATEWAY_STUB", "1")])
        .check_source(&production)
        .unwrap_err();
    assert!(err.contains("class: test"), "{err}");

    // A `hosted:` upstream isn't a form any source takes in this build (slice H12).
    for class in ["", "class: production\n"] {
        let err = Models::parse(
            &format!("{class}upstreams: {{h: 'hosted:owner-hosted'}}\nroles: {{default: {{upstream: h, model: m}}}}\n"),
            true,
        )
        .unwrap_err();
        assert!(err.contains("http or https"), "{err}");
    }
    // A production config names only the two pinned model servers.
    for other in [
        "http://vllm.pmos.internal:8000/v1",
        "http://gpu.models.pmos.internal/v1",
        "http://gateway.pmos.internal:8740/v1",
    ] {
        let err = Models::from_yaml(&format!(
            "class: production\nupstreams: {{a: '{other}'}}\nroles: {{default: {{upstream: a, model: m}}}}\n"
        ))
        .unwrap_err();
        assert!(err.contains("pinned"), "{other}: {err}");
    }
}

/// The shipped configs start under the source each is for: deploy/gateway.yaml under `local`
/// (and nothing else), the hosted-test example and the test stack's Claude config under
/// `hosted-test` with CLOUD_TEST.
#[test]
fn the_shipped_configs_fit_their_sources() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |p: &str| std::fs::read_to_string(root.join(p)).unwrap();
    let local = config(&[("PMOS_MODEL_SOURCE", "local")]).unwrap();
    let hosted_test = config(&[
        ("PMOS_MODEL_SOURCE", "hosted-test"),
        ("PMOS_GATEWAY_CLOUD_TEST", "1"),
    ])
    .unwrap();
    let production = Models::parse(&read("deploy/gateway.yaml"), false).unwrap();
    assert!(local.check_source(&production).is_ok());
    assert!(hosted_test.check_source(&production).is_err());
    for p in ["deploy/gateway.hosted-test.yaml", "config/gateway.claude-test.yaml"] {
        let m = Models::parse(&read(p), true).unwrap();
        assert!(hosted_test.check_source(&m).is_ok(), "{p}");
        assert!(local.check_source(&m).is_err(), "{p}");
    }
    // The example names no real host: its upstream is under `.invalid`.
    let example = Models::parse(&read("deploy/gateway.hosted-test.yaml"), true).unwrap();
    for r in example.roles.values() {
        assert!(r.upstream_host().ends_with(".invalid"), "{}", r.name);
    }
}
