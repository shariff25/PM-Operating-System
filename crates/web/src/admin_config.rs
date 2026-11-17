//! Configuration for the admin policy and content screens (`docs/policy-admin.md`,
//! `docs/content-admin.md`), from environment variables, checked at start-up.
//!
//! Everything is optional. Without GitHub (`PMOS_WEB_GITHUB_REPO` and `PMOS_WEB_GITHUB_TOKEN`)
//! the screens still read and validate; only the proposal routes answer
//! `501 {"code":"not_configured"}`.

use std::path::{Path, PathBuf};

/// The proposals one admin may open per hour, by default.
pub const DEFAULT_PROPOSALS_PER_HOUR: u32 = 10;
/// GitHub's public API.
pub const DEFAULT_GITHUB_API: &str = "https://api.github.com";

/// Where pull requests go, and as whom.
#[derive(Clone)]
pub struct GitHubConfig {
    /// `PMOS_WEB_GITHUB_REPO`: `owner/name`.
    pub repo: String,
    /// `PMOS_WEB_GITHUB_TOKEN`: contents write and pull requests write on that one repository.
    /// Never logged; `Debug` redacts it.
    pub token: String,
    /// `PMOS_WEB_GITHUB_API`, default `https://api.github.com`, no trailing slash. `https`
    /// unless the host is loopback (tests).
    pub api: String,
    /// `PMOS_WEB_GITHUB_BASE`: the branch proposals target. `None`: the repository's default
    /// branch, asked for on each proposal.
    pub base: Option<String>,
    /// `PMOS_WEB_GITHUB_CONTENT_DIR`, default `content`: where the content tree is in the
    /// repository (empty for the repository root).
    pub content_dir: String,
    /// `PMOS_WEB_GITHUB_POLICIES_DIR`, default `policies`: where `pmos policy generate` writes.
    pub policies_dir: String,
}

impl std::fmt::Debug for GitHubConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitHubConfig")
            .field("repo", &self.repo)
            .field("token", &"<redacted>")
            .field("api", &self.api)
            .field("base", &self.base)
            .field("content_dir", &self.content_dir)
            .field("policies_dir", &self.policies_dir)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct AdminConfig {
    /// `PMOS_WEB_POLICIES_ROOT`: the checked-in policy files the server compares against.
    /// Default: `policies` next to the content root, if that directory exists.
    pub policies_root: Option<PathBuf>,
    /// `PMOS_WEB_GATEWAY_CONFIG`: the gateway's model config (`deploy/gateway.yaml`), read for
    /// model names only. Optional.
    pub gateway_config: Option<PathBuf>,
    /// `PMOS_WEB_CONTENT_REVISION`: the commit the deployed content is from, when the content
    /// root has no `.git` to read it from (a container image).
    pub content_revision: Option<String>,
    /// The repository proposals go to. `None`: proposals answer `501 not_configured`.
    pub github: Option<GitHubConfig>,
    /// `PMOS_WEB_PROPOSALS_PER_HOUR`, default 10, 1 to 100: per admin, in memory.
    pub proposals_per_hour: u32,
    /// The repository paths, for showing files even when GitHub isn't configured.
    pub content_dir: String,
    pub policies_dir: String,
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            policies_root: None,
            gateway_config: None,
            content_revision: None,
            github: None,
            proposals_per_hour: DEFAULT_PROPOSALS_PER_HOUR,
            content_dir: "content".into(),
            policies_dir: "policies".into(),
        }
    }
}

impl AdminConfig {
    pub fn from_env(content_root: &Path) -> Result<Self, String> {
        Self::from_lookup(|k| std::env::var(k).ok(), content_root)
    }

    pub fn from_lookup(
        get: impl Fn(&str) -> Option<String>,
        content_root: &Path,
    ) -> Result<Self, String> {
        let set = |k: &str| {
            get(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let policies_root = match set("PMOS_WEB_POLICIES_ROOT") {
            Some(p) => Some(PathBuf::from(p)),
            None => default_policies_root(content_root),
        };
        let proposals_per_hour = match set("PMOS_WEB_PROPOSALS_PER_HOUR") {
            None => DEFAULT_PROPOSALS_PER_HOUR,
            Some(v) => v
                .parse::<u32>()
                .ok()
                .filter(|n| (1..=100).contains(n))
                .ok_or("PMOS_WEB_PROPOSALS_PER_HOUR must be a whole number from 1 to 100")?,
        };
        let content_revision = set("PMOS_WEB_CONTENT_REVISION")
            .map(|r| {
                if is_sha(&r) {
                    Ok(r.to_ascii_lowercase())
                } else {
                    Err("PMOS_WEB_CONTENT_REVISION must be a commit sha (40 hex characters)")
                }
            })
            .transpose()?;
        let repo_dir = |k: &str, default: &str| -> Result<String, String> {
            let v = get(k)
                .map(|v| v.trim().trim_matches('/').to_string())
                .unwrap_or_else(|| default.to_string());
            if v.is_empty() || crate::proposals::safe_repo_path(&v) {
                Ok(v)
            } else {
                Err(format!(
                    "{k} must be a relative folder in the repository, such as `{default}`"
                ))
            }
        };
        let content_dir = repo_dir("PMOS_WEB_GITHUB_CONTENT_DIR", "content")?;
        let policies_dir = repo_dir("PMOS_WEB_GITHUB_POLICIES_DIR", "policies")?;
        if policies_dir.is_empty() {
            return Err("PMOS_WEB_GITHUB_POLICIES_DIR can't be the repository root".into());
        }
        let repo = set("PMOS_WEB_GITHUB_REPO");
        let token = set("PMOS_WEB_GITHUB_TOKEN");
        let github = match (repo, token) {
            (None, None) => None,
            (Some(_), None) | (None, Some(_)) => {
                return Err(
                    "PMOS_WEB_GITHUB_REPO and PMOS_WEB_GITHUB_TOKEN must be set together".into(),
                )
            }
            (Some(repo), Some(token)) => {
                if !valid_repo(&repo) {
                    return Err(format!(
                        "PMOS_WEB_GITHUB_REPO must be `owner/name`, not `{repo}`"
                    ));
                }
                if token.len() < 20 || token.chars().any(|c| c.is_whitespace() || c.is_control()) {
                    // Never echo the value.
                    return Err("PMOS_WEB_GITHUB_TOKEN doesn't look like a GitHub token".into());
                }
                let api = api_url(
                    &set("PMOS_WEB_GITHUB_API").unwrap_or_else(|| DEFAULT_GITHUB_API.into()),
                )?;
                let base = set("PMOS_WEB_GITHUB_BASE")
                    .map(|b| {
                        if valid_branch(&b) {
                            Ok(b)
                        } else {
                            Err(format!("PMOS_WEB_GITHUB_BASE: `{b}` is not a branch name"))
                        }
                    })
                    .transpose()?;
                Some(GitHubConfig {
                    repo,
                    token,
                    api,
                    base,
                    content_dir: content_dir.clone(),
                    policies_dir: policies_dir.clone(),
                })
            }
        };
        Ok(Self {
            policies_root,
            gateway_config: set("PMOS_WEB_GATEWAY_CONFIG").map(PathBuf::from),
            content_revision,
            github,
            proposals_per_hour,
            content_dir,
            policies_dir,
        })
    }
}

/// `policies/` beside the content root, when there is one.
pub fn default_policies_root(content_root: &Path) -> Option<PathBuf> {
    let parent = content_root
        .canonicalize()
        .ok()?
        .parent()
        .map(Path::to_path_buf)?;
    let dir = parent.join("policies");
    dir.is_dir().then_some(dir)
}

pub fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `owner/name`: an owner of letters, digits and single hyphens (at most 39), and a name of
/// letters, digits, `.`, `_` and `-` (at most 100, not `.` or `..`).
pub fn valid_repo(full: &str) -> bool {
    let Some((owner, name)) = full.split_once('/') else {
        return false;
    };
    let owner_ok = !owner.is_empty()
        && owner.len() <= 39
        && !owner.starts_with('-')
        && !owner.ends_with('-')
        && !owner.contains("--")
        && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    owner_ok
        && !name.is_empty()
        && name.len() <= 100
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// A branch name PM OS is willing to use: letters, digits, `-_./`, no `..`, no leading or
/// trailing `/`, at most 200.
pub fn valid_branch(b: &str) -> bool {
    !b.is_empty()
        && b.len() <= 200
        && b.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
        && !b.contains("..")
        && !b.contains("//")
        && !b.starts_with('/')
        && !b.ends_with('/')
        && !b.ends_with(".lock")
}

/// An API base: `https`, or `http` to a loopback host only (tests), with no query.
fn api_url(value: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(value).map_err(|e| format!("PMOS_WEB_GITHUB_API: {e}"))?;
    let host = url.host_str().unwrap_or_default();
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    let ok = match url.scheme() {
        "https" => !host.is_empty(),
        "http" => loopback,
        _ => false,
    };
    if !ok || url.query().is_some() || !url.username().is_empty() || url.password().is_some() {
        return Err(
            "PMOS_WEB_GITHUB_API must be an https URL (http only to a loopback host)".into(),
        );
    }
    Ok(value.trim_end_matches('/').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| {
            pairs
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
        }
    }

    const TOKEN: &str = "github_pat_test_0123456789abcdefghij";

    #[test]
    fn everything_is_optional() {
        let c = AdminConfig::from_lookup(lookup(&[]), Path::new("/nonexistent")).unwrap();
        assert!(c.github.is_none() && c.policies_root.is_none());
        assert_eq!(c.proposals_per_hour, DEFAULT_PROPOSALS_PER_HOUR);
        assert_eq!(
            (c.content_dir.as_str(), c.policies_dir.as_str()),
            ("content", "policies")
        );
    }

    #[test]
    fn github_needs_both_and_redacts_the_token() {
        let root = Path::new("/nonexistent");
        assert!(
            AdminConfig::from_lookup(lookup(&[("PMOS_WEB_GITHUB_REPO", "acme/pmos")]), root)
                .is_err()
        );
        let err = AdminConfig::from_lookup(lookup(&[("PMOS_WEB_GITHUB_TOKEN", TOKEN)]), root)
            .unwrap_err();
        assert!(!err.contains(TOKEN));
        let c = AdminConfig::from_lookup(
            lookup(&[
                ("PMOS_WEB_GITHUB_REPO", "acme/pmos"),
                ("PMOS_WEB_GITHUB_TOKEN", TOKEN),
                ("PMOS_WEB_GITHUB_BASE", "main"),
            ]),
            root,
        )
        .unwrap();
        let g = c.github.as_ref().unwrap();
        assert_eq!(g.api, DEFAULT_GITHUB_API);
        assert_eq!(g.base.as_deref(), Some("main"));
        let debug = format!("{c:?}");
        assert!(!debug.contains(TOKEN) && debug.contains("<redacted>"));
        let err = AdminConfig::from_lookup(
            lookup(&[
                ("PMOS_WEB_GITHUB_REPO", "acme/pmos"),
                ("PMOS_WEB_GITHUB_TOKEN", "short"),
            ]),
            root,
        )
        .unwrap_err();
        assert!(!err.contains("short"));
    }

    #[test]
    fn the_api_is_https_unless_loopback() {
        let root = Path::new("/nonexistent");
        let with = |api: &str| {
            AdminConfig::from_lookup(
                lookup(&[
                    ("PMOS_WEB_GITHUB_REPO", "acme/pmos"),
                    ("PMOS_WEB_GITHUB_TOKEN", TOKEN),
                    ("PMOS_WEB_GITHUB_API", api),
                ]),
                root,
            )
        };
        assert!(with("http://127.0.0.1:9999/").is_ok());
        assert!(with("http://localhost:9999").is_ok());
        assert!(with("https://github.example.com/api/v3").is_ok());
        assert!(with("http://github.example.com").is_err());
        assert!(with("ftp://127.0.0.1").is_err());
        assert!(with("https://user:pw@github.example.com").is_err());
    }

    #[test]
    fn names_are_checked() {
        assert!(valid_repo("acme/pm-os.content"));
        assert!(!valid_repo("acme"));
        assert!(!valid_repo("acme/.."));
        assert!(!valid_repo("-acme/x"));
        assert!(!valid_repo("acme/a b"));
        assert!(valid_branch("main") && valid_branch("release/2026-11"));
        for bad in ["", "/main", "main/", "a..b", "a b", "x.lock", "a//b"] {
            assert!(!valid_branch(bad), "{bad}");
        }
        let root = Path::new("/nonexistent");
        assert!(
            AdminConfig::from_lookup(lookup(&[("PMOS_WEB_GITHUB_CONTENT_DIR", "../x")]), root)
                .is_err()
        );
        assert!(
            AdminConfig::from_lookup(lookup(&[("PMOS_WEB_PROPOSALS_PER_HOUR", "0")]), root)
                .is_err()
        );
    }
}
