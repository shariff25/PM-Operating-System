//! Slice MP3 (`docs/model-providers-spec.md` §4.3 to §4.5, §4.9): a routed role's call moves to
//! its next hop only on the upstream's side's failures, never after its first content, never on
//! a client's own error or a refusal, within its attempts and budget, sending each upstream
//! only its own key and fields, and naming no upstream's words anywhere.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use pmos_contract::model_source::ModelSource;
use pmos_gateway::config::{sha256_hex, Client, Models};
use pmos_gateway::retry::RetryPolicy;
use pmos_gateway::usage::UsageLog;
use pmos_gateway::{router_with, GatewayConfig, RouterOptions};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

pub const TOKEN: &str = "fallback-test-token-0123456789abcdef";
pub const EVALS: &str = "fallback-evals-token-0123456789abcdef";
pub const ADMIN: &str = "fallback-admin-token-0123456789abcdef";
/// What every mock's error body carries: no answer, header or usage line may.
pub const MARKER: &str = "MARKER-ProviderCo-secret-4711";

/// What one call to a mock gets.
#[derive(Clone)]
pub enum Step {
    /// A whole answer (or, for `stream: true`, a stream) with this content.
    Answer(&'static str),
    /// A status with a `Retry-After` (if any) and a body carrying [`MARKER`].
    Status(u16, Option<&'static str>, &'static str),
    /// A `200` whole answer that isn't JSON.
    NotJson,
    /// A `200` whole answer with `finish_reason: content_filter` (a refusal).
    Refusal,
    /// A stream that ends before any content.
    EndsEarly,
    /// A stream that sends content, then breaks off.
    BreaksAfterContent,
    /// A whole answer with a tool call carrying `extra_content`.
    ToolCall,
}

#[derive(Default)]
pub struct Mock {
    steps: Mutex<VecDeque<Step>>,
    /// Used when `steps` is empty.
    default: Mutex<Option<Step>>,
    pub calls: Mutex<u32>,
    pub auth: Mutex<Vec<String>>,
    pub bodies: Mutex<Vec<Value>>,
    pub probes: Mutex<u32>,
}

impl Mock {
    pub fn script(&self, steps: Vec<Step>) {
        *self.steps.lock().unwrap() = steps.into();
    }
    pub fn always(&self, step: Step) {
        *self.default.lock().unwrap() = Some(step);
    }
    pub fn calls(&self) -> u32 {
        *self.calls.lock().unwrap()
    }
}

fn sse(events: &[Value]) -> String {
    let mut s = String::new();
    for e in events {
        s.push_str(&format!("data: {e}\n\n"));
    }
    s
}

async fn mock_chat(State(m): State<Arc<Mock>>, headers: HeaderMap, body: Bytes) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap();
    *m.calls.lock().unwrap() += 1;
    m.auth.lock().unwrap().push(
        headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string(),
    );
    m.bodies.lock().unwrap().push(body.clone());
    let step = m
        .steps
        .lock()
        .unwrap()
        .pop_front()
        .or_else(|| m.default.lock().unwrap().clone())
        .unwrap_or(Step::Answer("ok"));
    let stream = body["stream"] == json!(true);
    let model = body["model"].clone();
    match step {
        Step::Answer(text) if stream => (
            [("content-type", "text/event-stream")],
            format!(
                "{}{}data: [DONE]\n\n",
                sse(&[
                    json!({"model": model, "choices": [{"index": 0, "delta": {"role": "assistant"}}]}),
                    json!({"model": model, "choices": [{"index": 0, "delta": {"content": text}}]}),
                ]),
                sse(&[json!({"model": model, "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                             "usage": {"prompt_tokens": 3, "completion_tokens": 1}})])
            ),
        )
            .into_response(),
        Step::Answer(text) => Json(json!({
            "id": "x", "object": "chat.completion", "model": model, "system_fingerprint": MARKER,
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": text}}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 1},
        }))
        .into_response(),
        Step::Refusal => Json(json!({
            "id": "x", "object": "chat.completion", "model": model,
            "choices": [{"index": 0, "finish_reason": "content_filter",
                         "message": {"role": "assistant", "content": ""}}],
        }))
        .into_response(),
        Step::ToolCall => Json(json!({
            "id": "x", "object": "chat.completion", "model": model,
            "choices": [{"index": 0, "finish_reason": "tool_calls",
                         "message": {"role": "assistant", "content": null, "tool_calls": [
                             {"id": "call-1", "type": "function",
                              "function": {"name": "search", "arguments": "{}"},
                              "extra_content": {"opaque": MARKER}}]}}],
        }))
        .into_response(),
        Step::Status(code, retry_after, text) => {
            let mut res = (
                StatusCode::from_u16(code).unwrap(),
                Json(json!({"error": {"message": format!("{text} {MARKER}")}})),
            )
                .into_response();
            if let Some(ra) = retry_after {
                res.headers_mut()
                    .insert("retry-after", ra.parse().unwrap());
            }
            if (300..400).contains(&code) {
                res.headers_mut()
                    .insert("location", format!("https://{MARKER}.invalid/").parse().unwrap());
            }
            res
        }
        Step::NotJson => (StatusCode::OK, format!("<html>{MARKER}</html>")).into_response(),
        Step::EndsEarly => (
            [("content-type", "text/event-stream")],
            sse(&[json!({"model": model, "choices": [{"index": 0, "delta": {"role": "assistant"}}]})]),
        )
            .into_response(),
        Step::BreaksAfterContent => {
            let first = sse(&[
                json!({"model": model, "choices": [{"index": 0, "delta": {"content": "partial-from-a"}}]}),
            ]);
            // The content goes out first; the break comes a moment later.
            let s = futures_util::stream::unfold(0u8, move |n| {
                let first = first.clone();
                async move {
                    match n {
                        0 => Some((Ok::<Bytes, std::io::Error>(Bytes::from(first)), 1)),
                        1 => {
                            tokio::time::sleep(Duration::from_millis(200)).await;
                            Some((Err(std::io::Error::other("cut")), 2))
                        }
                        _ => None,
                    }
                }
            });
            (
                [("content-type", "text/event-stream")],
                Body::from_stream(s),
            )
                .into_response()
        }
    }
}

pub async fn serve(app: Router) -> (String, CancellationToken) {
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

pub async fn start_mock() -> (String, Arc<Mock>) {
    let m = Arc::new(Mock::default());
    let app = Router::new()
        .route("/v1/chat/completions", post(mock_chat))
        .route(
            "/v1/models",
            get(|State(m): State<Arc<Mock>>| async move {
                *m.probes.lock().unwrap() += 1;
                Json(json!({"data": []}))
            }),
        )
        .with_state(m.clone());
    let (base, _stop) = serve(app).await;
    (format!("{base}/v1"), m)
}

pub struct Gw {
    pub base: String,
    pub usage: Arc<UsageLog>,
    pub mocks: Vec<Arc<Mock>>,
    pub dir: std::path::PathBuf,
    pub options: RouterOptions,
}

/// A hosted-test gateway whose `default` role routes over one mock per entry of `features`
/// (`hosted-model-a`, `-b`, …, each on its own upstream with its own key file), with `extra`
/// appended to the config (`routing:`, `clients:`).
pub async fn gateway(features: &[&str], extra: &str) -> Gw {
    gateway_with(features, extra, RouterOptions::default()).await
}

pub async fn gateway_with(features: &[&str], extra: &str, mut options: RouterOptions) -> Gw {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "pmos-fallback-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let mut upstreams = String::new();
    let mut models = String::new();
    let mut route = Vec::new();
    let mut mocks = Vec::new();
    for (i, f) in features.iter().enumerate() {
        let (mut url, m) = start_mock().await;
        // `closed;<features>`: an upstream nothing listens on.
        let f = match f.strip_prefix("closed;") {
            Some(rest) => {
                let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                url = format!("http://{}/v1", l.local_addr().unwrap());
                drop(l);
                rest
            }
            None => f,
        };
        let letter = (b'a' + i as u8) as char;
        let key = dir.join(format!("upstream-{letter}.key"));
        std::fs::write(&key, format!("key-{letter}-0123\n")).unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        upstreams.push_str(&format!(
            "  up-{letter}: {{url: 'openai+{url}', key-file: '{}'}}\n",
            key.display()
        ));
        models.push_str(&format!(
            "  hosted-model-{letter}: {{upstream: up-{letter}, model: 'id-{letter}', features: [{f}]}}\n"
        ));
        route.push(format!("hosted-model-{letter}"));
        mocks.push(m);
    }
    let yaml = format!(
        "class: test\nupstreams:\n{upstreams}models:\n{models}roles:\n  default: {{route: [{}]}}\n{extra}",
        route.join(", ")
    );
    let parsed = Models::parse(&yaml, true).unwrap();
    let usage = Arc::new(UsageLog::new(None).unwrap());
    options
        .route_control
        .get_or_insert_with(|| dir.join("route-control"));
    let (base, _stop) = serve(router_with(
        GatewayConfig {
            models: parsed,
            clients: vec![
                Client {
                    name: "runtime".into(),
                    token: TOKEN.into(),
                },
                Client {
                    name: "evals".into(),
                    token: EVALS.into(),
                },
                Client {
                    name: "admin".into(),
                    token: ADMIN.into(),
                },
            ],
            usage: usage.clone(),
            stub: false,
            workarounds_on: true,
            anthropic_key: String::new(),
            remote_key: String::new(),
            retry: RetryPolicy {
                max_attempts: 3,
                budget: Duration::from_secs(20),
                base: Duration::from_millis(10),
            },
            timeouts: pmos_gateway::Timeouts {
                ready_cache: Duration::ZERO,
                ..Default::default()
            },
            model_source: ModelSource::HostedTest,
        },
        options.clone(),
    ))
    .await;
    Gw {
        base,
        usage,
        mocks,
        dir,
        options,
    }
}

pub fn ask(gw: &Gw, body: Value) -> reqwest::RequestBuilder {
    ask_as(gw, TOKEN, body)
}

pub fn ask_as(gw: &Gw, token: &str, body: Value) -> reqwest::RequestBuilder {
    reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", gw.base))
        .bearer_auth(token)
        .header("x-pmos-model-source", "hosted-test")
        .json(&body)
}

pub fn hello(stream: bool) -> Value {
    json!({"model": "pmos/default", "stream": stream,
           "messages": [{"role": "user", "content": "hello"}]})
}

const FULL: &str = "tools, stream, json";

/// Everything that left the gateway for the caller or its logs, for the leak checks.
async fn everything(res: reqwest::Response, gw: &Gw) -> (u16, HeaderMap, String, String) {
    let status = res.status().as_u16();
    let headers = res.headers().clone();
    let body = res.text().await.unwrap_or_default();
    let usage = serde_json::to_string(&gw.usage.recent()).unwrap();
    (status, headers, body, usage)
}

fn header(h: &HeaderMap, k: &str) -> String {
    h.get(k)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

#[tokio::test]
async fn an_overloaded_primary_falls_back_and_the_answer_says_which_model_answered() {
    let gw = gateway(&[FULL, FULL], "").await;
    gw.mocks[0].always(Step::Status(503, None, "overloaded"));
    let res = ask(&gw, hello(false)).send().await.unwrap();
    let (status, h, body, usage) = everything(res, &gw).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(header(&h, "x-pmos-model"), "hosted-model-b");
    assert_eq!(header(&h, "x-pmos-model-sha256"), sha256_hex("id-b"));
    assert_eq!(header(&h, "x-pmos-route-hop"), "2");
    assert_eq!(header(&h, "x-pmos-fallback"), "1");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["model"], "hosted-model-b");
    // Same-hop attempts (2) on the primary, then one on the fallback.
    assert_eq!((gw.mocks[0].calls(), gw.mocks[1].calls()), (2, 1));
    // Each upstream saw only its own key.
    assert!(gw.mocks[0]
        .auth
        .lock()
        .unwrap()
        .iter()
        .all(|a| a == "Bearer key-a-0123"));
    assert_eq!(*gw.mocks[1].auth.lock().unwrap(), vec!["Bearer key-b-0123"]);
    // One usage line per attempt: two failed ones on hop 1, the answer on hop 2.
    let lines = gw.usage.recent();
    assert_eq!(lines.len(), 3, "{usage}");
    assert_eq!(lines[0].reason, Some("overloaded"));
    assert!(lines[0].retrying && !lines[0].fallback);
    assert_eq!(
        (lines[2].hop, lines[2].fallback, lines[2].status),
        (Some(2), true, 200)
    );
    assert_eq!(lines[2].model, "hosted-model-b");
    assert_eq!(
        lines[2].model_sha256.as_deref(),
        Some(sha256_hex("id-b").as_str())
    );
    assert!(lines[2].upstream_sha256.is_some());
    for secret in [MARKER, "id-a", "id-b", "127.0.0.1", "up-a", "up-b", "key-a"] {
        assert!(
            !body.contains(secret) && !usage.contains(secret),
            "{secret}"
        );
        assert!(
            h.values()
                .all(|v| !v.to_str().unwrap_or("").contains(secret)),
            "{secret}"
        );
    }
}

#[tokio::test]
async fn every_upstream_failure_of_the_table_moves_to_the_next_hop() {
    // (step on hop 1, attempts hop 1 gets with same-hop-attempts 2)
    let cases: Vec<(Step, u32)> = vec![
        (Step::Status(429, None, "slow down"), 2),
        (Step::Status(429, Some("1"), "slow down"), 2),
        (Step::Status(429, Some("120"), "slow down"), 1),
        (Step::Status(429, None, "insufficient_quota"), 1),
        (Step::Status(503, None, "busy"), 2),
        (Step::Status(529, Some("120"), "busy"), 1),
        (Step::Status(500, None, "oops"), 1),
        (Step::Status(502, None, "oops"), 1),
        (Step::Status(504, None, "oops"), 1),
        (Step::Status(404, None, "no such model"), 1),
        (
            Step::Status(400, None, "The model `id-a` does not exist"),
            1,
        ),
        (Step::Status(422, None, "model_not_found"), 1),
        (Step::Status(401, None, "bad key"), 1),
        (Step::Status(403, None, "bad key"), 1),
        (Step::Status(307, None, "elsewhere"), 1),
        (Step::NotJson, 1),
    ];
    for (step, want) in cases {
        let gw = gateway(&[FULL, FULL], "").await;
        gw.mocks[0].always(step.clone());
        let res = ask(&gw, hello(false)).send().await.unwrap();
        let (status, h, body, usage) = everything(res, &gw).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(header(&h, "x-pmos-fallback"), "1", "{body}");
        assert_eq!(gw.mocks[0].calls(), want, "{usage}");
        assert_eq!(gw.mocks[1].calls(), 1);
        assert!(!body.contains(MARKER) && !usage.contains(MARKER), "{usage}");
        assert!(h.get("location").is_none());
    }
    // A model server that can't be reached at all: retried once (same-hop), then the next.
    let gw = gateway(&["closed;tools, stream, json", FULL], "").await;
    let res = ask(&gw, hello(false)).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(header(res.headers(), "x-pmos-model"), "hosted-model-b");
    let lines = gw.usage.recent();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].reason, Some("connect"));
}

#[tokio::test]
async fn a_clients_bad_request_and_a_refusal_never_fall_back() {
    for (step, want) in [
        (
            Step::Status(400, None, "maximum context length exceeded"),
            400,
        ),
        (Step::Status(413, None, "too large"), 413),
        (Step::Status(422, None, "invalid schema"), 422),
        (Step::Refusal, 200),
    ] {
        let gw = gateway(&[FULL, FULL], "").await;
        gw.mocks[0].always(step);
        let res = ask(&gw, hello(false)).send().await.unwrap();
        let (status, h, body, usage) = everything(res, &gw).await;
        assert_eq!(status, want, "{body}");
        assert_eq!(header(&h, "x-pmos-model"), "hosted-model-a");
        assert_eq!(header(&h, "x-pmos-fallback"), "0");
        assert_eq!(gw.mocks[1].calls(), 0, "never routed to another model");
        assert!(!body.contains(MARKER) && !usage.contains(MARKER), "{body}");
        if want == 200 {
            let v: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(v["choices"][0]["finish_reason"], "content_filter");
        } else {
            let v: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(v["error"]["type"], "bad_request");
        }
    }
    // A hundred malformed requests (the gateway's own refusal) send nothing anywhere.
    let gw = gateway(&[FULL, FULL], "").await;
    for _ in 0..100 {
        let res = ask(&gw, json!({"model": "pmos/nope", "messages": []}))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 400);
    }
    assert_eq!((gw.mocks[0].calls(), gw.mocks[1].calls()), (0, 0));
}

#[tokio::test]
async fn a_stream_may_fall_back_before_its_first_content_and_never_after() {
    // Before: hop 1's stream ends with no content; hop 2 answers, and only hop 2's words
    // reach the caller.
    let gw = gateway(&[FULL, FULL], "").await;
    gw.mocks[0].always(Step::EndsEarly);
    gw.mocks[1].always(Step::Answer("from-b"));
    let res = ask(&gw, hello(true)).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(header(res.headers(), "x-pmos-model"), "hosted-model-b");
    let text = res.text().await.unwrap();
    assert!(text.contains("from-b") && text.contains("[DONE]"), "{text}");
    assert!(!text.contains("id-a") && !text.contains("id-b"), "{text}");
    assert_eq!(gw.mocks[0].calls(), 1);
    let line = gw.usage.recent()[0].clone();
    assert_eq!(line.reason, Some("first_token"));

    // After: hop 1 has sent content and breaks off; the caller's stream ends in an error, and
    // hop 2 is sent nothing: two models' output is never joined.
    let gw = gateway(&[FULL, FULL], "").await;
    gw.mocks[0].always(Step::BreaksAfterContent);
    let res = ask(&gw, hello(true)).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(header(res.headers(), "x-pmos-model"), "hosted-model-a");
    // The caller's stream ends in an error, never a clean end that looks whole.
    assert!(
        res.bytes().await.is_err(),
        "a broken stream never looks whole"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(gw.mocks[1].calls(), 0, "never continued by another model");
    assert_eq!(gw.mocks[0].calls(), 1, "never replayed");
}

#[tokio::test]
async fn three_failing_hops_make_at_most_two_one_and_one_attempts_and_name_nothing() {
    let gw = gateway(&[FULL, FULL, FULL], "").await;
    gw.mocks[0].always(Step::Status(503, None, "busy"));
    gw.mocks[1].always(Step::Status(500, None, "oops"));
    gw.mocks[2].always(Step::Status(401, None, "bad key"));
    let res = ask(&gw, hello(false)).send().await.unwrap();
    let (status, h, body, usage) = everything(res, &gw).await;
    assert_eq!(status, 503);
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["error"]["type"], "no_model_available");
    assert!(header(&h, "retry-after").parse::<u64>().unwrap() >= 1);
    assert_eq!(
        (
            gw.mocks[0].calls(),
            gw.mocks[1].calls(),
            gw.mocks[2].calls()
        ),
        (2, 1, 1)
    );
    for secret in [MARKER, "hosted-model", "id-", "127.0.0.1"] {
        assert!(!body.contains(secret), "{secret}: {body}");
    }
    assert!(!usage.contains(MARKER));
    // The budget bounds the whole call: with a 1-second budget, a 2-second Retry-After on
    // hop 1 is not waited for.
    let gw = gateway(
        &[FULL, FULL],
        "routing: {budget-secs: 1, short-retry-after-secs: 5}\n",
    )
    .await;
    gw.mocks[0].always(Step::Status(429, Some("2"), "slow"));
    let started = std::time::Instant::now();
    let res = ask(&gw, hello(false)).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(gw.mocks[0].calls(), 1);
}

#[tokio::test]
async fn a_hop_without_a_feature_the_request_uses_is_never_sent_it() {
    let gw = gateway(&["stream", FULL], "").await;
    let res = ask(
        &gw,
        json!({"model": "pmos/default", "messages": [{"role": "user", "content": "hi"}],
               "tools": [{"type": "function", "function": {"name": "search", "parameters": {}}}]}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(header(res.headers(), "x-pmos-model"), "hosted-model-b");
    assert_eq!(gw.mocks[0].calls(), 0);
    let line = gw.usage.recent().last().unwrap().clone();
    assert_eq!(line.skipped.len(), 1);
    assert_eq!(line.skipped[0].label, "hosted-model-a");
    assert_eq!(line.skipped[0].why, "feature");
    // Neither hop serves embeddings: nothing is sent.
    let res = reqwest::Client::new()
        .post(format!("{}/v1/embeddings", gw.base))
        .bearer_auth(TOKEN)
        .header("x-pmos-model-source", "hosted-test")
        .json(&json!({"model": "pmos/default", "input": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 503);
    assert_eq!((gw.mocks[0].calls(), gw.mocks[1].calls()), (0, 1));
}

#[tokio::test]
async fn one_upstreams_tool_call_extras_never_reach_another() {
    let gw = gateway(&[FULL, FULL], "").await;
    gw.mocks[0].script(vec![Step::ToolCall, Step::Status(500, None, "oops")]);
    let first = ask(&gw, hello(false)).send().await.unwrap();
    assert_eq!(first.status(), 200);
    // The history with hop 1's tool call (its extra_content dropped, as the harness drops
    // it) goes to hop 1, which fails, then to hop 2, which must not get hop 1's extras.
    let history = json!({"model": "pmos/default", "messages": [
        {"role": "user", "content": "hi"},
        {"role": "assistant", "content": null, "tool_calls": [
            {"id": "call-1", "type": "function", "function": {"name": "search", "arguments": "{}"}}]},
        {"role": "tool", "tool_call_id": "call-1", "content": "result"}]});
    let res = ask(&gw, history).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(header(res.headers(), "x-pmos-model"), "hosted-model-b");
    let to_a = gw.mocks[0].bodies.lock().unwrap()[1].to_string();
    assert!(
        to_a.contains(MARKER),
        "hop 1 gets its own extras back: {to_a}"
    );
    let to_b = gw.mocks[1].bodies.lock().unwrap()[0].to_string();
    assert!(
        !to_b.contains(MARKER),
        "hop 2 never sees hop 1's extras: {to_b}"
    );
    assert!(!to_b.contains("extra_content"));
    // Hop 2 got the history's tool-call ids as the caller sent them (whether a real
    // provider accepts another's ids is checked against real test upstreams in MP8).
    assert!(to_b.contains("call-1"));
}

// Slice MP4 (§4.6, §4.12): circuits per model and per upstream, `/readyz` by counts only,
// `GET /v1/pmos/routes` for the admin client alone.

fn routes(gw: &Gw, token: &str) -> reqwest::RequestBuilder {
    reqwest::Client::new()
        .get(format!("{}/v1/pmos/routes", gw.base))
        .bearer_auth(token)
}

#[tokio::test]
async fn a_circuit_opens_skips_its_model_and_lets_one_real_call_through_after_its_cool_down() {
    let gw = gateway(
        &[FULL, FULL],
        "routing: {circuit: {failures: 1, cool-down-secs: 1, max-cool-down-secs: 2}}\n",
    )
    .await;
    gw.mocks[0].script(vec![Step::Status(500, None, "oops")]);
    let res = ask(&gw, hello(false)).send().await.unwrap();
    assert_eq!(header(res.headers(), "x-pmos-model"), "hosted-model-b");
    assert_eq!(gw.mocks[0].calls(), 1);
    // Open: the primary is skipped, and the line says why.
    let res = ask(&gw, hello(false)).send().await.unwrap();
    assert_eq!(header(res.headers(), "x-pmos-model"), "hosted-model-b");
    assert_eq!(gw.mocks[0].calls(), 1, "an open circuit sends nothing");
    let line = gw.usage.recent().last().unwrap().clone();
    assert_eq!(line.skipped[0].why, "circuit");
    let v: Value = routes(&gw, ADMIN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["roles"][0]["hops"][0]["circuit"], "open", "{v}");
    assert_eq!(v["roles"][0]["hops"][1]["circuit"], "closed");
    // No synthetic traffic: while it cools down, the gateway sends the model nothing.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(gw.mocks[0].calls(), 1);
    // Half-open: the next real call tries it, succeeds, and closes it.
    let res = ask(&gw, hello(false)).send().await.unwrap();
    assert_eq!(header(res.headers(), "x-pmos-model"), "hosted-model-a");
    assert_eq!(header(res.headers(), "x-pmos-fallback"), "0");
    let v: Value = routes(&gw, ADMIN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["roles"][0]["hops"][0]["circuit"], "closed");
}

#[tokio::test]
async fn a_callers_own_errors_never_trip_a_circuit() {
    let gw = gateway(&[FULL, FULL], "routing: {circuit: {failures: 1}}\n").await;
    gw.mocks[0].script(vec![Step::Status(400, None, "bad request"); 100]);
    for _ in 0..100 {
        let res = ask(&gw, hello(false)).send().await.unwrap();
        assert_eq!(res.status(), 400);
    }
    assert_eq!(gw.mocks[1].calls(), 0);
    let res = ask(&gw, hello(false)).send().await.unwrap();
    assert_eq!(header(res.headers(), "x-pmos-model"), "hosted-model-a");
    let v: Value = routes(&gw, ADMIN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    for hop in v["roles"][0]["hops"].as_array().unwrap() {
        assert_eq!(hop["circuit"], "closed", "{v}");
    }
}

#[tokio::test]
async fn readiness_counts_usable_hops_and_names_no_label() {
    let gw = gateway(
        &[FULL, FULL],
        "routing: {circuit: {failures: 1, cool-down-secs: 60}}\n",
    )
    .await;
    let ready = || async {
        let res = reqwest::get(format!("{}/readyz", gw.base)).await.unwrap();
        let status = res.status().as_u16();
        let text = res.text().await.unwrap();
        for f in ["hosted-model", "id-a", "id-b", "127.0.0.1", "up-a", "key-"] {
            assert!(!text.contains(f), "/readyz carries `{f}`: {text}");
        }
        (status, serde_json::from_str::<Value>(&text).unwrap())
    };
    let (status, v) = ready().await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["upstreams"]["default"]["hops"], 2);
    assert_eq!(v["upstreams"]["default"]["hops_usable"], 2);
    // The primary's circuit opens: still ready, on the fallback.
    gw.mocks[0].script(vec![Step::Status(500, None, "x")]);
    ask(&gw, hello(false)).send().await.unwrap();
    let (status, v) = ready().await;
    assert_eq!(
        (status, &v["upstreams"]["default"]["hops_usable"]),
        (200, &json!(1))
    );
    assert_eq!(v["upstreams"]["default"]["status"], "ok");
    // Hop 1 is skipped (open), hop 2 fails and opens: down.
    gw.mocks[1].script(vec![Step::Status(500, None, "x")]);
    assert_eq!(ask(&gw, hello(false)).send().await.unwrap().status(), 503);
    let (status, v) = ready().await;
    assert_eq!(status, 503);
    assert_eq!(v["upstreams"]["default"]["status"], "down");
    // The 503 says when to come back: the soonest cool-down's end.
    let res = ask(&gw, hello(false)).send().await.unwrap();
    assert_eq!(res.status(), 503);
    let ra: u64 = header(res.headers(), "retry-after").parse().unwrap();
    assert!((50..=60).contains(&ra), "{ra}");
}

#[tokio::test]
async fn only_the_admin_client_reads_the_routes_and_it_makes_no_model_call() {
    let gw = gateway(&[FULL, FULL], "").await;
    assert_eq!(routes(&gw, TOKEN).send().await.unwrap().status(), 403);
    assert_eq!(routes(&gw, EVALS).send().await.unwrap().status(), 403);
    let v: Value = routes(&gw, ADMIN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["roles"][0]["hops"][1]["label"], "hosted-model-b");
    assert_eq!(v["roles"][0]["hops"][1]["model_sha256"], sha256_hex("id-b"));
    let text = v.to_string();
    for f in ["id-a", "127.0.0.1", "up-a", "key-"] {
        assert!(!text.contains(f), "{text}");
    }
    let res = ask_as(&gw, ADMIN, hello(false)).send().await.unwrap();
    assert_eq!(res.status(), 403);
    assert_eq!(gw.mocks[0].calls() + gw.mocks[1].calls(), 0);
    // The roles answer lists the route by label, the primary as the role's model.
    let v: Value = reqwest::Client::new()
        .get(format!("{}/v1/pmos/roles", gw.base))
        .bearer_auth(EVALS)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["roles"][0]["model"], "hosted-model-a");
    assert_eq!(v["roles"][0]["route"][1]["label"], "hosted-model-b");
    assert!(!v.to_string().contains("id-b"));
}
