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
use pmos_gateway::config::{
    handle_in_name, spelled_handle, valid_client_name, Client, Config, GatewayClass, Models,
    UserRule, MAX_SPELLED_HANDLE,
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
    let up = Arc::new(Upstream::default());
    let app = Router::new()
        .route("/v1/models", get(|| async { Json(json!({"data": []})) }))
        .route("/v1/chat/completions", post(mock))
        .with_state(up.clone());
    let (upstream, stop_up) = serve(app).await;
    let models = Models::from_yaml(&format!(
        "upstreams: {{a: '{upstream}/v1'}}\n\
         roles:\n  default: {{upstream: a, model: qwen-test}}\n  extraction: {{upstream: a, model: nemo-test}}\n\
         limits: {limits}\nclients: {clients}\n"
    ))
    .unwrap();
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
        ],
        usage: usage.clone(),
        stub: false,
        workarounds_on: true,
        anthropic_key: String::new(),
        remote_key: String::new(),
        retry: Default::default(),
        timeouts: Default::default(),
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
    reqwest::Client::new()
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
    let v: Value = reqwest::Client::new()
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
    // R2: waiting is capped per client too, at twice its slots by default.
    assert_eq!(own.max_queue, 2 * own.max_in_flight);
    assert_eq!(m.limits.person_queue(), 2 * m.limits.per_user);
    // R3: the Spark's gateway is the production one.
    assert_eq!(m.class, GatewayClass::Production);
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
    assert_eq!(line.attempts, 0);
    assert!(line.refused.is_some());
    let ok_for_ana = events
        .iter()
        .filter(|e| e.client == "bot-pm-ana" && e.status == 200)
        .count();
    assert_eq!(ok_for_ana, 2);
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
        let cfg = Config::from_lookup(|k| {
            (k == "PMOS_GATEWAY_TOKENS").then(|| format!("{client}={}", token(i)))
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
            assert_eq!(p.person_key(&client, h), spelled_handle(h));
            assert_eq!(format!("bot-pm-{}", p.person_key(&client, h)), client);
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
        "upstreams: {a: 'http://vllm.pmos.internal/v1'}\nroles: {default: {upstream: a, model: m}}\nclass: production\n",
    )
    .unwrap();
    let config = |stub: &'static str| {
        Config::from_lookup(move |k| match k {
            "PMOS_GATEWAY_TOKENS" => Some(format!("runtime={}", "a".repeat(32))),
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
    // What the Spark uses, over http or https.
    for upstream in [
        "http://vllm-default.pmos.internal:8000/v1",
        "https://gpu.models.pmos.internal/v1",
        "http://VLLM.pmos.internal/v1",
    ] {
        assert!(config(upstream, "", "production").is_ok(), "{upstream}");
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
        }))
        .await;
        let v: Value = reqwest::Client::new()
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
            let res = reqwest::Client::new()
                .post(format!("{base}/v1/chat/completions"))
                .bearer_auth(RUNTIME)
                .json(&json!({"model": "pmos/default",
                    "messages": [{"role": "user", "content": "hi"}]}))
                .send()
                .await
                .unwrap();
            assert_eq!(res.status(), 200);
        }
        stop.cancel();
    }
}
