//! A small GitHub REST client for admin proposals: one repository, one token
//! (`PMOS_WEB_GITHUB_TOKEN`), and only the calls a proposal needs (API version 2022-11-28):
//!
//! - `GET /repos/{repo}`: the default branch.
//! - `GET /repos/{repo}/git/ref/heads/{branch}` and `GET /repos/{repo}/git/commits/{sha}`: the
//!   base commit and its tree.
//! - `GET /repos/{repo}/git/trees/{tree}?recursive=1`: blob ids on the base, for the stale-base
//!   check.
//! - `POST /repos/{repo}/git/trees` (with `base_tree`, file text inline, `sha: null` to delete)
//!   and `POST /repos/{repo}/git/commits`: one commit on the base.
//! - `POST /repos/{repo}/git/refs`: a new branch at that commit. Never an update to an existing
//!   ref, so the default branch is never written.
//! - `POST /repos/{repo}/pulls` and `POST /repos/{repo}/issues/{n}/labels`.
//! - `GET /repos/{repo}/pulls?state=open`: open proposals.
//!
//! The token goes only in the `Authorization` header, only to the configured API base, and
//! redirects are not followed. Errors carry what was being done and GitHub's own `message`,
//! never the token or a file's text.

use std::time::Duration;

use reqwest::{Method, RequestBuilder, StatusCode};
use serde_json::{json, Value};

use crate::admin_config::GitHubConfig;

/// The API version every request asks for.
pub const API_VERSION: &str = "2022-11-28";
/// Open pull requests read, at most (pages of 100).
pub const MAX_LISTED: usize = 300;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitHubError {
    NotFound(String),
    /// `401`: the token is wrong or expired.
    Unauthorized,
    /// `403` that isn't a rate limit: the token lacks a permission.
    Forbidden(String),
    RateLimited(u64),
    /// `409`/`422` on a write: the branch exists already, or the request was refused.
    Invalid(String),
    Unavailable(String),
}

impl std::fmt::Display for GitHubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(what) => write!(f, "not found on GitHub: {what}"),
            Self::Unauthorized => f.write_str("GitHub refused the token"),
            Self::Forbidden(what) => write!(f, "GitHub refused: {what}"),
            Self::RateLimited(s) => write!(f, "GitHub's rate limit was reached; retry in {s} s"),
            Self::Invalid(what) => write!(f, "GitHub refused the request: {what}"),
            Self::Unavailable(what) => write!(f, "GitHub is unavailable: {what}"),
        }
    }
}

/// A branch's head commit and its tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    pub commit: String,
    pub tree: String,
}

/// One file of a commit: `text` to write, `None` to delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileWrite {
    pub path: String,
    pub text: Option<String>,
}

/// Blob ids by path, recursively, at some tree.
#[derive(Debug, Clone, Default)]
pub struct Blobs {
    pub by_path: std::collections::BTreeMap<String, String>,
    /// GitHub cut the listing short: the stale check can't be trusted.
    pub truncated: bool,
}

/// A pull request as the list shows it.
#[derive(Debug, Clone)]
pub struct Pull {
    pub number: u64,
    pub url: String,
    pub title: String,
    pub branch: String,
    pub base: String,
    pub draft: bool,
    pub author: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub labels: Vec<String>,
    pub body: String,
}

/// A path for a URL: anything but letters, digits and `-._~/` is percent-encoded.
pub fn encode_path(path: &str) -> String {
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
    http: reqwest::Client,
    api: String,
    repo: String,
    token: String,
}

impl std::fmt::Debug for GitHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitHub")
            .field("api", &self.api)
            .field("repo", &self.repo)
            .field("token", &"<redacted>")
            .finish()
    }
}

impl GitHub {
    pub fn new(config: &GitHubConfig) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("pmos-web/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| format!("GitHub HTTP client: {e}"))?;
        Ok(Self {
            http,
            api: config.api.clone(),
            repo: config.repo.clone(),
            token: config.token.clone(),
        })
    }

    pub fn repo(&self) -> &str {
        &self.repo
    }

    fn request(&self, method: Method, rest: &str) -> RequestBuilder {
        self.http
            .request(
                method,
                format!("{}/repos/{}{rest}", self.api, encode_path(&self.repo)),
            )
            .bearer_auth(&self.token)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
    }

    async fn send(&self, req: RequestBuilder, what: &str) -> Result<Value, GitHubError> {
        let res = req.send().await.map_err(|e| {
            GitHubError::Unavailable(format!(
                "{what}: {}",
                if e.is_timeout() {
                    "timed out"
                } else {
                    "didn't answer"
                }
            ))
        })?;
        let status = res.status();
        if status.is_success() {
            if status == StatusCode::NO_CONTENT {
                return Ok(Value::Null);
            }
            return res
                .json()
                .await
                .map_err(|_| GitHubError::Unavailable(format!("{what}: a reply that isn't JSON")));
        }
        let headers = res.headers().clone();
        let body: Value = res.json().await.unwrap_or(Value::Null);
        let message: String = body["message"]
            .as_str()
            .unwrap_or("no detail")
            .chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect();
        Err(match status {
            StatusCode::UNAUTHORIZED => GitHubError::Unauthorized,
            StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS => {
                match rate_limit_wait(status, &headers, &message) {
                    Some(secs) => GitHubError::RateLimited(secs),
                    None => GitHubError::Forbidden(format!("{what}: {message}")),
                }
            }
            StatusCode::NOT_FOUND => GitHubError::NotFound(what.to_string()),
            StatusCode::CONFLICT | StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
                GitHubError::Invalid(format!("{what}: {message}"))
            }
            s => GitHubError::Unavailable(format!("{what}: GitHub answered {s}")),
        })
    }

    /// The repository's default branch.
    pub async fn default_branch(&self) -> Result<String, GitHubError> {
        let v = self
            .send(self.request(Method::GET, ""), "the repository")
            .await?;
        v["default_branch"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| GitHubError::Unavailable("the repository has no default branch".into()))
    }

    /// The head commit of `branch` and its tree.
    pub async fn head(&self, branch: &str) -> Result<Head, GitHubError> {
        let r = self
            .send(
                self.request(
                    Method::GET,
                    &format!("/git/ref/heads/{}", encode_path(branch)),
                ),
                &format!("branch {branch}"),
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
                    &format!("/git/commits/{}", encode_path(&commit)),
                ),
                "the base commit",
            )
            .await?;
        let tree = c["tree"]["sha"]
            .as_str()
            .ok_or_else(|| GitHubError::Unavailable("a commit with no tree".into()))?
            .to_string();
        Ok(Head { commit, tree })
    }

    /// Every blob under `tree`, by path.
    pub async fn blobs(&self, tree: &str) -> Result<Blobs, GitHubError> {
        let v = self
            .send(
                self.request(Method::GET, &format!("/git/trees/{}", encode_path(tree)))
                    .query(&[("recursive", "1")]),
                "the base's files",
            )
            .await?;
        let by_path = v["tree"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|e| e["type"] == "blob")
                    .filter_map(|e| Some((e["path"].as_str()?.into(), e["sha"].as_str()?.into())))
                    .collect()
            })
            .unwrap_or_default();
        Ok(Blobs {
            by_path,
            truncated: v["truncated"].as_bool().unwrap_or(false),
        })
    }

    /// One commit on `head` with `files`, on no branch yet. Returns its sha.
    pub async fn commit(
        &self,
        head: &Head,
        message: &str,
        files: &[FileWrite],
    ) -> Result<String, GitHubError> {
        let entries: Vec<Value> = files
            .iter()
            .map(|f| match &f.text {
                Some(text) => {
                    json!({"path": f.path, "mode": "100644", "type": "blob", "content": text})
                }
                // A null sha deletes the path.
                None => json!({"path": f.path, "mode": "100644", "type": "blob", "sha": null}),
            })
            .collect();
        let tree = self
            .send(
                self.request(Method::POST, "/git/trees")
                    .json(&json!({"base_tree": head.tree, "tree": entries})),
                "a tree for the proposal",
            )
            .await?;
        let commit = self
            .send(
                self.request(Method::POST, "/git/commits").json(&json!({
                    "message": message,
                    "tree": tree["sha"],
                    "parents": [head.commit],
                })),
                "the proposal's commit",
            )
            .await?;
        commit["sha"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| GitHubError::Unavailable("a commit with no sha".into()))
    }

    /// A new branch at `commit`. Fails (`Invalid`) if the branch exists: never moves a ref.
    pub async fn create_branch(&self, branch: &str, commit: &str) -> Result<(), GitHubError> {
        self.send(
            self.request(Method::POST, "/git/refs")
                .json(&json!({"ref": format!("refs/heads/{branch}"), "sha": commit})),
            &format!("branch {branch}"),
        )
        .await
        .map(|_| ())
    }

    /// Open a pull request from `branch` into `base`: `(number, html_url)`.
    pub async fn open_pull(
        &self,
        title: &str,
        branch: &str,
        base: &str,
        body: &str,
    ) -> Result<(u64, String), GitHubError> {
        let v = self
            .send(
                self.request(Method::POST, "/pulls").json(&json!({
                    "title": title,
                    "head": branch,
                    "base": base,
                    "body": body,
                    "maintainer_can_modify": false,
                    "draft": false,
                })),
                "the pull request",
            )
            .await?;
        let number = v["number"]
            .as_u64()
            .ok_or_else(|| GitHubError::Unavailable("a pull request with no number".into()))?;
        let url = v["html_url"].as_str().unwrap_or_default().to_string();
        Ok((number, url))
    }

    pub async fn add_labels(&self, number: u64, labels: &[&str]) -> Result<(), GitHubError> {
        self.send(
            self.request(Method::POST, &format!("/issues/{number}/labels"))
                .json(&json!({ "labels": labels })),
            "the pull request's labels",
        )
        .await
        .map(|_| ())
    }

    /// Open pull requests, newest first, at most [`MAX_LISTED`].
    pub async fn open_pulls(&self) -> Result<Vec<Pull>, GitHubError> {
        let mut out = Vec::new();
        for page in 1..=(MAX_LISTED / 100) {
            let v = self
                .send(
                    self.request(Method::GET, "/pulls").query(&[
                        ("state", "open"),
                        ("sort", "created"),
                        ("direction", "desc"),
                        ("per_page", "100"),
                        ("page", &page.to_string()),
                    ]),
                    "open pull requests",
                )
                .await?;
            let items = v.as_array().cloned().unwrap_or_default();
            let n = items.len();
            out.extend(items.iter().filter_map(pull));
            if n < 100 {
                break;
            }
        }
        Ok(out)
    }
}

fn pull(v: &Value) -> Option<Pull> {
    let s = |v: &Value| v.as_str().map(String::from);
    Some(Pull {
        number: v["number"].as_u64()?,
        url: s(&v["html_url"]).unwrap_or_default(),
        title: s(&v["title"]).unwrap_or_default(),
        branch: s(&v["head"]["ref"])?,
        base: s(&v["base"]["ref"]).unwrap_or_default(),
        draft: v["draft"].as_bool().unwrap_or(false),
        author: s(&v["user"]["login"]),
        created_at: s(&v["created_at"]),
        updated_at: s(&v["updated_at"]),
        labels: v["labels"]
            .as_array()
            .map(|a| a.iter().filter_map(|l| s(&l["name"])).collect())
            .unwrap_or_default(),
        body: s(&v["body"]).unwrap_or_default(),
    })
}

/// How long to wait, if a `403` or `429` is a rate limit.
fn rate_limit_wait(
    status: StatusCode,
    headers: &reqwest::header::HeaderMap,
    message: &str,
) -> Option<u64> {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
    };
    if let Some(secs) = header("retry-after").and_then(|v| v.parse::<u64>().ok()) {
        return Some(secs.clamp(1, 3600));
    }
    let exhausted = header("x-ratelimit-remaining") == Some("0");
    if exhausted {
        if let Some(reset) = header("x-ratelimit-reset").and_then(|v| v.parse::<i64>().ok()) {
            return Some((reset - chrono::Utc::now().timestamp()).clamp(1, 3600) as u64);
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

    #[test]
    fn paths_are_encoded() {
        assert_eq!(encode_path("acme/pmos"), "acme/pmos");
        assert_eq!(encode_path("pmos/proposal/a b"), "pmos/proposal/a%20b");
        assert_eq!(encode_path("x?y#z"), "x%3Fy%23z");
    }

    #[test]
    fn debug_never_shows_the_token() {
        let gh = GitHub::new(&GitHubConfig {
            repo: "acme/pmos".into(),
            token: "github_pat_SECRET_0123456789".into(),
            api: "https://api.github.com".into(),
            base: None,
            content_dir: "content".into(),
            policies_dir: "policies".into(),
        })
        .unwrap();
        assert!(!format!("{gh:?}").contains("SECRET"));
    }
}
