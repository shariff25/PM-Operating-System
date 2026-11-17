//! GitHub linking, personal memory in GitHub and onboarding (`docs/onboarding-spec.md`
//! §1–3), against a local mock of GitHub. The mock keeps real git objects (blobs, trees,
//! commits and branch refs, content-addressed), so the tests can check exactly what was
//! committed, by whom, and on top of what. It records every request too.

mod common;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chrono::{Duration, Utc};
use pmos_contract::model::{Role, TeamFile, TeamMember};
use pmos_contract::onboarding::{render_personal_bot, render_team_yaml};
use pmos_contract::Sensitivity;
use pmos_toolserver::app::BotAccess;
use pmos_toolserver::audit::{Client, MemoryAudit, Outcome};
use pmos_toolserver::catalog::Catalog;
use pmos_toolserver::connectors::fake::FakeConnector;
use pmos_toolserver::connectors::Connector;
use pmos_toolserver::github::GitHub;
use pmos_toolserver::github_memory::{GitHubMemory, NO_REPO};
use pmos_toolserver::memory_repo::MemoryRepo;
use pmos_toolserver::oauth::{
    AppCredentials, Endpoints, OAuth, OAuthConfig, OAuthError, TokenError, TokenSource,
};
use pmos_toolserver::onboarding::{personal_policies, GitHubSetup, Onboarding};
use pmos_toolserver::service::ToolError;
use pmos_toolserver::session::Session;
use pmos_toolserver::team::Team;
use pmos_toolserver::vault::{Credential, KeyRing, Provider, Secret, Vault};
use pmos_toolserver::{router_with, AppConfig, Stores, ToolService};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

// ----- the mock ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Req {
    method: String,
    path: String,
    auth: Option<String>,
    accept: Option<String>,
    api_version: Option<String>,
    user_agent: Option<String>,
    body: String,
}

impl Req {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }

    fn form(&self) -> HashMap<String, String> {
        reqwest::Url::parse(&format!("http://x/?{}", self.body))
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect()
    }
}

#[derive(Debug, Clone)]
struct Commit {
    tree: String,
    parents: Vec<String>,
    message: String,
}

#[derive(Default)]
struct Objects {
    blobs: HashMap<String, String>,
    trees: HashMap<String, BTreeMap<String, String>>,
    commits: HashMap<String, Commit>,
    counter: usize,
}

fn hash(kind: &str, data: &str) -> String {
    let digest = Sha256::digest(format!("{kind}\0{data}").as_bytes());
    digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()[..40]
        .to_string()
}

impl Objects {
    fn blob(&mut self, text: &str) -> String {
        let sha = hash("blob", text);
        self.blobs.insert(sha.clone(), text.to_string());
        sha
    }

    fn tree(&mut self, files: BTreeMap<String, String>) -> String {
        let sha = hash("tree", &serde_json::to_string(&files).unwrap());
        self.trees.insert(sha.clone(), files);
        sha
    }

    fn commit(&mut self, tree: &str, parents: Vec<String>, message: &str) -> String {
        self.counter += 1;
        let sha = hash(
            "commit",
            &format!("{tree} {parents:?} {message} {}", self.counter),
        );
        self.commits.insert(
            sha.clone(),
            Commit {
                tree: tree.to_string(),
                parents,
                message: message.to_string(),
            },
        );
        sha
    }
}

#[derive(Debug, Clone, Default)]
struct Repo {
    owner: String,
    private: bool,
    archived: bool,
    default_branch: String,
    /// Logins that can see it, and push to it.
    visible: HashSet<String>,
    push: HashSet<String>,
    refs: HashMap<String, String>,
}

#[derive(Default)]
struct Mock {
    log: Mutex<Vec<Req>>,
    repos: Mutex<HashMap<String, Repo>>,
    objects: Mutex<Objects>,
    /// Refuse this many ref updates as not a fast-forward, moving the branch each time, as a
    /// concurrent push would.
    conflicts: Mutex<usize>,
    rate_limited: AtomicBool,
    /// Answer repo calls only after 3 s.
    slow: AtomicBool,
}

impl Mock {
    fn requests(&self, method: &str, path_contains: &str) -> Vec<Req> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method == method && r.path.contains(path_contains))
            .cloned()
            .collect()
    }

    fn clear_log(&self) {
        self.log.lock().unwrap().clear();
    }

    /// A repo, with one commit holding `files` unless `files` is empty.
    fn add_repo(
        &self,
        full: &str,
        private: bool,
        users: &[&str],
        push: &[&str],
        files: &[(&str, &str)],
    ) {
        let mut refs = HashMap::new();
        if !files.is_empty() {
            let mut objects = self.objects.lock().unwrap();
            let map = files
                .iter()
                .map(|(p, t)| (p.to_string(), objects.blob(t)))
                .collect();
            let tree = objects.tree(map);
            let commit = objects.commit(&tree, vec![], "Initial commit");
            refs.insert("main".to_string(), commit);
        }
        self.repos.lock().unwrap().insert(
            full.to_ascii_lowercase(),
            Repo {
                owner: full.split('/').next().unwrap().to_string(),
                private,
                archived: false,
                default_branch: "main".into(),
                visible: users.iter().map(|s| s.to_string()).collect(),
                push: push.iter().map(|s| s.to_string()).collect(),
                refs,
            },
        );
    }

    fn edit_repo(&self, full: &str, f: impl FnOnce(&mut Repo)) {
        f(self.repos.lock().unwrap().get_mut(full).unwrap());
    }

    fn head(&self, full: &str) -> String {
        self.repos.lock().unwrap()[full].refs["main"].clone()
    }

    fn commit(&self, sha: &str) -> Commit {
        self.objects.lock().unwrap().commits[sha].clone()
    }

    /// Path → text at a commit.
    fn files_at(&self, commit: &str) -> BTreeMap<String, String> {
        let objects = self.objects.lock().unwrap();
        let tree = &objects.trees[&objects.commits[commit].tree];
        tree.iter()
            .map(|(p, sha)| (p.clone(), objects.blobs[sha].clone()))
            .collect()
    }

    fn files(&self, full: &str) -> BTreeMap<String, String> {
        self.files_at(&self.head(full))
    }

    /// Paths that differ between a commit and its parent.
    fn changed(&self, commit: &str) -> Vec<String> {
        let after = self.files_at(commit);
        let parent = self.commit(commit).parents[0].clone();
        let before = self.files_at(&parent);
        let mut paths: Vec<String> = after
            .keys()
            .chain(before.keys())
            .filter(|p| after.get(*p) != before.get(*p))
            .cloned()
            .collect();
        paths.sort();
        paths.dedup();
        paths
    }
}

fn reply(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

fn not_found() -> Response {
    reply(StatusCode::NOT_FOUND, json!({"message": "Not Found"}))
}

/// `ghu-<login>-<n>`: the login a mock token belongs to.
fn login_of(auth: &str) -> Option<String> {
    let token = auth.strip_prefix("Bearer ghu-")?;
    Some(token.rsplit_once('-')?.0.to_string())
}

fn org_state(login: &str) -> Option<&'static str> {
    match login {
        "ana" | "raj-gh" | "kim" | "bo" => Some("active"),
        "pen" => Some("pending"),
        _ => None,
    }
}

fn emails(login: &str) -> Value {
    match login {
        "ana" => json!([{"email": "ana@example.com", "verified": true, "primary": true}]),
        "bo" => json!([
            {"email": "bo@example.com", "verified": false, "primary": false},
            {"email": "bo@home.example", "verified": true, "primary": true}
        ]),
        other => json!([{"email": format!("{other}@example.com"), "verified": true}]),
    }
}

/// The token response for `login`'s `n`th token.
fn tokens(login: &str, n: u32) -> Response {
    reply(
        StatusCode::OK,
        json!({
            "access_token": format!("ghu-{login}-{n}"),
            "expires_in": 28800,
            "refresh_token": format!("ghr-{login}-{n}"),
            "refresh_token_expires_in": 15897600,
            "scope": "",
            "token_type": "bearer"
        }),
    )
}

async fn handle(
    State(mock): State<Arc<Mock>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let header = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .map(String::from)
    };
    let req = Req {
        method: method.to_string(),
        path: uri.path().to_string(),
        auth: header("authorization"),
        accept: header("accept"),
        api_version: header("x-github-api-version"),
        user_agent: header("user-agent"),
        body: String::from_utf8_lossy(&body).into_owned(),
    };
    mock.log.lock().unwrap().push(req.clone());
    let query: HashMap<String, String> = uri
        .query()
        .map(|q| {
            reqwest::Url::parse(&format!("http://x/?{q}"))
                .unwrap()
                .query_pairs()
                .into_owned()
                .collect()
        })
        .unwrap_or_default();

    if req.path == "/login/oauth/access_token" {
        let f = req.form();
        if f.get("client_id").map(String::as_str) != Some("gh-client")
            || f.get("client_secret").map(String::as_str) != Some("gh-secret")
        {
            return reply(
                StatusCode::OK,
                json!({"error": "incorrect_client_credentials"}),
            );
        }
        if f.get("grant_type").map(String::as_str) == Some("refresh_token") {
            let refresh = f.get("refresh_token").cloned().unwrap_or_default();
            if refresh == "ghr-revoked" {
                return reply(
                    StatusCode::OK,
                    json!({"error": "bad_refresh_token", "error_description": "The refresh token passed is incorrect or expired."}),
                );
            }
            if refresh == "ghr-broken" {
                return reply(StatusCode::OK, json!({"error": "unexpected_error"}));
            }
            let Some((login, n)) = refresh
                .strip_prefix("ghr-")
                .and_then(|r| r.rsplit_once('-'))
            else {
                return reply(StatusCode::OK, json!({"error": "bad_refresh_token"}));
            };
            return tokens(login, n.parse::<u32>().unwrap() + 1);
        }
        return match f.get("code").and_then(|c| c.strip_prefix("code-")) {
            Some(login) => tokens(login, 1),
            None => reply(StatusCode::OK, json!({"error": "bad_verification_code"})),
        };
    }

    let Some(login) = req.auth.as_deref().and_then(login_of) else {
        return reply(
            StatusCode::UNAUTHORIZED,
            json!({"message": "Bad credentials"}),
        );
    };
    let path = req.path.clone();
    if path == "/user" {
        return reply(StatusCode::OK, json!({"login": login, "id": 1}));
    }
    if path == "/user/emails" {
        return reply(StatusCode::OK, emails(&login));
    }
    if let Some(org) = path.strip_prefix("/user/memberships/orgs/") {
        return match (org, org_state(&login)) {
            ("example-org", Some(state)) => {
                reply(StatusCode::OK, json!({"state": state, "role": "member"}))
            }
            _ => not_found(),
        };
    }
    let Some(rest) = path.strip_prefix("/repos/") else {
        return not_found();
    };
    if mock.slow.load(Ordering::SeqCst) {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
    if mock.rate_limited.load(Ordering::SeqCst) {
        let reset = Utc::now().timestamp() + 30;
        return (
            StatusCode::FORBIDDEN,
            [
                ("x-ratelimit-remaining", "0".to_string()),
                ("x-ratelimit-reset", reset.to_string()),
            ],
            Json(json!({"message": "API rate limit exceeded for user."})),
        )
            .into_response();
    }
    let parts: Vec<&str> = rest.split('/').collect();
    let full = format!("{}/{}", parts[0], parts[1]).to_ascii_lowercase();
    let tail = &parts[2..];
    let mut repos = mock.repos.lock().unwrap();
    let Some(repo) = repos.get_mut(&full) else {
        return not_found();
    };
    if !repo.visible.contains(&login) {
        return not_found();
    }
    let can_push = repo.push.contains(&login);
    let empty = || {
        reply(
            StatusCode::CONFLICT,
            json!({"message": "Git Repository is empty."}),
        )
    };
    let denied = || {
        reply(
            StatusCode::FORBIDDEN,
            json!({"message": "Resource not accessible by integration"}),
        )
    };
    let mut objects = mock.objects.lock().unwrap();
    let resolve = |objects: &Objects, repo: &Repo, r: &str| -> Option<String> {
        repo.refs
            .get(r)
            .cloned()
            .or_else(|| objects.commits.contains_key(r).then(|| r.to_string()))
    };
    let b = req.json();
    match (req.method.as_str(), tail) {
        ("GET", []) => reply(
            StatusCode::OK,
            json!({
                "full_name": format!("{}/{}", parts[0], parts[1]),
                "owner": {"login": repo.owner},
                "private": repo.private,
                "archived": repo.archived,
                "default_branch": repo.default_branch,
                "permissions": {"admin": false, "push": can_push, "pull": true},
            }),
        ),
        ("GET", ["contents", file @ ..]) => {
            let file = file.join("/");
            let r = query
                .get("ref")
                .cloned()
                .unwrap_or(repo.default_branch.clone());
            let Some(commit) = resolve(&objects, repo, &r) else {
                return not_found();
            };
            let tree = &objects.trees[&objects.commits[&commit].tree];
            match tree.get(&file) {
                Some(sha) => reply(
                    StatusCode::OK,
                    json!({"type": "file", "sha": sha, "encoding": "base64",
                           "content": B64.encode(&objects.blobs[sha])}),
                ),
                None => not_found(),
            }
        }
        ("PUT", ["contents", file @ ..]) => {
            if !can_push {
                return denied();
            }
            if !repo.refs.is_empty() {
                return reply(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    json!({"message": "mock: only for empty repos"}),
                );
            }
            let text =
                String::from_utf8(B64.decode(b["content"].as_str().unwrap()).unwrap()).unwrap();
            let blob = objects.blob(&text);
            let tree = objects.tree(BTreeMap::from([(file.join("/"), blob)]));
            let commit = objects.commit(&tree, vec![], b["message"].as_str().unwrap());
            repo.refs
                .insert(repo.default_branch.clone(), commit.clone());
            reply(StatusCode::CREATED, json!({"commit": {"sha": commit}}))
        }
        ("GET", ["git", "trees", tree_ish]) => {
            let tree_sha = if objects.trees.contains_key(*tree_ish) {
                tree_ish.to_string()
            } else {
                if repo.refs.is_empty() {
                    return empty();
                }
                match resolve(&objects, repo, tree_ish) {
                    Some(c) => objects.commits[&c].tree.clone(),
                    None => return not_found(),
                }
            };
            let entries: Vec<Value> = objects.trees[&tree_sha]
                .iter()
                .map(|(p, sha)| json!({"path": p, "mode": "100644", "type": "blob", "sha": sha}))
                .collect();
            reply(
                StatusCode::OK,
                json!({"sha": tree_sha, "tree": entries, "truncated": false}),
            )
        }
        ("GET", ["git", "blobs", sha]) => match objects.blobs.get(*sha) {
            Some(text) => reply(
                StatusCode::OK,
                json!({"sha": sha, "encoding": "base64", "content": B64.encode(text)}),
            ),
            None => not_found(),
        },
        ("GET", ["git", "ref", "heads", branch @ ..]) => {
            if repo.refs.is_empty() {
                return empty();
            }
            match repo.refs.get(&branch.join("/")) {
                Some(sha) => reply(
                    StatusCode::OK,
                    json!({"object": {"sha": sha, "type": "commit"}}),
                ),
                None => not_found(),
            }
        }
        ("GET", ["git", "commits", sha]) => match objects.commits.get(*sha) {
            Some(c) => reply(
                StatusCode::OK,
                json!({"sha": sha, "tree": {"sha": c.tree}, "message": c.message}),
            ),
            None => not_found(),
        },
        ("POST", ["git", "blobs"]) => {
            if !can_push {
                return denied();
            }
            let text =
                String::from_utf8(B64.decode(b["content"].as_str().unwrap()).unwrap()).unwrap();
            let sha = objects.blob(&text);
            reply(StatusCode::CREATED, json!({"sha": sha}))
        }
        ("POST", ["git", "trees"]) => {
            if !can_push {
                return denied();
            }
            let mut files = b["base_tree"]
                .as_str()
                .and_then(|t| objects.trees.get(t).cloned())
                .unwrap_or_default();
            for e in b["tree"].as_array().unwrap() {
                let path = e["path"].as_str().unwrap().to_string();
                match e["sha"].as_str() {
                    Some(sha) => {
                        files.insert(path, sha.to_string());
                    }
                    None => {
                        if files.remove(&path).is_none() {
                            return reply(
                                StatusCode::UNPROCESSABLE_ENTITY,
                                json!({"message": format!("mock: no {path} to delete")}),
                            );
                        }
                    }
                }
            }
            let sha = objects.tree(files);
            reply(StatusCode::CREATED, json!({"sha": sha}))
        }
        ("POST", ["git", "commits"]) => {
            if !can_push {
                return denied();
            }
            let parents = b["parents"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p.as_str().unwrap().to_string())
                .collect();
            let sha = objects.commit(
                b["tree"].as_str().unwrap(),
                parents,
                b["message"].as_str().unwrap(),
            );
            reply(StatusCode::CREATED, json!({"sha": sha}))
        }
        ("PATCH", ["git", "refs", "heads", branch @ ..]) => {
            if !can_push {
                return denied();
            }
            assert_eq!(b["force"], false, "ref updates are never forced");
            let branch = branch.join("/");
            let current = repo.refs[&branch].clone();
            let conflict = {
                let mut left = mock.conflicts.lock().unwrap();
                let conflict = *left > 0;
                *left = left.saturating_sub(1);
                conflict
            };
            if conflict {
                // Someone else pushes first.
                let mut files = objects.trees[&objects.commits[&current].tree].clone();
                let n = objects.counter;
                let blob = objects.blob(&format!("change {n}"));
                files.insert(format!("other/{n}.txt"), blob);
                let tree = objects.tree(files);
                let moved = objects.commit(&tree, vec![current], "someone else's change");
                repo.refs.insert(branch, moved);
                return reply(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    json!({"message": "Update is not a fast forward"}),
                );
            }
            let new = b["sha"].as_str().unwrap().to_string();
            if objects.commits[&new].parents.first() != Some(&current) {
                return reply(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    json!({"message": "Update is not a fast forward"}),
                );
            }
            repo.refs.insert(branch, new.clone());
            reply(StatusCode::OK, json!({"object": {"sha": new}}))
        }
        _ => reply(StatusCode::NOT_FOUND, json!({"message": "mock: no route"})),
    }
}

async fn start_mock() -> (String, Arc<Mock>) {
    let mock = Arc::new(Mock::default());
    let app = Router::new().fallback(handle).with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, mock)
}

// ----- the system under test --------------------------------------------------------------

const SERVICE_TOKEN: &str = "test-service-token-0123456789abcdef";
const BOT_TOKEN: &str = "test-bot-token-0123456789abcdef-0123";
const CONTENT_REPO: &str = "example-org/pm-os";
const MEMORY_REPO: &str = "example-org/pm-os-memory-ana";
const TEAM_HEADER: &str = "# Who may use PM OS.\n";

fn member(handle: &str, slack: &str, github: Option<&str>, roles: Vec<Role>) -> TeamMember {
    TeamMember {
        handle: handle.into(),
        slack_id: slack.into(),
        email: Some(format!("{handle}@example.com")),
        github: github.map(String::from),
        entra_oid: None,
        roles,
    }
}

/// Ana and Bo are PMs; Raj (GitHub `Raj-GH`) and Kim are platform owners; Eve isn't in the
/// GitHub org and Pen's org invitation is still pending.
fn team() -> Vec<TeamMember> {
    vec![
        member("ana", "U0ANA", None, vec![Role::Pm]),
        member("bo", "U0BO", None, vec![Role::Pm]),
        member("raj", "U0RAJ", Some("Raj-GH"), vec![Role::Pm, Role::Admin]),
        member("kim", "U0KIM", None, vec![Role::Admin]),
        member("eve", "U0EVE", None, vec![Role::Pm]),
        member("pen", "U0PEN", None, vec![Role::Pm]),
    ]
}

struct Env {
    mock: Arc<Mock>,
    oauth: Arc<OAuth>,
    vault: Vault,
    svc: Arc<ToolService>,
    stores: Stores,
    audit: Arc<MemoryAudit>,
    ts: String,
    http: reqwest::Client,
    root: std::path::PathBuf,
    _dir: tempfile::TempDir,
    cancel: CancellationToken,
}

impl Drop for Env {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn env() -> Env {
    let (base, mock) = start_mock().await;
    let (dir, root) = common::content_tree();
    let (mut stores, audit) = Stores::in_memory(&root);
    let members = Team::of(team());
    let vault = Vault::new(
        KeyRing::from_spec(&KeyRing::generate_spec("k1")).unwrap(),
        stores.credentials.clone(),
    );
    let oauth = Arc::new(
        OAuth::new(
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
                github_org: Some("example-org".into()),
                state_key: b"test-state-key".to_vec(),
                endpoints: Endpoints {
                    github_auth: base.clone(),
                    github_api: base.clone(),
                    ..Endpoints::default()
                },
            },
            vault.clone(),
        )
        .with_team(members.clone()),
    );
    let github = Arc::new(GitHub::new(base.clone()));
    let tokens: Arc<dyn TokenSource> = oauth.clone();
    stores.memory = Arc::new(GitHubMemory::new(
        github.clone(),
        tokens,
        stores.memory_repos.clone(),
        "example-org",
    ));
    let catalog = Catalog::load(&root, Utc::now().date_naive()).unwrap();
    let connectors: Vec<Arc<dyn Connector>> = vec![
        Arc::new(FakeConnector::jira()),
        Arc::new(FakeConnector::confluence()),
        Arc::new(FakeConnector::slack()),
        Arc::new(FakeConnector::docs()),
    ];
    let svc = Arc::new(
        ToolService::new(catalog, stores.clone(), connectors)
            .unwrap()
            .with_team(members),
    );
    let onboarding = Onboarding::new(
        svc.clone(),
        Some(oauth.clone()),
        Some(GitHubSetup {
            github,
            org: "example-org".into(),
            content_repo: Some(CONTENT_REPO.into()),
            content_branch: "main".into(),
            content_dir: "content".into(),
            policies_dir: "policies".into(),
            content_root: root.clone(),
        }),
    );
    let cancel = CancellationToken::new();
    let app = router_with(
        svc.clone(),
        AppConfig {
            service_token: SERVICE_TOKEN.into(),
            bot: Some(BotAccess {
                token: BOT_TOKEN.into(),
                skills: vec!["docs".into()],
            }),
            console_token: None,
            content_root: root.clone(),
            allowed_hosts: vec!["127.0.0.1".into()],
            oauth: Some(oauth.clone()),
        },
        Some(Arc::new(onboarding)),
        cancel.clone(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ts = format!("http://{}", listener.local_addr().unwrap());
    let ct = cancel.clone();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { ct.cancelled_owned().await })
            .await
            .unwrap();
    });

    // The content repo: team.yaml for the team above, and Bo's personal bot. Only Raj may
    // push to it.
    let team_yaml = render_team_yaml(TEAM_HEADER, &team());
    let bo_bot = render_personal_bot("bo");
    mock.add_repo(
        CONTENT_REPO,
        true,
        &["ana", "bo", "raj-gh", "kim"],
        &["raj-gh"],
        &[
            ("README.md", "# PM OS\n"),
            ("content/team.yaml", &team_yaml),
            ("content/bots/pm-bo/BOT.md", &bo_bot),
            ("policies/bots/pm-bo.yaml", "# Generated\n"),
            ("policies/bots/pm-bo.sandbox.yaml", "# Generated\n"),
        ],
    );
    Env {
        mock,
        oauth,
        vault,
        svc,
        stores,
        audit,
        ts,
        http: reqwest::Client::new(),
        root,
        _dir: dir,
        cancel,
    }
}

fn state_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned()
}

/// `user` links GitHub, signing in as `login`.
async fn try_link(e: &Env, user: &str, login: &str) -> Result<(), OAuthError> {
    let url = e
        .oauth
        .authorize_url(user, Provider::Github, Utc::now())
        .unwrap();
    e.oauth
        .complete(
            Provider::Github,
            &format!("code-{login}"),
            &state_of(&url),
            Utc::now(),
        )
        .await
        .map(|_| ())
}

async fn link(e: &Env, user: &str, login: &str) {
    try_link(e, user, login).await.unwrap();
}

/// A GitHub credential stored directly, for expiry and failure cases.
async fn put_github(e: &Env, user: &str, refresh: &str, expires_in: i64) {
    e.vault
        .put(&Credential {
            user: user.into(),
            provider: Provider::Github,
            access_token: Secret::new(format!("ghu-{user}-1")),
            refresh_token: Some(Secret::new(refresh)),
            expires_at: Some(Utc::now() + Duration::seconds(expires_in)),
            scopes: String::new(),
            cloud_id: None,
            site_url: None,
            external_id: Some(user.into()),
            updated_at: Utc::now(),
        })
        .await
        .unwrap();
}

async fn call(
    e: &Env,
    method: Method,
    path: &str,
    body: Option<Value>,
    token: &str,
) -> (u16, Value) {
    let mut req = e
        .http
        .request(method, format!("{}{path}", e.ts))
        .bearer_auth(token);
    if let Some(body) = body {
        req = req.json(&body);
    }
    let res = req.send().await.unwrap();
    let status = res.status().as_u16();
    (status, res.json().await.unwrap_or(Value::Null))
}

async fn bot(e: &Env, method: Method, path: &str, body: Option<Value>) -> (u16, Value) {
    call(e, method, path, body, BOT_TOKEN).await
}

// ----- linking ----------------------------------------------------------------------------

#[tokio::test]
async fn linking_checks_the_account_then_the_refresh_rotates_the_tokens() {
    let e = env().await;
    link(&e, "ana", "ana").await;

    let exchange = &e.mock.requests("POST", "/login/oauth/access_token")[0];
    assert_eq!(exchange.accept.as_deref(), Some("application/json"));
    let form = exchange.form();
    assert_eq!(form["code"], "code-ana");
    assert_eq!(
        form["redirect_uri"],
        "https://pmos.example.com/oauth/github/callback"
    );
    let me = &e.mock.requests("GET", "/user")[0];
    assert_eq!(me.auth.as_deref(), Some("Bearer ghu-ana-1"));
    assert_eq!(me.api_version.as_deref(), Some("2022-11-28"));
    assert!(me
        .user_agent
        .as_deref()
        .unwrap()
        .starts_with("pmos-toolserver/"));
    assert_eq!(
        e.mock
            .requests("GET", "/user/memberships/orgs/example-org")
            .len(),
        1
    );
    assert_eq!(
        e.mock.requests("GET", "/user/emails").len(),
        1,
        "no github login in team.yaml"
    );

    let stored = e.vault.get("ana", Provider::Github).await.unwrap().unwrap();
    assert_eq!(
        stored.external_id.as_deref(),
        Some("ana"),
        "the login is kept"
    );
    assert_eq!(stored.access_token.expose(), "ghu-ana-1");
    let expires = stored.expires_at.unwrap() - Utc::now();
    assert!(expires > Duration::hours(7) && expires <= Duration::hours(8));

    // Fresh: no refresh.
    let token = e.oauth.token("ana", Provider::Github).await.unwrap();
    assert_eq!(token.token.expose(), "ghu-ana-1");
    // About to expire: one refresh, and both tokens rotate.
    put_github(&e, "ana", "ghr-ana-1", 30).await;
    let token = e.oauth.token("ana", Provider::Github).await.unwrap();
    assert_eq!(token.token.expose(), "ghu-ana-2");
    let refresh = e.mock.requests("POST", "/login/oauth/access_token");
    let form = refresh.last().unwrap().form();
    assert_eq!(form["grant_type"], "refresh_token");
    assert_eq!(form["refresh_token"], "ghr-ana-1");
    let stored = e.vault.get("ana", Provider::Github).await.unwrap().unwrap();
    assert_eq!(stored.refresh_token.unwrap().expose(), "ghr-ana-2");
    assert_eq!(stored.external_id.as_deref(), Some("ana"));
    // Over HTTP, the callback page says so.
    let url = e
        .oauth
        .authorize_url("bo", Provider::Github, Utc::now())
        .unwrap();
    let page = e
        .http
        .get(format!("{}/oauth/github/callback", e.ts))
        .query(&[("code", "code-bo"), ("state", state_of(&url).as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(
        page.status(),
        403,
        "bo's only matching email isn't verified"
    );
}

#[tokio::test]
async fn a_bad_refresh_token_unlinks_and_other_errors_keep_the_account() {
    let e = env().await;
    put_github(&e, "ana", "ghr-revoked", 30).await;
    assert_eq!(
        e.oauth.token("ana", Provider::Github).await.unwrap_err(),
        TokenError::Reconnect(Provider::Github)
    );
    assert!(e
        .vault
        .get("ana", Provider::Github)
        .await
        .unwrap()
        .is_none());

    put_github(&e, "bo", "ghr-broken", 30).await;
    assert!(matches!(
        e.oauth.token("bo", Provider::Github).await.unwrap_err(),
        TokenError::Unavailable(Provider::Github, _)
    ));
    assert!(
        e.vault.get("bo", Provider::Github).await.unwrap().is_some(),
        "only a refused refresh token unlinks"
    );
}

#[tokio::test]
async fn a_mismatched_login_or_email_is_refused() {
    let e = env().await;
    // Raj's team.yaml entry names GitHub login Raj-GH: Ana's account isn't his.
    let err = try_link(&e, "raj", "ana").await.unwrap_err();
    assert!(
        matches!(err, OAuthError::WrongAccount(Provider::Github, _)),
        "{err}"
    );
    assert!(err.to_string().contains("github login"), "{err}");
    assert!(e
        .vault
        .get("raj", Provider::Github)
        .await
        .unwrap()
        .is_none());
    // His own, compared ignoring case. The login decides: no email lookup.
    e.mock.clear_log();
    link(&e, "raj", "raj-gh").await;
    assert!(e.mock.requests("GET", "/user/emails").is_empty());

    // Bo's only matching email isn't verified.
    let err = try_link(&e, "bo", "bo").await.unwrap_err();
    assert!(err.to_string().contains("verified"), "{err}");
    // Ana signing in for Kim: the verified email is Ana's.
    assert!(matches!(
        try_link(&e, "kim", "ana").await,
        Err(OAuthError::WrongAccount(..))
    ));
    assert!(e.vault.get("bo", Provider::Github).await.unwrap().is_none());
    assert!(e
        .vault
        .get("kim", Provider::Github)
        .await
        .unwrap()
        .is_none());
    // A code GitHub refuses (as a 200 with an error).
    let url = e
        .oauth
        .authorize_url("ana", Provider::Github, Utc::now())
        .unwrap();
    let err = e
        .oauth
        .complete(Provider::Github, "nope", &state_of(&url), Utc::now())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("bad_verification_code"), "{err}");
}

#[tokio::test]
async fn someone_outside_the_org_is_refused() {
    let e = env().await;
    let err = try_link(&e, "eve", "eve").await.unwrap_err();
    assert!(
        err.to_string().contains("active member of `example-org`"),
        "{err}"
    );
    let err = try_link(&e, "pen", "pen").await.unwrap_err();
    assert!(
        err.to_string().contains("active member"),
        "pending isn't active: {err}"
    );
    assert!(e
        .vault
        .get("eve", Provider::Github)
        .await
        .unwrap()
        .is_none());
    assert!(e
        .vault
        .get("pen", Provider::Github)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn the_slack_bot_gets_link_urls_for_members_only() {
    let e = env().await;
    let (status, body) = bot(
        &e,
        Method::GET,
        "/internal/oauth/github/start?user=ana",
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert!(body["url"]
        .as_str()
        .unwrap()
        .contains("/login/oauth/authorize?client_id=gh-client"));
    for user in ["nobody", "bots.docs"] {
        let (status, _) = bot(
            &e,
            Method::GET,
            &format!("/internal/oauth/github/start?user={user}"),
            None,
        )
        .await;
        assert_eq!(status, 403, "{user}");
    }
}

#[tokio::test]
async fn the_slack_bot_unlinks_members_accounts_only() {
    let e = env().await;
    link(&e, "ana", "ana").await;
    // A member's account: unlinked (the App Home's Disconnect), and audited with the client.
    let (status, _) = bot(&e, Method::DELETE, "/internal/credentials/ana/github", None).await;
    assert_eq!(status, 204);
    assert!(e
        .vault
        .get("ana", Provider::Github)
        .await
        .unwrap()
        .is_none());
    let last = e.audit.events().last().cloned().unwrap();
    assert_eq!(
        (
            last.tool.as_str(),
            last.client,
            last.outcome,
            last.user.as_str(),
            last.output_ref.as_deref()
        ),
        (
            "credentials.unlink",
            Client::Bot,
            Outcome::Ok,
            "ana",
            Some("github")
        )
    );
    // Nothing left to unlink.
    let (status, _) = bot(&e, Method::DELETE, "/internal/credentials/ana/github", None).await;
    assert_eq!(status, 404);
    // Not a member, or a team bot's own account: refused, and audited as denied.
    put_github(&e, "bots.docs", "r-bot", 3600).await;
    for user in ["nobody", "bots.docs"] {
        let (status, body) = bot(
            &e,
            Method::DELETE,
            &format!("/internal/credentials/{user}/github"),
            None,
        )
        .await;
        assert_eq!(status, 403, "{user}");
        assert_eq!(
            body["error"],
            "this token may unlink accounts of team members only"
        );
        let last = e.audit.events().last().cloned().unwrap();
        assert_eq!(
            (last.tool.as_str(), last.client, last.outcome),
            ("credentials.unlink", Client::Bot, Outcome::Denied)
        );
    }
    assert!(e
        .vault
        .get("bots.docs", Provider::Github)
        .await
        .unwrap()
        .is_some());
    // The service token may unlink anyone's, as before, audited as the orchestrator.
    let res = e
        .http
        .delete(format!("{}/internal/credentials/bots.docs/github", e.ts))
        .bearer_auth(SERVICE_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 204);
    let last = e.audit.events().last().cloned().unwrap();
    assert_eq!(
        (last.client, last.outcome),
        (Client::Orchestrator, Outcome::Ok)
    );
}

// ----- memory -----------------------------------------------------------------------------

/// Ana's memory repo, private, which only she can see and push to.
async fn set_up_memory(e: &Env, files: &[(&str, &str)]) {
    e.mock
        .add_repo(MEMORY_REPO, true, &["ana"], &["ana"], files);
    link(e, "ana", "ana").await;
    let (status, body) = bot(
        e,
        Method::POST,
        "/internal/memory/repo",
        Some(json!({"user": "ana", "repo": MEMORY_REPO})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
}

#[tokio::test]
async fn notes_are_written_and_read_in_the_pms_repo() {
    let e = env().await;
    let now = Utc::now();
    let s = e
        .svc
        .create_session("ana", "internal-brief", "run-1", None, now)
        .await
        .unwrap();
    // No repo yet.
    let err = e
        .svc
        .call(&s, "memory_read", json!({}), now)
        .await
        .unwrap_err();
    assert_eq!(err, ToolError::Connector(NO_REPO.into()));

    set_up_memory(&e, &[("README.md", "Ana's memory\n")]).await;
    let written = e
        .svc
        .call(
            &s,
            "memory_update",
            json!({"key": "areas", "text": "Acme Cloud, Acme Vault"}),
            now,
        )
        .await
        .unwrap();
    assert_eq!(written["note"]["key"], "areas");
    let head = e.mock.head(MEMORY_REPO);
    assert_eq!(
        e.mock.commit(&head).message,
        "memory: areas (internal-brief, task run-1)"
    );
    assert_eq!(
        e.mock.changed(&head),
        vec!["notes/areas.md"],
        "only that file"
    );
    let file = &e.mock.files(MEMORY_REPO)["notes/areas.md"];
    assert!(file.starts_with("---\nupdated: "), "{file}");
    assert!(
        file.contains(
            "source: \"internal-brief, task run-1\"\nsensitivity: public\n---\nAcme Cloud, Acme Vault"
        ),
        "{file}"
    );
    for r in e.mock.requests("POST", "/repos/") {
        assert_eq!(r.auth.as_deref(), Some("Bearer ghu-ana-1"), "as Ana");
    }

    e.svc
        .call(
            &s,
            "memory_update",
            json!({"key": "style", "text": "short"}),
            now,
        )
        .await
        .unwrap();
    let read = e.svc.call(&s, "memory_read", json!({}), now).await.unwrap();
    let notes = read["notes"].as_array().unwrap();
    assert_eq!(notes.len(), 2, "{read}");
    assert_eq!(notes[0]["key"], "areas");
    assert_eq!(notes[0]["text"], "Acme Cloud, Acme Vault");
    assert_eq!(notes[1]["key"], "style");
    // A note written by a bot names the bot.
    e.stores
        .memory
        .update(
            "ana",
            "areas",
            "Acme Cloud only",
            "pm-ana, task t-9",
            Sensitivity::Internal,
            now,
        )
        .await
        .unwrap();
    let head = e.mock.head(MEMORY_REPO);
    assert_eq!(
        e.mock.commit(&head).message,
        "memory: areas (pm-ana, task t-9)"
    );
}

/// A note on GitHub keeps the sensitivity of what its session had read, in its front matter
/// (S6): a restricted read then a write stores `restricted`, a later session reading it is
/// raised to `restricted`, and a note edited by hand without it reads as `internal`.
#[tokio::test]
async fn notes_carry_their_sensitivity_in_the_pms_repo() {
    let e = env().await;
    let now = Utc::now();
    set_up_memory(
        &e,
        &[(
            "notes/old.md",
            "---\nupdated: 2026-10-01T10:00:00Z\nsource: \"by hand\"\n---\nWritten before S6\n",
        )],
    )
    .await;
    let session = |run: &'static str| {
        let svc = e.svc.clone();
        async move {
            svc.create_session("ana", "internal-brief", run, None, now)
                .await
                .unwrap()
        }
    };
    let stored = |s: Session| {
        let sessions = e.stores.sessions.clone();
        async move {
            sessions
                .get(&s.token, now)
                .await
                .unwrap()
                .unwrap()
                .sources_sensitivity
        }
    };

    // An old note reads as internal.
    let reader = session("run-old").await;
    let read = e
        .svc
        .call(&reader, "memory_read", json!({}), now)
        .await
        .unwrap();
    assert_eq!(read["notes"][0]["sensitivity"], "internal", "{read}");
    assert_eq!(stored(reader).await, Sensitivity::Internal);

    // A session that had read restricted data writes a note: stored `restricted`.
    let writer = session("run-w").await;
    e.stores
        .sessions
        .raise_sensitivity(&writer.token, Sensitivity::Restricted)
        .await
        .unwrap();
    e.svc
        .call(
            &writer,
            "memory_update",
            json!({"key": "partners", "text": "Globex joint roadmap"}),
            now,
        )
        .await
        .unwrap();
    let file = &e.mock.files(MEMORY_REPO)["notes/partners.md"];
    assert!(file.contains("\nsensitivity: restricted\n---\n"), "{file}");

    // A later session that reads memory is raised to it.
    let later = session("run-later").await;
    e.svc
        .call(&later, "memory_read", json!({}), now)
        .await
        .unwrap();
    assert_eq!(stored(later).await, Sensitivity::Restricted);
}

/// The bets register on GitHub memory (`docs/staff-principal-spec.md` §5): nothing is
/// committed before approval; approval commits one note, `notes/bet-<key>.md`, as Ana; an
/// update appends evidence.
#[tokio::test]
async fn bets_live_in_the_pms_memory_repo() {
    let e = env().await;
    set_up_memory(&e, &[("README.md", "Ana's memory\n")]).await;
    common::write_content(
        &e.root,
        "skills/bets-test/SKILL.md",
        "---\nname: bets-test\ndescription: Test skill that keeps the bets register.\nmetadata:\n  owner: \"@pm\"\n  version: \"0.1.0\"\n  model: \"reasoning\"\n  data-class: \"internal\"\n  tools: \"bets.list bets.read bets.record\"\n---\n\n# /bets-test\n\nTest.\n",
    );
    let connectors: Vec<Arc<dyn Connector>> = vec![
        Arc::new(FakeConnector::jira()),
        Arc::new(FakeConnector::confluence()),
        Arc::new(FakeConnector::slack()),
        Arc::new(FakeConnector::docs()),
    ];
    let svc = Arc::new(
        ToolService::new(
            Catalog::load(&e.root, Utc::now().date_naive()).unwrap(),
            e.stores.clone(),
            connectors,
        )
        .unwrap()
        .with_team(Team::of(team())),
    );
    let now = Utc::now();
    let s = svc
        .create_session("ana", "bets-test", "run-b", None, now)
        .await
        .unwrap();
    let head = e.mock.head(MEMORY_REPO);
    let out = svc
        .call(
            &s,
            "bets_record",
            json!({
                "key": "sso-enterprise",
                "title": "SSO wins the enterprise tier",
                "bet": "SAML SSO in the console moves enterprise trials to paid.",
                "metric": "Enterprise trial-to-paid conversion",
                "riskiest_assumption": "SSO is the blocker, not price",
                "review_by": "2027-02-01",
            }),
            now,
        )
        .await
        .unwrap();
    assert_eq!(out["status"], "pending_approval");
    assert_eq!(e.mock.head(MEMORY_REPO), head, "nothing committed yet");

    let id = out["approval_id"].as_str().unwrap();
    let item = svc
        .decide(
            id,
            "ana",
            pmos_toolserver::approvals::Decision::Approve,
            None,
            now,
        )
        .await
        .unwrap();
    assert_eq!(
        item.result.as_ref().unwrap()["created"],
        true,
        "{:?}",
        item.result
    );
    let head = e.mock.head(MEMORY_REPO);
    assert_eq!(e.mock.changed(&head), vec!["notes/bet-sso-enterprise.md"]);
    assert_eq!(
        e.mock.commit(&head).message,
        "memory: bet-sso-enterprise (bets-test, task run-b, approved by ana)"
    );
    let file = &e.mock.files(MEMORY_REPO)["notes/bet-sso-enterprise.md"];
    assert!(
        file.contains("# Bet: SSO wins the enterprise tier\n"),
        "{file}"
    );
    for r in e.mock.requests("POST", "/repos/") {
        assert_eq!(r.auth.as_deref(), Some("Bearer ghu-ana-1"), "as Ana");
    }

    let out = svc
        .call(
            &s,
            "bets_record",
            json!({"key": "sso-enterprise", "evidence": [
                {"date": "2026-11-20", "text": "Two trials asked for SSO", "source": "CONF-9001"}]}),
            now,
        )
        .await
        .unwrap();
    let id = out["approval_id"].as_str().unwrap();
    let item = svc
        .decide(
            id,
            "ana",
            pmos_toolserver::approvals::Decision::Approve,
            None,
            now,
        )
        .await
        .unwrap();
    assert_eq!(item.result.as_ref().unwrap()["created"], false);
    let read = svc
        .call(&s, "bets_read", json!({"key": "sso-enterprise"}), now)
        .await
        .unwrap();
    assert_eq!(read["bet"]["evidence"][0]["source"], "CONF-9001");
    assert_eq!(read["bet"]["metric"], "Enterprise trial-to-paid conversion");
    let listed = svc.call(&s, "bets_list", json!({}), now).await.unwrap();
    assert_eq!(listed["bets"][0]["key"], "sso-enterprise");
}

#[tokio::test]
async fn the_cache_fetches_only_blobs_that_changed() {
    let e = env().await;
    set_up_memory(&e, &[]).await;
    let m = e.stores.memory.as_ref();
    let now = Utc::now();
    // An empty repo: the first note goes in through the contents API.
    assert!(m.read("ana").await.unwrap().is_empty());
    m.update(
        "ana",
        "areas",
        "Acme Cloud",
        "t, task 1",
        Sensitivity::Internal,
        now,
    )
    .await
    .unwrap();
    assert_eq!(e.mock.requests("PUT", "/contents/notes/areas.md").len(), 1);
    m.update(
        "ana",
        "style",
        "short",
        "t, task 1",
        Sensitivity::Internal,
        now,
    )
    .await
    .unwrap();

    e.mock.clear_log();
    assert_eq!(m.read("ana").await.unwrap().len(), 2);
    assert_eq!(
        e.mock.requests("GET", "/git/blobs/").len(),
        2,
        "a cold cache"
    );
    e.mock.clear_log();
    let again = m.read("ana").await.unwrap();
    assert_eq!(again.len(), 2);
    assert!(
        e.mock.requests("GET", "/git/blobs/").is_empty(),
        "nothing changed"
    );
    assert!(
        e.mock
            .requests("GET", "/repos/example-org/pm-os-memory-ana")
            .len()
            <= 1,
        "the tree only; the repo was verified minutes ago"
    );
    assert!(e
        .mock
        .requests("GET", "/repos/example-org/pm-os-memory-ana")
        .iter()
        .all(|r| r.path.contains("/git/trees/")));

    m.update(
        "ana",
        "style",
        "longer",
        "t, task 2",
        Sensitivity::Internal,
        now,
    )
    .await
    .unwrap();
    e.mock.clear_log();
    let notes = m.read("ana").await.unwrap();
    assert_eq!(notes[1].text, "longer");
    assert_eq!(
        e.mock.requests("GET", "/git/blobs/").len(),
        1,
        "only the changed note"
    );
}

#[tokio::test]
async fn a_repo_that_turns_public_is_refused() {
    let e = env().await;
    set_up_memory(&e, &[("README.md", "x\n")]).await;
    let m = e.stores.memory.as_ref();
    m.update(
        "ana",
        "areas",
        "Acme Cloud",
        "t, task 1",
        Sensitivity::Internal,
        Utc::now(),
    )
    .await
    .unwrap();
    e.mock.edit_repo(MEMORY_REPO, |r| r.private = false);
    // Within ten minutes of the last check the record is trusted…
    assert!(m.read("ana").await.is_ok());
    // …after that it is checked again, and refused.
    let stale = Utc::now() - Duration::minutes(11);
    e.stores
        .memory_repos
        .put(&MemoryRepo {
            user: "ana".into(),
            repo: MEMORY_REPO.into(),
            verified_at: stale,
        })
        .await
        .unwrap();
    let err = m.read("ana").await.unwrap_err();
    assert!(err.to_string().contains("is not private"), "{err}");
    let err = m
        .update(
            "ana",
            "areas",
            "x",
            "t, task 2",
            Sensitivity::Internal,
            Utc::now(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("is not private"), "{err}");
    let record = e.stores.memory_repos.get("ana").await.unwrap().unwrap();
    assert_eq!(
        record.verified_at, stale,
        "a failed check doesn't renew the record"
    );

    // Private again: the check passes and the record is renewed.
    e.mock.edit_repo(MEMORY_REPO, |r| r.private = true);
    assert_eq!(m.read("ana").await.unwrap().len(), 1);
    let record = e.stores.memory_repos.get("ana").await.unwrap().unwrap();
    assert!(record.verified_at > stale);
    // Rate limits are reported as such.
    e.mock.rate_limited.store(true, Ordering::SeqCst);
    let err = m.read("ana").await.unwrap_err();
    assert!(err.to_string().contains("rate limit"), "{err}");
}

#[tokio::test]
async fn setting_a_memory_repo_checks_it_first() {
    let e = env().await;
    let set = |repo: &str| {
        let body = json!({"user": "ana", "repo": repo});
        let e = &e;
        async move { bot(e, Method::POST, "/internal/memory/repo", Some(body)).await }
    };
    // Not linked yet.
    e.mock.add_repo(MEMORY_REPO, true, &["ana"], &["ana"], &[]);
    let (status, body) = set(MEMORY_REPO).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (409, Some("github_not_linked"))
    );

    link(&e, "ana", "ana").await;
    e.mock
        .add_repo("example-org/public-notes", false, &["ana"], &["ana"], &[]);
    e.mock
        .add_repo("example-org/read-only", true, &["ana"], &[], &[]);
    e.mock
        .add_repo("example-org/old", true, &["ana"], &["ana"], &[]);
    e.mock.edit_repo("example-org/old", |r| r.archived = true);
    for (repo, code) in [
        ("example-org/public-notes", "not_private"),
        ("example-org/read-only", "no_push"),
        ("example-org/old", "archived"),
        ("example-org/nothing-here", "not_found"),
        ("someone-else/pm-os-memory-ana", "not_in_org"),
        ("not a repo", "bad_request"),
    ] {
        let (status, body) = set(repo).await;
        assert_eq!(
            (status, body["code"].as_str()),
            (422, Some(code)),
            "{repo}: {body}"
        );
        assert!(body["error"].is_string());
    }
    let (status, _) = bot(&e, Method::GET, "/internal/memory/repo?user=ana", None).await;
    assert_eq!(status, 404, "nothing was recorded");

    let (status, body) = set(MEMORY_REPO).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["repo"], MEMORY_REPO);
    assert!(body["verified_at"].is_string());
    let (status, got) = bot(&e, Method::GET, "/internal/memory/repo?user=ana", None).await;
    assert_eq!(status, 200);
    assert_eq!(got["repo"], MEMORY_REPO);

    // It runs inside a Slack modal's acknowledgement: a slow GitHub gets an answer in time.
    e.mock.slow.store(true, Ordering::SeqCst);
    let started = std::time::Instant::now();
    let (status, body) = set(MEMORY_REPO).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (504, Some("github_unavailable"))
    );
    assert!(started.elapsed() < std::time::Duration::from_millis(2500));
}

// ----- members ----------------------------------------------------------------------------

async fn request(e: &Env, slack_id: &str, email: &str, name: &str) -> (u16, Value) {
    bot(
        e,
        Method::POST,
        "/internal/members/requests",
        Some(json!({"slack_id": slack_id, "email": email, "name": name})),
    )
    .await
}

async fn decide(e: &Env, id: &str, body: Value) -> (u16, Value) {
    bot(
        e,
        Method::POST,
        &format!("/internal/members/requests/{id}/decision"),
        Some(body),
    )
    .await
}

#[tokio::test]
async fn a_request_then_an_approval_commits_both_files_in_one_commit() {
    let e = env().await;
    let (status, body) = request(&e, "U0NIA", "Nia.Lee@example.com", "Nia Lee").await;
    assert_eq!(status, 201, "{body}");
    let r = &body["request"];
    assert_eq!(r["status"], "pending");
    assert_eq!(r["proposed_handle"], "nia-lee");
    let id = r["id"].as_str().unwrap().to_string();
    // Asking again gives the same request.
    let (status, again) = request(&e, "U0NIA", "nia.lee@example.com", "Nia").await;
    assert_eq!(status, 200);
    assert_eq!(again["request"]["id"], id.as_str());
    let (_, pending) = bot(
        &e,
        Method::GET,
        "/internal/members/requests?status=pending",
        None,
    )
    .await;
    assert_eq!(pending["requests"].as_array().unwrap().len(), 1);
    let (_, status_of) = bot(&e, Method::GET, "/internal/onboarding?slack_id=U0NIA", None).await;
    assert_eq!(status_of["member"], false);
    assert_eq!(status_of["pending_request"]["id"], id.as_str());

    link(&e, "raj", "raj-gh").await;
    let before = e.mock.head(CONTENT_REPO);
    let (status, body) = decide(&e, &id, json!({"decision": "approve", "decided_by": "raj"})).await;
    assert_eq!(status, 200, "{body}");
    let r = &body["request"];
    assert_eq!(r["status"], "approved");
    assert_eq!(r["handle"], "nia-lee");
    assert_eq!(r["decided_by"], "raj");
    let commit = r["commit"].as_str().unwrap().to_string();

    // One commit on main, on top of the old head, with both files.
    assert_eq!(e.mock.head(CONTENT_REPO), commit);
    let c = e.mock.commit(&commit);
    assert_eq!(c.parents, vec![before]);
    assert!(
        c.message.starts_with("Add nia-lee to PM OS\n"),
        "{}",
        c.message
    );
    assert!(
        c.message
            .contains(&format!("Approved-by: raj (Slack, request {id})")),
        "{}",
        c.message
    );
    assert_eq!(
        e.mock.changed(&commit),
        vec![
            "content/bots/pm-nia-lee/BOT.md",
            "content/team.yaml",
            "policies/bots/pm-nia-lee.sandbox.yaml",
            "policies/bots/pm-nia-lee.yaml",
        ]
    );
    let files = e.mock.files(CONTENT_REPO);
    // The policy files are generated, unchanged, from the content with Nia in it.
    let [policy, spec] = personal_policies(
        &e.root,
        &files["content/team.yaml"],
        "pm-nia-lee",
        &render_personal_bot("nia-lee"),
        Utc::now().date_naive(),
    )
    .unwrap();
    assert_eq!(files["policies/bots/pm-nia-lee.yaml"], policy);
    assert_eq!(files["policies/bots/pm-nia-lee.sandbox.yaml"], spec);
    assert!(policy.starts_with(pmos_policy::GENERATED_MARKER));
    assert!(policy.contains("pm-nia-lee") && spec.contains("pm-nia-lee"));
    // Nia can use PM OS at once: the Slack bot's welcome checklist works before any pull.
    let (_, nia) = bot(&e, Method::GET, "/internal/onboarding?user=nia-lee", None).await;
    assert_eq!(nia["member"], true, "{nia}");
    let (status, _) = bot(
        &e,
        Method::GET,
        "/internal/oauth/github/start?user=nia-lee",
        None,
    )
    .await;
    assert_eq!(status, 200);
    let yaml = &files["content/team.yaml"];
    assert!(yaml.starts_with(TEAM_HEADER), "the header is kept");
    let parsed = TeamFile::from_yaml(yaml).unwrap();
    assert_eq!(parsed.members.len(), team().len() + 1);
    let nia = parsed.by_handle("nia-lee").unwrap();
    assert_eq!(nia.slack_id, "U0NIA");
    assert_eq!(nia.email.as_deref(), Some("Nia.Lee@example.com"));
    assert_eq!(nia.roles, vec![Role::Pm]);
    assert_eq!(
        files["content/bots/pm-nia-lee/BOT.md"],
        render_personal_bot("nia-lee")
    );
    // With Raj's own token.
    let patch = e.mock.requests("PATCH", "/git/refs/heads/main");
    assert_eq!(patch.len(), 1);
    assert_eq!(patch[0].auth.as_deref(), Some("Bearer ghu-raj-gh-1"));

    let event = e
        .audit
        .events()
        .into_iter()
        .find(|ev| ev.tool == "members.add")
        .unwrap();
    assert_eq!(event.client, Client::Bot);
    assert_eq!(event.user, "raj");
    assert_eq!(event.outcome, Outcome::Approved);
    assert_eq!(event.output_ref.as_deref(), Some(commit.as_str()));
    let logged = serde_json::to_string(&e.audit.events()).unwrap();
    assert!(!logged.contains("ghu-"), "no token in the audit log");

    // Decided once.
    let (status, body) = decide(&e, &id, json!({"decision": "reject", "decided_by": "kim"})).await;
    assert_eq!(status, 409);
    assert_eq!(body["code"], "already_decided");
    assert_eq!(body["request"]["status"], "approved");
}

#[tokio::test]
async fn only_an_admin_with_github_and_push_access_can_approve() {
    let e = env().await;
    let (_, body) = request(&e, "U0NIA", "nia@example.com", "Nia").await;
    let id = body["request"]["id"].as_str().unwrap().to_string();

    // A PM isn't a platform owner.
    let (status, body) = decide(&e, &id, json!({"decision": "approve", "decided_by": "ana"})).await;
    assert_eq!(status, 403, "{body}");
    let denied = e
        .audit
        .events()
        .into_iter()
        .find(|ev| ev.tool == "members.add")
        .unwrap();
    assert_eq!(denied.outcome, Outcome::Denied);
    assert_eq!(denied.user, "ana");

    // Kim is, but hasn't linked GitHub; then can't push to the content repo.
    let (status, body) = decide(&e, &id, json!({"decision": "approve", "decided_by": "kim"})).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (409, Some("github_not_linked")),
        "{body}"
    );
    link(&e, "kim", "kim").await;
    let (status, body) = decide(&e, &id, json!({"decision": "approve", "decided_by": "kim"})).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (409, Some("no_push")),
        "{body}"
    );
    let (_, pending) = bot(
        &e,
        Method::GET,
        "/internal/members/requests?status=pending",
        None,
    )
    .await;
    assert_eq!(pending["requests"][0]["id"], id.as_str(), "still pending");
    assert!(
        e.mock.requests("PATCH", "/git/refs/").is_empty(),
        "nothing committed"
    );

    // A handle that isn't valid, or is taken.
    link(&e, "raj", "raj-gh").await;
    for handle in ["Nia.L", "ana"] {
        let (status, body) = decide(
            &e,
            &id,
            json!({"decision": "approve", "decided_by": "raj", "handle": handle}),
        )
        .await;
        assert_eq!(
            (status, body["code"].as_str()),
            (422, Some("bad_handle")),
            "{handle}"
        );
    }
    // Kim may reject.
    let (status, body) = decide(&e, &id, json!({"decision": "reject", "decided_by": "kim"})).await;
    assert_eq!(status, 200);
    assert_eq!(body["request"]["status"], "rejected");
    let rejected = e
        .audit
        .events()
        .into_iter()
        .rev()
        .find(|ev| ev.tool == "members.add")
        .unwrap();
    assert_eq!(rejected.outcome, Outcome::Rejected);
    assert_eq!(rejected.user, "kim");
}

/// The Slack bot may grant `pm` and `skill-author` only: an `admin` grant from it is
/// refused (S2), audited, and leaves the request pending. The service token may still.
#[tokio::test]
async fn an_admin_grant_from_slack_is_refused() {
    let e = env().await;
    link(&e, "raj", "raj-gh").await;
    let (_, body) = request(&e, "U0NIA", "nia@example.com", "Nia").await;
    let id = body["request"]["id"].as_str().unwrap().to_string();
    let head = e.mock.head(CONTENT_REPO);
    for roles in [json!(["admin"]), json!(["pm", "admin"])] {
        let (status, body) = decide(
            &e,
            &id,
            json!({"decision": "approve", "decided_by": "raj", "handle": "nia", "roles": roles}),
        )
        .await;
        assert_eq!(status, 403, "{body}");
        assert_eq!(body["code"], "admin_by_pull_request");
        let ev = e
            .audit
            .events()
            .into_iter()
            .rev()
            .find(|ev| ev.tool == "members.add")
            .unwrap();
        assert_eq!((ev.outcome, ev.client), (Outcome::Denied, Client::Bot));
    }
    assert_eq!(e.mock.head(CONTENT_REPO), head, "nothing committed");
    let (_, pending) = bot(
        &e,
        Method::GET,
        "/internal/members/requests?status=pending",
        None,
    )
    .await;
    assert_eq!(pending["requests"][0]["id"], id.as_str());

    // The orchestrator's token isn't limited.
    let (status, body) = call(
        &e,
        Method::POST,
        &format!("/internal/members/requests/{id}/decision"),
        Some(json!({"decision": "approve", "decided_by": "raj", "handle": "nia", "roles": ["pm", "admin"]})),
        SERVICE_TOKEN,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let team = TeamFile::from_yaml(&e.mock.files(CONTENT_REPO)["content/team.yaml"]).unwrap();
    assert_eq!(
        team.by_handle("nia").unwrap().roles,
        vec![Role::Pm, Role::Admin]
    );
}

#[tokio::test]
async fn a_ref_conflict_is_retried_and_a_failure_is_recorded() {
    let e = env().await;
    link(&e, "raj", "raj-gh").await;
    let (_, body) = request(&e, "U0NIA", "nia@example.com", "Nia").await;
    let id = body["request"]["id"].as_str().unwrap().to_string();

    // Someone pushes twice while PM OS commits: it rebuilds the commit on the new head.
    *e.mock.conflicts.lock().unwrap() = 2;
    let (status, body) = decide(
        &e,
        &id,
        json!({"decision": "approve", "decided_by": "raj", "handle": "nia", "roles": ["pm", "skill-author"]}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(e.mock.requests("PATCH", "/git/refs/heads/main").len(), 3);
    let commit = body["request"]["commit"].as_str().unwrap();
    assert_eq!(e.mock.head(CONTENT_REPO), commit);
    let parent = e.mock.commit(commit).parents[0].clone();
    assert_eq!(e.mock.commit(&parent).message, "someone else's change");
    let files = e.mock.files(CONTENT_REPO);
    assert_eq!(
        files.keys().filter(|p| p.starts_with("other/")).count(),
        2,
        "their changes stay"
    );
    let nia = TeamFile::from_yaml(&files["content/team.yaml"]).unwrap();
    assert_eq!(
        nia.by_handle("nia").unwrap().roles,
        vec![Role::Pm, Role::SkillAuthor]
    );

    // A branch that never stops moving: the request fails, readably.
    let (_, body) = request(&e, "U0OMI", "omi@example.com", "Omi").await;
    let id = body["request"]["id"].as_str().unwrap().to_string();
    *e.mock.conflicts.lock().unwrap() = 100;
    let (status, body) = decide(&e, &id, json!({"decision": "approve", "decided_by": "raj"})).await;
    assert_eq!(status, 502, "{body}");
    assert_eq!(body["request"]["status"], "failed");
    assert!(
        body["request"]["error"]
            .as_str()
            .unwrap()
            .contains("kept moving"),
        "{body}"
    );
    let failed = e
        .audit
        .events()
        .into_iter()
        .rev()
        .find(|ev| ev.tool == "members.add")
        .unwrap();
    assert_eq!(failed.outcome, Outcome::Failed);
}

#[tokio::test]
async fn removal_commits_and_unlinks() {
    let e = env().await;
    link(&e, "raj", "raj-gh").await;
    // Bo's accounts and memory record.
    put_github(&e, "bo", "ghr-bo-1", 3600).await;
    e.vault
        .put(&Credential {
            provider: Provider::Atlassian,
            ..e.vault.get("bo", Provider::Github).await.unwrap().unwrap()
        })
        .await
        .unwrap();
    e.stores
        .memory_repos
        .put(&MemoryRepo {
            user: "bo".into(),
            repo: "example-org/pm-os-memory-bo".into(),
            verified_at: Utc::now(),
        })
        .await
        .unwrap();

    let remove = |handle: &str, by: &str| {
        let path = format!("/internal/members/{handle}/remove");
        let body = json!({"decided_by": by});
        let e = &e;
        async move { bot(e, Method::POST, &path, Some(body)).await }
    };
    assert_eq!(remove("bo", "ana").await.0, 403, "admins only");
    let (status, body) = remove("raj", "raj").await;
    assert_eq!((status, body["code"].as_str()), (403, Some("self_removal")));
    assert_eq!(remove("zed", "raj").await.0, 404);
    // Ana owns team bots in the content: removing her would break the lint.
    let (status, body) = remove("ana", "raj").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (409, Some("owns_bots")),
        "{body}"
    );

    let (status, body) = remove("bo", "raj").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["handle"], "bo");
    let commit = body["commit"].as_str().unwrap();
    assert_eq!(e.mock.head(CONTENT_REPO), commit);
    assert_eq!(
        e.mock.changed(commit),
        vec![
            "content/bots/pm-bo/BOT.md",
            "content/team.yaml",
            "policies/bots/pm-bo.sandbox.yaml",
            "policies/bots/pm-bo.yaml",
        ]
    );
    // Gone at once.
    let (_, bo) = bot(&e, Method::GET, "/internal/onboarding?user=bo", None).await;
    assert_eq!(bo["member"], false);
    assert!(e
        .svc
        .create_session("bo", "ask", "run-after", None, Utc::now())
        .await
        .is_err());
    let files = e.mock.files(CONTENT_REPO);
    assert!(!files.contains_key("content/bots/pm-bo/BOT.md"));
    let team = TeamFile::from_yaml(&files["content/team.yaml"]).unwrap();
    assert!(team.by_handle("bo").is_none());
    assert_eq!(team.members.len(), 5);
    assert!(e
        .mock
        .commit(commit)
        .message
        .starts_with("Remove bo from PM OS"));

    assert!(
        e.vault.list("bo").await.unwrap().is_empty(),
        "every account unlinked"
    );
    assert!(e.stores.memory_repos.get("bo").await.unwrap().is_none());
    let event = e
        .audit
        .events()
        .into_iter()
        .rev()
        .find(|ev| ev.tool == "members.remove")
        .unwrap();
    assert_eq!(event.outcome, Outcome::Ok);
    assert_eq!(event.user, "raj");
    assert_eq!(event.output_ref.as_deref(), Some(commit));
}

#[tokio::test]
async fn requests_are_checked() {
    let e = env().await;
    let (status, body) = request(&e, "U0ANA", "ana@example.com", "Ana").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (409, Some("already_member"))
    );
    for (slack, email) in [("U0X", "not-an-email"), ("bad", "x@example.com"), ("", "")] {
        let (status, body) = request(&e, slack, email, "X").await;
        assert_eq!(
            (status, body["code"].as_str()),
            (422, Some("bad_request")),
            "{slack}"
        );
    }
    for n in 0..20 {
        let (status, _) = request(&e, &format!("U0N{n}"), &format!("n{n}@example.com"), "").await;
        assert_eq!(status, 201);
    }
    let (status, body) = request(&e, "U0N99", "n99@example.com", "").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (429, Some("too_many_pending"))
    );
    // Proposed handles avoid each other and the team.
    let (_, body) = request(&e, "U0N0", "", "").await;
    assert_eq!(body["code"], "bad_request");
    let (_, list) = bot(
        &e,
        Method::GET,
        "/internal/members/requests?status=pending",
        None,
    )
    .await;
    let handles: HashSet<&str> = list["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["proposed_handle"].as_str().unwrap())
        .collect();
    assert_eq!(handles.len(), 20);
    let (status, _) = bot(
        &e,
        Method::GET,
        "/internal/members/requests?status=maybe",
        None,
    )
    .await;
    assert_eq!(status, 422);
    // The orchestrator may call these too; a sandbox may not.
    let (status, _) = call(
        &e,
        Method::GET,
        "/internal/members/requests",
        None,
        SERVICE_TOKEN,
    )
    .await;
    assert_eq!(status, 200);
    let (status, _) = call(
        &e,
        Method::GET,
        "/internal/members/requests",
        None,
        "pmos_s_x",
    )
    .await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn onboarding_status_shows_each_step() {
    let e = env().await;
    let (_, raj) = bot(&e, Method::GET, "/internal/onboarding?user=raj", None).await;
    assert_eq!(
        raj,
        json!({"member": true, "handle": "raj", "admin": true,
               "linked": {"atlassian": false, "slack": false, "github": false},
               "memory_repo": null, "personal_bot": null, "pending_request": null})
    );
    set_up_memory(&e, &[]).await;
    let (_, ana) = bot(&e, Method::GET, "/internal/onboarding?user=ana", None).await;
    assert_eq!(ana["admin"], false);
    assert_eq!(ana["linked"]["github"], true);
    assert_eq!(ana["memory_repo"], MEMORY_REPO);
    assert_eq!(
        ana["personal_bot"], "pm-ana",
        "the fixture content has her bot"
    );
    // By Slack id, a member is found too.
    let (_, by_slack) = bot(&e, Method::GET, "/internal/onboarding?slack_id=U0ANA", None).await;
    assert_eq!(by_slack["handle"], "ana");
    let (_, stranger) = bot(&e, Method::GET, "/internal/onboarding?slack_id=U0NEW", None).await;
    assert_eq!(stranger["member"], false);
    assert_eq!(stranger["pending_request"], Value::Null);
    let (status, _) = bot(&e, Method::GET, "/internal/onboarding", None).await;
    assert_eq!(status, 400);
}
