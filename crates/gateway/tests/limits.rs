//! Review findings N3 and N4 and their Lows: which clients' caller headers are believed, the
//! per-client cap, the queue deadline, a stream's total time, the answer size cap, a caller
//! that hangs up, upstream URLs kept from callers, and `GET /v1/pmos/roles`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use pmos_gateway::config::{Client, Models};
use pmos_gateway::scheduler::Priority;
use pmos_gateway::usage::UsageLog;
use pmos_gateway::{router, GatewayConfig};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const RUNTIME: &str = "limits-test-runtime-token-0123456789";
const EVALS: &str = "limits-test-evals-token-0123456789ab";
const OTHER: &str = "limits-test-other-token-0123456789ab";

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
}
