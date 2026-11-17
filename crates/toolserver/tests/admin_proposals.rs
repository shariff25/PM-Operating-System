//! Admin proposals at the tool server (`docs/policy-admin.md`, How a proposal is opened;
//! `docs/toolserver.md`, Admin routes), against a mock GitHub that keeps a real tree of git
//! blob ids:
//!
//! - `POST /internal/admin/proposals/open` opens the pull request **as the admin**, with
//!   their own linked GitHub token from the vault (A2, A3): every GitHub call carries their
//!   token, the author is their login, and nothing but a new branch and a pull request is
//!   written.
//! - It trusts nothing `pmos-web` checked: the path allow-list (never `.github/`, `deploy/`,
//!   code, `team.yaml`), the caps, a policy file it doesn't regenerate itself, the content
//!   lint, a stale base against its own copy and against the default branch.
//! - Only `pmos-web` with a signed user assertion, in every assertion mode (`off` included),
//!   for an admin by this server's own team; an admin with no linked GitHub account gets
//!   `409 github_not_linked`.
//! - Rate limited per admin; audited first (no audit, no pull request); every refusal recorded
//!   with a fixed note, never file text (A4).
//! - An empty content folder never makes the repository root writable (A6).
//! - The proposals list and the branch-protection check use the viewer's own account.
//! - `POST /internal/admin/actions`: what `pmos-web` refused, and an admin's sensitive reads.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chrono::{Duration, Utc};
use common::Fixture;
use pmos_assertion::{unix_now, Claims, Key, KeyRing, RequestBinding};
use pmos_contract::model::Role;
use pmos_policy::admin::changeset::{proposed_files, Layout, ProposedFile};
use pmos_policy::admin::evaluate::{evaluate, Roots};
use pmos_policy::admin::proposals::{git_blob_sha, Change};
use pmos_toolserver::app::BotAccess;
use pmos_toolserver::assertion::{parse_keys, Assertions, Mode, AUDIENCE, ISSUERS};
use pmos_toolserver::github::GitHub;
use pmos_toolserver::oauth::{AppCredentials, Endpoints, OAuth, OAuthConfig};
use pmos_toolserver::onboarding::{GitHubSetup, Onboarding};
use pmos_toolserver::vault::{Credential, KeyRing as VaultKeys, Provider, Secret, Vault};
use pmos_toolserver::{router_full, AppConfig, PersonClients};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const SERVICE_TOKEN: &str = "test-service-token-0123456789abcdef";
const BOT_TOKEN: &str = "test-bot-token-0123456789abcdef-0123";
const WEB_TOKEN: &str = "test-web-token-0123456789abcdef-01234";
const CONSOLE_TOKEN: &str = "test-console-token-0123456789abcdef-0";
// Test key only: fixed bytes, never used anywhere real.
const WEB_KEY: &str = "d2ViLXRvb2xzZXJ2ZXItdGVzdC1rZXktMDEyMzQ1Njc=";
const REPO: &str = "acme/pmos";
const SKILL: &str = "content/skills/docs/SKILL.md";
const DOCS_POLICY: &str = "policies/bots/docs.yaml";

// ----- the mock GitHub --------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Req {
    method: String,
    path: String,
    query: String,
    login: Option<String>,
    body: Value,
}

#[derive(Default)]
struct Git {
    blobs: BTreeMap<String, String>,
    /// tree sha -> path -> blob sha
    trees: BTreeMap<String, BTreeMap<String, String>>,
    /// commit sha -> (tree, parents)
    commits: BTreeMap<String, (String, Vec<String>)>,
    refs: BTreeMap<String, String>,
    pulls: Vec<Value>,
}

#[derive(Default)]
struct Mock {
    log: Mutex<Vec<Req>>,
    git: Mutex<Git>,
    counter: AtomicU64,
    /// What `GET …/branches/main/protection` answers (status, body).
    protection: Mutex<(u16, Value)>,
    /// What `GET …/rules/branches/main` answers.
    rules: Mutex<(u16, Value)>,
}

impl Mock {
    fn sha(&self) -> String {
        format!("{:040x}", self.counter.fetch_add(1, Ordering::SeqCst) + 1)
    }

    fn requests(&self) -> Vec<Req> {
        self.log.lock().unwrap().clone()
    }

    fn writes(&self) -> Vec<Req> {
        self.requests()
            .into_iter()
            .filter(|r| r.method != "GET")
            .collect()
    }

    /// The default branch with `files` (repository path → text) as one commit.
    fn seed(&self, files: &BTreeMap<String, String>) {
        let mut git = self.git.lock().unwrap();
        let mut tree = BTreeMap::new();
        for (p, t) in files {
            let sha = git_blob_sha(t.as_bytes());
            git.blobs.insert(sha.clone(), t.clone());
            tree.insert(p.clone(), sha);
        }
        let tree_sha = self.sha();
        git.trees.insert(tree_sha.clone(), tree);
        let commit = self.sha();
        git.commits.insert(commit.clone(), (tree_sha, vec![]));
        git.refs.insert("main".into(), commit);
    }

    /// Someone else changes `path` on the default branch.
    fn push_change(&self, path: &str, text: &str) {
        let mut git = self.git.lock().unwrap();
        let head = git.refs["main"].clone();
        let mut tree = git.trees[&git.commits[&head].0].clone();
        let sha = git_blob_sha(text.as_bytes());
        git.blobs.insert(sha.clone(), text.into());
        tree.insert(path.into(), sha);
        let tree_sha = self.sha();
        git.trees.insert(tree_sha.clone(), tree);
        let commit = self.sha();
        git.commits.insert(commit.clone(), (tree_sha, vec![head]));
        git.refs.insert("main".into(), commit);
    }

    /// Path → text at a branch.
    fn files_at(&self, branch: &str) -> BTreeMap<String, String> {
        let git = self.git.lock().unwrap();
        let commit = &git.refs[branch];
        git.trees[&git.commits[commit].0]
            .iter()
            .map(|(p, s)| (p.clone(), git.blobs[s].clone()))
            .collect()
    }
}

fn reply(status: u16, body: Value) -> Response {
    (StatusCode::from_u16(status).unwrap(), Json(body)).into_response()
}

async fn github(
    State(m): State<Arc<Mock>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let login = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|a| a.strip_prefix("Bearer ghu-"))
        .and_then(|t| t.rsplit_once('-'))
        .map(|(l, _)| l.to_string());
    let req = Req {
        method: method.to_string(),
        path: uri.path().to_string(),
        query: uri.query().unwrap_or_default().to_string(),
        login: login.clone(),
        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
    };
    m.log.lock().unwrap().push(req.clone());
    let Some(login) = login else {
        return reply(401, json!({"message": "Bad credentials"}));
    };
    let Some(rest) = req.path.strip_prefix(&format!("/repos/{REPO}")) else {
        return reply(404, json!({"message": "Not Found"}));
    };
    let parts: Vec<&str> = rest.trim_start_matches('/').split('/').collect();
    let b = &req.body;
    match (req.method.as_str(), parts.as_slice()) {
        ("GET", ["git", "ref", "heads", branch @ ..]) => {
            let git = m.git.lock().unwrap();
            match git.refs.get(&branch.join("/")) {
                Some(sha) => reply(200, json!({"object": {"sha": sha, "type": "commit"}})),
                None => reply(404, json!({"message": "Not Found"})),
            }
        }
        ("GET", ["git", "commits", sha]) => {
            let git = m.git.lock().unwrap();
            match git.commits.get(*sha) {
                Some((tree, _)) => reply(200, json!({"sha": sha, "tree": {"sha": tree}})),
                None => reply(404, json!({"message": "Not Found"})),
            }
        }
        ("GET", ["git", "trees", sha]) => {
            let git = m.git.lock().unwrap();
            match git.trees.get(*sha) {
                Some(t) => reply(
                    200,
                    json!({"sha": sha, "truncated": false, "tree": t.iter().map(|(p, s)| json!({"path": p, "sha": s, "type": "blob", "mode": "100644"})).collect::<Vec<_>>()}),
                ),
                None => reply(404, json!({"message": "Not Found"})),
            }
        }
        ("POST", ["git", "blobs"]) => {
            let text =
                String::from_utf8(B64.decode(b["content"].as_str().unwrap()).unwrap()).unwrap();
            let sha = git_blob_sha(text.as_bytes());
            m.git.lock().unwrap().blobs.insert(sha.clone(), text);
            reply(201, json!({"sha": sha}))
        }
        ("POST", ["git", "trees"]) => {
            let sha = m.sha();
            let mut git = m.git.lock().unwrap();
            let mut files = git.trees[b["base_tree"].as_str().unwrap()].clone();
            for e in b["tree"].as_array().unwrap() {
                let path = e["path"].as_str().unwrap().to_string();
                match e["sha"].as_str() {
                    Some(s) => {
                        files.insert(path, s.to_string());
                    }
                    None => {
                        files.remove(&path);
                    }
                }
            }
            git.trees.insert(sha.clone(), files);
            reply(201, json!({"sha": sha}))
        }
        ("POST", ["git", "commits"]) => {
            let sha = m.sha();
            let parents = b["parents"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p.as_str().unwrap().to_string())
                .collect();
            m.git.lock().unwrap().commits.insert(
                sha.clone(),
                (b["tree"].as_str().unwrap().to_string(), parents),
            );
            reply(201, json!({"sha": sha}))
        }
        ("POST", ["git", "refs"]) => {
            let name = b["ref"]
                .as_str()
                .unwrap()
                .strip_prefix("refs/heads/")
                .unwrap()
                .to_string();
            let mut git = m.git.lock().unwrap();
            if git.refs.contains_key(&name) {
                return reply(422, json!({"message": "Reference already exists"}));
            }
            git.refs
                .insert(name, b["sha"].as_str().unwrap().to_string());
            reply(201, json!({"ref": b["ref"]}))
        }
        ("POST", ["pulls"]) => {
            let mut git = m.git.lock().unwrap();
            let n = git.pulls.len() as u64 + 1;
            let pr = json!({
                "number": n,
                "html_url": format!("https://github.example/{REPO}/pull/{n}"),
                "title": b["title"], "head": {"ref": b["head"]}, "base": {"ref": b["base"]},
                "draft": false, "user": {"login": login}, "labels": [], "body": b["body"],
                "created_at": "2026-11-17T10:00:00Z", "updated_at": "2026-11-17T10:00:00Z",
            });
            git.pulls.push(pr.clone());
            reply(201, pr)
        }
        ("POST", ["issues", n, "labels"]) => {
            let n: u64 = n.parse().unwrap();
            let mut git = m.git.lock().unwrap();
            for pr in git.pulls.iter_mut().filter(|p| p["number"] == n) {
                pr["labels"] = json!(b["labels"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|l| json!({"name": l}))
                    .collect::<Vec<_>>());
            }
            reply(200, json!([]))
        }
        ("GET", ["pulls"]) => {
            let mut all = m.git.lock().unwrap().pulls.clone();
            all.push(json!({"number": 99, "html_url": "x", "title": "Fix typo",
                "head": {"ref": "fix-typo"}, "base": {"ref": "main"}, "draft": false,
                "user": {"login": "someone"}, "labels": [], "body": "hi"}));
            all.reverse();
            reply(200, json!(all))
        }
        ("GET", ["branches", "main", "protection"]) => {
            let (s, v) = m.protection.lock().unwrap().clone();
            reply(s, v)
        }
        ("GET", ["rules", "branches", "main"]) => {
            let (s, v) = m.rules.lock().unwrap().clone();
            reply(s, v)
        }
        _ => panic!(
            "the mock GitHub has no route for {} {}",
            req.method, req.path
        ),
    }
}

// ----- the system under test --------------------------------------------------------------

struct Server {
    base: String,
    gh: Arc<Mock>,
    f: Fixture,
    policies: PathBuf,
    cancel: CancellationToken,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

struct Opts {
    mode: Mode,
    per_hour: u32,
    content_dir: &'static str,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            mode: Mode::Off,
            per_hour: 10,
            content_dir: "content",
        }
    }
}

fn files_under(dir: &Path, prefix: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for e in walkdir::WalkDir::new(dir) {
        let e = e.unwrap();
        if e.file_type().is_file() {
            let rel = e.path().strip_prefix(dir).unwrap().to_string_lossy();
            out.insert(
                format!("{prefix}/{rel}"),
                std::fs::read_to_string(e.path()).unwrap(),
            );
        }
    }
    out
}

/// Ada and Kim are admins (only Ada has linked GitHub, as `ada-gh`); Aud is an auditor; Ana
/// a PM. The mock's default branch holds the fixture's content and its generated policies.
async fn start(opts: Opts) -> Server {
    let (f, _) = common::roles_fixture(
        &[
            ("ada", Role::Admin),
            ("kim", Role::Admin),
            ("aud", Role::Auditor),
            ("ana", Role::Pm),
        ],
        false,
    )
    .await
    .unwrap();
    let policies = f.root.parent().unwrap().join("policies");
    let generated = pmos_policy::generate(&f.root, Utc::now().date_naive())
        .expect("the fixture's content generates");
    pmos_policy::write(&policies, &generated).unwrap();

    let gh = Arc::new(Mock::default());
    *gh.protection.lock().unwrap() = (
        200,
        json!({"required_pull_request_reviews": {"required_approving_review_count": 1}}),
    );
    *gh.rules.lock().unwrap() = (200, json!([]));
    let mut seed = files_under(&f.root, "content");
    seed.extend(files_under(&policies, "policies"));
    seed.insert("README.md".into(), "# PM OS\n".into());
    gh.seed(&seed);
    let app = Router::new().fallback(github).with_state(gh.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gh_url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let vault = Vault::new(
        VaultKeys::from_spec(&VaultKeys::generate_spec("k1")).unwrap(),
        f.stores.credentials.clone(),
    );
    let oauth = Arc::new(OAuth::new(
        OAuthConfig {
            public_base_url: "https://pmos.example.com".into(),
            atlassian: None,
            atlassian_site: None,
            slack: None,
            github: Some(AppCredentials {
                client_id: "gh-client".into(),
                client_secret: Secret::new("gh-secret"),
                scopes: Vec::new(),
            }),
            github_org: Some("acme".into()),
            state_key: b"test-state-key".to_vec(),
            endpoints: Endpoints {
                github_auth: gh_url.clone(),
                github_api: gh_url.clone(),
                ..Endpoints::default()
            },
        },
        vault.clone(),
        f.svc.people().clone(),
    ));
    vault
        .put(&Credential {
            user: "ada".into(),
            provider: Provider::Github,
            access_token: Secret::new("ghu-ada-gh-1"),
            refresh_token: Some(Secret::new("ghr-ada-gh-1")),
            expires_at: Some(Utc::now() + Duration::hours(8)),
            scopes: String::new(),
            cloud_id: None,
            site_url: None,
            external_id: Some("ada-gh".into()),
            updated_at: Utc::now(),
        })
        .await
        .unwrap();
    let onboarding = Onboarding::new(
        f.svc.clone(),
        Some(oauth.clone()),
        Some(GitHubSetup {
            github: Arc::new(GitHub::new(gh_url)),
            org: "acme".into(),
            content_repo: Some(REPO.into()),
            content_branch: "main".into(),
            content_dir: opts.content_dir.into(),
            policies_dir: "policies".into(),
            content_root: f.root.clone(),
        }),
    )
    .with_proposals(opts.per_hour, Some(policies.clone()));

    let keys = parse_keys(
        &format!("web:toolserver:w1:{WEB_KEY}"),
        "the tool server",
        |iss, aud| aud == AUDIENCE && ISSUERS.contains(&iss),
    )
    .unwrap();
    let cancel = CancellationToken::new();
    let app = router_full(
        Arc::clone(&f.svc),
        AppConfig {
            service_token: SERVICE_TOKEN.into(),
            bot: Some(BotAccess {
                token: BOT_TOKEN.into(),
                skills: vec!["docs".into()],
            }),
            console_token: Some(CONSOLE_TOKEN.into()),
            content_root: f.root.clone(),
            oauth: Some(oauth),
            allowed_hosts: vec!["127.0.0.1".into(), "localhost".into()],
        },
        Some(Arc::new(onboarding)),
        PersonClients {
            web_token: Some(WEB_TOKEN.into()),
            assertions: Arc::new(Assertions::new(opts.mode, keys).unwrap()),
        },
        cancel.clone(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let ct = cancel.clone();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { ct.cancelled_owned().await })
            .await
            .unwrap();
    });
    Server {
        base,
        gh,
        f,
        policies,
        cancel,
    }
}

fn sign(sub: &str, method: &str, target: &str, body: &[u8]) -> String {
    let ring = KeyRing::new(Key::from_base64("w1", WEB_KEY).unwrap());
    let claims = Claims::new("web", "toolserver", sub, &[], "unknown").unwrap();
    let req = RequestBinding::new(method, target, body).unwrap();
    ring.sign(&claims, unix_now(), 60, Some(&req)).unwrap()
}

impl Server {
    async fn send(
        &self,
        method: reqwest::Method,
        target: &str,
        token: Option<&str>,
        assertion: Option<&str>,
        body: Option<&[u8]>,
    ) -> (u16, Value, reqwest::header::HeaderMap) {
        let mut r = reqwest::Client::new().request(method, format!("{}{target}", self.base));
        if let Some(t) = token {
            r = r.bearer_auth(t);
        }
        if let Some(a) = assertion {
            r = r.header("x-pmos-assertion", a);
        }
        if let Some(b) = body {
            r = r
                .header("content-type", "application/json")
                .body(b.to_vec());
        }
        let res = r.send().await.unwrap();
        let status = res.status().as_u16();
        let headers = res.headers().clone();
        let text = res.text().await.unwrap();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::Null),
            headers,
        )
    }

    /// `pmos-web`'s call for `user`, signed for them.
    async fn web(
        &self,
        method: reqwest::Method,
        user: &str,
        target: &str,
        body: Option<&Value>,
    ) -> (u16, Value) {
        let bytes = body.map(|b| serde_json::to_vec(b).unwrap());
        let a = sign(
            user,
            method.as_str(),
            target,
            bytes.as_deref().unwrap_or(b""),
        );
        let (s, v, _) = self
            .send(method, target, Some(WEB_TOKEN), Some(&a), bytes.as_deref())
            .await;
        (s, v)
    }

    async fn open(&self, user: &str, body: &Value) -> (u16, Value) {
        self.web(
            reqwest::Method::POST,
            user,
            &format!("/internal/admin/proposals/open?user={user}"),
            Some(body),
        )
        .await
    }

    /// Audit events of `action`, newest first, as an admin reads them.
    async fn events(&self, action: &str) -> Vec<Value> {
        let target = format!("/internal/admin/audit?user=ada&action={action}");
        let a = sign("ada", "GET", &target, b"");
        let (status, body, _) = self
            .send(
                reqwest::Method::GET,
                &target,
                Some(WEB_TOKEN),
                Some(&a),
                None,
            )
            .await;
        assert_eq!(status, 200, "{body}");
        body["events"].as_array().unwrap().clone()
    }

    /// The change set `pmos-web` would send for `content` changes (content-relative).
    fn change_set(&self, kind: &str, content: Vec<Change>) -> Value {
        let layout = Layout::new("content", "policies").unwrap();
        let e = evaluate(
            &Roots {
                content: &self.f.root,
                policies: Some(&self.policies),
                content_dir: "content",
                policies_dir: "policies",
            },
            &content,
            Utc::now().date_naive(),
        )
        .unwrap();
        assert!(e.valid, "{:?}", e.issues);
        body(kind, proposed_files(&layout, &content, &e))
    }

    fn skill_with(&self, tools: &str) -> Change {
        let text = std::fs::read_to_string(self.f.root.join("skills/docs/SKILL.md"))
            .unwrap()
            .replace("docs.search docs.read", tools);
        Change {
            rel: "skills/docs/SKILL.md".into(),
            text: Some(text),
        }
    }
}

fn body(kind: &str, changes: Vec<ProposedFile>) -> Value {
    json!({
        "kind": kind,
        "title": "Let the Docs bot search Jira",
        "reason": "PMs asked @everyone ``` fence",
        "base_sha": null,
        "changes": changes,
    })
}

// ----- opening ----------------------------------------------------------------------------

#[tokio::test]
async fn an_admins_proposal_is_opened_with_their_own_github_account() {
    let srv = start(Opts::default()).await;
    let before = srv.gh.files_at("main");
    let set = srv.change_set(
        "policy",
        vec![srv.skill_with("docs.search docs.read jira.search")],
    );
    let (status, r) = srv.open("ada", &set).await;
    assert_eq!(status, 201, "{r}");
    assert_eq!(r["number"], 1);
    assert_eq!(
        r["author"], "ada-gh",
        "the admin is the pull request's author"
    );
    assert_eq!(r["files"], json!([SKILL, DOCS_POLICY]));
    assert_eq!(r["widening"], true);
    assert_eq!(r["stale_check"], "passed");
    assert_eq!(r["labels_added"], true);
    let branch = r["branch"].as_str().unwrap().to_string();
    assert!(branch.starts_with("pmos/proposal/policy/ada-"), "{branch}");

    // Every GitHub call as Ada, and only these: no update of an existing ref, no merge.
    let calls = srv.gh.requests();
    assert!(calls.iter().all(|c| c.login.as_deref() == Some("ada-gh")));
    let seq: Vec<String> = calls
        .iter()
        .map(|c| {
            let p = c.path.trim_start_matches(&format!("/repos/{REPO}"));
            // A read of one object: its sha left out.
            let one_object = c.method == "GET"
                && (p.starts_with("/git/commits/") || p.starts_with("/git/trees/"));
            let p = if one_object {
                p.rsplit_once('/').unwrap().0
            } else {
                p
            };
            format!("{} {p}", c.method)
        })
        .collect();
    assert_eq!(
        seq,
        [
            "GET /git/ref/heads/main",
            "GET /git/commits",
            "GET /git/trees",
            "POST /git/blobs",
            "POST /git/blobs",
            "POST /git/trees",
            "POST /git/commits",
            "POST /git/refs",
            "POST /pulls",
            "POST /issues/1/labels",
        ]
    );
    // The default branch is untouched; the new branch differs from it in exactly the two
    // files, which are what the tool server regenerated.
    assert_eq!(srv.gh.files_at("main"), before);
    let after = srv.gh.files_at(&branch);
    let changed: Vec<&String> = after
        .keys()
        .chain(before.keys())
        .filter(|p| after.get(*p) != before.get(*p))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    assert_eq!(changed, [SKILL, DOCS_POLICY]);
    assert!(after[SKILL].contains("docs.search docs.read jira.search"));
    assert!(after[DOCS_POLICY].contains("- jira_search"));
    let pr = srv.gh.git.lock().unwrap().pulls[0].clone();
    let text = pr["body"].as_str().unwrap();
    assert!(text.contains("by `ada`"), "{text}");
    assert!(text.contains("their own linked GitHub account"));
    assert!(text.contains("**WIDER**"));
    assert!(
        text.contains("```text\nPMs asked @everyone ''' fence\n```"),
        "{text}"
    );
    assert_eq!(pr["base"]["ref"], "main");
    assert_eq!(
        pr["labels"],
        json!([{"name": "pmos-proposal"}, {"name": "pmos-proposal:policy"}])
    );

    // Audited: started, then ok with the pull request; paths as a count, never text.
    let events = srv.events("admin.policy_proposal").await;
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[0]["outcome"], "ok");
    assert_eq!(events[0]["target"], format!("github:{REPO}#1"));
    assert_eq!(events[1]["outcome"], "started");
    for e in &events {
        assert_eq!(e["actor"], "ada");
        assert_eq!(e["client"], "web");
        assert_eq!(e["sources"], 2);
    }
    let stored = serde_json::to_string(&srv.f.audit.events()).unwrap();
    assert!(!stored.contains("jira_search") && !stored.contains("tools:"));
    assert!(!stored.contains("ghu-ada"), "never the token");

    // Listed, with Ada's own account; someone else's pull request isn't.
    let (status, l) = srv
        .web(
            reqwest::Method::GET,
            "ada",
            "/internal/admin/proposals?user=ada&kind=policy",
            None,
        )
        .await;
    assert_eq!(status, 200, "{l}");
    assert_eq!(l["github"], "linked");
    let list = l["proposals"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["proposed_by"], "ada");
    assert_eq!(list[0]["author"], "ada-gh");
    let read = srv.gh.requests();
    let listing = read.last().unwrap();
    assert_eq!(listing.path, format!("/repos/{REPO}/pulls"));
    assert!(listing.query.contains("state=open"), "{}", listing.query);
    assert_eq!(listing.login.as_deref(), Some("ada-gh"));
    let (_, l) = srv
        .web(
            reqwest::Method::GET,
            "ada",
            "/internal/admin/proposals?user=ada&kind=content",
            None,
        )
        .await;
    assert!(l["proposals"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn the_tool_server_checks_everything_again() {
    let srv = start(Opts::default()).await;
    let good = srv.change_set(
        "policy",
        vec![srv.skill_with("docs.search docs.read jira.search")],
    );
    let blob = |path: &str| -> Value {
        let rel = path.strip_prefix("content/").unwrap();
        json!(git_blob_sha(
            std::fs::read_to_string(srv.f.root.join(rel))
                .unwrap()
                .as_bytes()
        ))
    };
    let with_changes = |kind: &str, changes: Value| {
        let mut b = good.clone();
        b["kind"] = json!(kind);
        b["changes"] = changes;
        b
    };
    let mut cases: Vec<(&str, Value, u16, &str)> = Vec::new();
    // Outside the allow-list, whatever pmos-web said.
    for path in [
        ".github/workflows/ci.yml",
        "deploy/compose.yaml",
        "Cargo.toml",
        "crates/web/src/app.rs",
        "README.md",
        "content/team.yaml",
        "content/.hidden.md",
        "content/../deploy/compose.yaml",
        "policies/README.md",
        "policies/bots/../../deploy/x.yaml",
    ] {
        cases.push((
            path,
            with_changes(
                "content",
                json!([{"path": path, "text": "x", "base_blob_sha": null}]),
            ),
            400,
            "",
        ));
    }
    // A policy file the tool server doesn't regenerate: tampered with, or left out.
    let mut tampered = good.clone();
    let t = tampered["changes"][1]["text"]
        .as_str()
        .unwrap()
        .replace("host: gateway.pmos.internal", "host: evil.example.com");
    tampered["changes"][1]["text"] = json!(t);
    cases.push(("tampered policy", tampered, 422, "policy_mismatch"));
    let mut left_out = good.clone();
    left_out["changes"] = json!([good["changes"][0].clone()]);
    cases.push(("policy left out", left_out, 422, "policy_mismatch"));
    let mut extra = good.clone();
    extra["changes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"path": "policies/bots/extra.yaml", "text": "x", "base_blob_sha": null}));
    cases.push(("extra policy file", extra, 422, "policy_mismatch"));
    // The content lint, run here.
    let role = std::fs::read_to_string(srv.f.root.join("roles/docs-helper/ROLE.md"))
        .unwrap()
        .replace("skills: \"docs\"", "skills: \"docs no-such-skill\"");
    cases.push((
        "lint error",
        with_changes(
            "content",
            json!([{"path": "content/roles/docs-helper/ROLE.md", "text": role, "base_blob_sha": blob("content/roles/docs-helper/ROLE.md")}]),
        ),
        422,
        "invalid",
    ));
    // A policy proposal changes bot, role and skill definitions only.
    cases.push((
        "policy kind, playbook",
        with_changes(
            "policy",
            json!([{"path": "content/playbooks/new.md", "text": "---\ntitle: New\nowner: \"ada\"\nversion: \"0.1.0\"\n---\n\nBody\n", "base_blob_sha": null}]),
        ),
        400,
        "path_not_allowed",
    ));
    // Computed from another copy than this server's.
    let mut stale = good.clone();
    stale["changes"][0]["base_blob_sha"] = json!(git_blob_sha(b"another copy"));
    cases.push(("stale here", stale, 409, "stale_base"));
    // Caps.
    let many: Vec<Value> = (0..51)
        .map(|i| json!({"path": format!("content/playbooks/p{i}.md"), "text": "x", "base_blob_sha": null}))
        .collect();
    cases.push((
        "51 files",
        with_changes("content", json!(many)),
        413,
        "too_many_files",
    ));
    cases.push((
        "too large",
        with_changes(
            "content",
            json!([{"path": "content/playbooks/big.md", "text": "a".repeat(256 * 1024 + 1), "base_blob_sha": null}]),
        ),
        413,
        "too_large",
    ));
    cases.push((
        "no changes",
        with_changes("content", json!([])),
        400,
        "no_change",
    ));
    let mut untitled = good.clone();
    untitled["title"] = json!(" \u{7} ");
    cases.push(("no title", untitled, 400, "bad_request"));
    let mut unknown = good.clone();
    unknown["merge"] = json!(true);
    cases.push(("unknown field", unknown, 400, "bad_request"));

    for (what, body, status, code) in &cases {
        let (s, r) = srv.open("ada", body).await;
        assert_eq!(s, *status, "{what}: {r}");
        if !code.is_empty() {
            assert_eq!(r["code"], *code, "{what}: {r}");
        }
    }
    assert!(srv.gh.writes().is_empty(), "nothing was written to GitHub");
    let mut denied = srv.events("admin.content_proposal").await;
    denied.extend(srv.events("admin.policy_proposal").await);
    assert!(denied.iter().all(|e| e["outcome"] == "denied"));
    let notes: std::collections::BTreeSet<String> = denied
        .iter()
        .map(|e| e["detail"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        notes,
        ["invalid", "policy mismatch", "stale base"]
            .into_iter()
            .map(String::from)
            .collect()
    );

    // The good one still opens.
    let (s, r) = srv.open("ada", &good).await;
    assert_eq!(s, 201, "{r}");
}

#[tokio::test]
async fn a_moved_default_branch_is_a_stale_base() {
    let srv = start(Opts::default()).await;
    srv.gh.push_change(SKILL, "someone else's edit\n");
    let set = srv.change_set("policy", vec![srv.skill_with("docs.search")]);
    let (s, r) = srv.open("ada", &set).await;
    assert_eq!(s, 409, "{r}");
    assert_eq!(r["code"], "stale_base");
    assert_eq!(r["paths"], json!([SKILL]));
    assert_eq!(r["base"]["branch"], "main");
    assert!(srv.gh.writes().is_empty());
    let e = srv.events("admin.policy_proposal").await;
    assert_eq!(e.len(), 1);
    assert_eq!(e[0]["outcome"], "denied");
    assert_eq!(e[0]["detail"], "stale base");
}

#[tokio::test]
async fn only_an_admin_with_a_linked_github_account() {
    let srv = start(Opts::default()).await;
    let set = srv.change_set("content", vec![srv.skill_with("docs.search")]);
    for who in ["aud", "ana"] {
        let (s, r) = srv.open(who, &set).await;
        assert_eq!(s, 403, "{who}: {r}");
        assert_eq!(r["code"], "forbidden");
    }
    // An admin who hasn't linked GitHub: told so, nothing written.
    let (s, r) = srv.open("kim", &set).await;
    assert_eq!(s, 409, "{r}");
    assert_eq!(r["code"], "github_not_linked");
    assert!(srv.gh.requests().is_empty());
    let e = srv.events("admin.content_proposal").await;
    let notes: Vec<(&str, &str)> = e
        .iter()
        .map(|e| (e["actor"].as_str().unwrap(), e["detail"].as_str().unwrap()))
        .collect();
    assert_eq!(
        notes,
        [
            ("kim", "github not linked"),
            ("ana", "role"),
            ("aud", "role")
        ]
    );
    // Kim's list is an empty state that says why, and so is the protection check.
    let (s, l) = srv
        .web(
            reqwest::Method::GET,
            "kim",
            "/internal/admin/proposals?user=kim&kind=content",
            None,
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(l["github"], "not_linked");
    assert_eq!(l["proposals"], json!([]));
    let (_, p) = srv
        .web(
            reqwest::Method::GET,
            "kim",
            "/internal/admin/protection?user=kim",
            None,
        )
        .await;
    assert_eq!(p["verified"], false);
    assert_eq!(p["github"], "not_linked");
}

#[tokio::test]
async fn a_signed_assertion_is_needed_in_every_mode() {
    // `off`: the other routes would take pmos-web's word; these never do.
    let srv = start(Opts::default()).await;
    let set = srv.change_set("policy", vec![srv.skill_with("docs.search")]);
    let bytes = serde_json::to_vec(&set).unwrap();
    let target = "/internal/admin/proposals/open?user=ada";
    for t in [
        target,
        "/internal/admin/proposals?user=ada&kind=policy",
        "/internal/admin/protection?user=ada",
        "/internal/admin/actions?user=ada",
    ] {
        let method = if t.contains("open") || t.contains("actions") {
            reqwest::Method::POST
        } else {
            reqwest::Method::GET
        };
        let (s, r, _) = srv
            .send(method, t, Some(WEB_TOKEN), None, Some(&bytes))
            .await;
        assert_eq!(s, 401, "{t}: {r}");
        assert_eq!(r["code"], "bad_assertion");
    }
    // Signed for a PM, naming the admin.
    let a = sign("ana", "POST", target, &bytes);
    let (s, _, _) = srv
        .send(
            reqwest::Method::POST,
            target,
            Some(WEB_TOKEN),
            Some(&a),
            Some(&bytes),
        )
        .await;
    assert_eq!(s, 401);
    // Signed over another body.
    let other = serde_json::to_vec(&body("content", vec![])).unwrap();
    let a = sign("ada", "POST", target, &other);
    let (s, _, _) = srv
        .send(
            reqwest::Method::POST,
            target,
            Some(WEB_TOKEN),
            Some(&a),
            Some(&bytes),
        )
        .await;
    assert_eq!(s, 401);
    // Not pmos-web: the orchestrator, the Slack bot, the console; no token.
    for token in [SERVICE_TOKEN, BOT_TOKEN, CONSOLE_TOKEN] {
        let (s, _, _) = srv
            .send(
                reqwest::Method::POST,
                target,
                Some(token),
                None,
                Some(&bytes),
            )
            .await;
        assert_eq!(s, 403);
    }
    let (s, _, _) = srv
        .send(reqwest::Method::POST, target, None, None, Some(&bytes))
        .await;
    assert_eq!(s, 401);
    assert!(srv.gh.requests().is_empty());
    // And in `require`, the same.
    let srv = start(Opts {
        mode: Mode::Require,
        ..Opts::default()
    })
    .await;
    let (s, _, _) = srv
        .send(
            reqwest::Method::POST,
            target,
            Some(WEB_TOKEN),
            None,
            Some(&bytes),
        )
        .await;
    assert_eq!(s, 401);
    let (s, r) = srv.open("ada", &set).await;
    assert_eq!(s, 201, "{r}");
}

#[tokio::test]
async fn proposals_are_rate_limited_per_admin() {
    let srv = start(Opts {
        per_hour: 1,
        ..Opts::default()
    })
    .await;
    let (s, r) = srv
        .open(
            "ada",
            &srv.change_set("policy", vec![srv.skill_with("docs.search")]),
        )
        .await;
    assert_eq!(s, 201, "{r}");
    let set = srv.change_set("policy", vec![srv.skill_with("docs.read")]);
    let bytes = serde_json::to_vec(&set).unwrap();
    let target = "/internal/admin/proposals/open?user=ada";
    let a = sign("ada", "POST", target, &bytes);
    let (s, r, h) = srv
        .send(
            reqwest::Method::POST,
            target,
            Some(WEB_TOKEN),
            Some(&a),
            Some(&bytes),
        )
        .await;
    assert_eq!(s, 429, "{r}");
    assert_eq!(r["code"], "rate_limited");
    assert!(r["retry_after"].as_u64().unwrap() > 0);
    assert!(h.get("retry-after").is_some());
    let e = srv.events("admin.policy_proposal").await;
    assert_eq!(e[0]["outcome"], "denied");
    assert_eq!(e[0]["detail"], "rate limited");
}

#[tokio::test]
async fn no_audit_record_means_no_pull_request() {
    let srv = start(Opts::default()).await;
    srv.f.audit_switch.down.store(true, Ordering::SeqCst);
    let set = srv.change_set("policy", vec![srv.skill_with("docs.search")]);
    let (s, r) = srv.open("ada", &set).await;
    assert_eq!(s, 502, "{r}");
    assert_eq!(r["code"], "audit_unavailable");
    assert!(srv.gh.writes().is_empty(), "nothing written to GitHub");
}

#[tokio::test]
async fn an_empty_content_folder_never_makes_the_repository_root_writable() {
    let srv = start(Opts {
        content_dir: "",
        ..Opts::default()
    })
    .await;
    // Even a change set that names a file at the repository root.
    let set = json!({"kind": "content", "title": "t", "reason": "r", "base_sha": null,
        "changes": [{"path": "deploy/compose.yaml", "text": "x", "base_blob_sha": null}]});
    let (s, r) = srv.open("ada", &set).await;
    assert_eq!(s, 501, "{r}");
    assert_eq!(r["code"], "not_configured");
    assert!(srv.gh.requests().is_empty());
    let (_, p) = srv
        .web(
            reqwest::Method::GET,
            "ada",
            "/internal/admin/protection?user=ada",
            None,
        )
        .await;
    assert_eq!(p["configured"], false);
}

// ----- branch protection ------------------------------------------------------------------

#[tokio::test]
async fn branch_protection_is_read_with_the_viewers_account() {
    let srv = start(Opts::default()).await;
    let check = |who: &'static str| {
        let srv = &srv;
        async move {
            srv.web(
                reqwest::Method::GET,
                who,
                &format!("/internal/admin/protection?user={who}"),
                None,
            )
            .await
        }
    };
    let (s, p) = check("ada").await;
    assert_eq!(s, 200, "{p}");
    assert_eq!(p["required_reviews"], 1);
    assert_eq!(p["verified"], true);
    assert_eq!(p["source"], "branch_protection");
    assert_eq!(p["branch"], "main");
    assert_eq!(p["configured"], true);
    // Classic protection unreadable (no administration permission), a ruleset requires two.
    *srv.gh.protection.lock().unwrap() = (403, json!({"message": "Resource not accessible"}));
    *srv.gh.rules.lock().unwrap() = (
        200,
        json!([{"type": "pull_request", "parameters": {"required_approving_review_count": 2}}]),
    );
    let (_, p) = check("ada").await;
    assert_eq!(
        (p["required_reviews"].clone(), p["verified"].clone()),
        (json!(2), json!(true))
    );
    assert_eq!(p["source"], "rulesets");
    // Neither readable, or a ruleset that requires none: unknown, never an error.
    *srv.gh.rules.lock().unwrap() = (200, json!([]));
    let (s, p) = check("ada").await;
    assert_eq!(s, 200);
    assert_eq!(p["verified"], false);
    assert_eq!(p["required_reviews"], Value::Null);
    // Protected without required reviews: verified, zero.
    *srv.gh.protection.lock().unwrap() = (200, json!({"enforce_admins": {"enabled": true}}));
    let (_, p) = check("ada").await;
    assert_eq!(
        (p["required_reviews"].clone(), p["verified"].clone()),
        (json!(0), json!(true))
    );
    // An auditor may ask; a PM may not, and that refusal is recorded.
    let (s, _) = check("aud").await;
    assert_eq!(s, 200);
    let (s, _) = check("ana").await;
    assert_eq!(s, 403);
    let e = srv.events("admin.protection").await;
    assert_eq!(e.len(), 1);
    assert_eq!(e[0]["actor"], "ana");
    assert_eq!(e[0]["outcome"], "denied");
}

// ----- events pmos-web records --------------------------------------------------------------

async fn event(srv: &Server, user: &str, body: Value) -> (u16, Value) {
    srv.web(
        reqwest::Method::POST,
        user,
        &format!("/internal/admin/actions?user={user}"),
        Some(&body),
    )
    .await
}

#[tokio::test]
async fn pmos_web_records_refusals_and_sensitive_reads() {
    let srv = start(Opts::default()).await;
    // A role refusal pmos-web made: anyone on the team.
    let (s, r) = event(
        &srv,
        "aud",
        json!({"action": "admin.content_validate", "outcome": "denied", "detail": "role", "count": 1}),
    )
    .await;
    assert_eq!(s, 200, "{r}");
    let e = srv.events("admin.content_validate").await;
    assert_eq!(e[0]["actor"], "aud");
    assert_eq!(e[0]["outcome"], "denied");
    assert_eq!(e[0]["detail"], "role");
    assert_eq!(e[0]["sources"], 1);
    // An admin's sensitive read, with whose file it was.
    let (s, _) = event(
        &srv,
        "ada",
        json!({"action": "admin.content_sensitive_read", "outcome": "ok", "detail": "personal file", "count": 1, "target": "content:personal:ana"}),
    )
    .await;
    assert_eq!(s, 200);
    let e = srv.events("admin.content_sensitive_read").await;
    assert_eq!(e[0]["outcome"], "ok");
    assert_eq!(e[0]["detail"], "personal file");
    assert_eq!(e[0]["target"], "content:personal:ana");
    // Only an admin reads one: an auditor's claim is refused, and that is recorded.
    let (s, _) = event(
        &srv,
        "aud",
        json!({"action": "admin.content_sensitive_read", "outcome": "ok", "detail": "restricted context", "count": 1}),
    )
    .await;
    assert_eq!(s, 403);
    let e = srv.events("admin.content_sensitive_read").await;
    assert_eq!(
        (e[0]["actor"].clone(), e[0]["outcome"].clone()),
        (json!("aud"), json!("denied"))
    );
    // Malformed: unknown action, note, outcome, target, a path or text field.
    for bad in [
        json!({"action": "admin.merge", "outcome": "denied", "detail": "role"}),
        json!({"action": "admin.content_file", "outcome": "denied", "detail": "the file says hello"}),
        json!({"action": "admin.content_file", "outcome": "ok", "detail": "role"}),
        json!({"action": "admin.content_file", "outcome": "denied", "detail": "role", "target": "content/context/products/secret.md"}),
        json!({"action": "admin.content_file", "outcome": "denied", "detail": "role", "path": "content/x.md"}),
        json!({"action": "admin.content_file", "outcome": "denied", "detail": "role", "text": "file text is never taken"}),
        json!({"action": "admin.content_file", "outcome": "denied", "detail": "role", "count": 100000}),
    ] {
        let (s, r) = event(&srv, "ada", bad.clone()).await;
        assert_eq!(s, 400, "{bad}: {r}");
    }
    // Someone not on the team.
    let (s, _) = event(
        &srv,
        "mallory",
        json!({"action": "admin.content_file", "outcome": "denied", "detail": "role"}),
    )
    .await;
    assert_eq!(s, 403);
    // Audit log down: pmos-web is told, so it serves nothing.
    srv.f.audit_switch.down.store(true, Ordering::SeqCst);
    let (s, r) = event(
        &srv,
        "ada",
        json!({"action": "admin.content_sensitive_read", "outcome": "ok", "detail": "restricted context", "count": 1}),
    )
    .await;
    assert_eq!(s, 503);
    assert_eq!(r["code"], "audit_unavailable");
}
