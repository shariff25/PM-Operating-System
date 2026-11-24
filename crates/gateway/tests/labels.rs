//! A role's `label:`: the gateway reports the label to its clients and in its logs in place of
//! the model id (the roles answer, `x-pmos-model`, an answer's `model`, the usage log), while
//! the real model id is what goes upstream. A role without a label is reported as before.
//! For a labelled role the upstream's own words never reach the caller either: pass-through
//! fields, a broken last streamed line and error bodies (the label hardening of the review
//! at 52dbae2); the roles answer and the usage log carry the hash of the real id.

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use pmos_contract::model_source::ModelSource;
use pmos_gateway::config::{Client, Models};
use pmos_gateway::usage::UsageLog;
use pmos_gateway::{router, GatewayConfig};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "gateway-label-test-token-0123456789";
const REAL: &str = "upstream-real-model-id";
const LABEL: &str = "hosted-model-a";

/// The model ids the mock model server was sent, in order.
#[derive(Default)]
struct Upstream {
    seen: Mutex<Vec<String>>,
}

/// What a provider adds that names it.
const PROVIDER: &str = "ProviderCo";

/// Answers with the model id it was sent, as a real model server does, streamed or not. The
/// ask decides the rest:
/// - `leaky`: the answer also carries `system_fingerprint` and `provider` naming the model and
///   the provider, and, streamed, a comment naming them and an unterminated last line;
/// - `bad`: `400` with an error naming the model and the provider;
/// - `busy`: `429` with `Retry-After: 0` and such an error (retried, then passed on);
/// - `quota`: `429` saying the daily quota is spent (not retried);
/// - `down`: `503` with `Retry-After: 0`.
async fn mock_chat(State(u): State<Arc<Upstream>>, body: Bytes) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap();
    let model = body["model"].as_str().unwrap().to_string();
    u.seen.lock().unwrap().push(model.clone());
    let ask = body["messages"][0]["content"].as_str().unwrap_or_default();
    let named = format!("{model} at {PROVIDER}");
    let failed = |status: u16, kind: &str| {
        let mut res = (
            axum::http::StatusCode::from_u16(status).unwrap(),
            Json(
                json!({"error": {"message": format!("{named}: {kind}"), "type": kind,
                                  "code": kind}}),
            ),
        )
            .into_response();
        res.headers_mut()
            .insert("retry-after", axum::http::HeaderValue::from_static("0"));
        res
    };
    match ask {
        "bad" => return failed(400, "invalid_request_error"),
        "busy" => return failed(429, "rate_limit_exceeded"),
        "quota" => return failed(429, "insufficient_quota"),
        "down" => return failed(503, "overloaded"),
        _ => {}
    }
    if ask == "leaky" {
        if body["stream"] == json!(true) {
            let chunk = |text: &str| {
                json!({"id": "c", "object": "chat.completion.chunk", "model": model,
                       "system_fingerprint": format!("fp-{model}"), "provider": PROVIDER,
                       "choices": [{"index": 0, "delta": {"content": text}}]})
            };
            let sse = format!(
                ": {named}\n\ndata: {}\n\ndata: {}\n\n\
                 data: {{\"id\":\"c\",\"model\":\"{model}\",\"provider\":\"{PROVIDER}",
                chunk("Hello"),
                chunk(" there")
            );
            return ([("content-type", "text/event-stream")], sse).into_response();
        }
        return Json(json!({
            "id": "x", "object": "chat.completion", "created": 1, "model": model,
            "system_fingerprint": format!("fp-{model}"), "provider": PROVIDER,
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": "The answer."}}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5},
        }))
        .into_response();
    }
    if body["stream"] == json!(true) {
        let chunk = |text: &str| {
            json!({"id": "c", "object": "chat.completion.chunk", "model": model,
                   "choices": [{"index": 0, "delta": {"content": text}}]})
        };
        let sse = format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            chunk("Hello"),
            chunk(" there")
        );
        return ([("content-type", "text/event-stream")], sse).into_response();
    }
    Json(json!({
        "id": "x", "object": "chat.completion", "model": model,
        "choices": [{"index": 0, "finish_reason": "stop",
                     "message": {"role": "assistant", "content": "The answer."}}],
        "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5},
    }))
    .into_response()
}

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

struct Setup {
    gw: String,
    upstream: Arc<Upstream>,
    usage: Arc<UsageLog>,
    _stops: (CancellationToken, CancellationToken),
}

/// A gateway whose `default` role has a label and whose `extraction` role hasn't, both in
/// front of one mock model server.
async fn setup() -> Setup {
    let upstream = Arc::new(Upstream::default());
    let app = Router::new()
        .route("/v1/models", get(|| async { Json(json!({"data": []})) }))
        .route("/v1/chat/completions", post(mock_chat))
        .with_state(upstream.clone());
    let (up, stop_up) = serve(app).await;
    let yaml = format!(
        "upstreams: {{a: '{up}/v1'}}\n\
         roles:\n  default: {{upstream: a, model: {REAL}, label: {LABEL}}}\n  \
         extraction: {{upstream: a, model: nemo-test}}\n\
         clients: {{runtime: {{trusted: true}}}}\n"
    );
    let usage = Arc::new(UsageLog::new(None).unwrap());
    let app = router(GatewayConfig {
        models: Models::from_yaml(&yaml).unwrap(),
        clients: vec![Client {
            name: "runtime".into(),
            token: TOKEN.into(),
        }],
        usage: usage.clone(),
        stub: false,
        workarounds_on: true,
        anthropic_key: String::new(),
        remote_key: String::new(),
        retry: pmos_gateway::retry::RetryPolicy {
            base: std::time::Duration::from_millis(1),
            ..Default::default()
        },
        timeouts: Default::default(),
        model_source: ModelSource::Local,
    });
    let (gw, stop_gw) = serve(app).await;
    Setup {
        gw,
        upstream,
        usage,
        _stops: (stop_up, stop_gw),
    }
}

fn chat(s: &Setup, role: &str, stream: bool) -> reqwest::RequestBuilder {
    ask(s, role, stream, "hi")
}

fn ask(s: &Setup, role: &str, stream: bool, text: &str) -> reqwest::RequestBuilder {
    http()
        .post(format!("{}/v1/chat/completions", s.gw))
        .bearer_auth(TOKEN)
        .json(&json!({"model": role, "stream": stream,
                      "messages": [{"role": "user", "content": text}]}))
}

/// Lowercase hex SHA-256, as the gateway reports a labelled role's model id.
fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[tokio::test]
async fn a_labelled_role_is_reported_by_its_label_and_still_sends_the_real_id_upstream() {
    let s = setup().await;

    // The roles answer names the label, never the real id; the unlabelled role is unchanged.
    let v: Value = http()
        .get(format!("{}/v1/pmos/roles", s.gw))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // The labelled role also carries the hash of its real id (so a benchmark can tell a model
    // swapped behind an unchanged label); the unlabelled one has none, its model is its id.
    assert_eq!(
        v["roles"],
        json!([
            {"role": "default", "alias": "pmos/default", "model": LABEL, "upstream": "local",
             "model_sha256": sha256_hex(REAL)},
            {"role": "extraction", "alias": "pmos/extraction", "model": "nemo-test", "upstream": "local"},
        ])
    );
    assert!(!v.to_string().contains(REAL));

    // `/readyz` names neither.
    let ready = reqwest::get(format!("{}/readyz", s.gw))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!ready.contains(REAL) && !ready.contains(LABEL), "{ready}");

    // A whole answer: the real id upstream, the label to the caller.
    let res = chat(&s, "pmos/default", false).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["x-pmos-model"], LABEL);
    let body = res.text().await.unwrap();
    assert!(!body.contains(REAL), "{body}");
    let answer: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(answer["model"], LABEL);
    assert_eq!(answer["choices"][0]["message"]["content"], "The answer.");
    assert_eq!(s.upstream.seen.lock().unwrap().last().unwrap(), REAL);

    // A streamed answer: each event's `model` is the label, the content as it came.
    let res = chat(&s, "pmos/default", true).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["x-pmos-model"], LABEL);
    let body = res.text().await.unwrap();
    assert!(!body.contains(REAL), "{body}");
    let events: Vec<Value> = body
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter(|l| *l != "[DONE]")
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(events.len(), 2, "{body}");
    assert!(events.iter().all(|e| e["model"] == LABEL), "{body}");
    assert_eq!(events[0]["choices"][0]["delta"]["content"], "Hello");
    assert!(body.trim_end().ends_with("data: [DONE]"), "{body}");
    assert_eq!(s.upstream.seen.lock().unwrap().last().unwrap(), REAL);

    // The usage log names the label and the upstream's kind, not the model id or address.
    let lines = s.usage.recent();
    let labelled: Vec<_> = lines.iter().filter(|e| e.role == "default").collect();
    assert_eq!(labelled.len(), 2);
    for e in labelled {
        assert_eq!(e.model, LABEL);
        assert_eq!(e.model_sha256.as_deref(), Some(sha256_hex(REAL).as_str()));
        assert_eq!(e.upstream, "local");
        let line = serde_json::to_string(e).unwrap();
        assert!(line.contains("\"model_sha256\""), "{line}");
        assert!(!line.contains(REAL), "{line}");
    }
}

/// The label hardening (review at 52dbae2): fields the upstream adds that can name the model
/// or the provider (`system_fingerprint`, `provider`), a streamed comment naming them, and an
/// unterminated last line that doesn't parse never reach the caller of a labelled role.
#[tokio::test]
async fn a_labelled_roles_answers_carry_nothing_of_the_upstreams_own() {
    let s = setup().await;
    let res = ask(&s, "pmos/default", false, "leaky")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body = res.text().await.unwrap();
    for f in [REAL, PROVIDER, "system_fingerprint", "provider"] {
        assert!(!body.contains(f), "`{f}` reached the caller: {body}");
    }
    let answer: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(answer["model"], LABEL);
    assert_eq!(answer["choices"][0]["message"]["content"], "The answer.");
    assert_eq!(answer["usage"]["total_tokens"], 5);

    let res = ask(&s, "pmos/default", true, "leaky").send().await.unwrap();
    assert_eq!(res.status(), 200);
    let body = res.text().await.unwrap();
    for f in [REAL, PROVIDER, "system_fingerprint", "provider"] {
        assert!(!body.contains(f), "`{f}` reached the caller: {body}");
    }
    let events: Vec<Value> = body
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(events.len(), 2, "the broken last line is dropped: {body}");
    assert!(events.iter().all(|e| e["model"] == LABEL), "{body}");
    assert_eq!(events[1]["choices"][0]["delta"]["content"], " there");

    // An unlabelled role's answer is passed on as it came, extra fields and all.
    let res = ask(&s, "pmos/extraction", false, "leaky")
        .send()
        .await
        .unwrap();
    let answer: Value = res.json().await.unwrap();
    assert_eq!(answer["provider"], PROVIDER);
    assert_eq!(answer["system_fingerprint"], "fp-nemo-test");
}

/// The spec's point V3, for labelled roles (review at 52dbae2): an upstream's error answer
/// names the model and the provider, so a labelled role's caller gets a neutral one instead,
/// with the status, a coarse type it can act on and `Retry-After`; a role without a label
/// still gets the model server's own error, which helps its caller.
#[tokio::test]
async fn a_labelled_roles_errors_name_nothing_but_keep_what_callers_act_on() {
    let s = setup().await;
    for (text, status, kind, retry_after) in [
        ("bad", 400, "bad_request", false),
        ("busy", 429, "rate_limit", true),
        ("quota", 429, "quota_exhausted", true),
        ("down", 503, "overloaded", true),
    ] {
        for stream in [false, true] {
            let res = ask(&s, "pmos/default", stream, text).send().await.unwrap();
            assert_eq!(res.status().as_u16(), status, "{text}");
            assert_eq!(res.headers()["x-pmos-model"], LABEL);
            assert_eq!(
                res.headers().contains_key("retry-after"),
                retry_after,
                "{text}: Retry-After"
            );
            let body = res.text().await.unwrap();
            for f in [REAL, PROVIDER, "insufficient_quota", "rate_limit_exceeded"] {
                assert!(
                    !body.contains(f),
                    "{text}: `{f}` reached the caller: {body}"
                );
            }
            let v: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(v["error"]["type"], kind, "{text}: {body}");
            assert_eq!(v["error"]["status"], status, "{text}");
        }
    }
    // The gateway still judged the upstream's own words: a spent quota isn't retried, a busy
    // server is, up to its attempts.
    let attempts = |text: &str| {
        let s = &s;
        let text = text.to_string();
        async move {
            let before = s.upstream.seen.lock().unwrap().len();
            let res = ask(s, "pmos/default", false, &text).send().await.unwrap();
            let header = res.headers()["x-pmos-upstream-attempts"]
                .to_str()
                .unwrap()
                .to_string();
            (s.upstream.seen.lock().unwrap().len() - before, header)
        }
    };
    assert_eq!(attempts("quota").await, (1, "1".to_string()));
    assert_eq!(attempts("busy").await, (3, "3".to_string()));
    // Unlabelled: the model server's own error, as it came.
    let res = ask(&s, "pmos/extraction", false, "bad")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
    let v: Value = res.json().await.unwrap();
    assert_eq!(
        v["error"]["message"],
        format!("nemo-test at {PROVIDER}: invalid_request_error")
    );
}

#[tokio::test]
async fn a_role_without_a_label_is_reported_as_before() {
    let s = setup().await;
    let res = chat(&s, "pmos/extraction", false).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["x-pmos-model"], "nemo-test");
    let answer: Value = res.json().await.unwrap();
    assert_eq!(answer["model"], "nemo-test");
    let res = chat(&s, "pmos/extraction", true).send().await.unwrap();
    let body = res.text().await.unwrap();
    assert!(body.contains("\"model\":\"nemo-test\""), "{body}");
    assert_eq!(s.upstream.seen.lock().unwrap().last().unwrap(), "nemo-test");
    let e = s.usage.recent().pop().unwrap();
    assert_eq!(e.model, "nemo-test");
    assert_eq!(e.upstream, "127.0.0.1");
}

/// A client that names the model source the gateways here serve (`local`; X4, slice H4).
fn http() -> reqwest::Client {
    http_as("local")
}

/// A client that names `source` on every call.
fn http_as(source: &str) -> reqwest::Client {
    let mut h = reqwest::header::HeaderMap::new();
    h.insert(
        "x-pmos-model-source",
        reqwest::header::HeaderValue::from_str(source).unwrap(),
    );
    // Under `local` and `hosted` every model call names its task (slice H12); a call that
    // names its own run keeps it.
    h.insert(
        "x-pmos-run",
        reqwest::header::HeaderValue::from_static("test-run"),
    );
    reqwest::Client::builder()
        .default_headers(h)
        .build()
        .unwrap()
}
