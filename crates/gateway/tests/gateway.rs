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
use pmos_gateway::config::{Client, Config, Limits, Models, UpstreamKind};
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
         reasoning: {{upstream: a, model: qwen-test, workarounds: [strip-think]}}\n  \
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
        anthropic_key: "test-key".into(),
        remote_key: String::new(),
        retry: Default::default(),
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
        vec![
            "pmos/default",
            "pmos/embedding",
            "pmos/extraction",
            "pmos/reasoning"
        ]
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
    assert_eq!(
        reg["workarounds"][0]["roles"],
        json!(["pmos/default", "pmos/reasoning"])
    );
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
    let body: Value = chat(&gw, "pmos/default", "What is Acme Vault?")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["choices"][0]["message"]["content"],
        "Stub reply to: What is Acme Vault?"
    );
    // The PM assistant's role answers too.
    let body: Value = chat(&gw, "pmos/reasoning", "Critique this PRD")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["choices"][0]["message"]["content"],
        "Stub reply to: Critique this PRD"
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
    assert!(models.role("pmos/reasoning").is_some());
}

/// Every shipped gateway config serves `pmos/reasoning`, on the same upstream, model and
/// workarounds as `pmos/default` until the bake-off picks a larger model.
#[test]
fn every_repo_config_serves_reasoning_like_default() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (file, cloud_test) in [
        ("config/gateway.yaml", false),
        ("deploy/gateway.yaml", false),
        ("config/gateway.claude-test.yaml", true),
    ] {
        let text = std::fs::read_to_string(root.join(file)).unwrap();
        let models = Models::parse(&text, cloud_test).unwrap_or_else(|e| panic!("{file}: {e}"));
        let default = models.role("pmos/default").unwrap();
        let reasoning = models
            .role("pmos/reasoning")
            .unwrap_or_else(|| panic!("{file} has no reasoning role"));
        assert_eq!(reasoning.upstream, default.upstream, "{file}");
        assert_eq!(reasoning.model, default.model, "{file}");
        assert_eq!(reasoning.workarounds, default.workarounds, "{file}");
        assert_eq!(reasoning.kind, default.kind, "{file}");
        assert_eq!(reasoning.effort, default.effort, "{file}");
    }
}

/// What the mock Claude Messages API saw: headers and body of each request.
#[derive(Default)]
struct Claude {
    seen: Mutex<Vec<(axum::http::HeaderMap, Value)>>,
}

async fn mock_messages(
    State(c): State<Arc<Claude>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap();
    let refuse = body["system"]
        .as_str()
        .unwrap_or_default()
        .contains("REFUSE-ME");
    c.seen.lock().unwrap().push((headers, body));
    if refuse {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"type": "error", "error": {"type": "invalid_request_error", "message": "nope"}})),
        )
            .into_response();
    }
    Json(json!({
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "content": [
            {"type": "thinking", "thinking": ""},
            {"type": "text", "text": "Port 443."},
            {"type": "tool_use", "id": "toolu_1", "name": "docs_search", "input": {"query": "ports"}}
        ],
        "stop_reason": "tool_use",
        "usage": {"input_tokens": 12, "output_tokens": 4}
    }))
    .into_response()
}

#[tokio::test]
async fn the_claude_test_upstream_needs_its_switch_and_translates_both_ways() {
    let claude = Arc::new(Claude::default());
    let (up, stop_up) = serve(
        Router::new()
            .route("/v1/messages", post(mock_messages))
            .with_state(claude.clone()),
    )
    .await;
    let yaml = format!(
        "upstreams: {{claude: 'anthropic+{up}'}}\n\
         roles:\n  default: {{upstream: claude, model: claude-opus-5-5, effort: medium}}\n"
    );
    // Off by default: the gateway refuses to start with it.
    let err = Models::from_yaml(&yaml).unwrap_err();
    assert!(err.contains("PMOS_GATEWAY_CLOUD_TEST=1"), "{err}");
    // A non-local Claude upstream must be https.
    let err = Models::parse(
        "upstreams: {c: 'anthropic+http://api.example.com'}\nroles: {default: {upstream: c, model: m}}\n",
        true,
    )
    .unwrap_err();
    assert!(err.contains("https"), "{err}");
    // Effort is only for the Claude upstream.
    let err = Models::parse(
        "upstreams: {q: 'http://127.0.0.1:1/v1'}\nroles: {default: {upstream: q, model: m, effort: low}}\n",
        true,
    )
    .unwrap_err();
    assert!(err.contains("only the Claude test upstream"), "{err}");

    let models = Models::parse(&yaml, true).unwrap();
    let usage = Arc::new(UsageLog::new(None).unwrap());
    let (gw, stop_gw) = serve(router(GatewayConfig {
        models,
        clients: vec![Client {
            name: "runtime".into(),
            token: TOKEN.into(),
        }],
        usage: usage.clone(),
        stub: false,
        workarounds_on: true,
        anthropic_key: "sk-test-placeholder".into(),
        remote_key: String::new(),
        retry: Default::default(),
    }))
    .await;
    let http = reqwest::Client::new();
    let ask = json!({
        "model": "pmos/default",
        "temperature": 0,
        "messages": [
            {"role": "system", "content": "You are the Docs bot."},
            {"role": "user", "content": "What ports?"}
        ],
        "tools": [{"type": "function", "function": {"name": "docs_search", "parameters": {"type": "object", "properties": {}}}}]
    });
    let res = http
        .post(format!("{gw}/v1/chat/completions"))
        .bearer_auth(TOKEN)
        .header("X-Pmos-User", "ana")
        .json(&ask)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let answer: Value = res.json().await.unwrap();
    let choice = &answer["choices"][0];
    assert_eq!(choice["finish_reason"], "tool_calls");
    assert_eq!(choice["message"]["content"], "Port 443.");
    assert_eq!(
        choice["message"]["tool_calls"][0]["function"]["name"],
        "docs_search"
    );
    assert_eq!(answer["usage"]["prompt_tokens"], 12);

    {
        let seen = claude.seen.lock().unwrap();
        let (headers, body) = &seen[0];
        assert_eq!(headers["x-api-key"], "sk-test-placeholder");
        assert_eq!(headers["anthropic-version"], "2026-10-21");
        assert_eq!(headers["anthropic-beta"], "server-side-fallback-2026-07-01");
        assert!(
            headers.get("authorization").is_none(),
            "the client token never goes upstream"
        );
        assert_eq!(body["model"], "claude-opus-5-5");
        assert_eq!(body["output_config"]["effort"], "medium");
        assert!(body.get("temperature").is_none());
        assert_eq!(body["system"], "You are the Docs bot.");
    }

    // A streamed request gets the answer as an event stream.
    let mut streamed = ask.clone();
    streamed["stream"] = json!(true);
    let text = http
        .post(format!("{gw}/v1/chat/completions"))
        .bearer_auth(TOKEN)
        .json(&streamed)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        text.contains("Port 443.") && text.ends_with("data: [DONE]\n\n"),
        "{text}"
    );

    // An error from the Messages API comes back with its message and status.
    let mut bad = ask.clone();
    bad["messages"][0]["content"] = json!("REFUSE-ME");
    let res = http
        .post(format!("{gw}/v1/chat/completions"))
        .bearer_auth(TOKEN)
        .json(&bad)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["message"], "nope");

    // No embeddings on the Claude upstream.
    let res = http
        .post(format!("{gw}/v1/embeddings"))
        .bearer_auth(TOKEN)
        .json(&json!({"model": "pmos/default", "input": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
    stop_gw.cancel();
    stop_up.cancel();
}

/// What a mock OpenAI-compatible server saw: path, headers and body of each request.
#[derive(Default)]
struct Recorder {
    seen: Mutex<Vec<(String, axum::http::HeaderMap, Value)>>,
}

async fn record_chat(
    State(r): State<Arc<Recorder>>,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap();
    let stream = body["stream"] == json!(true);
    r.seen
        .lock()
        .unwrap()
        .push((uri.path().to_string(), headers, body));
    if stream {
        // The tool call's id comes first; its extra_content in a later delta.
        let sse = "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"tool_calls\":[{\"index\":0,\"id\":\"call_2\",\"type\":\"function\",\"function\":{\"name\":\"docs_search\"}}]}}]}\n\n\
                   data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"extra_content\":{\"google\":{\"thought_signature\":\"sig-2\"}},\"function\":{\"arguments\":\"{}\"}}]}}]}\n\n\
                   data: {\"choices\":[],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":2}}\n\n\
                   data: [DONE]\n\n";
        return ([("content-type", "text/event-stream")], sse).into_response();
    }
    Json(json!({
        "id": "x", "object": "chat.completion", "model": "remote-model",
        "choices": [{"index": 0, "finish_reason": "tool_calls", "message": {"role": "assistant",
            "content": null, "tool_calls": [{"id": "call_1", "type": "function",
            "extra_content": {"google": {"thought_signature": "sig-1"}},
            "function": {"name": "docs_search", "arguments": "{\"query\":\"ports\"}"}}]}}],
        "usage": {"prompt_tokens": 21, "completion_tokens": 5, "total_tokens": 26},
    }))
    .into_response()
}

async fn record_models(State(r): State<Arc<Recorder>>, headers: axum::http::HeaderMap) -> Response {
    r.seen
        .lock()
        .unwrap()
        .push(("models".into(), headers, Value::Null));
    Json(json!({"data": []})).into_response()
}

async fn start_recorder(prefix: &str) -> (String, Arc<Recorder>, CancellationToken) {
    let r = Arc::new(Recorder::default());
    let app = Router::new()
        .route(&format!("{prefix}/chat/completions"), post(record_chat))
        .route(&format!("{prefix}/models"), get(record_models))
        .with_state(r.clone());
    let (base, stop) = serve(app).await;
    (format!("{base}{prefix}"), r, stop)
}

const REMOTE_KEY: &str = "remote-test-key-must-never-be-logged-42";

#[tokio::test]
async fn a_remote_test_upstream_needs_its_switch_and_forwards_as_is_with_its_key() {
    let (remote, seen_remote, stop_remote) = start_recorder("/v1beta/openai").await;
    let (local, seen_local, stop_local) = start_recorder("/v1").await;
    let yaml = format!(
        "upstreams: {{remote: 'openai+{remote}', local: '{local}'}}\n\
         roles:\n  default: {{upstream: remote, model: remote-model}}\n  \
         extraction: {{upstream: local, model: local-model}}\n"
    );

    // Off by default: prompts would leave the machine.
    let err = Models::from_yaml(&yaml).unwrap_err();
    assert!(
        err.contains("PMOS_GATEWAY_CLOUD_TEST=1")
            && err.contains("leave this machine")
            && err.contains("testing only"),
        "{err}"
    );
    // A real remote API must be https.
    let err = Models::parse(
        "upstreams: {r: 'openai+http://api.example.com/v1'}\nroles: {default: {upstream: r, model: m}}\n",
        true,
    )
    .unwrap_err();
    assert!(err.contains("https"), "{err}");
    let https = Models::parse(
        "upstreams: {r: 'openai+https://api.example.com/v1beta/openai/'}\nroles: {default: {upstream: r, model: m}}\n",
        true,
    )
    .unwrap();
    let role = https.role("pmos/default").unwrap();
    assert_eq!(role.kind, UpstreamKind::RemoteOpenAi);
    assert_eq!(role.upstream, "https://api.example.com/v1beta/openai");
    // `effort` is only for the Claude upstream.
    let err = Models::parse(
        "upstreams: {r: 'openai+https://api.example.com/v1'}\nroles: {default: {upstream: r, model: m, effort: low}}\n",
        true,
    )
    .unwrap_err();
    assert!(err.contains("only the Claude test upstream"), "{err}");

    // Without the key the gateway won't start.
    let models = Models::parse(&yaml, true).unwrap();
    let token = "t".repeat(32);
    let env = |key: Option<&'static str>| {
        let token = token.clone();
        Config::from_lookup(move |k| match k {
            "PMOS_GATEWAY_TOKENS" => Some(format!("runtime={token}")),
            "PMOS_GATEWAY_CLOUD_TEST" => Some("1".into()),
            "PMOS_GATEWAY_REMOTE_KEY" => key.map(String::from),
            _ => None,
        })
        .unwrap()
    };
    let err = env(None).check_keys(&models).unwrap_err();
    assert!(err.contains("PMOS_GATEWAY_REMOTE_KEY"), "{err}");
    let cfg = env(Some(REMOTE_KEY));
    cfg.check_keys(&models).unwrap();
    assert!(!format!("{cfg:?}").contains(REMOTE_KEY));

    let usage = Arc::new(UsageLog::new(None).unwrap());
    let (gw, stop_gw) = serve(router(GatewayConfig {
        models,
        clients: vec![Client {
            name: "worker".into(),
            token: TOKEN.into(),
        }],
        usage: usage.clone(),
        stub: false,
        workarounds_on: true,
        anthropic_key: String::new(),
        remote_key: REMOTE_KEY.into(),
        retry: Default::default(),
    }))
    .await;
    let http = reqwest::Client::new();
    let ask = json!({
        "model": "pmos/default",
        "temperature": 0,
        "messages": [{"role": "user", "content": "What ports?"}],
        "tools": [{"type": "function", "function": {"name": "docs_search",
            "parameters": {"type": "object", "properties": {"query": {"type": "string"}}}}}]
    });
    let post = |body: &Value| {
        http.post(format!("{gw}/v1/chat/completions"))
            .bearer_auth(TOKEN)
            .header("X-Pmos-User", "ana")
            .header("X-Pmos-Run", "run-remote")
            .header("X-Pmos-Priority", "interactive")
            .json(body)
    };

    // Forwarded to <base>/chat/completions with the remote key and the role's model; the
    // answer (a tool call) comes back as the upstream sent it.
    let res = post(&ask).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["x-pmos-model"], "remote-model");
    let answer: Value = res.json().await.unwrap();
    assert_eq!(answer["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(
        answer["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "docs_search"
    );
    {
        let seen = seen_remote.seen.lock().unwrap();
        let (path, headers, body) = &seen[0];
        assert_eq!(path, "/v1beta/openai/chat/completions");
        assert_eq!(headers["authorization"], format!("Bearer {REMOTE_KEY}"));
        assert!(headers.get("x-pmos-user").is_none());
        assert_eq!(body["model"], "remote-model");
        assert_eq!(body["temperature"], 0);
        assert_eq!(body["tools"], ask["tools"]);
        assert_eq!(body["messages"], ask["messages"]);
    }

    // Streaming passes through.
    let mut streamed = ask.clone();
    streamed["stream"] = json!(true);
    streamed["stream_options"] = json!({"include_usage": true});
    let res = post(&streamed).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["content-type"], "text/event-stream");
    let text = res.text().await.unwrap();
    assert!(
        text.contains("call_2") && text.ends_with("data: [DONE]\n\n"),
        "{text}"
    );
    assert_eq!(seen_remote.seen.lock().unwrap()[1].2["stream"], true);

    // The usage log records both calls against the remote host, never the text or the key.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let events: Vec<_> = usage
        .recent()
        .into_iter()
        .filter(|e| e.run.as_deref() == Some("run-remote"))
        .collect();
    assert_eq!(events.len(), 2);
    assert_eq!(
        (events[0].role.as_str(), events[0].model.as_str()),
        ("default", "remote-model")
    );
    assert_eq!(events[0].upstream, "127.0.0.1");
    assert_eq!(
        (events[0].prompt_tokens, events[0].completion_tokens),
        (Some(21), Some(5))
    );
    assert!(!events[0].stream && events[1].stream);
    assert_eq!(events[1].status, 200);
    for e in &events {
        let line = serde_json::to_string(e).unwrap();
        assert!(!line.contains(REMOTE_KEY) && !line.contains("What ports"));
    }

    // The caller's client drops each tool call's extra_content when it sends the history
    // back; the gateway gives it back to the remote upstream, from both answers above.
    let mut follow_up = ask.clone();
    follow_up["messages"] = json!([
        {"role": "user", "content": "What ports?"},
        {"role": "assistant", "content": "", "tool_calls": [
            {"id": "call_1", "type": "function", "function": {"name": "docs_search", "arguments": "{}"}},
            {"id": "call_2", "type": "function", "function": {"name": "docs_search", "arguments": "{}"}},
            {"id": "call_x", "type": "function", "function": {"name": "docs_search", "arguments": "{}"}}
        ]},
        {"role": "tool", "tool_call_id": "call_1", "content": "443"},
        {"role": "tool", "tool_call_id": "call_2", "content": "443"},
        {"role": "tool", "tool_call_id": "call_x", "content": "443"}
    ]);
    assert_eq!(post(&follow_up).send().await.unwrap().status(), 200);
    {
        let seen = seen_remote.seen.lock().unwrap();
        let calls = &seen[2].2["messages"][1]["tool_calls"];
        assert_eq!(
            calls[0]["extra_content"]["google"]["thought_signature"],
            "sig-1"
        );
        assert_eq!(
            calls[1]["extra_content"]["google"]["thought_signature"],
            "sig-2"
        );
        assert!(calls[2].get("extra_content").is_none());
        let mut want = follow_up["messages"].clone();
        want[1]["tool_calls"][0]["extra_content"] = calls[0]["extra_content"].clone();
        want[1]["tool_calls"][1]["extra_content"] = calls[1]["extra_content"].clone();
        assert_eq!(seen[2].2["messages"], want, "nothing else changes");
    }

    // A local upstream gets the request unchanged and never sees the remote key.
    let mut local_ask = follow_up.clone();
    local_ask["model"] = json!("pmos/extraction");
    local_ask["stream"] = json!(true);
    local_ask["stream_options"] = json!({"include_usage": true});
    local_ask["parallel_tool_calls"] = json!(true);
    let res = post(&local_ask).send().await.unwrap();
    assert_eq!(res.status(), 200);
    res.text().await.unwrap();
    {
        let seen = seen_local.seen.lock().unwrap();
        let (path, headers, body) = &seen[0];
        assert_eq!(path, "/v1/chat/completions");
        assert!(headers.get("authorization").is_none());
        let mut want = local_ask.clone();
        want["model"] = json!("local-model");
        assert_eq!(body, &want);
    }

    // Readiness asks the remote API with its key, and says a testing-only upstream is on.
    let ready: Value = reqwest::get(format!("{gw}/readyz"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ready["ready"], true);
    assert_eq!(ready["cloud_test"], true);
    {
        let seen = seen_remote.seen.lock().unwrap();
        let (_, headers, _) = seen.iter().find(|s| s.0 == "models").unwrap();
        assert_eq!(headers["authorization"], format!("Bearer {REMOTE_KEY}"));
        let seen = seen_local.seen.lock().unwrap();
        let (_, headers, _) = seen.iter().find(|s| s.0 == "models").unwrap();
        assert!(headers.get("authorization").is_none());
    }

    // An outage is reported without the key.
    stop_remote.cancel();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let res = post(&ask).send().await.unwrap();
    assert_eq!(res.status(), 502);
    assert!(!res.text().await.unwrap().contains(REMOTE_KEY));

    stop_gw.cancel();
    stop_local.cancel();
}

/// The real binary: it won't start without the remote key, says at start-up that a remote
/// test upstream is on, and never prints the key, even when the upstream is down.
#[tokio::test]
async fn the_gateway_binary_needs_the_remote_key_and_never_logs_it() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    let dir = std::env::temp_dir().join(format!("pmos-gateway-remote-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // Nothing listens on the upstream's port.
    let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_port = dead.local_addr().unwrap().port();
    drop(dead);
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let bind = free.local_addr().unwrap();
    drop(free);
    let config = dir.join("gateway.yaml");
    std::fs::write(
        &config,
        format!(
            "upstreams: {{r: 'openai+http://127.0.0.1:{dead_port}/v1'}}\n\
             roles: {{default: {{upstream: r, model: remote-model}}}}\n"
        ),
    )
    .unwrap();
    let gateway = || {
        let mut c = Command::new(env!("CARGO_BIN_EXE_pmos-gateway"));
        c.env_clear()
            .env("PMOS_GATEWAY_TOKENS", format!("worker={TOKEN}"))
            .env("PMOS_GATEWAY_CONFIG", &config)
            .env("PMOS_GATEWAY_BIND", bind.to_string())
            .env("PMOS_GATEWAY_CLOUD_TEST", "1")
            .env("RUST_BACKTRACE", "0")
            .env("RUST_LOG", "debug")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    };

    let out = gateway().output().unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("PMOS_GATEWAY_REMOTE_KEY"), "{stderr}");

    let mut child = gateway()
        .env("PMOS_GATEWAY_REMOTE_KEY", REMOTE_KEY)
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let lines = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = lines.clone();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            sink.lock().unwrap().push(line);
        }
    });
    let base = format!("http://{bind}");
    let mut up = false;
    for _ in 0..100 {
        if reqwest::get(format!("{base}/healthz")).await.is_ok() {
            up = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(up, "the gateway didn't start: {:?}", lines.lock().unwrap());
    let res = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth(TOKEN)
        .json(&json!({"model": "pmos/default", "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 502);
    assert!(!res.text().await.unwrap().contains(REMOTE_KEY));
    child.kill().unwrap();
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
    child.wait().unwrap();
    reader.join().unwrap();
    let log = lines.lock().unwrap().join("\n") + &stderr;
    assert!(log.contains("a remote test upstream is on"), "{log}");
    assert!(log.contains("model server unreachable"), "{log}");
    assert!(!log.contains(REMOTE_KEY), "{log}");
    std::fs::remove_dir_all(&dir).ok();
}

// ---- Retry of 429 / 503 (`retry.rs`) ----

use pmos_gateway::retry::RetryPolicy;

/// One scripted answer of the mock upstream.
#[derive(Clone)]
enum Step {
    Refuse(u16, Option<&'static str>, &'static str),
    Answer,
    /// A 200 stream that sends one event and then breaks.
    BrokenStream,
}

#[derive(Default)]
struct Scripted {
    steps: Vec<Step>,
    calls: Mutex<Vec<std::time::Instant>>,
}

async fn mock_scripted(State(s): State<Arc<Scripted>>, _body: Bytes) -> Response {
    let n = {
        let mut c = s.calls.lock().unwrap();
        c.push(std::time::Instant::now());
        c.len()
    };
    // Past the script, the last step repeats.
    match s.steps[(n - 1).min(s.steps.len() - 1)].clone() {
        Step::Refuse(status, retry_after, body) => {
            let mut res = (
                StatusCode::from_u16(status).unwrap(),
                [("content-type", "application/json")],
                body,
            )
                .into_response();
            if let Some(v) = retry_after {
                res.headers_mut().insert("retry-after", v.parse().unwrap());
            }
            res
        }
        Step::Answer => Json(json!({
            "id": "x", "object": "chat.completion", "model": "qwen-test",
            "choices": [{"index": 0, "finish_reason": "stop",
                "message": {"role": "assistant", "content": "Done."}}],
            "usage": {"prompt_tokens": 4, "completion_tokens": 1},
        }))
        .into_response(),
        Step::BrokenStream => {
            use futures_util::StreamExt;
            let first = futures_util::stream::iter([Ok::<_, std::io::Error>(Bytes::from_static(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
            ))]);
            // The break comes after the first event has had time to reach the caller.
            let broken = futures_util::stream::once(async {
                tokio::time::sleep(Duration::from_millis(150)).await;
                Err(std::io::Error::other("upstream died"))
            });
            (
                [("content-type", "text/event-stream")],
                axum::body::Body::from_stream(first.chain(broken)),
            )
                .into_response()
        }
    }
}

const RATE: &str = r#"{"error":{"code":"rate_limit_exceeded","message":"Rate limit reached"}}"#;

/// A gateway with one slot in front of a scripted upstream.
async fn retry_gateway(steps: Vec<Step>, retry: RetryPolicy) -> (Gw, Arc<Scripted>) {
    let s = Arc::new(Scripted {
        steps,
        calls: Mutex::default(),
    });
    let app = Router::new()
        .route("/v1/chat/completions", post(mock_scripted))
        .with_state(s.clone());
    let (base, _stop) = serve(app).await;
    let models = Models::from_yaml(&format!(
        "upstreams: {{a: '{base}/v1'}}\nroles:\n  default: {{upstream: a, model: qwen-test}}\n\
         limits: {{max-in-flight: 1, per-user: 1, max-queue: 8}}\n"
    ))
    .unwrap();
    let usage = Arc::new(UsageLog::new(None).unwrap());
    let (gw, stop) = serve(router(GatewayConfig {
        models,
        clients: vec![Client {
            name: "runtime".into(),
            token: TOKEN.into(),
        }],
        usage: usage.clone(),
        stub: false,
        workarounds_on: true,
        anthropic_key: String::new(),
        remote_key: String::new(),
        retry,
    }))
    .await;
    (
        Gw {
            base: gw,
            usage,
            stop,
        },
        s,
    )
}

fn quick(max_attempts: u32, budget_ms: u64) -> RetryPolicy {
    RetryPolicy {
        max_attempts,
        budget: Duration::from_millis(budget_ms),
        base: Duration::from_millis(20),
    }
}

fn streamed(gw: &Gw) -> reqwest::RequestBuilder {
    reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", gw.base))
        .bearer_auth(TOKEN)
        .json(&json!({"model": "pmos/default", "stream": true,
            "messages": [{"role": "user", "content": "hi"}]}))
}

#[tokio::test]
async fn a_429_is_retried_and_the_second_attempt_answers() {
    let (gw, up) = retry_gateway(
        vec![Step::Refuse(429, None, RATE), Step::Answer],
        quick(3, 5000),
    )
    .await;
    let res = chat(&gw, "pmos/default", "secret prompt text")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["x-pmos-upstream-attempts"], "2");
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "Done.");
    assert_eq!(up.calls.lock().unwrap().len(), 2);

    // One usage line per attempt, the failed one marked, never any text.
    let events = gw.usage.recent();
    assert_eq!(events.len(), 2);
    let seen: Vec<_> = events
        .iter()
        .map(|e| (e.status, e.attempt, e.retrying))
        .collect();
    assert_eq!(seen, vec![(429, 1, true), (200, 2, false)]);
    assert_eq!(events[1].attempts, 2);
    assert_eq!(events[1].completion_tokens, Some(1));
    let logged = serde_json::to_string(&events).unwrap();
    assert!(!logged.contains("secret prompt") && !logged.contains("Done."));
    gw.stop.cancel();
}

#[tokio::test]
async fn retry_after_is_honoured() {
    let (gw, up) = retry_gateway(
        vec![Step::Refuse(503, Some("1"), "{}"), Step::Answer],
        quick(3, 5000),
    )
    .await;
    let res = chat(&gw, "pmos/default", "hi").send().await.unwrap();
    assert_eq!(res.status(), 200);
    let calls = up.calls.lock().unwrap();
    let gap = calls[1] - calls[0];
    assert!(gap >= Duration::from_millis(990), "waited only {gap:?}");
    gw.stop.cancel();
}

#[tokio::test]
async fn it_gives_up_within_the_budget_and_returns_the_upstreams_answer() {
    // Retry-After (2 s) is beyond the 1 s budget: no wait is started at all.
    let (gw, up) = retry_gateway(vec![Step::Refuse(429, Some("2"), RATE)], quick(3, 1000)).await;
    let t = std::time::Instant::now();
    let res = chat(&gw, "pmos/default", "hi").send().await.unwrap();
    assert!(t.elapsed() < Duration::from_millis(900));
    assert_eq!(res.status(), 429);
    assert_eq!(res.headers()["x-pmos-upstream-attempts"], "1");
    assert_eq!(res.headers()["retry-after"], "2");
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "rate_limit_exceeded");
    assert_eq!(up.calls.lock().unwrap().len(), 1);
    gw.stop.cancel();

    // With backoff only, it uses every attempt and then returns the upstream's status.
    let (gw, up) = retry_gateway(vec![Step::Refuse(503, None, RATE)], quick(3, 1000)).await;
    let t = std::time::Instant::now();
    let res = chat(&gw, "pmos/default", "hi").send().await.unwrap();
    assert!(t.elapsed() < Duration::from_millis(1000));
    assert_eq!(res.status(), 503);
    assert_eq!(res.headers()["x-pmos-upstream-attempts"], "3");
    assert_eq!(up.calls.lock().unwrap().len(), 3);
    let events = gw.usage.recent();
    assert_eq!(
        events.iter().map(|e| e.retrying).collect::<Vec<_>>(),
        vec![true, true, false]
    );
    assert!(events.iter().all(|e| e.status == 503));
    gw.stop.cancel();
}

#[tokio::test]
async fn a_daily_quota_fails_fast() {
    let daily = r#"[{"error":{"status":"RESOURCE_EXHAUSTED","details":[{"violations":[{"quotaId":"GenerateRequestsPerDayPerProjectPerModel-FreeTier"}]}]}}]"#;
    let (gw, up) = retry_gateway(vec![Step::Refuse(429, None, daily)], quick(3, 5000)).await;
    let res = chat(&gw, "pmos/default", "hi").send().await.unwrap();
    assert_eq!(res.status(), 429);
    assert_eq!(res.headers()["x-pmos-upstream-attempts"], "1");
    assert_eq!(res.text().await.unwrap(), daily);
    assert_eq!(up.calls.lock().unwrap().len(), 1);
    gw.stop.cancel();
}

#[tokio::test]
async fn a_400_is_never_retried() {
    let bad = r#"{"error":{"message":"maximum context length exceeded"}}"#;
    for status in [400, 401, 404, 422] {
        let (gw, up) = retry_gateway(
            vec![Step::Refuse(status, None, bad), Step::Answer],
            quick(3, 5000),
        )
        .await;
        let res = chat(&gw, "pmos/default", "hi").send().await.unwrap();
        assert_eq!(res.status(), status);
        assert_eq!(res.headers()["x-pmos-upstream-attempts"], "1");
        assert_eq!(up.calls.lock().unwrap().len(), 1, "{status}");
        gw.stop.cancel();
    }
}

#[tokio::test]
async fn nothing_is_retried_once_streaming_has_begun() {
    let (gw, up) = retry_gateway(vec![Step::BrokenStream, Step::Answer], quick(3, 5000)).await;
    let res = streamed(&gw).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["x-pmos-upstream-attempts"], "1");
    // The stream breaks after its first event; the caller sees that, not a second try.
    let _ = res.bytes().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(up.calls.lock().unwrap().len(), 1);
    let events = gw.usage.recent();
    assert_eq!(events.len(), 1);
    assert_eq!((events[0].attempts, events[0].retrying), (1, false));
    gw.stop.cancel();

    // A streamed request refused before its first byte is retried like any other.
    let (gw, up) = retry_gateway(
        vec![Step::Refuse(429, None, RATE), Step::BrokenStream],
        quick(3, 5000),
    )
    .await;
    let res = streamed(&gw).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["x-pmos-upstream-attempts"], "2");
    assert_eq!(up.calls.lock().unwrap().len(), 2);
    gw.stop.cancel();
}

#[tokio::test]
async fn a_waiting_retry_gives_its_slot_to_the_queue() {
    // One slot. The first call is refused and waits a second to retry; a second call must
    // not be stuck behind that wait.
    let (gw, up) = retry_gateway(
        vec![Step::Refuse(429, Some("1"), RATE), Step::Answer],
        quick(3, 5000),
    )
    .await;
    let first = chat(&gw, "pmos/default", "first").send();
    let first = tokio::spawn(async move { first.await.unwrap() });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let t = std::time::Instant::now();
    let second = chat(&gw, "pmos/default", "second").send().await.unwrap();
    assert_eq!(second.status(), 200);
    assert!(t.elapsed() < Duration::from_millis(500));
    assert_eq!(first.await.unwrap().status(), 200);
    assert_eq!(up.calls.lock().unwrap().len(), 3);
    gw.stop.cancel();
}
