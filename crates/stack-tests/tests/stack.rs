//! R15: the real bot runtime, tool server and `pmos-web` routers, together, in one process.
//! Memory stores, fake connectors, user assertions in `require` on both receivers with real
//! (test) keys, and `pmos-web` signing with them. A stand-in worker does what the harness
//! does through the runtime's worker API: claim, call a tool with the session, complete.
//!
//! The walk: dev sign-in → ask → the bot drafts a Slack reply → the draft appears in the web
//! app → the PM saves an edit → approves → the tool server writes the edited text. And a
//! refusal: another PM gets `404` for that draft, reading or deciding it.

use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use pmos_assertion::{Key, KeyRing};
use pmos_toolserver::catalog::Catalog;
use pmos_toolserver::connectors::fake::{FakeConnector, SLACK_ALL_TEAM};
use pmos_toolserver::connectors::Connector;
use pmos_toolserver::{router_full, AppConfig, PersonClients, Stores, ToolService};
use pmos_web::upstream::{Credentials, Signer};
use pmos_web::{Config, Deps};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

// Test tokens and keys only: fixed values, never used anywhere real.
const SERVICE_TOKEN: &str = "stack-service-token-0123456789abcdef01";
const TS_WEB_TOKEN: &str = "stack-ts-web-token-0123456789abcdef012";
const RT_WEB_TOKEN: &str = "stack-rt-web-token-0123456789abcdef012";
const RT_WORKER_TOKEN: &str = "stack-rt-worker-token-0123456789abcdef";
const GATEWAY_TOKEN: &str = "stack-gateway-token-0123456789abcdef01";
const WEB_BOTS_KEY: &str = "d2ViLWJvdHMtdGVzdC1rZXktMDEyMzQ1Njc4OWFiY2Q=";
const WEB_TOOLS_KEY: &str = "d2ViLXRvb2xzZXJ2ZXItdGVzdC1rZXktMDEyMzQ1Njc=";

const TEAM: &str = "members:
  - {handle: ana, slack-id: U0ANA, email: ana@example.com, roles: [pm]}
  - {handle: ben, slack-id: U0BEN, email: ben@example.com, roles: [pm]}
";
const THREAD: &str = "1731600000.000100";

fn copy_dir(from: &Path, to: &Path) {
    for entry in walkdir::WalkDir::new(from) {
        let entry = entry.unwrap();
        let dest = to.join(entry.path().strip_prefix(from).unwrap());
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&dest).unwrap();
        } else {
            std::fs::copy(entry.path(), &dest).unwrap();
        }
    }
}

/// The repo's content, with Ana's personal bot and a team of Ana and Ben.
fn content() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("content");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content"),
        &root,
    );
    std::fs::create_dir_all(root.join("bots/pm-ana")).unwrap();
    std::fs::write(
        root.join("bots/pm-ana/BOT.md"),
        "---\nname: pm-ana\nrole: pm-assistant\nscope: personal\nowner: \"@ana\"\ndescription: Ana's assistant.\n---\n\n# Ana's assistant\n\nHelp Ana.\n",
    )
    .unwrap();
    std::fs::write(root.join("team.yaml"), TEAM).unwrap();
    (dir, root)
}

async fn serve(app: axum::Router, cancel: &CancellationToken) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let ct = cancel.clone();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { ct.cancelled_owned().await })
            .await
            .unwrap();
    });
    base
}

fn ring(id: &str, key: &str) -> KeyRing {
    KeyRing::new(Key::from_base64(id, key).unwrap())
}

struct Stack {
    web: String,
    runtime: String,
    svc: Arc<ToolService>,
    slack: Arc<FakeConnector>,
    http: reqwest::Client,
    cancel: CancellationToken,
    _content: tempfile::TempDir,
}

impl Drop for Stack {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn start() -> Stack {
    let cancel = CancellationToken::new();
    let (dir, root) = content();

    // The tool server: memory stores, fake connectors, `require` for pmos-web.
    let (stores, _audit) = Stores::in_memory(&root);
    let catalog = Catalog::load(&root, Utc::now().date_naive()).unwrap();
    let slack = Arc::new(FakeConnector::slack());
    let connectors: Vec<Arc<dyn Connector>> = vec![
        Arc::new(FakeConnector::jira()),
        Arc::new(FakeConnector::confluence()),
        slack.clone(),
        Arc::new(FakeConnector::docs()),
    ];
    // An open user directory, as with fake connectors before the user database (slice U3).
    let svc = Arc::new(
        ToolService::new(catalog, stores, connectors)
            .unwrap()
            .with_open_directory(),
    );
    let ts_keys = pmos_toolserver::assertion::parse_keys(
        &format!("web:toolserver:wt1:{WEB_TOOLS_KEY}"),
        "the tool server",
        |iss, aud| aud == "toolserver" && iss == "web",
    )
    .unwrap();
    let toolserver = serve(
        router_full(
            svc.clone(),
            AppConfig {
                service_token: SERVICE_TOKEN.into(),
                bot: None,
                console_token: None,
                content_root: root.clone(),
                oauth: None,
                allowed_hosts: vec!["127.0.0.1".into(), "localhost".into()],
            },
            None,
            PersonClients {
                web_token: Some(TS_WEB_TOKEN.into()),
                assertions: Arc::new(
                    pmos_toolserver::assertion::Assertions::new(
                        pmos_toolserver::assertion::Mode::Require,
                        ts_keys,
                    )
                    .unwrap(),
                ),
            },
            cancel.clone(),
        ),
        &cancel,
    )
    .await;

    // The bot runtime: memory store, `require` for the web client.
    let rt_keys = pmos_bots::assertion::parse_keys(
        &format!("web:bots:wb1:{WEB_BOTS_KEY}"),
        "the bot runtime",
        |iss, aud| aud == "bots" && iss == "web",
    )
    .unwrap();
    let rt = Arc::new(pmos_bots::Runtime::new(
        Arc::new(pmos_bots::store::MemoryStore::default()),
        Arc::new(pmos_bots::catalog::Catalog::load(&root, Utc::now().date_naive()).unwrap()),
        pmos_bots::clients::ToolServer::new(&toolserver, SERVICE_TOKEN),
        // Never called: the stand-in worker answers without a model.
        pmos_bots::clients::Gateway::new("http://127.0.0.1:9/v1", GATEWAY_TOKEN),
        cancel.child_token(),
    ));
    let runtime = serve(
        pmos_bots::router_with(
            rt,
            pmos_bots::config::parse_clients(&format!(
                "web={RT_WEB_TOKEN},worker={RT_WORKER_TOKEN}"
            ))
            .unwrap(),
            Vec::new(),
            Arc::new(
                pmos_bots::assertion::Assertions::new(pmos_bots::assertion::Mode::Require, rt_keys)
                    .unwrap(),
            ),
        ),
        &cancel,
    )
    .await;

    // pmos-web, signing with the same keys.
    let config = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        bots_url: runtime.clone(),
        bots_token: Credentials::new(RT_WEB_TOKEN).with_signer(Signer::new(
            "web",
            "bots",
            ring("wb1", WEB_BOTS_KEY),
            root.clone(),
        )),
        toolserver_url: toolserver.clone(),
        toolserver_token: Credentials::new(TS_WEB_TOKEN).with_signer(Signer::new(
            "web",
            "toolserver",
            ring("wt1", WEB_TOOLS_KEY),
            root.clone(),
        )),
        content_root: root.clone(),
        static_dir: None,
        public_url: None,
        dev_login: true,
        entra: None,
        session_idle: chrono::Duration::hours(8),
        session_max: chrono::Duration::hours(24),
        internal_bind: None,
    };
    let web = serve(pmos_web::build(config, Deps::in_memory()).unwrap(), &cancel).await;

    Stack {
        web,
        runtime,
        svc,
        slack,
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
        cancel,
        _content: dir,
    }
}

/// A signed-in browser: its cookie and CSRF token.
struct Browser {
    cookie: String,
    csrf: String,
}

impl Stack {
    async fn send(
        &self,
        b: Option<&Browser>,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value, reqwest::header::HeaderMap) {
        let mut req = self
            .http
            .request(method.clone(), format!("{}{path}", self.web));
        if let Some(b) = b {
            req = req.header("cookie", format!("__Host-pmos={}", b.cookie));
            if method != Method::GET {
                req = req.header("x-pmos-csrf", &b.csrf);
            }
        }
        if method != Method::GET {
            req = req.header("origin", self.web.clone());
        }
        if let Some(body) = body {
            req = req.json(&body);
        }
        let res = req.send().await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let body = res.json().await.unwrap_or(Value::Null);
        (status, body, headers)
    }

    async fn login(&self, handle: &str) -> Browser {
        let (status, body, headers) = self
            .send(
                None,
                Method::POST,
                "/api/auth/dev-login",
                Some(json!({ "handle": handle })),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        let cookie = headers["set-cookie"]
            .to_str()
            .unwrap()
            .strip_prefix("__Host-pmos=")
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let mut b = Browser {
            cookie,
            csrf: String::new(),
        };
        let (status, me, _) = self.send(Some(&b), Method::GET, "/api/me", None).await;
        assert_eq!(status, StatusCode::OK, "{me}");
        b.csrf = me["csrf_token"].as_str().unwrap().to_string();
        b
    }

    /// The stand-in worker: claims the next task with the worker token, drafts a Slack reply
    /// with the task's own tool-server session, and completes the task with that draft.
    async fn work(&self) -> String {
        let claim: Value = self
            .http
            .post(format!("{}/v1/worker/claim", self.runtime))
            .bearer_auth(RT_WORKER_TOKEN)
            .json(&json!({"worker": "w1"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let task_id = claim["task"]["id"].as_str().unwrap().to_string();
        let session = self
            .svc
            .session(claim["session_token"].as_str().unwrap(), Utc::now())
            .await
            .unwrap()
            .unwrap();
        let drafted = self
            .svc
            .call(
                &session,
                "slack_post_reply",
                json!({"channel": SLACK_ALL_TEAM, "thread_ts": THREAD,
                       "text": "The bot's first try."}),
                Utc::now(),
            )
            .await
            .unwrap();
        let approval = drafted["approval_id"].as_str().unwrap().to_string();
        let res = self
            .http
            .post(format!(
                "{}/v1/worker/tasks/{task_id}/complete",
                self.runtime
            ))
            .bearer_auth(RT_WORKER_TOKEN)
            .json(
                &json!({"worker": "w1", "result": "I drafted a reply for you to approve.",
                          "approvals": [approval]}),
            )
            .send()
            .await
            .unwrap();
        assert!(res.status().is_success(), "{}", res.status());
        approval
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ask_draft_edit_approve_through_the_real_services() {
    let s = start().await;
    let ana = s.login("ana").await;

    // Ask: pmos-web creates the task at the real runtime, signed for Ana.
    let (status, task, _) = s
        .send(
            Some(&ana),
            Method::POST,
            "/api/tasks",
            Some(json!({"text": "Answer Ben in #launch about SSO"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{task}");
    assert_eq!(task["bot"], "pm-ana");
    let approval = s.work().await;

    // The draft appears, for Ana only.
    let (status, list, _) = s.send(Some(&ana), Method::GET, "/api/drafts", None).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["truncated"], false);
    let ids: Vec<&str> = list["drafts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["practice"] == false)
        .filter_map(|d| d["id"].as_str())
        .collect();
    assert_eq!(ids, [approval.as_str()]);
    let draft_path = format!("/api/drafts/{approval}");
    let (status, draft, _) = s.send(Some(&ana), Method::GET, &draft_path, None).await;
    assert_eq!(status, StatusCode::OK, "{draft}");
    assert_eq!(draft["body"], "The bot's first try.");

    // The refusal: Ben gets 404 for Ana's draft, reading it or deciding it, and nothing moves.
    let ben = s.login("ben").await;
    let (status, body, _) = s.send(Some(&ben), Method::GET, &draft_path, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, body, _) = s
        .send(
            Some(&ben),
            Method::POST,
            &format!("{draft_path}/approve"),
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // Ana saves an edit: the tool server holds it.
    let (status, saved, _) = s
        .send(
            Some(&ana),
            Method::PUT,
            &draft_path,
            Some(json!({"body": "SSO ships in 5.1, edited by Ana."})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["body"], "SSO ships in 5.1, edited by Ana.");
    let item = s.svc.approval(&approval).await.unwrap().unwrap();
    assert_eq!(item.args["text"], "SSO ships in 5.1, edited by Ana.");
    assert_eq!(item.status.as_str(), "pending");
    assert!(s.slack.writes().is_empty());

    // An approve with the version Ana saw before her edit is stale: nothing is sent.
    let (status, body, _) = s
        .send(
            Some(&ana),
            Method::POST,
            &format!("{draft_path}/approve"),
            Some(json!({"version": draft["version"]})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "stale");
    assert!(s.slack.writes().is_empty());

    // Ana approves what she sees now: the write carries her edited text.
    let (status, approved, _) = s
        .send(
            Some(&ana),
            Method::POST,
            &format!("{draft_path}/approve"),
            Some(json!({"version": saved["version"]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{approved}");
    assert_eq!(approved["status"], "approved");
    let writes = s.slack.writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].args["text"], "SSO ships in 5.1, edited by Ana.");
    assert_eq!(writes[0].user, "ana");
    let item = s.svc.approval(&approval).await.unwrap().unwrap();
    assert_eq!(item.decided_by.as_deref(), Some("ana"));
}
