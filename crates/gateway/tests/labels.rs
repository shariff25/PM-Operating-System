//! A role's `label:`: the gateway reports the label to its clients and in its logs in place of
//! the model id (the roles answer, `x-pmos-model`, an answer's `model`, the usage log), while
//! the real model id is what goes upstream. A role without a label is reported as before.

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
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

/// Answers with the model id it was sent, as a real model server does, streamed or not.
async fn mock_chat(State(u): State<Arc<Upstream>>, body: Bytes) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap();
    let model = body["model"].as_str().unwrap().to_string();
    u.seen.lock().unwrap().push(model.clone());
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
        retry: Default::default(),
        timeouts: Default::default(),
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
    reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", s.gw))
        .bearer_auth(TOKEN)
        .json(&json!({"model": role, "stream": stream,
                      "messages": [{"role": "user", "content": "hi"}]}))
}

#[tokio::test]
async fn a_labelled_role_is_reported_by_its_label_and_still_sends_the_real_id_upstream() {
    let s = setup().await;

    // The roles answer names the label, never the real id; the unlabelled role is unchanged.
    let v: Value = reqwest::Client::new()
        .get(format!("{}/v1/pmos/roles", s.gw))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        v["roles"],
        json!([
            {"role": "default", "alias": "pmos/default", "model": LABEL, "upstream": "local"},
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
        assert_eq!(e.upstream, "local");
    }
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
