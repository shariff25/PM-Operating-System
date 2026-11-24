//! Slice MP2 (`docs/model-providers-spec.md` §4.11): one key per upstream, each in its own file,
//! sent only to its own upstream, re-read when it changes, never logged.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use pmos_contract::model_source::ModelSource;
use pmos_gateway::config::{Client, Models};
use pmos_gateway::keys::KeyStore;
use pmos_gateway::usage::UsageLog;
use pmos_gateway::{router_with, GatewayConfig, RouterOptions};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "keys-test-token-0123456789abcdef-xyz";

#[derive(Default)]
struct Seen {
    auth: Mutex<Vec<String>>,
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

async fn mock() -> (String, Arc<Seen>, CancellationToken) {
    let seen = Arc::new(Seen::default());
    let record = |State(s): State<Arc<Seen>>, h: HeaderMap| async move {
        let a = h
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        s.auth.lock().unwrap().push(a);
    };
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(move |st: State<Arc<Seen>>, h: HeaderMap| async move {
                record(st, h).await;
                Json(json!({"id": "x", "object": "chat.completion", "model": "m",
                    "choices": [{"index": 0, "finish_reason": "stop",
                                 "message": {"role": "assistant", "content": "ok"}}]}))
            }),
        )
        .route(
            "/v1/models",
            get(move |st: State<Arc<Seen>>, h: HeaderMap| async move {
                record(st, h).await;
                Json(json!({"data": []}))
            }),
        )
        .with_state(seen.clone());
    let (base, stop) = serve(app).await;
    (format!("{base}/v1"), seen, stop)
}

fn key_file(dir: &Path, name: &str, key: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, format!("{key}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    p
}

#[tokio::test]
async fn each_upstream_gets_only_its_own_key_from_its_own_file() {
    let dir = std::env::temp_dir().join(format!("pmos-gw-keys-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ka = key_file(&dir, "upstream-a.key", "key-for-a-0001");
    let kb = key_file(&dir, "upstream-b.key", "key-for-b-0002");
    let (a, seen_a, stop_a) = mock().await;
    let (b, seen_b, stop_b) = mock().await;
    let yaml = format!(
        "upstreams:\n  \
           up-a: {{url: 'openai+{a}', key-file: '{}'}}\n  \
           up-b: {{url: 'openai+{b}', key-file: '{}'}}\n\
         roles:\n  \
           default: {{upstream: up-a, model: id-a, label: hosted-model-a}}\n  \
           extraction: {{upstream: up-b, model: id-b, label: hosted-model-b}}\n",
        ka.display(),
        kb.display()
    );
    let models = Models::parse(&yaml, true).unwrap();
    let keys = Arc::new(KeyStore::default());
    let (base, stop) = serve(router_with(
        GatewayConfig {
            models,
            clients: vec![Client {
                name: "runtime".into(),
                token: TOKEN.into(),
            }],
            usage: Arc::new(UsageLog::new(None).unwrap()),
            stub: false,
            workarounds_on: true,
            anthropic_key: String::new(),
            remote_key: String::new(),
            retry: Default::default(),
            timeouts: pmos_gateway::Timeouts {
                ready_cache: Duration::ZERO,
                ..Default::default()
            },
            model_source: ModelSource::HostedTest,
        },
        RouterOptions {
            keys: keys.clone(),
            ..Default::default()
        },
    ))
    .await;
    let call = |role: &str| {
        reqwest::Client::new()
            .post(format!("{base}/v1/chat/completions"))
            .bearer_auth(TOKEN)
            .header("x-pmos-model-source", "hosted-test")
            .json(&json!({"model": format!("pmos/{role}"), "messages": [{"role": "user", "content": "hi"}]}))
            .send()
    };
    assert_eq!(call("default").await.unwrap().status(), 200);
    assert_eq!(call("extraction").await.unwrap().status(), 200);
    assert_eq!(*seen_a.auth.lock().unwrap(), vec!["Bearer key-for-a-0001"]);
    assert_eq!(*seen_b.auth.lock().unwrap(), vec!["Bearer key-for-b-0002"]);
    // The readiness probe sends each upstream its own key too.
    reqwest::get(format!("{base}/readyz")).await.unwrap();
    assert!(seen_a
        .auth
        .lock()
        .unwrap()
        .iter()
        .all(|k| k == "Bearer key-for-a-0001"));
    assert!(seen_b
        .auth
        .lock()
        .unwrap()
        .iter()
        .all(|k| k == "Bearer key-for-b-0002"));

    // A new key is used after SIGHUP's reload; an empty file sends nothing at all.
    key_file(&dir, "upstream-a.key", "key-for-a-0003-new");
    keys.reload();
    assert_eq!(call("default").await.unwrap().status(), 200);
    assert_eq!(
        seen_a.auth.lock().unwrap().last().unwrap(),
        "Bearer key-for-a-0003-new"
    );
    key_file(&dir, "upstream-b.key", "");
    keys.reload();
    let before = seen_b.auth.lock().unwrap().len();
    let res = call("extraction").await.unwrap();
    assert_eq!(res.status(), 502);
    let body = res.text().await.unwrap();
    assert!(
        !body.contains("key") || !body.contains("upstream-b"),
        "{body}"
    );
    assert_eq!(
        seen_b.auth.lock().unwrap().len(),
        before,
        "nothing was sent"
    );
    // A key file readable by others is refused the same way.
    let kb = key_file(&dir, "upstream-b.key", "key-for-b-0004");
    std::fs::set_permissions(&kb, std::fs::Permissions::from_mode(0o644)).unwrap();
    keys.reload();
    assert_eq!(call("extraction").await.unwrap().status(), 502);
    assert_eq!(seen_b.auth.lock().unwrap().len(), before);
    std::fs::set_permissions(&kb, std::fs::Permissions::from_mode(0o600)).unwrap();
    keys.reload();
    assert_eq!(call("extraction").await.unwrap().status(), 200);
    assert_eq!(
        seen_b.auth.lock().unwrap().last().unwrap(),
        "Bearer key-for-b-0004"
    );
    stop.cancel();
    stop_a.cancel();
    stop_b.cancel();
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_binary_never_takes_a_key_in_argv_and_never_prints_one() {
    // `--check-config` reads the config and the environment; no key ever goes in argv, and
    // its summary names labels only.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_pmos-gateway"))
        .arg("--check-config")
        .arg("/nonexistent/gateway.yaml")
        .env("PMOS_MODEL_SOURCE", "hosted-test")
        .env("PMOS_GATEWAY_CLOUD_TEST", "1")
        .env(
            "PMOS_GATEWAY_REMOTE_KEY",
            "a-key-that-must-never-be-printed",
        )
        .output()
        .unwrap();
    assert!(!out.status.success());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!all.contains("a-key-that-must-never"), "{all}");
    let _: Value = json!(null);
}
