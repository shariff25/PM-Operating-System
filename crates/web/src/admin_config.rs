//! Configuration for the admin policy and content screens (`docs/policy-admin.md`,
//! `docs/content-admin.md`), from environment variables, checked at start-up.
//!
//! `pmos-web` holds **no GitHub token**. It reads the deployed content and policies from disk,
//! validates a proposed change interactively, and sends the change set to the tool server,
//! which checks it again and opens the pull request with the acting admin's own linked GitHub
//! account (`docs/policy-admin.md`, How a proposal is opened). The settings that configured a
//! token here are refused at start-up, so a forgotten one can't linger.

use std::path::{Path, PathBuf};

use pmos_policy::admin::changeset::Layout;

/// Settings that existed when `pmos-web` opened pull requests itself, and where each went.
/// Any of them set refuses start-up.
pub const REMOVED: [(&str, &str); 7] = [
    (
        "PMOS_WEB_GITHUB_TOKEN",
        "pmos-web holds no GitHub token: the tool server opens proposals with the admin's own linked GitHub account; delete the token",
    ),
    (
        "PMOS_WEB_GITHUB_REPO",
        "the tool server's PMOS_CONTENT_REPO is the repository proposals go to",
    ),
    (
        "PMOS_WEB_GITHUB_API",
        "the tool server's PMOS_GITHUB_URL is the GitHub API it calls",
    ),
    (
        "PMOS_WEB_GITHUB_BASE",
        "the tool server's PMOS_CONTENT_BRANCH is the branch proposals target",
    ),
    (
        "PMOS_WEB_PROPOSALS_PER_HOUR",
        "the tool server's PMOS_PROPOSALS_PER_HOUR limits proposals now",
    ),
    (
        "PMOS_WEB_GITHUB_CONTENT_DIR",
        "renamed PMOS_WEB_CONTENT_DIR",
    ),
    (
        "PMOS_WEB_GITHUB_POLICIES_DIR",
        "renamed PMOS_WEB_POLICIES_DIR",
    ),
];

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
    /// `PMOS_WEB_CONTENT_DIR` (default `content`) and `PMOS_WEB_POLICIES_DIR` (default
    /// `policies`): where those trees are in the repository. Neither may be the repository
    /// root, nor inside the other ([`Layout::new`]); they must match the tool server's
    /// `PMOS_CONTENT_DIR` and `PMOS_POLICIES_DIR`, which it checks again.
    pub layout: Layout,
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            policies_root: None,
            gateway_config: None,
            content_revision: None,
            layout: Layout::new("content", "policies").expect("the default layout is valid"),
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
        for (name, why) in REMOVED {
            // The value is never echoed.
            if set(name).is_some() {
                return Err(format!("{name} is no longer used: {why}."));
            }
        }
        let policies_root = match set("PMOS_WEB_POLICIES_ROOT") {
            Some(p) => Some(PathBuf::from(p)),
            None => default_policies_root(content_root),
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
        // Set but empty is the repository root, which is refused: not the default.
        let dir = |k: &str, default: &str| get(k).unwrap_or_else(|| default.to_string());
        let layout = Layout::new(
            &dir("PMOS_WEB_CONTENT_DIR", "content"),
            &dir("PMOS_WEB_POLICIES_DIR", "policies"),
        )
        .map_err(|why| format!("PMOS_WEB_CONTENT_DIR, PMOS_WEB_POLICIES_DIR: {why}"))?;
        Ok(Self {
            policies_root,
            gateway_config: set("PMOS_WEB_GATEWAY_CONFIG").map(PathBuf::from),
            content_revision,
            layout,
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
        assert!(c.policies_root.is_none() && c.gateway_config.is_none());
        assert_eq!(
            (c.layout.content_dir(), c.layout.policies_dir()),
            ("content", "policies")
        );
    }

    #[test]
    fn the_github_token_and_its_settings_are_refused() {
        let root = Path::new("/nonexistent");
        for (name, _) in REMOVED {
            let value = if name == "PMOS_WEB_GITHUB_TOKEN" {
                TOKEN
            } else {
                "x"
            };
            let err = AdminConfig::from_lookup(lookup(&[(name, value)]), root).unwrap_err();
            assert!(err.contains(name), "{err}");
            assert!(!err.contains(TOKEN), "the token is never echoed");
        }
    }

    #[test]
    fn the_repository_root_is_never_the_content_or_policies_folder() {
        let root = Path::new("/nonexistent");
        for (k, v) in [
            ("PMOS_WEB_CONTENT_DIR", ""),
            ("PMOS_WEB_CONTENT_DIR", "/"),
            ("PMOS_WEB_CONTENT_DIR", "."),
            ("PMOS_WEB_POLICIES_DIR", ""),
            ("PMOS_WEB_CONTENT_DIR", "../x"),
            ("PMOS_WEB_POLICIES_DIR", "content/policies"),
        ] {
            assert!(
                AdminConfig::from_lookup(lookup(&[(k, v)]), root).is_err(),
                "{k}={v:?}"
            );
        }
        let c = AdminConfig::from_lookup(
            lookup(&[
                ("PMOS_WEB_CONTENT_DIR", "pmos/content"),
                ("PMOS_WEB_POLICIES_DIR", "pmos/policies"),
            ]),
            root,
        )
        .unwrap();
        assert_eq!(c.layout.content_dir(), "pmos/content");
        assert!(
            AdminConfig::from_lookup(lookup(&[("PMOS_WEB_CONTENT_REVISION", "abc")]), root)
                .is_err()
        );
    }
}
