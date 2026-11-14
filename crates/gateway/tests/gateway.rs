//! The gateway against a mock vLLM server: auth, role routing, workarounds, streaming,
//! fair queueing, failures, and the stub model.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use pmos_gateway::config::{Client, Limits, Models};
use pmos_gateway::scheduler::Priority;
use pmos_gateway::usage::UsageLog;
use pmos_gateway::{router, GatewayConfig};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "gateway-test-token-0123456789abcdef";

/// What the mock model server saw, in arrival order: (model, last user message).
#[derive(Default)]
struct Vllm {
    seen: Mutex<Vec<(String, String)>>,
}

fn last_user(body: &Value) -> String {
    body["messages"]
        .as_array()
        .and_then(|m| m.last())
        .and_then(|m| m["content"].as_str())
        .unwrap_or_default()
        .to_string()
}

async fn mock_chat(State(v): State<Arc<Vllm>>, body: Bytes) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap();
    let asked = last_user(&body);
    v.seen
        .lock()
        .unwrap()
        .push((body["model"].as_str().unwrap().to_string(), asked.clone()));
    if asked.starts_with("slow") {
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    if asked == "too long" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"message": "maximum context length exceeded"}})),
        )
            .into_response();
    }
    if body["stream"] == json!(true) {
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n\
                   data: {\"choices\":[{\"delta\":{\"content\":\" there\"}}]}\n\n\
                   data: [DONE]\n\n";
        return ([("content-type", "text/event-stream")], sse).into_response();
    }
    Json(json!({
        "id": "x", "object": "chat.completion", "model": body["model"],
        "choices": [{"index": 0, "finish_reason": "stop", "message": {"role": "assistant",
            "content": "<think>\nreasoning here\n</think>\n\nThe answer."}}],
        "usage": {"prompt_tokens": 12, "completion_tokens": 3, "total_tokens": 15},
    }))
    .into_response()
}

async fn mock_embeddings(State(v): State<Arc<Vllm>>, body: Bytes) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap();
    v.seen
        .lock()
        .unwrap()
        .push((body["model"].as_str().unwrap().to_string(), "embed".into()));
    Json(
        json!({"object": "list", "data": [{"index": 0, "embedding": [0.1, 0.2]}],
                "usage": {"prompt_tokens": 2}}),
    )
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

async fn start_vllm() -> (String, Arc<Vllm>, CancellationToken) {
    let v = Arc::new(Vllm::default());
    let app = Router::new()
        .route("/v1/models", get(|| async { Json(json!({"data": []})) }))
        .route("/v1/chat/completions", post(mock_chat))
        .route("/v1/embeddings", post(mock_embeddings))
        .with_state(v.clone());
    let (base, stop) = serve(app).await;
    (format!("{base}/v1"), v, stop)
}

struct Gw {
    base: String,
    usage: Arc<UsageLog>,
    stop: CancellationToken,
}

async fn start_gateway(upstream: &str, limits: Limits, stub: bool, workarounds_on: bool) -> Gw {
    let yaml = format!(
        "upstreams: {{a: '{upstream}'}}\n\
         roles:\n  default: {{upstream: a, model: qwen-test, workarounds: [strip-think]}}\n  \
         extraction: {{upstream: a, model: nemo-test}}\n  embedding: {{upstream: a, model: embed-test}}\n\
         limits: {{max-in-flight: {}, per-user: {}, max-queue: {}}}\n",
        limits.max_in_flight, limits.per_user, limits.max_queue
    );
    let models = Models::from_yaml(&yaml).unwrap();
    let usage = Arc::new(UsageLog::new(None).unwrap());
    let app = router(GatewayConfig {
        models,
        clients: vec![Client {
            name: "runtime".into(),
            token: TOKEN.into(),
        }],
        usage: usage.clone(),
        stub,
        workarounds_on,
    });
    let (base, stop) = serve(app).await;
    Gw { base, usage, stop }
}

fn wide() -> Limits {
    Limits {
        max_in_flight: 8,
        per_user: 8,
        max_queue: 16,
    }
}

fn chat(gw: &Gw, model: &str, text: &str) -> reqwest::RequestBuilder {
    reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", gw.base))
        .bearer_auth(TOKEN)
        .json(&json!({"model": model, "messages": [{"role": "user", "content": text}]}))
}

#[tokio::test]
async fn roles_route_to_their_local_model_and_nothing_else_gets_through() {
    let (up, vllm, stop_up) = start_vllm().await;
    let gw = start_gateway(&up, wide(), false, true).await;
    let http = reqwest::Client::new();

    // No token, or a wrong one.
    let res = http
        .post(format!("{}/v1/chat/completions", gw.base))
        .json(&json!({"model": "pmos/default", "messages": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    let res = http
        .get(format!("{}/v1/models", gw.base))
        .bearer_auth("wrong-token-0123456789abcdef-012345")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);

    // The model list is the role aliases, not the models behind them.
    let list: Value = http
        .get(format!("{}/v1/models", gw.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids: Vec<&str> = list["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec!["pmos/default", "pmos/embedding", "pmos/extraction"]
    );

    // A role alias reaches its model; the answer says which model served it.
    let res = chat(&gw, "pmos/extraction", "hi")
        .header("x-pmos-user", "ana")
        .header("x-pmos-run", "run-1")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["x-pmos-model"], "nemo-test");
    assert_eq!(vllm.seen.lock().unwrap().last().unwrap().0, "nemo-test");

    // A concrete model name or an unknown role never reaches a model server.
    let before = vllm.seen.lock().unwrap().len();
    for model in ["qwen-test", "pmos/nope", "gpt-4o"] {
        let res = chat(&gw, model, "hi").send().await.unwrap();
        assert_eq!(res.status(), 400, "{model}");
        let body: Value = res.json().await.unwrap();
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("pmos/default"));
    }
    assert_eq!(vllm.seen.lock().unwrap().len(), before);

    // Bad caller headers are refused before queueing.
    let res = chat(&gw, "pmos/default", "hi")
        .header("x-pmos-user", "Not A Handle")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
    let res = chat(&gw, "pmos/default", "hi")
        .header("x-pmos-priority", "urgent")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    // Embeddings go through the same way.
    let res = http
        .post(format!("{}/v1/embeddings", gw.base))
        .bearer_auth(TOKEN)
        .json(&json!({"model": "pmos/embedding", "input": "SAML"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(vllm.seen.lock().unwrap().last().unwrap().0, "embed-test");

    // The usage log names the caller, role, model and tokens; never the text.
    let ev = gw
        .usage
        .recent()
        .into_iter()
        .find(|e| e.run.as_deref() == Some("run-1"))
        .unwrap();
    assert_eq!(ev.client, "runtime");
    assert_eq!(ev.user.as_deref(), Some("ana"));
    assert_eq!(
        (ev.role.as_str(), ev.model.as_str()),
        ("extraction", "nemo-test")
    );
    assert_eq!(ev.upstream, "127.0.0.1");
    assert_eq!(ev.priority, Priority::Interactive);
    assert_eq!(
        (ev.prompt_tokens, ev.completion_tokens),
        (Some(12), Some(3))
    );
    assert!(!serde_json::to_string(&ev).unwrap().contains("hi\""));

    gw.stop.cancel();
    stop_up.cancel();
}

#[tokio::test]
async fn workarounds_apply_per_role_and_switch_off_together() {
    let (up, _vllm, stop_up) = start_vllm().await;
    let on = start_gateway(&up, wide(), false, true).await;
    let off = start_gateway(&up, wide(), false, false).await;

    // `default` enables strip-think: the reasoning leaves the answer.
    let body: Value = chat(&on, "pmos/default", "q")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let msg = &body["choices"][0]["message"];
    assert_eq!(msg["content"], "The answer.");
    assert_eq!(msg["reasoning_content"], "reasoning here");
    assert_eq!(
        on.usage.recent().last().unwrap().workarounds,
        vec!["strip-think"]
    );

    // `extraction` doesn't enable it.
    let body: Value = chat(&on, "pmos/extraction", "q")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body["choices"][0]["message"]["content"]
        .as_str()
        .unwrap()
        .starts_with("<think>"));

    // All off, for the benchmark that decides whether a workaround can go.
    let body: Value = chat(&off, "pmos/default", "q")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body["choices"][0]["message"]["content"]
        .as_str()
        .unwrap()
        .starts_with("<think>"));

    // The registry is visible, with each entry's delete-when test.
    let reg: Value = reqwest::Client::new()
        .get(format!("{}/v1/pmos/workarounds", on.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reg["enabled"], true);
    assert_eq!(reg["workarounds"][0]["id"], "strip-think");
    assert_eq!(reg["workarounds"][0]["roles"], json!(["pmos/default"]));
    assert!(reg["workarounds"][0]["delete_when"]
        .as_str()
        .unwrap()
        .contains("benchmark"));

    on.stop.cancel();
    off.stop.cancel();
    stop_up.cancel();
}

#[tokio::test]
async fn streams_pass_through_and_are_logged_when_they_end() {
    let (up, _vllm, stop_up) = start_vllm().await;
    let gw = start_gateway(&up, wide(), false, true).await;
    let res = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", gw.base))
        .bearer_auth(TOKEN)
        .json(&json!({"model": "pmos/default", "stream": true,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["content-type"], "text/event-stream");
    let text = res.text().await.unwrap();
    assert!(text.contains("Hello") && text.contains(" there") && text.contains("[DONE]"));
    tokio::time::sleep(Duration::from_millis(50)).await;
    let ev = gw.usage.recent().pop().unwrap();
    assert!(ev.stream);
    assert_eq!(ev.status, 200);
    gw.stop.cancel();
    stop_up.cancel();
}

#[tokio::test]
async fn interactive_work_overtakes_background_work() {
    let (up, vllm, stop_up) = start_vllm().await;
    let gw = Arc::new(
        start_gateway(
            &up,
            Limits {
                max_in_flight: 1,
                per_user: 4,
                max_queue: 8,
            },
            false,
            true,
        )
        .await,
    );
    let send = |text: &'static str, user: &'static str, prio: &'static str| {
        let gw = gw.clone();
        tokio::spawn(async move {
            chat(&gw, "pmos/default", text)
                .header("x-pmos-user", user)
                .header("x-pmos-priority", prio)
                .send()
                .await
                .unwrap()
                .status()
        })
    };
    let first = send("slow-first", "ana", "personal");
    tokio::time::sleep(Duration::from_millis(60)).await;
    let routine = send("slow-routine", "bo", "scheduled");
    tokio::time::sleep(Duration::from_millis(30)).await;
    let personal = send("slow-personal", "cy", "personal");
    tokio::time::sleep(Duration::from_millis(30)).await;
    let asked = send("slow-asked", "di", "interactive");
    for h in [first, routine, personal, asked] {
        assert_eq!(h.await.unwrap(), 200);
    }
    let order: Vec<String> = vllm
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.1.clone())
        .collect();
    assert_eq!(
        order,
        vec!["slow-first", "slow-asked", "slow-routine", "slow-personal"]
    );
    gw.stop.cancel();
    stop_up.cancel();
}

#[tokio::test]
async fn one_person_cant_hold_every_slot_and_a_full_queue_says_busy() {
    let (up, vllm, stop_up) = start_vllm().await;
    let gw = Arc::new(
        start_gateway(
            &up,
            Limits {
                max_in_flight: 2,
                per_user: 1,
                max_queue: 1,
            },
            false,
            true,
        )
        .await,
    );
    let send = |text: &'static str, user: &'static str| {
        let gw = gw.clone();
        tokio::spawn(async move {
            chat(&gw, "pmos/default", text)
                .header("x-pmos-user", user)
                .send()
                .await
                .unwrap()
        })
    };
    let a1 = send("slow-a1", "ana");
    tokio::time::sleep(Duration::from_millis(40)).await;
    let a2 = send("slow-a2", "ana"); // waits: Ana is at her cap
    tokio::time::sleep(Duration::from_millis(40)).await;
    let b1 = send("slow-b1", "bo"); // runs in the free slot
    tokio::time::sleep(Duration::from_millis(40)).await;
    // The queue (1) holds Ana's second request; one more is refused.
    let busy = send("slow-c1", "cy").await.unwrap();
    assert_eq!(busy.status(), 503);
    assert_eq!(busy.headers()["retry-after"], "5");
    for h in [a1, a2, b1] {
        assert_eq!(h.await.unwrap().status(), 200);
    }
    let order: Vec<String> = vllm
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.1.clone())
        .collect();
    assert_eq!(order, vec!["slow-a1", "slow-b1", "slow-a2"]);
    gw.stop.cancel();
    stop_up.cancel();
}

#[tokio::test]
async fn model_server_errors_are_passed_on_and_an_outage_is_reported() {
    let (up, _vllm, stop_up) = start_vllm().await;
    let gw = start_gateway(&up, wide(), false, true).await;
    // The model server's own refusal reaches the caller.
    let res = chat(&gw, "pmos/default", "too long").send().await.unwrap();
    assert_eq!(res.status(), 400);
    let body: Value = res.json().await.unwrap();
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("context length"));
    let ready = reqwest::get(format!("{}/readyz", gw.base)).await.unwrap();
    assert_eq!(ready.status(), 200);

    // The model server goes away.
    stop_up.cancel();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let res = chat(&gw, "pmos/default", "hi").send().await.unwrap();
    assert_eq!(res.status(), 502);
    assert_eq!(gw.usage.recent().last().unwrap().status, 502);
    let ready = reqwest::get(format!("{}/readyz", gw.base)).await.unwrap();
    assert_eq!(ready.status(), 503);
    gw.stop.cancel();
}

#[tokio::test]
async fn the_stub_model_needs_no_model_server() {
    // Nothing listens on this port.
    let gw = start_gateway("http://127.0.0.1:9/v1", wide(), true, true).await;
    let body: Value = chat(&gw, "pmos/default", "What is DSM?")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["choices"][0]["message"]["content"],
        "Stub reply to: What is DSM?"
    );
    let text = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", gw.base))
        .bearer_auth(TOKEN)
        .json(&json!({"model": "pmos/default", "stream": true,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(text.contains("Stub") && text.ends_with("data: [DONE]\n\n"));
    let emb: Value = reqwest::Client::new()
        .post(format!("{}/v1/embeddings", gw.base))
        .bearer_auth(TOKEN)
        .json(&json!({"model": "pmos/embedding", "input": ["a", "a"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(emb["data"][0]["embedding"], emb["data"][1]["embedding"]);
    assert_eq!(
        reqwest::get(format!("{}/readyz", gw.base))
            .await
            .unwrap()
            .status(),
        200
    );
    assert!(gw.usage.recent().iter().all(|e| e.stub));
    gw.stop.cancel();
}

#[test]
fn the_repo_config_is_valid() {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/gateway.yaml"),
    )
    .unwrap();
    let models = Models::from_yaml(&text).unwrap();
    assert!(models.role("pmos/default").is_some());
    assert!(models.role("pmos/extraction").is_some());
}
