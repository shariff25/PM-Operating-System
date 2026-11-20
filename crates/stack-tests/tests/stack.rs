//! R15: the real bot runtime, tool server and `pmos-web` routers, together, in one process.
//! Memory stores, fake connectors, user assertions in `require` on both receivers with real
//! (test) keys, and `pmos-web` signing with them. A stand-in worker does what the harness
//! does through the runtime's worker API: claim, call a tool with the session, complete.
//!
//! The walk: dev sign-in → ask → the bot drafts a Slack reply → the draft appears in the web
//! app → the PM saves an edit → approves → the tool server writes the edited text. And a
//! refusal: another PM gets `404` for that draft, reading or deciding it. And slice U4c:
//! `pmos-web` signs people in from the tool server's real user database (`resolve`), so a
//! deactivation there ends a web session within 10 seconds.
//!
//! And slice U7's proof (`docs/user-database-spec.md` §12, "Proving no service decides from the
//! file"): the console and the Slack service's directory join the stack, a hostile `team.yaml`
//! is planted wherever a service could read one, and every service decides as before; then the
//! same with no `team.yaml` at all. The source half of the proof is `tests/no_team_file.rs`.

use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
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
const TS_BOT_TOKEN: &str = "stack-ts-bot-token-0123456789abcdef012";
const TS_CONSOLE_TOKEN: &str = "stack-ts-console-token-0123456789abcdef";
const RT_SLACK_TOKEN: &str = "stack-rt-slack-token-0123456789abcdef01";
const RT_CONSOLE_TOKEN: &str = "stack-rt-console-token-0123456789abcde";
const PROXY_SECRET: &str = "stack-proxy-secret-0123456789abcdef012";
const CONSOLE_BOTS_KEY: &str = "Y29uc29sZS1ib3RzLXRlc3Qta2V5LTAxMjM0NTY3ODk=";
const CONSOLE_TOOLS_KEY: &str = "Y29uc29sZS10b29scy10ZXN0LWtleS0wMTIzNDU2Nzg=";
const TS_WEB_TOKEN: &str = "stack-ts-web-token-0123456789abcdef012";
const RT_WEB_TOKEN: &str = "stack-rt-web-token-0123456789abcdef012";
const RT_WORKER_TOKEN: &str = "stack-rt-worker-token-0123456789abcdef";
const GATEWAY_TOKEN: &str = "stack-gateway-token-0123456789abcdef01";
const WEB_BOTS_KEY: &str = "d2ViLWJvdHMtdGVzdC1rZXktMDEyMzQ1Njc4OWFiY2Q=";
const WEB_TOOLS_KEY: &str = "d2ViLXRvb2xzZXJ2ZXItdGVzdC1rZXktMDEyMzQ1Njc=";

/// A hostile `team.yaml` (slice U7's proof): `mallory` as an admin with Ana's Slack id and
/// address, Ana and Ben made admins and auditors, and a stranger with a Slack id. The user
/// database says none of it, so none of it may count anywhere.
const HOSTILE_TEAM: &str = "members:
  - {handle: mallory, slack-id: U0ANA, email: ana@example.com, roles: [pm, admin]}
  - {handle: ana, slack-id: U0MALLORY, email: mallory@example.com, roles: [pm, admin, auditor]}
  - {handle: ben, slack-id: U0BEN, email: ben@example.com, roles: [admin, reviewer]}
  - {handle: eve, slack-id: U0EVE, email: eve@example.com, roles: [pm, admin]}
";
/// The user database's seed: two PMs, and the admin every user database has.
const USERS: &str = "members:
  - {handle: ana, slack-id: U0ANA, email: ana@example.com, roles: [pm]}
  - {handle: ben, slack-id: U0BEN, email: ben@example.com, roles: [pm]}
  - {handle: stack-admin, slack-id: U0STACKADM, roles: [pm, admin]}
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

/// Where a service could find a `team.yaml`, relative to the stack's checkout: the content
/// root every service is given (`PMOS_CONTENT_ROOT`), the checkout around it, and the folders
/// the lint and the runtime's reloader walk.
const TEAM_FILE_PLACES: [&str; 6] = [
    "content/team.yaml",
    "team.yaml",
    "content/bots/team.yaml",
    "content/bots/pm-ana/team.yaml",
    "content/master/team.yaml",
    "content/roles/team.yaml",
];

/// The repo's content, with Ana's personal bot; with `hostile`, [`HOSTILE_TEAM`] in every one
/// of [`TEAM_FILE_PLACES`].
fn content(hostile: bool) -> (tempfile::TempDir, std::path::PathBuf) {
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
    for place in TEAM_FILE_PLACES {
        let path = dir.path().join(place);
        if hostile {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, HOSTILE_TEAM).unwrap();
        } else {
            assert!(!path.exists(), "{place}");
        }
    }
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
    /// The console, behind a stand-in sign-in proxy (`X-Forwarded-User`).
    console: String,
    /// The Slack service's view of who people are: the tool server's directory, read with
    /// the Slack bot's token, as `pmos-slack` reads it.
    slack_people: pmos_slack::people::People,
    svc: Arc<ToolService>,
    slack: Arc<FakeConnector>,
    http: reqwest::Client,
    cancel: CancellationToken,
    /// `pmos-web`'s clock, which a test moves (its directory cache reads it).
    web_clock: Arc<Mutex<DateTime<Utc>>>,
    /// The tool server's user database.
    users: Arc<dyn pmos_toolserver::users::UserStore>,
    root: std::path::PathBuf,
    _content: tempfile::TempDir,
}

impl Drop for Stack {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn start() -> Stack {
    start_with(false).await
}

async fn start_with(hostile: bool) -> Stack {
    let cancel = CancellationToken::new();
    let (dir, root) = content(hostile);

    // The tool server: memory stores, fake connectors, `require` for pmos-web. Its user
    // database is seeded as a dev stack's is (PMOS_USERS_SEED): the tool server and the
    // runtime (slice U4a, through `/internal/directory`) decide from it.
    let (mut stores, _audit) = Stores::in_memory(&root);
    let seed = pmos_toolserver::users::ImportRequest::parse(
        USERS.as_bytes(),
        None,
        pmos_toolserver::users::ImportMode::Bootstrap,
    )
    .unwrap();
    stores.users = Arc::new(
        pmos_toolserver::users::MemoryUsers::seeded(
            seed,
            pmos_toolserver::users::Rules::default(),
            Some(stores.audit.clone()),
        )
        .await
        .unwrap()
        .with_linked(stores.linked()),
    );
    let users = stores.users.clone();
    let catalog = Catalog::load(&root, Utc::now().date_naive()).unwrap();
    let slack = Arc::new(FakeConnector::slack());
    let connectors: Vec<Arc<dyn Connector>> = vec![
        Arc::new(FakeConnector::jira()),
        Arc::new(FakeConnector::confluence()),
        slack.clone(),
        Arc::new(FakeConnector::docs()),
    ];
    // Password sign-in on (docs/password-sign-in.md), with a test vault key.
    let vault = pmos_toolserver::vault::KeyRing::from_spec(
        &pmos_toolserver::vault::KeyRing::generate_spec("pw1"),
    )
    .unwrap();
    let svc = Arc::new(
        ToolService::new(catalog, stores, connectors)
            .unwrap()
            .with_passwords(Arc::new(
                pmos_toolserver::password::PasswordService::new(vault).unwrap(),
            )),
    );
    let ts_keys = pmos_toolserver::assertion::parse_keys(
        &format!("web:toolserver:wt1:{WEB_TOOLS_KEY},console:toolserver:ct1:{CONSOLE_TOOLS_KEY}"),
        "the tool server",
        |iss, aud| aud == "toolserver" && (iss == "web" || iss == "console"),
    )
    .unwrap();
    let toolserver = serve(
        router_full(
            svc.clone(),
            AppConfig {
                service_token: SERVICE_TOKEN.into(),
                bot: Some(pmos_toolserver::app::BotAccess {
                    token: TS_BOT_TOKEN.into(),
                    skills: vec!["docs".into()],
                }),
                console_token: Some(TS_CONSOLE_TOKEN.into()),
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
        &format!("web:bots:wb1:{WEB_BOTS_KEY},console:bots:cb1:{CONSOLE_BOTS_KEY}"),
        "the bot runtime",
        |iss, aud| aud == "bots" && (iss == "web" || iss == "console"),
    )
    .unwrap();
    // Who may ask: the tool server's directory, with the orchestrator token, as in deploy.
    let people = Arc::new(pmos_bots::people::SnapshotClient::http(
        &toolserver,
        SERVICE_TOKEN,
    ));
    people
        .refresh()
        .await
        .expect("the tool server's directory answers");
    let rt = Arc::new(pmos_bots::Runtime::new(
        Arc::new(pmos_bots::store::MemoryStore::default()),
        Arc::new(pmos_bots::catalog::Catalog::load(&root, Utc::now().date_naive()).unwrap()),
        people,
        pmos_bots::clients::ToolServer::new(&toolserver, SERVICE_TOKEN),
        // Never called: the stand-in worker answers without a model.
        pmos_bots::clients::Gateway::new("http://127.0.0.1:9/v1", GATEWAY_TOKEN),
        cancel.child_token(),
    ));
    let runtime = serve(
        pmos_bots::router_with(
            rt,
            pmos_bots::config::parse_clients(&format!(
                "web={RT_WEB_TOKEN},worker={RT_WORKER_TOKEN},slack={RT_SLACK_TOKEN},console={RT_CONSOLE_TOKEN}"
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
        )),
        toolserver_url: toolserver.clone(),
        toolserver_token: Credentials::new(TS_WEB_TOKEN).with_signer(Signer::new(
            "web",
            "toolserver",
            ring("wt1", WEB_TOOLS_KEY),
        )),
        content_root: root.clone(),
        static_dir: None,
        public_url: None,
        dev_login: true,
        entra: None,
        session_idle: chrono::Duration::hours(8),
        session_max: chrono::Duration::hours(24),
        internal_bind: None,
        password_login: true,
        trusted_proxies: Vec::new(),
    };
    let web_clock = Arc::new(Mutex::new(Utc::now()));
    let clock = web_clock.clone();
    let deps = Deps {
        clock: Arc::new(move || *clock.lock().unwrap()),
        ..Deps::in_memory()
    };
    let web = serve(pmos_web::build(config, deps).unwrap(), &cancel).await;

    // The console, as deployed behind a sign-in proxy, signing for the runtime and the tool
    // server. Its content root is the hostile one too: it reads no content at all (U4d).
    let root_text = root.display().to_string();
    let console_config = pmos_console::Config::from_lookup(|k| match k {
        "PMOS_BOTS_TOKEN" => Some(RT_CONSOLE_TOKEN.to_string()),
        "PMOS_TOOLSERVER_TOKEN" => Some(TS_CONSOLE_TOKEN.to_string()),
        "PMOS_CONSOLE_TRUST_PROXY" => Some("1".into()),
        "PMOS_CONSOLE_PROXY_SECRET" => Some(PROXY_SECRET.into()),
        "PMOS_ASSERTION_KEYS" => Some(format!(
            "console:bots:cb1:{CONSOLE_BOTS_KEY},console:toolserver:ct1:{CONSOLE_TOOLS_KEY}"
        )),
        "PMOS_CONTENT_ROOT" => Some(root_text.clone()),
        "PMOS_BOTS_URL" => Some(runtime.clone()),
        "PMOS_TOOLSERVER_URL" => Some(toolserver.clone()),
        _ => None,
    })
    .unwrap();
    let console = serve(pmos_console::router(console_config), &cancel).await;

    // The Slack service's directory: the tool server's, with the Slack bot's token.
    let slack_people = pmos_slack::people::People::new(Arc::new(
        pmos_bots::people::SnapshotClient::http(&toolserver, TS_BOT_TOKEN),
    ));
    slack_people
        .client()
        .refresh()
        .await
        .expect("the tool server's directory answers the Slack bot");

    Stack {
        web,
        runtime,
        console,
        slack_people,
        svc,
        slack,
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
        cancel,
        web_clock,
        users,
        root,
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

/// Slice U4c on the real tool server: sign-in and every request resolve the person in its
/// user database; a deactivation there ends the web session within 10 seconds, and sign-in is
/// then refused; someone only in `team.yaml` never signs in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deactivation_in_the_user_database_ends_the_web_session_within_ten_seconds() {
    let stack = start().await;
    let ana = stack.login("ana").await;
    let (status, me, _) = stack.send(Some(&ana), Method::GET, "/api/me", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["roles"], json!(["pm"]));
    stack
        .users
        .apply(
            &pmos_toolserver::users::Actor::web("stack-admin"),
            pmos_toolserver::users::Op::Deactivate {
                handle: "ana".into(),
            },
            Utc::now(),
        )
        .await
        .expect("ada deactivates ana");
    *stack.web_clock.lock().unwrap() += chrono::Duration::seconds(10);
    let (status, body, _) = stack
        .send(Some(&ana), Method::GET, "/api/drafts", None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "not_a_member");
    let (status, body, _) = stack
        .send(
            None,
            Method::POST,
            "/api/auth/dev-login",
            Some(json!({"handle": "ana"})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // Someone not in the user database never signs in; Ben still does.
    let (status, _, _) = stack
        .send(
            None,
            Method::POST,
            "/api/auth/dev-login",
            Some(json!({"handle": "mallory"})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    stack.login("ben").await;
}

/// Slice U5a on the real tool server: an admin's Team screen calls go through `pmos-web` to the
/// tool server's Admin API, signed for the admin, and its rules answer: the list, a role
/// change with `expected_version`, a stale one, a self-change, and an invite.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_admin_changes_people_through_pmos_web_and_the_tool_server_decides() {
    let s = start().await;
    let admin = s.login("stack-admin").await;
    let (status, list, _) = s
        .send(Some(&admin), Method::GET, "/api/admin/users", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["view"], "admin");
    let version = list["version"].as_i64().unwrap();
    let ana = list["users"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["handle"] == "ana")
        .unwrap();
    assert_eq!(ana["sign_in_name"], "ana@example.com");
    assert!(ana.get("slack_id").is_none() && ana.get("oid").is_none());

    let (status, done, _) = s
        .send(
            Some(&admin),
            Method::PUT,
            "/api/admin/users/ana/roles",
            Some(json!({"roles": ["pm", "reviewer"], "expected_version": version})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["user"]["roles"], json!(["pm", "reviewer"]));
    // The same version again: the directory moved.
    let (status, body, _) = s
        .send(
            Some(&admin),
            Method::PUT,
            "/api/admin/users/ben/roles",
            Some(json!({"roles": ["pm", "reviewer"], "expected_version": version})),
        )
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("stale"))
    );
    // Nobody changes their own status.
    let (status, body, _) = s
        .send(
            Some(&admin),
            Method::POST,
            "/api/admin/users/stack-admin/deactivate",
            None,
        )
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("self_change"))
    );
    // Invite: the new person signs in at once.
    let (status, body, _) = s
        .send(
            Some(&admin),
            Method::POST,
            "/api/admin/users",
            Some(
                json!({"handle": "cy", "display_name": "Cy", "sign_in_name": "cy@example.com",
                        "atlassian_email": "cy@example.com", "roles": ["pm"]}),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["user"]["sign_in_name_confirmed"], true);
    s.login("cy").await;
    // A PM may not.
    let ben = s.login("ben").await;
    let (status, _, _) = s
        .send(
            Some(&ben),
            Method::POST,
            "/api/admin/users/cy/deactivate",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// Slice U6a on the real tool server: an admin makes Ben a reviewer of Ana; Ben reads Ana's
/// draft and recommends through `pmos-web`, signed for him; Ana sees the advice on her draft,
/// marked "on an earlier version" once she edits it; Ben's recommendation on the old version
/// is refused; nothing is ever sent by a recommendation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reviewer_recommends_through_pmos_web_and_the_owner_sees_it() {
    let s = start().await;
    let admin = s.login("stack-admin").await;
    let (_, list, _) = s
        .send(Some(&admin), Method::GET, "/api/admin/users", None)
        .await;
    let version = list["version"].as_i64().unwrap();
    let (status, body, _) = s
        .send(
            Some(&admin),
            Method::PUT,
            "/api/admin/users/ben/roles",
            Some(json!({"roles": ["pm", "reviewer"], "expected_version": version})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body, _) = s
        .send(
            Some(&admin),
            Method::PUT,
            "/api/admin/reviewers/ben",
            Some(json!({"pms": ["ana"]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let ana = s.login("ana").await;
    let (status, _, _) = s
        .send(
            Some(&ana),
            Method::POST,
            "/api/tasks",
            Some(json!({"text": "Answer Ben in #launch about SSO"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let approval = s.work().await;

    let ben = s.login("ben").await;
    let (_, me, _) = s.send(Some(&ben), Method::GET, "/api/me", None).await;
    assert_eq!(me["reviewer"], true);
    let (status, inbox, _) = s.send(Some(&ben), Method::GET, "/api/reviews", None).await;
    assert_eq!(status, StatusCode::OK, "{inbox}");
    assert_eq!(inbox["drafts"][0]["id"], approval.as_str());
    let review_path = format!("/api/reviews/{approval}");
    let (status, read, _) = s.send(Some(&ben), Method::GET, &review_path, None).await;
    assert_eq!(status, StatusCode::OK, "{read}");
    assert_eq!(read["restricted"], false, "{read}");
    assert_eq!(read["body"], "The bot's first try.");
    let (status, done, _) = s
        .send(
            Some(&ben),
            Method::POST,
            &format!("{review_path}/recommendation"),
            Some(
                json!({"verdict": "approve", "note": "Fake note: looks right.",
                        "revision": read["revision"], "args_hash": read["args_hash"]}),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["recommendation"]["verdict"], "approve");
    let item = s.svc.approval(&approval).await.unwrap().unwrap();
    assert_eq!(item.status.as_str(), "pending");
    assert!(s.slack.writes().is_empty());

    // Ana sees it on her draft; Ana is not a reviewer.
    let draft_path = format!("/api/drafts/{approval}");
    let (_, draft, _) = s.send(Some(&ana), Method::GET, &draft_path, None).await;
    let rec = &draft["recommendations"][0];
    assert_eq!(rec["reviewer"], "ben");
    assert_eq!(rec["note"], "Fake note: looks right.");
    assert_eq!(rec["stale"], false);
    let (status, _, _) = s.send(Some(&ana), Method::GET, "/api/reviews", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Ana edits: the advice was on an earlier version, and Ben's old read is refused.
    let (status, saved, _) = s
        .send(
            Some(&ana),
            Method::PUT,
            &draft_path,
            Some(json!({"body": "SSO ships in 5.1, edited by Ana."})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["recommendations"][0]["stale"], true);
    let (status, body, _) = s
        .send(
            Some(&ben),
            Method::POST,
            &format!("{review_path}/recommendation"),
            Some(json!({"verdict": "changes", "revision": read["revision"],
                        "args_hash": read["args_hash"]})),
        )
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("draft_changed"))
    );
    assert!(s.slack.writes().is_empty());
}

impl Stack {
    /// A console page for `user`, as the sign-in proxy names them.
    async fn console_get(&self, user: &str, path: &str) -> (StatusCode, String) {
        let res = self
            .http
            .get(format!("{}{path}", self.console))
            .header("x-pmos-proxy-secret", PROXY_SECRET)
            .header("x-forwarded-user", user)
            .send()
            .await
            .unwrap();
        let status = res.status();
        (status, res.text().await.unwrap())
    }

    /// An ask through the runtime's Slack client, as the Slack service sends one for `who`.
    async fn slack_ask(&self, who: &str) -> (StatusCode, Value) {
        let res = self
            .http
            .post(format!("{}/v1/tasks", self.runtime))
            .bearer_auth(RT_SLACK_TOKEN)
            .header("x-pmos-user", who)
            .json(
                &json!({"bot": "docs", "requested_by": who, "text": "What does the docs site say?",
                           "source": "slack", "slack": {"channel": "C0PROOF", "thread_ts": "0.0"}}),
            )
            .send()
            .await
            .unwrap();
        let status = res.status();
        (status, res.json().await.unwrap_or(Value::Null))
    }

    /// Every decision slice U7's proof checks, the same with a hostile `team.yaml` or none.
    async fn decisions_from_the_user_database_only(&self) {
        use pmos_slack::people::{AdminCheck, Identity};

        // Sign-in (pmos-web): Mallory and Eve, only in the file, never sign in; Ana does, as
        // the PM the database says, not the admin and auditor the file says.
        for who in ["mallory", "eve"] {
            let (status, body, _) = self
                .send(
                    None,
                    Method::POST,
                    "/api/auth/dev-login",
                    Some(json!({ "handle": who })),
                )
                .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{who}: {body}");
        }
        let ana = self.login("ana").await;
        let (_, me, _) = self.send(Some(&ana), Method::GET, "/api/me", None).await;
        assert_eq!(me["roles"], json!(["pm"]), "{me}");
        assert_eq!(me["reviewer"], false, "{me}");

        // Ana asks her own bot from the web app; the stand-in worker drafts a Slack reply.
        let (status, task, _) = self
            .send(
                Some(&ana),
                Method::POST,
                "/api/tasks",
                Some(json!({"text": "Answer Ben in #launch about SSO"})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{task}");
        let task_path = format!("/tasks/{}", task["id"].as_str().unwrap());
        let approval = self.work().await;

        // An admin read: refused to Ana and Ben (admins only in the file), served to the
        // database's admin.
        for (who, browser) in [("ana", &ana), ("ben", &self.login("ben").await)] {
            for path in ["/api/admin/users", "/api/admin/overview"] {
                let (status, body, _) = self.send(Some(browser), Method::GET, path, None).await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{who} {path}: {body}");
            }
        }
        let admin = self.login("stack-admin").await;
        let (status, list, _) = self
            .send(Some(&admin), Method::GET, "/api/admin/users", None)
            .await;
        assert_eq!(status, StatusCode::OK, "{list}");
        let handles: Vec<&str> = list["users"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|u| u["handle"].as_str())
            .collect();
        assert_eq!(handles, ["ana", "ben", "stack-admin"], "{list}");

        // The tool server's own view.
        let people = self.svc.people();
        assert!(people.user("mallory").await.unwrap().is_none());
        assert!(people.user("eve").await.unwrap().is_none());
        for who in ["ana", "ben", "mallory"] {
            assert!(!people.is_admin(who).await.unwrap(), "{who}");
        }
        assert!(people.is_admin("stack-admin").await.unwrap());

        // A bot ask (the runtime): Mallory and Eve are refused; Ana is not.
        for who in ["mallory", "eve"] {
            let (status, body) = self.slack_ask(who).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{who}: {body}");
        }
        let (status, asked) = self.slack_ask("ana").await;
        assert_eq!(status, StatusCode::CREATED, "{asked}");

        // Slack: Ana's Slack id is Ana, a PM and no admin; the file's Slack ids are nobody.
        match self.slack_people.identify("U0ANA") {
            Identity::Member(m) => assert_eq!((m.handle.as_str(), m.admin), ("ana", false)),
            other => panic!("U0ANA is {other:?}"),
        }
        assert_eq!(self.slack_people.admin("U0ANA"), AdminCheck::No);
        assert_eq!(self.slack_people.admin("U0BEN"), AdminCheck::No);
        for id in ["U0MALLORY", "U0EVE"] {
            assert_eq!(self.slack_people.identify(id), Identity::Stranger, "{id}");
        }
        assert_eq!(self.slack_people.of_handle("mallory"), Identity::Stranger);
        assert!(matches!(
            self.slack_people.admin("U0STACKADM"),
            AdminCheck::Admin(_)
        ));
        // The Slack App Home's skills catalog loads the content the same.
        let today = Utc::now().date_naive();
        let report =
            pmos_contract::lint(&self.root, &pmos_contract::LintOptions::for_service(today));
        assert!(!report.has_errors(), "{:#?}", report.diagnostics);
        assert_eq!(report.skills.len(), 40);

        // A console item read: Ana reads her task; Mallory (by handle, or by the address the
        // file gives her) and Eve read nothing; Ben's console isn't an admin's.
        for who in ["mallory", "ana@example.com", "eve", "eve@example.com"] {
            let (status, body) = self.console_get(who, &task_path).await;
            let expected = if who == "ana@example.com" {
                StatusCode::OK
            } else {
                StatusCode::FORBIDDEN
            };
            assert_eq!(status, expected, "{who}: {body}");
        }
        let (status, _) = self.console_get("mallory@example.com", "/bots").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, body) = self.console_get("ana", "/bots").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            !body.contains("· admin"),
            "the file can't make Ana an admin"
        );
        assert_eq!(
            self.console_get("ben", &task_path).await.0,
            StatusCode::NOT_FOUND
        );
        let (status, body) = self.console_get("stack-admin", "/bots").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains("stack-admin · admin"),
            "the database's admin is"
        );

        // An approval decision (the tool server): Mallory's is refused and nothing moves;
        // Ana's own goes through.
        let refused = self
            .svc
            .decide(
                &approval,
                "mallory",
                pmos_toolserver::approvals::Decision::Approve,
                None,
                Utc::now(),
            )
            .await;
        assert!(refused.is_err(), "{refused:?}");
        let item = self.svc.approval(&approval).await.unwrap().unwrap();
        assert_eq!(item.status.as_str(), "pending");
        assert!(self.slack.writes().is_empty());
        let (status, body, _) = self
            .send(
                Some(&ana),
                Method::POST,
                &format!("/api/drafts/{approval}/approve"),
                Some(json!({})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(self.slack.writes()[0].user, "ana");
    }
}

/// Slice U7's proof, part 1: a hostile `team.yaml` in every place a service could read one
/// ([`TEAM_FILE_PLACES`]) changes no decision in any service: sign-in, an admin read, a bot ask,
/// Slack's identities and admins, a console item read, an approval decision. Every service
/// starts with it (it is at most a lint warning to them).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hostile_team_yaml_decides_nothing_in_any_service() {
    let s = start_with(true).await;
    for place in TEAM_FILE_PLACES {
        let path = s._content.path().join(place);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            HOSTILE_TEAM,
            "{place}"
        );
    }
    s.decisions_from_the_user_database_only().await;
}

/// Slice U7's proof, part 2: the same services start with no `team.yaml` anywhere, and decide
/// the same.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_services_start_and_decide_with_no_team_yaml() {
    let s = start().await;
    let found: Vec<_> = walkdir::WalkDir::new(s._content.path())
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_name() == "team.yaml")
        .collect();
    assert!(found.is_empty(), "{found:?}");
    s.decisions_from_the_user_database_only().await;
}

// --- Password sign-in (docs/password-sign-in.md) ----------------------------------------------

/// The value of the cookie `name` that `headers` set, if any (empty when cleared).
fn set_cookie(headers: &reqwest::header::HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.strip_prefix(&format!("{name}=")))
        .map(|v| v.split(';').next().unwrap_or_default().to_string())
        .next_back()
}

impl Stack {
    /// A call with exactly these cookies (no session unless given), from the app's origin.
    async fn with_cookies(
        &self,
        cookies: &[(&str, &str)],
        csrf: Option<&str>,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value, reqwest::header::HeaderMap) {
        let mut req = self
            .http
            .request(method.clone(), format!("{}{path}", self.web))
            .header("origin", self.web.clone());
        let jar: Vec<String> = cookies.iter().map(|(k, v)| format!("{k}={v}")).collect();
        if !jar.is_empty() {
            req = req.header("cookie", jar.join("; "));
        }
        if let Some(c) = csrf {
            req = req.header("x-pmos-csrf", c);
        }
        req = req.json(&body.unwrap_or_else(|| json!({})));
        let res = req.send().await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let body = res.json().await.unwrap_or(Value::Null);
        (status, body, headers)
    }

    /// A browser for the session cookie `headers` set.
    async fn browser_from(&self, headers: &reqwest::header::HeaderMap) -> Browser {
        let cookie = set_cookie(headers, "__Host-pmos").expect("a session cookie");
        let mut b = Browser {
            cookie,
            csrf: String::new(),
        };
        let (status, me, _) = self.send(Some(&b), Method::GET, "/api/me", None).await;
        assert_eq!(status, StatusCode::OK, "{me}");
        b.csrf = me["csrf_token"].as_str().unwrap().to_string();
        b
    }

    /// The whole set-up with `code`: the enrolment secret, the recovery codes and the browser.
    async fn password_setup(&self, code: &str, password: &str) -> (Vec<u8>, Vec<String>, Browser) {
        let (status, begun, h) = self
            .with_cookies(
                &[],
                None,
                Method::POST,
                "/api/auth/password/setup/begin",
                Some(json!({"sign_in_name": "ana@example.com", "setup_code": code})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{begun}");
        assert_eq!(begun["handle"], "ana");
        assert_eq!(begun["password"]["min_length"], 12);
        let setup = set_cookie(&h, "__Host-pmos-setup").unwrap();
        let jar = [("__Host-pmos-setup", setup.as_str())];
        let (status, body, _) = self
            .with_cookies(
                &jar,
                None,
                Method::POST,
                "/api/auth/password/setup/password",
                Some(json!({"password": "password1234"})),
            )
            .await;
        assert_eq!(
            (status, body["code"].as_str(), body["reason"].as_str()),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Some("weak_password"),
                Some("common")
            )
        );
        let (status, _, _) = self
            .with_cookies(
                &jar,
                None,
                Method::POST,
                "/api/auth/password/setup/password",
                Some(json!({"password": password})),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, enrol, _) = self
            .with_cookies(
                &jar,
                None,
                Method::POST,
                "/api/auth/password/setup/authenticator",
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{enrol}");
        assert!(enrol["otpauth_uri"]
            .as_str()
            .unwrap()
            .starts_with("otpauth://totp/"));
        let secret = pmos_toolserver::password::totp::unbase32(enrol["secret"].as_str().unwrap())
            .unwrap()
            .to_vec();
        let code = pmos_toolserver::password::totp::code_at(
            &secret,
            pmos_toolserver::password::totp::step_at(Utc::now().timestamp()),
        );
        let (status, done, h) = self
            .with_cookies(
                &jar,
                None,
                Method::POST,
                "/api/auth/password/setup/confirm",
                Some(json!({"code": code})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{done}");
        assert_eq!(
            set_cookie(&h, "__Host-pmos-setup").as_deref(),
            Some(""),
            "the setup cookie is cleared"
        );
        let codes: Vec<String> = done["recovery_codes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap().to_string())
            .collect();
        assert_eq!(codes.len(), 10);
        let b = self.browser_from(&h).await;
        (secret, codes, b)
    }

    /// Password, then a recovery code: a browser.
    async fn password_sign_in(&self, password: &str, recovery: &str) -> Browser {
        let (status, body, h) = self
            .with_cookies(
                &[],
                None,
                Method::POST,
                "/api/auth/password/sign-in",
                Some(json!({"sign_in_name": "ana@example.com", "password": password})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            set_cookie(&h, "__Host-pmos").is_none(),
            "a password alone is no session"
        );
        let pending = set_cookie(&h, "__Host-pmos-pending").unwrap();
        let (status, body, h) = self
            .with_cookies(
                &[("__Host-pmos-pending", &pending)],
                None,
                Method::POST,
                "/api/auth/password/second-step",
                Some(json!({"recovery_code": recovery})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["recovery_code_used"], true);
        self.browser_from(&h).await
    }
}

/// Password sign-in through `pmos-web` and the real tool server: an admin makes a setup code;
/// Ana sets up (password, authenticator, a code) and is signed in; she signs in again with
/// her password and the second step; the refusals look alike; a password change ends her
/// other sessions; a reset ends every one; a deactivation in the user database ends the
/// session within 10 seconds; and an address that keeps guessing is told to wait.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn password_sign_in_through_pmos_web_and_the_real_tool_server() {
    let s = start().await;
    let (status, providers, _) = s.send(None, Method::GET, "/api/auth/providers", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(providers["providers"], json!(["password", "dev"]));

    // The admin makes a setup code (shown once); the Team page shows it pending.
    let admin = s.login("stack-admin").await;
    let (status, issued, _) = s
        .send(
            Some(&admin),
            Method::POST,
            "/api/admin/users/ana/password-setup-code",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{issued}");
    assert_eq!(issued["kind"], "setup");
    let code = issued["setup_code"].as_str().unwrap().to_string();
    let (_, list, _) = s
        .send(Some(&admin), Method::GET, "/api/admin/users", None)
        .await;
    let ana = list["users"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["handle"] == "ana")
        .unwrap()
        .clone();
    assert_eq!(
        (
            ana["setup_code_pending"].as_bool(),
            ana["password_set"].as_bool()
        ),
        (Some(true), Some(false))
    );
    // Never for oneself; a PM can't make one.
    let (status, body, _) = s
        .send(
            Some(&admin),
            Method::POST,
            "/api/admin/users/stack-admin/password-setup-code",
            None,
        )
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("self_change"))
    );
    let ben = s.login("ben").await;
    let (status, _, _) = s
        .send(
            Some(&ben),
            Method::POST,
            "/api/admin/users/ana/password-setup-code",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Set-up: Ana is signed in at the end, and her status says so.
    let (_secret, recovery, first) = s
        .password_setup(&code, "correct horse battery staple")
        .await;
    let (status, st, _) = s
        .send(Some(&first), Method::GET, "/api/auth/password/status", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{st}");
    assert_eq!(
        st,
        json!({"enabled": true, "password_set": true, "second_step_enrolled": true, "recovery_codes_left": 10,
               "needs_host_setup": false, "reset_notice": null})
    );
    // The code is spent.
    let (status, body, _) = s
        .with_cookies(
            &[],
            None,
            Method::POST,
            "/api/auth/password/setup/begin",
            Some(json!({"sign_in_name": "ana@example.com", "setup_code": code})),
        )
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("setup_failed"))
    );

    // Sign in twice more (two browsers), each with the password and then the second step.
    let second = s
        .password_sign_in("correct horse battery staple", &recovery[0])
        .await;
    let third = s
        .password_sign_in("correct horse battery staple", &recovery[1])
        .await;
    // The refusals at the password step look alike: a wrong password, nobody, no password.
    let mut answers = Vec::new();
    for (name, pw) in [
        ("ana@example.com", "not the right passphrase"),
        ("nobody@example.com", "correct horse battery staple"),
        ("ben@example.com", "correct horse battery staple"),
    ] {
        let (status, body, h) = s
            .with_cookies(
                &[],
                None,
                Method::POST,
                "/api/auth/password/sign-in",
                Some(json!({"sign_in_name": name, "password": pw})),
            )
            .await;
        assert!(set_cookie(&h, "__Host-pmos-pending").is_none());
        answers.push((status, body));
    }
    assert!(answers.iter().all(|a| a == &answers[0]), "{answers:?}");
    assert_eq!(answers[0].0, StatusCode::UNAUTHORIZED);
    assert_eq!(answers[0].1["code"], "sign_in_failed");
    // A second step without the pending cookie: start again.
    let (status, body, _) = s
        .with_cookies(
            &[],
            None,
            Method::POST,
            "/api/auth/password/second-step",
            Some(json!({"code": "123456"})),
        )
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("sign_in_expired"))
    );

    // A password change (from the second browser) ends the others; the changer keeps going.
    let (status, body, _) = s
        .with_cookies(
            &[("__Host-pmos", &second.cookie)],
            Some(&second.csrf),
            Method::POST,
            "/api/auth/password/change",
            Some(
                json!({"current_password": "correct horse battery staple", "code": "000000",
                        "new_password": "a brand new passphrase"}),
            ),
        )
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("reauth_failed")),
        "{body}"
    );
    let secret = _secret;
    let step = pmos_toolserver::password::totp::step_at(Utc::now().timestamp());
    let mut changed = None;
    for next in [step + 1, step] {
        let code = pmos_toolserver::password::totp::code_at(&secret, next);
        let (status, _, h) = s
            .with_cookies(
                &[("__Host-pmos", &second.cookie)],
                Some(&second.csrf),
                Method::POST,
                "/api/auth/password/change",
                Some(
                    json!({"current_password": "correct horse battery staple", "code": code,
                            "new_password": "a brand new passphrase"}),
                ),
            )
            .await;
        if status == StatusCode::NO_CONTENT {
            changed = Some(h);
            break;
        }
    }
    let changed = changed.expect("the password change went through");
    let after = s.browser_from(&changed).await;
    for gone in [&first, &second, &third] {
        let (status, _, _) = s.send(Some(gone), Method::GET, "/api/me", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "the other sessions ended");
    }
    let (status, _, _) = s.send(Some(&after), Method::GET, "/api/drafts", None).await;
    assert_eq!(status, StatusCode::OK);

    // A reset (a new code from the admin) ends every session of hers, the new one included.
    let (status, issued, _) = s
        .send(
            Some(&admin),
            Method::POST,
            "/api/admin/users/ana/password-setup-code",
            None,
        )
        .await;
    assert_eq!(
        (status, issued["kind"].as_str()),
        (StatusCode::CREATED, Some("reset"))
    );
    let reset = issued["setup_code"].as_str().unwrap().to_string();
    let (_, recovery, fresh) = s.password_setup(&reset, "the reset passphrase here").await;
    let (status, _, _) = s.send(Some(&after), Method::GET, "/api/me", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = s.send(Some(&fresh), Method::GET, "/api/me", None).await;
    assert_eq!(status, StatusCode::OK);
    // The reset is never silent (PM2): her status names who reset it until she dismisses it.
    let (_, st, _) = s
        .send(Some(&fresh), Method::GET, "/api/auth/password/status", None)
        .await;
    assert_eq!(st["reset_notice"]["by"], "stack-admin", "{st}");
    let (status, _, _) = s
        .send(
            Some(&fresh),
            Method::POST,
            "/api/auth/password/reset-notice/dismiss",
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, st, _) = s
        .send(Some(&fresh), Method::GET, "/api/auth/password/status", None)
        .await;
    assert_eq!(st["reset_notice"], Value::Null, "{st}");
    let later = s
        .password_sign_in("the reset passphrase here", &recovery[0])
        .await;

    // A deactivation in the user database ends the session within 10 seconds.
    s.users
        .apply(
            &pmos_toolserver::users::Actor::web("stack-admin"),
            pmos_toolserver::users::Op::Deactivate {
                handle: "ana".into(),
            },
            Utc::now(),
        )
        .await
        .unwrap();
    *s.web_clock.lock().unwrap() += chrono::Duration::seconds(10);
    let (status, body, _) = s.send(Some(&later), Method::GET, "/api/drafts", None).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("not_a_member"))
    );
    let (status, body, _) = s
        .with_cookies(
            &[],
            None,
            Method::POST,
            "/api/auth/password/sign-in",
            Some(
                json!({"sign_in_name": "ana@example.com", "password": "the reset passphrase here"}),
            ),
        )
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("sign_in_failed"))
    );

    // An address that keeps guessing waits (20 refused guesses in 15 minutes; this test
    // serves without peer addresses, so every request shares one count).
    let mut limited = None;
    for _ in 0..25 {
        let (status, body, h) = s
            .with_cookies(
                &[],
                None,
                Method::POST,
                "/api/auth/password/sign-in",
                Some(
                    json!({"sign_in_name": "nobody@example.com", "password": "guessing guessing"}),
                ),
            )
            .await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            limited = Some((body, h));
            break;
        }
    }
    let (body, h) = limited.expect("the address was told to wait");
    assert_eq!(body["code"], "too_many_attempts");
    assert!(h.get("retry-after").is_some());

    // Writes without the app's origin are refused before anything is asked.
    let res = s
        .http
        .post(format!("{}/api/auth/password/sign-in", s.web))
        .json(&json!({"sign_in_name": "ana@example.com", "password": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}
