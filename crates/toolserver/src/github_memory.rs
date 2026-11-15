//! Personal memory in each PM's own private repo in the GitHub org, with `PMOS_MEMORY=github`
//! (`docs/onboarding-spec.md` §2). Every read and write acts as the PM, with their GitHub
//! token.
//!
//! One file per note, `notes/<key>.md`, on the repo's default branch:
//!
//! ```markdown
//! ---
//! updated: 2026-11-14T10:00:00Z
//! source: "pm-ana, task 0d3f…"
//! ---
//! <note text>
//! ```
//!
//! - **Reads** list the tree and fetch only the blobs whose sha changed since the last read
//!   (a per-repo cache). An unchanged tree sha means no blob is fetched at all.
//! - **Writes** commit only that file, with the message `memory: <key> (<source>)`.
//! - **Re-verification:** a record more than [`REVERIFY_AFTER_MINUTES`] old is checked again.
//!   A repo that became public, was archived, left the org or lost push access refuses reads
//!   and writes with a tool error naming the reason.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Duration, SecondsFormat, Utc};

use crate::github::{Change, GitHub, GitHubError, RepoInfo};
use crate::memory::{valid_key, MemoryError, MemoryResult, MemoryStore, Note};
use crate::memory_repo::{MemoryRepo, MemoryRepoStore};
use crate::oauth::{TokenError, TokenSource};
use crate::vault::{Provider, Secret};

/// A memory-repo record older than this is checked again before use.
pub const REVERIFY_AFTER_MINUTES: i64 = 10;

/// The tool error when a PM has no memory repo yet.
pub const NO_REPO: &str = "Set up your memory repo first: DM PM OS \"setup\".";

/// Why a repo can't hold a PM's memory. [`RepoProblem::code`] is the API's `code`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoProblem {
    NotInOrg,
    NotPrivate,
    Archived,
    NoPush,
    NotFound,
}

impl RepoProblem {
    pub fn code(self) -> &'static str {
        match self {
            Self::NotInOrg => "not_in_org",
            Self::NotPrivate => "not_private",
            Self::Archived => "archived",
            Self::NoPush => "no_push",
            Self::NotFound => "not_found",
        }
    }

    pub fn message(self, repo: &str, org: &str) -> String {
        match self {
            Self::NotInOrg => format!("{repo} is not in the {org} GitHub org"),
            Self::NotPrivate => format!("{repo} is not private"),
            Self::Archived => format!("{repo} is archived"),
            Self::NoPush => format!("your GitHub account can't push to {repo}"),
            Self::NotFound => format!("your GitHub account can't see a repo {repo}"),
        }
    }
}

/// Can `info` hold a PM's memory: in `org`, private, not archived, and pushable?
pub fn check_repo(info: &RepoInfo, org: &str) -> Result<(), RepoProblem> {
    if !info.owner.eq_ignore_ascii_case(org) {
        Err(RepoProblem::NotInOrg)
    } else if !info.private {
        Err(RepoProblem::NotPrivate)
    } else if info.archived {
        Err(RepoProblem::Archived)
    } else if !info.push {
        Err(RepoProblem::NoPush)
    } else {
        Ok(())
    }
}

/// What was read from one repo last time.
#[derive(Default, Clone)]
struct RepoCache {
    default_branch: Option<String>,
    tree_sha: Option<String>,
    /// By path: the blob sha and the note it holds.
    notes: HashMap<String, (String, Note)>,
}

pub struct GitHubMemory {
    github: Arc<GitHub>,
    tokens: Arc<dyn TokenSource>,
    repos: Arc<dyn MemoryRepoStore>,
    org: String,
    cache: Mutex<HashMap<String, RepoCache>>,
}

impl GitHubMemory {
    pub fn new(
        github: Arc<GitHub>,
        tokens: Arc<dyn TokenSource>,
        repos: Arc<dyn MemoryRepoStore>,
        org: impl Into<String>,
    ) -> Self {
        Self {
            github,
            tokens,
            repos,
            org: org.into(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn cached(&self, repo: &str) -> RepoCache {
        self.cache
            .lock()
            .expect("memory cache lock")
            .get(&repo.to_ascii_lowercase())
            .cloned()
            .unwrap_or_default()
    }

    fn store_cache(&self, repo: &str, cache: RepoCache) {
        self.cache
            .lock()
            .expect("memory cache lock")
            .insert(repo.to_ascii_lowercase(), cache);
    }

    fn refused(&self, repo: &str, problem: RepoProblem) -> MemoryError {
        MemoryError::Refused(format!(
            "Your memory repo can't be used: {}. Fix that on GitHub, or DM PM OS \"setup\" to set another one.",
            problem.message(repo, &self.org)
        ))
    }

    fn github_error(&self, repo: &str, e: GitHubError) -> MemoryError {
        match e {
            GitHubError::NotFound(_) => self.refused(repo, RepoProblem::NotFound),
            GitHubError::Reconnect => MemoryError::Refused(
                "GitHub rejected your sign-in. Reconnect GitHub: DM PM OS \"setup\".".into(),
            ),
            GitHubError::Forbidden(m) => {
                MemoryError::Refused(format!("GitHub refused the memory call: {m}"))
            }
            e => MemoryError::Unavailable(format!("Memory is unavailable: {e}")),
        }
    }

    /// The PM's token, memory repo and its default branch, checking the repo again if its
    /// record is stale.
    async fn open(&self, user: &str, now: DateTime<Utc>) -> MemoryResult<(Secret, String, String)> {
        let record = self
            .repos
            .get(user)
            .await?
            .ok_or_else(|| MemoryError::Refused(NO_REPO.into()))?;
        let token = match self.tokens.token(user, Provider::Github).await {
            Ok(t) => t.token,
            Err(e @ (TokenError::NotConnected(_) | TokenError::Reconnect(_))) => {
                return Err(MemoryError::Refused(format!(
                    "Memory needs GitHub: {e}. DM PM OS \"setup\"."
                )))
            }
            Err(TokenError::Store(e)) => return Err(e.into()),
            Err(e) => return Err(MemoryError::Unavailable(e.to_string())),
        };
        let repo = record.repo.clone();
        let stale = now - record.verified_at > Duration::minutes(REVERIFY_AFTER_MINUTES);
        let mut cache = self.cached(&repo);
        if stale || cache.default_branch.is_none() {
            let info = self
                .github
                .repo(&token, &repo)
                .await
                .map_err(|e| self.github_error(&repo, e))?;
            if let Err(problem) = check_repo(&info, &self.org) {
                tracing::warn!(user, repo = %repo, problem = problem.code(), "memory repo refused");
                return Err(self.refused(&repo, problem));
            }
            if stale {
                self.repos
                    .put(&MemoryRepo {
                        verified_at: now,
                        ..record
                    })
                    .await?;
            }
            cache.default_branch = Some(info.default_branch.clone());
            self.store_cache(&repo, cache.clone());
        }
        let branch = cache.default_branch.unwrap_or_else(|| "main".into());
        Ok((token, repo, branch))
    }
}

fn note_path(key: &str) -> String {
    format!("notes/{key}.md")
}

/// The key of a note file, `notes/<key>.md`.
fn note_key(path: &str) -> Option<&str> {
    path.strip_prefix("notes/")?
        .strip_suffix(".md")
        .filter(|k| valid_key(k))
}

/// A note file's text. `source` is quoted as a JSON string, which is also valid YAML.
pub fn render_note(text: &str, source: &str, now: DateTime<Utc>) -> String {
    format!(
        "---\nupdated: {}\nsource: {}\n---\n{text}",
        now.to_rfc3339_opts(SecondsFormat::Secs, true),
        serde_json::to_string(source).expect("a string serializes"),
    )
}

/// A note from its file. A file someone edited by hand without front matter is read whole,
/// with no update time.
pub fn parse_note(key: &str, file: &str) -> Note {
    let (updated, text) = match pmos_contract::frontmatter::split(file) {
        Ok(doc) => {
            let updated = doc
                .yaml
                .lines()
                .find_map(|l| l.trim().strip_prefix("updated:"))
                .map(|v| v.trim().trim_matches(['"', '\'']))
                .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
                .map(|d| d.with_timezone(&Utc));
            (updated, doc.body)
        }
        Err(_) => (None, file),
    };
    Note {
        key: key.to_string(),
        text: text.to_string(),
        updated_at: updated.unwrap_or(DateTime::UNIX_EPOCH),
    }
}

#[async_trait]
impl MemoryStore for GitHubMemory {
    async fn read(&self, user: &str) -> MemoryResult<Vec<Note>> {
        let (token, repo, branch) = self.open(user, Utc::now()).await?;
        let tree = match self.github.tree(&token, &repo, &branch).await {
            Ok(tree) => tree,
            // A new repo has no commits, or no default branch yet: no notes.
            Err(GitHubError::Empty | GitHubError::NotFound(_)) => return Ok(Vec::new()),
            Err(e) => return Err(self.github_error(&repo, e)),
        };
        if tree.truncated {
            tracing::warn!(repo = %repo, "memory repo tree is truncated; some notes are missing");
        }
        let mut cache = self.cached(&repo);
        if cache.tree_sha.as_deref() != Some(tree.sha.as_str()) {
            let mut notes = HashMap::new();
            for entry in tree.entries.iter().filter(|e| e.kind == "blob") {
                let Some(key) = note_key(&entry.path) else {
                    continue;
                };
                let note = match cache.notes.get(&entry.path) {
                    Some((sha, note)) if *sha == entry.sha => note.clone(),
                    _ => {
                        let file = self
                            .github
                            .blob(&token, &repo, &entry.sha)
                            .await
                            .map_err(|e| self.github_error(&repo, e))?;
                        parse_note(key, &file)
                    }
                };
                notes.insert(entry.path.clone(), (entry.sha.clone(), note));
            }
            cache.notes = notes;
            cache.tree_sha = Some(tree.sha);
            self.store_cache(&repo, cache.clone());
        }
        let mut notes: Vec<Note> = cache.notes.into_values().map(|(_, n)| n).collect();
        notes.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(notes)
    }

    async fn update(
        &self,
        user: &str,
        key: &str,
        text: &str,
        source: &str,
        now: DateTime<Utc>,
    ) -> MemoryResult<Note> {
        if !valid_key(key) {
            return Err(MemoryError::Refused(format!("`{key}` is not a memory key")));
        }
        let (token, repo, branch) = self.open(user, now).await?;
        let path = note_path(key);
        let file = render_note(text, source, now);
        let message = format!("memory: {key} ({source})");
        let written = match self
            .github
            .commit_files(
                &token,
                &repo,
                &branch,
                &message,
                &[Change::write(&path, &file)],
            )
            .await
        {
            // An empty repo has nothing for the git data API to build on.
            Err(GitHubError::Empty | GitHubError::NotFound(_)) => {
                self.github
                    .create_first_file(&token, &repo, &path, &message, &file)
                    .await
            }
            other => other,
        };
        let commit = written.map_err(|e| self.github_error(&repo, e))?;
        tracing::info!(user, repo = %repo, key, commit = %commit, "memory note written");
        Ok(Note {
            key: key.to_string(),
            text: text.to_string(),
            updated_at: now,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(owner: &str, private: bool, archived: bool, push: bool) -> RepoInfo {
        RepoInfo {
            full_name: format!("{owner}/m"),
            owner: owner.into(),
            private,
            archived,
            default_branch: "main".into(),
            push,
        }
    }

    #[test]
    fn repo_checks_in_order() {
        assert_eq!(
            check_repo(&info("Example-Org", true, false, true), "example-org"),
            Ok(())
        );
        assert_eq!(
            check_repo(&info("other", true, false, true), "example-org"),
            Err(RepoProblem::NotInOrg)
        );
        assert_eq!(
            check_repo(&info("example-org", false, false, true), "example-org"),
            Err(RepoProblem::NotPrivate)
        );
        assert_eq!(
            check_repo(&info("example-org", true, true, true), "example-org"),
            Err(RepoProblem::Archived)
        );
        assert_eq!(
            check_repo(&info("example-org", true, false, false), "example-org"),
            Err(RepoProblem::NoPush)
        );
    }

    #[test]
    fn notes_round_trip_through_their_files() {
        let now: DateTime<Utc> = "2026-11-14T10:00:00Z".parse().unwrap();
        let file = render_note(
            "Acme Cloud, Acme Vault\n---\nmore",
            "pm-ana, task \"7\"",
            now,
        );
        assert!(file.starts_with(
            "---\nupdated: 2026-11-14T10:00:00Z\nsource: \"pm-ana, task \\\"7\\\"\"\n---\n"
        ));
        let note = parse_note("areas", &file);
        assert_eq!(note.text, "Acme Cloud, Acme Vault\n---\nmore");
        assert_eq!(note.updated_at, now);
        let by_hand = parse_note("x", "just text");
        assert_eq!(by_hand.text, "just text");
        assert_eq!(by_hand.updated_at, DateTime::UNIX_EPOCH);
        assert_eq!(note_key("notes/areas.md"), Some("areas"));
        assert_eq!(note_key("notes/sub/areas.md"), None);
        assert_eq!(note_key("README.md"), None);
    }
}
