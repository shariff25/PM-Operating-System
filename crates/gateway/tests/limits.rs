//! Review findings N3 and N4 and their Lows: which clients' caller headers are believed, the
//! per-client cap, the queue deadline, a stream's total time, the answer size cap, a caller
//! that hangs up, upstream URLs kept from callers, and `GET /v1/pmos/roles`; and review
//! findings R1 (a PM's own bot names only that PM), R2 (caps on calls waiting) and the Low on
//! bare client patterns.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use pmos_contract::model_source::ModelSource;
use pmos_gateway::config::{
    handle_in_name, person_key, spelled_handle, valid_client_name, Client, Config, GatewayClass,
    Models, UserRule, MAX_SPELLED_HANDLE,
};
use pmos_gateway::scheduler::Priority;
use pmos_gateway::usage::UsageLog;
use pmos_gateway::{router, GatewayConfig};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const RUNTIME: &str = "limits-test-runtime-token-0123456789";
const EVALS: &str = "limits-test-evals-token-0123456789ab";
const OTHER: &str = "limits-test-other-token-0123456789ab";
const BOT_ANA: &str = "limits-test-bot-pm-ana-token-0123456";
const BOT_DOCS: &str = "limits-test-bot-docs-token-01234567";
const BOT_ANA_K: &str = "limits-test-bot-pm-ana-k-token-01234";
const WORKER: &str = "limits-test-worker-token-0123456789a";
const BOT_JIRA: &str = "limits-test-bot-jira-token-01234567";

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

/// Counts what the mock model server is doing: requests seen, handlers (or streams) still
/// alive, and the order of the asks.
#[derive(Default)]
struct Upstream {
    seen: Mutex<Vec<String>>,
    alive: AtomicUsize,
    /// How many requests were dropped before they finished: the gateway closed the
    /// connection (its caller hung up).
    dropped: AtomicUsize,
}

/// Decrements `alive` when dropped; counts it as `dropped` unless `done` was set.
struct Alive {
    up: Arc<Upstream>,
    done: bool,
}

impl Alive {
    fn new(up: &Arc<Upstream>) -> Self {
        up.alive.fetch_add(1, Ordering::SeqCst);
        Self {
            up: up.clone(),
            done: false,
        }
    }
}

impl Drop for Alive {
    fn drop(&mut self) {
        self.up.alive.fetch_sub(1, Ordering::SeqCst);
        if !self.done {
            self.up.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }
}

fn ask_of(body: &Value) -> String {
    body["messages"][0]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The mock model server. The ask decides what it does:
/// - `slow <ms>`: answers after that long;
/// - `hang`: never answers (until the gateway hangs up);
/// - `trickle`: streams an event every 100 ms, forever;
/// - `big`: a 4 KiB answer (streamed when asked to stream);
/// - `break`: sends half an answer and then breaks the connection;
/// - anything else: answers at once.
async fn mock(State(up): State<Arc<Upstream>>, body: Bytes) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap();
    let ask = ask_of(&body);
    up.seen.lock().unwrap().push(ask.clone());
    let stream = body["stream"] == json!(true);
    let mut alive = Alive::new(&up);
    if let Some(ms) = ask.strip_prefix("slow ") {
        tokio::time::sleep(Duration::from_millis(ms.parse().unwrap())).await;
    }
    if ask == "hang" {
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
    if ask == "trickle" {
        // The guard lives in the stream, so it drops when the gateway closes the connection.
        let events = futures_util::stream::unfold(alive, |a| async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Some((
                Ok::<_, std::io::Error>(Bytes::from_static(
                    b"data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n",
                )),
                a,
            ))
        });
        return (
            [("content-type", "text/event-stream")],
            Body::from_stream(events),
        )
            .into_response();
    }
    if ask == "break" {
        alive.done = true;
        use futures_util::StreamExt;
        let first = futures_util::stream::iter([Ok::<_, std::io::Error>(Bytes::from_static(
            b"{\"id\": \"x\", \"choices\": [",
        ))]);
        // The break comes after the headers and the first bytes have gone to the gateway.
        let broken = futures_util::stream::once(async {
            tokio::time::sleep(Duration::from_millis(150)).await;
            Err(std::io::Error::other("upstream died"))
        });
        return (
            [("content-type", "application/json")],
            Body::from_stream(first.chain(broken)),
        )
            .into_response();
    }
    alive.done = true;
    let content = if ask == "big" {
        "x".repeat(4096)
    } else {
        "ok".into()
    };
    if stream {
        let sse = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"choices": [{"delta": {"content": content}}]})
        );
        return ([("content-type", "text/event-stream")], sse).into_response();
    }
    Json(json!({
        "id": "x", "object": "chat.completion", "model": body["model"],
        "choices": [{"index": 0, "finish_reason": "stop",
            "message": {"role": "assistant", "content": content}}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1},
    }))
    .into_response()
}

struct Gw {
    base: String,
    upstream: String,
    usage: Arc<UsageLog>,
    up: Arc<Upstream>,
    stops: Vec<CancellationToken>,
}

impl Drop for Gw {
    fn drop(&mut self) {
        for s in &self.stops {
            s.cancel();
        }
    }
}

/// A gateway with clients `runtime`, `evals` and `other`, the given `limits:` and `clients:`
/// (YAML flow mappings), in front of [`mock`].
async fn gateway(limits: &str, clients: &str) -> Gw {
    gateway_from(
        |upstream| {
            format!(
                "upstreams: {{a: '{upstream}/v1'}}\n\
                 roles:\n  default: {{upstream: a, model: qwen-test}}\n  extraction: {{upstream: a, model: nemo-test}}\n\
                 limits: {limits}\nclients: {clients}\n"
            )
        },
        false,
    )
    .await
}

/// A gateway whose whole config `yaml` makes from the mock's address (`http://127.0.0.1:…`,
/// no `/v1`), parsed with `cloud_test` as given; clients as [`gateway`]'s, plus `worker`.
async fn gateway_from(yaml: impl FnOnce(&str) -> String, cloud_test: bool) -> Gw {
    gateway_models(|upstream| Models::parse(&yaml(upstream), cloud_test).unwrap()).await
}

/// A shipped config (`deploy/gateway.yaml`, `config/*.yaml`) as parsed, its limits, clients
/// and class as they are, with every role's upstream moved to the mock at `upstream` (the
/// shipped hosts don't resolve here, and nothing may leave the machine in a test).
fn shipped(path: &str, upstream: &str) -> Models {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let text = std::fs::read_to_string(root.join(path)).unwrap();
    let mut m = Models::parse(&text, path.contains("claude-test")).unwrap();
    for role in m.roles.values_mut() {
        role.upstream = format!("{upstream}/v1");
    }
    m
}

/// [`gateway_from`], with the config made by `models` from the mock's address.
async fn gateway_models(models: impl FnOnce(&str) -> Models) -> Gw {
    let up = Arc::new(Upstream::default());
    let app = Router::new()
        .route("/v1/models", get(|| async { Json(json!({"data": []})) }))
        .route("/v1/chat/completions", post(mock))
        .with_state(up.clone());
    let (upstream, stop_up) = serve(app).await;
    let models = models(&upstream);
    let usage = Arc::new(UsageLog::new(None).unwrap());
    let client = |name: &str, token: &str| Client {
        name: name.into(),
        token: token.into(),
    };
    let (base, stop) = serve(router(GatewayConfig {
        models,
        clients: vec![
            client("runtime", RUNTIME),
            client("evals", EVALS),
            client("other", OTHER),
            client("bot-pm-ana", BOT_ANA),
            client("bot-docs", BOT_DOCS),
            client("bot-pm-ana-k", BOT_ANA_K),
            client("worker", WORKER),
            client("bot-jira", BOT_JIRA),
        ],
        usage: usage.clone(),
        stub: false,
        workarounds_on: true,
        anthropic_key: String::new(),
        remote_key: "limits-test-remote-key".into(),
        retry: Default::default(),
        timeouts: Default::default(),
        model_source: ModelSource::Local,
    }))
    .await;
    Gw {
        base,
        upstream,
        usage,
        up,
        stops: vec![stop, stop_up],
    }
}

fn ask(gw: &Gw, token: &str, text: &str, stream: bool) -> reqwest::RequestBuilder {
    http()
        .post(format!("{}/v1/chat/completions", gw.base))
        .bearer_auth(token)
        .json(&json!({"model": "pmos/default", "stream": stream,
            "messages": [{"role": "user", "content": text}]}))
}

/// `(running, waiting)` from `/readyz`.
async fn load(gw: &Gw) -> (u64, u64) {
    let v: Value = reqwest::get(format!("{}/readyz", gw.base))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    (
        v["running"].as_u64().unwrap(),
        v["waiting"].as_u64().unwrap(),
    )
}

/// Polls until `f` holds, for up to five seconds.
async fn eventually(what: &str, mut f: impl AsyncFnMut() -> bool) {
    for _ in 0..100 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("never happened: {what}");
}

#[tokio::test]
async fn only_trusted_clients_name_the_person_and_the_priority() {
    let gw = gateway(
        "{max-in-flight: 4, per-user: 1}",
        "{runtime: {trusted: true}, evals: {priority: personal}}",
    )
    .await;
    for token in [RUNTIME, EVALS, OTHER] {
        let res = ask(&gw, token, "hi", false)
            .header("x-pmos-user", "ana")
            .header("x-pmos-priority", "interactive")
            .header("x-pmos-run", "r1")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
    }
    let events = gw.usage.recent();
    let by = |c: &str| events.iter().find(|e| e.client == c).unwrap().clone();
    // The trusted runtime's headers are believed.
    let rt = by("runtime");
    assert_eq!(rt.user.as_deref(), Some("ana"));
    assert_eq!(rt.priority, Priority::Interactive);
    // `evals` is keyed and prioritised by its configured settings, whatever it declares.
    let ev = by("evals");
    assert_eq!(ev.user, None);
    assert_eq!(ev.priority, Priority::Personal);
    assert_eq!(ev.run.as_deref(), Some("r1"));
    // A client the config doesn't mention is not trusted, and is `personal`.
    let other = by("other");
    assert_eq!((other.user, other.priority), (None, Priority::Personal));
    // Malformed headers are still refused from every client.
    for token in [RUNTIME, EVALS, OTHER] {
        let res = ask(&gw, token, "hi", false)
            .header("x-pmos-priority", "urgent")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 400);
    }
}

#[tokio::test]
async fn an_untrusted_client_cant_spend_other_peoples_share_by_naming_them() {
    // One per person, but `other` is untrusted: naming two people doesn't get it two slots.
    let gw = Arc::new(
        gateway(
            "{max-in-flight: 4, per-user: 1, per-client: 1}",
            "{runtime: {trusted: true, max-in-flight: 4}}",
        )
        .await,
    );
    let send = |token: &'static str, text: &'static str, user: &'static str| {
        let gw = gw.clone();
        tokio::spawn(async move {
            ask(&gw, token, text, false)
                .header("x-pmos-user", user)
                .send()
                .await
                .unwrap()
                .status()
        })
    };
    let a = send(OTHER, "slow 400", "ana");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let b = send(OTHER, "slow 400", "bo");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        load(&gw).await,
        (1, 1),
        "the second waits for the client's one slot"
    );
    // The trusted runtime's calls for two people both start.
    let c = send(RUNTIME, "slow 100", "cy");
    let d = send(RUNTIME, "slow 100", "di");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(load(&gw).await, (3, 1));
    for h in [a, b, c, d] {
        assert_eq!(h.await.unwrap(), 200);
    }
}

#[tokio::test]
async fn one_token_cant_hold_every_slot() {
    // N4: the deploy config caps `evals` at 1 of 4. Three benchmark calls at once take one
    // slot; the runtime's calls still start at once.
    let gw = Arc::new(
        gateway(
            "{max-in-flight: 4, per-user: 2}",
            "{runtime: {trusted: true, max-in-flight: 4}, evals: {max-in-flight: 1, priority: personal}}",
        )
        .await,
    );
    let send = |token: &'static str, text: &'static str| {
        let gw = gw.clone();
        tokio::spawn(async move { ask(&gw, token, text, false).send().await.unwrap().status() })
    };
    let evals: Vec<_> = (0..3).map(|_| send(EVALS, "slow 300")).collect();
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(load(&gw).await, (1, 2));
    let t = std::time::Instant::now();
    assert_eq!(send(RUNTIME, "now").await.unwrap(), 200);
    assert!(
        t.elapsed() < Duration::from_millis(250),
        "the runtime didn't wait"
    );
    for h in evals {
        assert_eq!(h.await.unwrap(), 200);
    }
    assert_eq!(load(&gw).await, (0, 0));
}

#[tokio::test]
async fn a_queued_call_gives_up_at_its_deadline() {
    let gw = Arc::new(
        gateway(
            "{max-in-flight: 1, per-user: 1, queue-wait-secs: 1}",
            "{runtime: {trusted: true}}",
        )
        .await,
    );
    let gw2 = gw.clone();
    let first =
        tokio::spawn(async move { ask(&gw2, RUNTIME, "slow 2500", false).send().await.unwrap() });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let t = std::time::Instant::now();
    let res = ask(&gw, RUNTIME, "second", false).send().await.unwrap();
    assert_eq!(res.status(), 503);
    assert_eq!(res.headers()["retry-after"], "5");
    assert!(t.elapsed() >= Duration::from_millis(900) && t.elapsed() < Duration::from_secs(2));
    // It left the queue, and never reached the model server.
    assert_eq!(load(&gw).await, (1, 0));
    assert_eq!(first.await.unwrap().status(), 200);
    assert_eq!(*gw.up.seen.lock().unwrap(), vec!["slow 2500".to_string()]);
}

#[tokio::test]
async fn a_stream_is_cut_off_at_its_total_time_and_frees_its_slot() {
    let gw = gateway(
        "{max-in-flight: 1, per-user: 1, stream-max-secs: 1}",
        "{runtime: {trusted: true}}",
    )
    .await;
    let t = std::time::Instant::now();
    let res = ask(&gw, RUNTIME, "trickle", true).send().await.unwrap();
    assert_eq!(res.status(), 200);
    // The body ends in an error, not a clean end that looks like a whole answer.
    assert!(res.bytes().await.is_err());
    assert!(t.elapsed() < Duration::from_secs(3));
    eventually(
        "the slot is free and the upstream stream dropped",
        async || load(&gw).await == (0, 0) && gw.up.alive.load(Ordering::SeqCst) == 0,
    )
    .await;
    assert_eq!(gw.usage.recent().last().unwrap().status, 504);
}

#[tokio::test]
async fn answers_over_the_size_cap_are_refused() {
    let gw = gateway(
        "{max-in-flight: 2, per-user: 2, max-answer-bytes: 1024}",
        "{runtime: {trusted: true}}",
    )
    .await;
    let res = ask(&gw, RUNTIME, "big", false).send().await.unwrap();
    assert_eq!(res.status(), 502);
    let body: Value = res.json().await.unwrap();
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("larger than this gateway accepts"));
    // A streamed one is cut off with an error (here before its first byte, so the caller
    // sees the connection fail; never a clean end).
    match ask(&gw, RUNTIME, "big", true).send().await {
        Err(_) => {}
        Ok(res) => assert!(res.bytes().await.is_err()),
    }
    eventually("the stream's line is logged", async || {
        gw.usage.recent().len() == 2
    })
    .await;
    assert!(gw.usage.recent().iter().all(|e| e.status == 502));
    // A small answer still passes.
    let res = ask(&gw, RUNTIME, "hi", false).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(load(&gw).await, (0, 0));
}

#[tokio::test]
async fn a_broken_answer_never_shows_the_caller_the_upstream_url() {
    let gw = gateway("{}", "{runtime: {trusted: true}}").await;
    let res = ask(&gw, RUNTIME, "break", false).send().await.unwrap();
    assert_eq!(res.status(), 502);
    let text = res.text().await.unwrap();
    assert!(text.contains("stopped mid-answer"), "{text}");
    let host = gw.upstream.trim_start_matches("http://");
    assert!(!text.contains(host), "{text}");
    assert!(!text.contains("/v1/chat/completions"), "{text}");
}

#[tokio::test]
async fn a_caller_that_hangs_up_cancels_the_upstream_call_and_frees_its_slot() {
    let gw = gateway(
        "{max-in-flight: 1, per-user: 1}",
        "{runtime: {trusted: true}}",
    )
    .await;
    // Not streamed: the caller gives up while the model server is still working.
    let gone = reqwest::Client::builder()
        .timeout(Duration::from_millis(300))
        .build()
        .unwrap()
        .post(format!("{}/v1/chat/completions", gw.base))
        .bearer_auth(RUNTIME)
        .header("x-pmos-model-source", "local")
        .json(&json!({"model": "pmos/default", "messages": [{"role": "user", "content": "hang"}]}))
        .send()
        .await;
    assert!(gone.is_err());
    eventually(
        "the upstream call is cancelled and the slot freed",
        async || gw.up.dropped.load(Ordering::SeqCst) == 1 && load(&gw).await == (0, 0),
    )
    .await;
    // Streamed: the caller reads a little and goes away.
    let mut res = ask(&gw, RUNTIME, "trickle", true).send().await.unwrap();
    assert!(res.chunk().await.unwrap().is_some());
    assert_eq!(load(&gw).await.0, 1);
    drop(res);
    eventually(
        "the upstream stream is closed and the slot freed",
        async || {
            gw.up.dropped.load(Ordering::SeqCst) == 2
                && gw.up.alive.load(Ordering::SeqCst) == 0
                && load(&gw).await == (0, 0)
        },
    )
    .await;
    // The slot serves the next caller.
    let res = ask(&gw, RUNTIME, "hi", false).send().await.unwrap();
    assert_eq!(res.status(), 200);
}

#[tokio::test]
async fn the_roles_endpoint_names_models_but_never_an_address() {
    let gw = gateway("{}", "{}").await;
    let v: Value = http()
        .get(format!("{}/v1/pmos/roles", gw.base))
        .bearer_auth(EVALS)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        v["class"], "test",
        "a config that says no class is a test gateway"
    );
    assert_eq!(v["stub"], false);
    assert_eq!(v["cloud_test"], false);
    assert_eq!(v["workarounds"], true);
    assert_eq!(
        v["roles"],
        json!([
            {"role": "default", "alias": "pmos/default", "model": "qwen-test", "upstream": "local"},
            {"role": "extraction", "alias": "pmos/extraction", "model": "nemo-test", "upstream": "local"},
        ])
    );
    assert!(!v.to_string().contains("127.0.0.1"));
    // A client token is required.
    let res = reqwest::get(format!("{}/v1/pmos/roles", gw.base))
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
}

#[test]
fn client_rules_resolve_exact_then_longest_pattern_then_safe_defaults() {
    let m = Models::from_yaml(
        "upstreams: {a: 'http://vllm/v1'}\nroles: {default: {upstream: a, model: m}}\n\
         limits: {max-in-flight: 4, per-user: 2, per-client: 3}\n\
         clients:\n  runtime: {trusted: true, max-in-flight: 2}\n  bot-*: {trusted: true}\n  \
         bot-pm-*: {max-in-flight: 1, priority: scheduled}\n  evals: {max-in-flight: 1}\n",
    )
    .unwrap();
    let p = |c: &str| m.client_policy(c);
    assert!(p("runtime").trusted && p("runtime").max_in_flight == 2);
    assert_eq!(p("runtime").priority, Priority::Interactive);
    assert!(p("bot-docs").trusted && p("bot-docs").max_in_flight == 3);
    // The longest pattern wins, and isn't trusted unless it says so.
    assert!(!p("bot-pm-ana").trusted);
    assert_eq!(
        (p("bot-pm-ana").max_in_flight, p("bot-pm-ana").priority),
        (1, Priority::Scheduled)
    );
    assert_eq!(
        (
            p("evals").trusted,
            p("evals").max_in_flight,
            p("evals").priority
        ),
        (false, 1, Priority::Personal)
    );
    // Not listed: untrusted, personal, `per-client`.
    assert_eq!(
        (
            p("stranger").trusted,
            p("stranger").max_in_flight,
            p("stranger").priority
        ),
        (false, 3, Priority::Personal)
    );
    // With nothing said, a client's cap is `per-user`, and the deadlines are on.
    let bare = Models::from_yaml(
        "upstreams: {a: 'http://vllm/v1'}\nroles: {default: {upstream: a, model: m}}\n",
    )
    .unwrap();
    assert_eq!(bare.client_policy("runtime").max_in_flight, 2);
    assert!(!bare.client_policy("runtime").trusted);
    assert_eq!(bare.limits.queue_wait_secs, 300);
    assert_eq!(bare.limits.stream_max_secs, 900);
    assert_eq!(bare.limits.max_answer_bytes, 16 * 1024 * 1024);
    // Bad settings are refused.
    for (clients, want) in [
        ("{Bad: {}}", "client name"),
        ("{evals: {max-in-flight: 0}}", "at least 1"),
        ("{evals: {priority: urgent}}", "priority"),
        ("{evals: {trusted: yes-please}}", "trusted"),
    ] {
        let err = Models::from_yaml(&format!(
            "upstreams: {{a: 'http://vllm/v1'}}\nroles: {{default: {{upstream: a, model: m}}}}\nclients: {clients}\n"
        ))
        .unwrap_err();
        assert!(err.contains(want), "{clients}: {err}");
    }
    for (limits, want) in [
        ("{queue-wait-secs: 0}", "queue-wait-secs"),
        ("{stream-max-secs: 0}", "stream-max-secs"),
        ("{max-answer-bytes: 10}", "max-answer-bytes"),
        ("{per-client: 0}", "at least 1"),
    ] {
        let err = Models::from_yaml(&format!(
            "upstreams: {{a: 'http://vllm/v1'}}\nroles: {{default: {{upstream: a, model: m}}}}\nlimits: {limits}\n"
        ))
        .unwrap_err();
        assert!(err.contains(want), "{limits}: {err}");
    }
}

/// The shipped deploy config: the runtime and the bots' workers are trusted, and the `evals`
/// token (the eval stack: its runtime, workers and judges) holds at most one slot (N4).
#[test]
fn the_deploy_config_caps_evals_at_one_slot() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let text = std::fs::read_to_string(root.join("deploy/gateway.yaml")).unwrap();
    let m = Models::from_yaml(&text).unwrap();
    let evals = m.client_policy("evals");
    assert_eq!(
        (evals.trusted, evals.max_in_flight, evals.priority),
        (false, 1, Priority::Personal)
    );
    assert!(evals.max_in_flight < m.limits.max_in_flight);
    for c in ["runtime", "bot-docs", "bot-pm-ana"] {
        let p = m.client_policy(c);
        assert!(p.trusted, "{c}");
        assert!(
            p.max_in_flight < m.limits.max_in_flight,
            "{c} can't hold every slot"
        );
    }
    assert!(!m.client_policy("stranger").trusted);
    // R1: each PM's own bot names only that PM; a team bot keeps naming anyone.
    let own = m.client_policy("bot-pm-ana");
    assert!(own.may_name("bot-pm-ana", "ana"));
    assert!(!own.may_name("bot-pm-ana", "bo"));
    assert!(m.client_policy("bot-docs").may_name("bot-docs", "bo"));
    // R2: waiting is capped per client too, at twice its slots by default. A person's places
    // stay 4 per client (`per-user-queue`), as before W1 raised `per-user` from 2 to 3.
    assert_eq!(own.max_queue, 2 * own.max_in_flight);
    assert_eq!(m.limits.person_queue(), 4);
    // R3: the Spark's gateway is the production one.
    assert_eq!(m.class, GatewayClass::Production);
    // W1 (review at 52dbae2), a deliberate change from G3 and U1: `per-user` is 3 (it was 2),
    // and the runtime and the team bots are in separate person pools of one slot each, so a
    // team bot's calls never hold up the runtime's; the PM's own bot holds the one slot the
    // pools leave (before, it could hold both of two, and the runtime shared one with every
    // team bot). One PM can so hold 3 of the 4 model slots at once.
    assert_eq!((m.limits.per_user, m.limits.max_in_flight), (3, 4));
    let pool = |c: &str| {
        m.client_policy(c)
            .pool
            .map(|p| (m.pool_name(p.id).to_string(), p.cap))
    };
    assert_eq!(pool("runtime"), Some(("runtime".into(), 1)));
    for c in ["bot-docs", "bot-jira"] {
        assert_eq!(pool(c), Some(("team-bots".into(), 1)), "{c}");
    }
    for c in ["runtime", "bot-docs"] {
        assert_eq!(m.client_policy(c).per_user_share, 1, "{c}");
    }
    assert_eq!(m.own_bot_slots, Some(1));
    assert_eq!(own.per_user_share, 1, "her own bot: what the pools leave");
    assert_eq!(own.pool, None);
    assert_eq!(evals.pool, None);
    // Runtime, team bots and her own bot: exactly her `per-user`, so each one's slot is kept.
    assert_eq!(1 + 1 + own.per_user_share, m.limits.per_user);
    // An unlisted client still holds at most 2 slots (`per-client`), not `per-user`.
    assert_eq!(m.client_policy("stranger").max_in_flight, 2);
}

/// W1 (review at 52dbae2), with the shipped `deploy/gateway.yaml` (its limits and clients;
/// its model servers moved to a mock). Team bots that keep Ana's team-bot slot busy (a stolen
/// team-bot token could) don't hold up the runtime's call for her; with the runtime's slot
/// and the team bots' both busy, her own bot's call still starts; and she never has more than
/// her 3 running. Before W1, the runtime shared one slot with every team bot (U1), so
/// `bot-docs` holding it made her routine parsing wait `queue-wait-secs` and get `503`.
/// (Deliberately changed from U1's test, which had the runtime's call hold up bot-docs'.)
#[tokio::test]
async fn team_bots_cant_hold_up_the_runtime_and_neither_holds_up_her_own_bot() {
    let gw = Arc::new(gateway_models(|up| shipped("deploy/gateway.yaml", up)).await);
    let send = |token: &'static str, text: &'static str| {
        let gw = gw.clone();
        tokio::spawn(async move {
            ask(&gw, token, text, false)
                .header("x-pmos-user", "ana")
                .header("x-pmos-priority", "interactive")
                .send()
                .await
                .unwrap()
                .status()
        })
    };
    // Two team bots fill her team-bot slot and queue behind it.
    let d = send(BOT_DOCS, "slow 800");
    eventually("bot-docs runs", async || load(&gw).await == (1, 0)).await;
    let j = send(BOT_JIRA, "slow 800");
    let d2 = send(BOT_DOCS, "slow 800");
    eventually("the team bots' calls wait", async || {
        load(&gw).await == (1, 2)
    })
    .await;
    // The runtime's call for her starts at once.
    let r = send(RUNTIME, "slow 800");
    eventually("the runtime's call runs beside them", async || {
        load(&gw).await == (2, 2)
    })
    .await;
    // Its second waits for the runtime's own slot (one call per PM at a time, U2).
    let r2 = send(RUNTIME, "slow 100");
    eventually("the runtime's second waits", async || {
        load(&gw).await == (2, 3)
    })
    .await;
    // Runtime and team bots both busy for her: her own bot's call starts at once.
    let t = std::time::Instant::now();
    assert_eq!(send(BOT_ANA, "now").await.unwrap(), 200);
    assert!(
        t.elapsed() < Duration::from_millis(400),
        "her own bot waited"
    );
    // Her own bot holds one slot: with it busy, she is at her three, and its next call waits.
    let o = send(BOT_ANA, "slow 300");
    eventually("her own bot's slot runs", async || {
        load(&gw).await == (3, 3)
    })
    .await;
    let o2 = send(BOT_ANA, "now");
    eventually("her own bot's second waits", async || {
        load(&gw).await == (3, 4)
    })
    .await;
    // Someone else, through the same clients, takes the fourth GPU slot at once.
    let bo = ask(&gw, BOT_DOCS, "now", false)
        .header("x-pmos-user", "bo")
        .send()
        .await
        .unwrap();
    assert_eq!(bo.status(), 200);
    for h in [d, j, d2, r, r2, o, o2] {
        assert_eq!(h.await.unwrap(), 200);
    }
    assert_eq!(load(&gw).await, (0, 0));
}

/// The Low on the shipped configs' readiness (review at c78b5fb): each shipped config, stub
/// off, with its model servers moved to mocks (no network): `/readyz` reports each role by
/// its label, `ok` or `down`, and its class, and names none of the config's hosts, server
/// names or model ids. Before, only `deploy/gateway.yaml` with the stub on was checked.
#[tokio::test]
async fn readiness_of_every_shipped_config_reports_roles_by_label() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (path, class, label) in [
        ("deploy/gateway.yaml", "production", "local"),
        ("config/gateway.yaml", "test", "local"),
        ("config/gateway.claude-test.yaml", "test", "claude-test"),
    ] {
        let text = std::fs::read_to_string(root.join(path)).unwrap();
        // What the body must never carry: the config's hosts, server names and model ids.
        let original = Models::parse(&text, path.contains("claude-test")).unwrap();
        let mut forbidden: Vec<String> = vec!["http".into(), "://".into(), "127.0.0.1".into()];
        for role in original.roles.values() {
            forbidden.push(role.upstream_host());
            forbidden.push(role.model.clone());
        }
        // Each upstream the config names gets its own mock, so one can go away alone.
        let mut bases: Vec<&str> = original
            .roles
            .values()
            .map(|r| r.upstream.as_str())
            .collect();
        bases.sort();
        bases.dedup();
        let mut mocks = Vec::new();
        for _ in &bases {
            let app =
                Router::new().route("/v1/models", get(|| async { Json(json!({"data": []})) }));
            mocks.push(serve(app).await);
        }
        let mut models = shipped(path, "unused");
        for role in models.roles.values_mut() {
            let i = bases
                .iter()
                .position(|b| *b == original.roles[&role.name].upstream)
                .unwrap();
            role.upstream = format!("{}/v1", mocks[i].0);
        }
        assert_eq!(models.class.as_str(), class, "{path}");
        let (base, stop) = serve(router(GatewayConfig {
            models,
            clients: vec![Client {
                name: "runtime".into(),
                token: RUNTIME.into(),
            }],
            usage: Arc::new(UsageLog::new(None).unwrap()),
            stub: false,
            workarounds_on: true,
            anthropic_key: "limits-test-anthropic-key".into(),
            remote_key: String::new(),
            retry: Default::default(),
            timeouts: pmos_gateway::Timeouts {
                ready_cache: Duration::ZERO,
                ..Default::default()
            },
            model_source: ModelSource::Local,
        }))
        .await;
        let ready = async || {
            let res = reqwest::get(format!("{base}/readyz")).await.unwrap();
            let status = res.status().as_u16();
            let text = res.text().await.unwrap();
            for f in &forbidden {
                assert!(
                    !text.contains(f.as_str()),
                    "{path}: /readyz carries `{f}`: {text}"
                );
            }
            for (mock, _) in &mocks {
                let port = mock.rsplit(':').next().unwrap();
                assert!(
                    !text.contains(port),
                    "{path}: /readyz carries a port: {text}"
                );
            }
            (status, serde_json::from_str::<Value>(&text).unwrap())
        };
        let (status, v) = ready().await;
        assert_eq!(status, 200, "{path}: {v}");
        assert_eq!(
            (&v["class"], &v["stub"]),
            (&json!(class), &json!(false)),
            "{path}"
        );
        let want: serde_json::Map<String, Value> = original
            .roles
            .keys()
            .map(|r| (r.clone(), json!({"upstream": label, "status": "ok"})))
            .collect();
        assert_eq!(v["upstreams"], Value::Object(want), "{path}");
        assert_eq!(v["cloud_test"], label != "local", "{path}");
        // The first model server goes away: its roles are down, the others still ok. A
        // testing-only remote upstream's probe is reused for 30 seconds (slice H4), so that
        // config still reads ready from its cache.
        mocks[0].1.cancel();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (status, v) = ready().await;
        if label != "local" {
            assert_eq!(
                status, 200,
                "{path}: answered from the 30-second cache: {v}"
            );
            stop.cancel();
            for (_, s) in &mocks {
                s.cancel();
            }
            continue;
        }
        assert_eq!(status, 503, "{path}: {v}");
        for (name, role) in &original.roles {
            let want = if role.upstream == bases[0] {
                "down"
            } else {
                "ok"
            };
            assert_eq!(v["upstreams"][name]["status"], want, "{path}: {name}");
        }
        stop.cancel();
        for (_, s) in &mocks {
            s.cancel();
        }
    }
}

/// R1: a client `bot-pm-<handle>` (pattern `bot-pm-*`, `user: from-name`) may name only that
/// PM. Naming anyone else is `403`, and the usage log records the attempt; a team bot
/// (`bot-docs`) and the runtime still name anyone.
#[tokio::test]
async fn a_pms_own_bot_names_only_that_pm() {
    let gw = gateway(
        "{max-in-flight: 4, per-user: 2}",
        "{runtime: {trusted: true}, bot-pm-*: {trusted: true, user: from-name}, bot-*: {trusted: true}}",
    )
    .await;
    let send = |token: &'static str, user: Option<&'static str>| {
        let mut req = ask(&gw, token, "hi", false).header("x-pmos-run", "r1");
        if let Some(u) = user {
            req = req.header("x-pmos-user", u);
        }
        req.send()
    };
    assert_eq!(send(BOT_ANA, Some("ana")).await.unwrap().status(), 200);
    assert_eq!(send(BOT_ANA, None).await.unwrap().status(), 200);
    let res = send(BOT_ANA, Some("bo")).await.unwrap();
    assert_eq!(res.status(), 403);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["type"], "permission_error");
    // Nothing went to the model server for it.
    assert_eq!(gw.up.seen.lock().unwrap().len(), 2);
    // The team bot and the runtime name anyone.
    assert_eq!(send(BOT_DOCS, Some("bo")).await.unwrap().status(), 200);
    assert_eq!(send(RUNTIME, Some("bo")).await.unwrap().status(), 200);
    let events = gw.usage.recent();
    let refused: Vec<_> = events.iter().filter(|e| e.status == 403).collect();
    assert_eq!(refused.len(), 1, "{events:?}");
    let line = refused[0];
    assert_eq!(line.client, "bot-pm-ana");
    assert_eq!(line.user.as_deref(), Some("bo"), "whom it tried to name");
    assert_eq!(line.user_key.as_deref(), Some("bo"), "its person key");
    assert_eq!(line.attempts, 0);
    assert!(line.refused.is_some());
    let ok_for_ana = events
        .iter()
        .filter(|e| e.client == "bot-pm-ana" && e.status == 200)
        .count();
    assert_eq!(ok_for_ana, 2);
    // The Low on 403 lines (review at c78b5fb): a refusal naming a dotted handle keeps the
    // handle as sent in `user`, and gives its person key, the form every other line uses,
    // in `user_key`. Only a 403 line has the field. (Another client: bot-pm-ana's next
    // refusal within the minute is only counted, `UsageLog::record_refusal`.)
    let res = send(BOT_ANA_K, Some("bo.k")).await.unwrap();
    assert_eq!(res.status(), 403);
    let events = gw.usage.recent();
    let line = events.last().unwrap();
    assert_eq!(
        (line.status, line.user.as_deref(), line.user_key.as_deref()),
        (403, Some("bo.k"), Some("bo-k"))
    );
    let text = serde_json::to_string(line).unwrap();
    assert!(text.contains("\"user_key\":\"bo-k\""), "{text}");
    for e in events.iter().filter(|e| e.status != 403) {
        assert_eq!(e.user_key, None, "{e:?}");
        assert!(!serde_json::to_string(e).unwrap().contains("user_key"));
    }
}

/// R1: every valid handle gives a personal bot whose gateway client name (`bot-pm-<handle>`
/// as `personal_bot_name` writes it) is a valid client name, falls under the deploy config's
/// `bot-pm-*` rule, and may name that handle and no other.
#[test]
fn every_valid_handle_fits_a_client_name_that_names_only_it() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let m = Models::from_yaml(&std::fs::read_to_string(root.join("deploy/gateway.yaml")).unwrap())
        .unwrap();
    let mut handles: Vec<String> = [
        "ana",
        "a",
        "0",
        "ana.k",
        "ana_k",
        "a.b_c-d",
        "ana--k",
        "x1.y2",
        "a.",
        "a_",
        "a-",
        "ben.de-la-cruz",
        "9lives",
        "pm",
        "bot",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    // Long ones: up to the 64 characters a handle may have, with every allowed character.
    for n in [24, 28, 29, 30, 31, 32, 33, 40, 63, 64] {
        handles.push("x".repeat(n));
        handles.push(
            format!("a{}", ".b_c-".repeat(n / 5))
                .chars()
                .take(n)
                .collect(),
        );
    }
    // Every one- and two-character handle.
    let first: Vec<char> = ('a'..='z').chain('0'..='9').collect();
    let rest: Vec<char> = first.iter().copied().chain(['.', '_', '-']).collect();
    for a in &first {
        handles.push(a.to_string());
        for b in &rest {
            handles.push(format!("{a}{b}"));
        }
    }
    let token = |i: usize| format!("{i:0>40}");
    for (i, h) in handles.iter().enumerate() {
        assert!(
            pmos_contract::model::valid_handle(h),
            "{h} should be a handle"
        );
        let client = format!("bot-{}", pmos_contract::onboarding::personal_bot_name(h));
        assert!(valid_client_name(&client), "{h}: `{client}`");
        // PMOS_GATEWAY_TOKENS takes it.
        let cfg = Config::from_lookup(|k| match k {
            "PMOS_GATEWAY_TOKENS" => Some(format!("{client}={}", token(i))),
            "PMOS_MODEL_SOURCE" => Some("local".into()),
            _ => None,
        });
        assert!(cfg.is_ok(), "{h}: {:?}", cfg.err());
        let p = m.client_policy(&client);
        assert!(p.trusted, "{client}");
        assert!(
            matches!(p.user, UserRule::FromName { .. }),
            "{client} falls under bot-pm-*"
        );
        // F2 (review at 2544d01): a handle whose spelled form doesn't fit a personal bot's
        // name is cut there, so that client name would spell other handles too: refused.
        let fits = spelled_handle(h).len() <= MAX_SPELLED_HANDLE;
        assert_eq!(p.may_name(&client, h), fits, "{client} naming {h}");
        if fits {
            // The one key it is kept under is the form its name spells.
            assert_eq!(person_key(h), spelled_handle(h));
            assert_eq!(format!("bot-pm-{}", person_key(h)), client);
        }
        for other in ["zz-someone-else", "ana-x"] {
            if handle_in_name(other) != handle_in_name(h) {
                assert!(!p.may_name(&client, other), "{client} named {other}");
            }
        }
    }
    // A client that is only the prefix names no one.
    assert!(!m.client_policy("bot-pm-").may_name("bot-pm-", "ana"));
}

/// F2 (review at 2544d01): `bot-pm-<29 characters>` names only the handle it spells in full,
/// never one that is longer and starts the same way (whose bot name would be cut to it).
#[test]
fn a_cut_bot_name_names_no_one_longer() {
    let m = Models::from_yaml(
        &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/gateway.yaml"),
        )
        .unwrap(),
    )
    .unwrap();
    let stem = "a".repeat(MAX_SPELLED_HANDLE);
    let client = format!("bot-pm-{stem}");
    assert!(valid_client_name(&client));
    let p = m.client_policy(&client);
    assert!(p.may_name(&client, &stem), "the handle it spells in full");
    for longer in [
        format!("{stem}b"),
        format!("{stem}.b"),
        format!("{stem}-zz"),
        format!("{stem}{}", "c".repeat(30)),
    ] {
        assert_eq!(
            format!(
                "bot-{}",
                pmos_contract::onboarding::personal_bot_name(&longer)
            ),
            client,
            "{longer} shares the cut bot name"
        );
        assert!(!p.may_name(&client, &longer), "{client} named {longer}");
    }
}

/// The Low on canonical handles (review at 2544d01): `bot-pm-ana` may name `ana`, `ana.`,
/// `ana-` or `ana..`, but its calls are kept under one person, `ana`, for the caps and the
/// usage log; and a name that only looks like `bot-pm-*` falls under no trusted pattern.
#[tokio::test]
async fn a_pms_own_bot_is_kept_under_one_person_key() {
    let gw = gateway(
        "{max-in-flight: 4, per-user: 2}",
        "{bot-pm-*: {trusted: true, user: from-name}, bot-*: {trusted: true}}",
    )
    .await;
    for named in ["ana", "ana.", "ana-", "ana..", "ana_"] {
        let res = ask(&gw, BOT_ANA, "hi", false)
            .header("x-pmos-user", named)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200, "{named}");
    }
    let users: std::collections::BTreeSet<_> = gw
        .usage
        .recent()
        .into_iter()
        .filter(|e| e.client == "bot-pm-ana")
        .map(|e| e.user)
        .collect();
    assert_eq!(users, [Some("ana".to_string())].into());
    // `bot-pm` and `bot-pmana` look like `bot-pm-*` but aren't: not trusted, not `bot-*`.
    let m = Models::from_yaml(
        "upstreams: {a: 'http://vllm/v1'}\nroles: {default: {upstream: a, model: m}}\n\
         clients: {bot-pm-*: {trusted: true, user: from-name}, bot-*: {trusted: true}, bot-pmx: {trusted: true}}\n",
    )
    .unwrap();
    for name in ["bot-pm", "bot-pmana", "bot-pm0"] {
        let p = m.client_policy(name);
        assert!(!p.trusted, "{name}");
        assert_eq!(m.lookalike_of(name), Some("bot-pm-*"));
        assert_eq!(p.priority, Priority::Personal);
        let c = Client {
            name: name.into(),
            token: "t".repeat(32),
        };
        let warnings = m.lookalike_warnings(&[c]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("not trusted"), "{warnings:?}");
    }
    // An exact entry is what it says; real team bots and PMs' bots are untouched.
    assert!(m.client_policy("bot-pmx").trusted);
    assert_eq!(m.lookalike_of("bot-pmx"), None);
    for name in ["bot-docs", "bot-pm-ana", "bot-p", "runtime"] {
        assert_eq!(m.lookalike_of(name), None, "{name}");
    }
    assert!(m.client_policy("bot-docs").trusted);
    let ok = ["bot-docs", "bot-pm-ana"].map(|n| Client {
        name: n.into(),
        token: "t".repeat(32),
    });
    assert!(m.lookalike_warnings(&ok).is_empty());
}

/// R2: one client can't fill the queue and so turn every other client away; its own excess
/// is `429` with `Retry-After` (and a usage line), while another client still queues.
#[tokio::test]
async fn one_client_cant_take_every_place_in_the_queue() {
    let gw = Arc::new(
        gateway(
            "{max-in-flight: 1, per-user: 4, max-queue: 6}",
            "{evals: {max-in-flight: 1, max-queue: 2}, runtime: {trusted: true, max-in-flight: 1}}",
        )
        .await,
    );
    let send = |token: &'static str, text: &'static str| {
        let gw = gw.clone();
        tokio::spawn(async move {
            let res = ask(&gw, token, text, false).send().await.unwrap();
            let retry = res
                .headers()
                .get("retry-after")
                .map(|v| v.to_str().unwrap().to_string());
            (res.status().as_u16(), retry)
        })
    };
    // One call runs; two of the same client's wait; its fourth may not.
    let running = send(EVALS, "slow 400");
    eventually("one runs", async || load(&gw).await == (1, 0)).await;
    let waiting: Vec<_> = (0..2).map(|_| send(EVALS, "slow 10")).collect();
    eventually("two wait", async || load(&gw).await == (1, 2)).await;
    assert_eq!(
        send(EVALS, "slow 10").await.unwrap(),
        (429, Some("5".to_string()))
    );
    // Another client's calls still get places, though the first client asked first.
    let others: Vec<_> = (0..2).map(|_| send(RUNTIME, "slow 10")).collect();
    eventually("four wait", async || load(&gw).await == (1, 4)).await;
    assert_eq!(running.await.unwrap().0, 200);
    for h in waiting.into_iter().chain(others) {
        assert_eq!(h.await.unwrap().0, 200);
    }
    let refused: Vec<_> = gw
        .usage
        .recent()
        .into_iter()
        .filter(|e| e.status == 429)
        .collect();
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0].client, "evals");
    assert!(refused[0].refused.is_some());
}

/// The Low on client patterns: a bare `*`, or one so short it covers nearly every client, is
/// refused, and so are `user` rules that can't mean anything; and the class.
#[test]
fn client_patterns_and_user_rules_that_cover_too_much_are_refused() {
    let with = |extra: &str| {
        Models::parse(
            &format!(
                "upstreams: {{a: 'http://vllm/v1', c: 'anthropic+https://api.anthropic.com'}}\n\
                 roles: {{default: {{upstream: a, model: m}}}}\n{extra}"
            ),
            true,
        )
    };
    for (extra, want) in [
        ("clients: {'*': {trusted: true}}", "at least 4 characters"),
        ("clients: {'b*': {trusted: true}}", "at least 4 characters"),
        ("clients: {'bot*': {}}", "at least 4 characters"),
        (
            "clients: {runtime: {trusted: true, user: from-name}}",
            "pattern",
        ),
        ("clients: {bot-pm-*: {user: from-name}}", "trusted: true"),
        (
            "clients: {bot-pm-*: {trusted: true, user: someone}}",
            "any` or `from-name",
        ),
        ("clients: {evals: {max-queue: 0}}", "at least 1"),
        ("limits: {per-user-queue: 0}", "at least 1"),
        ("class: staging", "production` or `test"),
    ] {
        let err = with(extra).unwrap_err();
        assert!(err.contains(want), "{extra}: {err}");
    }
    // The shortest pattern allowed, and the rules that are.
    let m = with("clients: {bot-*: {trusted: true}, bot-pm-*: {trusted: true, user: from-name, max-queue: 9}}")
        .unwrap();
    assert_eq!(m.client_policy("bot-pm-ana").max_queue, 9);
    assert_eq!(m.class, GatewayClass::Test);
    // A production gateway never serves a testing-only upstream.
    let err = Models::parse(
        "upstreams: {c: 'anthropic+https://api.anthropic.com'}\n\
         roles: {default: {upstream: c, model: m}}\nclass: production\n",
        true,
    )
    .unwrap_err();
    assert!(err.contains("testing-only"), "{err}");
    // With the stub on, a production config starts but serves as `test`, with a warning; the
    // stub is never a production gateway (and a first install runs exactly that).
    let prod = Models::from_yaml(
        "upstreams: {a: 'http://vllm-default.pmos.internal/v1'}\nroles: {default: {upstream: a, model: m}}\nclass: production\n",
    )
    .unwrap();
    let config = |stub: &'static str| {
        Config::from_lookup(move |k| match k {
            "PMOS_GATEWAY_TOKENS" => Some(format!("runtime={}", "a".repeat(32))),
            "PMOS_MODEL_SOURCE" => Some("local".into()),
            "PMOS_GATEWAY_STUB" => Some(stub.into()),
            _ => None,
        })
        .unwrap()
    };
    let (class, warning) = config("1").served_class(&prod);
    assert_eq!(class, GatewayClass::Test);
    assert!(warning.is_some_and(|w| w.contains("class `test`")));
    assert_eq!(
        config("0").served_class(&prod),
        (GatewayClass::Production, None)
    );
    assert_eq!(
        config("1").served_class(&with("").unwrap()),
        (GatewayClass::Test, None)
    );
}

/// F3 (review at 2544d01): a `class: production` gateway names its model servers
/// `<name>.pmos.internal` only: no address, bare name or `local-hosts` entry, though a test
/// gateway accepts all three.
#[test]
fn a_production_gateway_names_its_model_servers_under_pmos_internal() {
    let config = |upstream: &str, extra: &str, class: &str| {
        Models::from_yaml(&format!(
            "upstreams: {{a: '{upstream}'}}\nroles: {{default: {{upstream: a, model: m}}}}\n{extra}class: {class}\n"
        ))
    };
    for upstream in [
        "http://vllm/v1",
        "http://localhost:8000/v1",
        "http://127.0.0.1:8001/v1",
        "http://10.0.0.5:8000/v1",
        "http://[::1]:8000/v1",
        "http://gpu.corp.example/v1",
        "http://pmos.internal/v1",
        "http://.pmos.internal/v1",
        "http://x.pmos.internal.example/v1",
        "http://-x.pmos.internal/v1",
    ] {
        let err = config(upstream, "", "production").err();
        assert!(
            err.as_deref().is_some_and(|e| e.contains("pmos.internal")
                || e.contains("only to local models")
                || e.contains("upstream `a`")),
            "{upstream}: {err:?}"
        );
    }
    // A `local-hosts` entry is refused outright, used or not.
    let err = config(
        "http://vllm-default.pmos.internal:8000/v1",
        "local-hosts: [spark.corp.example]\n",
        "production",
    )
    .unwrap_err();
    assert!(err.contains("local-hosts"), "{err}");
    let err = config(
        "http://spark.corp.example/v1",
        "local-hosts: [spark.corp.example]\n",
        "production",
    )
    .unwrap_err();
    assert!(err.contains("local-hosts"), "{err}");
    // The two pinned model servers (slice H4), over http or https; any other name under
    // `pmos.internal` is refused, however it is written.
    for upstream in [
        "http://vllm-default.pmos.internal:8000/v1",
        "https://vllm-extraction.pmos.internal/v1",
    ] {
        assert!(config(upstream, "", "production").is_ok(), "{upstream}");
    }
    for upstream in [
        "https://gpu.models.pmos.internal/v1",
        "http://vllm.pmos.internal/v1",
        "http://toolserver.pmos.internal:8710/v1",
    ] {
        let err = config(upstream, "", "production").unwrap_err();
        assert!(err.contains("pinned"), "{upstream}: {err}");
    }
    // A test gateway keeps accepting the local forms.
    for upstream in ["http://vllm/v1", "http://127.0.0.1:8001/v1"] {
        assert!(config(upstream, "", "test").is_ok(), "{upstream}");
    }
    assert!(config(
        "http://spark.corp.example/v1",
        "local-hosts: [spark.corp.example]\n",
        "test"
    )
    .is_ok());
    // The shipped deploy config passes.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let m = Models::from_yaml(&std::fs::read_to_string(root.join("deploy/gateway.yaml")).unwrap())
        .unwrap();
    assert_eq!(m.class, GatewayClass::Production);
    for r in m.roles.values() {
        assert!(r.upstream_host().ends_with(".pmos.internal"), "{}", r.name);
    }
}

/// The regression at 2544d01 (CI's deploy kit): a first install runs the Spark's own
/// `class: production` config with the stub on. The gateway starts and reports class `test`
/// with `stub: true`; with the stub off the same config reports `production`.
#[tokio::test]
async fn the_deploy_config_serves_as_test_with_the_stub_and_production_without() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let text = std::fs::read_to_string(root.join("deploy/gateway.yaml")).unwrap();
    for (stub, class) in [(true, "test"), (false, "production")] {
        let models = Models::parse(&text, false).unwrap();
        let config = Config::from_lookup(|k| match k {
            "PMOS_GATEWAY_TOKENS" => Some(format!("runtime={RUNTIME}")),
            "PMOS_MODEL_SOURCE" => Some("local".into()),
            "PMOS_GATEWAY_STUB" => Some(if stub { "1" } else { "0" }.into()),
            _ => None,
        })
        .unwrap();
        config.check_keys(&models).unwrap();
        assert!(models.lookalike_warnings(&config.clients).is_empty());
        let (base, stop) = serve(router(GatewayConfig {
            models,
            clients: config.clients.clone(),
            usage: Arc::new(UsageLog::new(None).unwrap()),
            stub: config.stub,
            workarounds_on: true,
            anthropic_key: String::new(),
            remote_key: String::new(),
            retry: Default::default(),
            timeouts: Default::default(),
            model_source: ModelSource::Local,
        }))
        .await;
        let v: Value = http()
            .get(format!("{base}/v1/pmos/roles"))
            .bearer_auth(RUNTIME)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(v["class"], class, "stub {stub}");
        assert_eq!(v["stub"], stub);
        if stub {
            // And it answers, with no model server behind it.
            let res = http_as("stub")
                .post(format!("{base}/v1/chat/completions"))
                .bearer_auth(RUNTIME)
                .json(&json!({"model": "pmos/default",
                    "messages": [{"role": "user", "content": "hi"}]}))
                .send()
                .await
                .unwrap();
            assert_eq!(res.status(), 200);
            // G2 (review at 6bb806f): readiness says so too, so `pmos-admin status` can show
            // it; it names no address. (With the stub off, readiness asks the model servers,
            // which aren't here; `a_production_gateway_says_its_class_when_ready` covers it.)
            let ready = reqwest::get(format!("{base}/readyz")).await.unwrap();
            assert_eq!(ready.status(), 200, "the stub answers, so it is ready");
            let ready: Value = ready.json().await.unwrap();
            assert_eq!(
                (&ready["class"], &ready["stub"]),
                (&json!("test"), &json!(true))
            );
            assert!(!ready.to_string().contains("pmos.internal"), "{ready}");
        }
        stop.cancel();
    }
}

/// G2 (review at 6bb806f): `/readyz` says the class and that the stub is off, without an
/// address or a key, for a production gateway and a test one alike.
#[tokio::test]
async fn a_production_gateway_says_its_class_when_ready() {
    // A test gateway in front of a model server that answers: ready, class `test`.
    let gw = gateway("{}", "{}").await;
    let v: Value = reqwest::get(format!("{}/readyz", gw.base))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["ready"], true);
    assert_eq!((&v["class"], &v["stub"]), (&json!("test"), &json!(false)));
    // Labels by role, never the model server's address (the route needs no token).
    assert_eq!(
        v["upstreams"],
        json!({"default": {"upstream": "local", "status": "ok"},
               "extraction": {"upstream": "local", "status": "ok"}})
    );
    let host = gw.upstream.trim_start_matches("http://");
    assert!(!v.to_string().contains(host), "{v}");
    assert!(!v.to_string().contains("127.0.0.1"), "{v}");
    drop(gw);
    // A production one (its hosts are `<name>.pmos.internal`, which don't resolve here, so
    // the class is set after parsing a local config): nothing listens on port 9, so it isn't
    // ready, and still says what it is.
    let mut models = Models::from_yaml(
        "upstreams: {a: 'http://127.0.0.1:9/v1'}\nroles: {default: {upstream: a, model: m}}\n",
    )
    .unwrap();
    models.class = GatewayClass::Production;
    let (base, stop) = serve(router(GatewayConfig {
        models,
        clients: vec![Client {
            name: "runtime".into(),
            token: RUNTIME.into(),
        }],
        usage: Arc::new(UsageLog::new(None).unwrap()),
        stub: false,
        workarounds_on: true,
        anthropic_key: String::new(),
        remote_key: String::new(),
        retry: Default::default(),
        timeouts: Default::default(),
        model_source: ModelSource::Local,
    }))
    .await;
    let res = reqwest::get(format!("{base}/readyz")).await.unwrap();
    assert_eq!(res.status(), 503);
    let v: Value = res.json().await.unwrap();
    assert_eq!(
        (&v["class"], &v["stub"]),
        (&json!("production"), &json!(false))
    );
    // The one that doesn't answer is named by its role, not its address.
    assert_eq!(v["upstreams"]["default"]["status"], "down");
    assert!(!v.to_string().contains("127.0.0.1"), "{v}");
    stop.cancel();
}

/// G1 (review at 6bb806f): one person's running cap (`per-user`) holds across clients for a
/// handle with `.` or `_`. Before, the runtime and team bots kept `ana.k` and `bot-pm-ana-k`
/// kept `ana-k`, so Ana had `per-user` slots under each key and her usage was split.
#[tokio::test]
async fn a_persons_running_cap_holds_across_clients_for_a_dotted_handle() {
    let gw = Arc::new(
        gateway(
            "{max-in-flight: 8, per-user: 2}",
            // No `per-user-share: 2` for runtime and bot-* (it was here before U1): beside a
            // `from-name` client they may hold only `per-user - 1` of her slots together, and
            // the config is refused otherwise. The cap across spellings is what's tested.
            "{runtime: {trusted: true, max-in-flight: 8}, \
              bot-pm-*: {trusted: true, max-in-flight: 8, user: from-name}, \
              bot-*: {trusted: true, max-in-flight: 8}}",
        )
        .await,
    );
    let send = |token: &'static str, user: &'static str| {
        let gw = gw.clone();
        tokio::spawn(async move {
            ask(&gw, token, "slow 300", false)
                .header("x-pmos-user", user)
                .send()
                .await
                .unwrap()
                .status()
        })
    };
    // The runtime names `ana.k`, her own bot `ana-k`: two of her two slots.
    let a = send(RUNTIME, "ana.k");
    let b = send(BOT_ANA_K, "ana.k");
    eventually("both run", async || load(&gw).await == (2, 0)).await;
    // A third call for her waits, through any client and in any spelling...
    let c = send(BOT_DOCS, "ana_k");
    let d = send(RUNTIME, "ana-k");
    eventually("two wait", async || load(&gw).await == (2, 2)).await;
    // ...while someone else's starts at once.
    assert_eq!(send(RUNTIME, "bo").await.unwrap(), 200);
    for h in [a, b, c, d] {
        assert_eq!(h.await.unwrap(), 200);
    }
    // Her usage is under one name, whichever client made the call.
    let users: std::collections::BTreeSet<_> = gw
        .usage
        .recent()
        .into_iter()
        .filter(|e| e.user.as_deref() != Some("bo"))
        .map(|e| (e.user, e.status))
        .collect();
    assert_eq!(users, [(Some("ana-k".to_string()), 200)].into());
}

/// G3 (review at 6bb806f): a client that may name anyone (`user: any`, such as a team bot's
/// token) holds at most its `per-user-share` of one person's running slots, 1 by default;
/// dispatch is oldest-first, so before, two of the team bot's calls could take both. (Two
/// *different* such clients, with the shipped person pools, are W1's test,
/// `team_bots_cant_hold_up_the_runtime_and_neither_holds_up_her_own_bot`.)
#[tokio::test]
async fn a_team_bot_holds_at_most_its_share_of_a_pms_slots() {
    let gw = Arc::new(
        gateway(
            "{max-in-flight: 4, per-user: 2}",
            "{runtime: {trusted: true}, bot-pm-*: {trusted: true, max-in-flight: 3, user: from-name}, \
              bot-*: {trusted: true, max-in-flight: 3}}",
        )
        .await,
    );
    let send = |token: &'static str, text: &'static str| {
        let gw = gw.clone();
        tokio::spawn(async move {
            ask(&gw, token, text, false)
                .header("x-pmos-user", "ana")
                .send()
                .await
                .unwrap()
                .status()
        })
    };
    let docs: Vec<_> = (0..3).map(|_| send(BOT_DOCS, "slow 400")).collect();
    eventually("one runs, two wait", async || load(&gw).await == (1, 2)).await;
    // Ana's own bot gets her other slot at once, though bot-docs asked first.
    let t = std::time::Instant::now();
    assert_eq!(send(BOT_ANA, "now").await.unwrap(), 200);
    assert!(
        t.elapsed() < Duration::from_millis(300),
        "her own bot waited"
    );
    // Her own bot may hold both of her slots; one of them is still bot-docs'.
    let own: Vec<_> = (0..2).map(|_| send(BOT_ANA, "slow 300")).collect();
    eventually("her bot holds her second slot", async || {
        load(&gw).await == (2, 3)
    })
    .await;
    for h in docs.into_iter().chain(own) {
        assert_eq!(h.await.unwrap(), 200);
    }
    // The share is configurable, up to `per-user`, and only for a trusted client.
    let m = |clients: &str| {
        Models::from_yaml(&format!(
            "upstreams: {{a: 'http://vllm/v1'}}\nroles: {{default: {{upstream: a, model: m}}}}\n\
             limits: {{per-user: 2}}\nclients: {clients}\n"
        ))
    };
    // Without a `from-name` client, a `user: any` client may hold all of `per-user`.
    let ok = m("{bot-*: {trusted: true, per-user-share: 2}, runtime: {trusted: true}}").unwrap();
    assert_eq!(ok.client_policy("bot-docs").per_user_share, 2);
    assert_eq!(ok.pools, [("user-any".to_string(), 2)]);
    assert_eq!(
        ok.client_policy("runtime").per_user_share,
        1,
        "user: any: 1 by default"
    );
    // U1: beside a `from-name` client, one slot of each person's is kept for their own bot,
    // so a `user: any` share of all of `per-user` is refused (it was accepted before U1)...
    let err = m("{bot-*: {trusted: true, per-user-share: 2}, bot-pm-*: {trusted: true, user: from-name}, runtime: {trusted: true}}").unwrap_err();
    assert!(err.contains("per-user-share must be 1 to 1"), "{err}");
    assert!(err.contains("own bot"), "{err}");
    // ...and with `per-user` 3, a share of 2 is fine; the clients that may name anyone hold
    // 2 of a person's slots together, and her own bot may hold all 3.
    let ok = Models::from_yaml(
        "upstreams: {a: 'http://vllm/v1'}\nroles: {default: {upstream: a, model: m}}\n\
         limits: {per-user: 3}\nclients: {bot-*: {trusted: true, per-user-share: 2}, \
         bot-pm-*: {trusted: true, user: from-name}, runtime: {trusted: true}}\n",
    )
    .unwrap();
    assert_eq!(ok.pools, [("user-any".to_string(), 2)]);
    assert_eq!(ok.client_policy("bot-docs").per_user_share, 2);
    assert_eq!(ok.client_policy("bot-docs").pool.map(|p| p.cap), Some(2));
    assert_eq!(
        ok.client_policy("bot-pm-ana").per_user_share,
        3,
        "from-name: its own PM"
    );
    assert_eq!(ok.client_policy("bot-pm-ana").pool, None);
    // A `from-name` client needs a slot to keep: `per-user` 1 is refused beside one.
    let err = Models::from_yaml(
        "upstreams: {a: 'http://vllm/v1'}\nroles: {default: {upstream: a, model: m}}\n\
         limits: {per-user: 1}\nclients: {bot-pm-*: {trusted: true, user: from-name}}\n",
    )
    .unwrap_err();
    assert!(err.contains("at least 2"), "{err}");
    for (clients, want) in [
        (
            "{bot-*: {trusted: true, per-user-share: 0}}",
            "1 to limits.per-user",
        ),
        (
            "{bot-*: {trusted: true, per-user-share: 3}}",
            "1 to limits.per-user",
        ),
        ("{evals: {per-user-share: 1}}", "trusted: true"),
    ] {
        let err = m(clients).unwrap_err();
        assert!(err.contains(want), "{clients}: {err}");
    }
}

/// G3 and the test stack's eval mode: `scripts/claude-test-stack.sh` with `PMOS_TEST_EVALS=1`
/// runs every attempt as the one user `evals` through its one trusted `worker` token, one
/// call per lane. Its gateway config (the heredoc in the script, read here as written) gives
/// `worker` the whole of that person's `per-user`, so 4 lanes still have 4 calls in flight,
/// and the judges' `evals` token its 2 beside them.
#[tokio::test]
async fn the_test_stacks_eval_lanes_keep_one_call_in_flight_each() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = std::fs::read_to_string(root.join("scripts/claude-test-stack.sh")).unwrap();
    // The script's rule: more than two lanes raise per-user to the lanes, in-flight to that + 2.
    assert!(script.contains("GW_PER_USER=$EVALS_PARALLEL GW_IN_FLIGHT=$((EVALS_PARALLEL + 2))"));
    let start = script.find("cat >\"$GATEWAY_CONFIG\" <<EOF\n").unwrap();
    let body = &script[start..];
    let body = &body[body.find('\n').unwrap() + 1..body.find("\nEOF\n").unwrap()];
    // The config as the shell writes it, for a model server at `up`.
    let render = |up: &str, per_user: usize, in_flight: usize| {
        let text = body
            .replace("${PMOS_TEST_MODEL_URL%/}", &format!("{up}/v1"))
            .replace("$FAST_MODEL", "m-fast")
            .replace("$FAST_LABEL", "label-fast")
            .replace("$MODEL_LABEL", "label-a")
            .replace("$PMOS_TEST_MODEL", "m")
            .replace("$GW_IN_FLIGHT", &in_flight.to_string())
            .replace("$GW_PER_USER", &per_user.to_string())
            .replace("\\`", "`");
        assert!(!text.contains('$'), "{text}");
        text
    };
    for (lanes, per_user, in_flight) in [(4usize, 4usize, 6usize), (2, 2, 4)] {
        let m = Models::parse(&render("http://127.0.0.1:9", per_user, in_flight), true).unwrap();
        // Every role is reported by its label (PMOS_TEST_MODEL_LABEL), never the model id.
        for (role, model, label) in [
            ("default", "m", "label-a"),
            ("reasoning", "m", "label-a"),
            ("extraction", "m-fast", "label-fast"),
        ] {
            let r = &m.roles[role];
            assert_eq!(
                (r.model.as_str(), r.reported_model()),
                (model, label),
                "{role}"
            );
        }
        for c in ["runtime", "worker"] {
            assert_eq!(
                m.client_policy(c).per_user_share,
                per_user,
                "{c}'s share of `evals` is all of per-user"
            );
        }
        let gw = Arc::new(gateway_from(|up| render(up, per_user, in_flight), true).await);
        let send = |token: &'static str, user: Option<&'static str>| {
            let gw = gw.clone();
            tokio::spawn(async move {
                let mut req = ask(&gw, token, "slow 400", false);
                if let Some(u) = user {
                    req = req.header("x-pmos-user", u);
                }
                req.send().await.unwrap().status()
            })
        };
        let mut calls: Vec<_> = (0..lanes).map(|_| send(WORKER, Some("evals"))).collect();
        calls.extend((0..2).map(|_| send(EVALS, None)));
        eventually("every lane and judge runs", async || {
            load(&gw).await == ((lanes + 2) as u64, 0)
        })
        .await;
        for h in calls {
            assert_eq!(h.await.unwrap(), 200);
        }
    }
    // The Claude test config the script uses otherwise keeps two calls per person, as before.
    let text = std::fs::read_to_string(root.join("config/gateway.claude-test.yaml")).unwrap();
    let m = Models::parse(&text, true).unwrap();
    for c in ["runtime", "worker"] {
        assert_eq!(m.client_policy(c).per_user_share, m.limits.per_user, "{c}");
    }
}

/// W1 (review at 52dbae2): `person-pools:` and `person-pool:` are checked at start-up, so a
/// config can't leave a pool's slot or a PM's own bot's slot unkept by mistake; a config
/// without them behaves as before (one pool for every client that may name anyone).
#[test]
fn person_pools_are_checked_at_start_up() {
    let m = |rest: &str| {
        Models::from_yaml(&format!(
            "upstreams: {{a: 'http://vllm/v1'}}\nroles: {{default: {{upstream: a, model: m}}}}\n{rest}"
        ))
    };
    let own = "bot-pm-*: {trusted: true, user: from-name}";
    // The production shape, and the same pools with no own bot: then nothing is kept for one.
    let ok = m(&format!(
        "limits: {{per-user: 3}}\nperson-pools: {{runtime: 1, team-bots: 1}}\n\
         clients: {{runtime: {{trusted: true, person-pool: runtime}}, \
         bot-*: {{trusted: true, person-pool: team-bots}}, {own}}}\n"
    ))
    .unwrap();
    assert_eq!(ok.own_bot_slots, Some(1));
    assert_eq!(ok.client_policy("bot-pm-ana").per_user_share, 1);
    let ok = m(
        "limits: {per-user: 3}\nperson-pools: {runtime: 1, team-bots: 2}\n\
         clients: {runtime: {trusted: true, person-pool: runtime}, \
         bot-*: {trusted: true, person-pool: team-bots, per-user-share: 2}}\n",
    )
    .unwrap();
    assert_eq!(ok.client_policy("bot-docs").pool.map(|p| p.cap), Some(2));
    // Without pools: as before (one implicit pool of `per-user - 1` beside an own bot).
    let ok = m(&format!(
        "limits: {{per-user: 2}}\nclients: {{runtime: {{trusted: true}}, {own}}}\n"
    ))
    .unwrap();
    assert_eq!(ok.pools, [("user-any".to_string(), 1)]);
    assert_eq!(ok.own_bot_slots, None);
    assert_eq!(ok.client_policy("bot-pm-ana").per_user_share, 2);
    let cases: Vec<(String, &str)> = vec![
        // The pools and the own bot's slot need more than `per-user`.
        (
            format!(
                "limits: {{per-user: 2}}\nperson-pools: {{runtime: 1, team-bots: 1}}\n\
                 clients: {{runtime: {{trusted: true, person-pool: runtime}}, \
                 bot-*: {{trusted: true, person-pool: team-bots}}, {own}}}\n"
            ),
            "more than limits.per-user (2)",
        ),
        // A pool of no slots.
        (
            "limits: {per-user: 3}\nperson-pools: {runtime: 0}\n\
             clients: {runtime: {trusted: true, person-pool: runtime}}\n"
                .into(),
            "at least 1 slot",
        ),
        // A client that may name anyone, in no pool, when pools are given.
        (
            "limits: {per-user: 3}\nperson-pools: {runtime: 1}\n\
             clients: {runtime: {trusted: true, person-pool: runtime}, bot-*: {trusted: true}}\n"
                .into(),
            "must say which pool",
        ),
        // A pool no client is in (a misspelt name, say).
        (
            "limits: {per-user: 3}\nperson-pools: {runtime: 1, team-bot: 1}\n\
             clients: {runtime: {trusted: true, person-pool: runtime}}\n"
                .into(),
            "person pool `team-bot` has no client",
        ),
        // A pool that isn't listed, or a pool without `person-pools:`.
        (
            "limits: {per-user: 3}\nperson-pools: {runtime: 1}\n\
             clients: {runtime: {trusted: true, person-pool: runtimes}}\n"
                .into(),
            "isn't listed",
        ),
        (
            "clients: {runtime: {trusted: true, person-pool: runtime}}\n".into(),
            "isn't listed",
        ),
        // A pool for a client that names no one, or only its own PM.
        (
            "limits: {per-user: 3}\nperson-pools: {runtime: 1}\n\
             clients: {runtime: {trusted: true, person-pool: runtime}, \
             evals: {person-pool: runtime}}\n"
                .into(),
            "is for a trusted client that may name anyone",
        ),
        (
            "limits: {per-user: 3}\nperson-pools: {runtime: 1}\n\
             clients: {runtime: {trusted: true, person-pool: runtime}, \
             bot-pm-*: {trusted: true, user: from-name, person-pool: runtime}}\n"
                .into(),
            "is for a trusted client that may name anyone",
        ),
        // A share above the pool's slots, or an own bot's above what the pools leave.
        (
            "limits: {per-user: 3}\nperson-pools: {runtime: 1}\n\
             clients: {runtime: {trusted: true, person-pool: runtime, per-user-share: 2}}\n"
                .into(),
            "per-user-share must be 1 to 1, the slots of its person pool `runtime`",
        ),
        (
            "limits: {per-user: 3}\nperson-pools: {runtime: 1, team-bots: 1}\n\
             clients: {runtime: {trusted: true, person-pool: runtime}, \
             bot-*: {trusted: true, person-pool: team-bots}, \
             bot-pm-*: {trusted: true, user: from-name, per-user-share: 2}}\n"
                .into(),
            "what the person pools leave",
        ),
        // A pool name that isn't a plain name.
        (
            "limits: {per-user: 3}\nperson-pools: {Runtime: 1}\n\
             clients: {runtime: {trusted: true, person-pool: Runtime}}\n"
                .into(),
            "lowercase letters",
        ),
    ];
    for (rest, want) in cases {
        let err = m(&rest).unwrap_err();
        assert!(err.contains(want), "{rest}\nwant `{want}`: {err}");
    }
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
    reqwest::Client::builder()
        .default_headers(h)
        .build()
        .unwrap()
}
