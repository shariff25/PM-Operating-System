//! What the policy and content admin routes share: role checks, the GitHub set-up, running
//! the file-system work off the async threads, and opening and listing proposals.
//!
//! **Opening a proposal**, after the route has validated it:
//! 1. The base: `PMOS_WEB_GITHUB_BASE`, or the repository's default branch, at its head now.
//! 2. **Stale base:** each file the pull request writes must be on that commit exactly as the
//!    server had it when it computed the change (by git blob id), or the proposal is refused
//!    `409 stale_base` with the paths. Skipped (and said so) when GitHub truncates the tree.
//! 3. The rate limit (per admin, per hour, in memory).
//! 4. **Audit first:** the tool server records `admin.<kind>_proposal` `started` with the
//!    paths. If it can't, nothing is written to GitHub.
//! 5. One commit on the base, a new branch `pmos/proposal/<kind>/<handle>-<time>-<random>`
//!    (a new ref only: the default branch is never written), the pull request, and the labels
//!    `pmos-proposal` and `pmos-proposal:<kind>`. Never merged here.
//! 6. The tool server records `opened` with the pull request, or `failed`.
//!
//! The log has the handle, the kind, how many files and the pull request number; never the
//! token or a file's text.

use std::collections::BTreeMap;

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use pmos_contract::permissions::Permission;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{now, ApiError, ApiResult, AppState, Signed};
use crate::admin_config::AdminConfig;
use crate::github::{FileWrite, GitHub, GitHubError};
use crate::policy_admin::Evaluation;
use crate::proposals::{self, Change, Issue, Kind, RateLimiter};
use crate::upstream::UpstreamError;

/// The largest request body the admin write routes take: 50 files of 256 KiB, as JSON.
pub const MAX_REQUEST: usize = 16 * 1024 * 1024;

pub struct Proposals {
    pub config: AdminConfig,
    pub github: Option<GitHub>,
    pub limiter: RateLimiter,
}

impl Proposals {
    pub fn new(config: AdminConfig) -> Result<Self, String> {
        let github = config.github.as_ref().map(GitHub::new).transpose()?;
        Ok(Self {
            limiter: RateLimiter::new(config.proposals_per_hour),
            github,
            config,
        })
    }
}

pub(super) fn forbidden() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "forbidden",
        "Your role in PM OS doesn't allow this.",
    )
}

/// The viewer has `p`, else `403 forbidden`.
pub(super) fn need(signed: &Signed, p: Permission) -> Result<(), ApiError> {
    if signed.member.allows(p) {
        Ok(())
    } else {
        Err(forbidden())
    }
}

pub(super) fn not_configured() -> ApiError {
    ApiError::new(
        StatusCode::NOT_IMPLEMENTED,
        "not_configured",
        "Proposals need a GitHub repository and token (PMOS_WEB_GITHUB_REPO, PMOS_WEB_GITHUB_TOKEN). Reading and validating still work.",
    )
}

/// The GitHub client, or `501 not_configured`.
pub(super) fn github(state: &AppState) -> Result<&GitHub, ApiError> {
    state.0.proposals.github.as_ref().ok_or_else(not_configured)
}

/// Run file-system work (lint, generation, a scratch copy) off the async threads.
pub(super) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f).await.map_err(|_| {
        tracing::error!("admin validation task failed");
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "PM OS couldn't check this. Try again.",
        )
    })
}

pub(super) fn read_log(signed: &Signed, route: &str) {
    tracing::info!(user = %signed.member.handle, route, "admin read");
}

/// A JSON answer with a status and extra fields beside `code` and `error`.
pub(super) fn coded(status: StatusCode, code: &str, error: &str, extra: Value) -> Response {
    let mut body = json!({ "code": code, "error": error });
    if let (Some(b), Value::Object(extra)) = (body.as_object_mut(), extra) {
        b.extend(extra);
    }
    (status, Json(body)).into_response()
}

/// The errors and warnings of `issues`, apart.
pub(super) fn split(issues: &[Issue]) -> (Vec<&Issue>, Vec<&Issue>) {
    issues.iter().partition(|i| i.severity == "error")
}

/// A validation as the API serves it.
pub(super) fn evaluation_json(e: &Evaluation) -> Value {
    let (errors, warnings) = split(&e.issues);
    let count = |d: &str| {
        e.allowance_changes
            .iter()
            .filter(|c| c.direction == d)
            .count()
    };
    json!({
        "valid": e.valid,
        "errors": errors,
        "warnings": warnings,
        "content_changes": e.content_diffs,
        "policy_changes": e.policy_diffs,
        "allowance_changes": e.allowance_changes,
        "widening": e.widening,
        "summary": {"wider": count("wider"), "narrower": count("narrower"), "changed": count("changed")},
    })
}

/// `422 invalid` with the validation's errors and warnings.
pub(super) fn invalid(e: &Evaluation) -> Response {
    let (errors, warnings) = split(&e.issues);
    coded(
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid",
        "This change doesn't pass validation; nothing was proposed.",
        json!({"errors": errors, "warnings": warnings}),
    )
}

/// A path or file refusal as an answer: `413 too_large` for size, else `400` with its code.
pub(super) fn refused(r: proposals::Refusal, path: &str) -> ApiError {
    let status = if r.code == "too_large" {
        StatusCode::PAYLOAD_TOO_LARGE
    } else {
        StatusCode::BAD_REQUEST
    };
    let error = if path.is_empty() {
        r.message
    } else {
        format!("{path}: {}", r.message)
    };
    ApiError::new(status, r.code, error)
}

/// The title and reason every proposal carries.
#[derive(Deserialize)]
pub(super) struct Meta {
    pub title: String,
    pub reason: String,
}

pub(super) fn check_meta(m: &Meta) -> Result<(String, String), ApiError> {
    let title = proposals::clean_line(&m.title, proposals::MAX_TITLE);
    let reason = proposals::clean_text(&m.reason, proposals::MAX_REASON);
    if title.is_empty() {
        return Err(ApiError::bad_request("A proposal needs a title."));
    }
    if reason.is_empty() {
        return Err(ApiError::bad_request(
            "A proposal needs a reason: reviewers read it in the pull request.",
        ));
    }
    Ok((title, reason))
}

fn github_error(e: GitHubError) -> ApiError {
    tracing::warn!(error = %e, "GitHub call failed");
    match e {
        GitHubError::Unauthorized | GitHubError::Forbidden(_) => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "github_refused",
            "GitHub refused PM OS's token for this repository. Tell a platform owner: it needs contents and pull requests write.",
        ),
        GitHubError::RateLimited(s) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "github_rate_limited",
            format!("GitHub's rate limit was reached. Try again in {s} seconds."),
        ),
        GitHubError::NotFound(_) => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "github_unavailable",
            "GitHub couldn't find the repository or its base branch. Tell a platform owner.",
        ),
        GitHubError::Invalid(_) | GitHubError::Unavailable(_) => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "github_unavailable",
            "GitHub didn't take this proposal. Try again in a minute.",
        ),
    }
}

/// Record a proposal stage at the tool server, for `signed`.
async fn record(
    state: &AppState,
    signed: &Signed,
    kind: Kind,
    stage: &str,
    paths: &[String],
    base: &str,
    pr: Option<u64>,
) -> Result<(), UpstreamError> {
    let repo = state
        .0
        .proposals
        .github
        .as_ref()
        .map(|g| g.repo().to_string())
        .unwrap_or_default();
    signed
        .toolserver(state)
        .admin_record(
            &signed.member.handle,
            "proposals",
            &json!({
                "kind": kind.as_str(),
                "stage": stage,
                "paths": paths,
                "repo": repo,
                "base": base,
                "pr": pr,
            }),
        )
        .await
        .map(|_| ())
}

/// The pull request's body: who proposed it (their PM OS handle, in code so GitHub notifies
/// nobody), the reason (fenced, so it can't format or mention), the base, what the sandboxes
/// gain and lose, and the files.
/// The base a proposal is made on: the branch, its head commit, and the server's own
/// content revision when known.
struct Base<'a> {
    branch: &'a str,
    commit: &'a str,
    revision: Option<&'a str>,
}

fn body(
    kind: Kind,
    handle: &str,
    reason: &str,
    base: &Base,
    e: &Evaluation,
    files: &[String],
) -> String {
    let (base_branch, base_commit, revision) = (base.branch, base.commit, base.revision);
    let mut b = format!(
        "<!-- pmos-proposal kind={} by={handle} base={base_commit} -->\n\
         **Proposed in PM OS by `{handle}`** (their PM OS handle; this pull request's GitHub author is the PM OS token).\n\n\
         ### Reason\n\n```text\n{}\n```\n\n\
         ### Base\n\n`{base_branch}` at `{base_commit}`. The PM OS server computed this change from its deployed content{}.\n\n",
        kind.as_str(),
        reason.replace("```", "'''"),
        match revision {
            Some(r) => format!(" at `{r}`"),
            None => " (revision unknown)".into(),
        },
    );
    b.push_str("### What changes in the sandboxes\n\n");
    if e.allowance_changes.is_empty() {
        b.push_str("No sandbox policy allowance changes.\n");
    } else {
        if e.widening {
            b.push_str("**This widens what a sandbox may do.** A network change needs a second platform owner's review (POL-2, `policies/README.md`).\n\n");
        }
        for c in &e.allowance_changes {
            let mark = match c.direction {
                "wider" => "WIDER",
                "narrower" => "narrower",
                _ => "CHANGED",
            };
            b.push_str(&format!(
                "- **{mark}**: {}\n",
                proposals::clean_line(&c.description, 300).replace('`', "'")
            ));
        }
    }
    b.push_str("\n### Files\n\n");
    for f in files {
        b.push_str(&format!("- `{f}`\n"));
    }
    b.push_str(
        "\nGenerated policy files are regenerated by PM OS with the same code as `pmos policy generate`; CI's `pmos policy check` runs on this pull request as on any other.\n\n\
         PM OS never merges: a person reviews and merges this in GitHub.\n",
    );
    proposals::clean_text(&b, proposals::MAX_BODY)
}

/// Open a pull request for a validated change (see the module docs). `content` is what the
/// admin changed (content-relative); the regenerated policy files come from `e`.
pub(super) async fn open(
    state: &AppState,
    signed: &Signed,
    kind: Kind,
    title: &str,
    reason: &str,
    content: &[Change],
    e: &Evaluation,
) -> ApiResult {
    let gh = github(state)?;
    let cfg = &state.0.proposals.config;
    let handle = signed.member.handle.as_str();
    let mut files: Vec<FileWrite> = content
        .iter()
        .map(|c| FileWrite {
            path: proposals::repo_path(&cfg.content_dir, &c.rel),
            text: c.text.clone(),
        })
        .collect();
    files.extend(e.policy_writes.iter().map(|(rel, text)| FileWrite {
        path: proposals::repo_path(&cfg.policies_dir, rel),
        text: text.clone(),
    }));
    let paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();

    let base_branch = match &cfg.github.as_ref().and_then(|g| g.base.clone()) {
        Some(b) => b.clone(),
        None => gh.default_branch().await.map_err(github_error)?,
    };
    let head = gh.head(&base_branch).await.map_err(github_error)?;
    let blobs = gh.blobs(&head.tree).await.map_err(github_error)?;
    let stale_check = if blobs.truncated {
        tracing::warn!(user = %handle, "GitHub truncated the base tree: stale-base check skipped");
        "skipped"
    } else {
        let stale: Vec<&String> = e
            .base_texts
            .iter()
            .filter(|(path, old)| {
                blobs.by_path.get(*path).map(String::as_str)
                    != old
                        .as_ref()
                        .map(|t| proposals::git_blob_sha(t.as_bytes()))
                        .as_deref()
            })
            .map(|(p, _)| p)
            .collect();
        if !stale.is_empty() {
            tracing::info!(user = %handle, kind = kind.as_str(), files = stale.len(), "proposal refused: stale base");
            return Ok(coded(
                StatusCode::CONFLICT,
                "stale_base",
                "These files on the base branch differ from the server's deployed copy this change was made from. Wait for the deployment to catch up, or make the change in GitHub.",
                json!({"paths": stale, "base": {"branch": base_branch, "commit": head.commit}}),
            ));
        }
        "passed"
    };

    if let Err(wait) = state.0.proposals.limiter.take(handle, now(state)) {
        tracing::info!(user = %handle, "proposal refused: rate limit");
        let mut res = ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            format!(
                "That's {} proposals this hour. Try again in {} minutes.",
                cfg.proposals_per_hour,
                wait.div_ceil(60)
            ),
        )
        .into_response();
        if let Ok(v) = HeaderValue::from_str(&wait.to_string()) {
            res.headers_mut().insert(header::RETRY_AFTER, v);
        }
        return Ok(res);
    }

    if let Err(e) = record(state, signed, kind, "started", &paths, &head.commit, None).await {
        tracing::warn!(error = ?e, "proposal not recorded: nothing was written to GitHub");
        return Err(match e {
            UpstreamError::Refused { status: 403, .. } => forbidden(),
            _ => ApiError::new(
                StatusCode::BAD_GATEWAY,
                "audit_unavailable",
                "PM OS couldn't record this proposal in the audit log, so it wasn't opened. Try again in a minute.",
            ),
        });
    }

    let branch = proposals::branch_name(kind, handle, now(state));
    let full_title = format!("PM OS {}: {title}", kind.as_str());
    let message = format!("{full_title}\n\nProposed in PM OS by {handle}.");
    let pr_body = body(
        kind,
        handle,
        reason,
        &Base {
            branch: &base_branch,
            commit: &head.commit,
            revision: cfg.content_revision.as_deref(),
        },
        e,
        &paths,
    );
    let opened = async {
        let commit = gh.commit(&head, &message, &files).await?;
        gh.create_branch(&branch, &commit).await?;
        gh.open_pull(&full_title, &branch, &base_branch, &pr_body)
            .await
    }
    .await;
    let (number, url) = match opened {
        Ok(v) => v,
        Err(err) => {
            if let Err(e) = record(state, signed, kind, "failed", &paths, &head.commit, None).await
            {
                tracing::warn!(error = ?e, "proposal failure not recorded");
            }
            return Err(github_error(err));
        }
    };
    let labels_added = match gh
        .add_labels(number, &[proposals::LABEL, kind.label()])
        .await
    {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!(error = %err, pr = number, "labels not added");
            false
        }
    };
    if let Err(e) = record(
        state,
        signed,
        kind,
        "opened",
        &paths,
        &head.commit,
        Some(number),
    )
    .await
    {
        tracing::warn!(error = ?e, pr = number, "proposal opening not recorded");
    }
    tracing::info!(user = %handle, kind = kind.as_str(), files = paths.len(), pr = number, "proposal opened");
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "number": number,
            "url": url,
            "branch": branch,
            "base": {"branch": base_branch, "commit": head.commit},
            "files": paths,
            "widening": e.widening,
            "allowance_changes": e.allowance_changes,
            "stale_check": stale_check,
            "labels_added": labels_added,
        })),
    )
        .into_response())
}

/// The `key=value` pairs of a body's first-line marker.
fn marker(body: &str) -> BTreeMap<String, String> {
    let Some(rest) = body
        .lines()
        .next()
        .and_then(|l| l.trim().strip_prefix("<!-- pmos-proposal "))
        .and_then(|l| l.strip_suffix("-->"))
    else {
        return BTreeMap::new();
    };
    rest.split_whitespace()
        .filter_map(|kv| kv.split_once('='))
        .filter(|(_, v)| {
            v.len() <= 64
                && v.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        })
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// `GET …/proposals`: open pull requests this feature made for `kind` (by branch prefix or
/// label).
pub(super) async fn list(state: &AppState, signed: &Signed, kind: Kind) -> ApiResult {
    let gh = github(state)?;
    let pulls = gh.open_pulls().await.map_err(github_error)?;
    let prefix = format!("{}{}/", proposals::BRANCH_PREFIX, kind.as_str());
    let items: Vec<Value> = pulls
        .into_iter()
        .filter(|p| p.branch.starts_with(&prefix) || p.labels.iter().any(|l| l == kind.label()))
        .map(|p| {
            let m = marker(&p.body);
            json!({
                "number": p.number,
                "url": p.url,
                "title": p.title,
                "branch": p.branch,
                "base": p.base,
                "status": if p.draft { "draft" } else { "open" },
                "proposed_by": m.get("by"),
                "base_commit": m.get("base"),
                "author": p.author,
                "labels": p.labels,
                "created_at": p.created_at,
                "updated_at": p.updated_at,
            })
        })
        .collect();
    tracing::info!(user = %signed.member.handle, kind = kind.as_str(), open = items.len(), "admin read proposals");
    Ok(Json(json!({ "repo": gh.repo(), "proposals": items })).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_are_read_strictly() {
        let m = marker("<!-- pmos-proposal kind=policy by=adam base=abc123 -->\n**x**");
        assert_eq!(m["by"], "adam");
        assert_eq!(m["base"], "abc123");
        let m = marker("<!-- pmos-proposal by=<script> -->");
        assert!(m.is_empty());
        assert!(marker("hello").is_empty());
    }
}
