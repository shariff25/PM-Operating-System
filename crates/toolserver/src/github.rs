//! GitHub's REST API, called with a PM's own GitHub App user token (`docs/onboarding-spec.md`
//! §1). The tool server never uses an installation token, so every call can reach only what
//! that PM can already reach: memory reads and writes act as the memory's owner, and
//! content-repo commits act as the platform owner who approved them.
//!
//! Calls used (checked against docs.github.com, API version 2022-11-28):
//! - `GET /repos/{repo}`: privacy, archived, owner, default branch, push permission.
//! - `GET /repos/{repo}/contents/{path}?ref=`: one file and its blob sha.
//! - `GET /repos/{repo}/git/trees/{ref}?recursive=1` and `GET /repos/{repo}/git/blobs/{sha}`.
//! - A commit of several files through the git data API: `GET git/ref/heads/{branch}`,
//!   `GET git/commits/{sha}`, `POST git/blobs`, `POST git/trees` (with `base_tree`),
//!   `POST git/commits`, then `PATCH git/refs/heads/{branch}` with `force: false`. A `422`
//!   there means the branch moved: the commit is rebuilt on the new head and tried again.
//! - `PUT /repos/{repo}/contents/{path}`: only for the first file in an empty repo, where
//!   the git data API has nothing to build on.

use std::time::Duration;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use reqwest::{Method, RequestBuilder, StatusCode};
use serde_json::{json, Value};

use crate::vault::Secret;

/// How many times a commit is rebuilt after the branch moved under it.
pub const COMMIT_RETRIES: usize = 3;
/// The API version every request asks for.
pub const API_VERSION: &str = "2022-11-28";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GitHubError {
    #[error("not found on GitHub: {0}")]
    NotFound(String),
    /// The token was refused: the PM needs to link GitHub again.
    #[error("GitHub rejected your sign-in; reconnect GitHub in PM OS")]
    Reconnect,
    #[error("GitHub refused: {0}")]
    Forbidden(String),
    #[error("GitHub's rate limit was reached; try again in {0} s")]
    RateLimited(u64),
    /// The branch moved while committing, or a file changed under a contents-API write.
    #[error("the branch moved while committing; try again")]
    Conflict,
    #[error("the repository is empty")]
    Empty,
    #[error("GitHub refused the request: {0}")]
    Invalid(String),
    #[error("GitHub is unavailable: {0}")]
    Unavailable(String),
}

/// What PM OS needs to know about a repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoInfo {
    /// `owner/name`, as GitHub spells it.
    pub full_name: String,
    pub owner: String,
    pub private: bool,
    pub archived: bool,
    pub default_branch: String,
    /// Whether the token's user may push.
    pub push: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileContent {
    pub sha: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub path: String,
    pub sha: String,
    /// `blob`, `tree` or `commit`.
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    /// The tree's own sha: unchanged means nothing under it changed.
    pub sha: String,
    pub entries: Vec<TreeEntry>,
    /// GitHub stops listing past 100,000 entries or 7 MB.
    pub truncated: bool,
}

/// A branch head: its commit and that commit's tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    pub commit: String,
    pub tree: String,
}

/// One file in a commit: new text, or `None` to delete it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    pub text: Option<String>,
}

impl Change {
    pub fn write(path: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            text: Some(text.into()),
        }
    }

    pub fn delete(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            text: None,
        }
    }
}

/// A GitHub login or org: 1–39 letters, digits and single hyphens.
pub use pmos_contract::onboarding::valid_github_login as valid_login;

/// `owner/name`, with a valid owner and a repo name of letters, digits, `.`, `_` and `-`.
pub fn valid_repo(full: &str) -> bool {
    let Some((owner, name)) = full.split_once('/') else {
        return false;
    };
    valid_login(owner)
        && !name.is_empty()
        && name.len() <= 100
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// A path for a URL: anything but letters, digits and `-._~/` is percent-encoded.
fn encode_path(path: &str) -> String {
    let mut out = String::new();
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub struct GitHub {
    /// `https://api.github.com`
    api: String,
    http: reqwest::Client,
}

impl GitHub {
    pub fn new(api: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("pmos-toolserver/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("http client builds");
        Self {
            api: api.into().trim_end_matches('/').to_string(),
            http,
        }
    }

    fn request(&self, method: Method, path: &str, token: &Secret) -> RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.api))
            .bearer_auth(token.expose())
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
    }

    async fn send(&self, req: RequestBuilder, what: &str) -> Result<Value, GitHubError> {
        let res = req
            .send()
            .await
            .map_err(|e| GitHubError::Unavailable(format!("{what}: {e}")))?;
        let status = res.status();
        if status.is_success() {
            if status == StatusCode::NO_CONTENT {
                return Ok(Value::Null);
            }
            return res
                .json()
                .await
                .map_err(|e| GitHubError::Unavailable(format!("{what}: bad response: {e}")));
        }
        let headers = res.headers().clone();
        let body: Value = res.json().await.unwrap_or(Value::Null);
        let message = body["message"].as_str().unwrap_or("no detail").to_string();
        Err(match status {
            StatusCode::UNAUTHORIZED => GitHubError::Reconnect,
            StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS => {
                match rate_limit_wait(status, &headers, &message, chrono::Utc::now().timestamp()) {
                    Some(secs) => GitHubError::RateLimited(secs),
                    None => GitHubError::Forbidden(format!("{what}: {message}")),
                }
            }
            StatusCode::NOT_FOUND => GitHubError::NotFound(what.to_string()),
            StatusCode::CONFLICT if message.contains("is empty") => GitHubError::Empty,
            StatusCode::CONFLICT => GitHubError::Conflict,
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
                GitHubError::Invalid(format!("{what}: {message}"))
            }
            s => GitHubError::Unavailable(format!("{what}: GitHub returned {s}")),
        })
    }

    /// A repo, as the token's user sees it.
    pub async fn repo(&self, token: &Secret, repo: &str) -> Result<RepoInfo, GitHubError> {
        let v = self
            .send(
                self.request(Method::GET, &format!("/repos/{}", encode_path(repo)), token),
                &format!("repository {repo}"),
            )
            .await?;
        Ok(RepoInfo {
            full_name: v["full_name"].as_str().unwrap_or(repo).to_string(),
            owner: v["owner"]["login"].as_str().unwrap_or_default().to_string(),
            private: v["private"].as_bool().unwrap_or(false),
            archived: v["archived"].as_bool().unwrap_or(false),
            default_branch: v["default_branch"].as_str().unwrap_or("main").to_string(),
            push: v["permissions"]["push"].as_bool().unwrap_or(false),
        })
    }

    /// One file at `git_ref` (a branch or a commit sha), or `None` if it doesn't exist there.
    pub async fn file(
        &self,
        token: &Secret,
        repo: &str,
        path: &str,
        git_ref: &str,
    ) -> Result<Option<FileContent>, GitHubError> {
        let req = self
            .request(
                Method::GET,
                &format!(
                    "/repos/{}/contents/{}",
                    encode_path(repo),
                    encode_path(path)
                ),
                token,
            )
            .query(&[("ref", git_ref)]);
        let v = match self.send(req, &format!("{path} in {repo}")).await {
            Ok(v) => v,
            Err(GitHubError::NotFound(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        if v["type"] != "file" {
            return Err(GitHubError::Invalid(format!(
                "{path} in {repo} is not a file"
            )));
        }
        let sha = v["sha"].as_str().unwrap_or_default().to_string();
        // Files over 1 MB come back without content; their blob has it.
        let text = match (v["encoding"].as_str(), v["content"].as_str()) {
            (Some("base64"), Some(content)) => decode(content, path)?,
            _ => self.blob(token, repo, &sha).await?,
        };
        Ok(Some(FileContent { sha, text }))
    }

    /// Every entry under `tree_ish` (a branch, commit or tree sha), recursively.
    pub async fn tree(
        &self,
        token: &Secret,
        repo: &str,
        tree_ish: &str,
    ) -> Result<Tree, GitHubError> {
        let req = self
            .request(
                Method::GET,
                &format!(
                    "/repos/{}/git/trees/{}",
                    encode_path(repo),
                    encode_path(tree_ish)
                ),
                token,
            )
            .query(&[("recursive", "1")]);
        let v = self.send(req, &format!("the files of {repo}")).await?;
        let entries = v["tree"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|e| TreeEntry {
                        path: e["path"].as_str().unwrap_or_default().to_string(),
                        sha: e["sha"].as_str().unwrap_or_default().to_string(),
                        kind: e["type"].as_str().unwrap_or_default().to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Tree {
            sha: v["sha"].as_str().unwrap_or_default().to_string(),
            entries,
            truncated: v["truncated"].as_bool().unwrap_or(false),
        })
    }

    /// A blob's text.
    pub async fn blob(&self, token: &Secret, repo: &str, sha: &str) -> Result<String, GitHubError> {
        let v = self
            .send(
                self.request(
                    Method::GET,
                    &format!(
                        "/repos/{}/git/blobs/{}",
                        encode_path(repo),
                        encode_path(sha)
                    ),
                    token,
                ),
                &format!("blob {sha} in {repo}"),
            )
            .await?;
        decode(v["content"].as_str().unwrap_or_default(), sha)
    }

    /// The branch's head commit and its tree. [`GitHubError::Empty`] for an empty repo.
    pub async fn head(
        &self,
        token: &Secret,
        repo: &str,
        branch: &str,
    ) -> Result<Head, GitHubError> {
        let r = self
            .send(
                self.request(
                    Method::GET,
                    &format!(
                        "/repos/{}/git/ref/heads/{}",
                        encode_path(repo),
                        encode_path(branch)
                    ),
                    token,
                ),
                &format!("branch {branch} of {repo}"),
            )
            .await?;
        let commit = r["object"]["sha"]
            .as_str()
            .ok_or_else(|| GitHubError::Unavailable("a branch with no commit".into()))?
            .to_string();
        let c = self
            .send(
                self.request(
                    Method::GET,
                    &format!(
                        "/repos/{}/git/commits/{}",
                        encode_path(repo),
                        encode_path(&commit)
                    ),
                    token,
                ),
                &format!("commit {commit} of {repo}"),
            )
            .await?;
        let tree = c["tree"]["sha"]
            .as_str()
            .ok_or_else(|| GitHubError::Unavailable("a commit with no tree".into()))?
            .to_string();
        Ok(Head { commit, tree })
    }

    /// A commit on top of `head` with `changes`, not yet on any branch. Returns its sha.
    pub async fn create_commit(
        &self,
        token: &Secret,
        repo: &str,
        head: &Head,
        message: &str,
        changes: &[Change],
    ) -> Result<String, GitHubError> {
        let base = format!("/repos/{}/git", encode_path(repo));
        let mut entries = Vec::new();
        for change in changes {
            let sha = match &change.text {
                Some(text) => {
                    let blob = self
                        .send(
                            self.request(Method::POST, &format!("{base}/blobs"), token)
                                .json(&json!({"content": B64.encode(text), "encoding": "base64"})),
                            &format!("a blob for {}", change.path),
                        )
                        .await?;
                    Value::String(blob["sha"].as_str().unwrap_or_default().to_string())
                }
                // A null sha deletes the path.
                None => Value::Null,
            };
            entries
                .push(json!({"path": change.path, "mode": "100644", "type": "blob", "sha": sha}));
        }
        let tree = self
            .send(
                self.request(Method::POST, &format!("{base}/trees"), token)
                    .json(&json!({"base_tree": head.tree, "tree": entries})),
                &format!("a tree for {repo}"),
            )
            .await?;
        let commit = self
            .send(
                self.request(Method::POST, &format!("{base}/commits"), token)
                    .json(&json!({
                        "message": message,
                        "tree": tree["sha"],
                        "parents": [head.commit],
                    })),
                &format!("a commit for {repo}"),
            )
            .await?;
        commit["sha"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| GitHubError::Unavailable("a commit with no sha".into()))
    }

    /// Move `branch` to `commit`, only as a fast-forward. [`GitHubError::Conflict`] if the
    /// branch moved since the commit's parent was read.
    pub async fn update_ref(
        &self,
        token: &Secret,
        repo: &str,
        branch: &str,
        commit: &str,
    ) -> Result<(), GitHubError> {
        let req = self
            .request(
                Method::PATCH,
                &format!(
                    "/repos/{}/git/refs/heads/{}",
                    encode_path(repo),
                    encode_path(branch)
                ),
                token,
            )
            .json(&json!({"sha": commit, "force": false}));
        match self.send(req, &format!("branch {branch} of {repo}")).await {
            Ok(_) => Ok(()),
            Err(GitHubError::Invalid(_)) => Err(GitHubError::Conflict),
            Err(e) => Err(e),
        }
    }

    /// Commit `changes` to `branch` in one commit, rebuilding it on the new head up to
    /// [`COMMIT_RETRIES`] times if the branch moves meanwhile. Returns the commit sha.
    pub async fn commit_files(
        &self,
        token: &Secret,
        repo: &str,
        branch: &str,
        message: &str,
        changes: &[Change],
    ) -> Result<String, GitHubError> {
        for attempt in 0..=COMMIT_RETRIES {
            let head = self.head(token, repo, branch).await?;
            let commit = self
                .create_commit(token, repo, &head, message, changes)
                .await?;
            match self.update_ref(token, repo, branch, &commit).await {
                Ok(()) => return Ok(commit),
                Err(GitHubError::Conflict) => {
                    tracing::info!(
                        repo,
                        branch,
                        attempt,
                        "branch moved while committing; retrying"
                    );
                }
                Err(e) => return Err(e),
            }
        }
        Err(GitHubError::Conflict)
    }

    /// Create the first file in an empty repo, which also creates its default branch.
    /// Returns the commit sha.
    pub async fn create_first_file(
        &self,
        token: &Secret,
        repo: &str,
        path: &str,
        message: &str,
        text: &str,
    ) -> Result<String, GitHubError> {
        let v = self
            .send(
                self.request(
                    Method::PUT,
                    &format!(
                        "/repos/{}/contents/{}",
                        encode_path(repo),
                        encode_path(path)
                    ),
                    token,
                )
                .json(&json!({"message": message, "content": B64.encode(text)})),
                &format!("{path} in {repo}"),
            )
            .await?;
        v["commit"]["sha"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| GitHubError::Unavailable("a commit with no sha".into()))
    }
}

/// Base64 content as GitHub sends it (with line breaks), as UTF-8 text.
fn decode(content: &str, what: &str) -> Result<String, GitHubError> {
    let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = B64
        .decode(compact)
        .map_err(|_| GitHubError::Unavailable(format!("{what}: content is not base64")))?;
    String::from_utf8(bytes).map_err(|_| GitHubError::Invalid(format!("{what} is not UTF-8 text")))
}

/// How long to wait, if a `403` or `429` is a rate limit: `Retry-After` (secondary limits),
/// else `x-ratelimit-reset` when `x-ratelimit-remaining` is `0`. A `403` that is neither is a
/// permission refusal.
fn rate_limit_wait(
    status: StatusCode,
    headers: &reqwest::header::HeaderMap,
    message: &str,
    now: i64,
) -> Option<u64> {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
    };
    if let Some(secs) = header("retry-after").and_then(|v| v.parse::<u64>().ok()) {
        return Some(secs.max(1));
    }
    let exhausted = header("x-ratelimit-remaining") == Some("0");
    if exhausted {
        if let Some(reset) = header("x-ratelimit-reset").and_then(|v| v.parse::<i64>().ok()) {
            return Some((reset - now).max(1) as u64);
        }
    }
    if exhausted
        || status == StatusCode::TOO_MANY_REQUESTS
        || message.to_ascii_lowercase().contains("rate limit")
    {
        return Some(60);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    #[test]
    fn repo_names() {
        assert!(valid_repo("fortanix/pm-os-memory-ana"));
        assert!(valid_repo("Fortanix/a.b_c"));
        for bad in [
            "fortanix",
            "/x",
            "fortanix/",
            "fortanix/..",
            "a/b/c",
            "fort anix/x",
            "-x/y",
        ] {
            assert!(!valid_repo(bad), "{bad}");
        }
    }

    #[test]
    fn paths_are_encoded() {
        assert_eq!(encode_path("content/team.yaml"), "content/team.yaml");
        assert_eq!(encode_path("a b?#"), "a%20b%3F%23");
    }

    #[test]
    fn rate_limits_are_told_from_refusals() {
        let mut h = HeaderMap::new();
        assert_eq!(
            rate_limit_wait(StatusCode::FORBIDDEN, &h, "Resource not accessible", 0),
            None
        );
        assert_eq!(
            rate_limit_wait(StatusCode::TOO_MANY_REQUESTS, &h, "", 0),
            Some(60)
        );
        h.insert("retry-after", HeaderValue::from_static("7"));
        assert_eq!(rate_limit_wait(StatusCode::FORBIDDEN, &h, "", 0), Some(7));
        let mut h = HeaderMap::new();
        h.insert("x-ratelimit-remaining", HeaderValue::from_static("0"));
        h.insert("x-ratelimit-reset", HeaderValue::from_static("1000"));
        assert_eq!(
            rate_limit_wait(StatusCode::FORBIDDEN, &h, "", 990),
            Some(10)
        );
        assert_eq!(
            rate_limit_wait(StatusCode::FORBIDDEN, &h, "", 2000),
            Some(1)
        );
    }

    #[test]
    fn content_decodes_with_line_breaks() {
        assert_eq!(decode("aGVs\nbG8=\n", "x").unwrap(), "hello");
        assert!(decode("!!", "x").is_err());
    }
}
