//! Admin policy and content proposals, opened as GitHub pull requests **as the acting admin**
//! (`docs/policy-admin.md`, How a proposal is opened; `docs/toolserver.md`, Admin routes).
//!
//! `pmos-web` validates a change interactively and sends the change set here
//! (`POST /internal/admin/proposals/open`, with the admin's signed user assertion bound to the
//! body). This server trusts none of `pmos-web`'s work:
//!
//! 1. **Who:** the assertion's subject must be the `user=` it names, and an admin by this
//!    server's own `team.yaml` (`edit-policies` or `edit-content`, by kind).
//! 2. **Where:** the content and policies folders are separate, and neither is the
//!    repository root (refused at start-up, and again here).
//! 3. **What:** [`pmos_policy::admin::changeset::recheck`]: the path allow-list (content files
//!    and generated policy files only), the caps, the kind's limits, each file against this
//!    server's own deployed copy, the content lint and every policy rule on the changed tree,
//!    and the policy files exactly as this server regenerates them.
//! 4. **As whom:** the admin's own linked GitHub token from the vault, or `409
//!    github_not_linked`. So the pull request's author is the admin, and GitHub's branch
//!    protection refuses their own approval.
//! 5. **Stale base:** each file on the content branch's head must be what this server had.
//! 6. **Rate limit:** [`ProposalsSetup::per_hour`] per admin per hour, in memory (this server
//!    runs as one replica; a restart forgets).
//! 7. **Audit first:** `started`, with the paths (served as a count); if it can't be recorded,
//!    nothing is written to GitHub. Then one commit on the head, a new branch under
//!    `pmos/proposal/<kind>/` made here, the pull request, its labels, and `ok` (the pull
//!    request as the target) or `error`. Every refusal is recorded too, `denied`, with a fixed
//!    note. Never file text; never the token.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::Utc;
use pmos_contract::permissions::Permission;
use pmos_policy::admin::changeset::{self, Deployed, Layout, ProposedFile, Rejection};
use pmos_policy::admin::evaluate::Evaluation;
use pmos_policy::admin::proposals::{self, git_blob_sha, Kind, RateLimiter};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::audit::{AuditEvent, Client, Outcome};
use crate::github::{Change, GitHub, GitHubError};
use crate::oauth::{OAuth, TokenError, TokenSource};
use crate::onboarding::GitHubSetup;
use crate::service::ToolService;
use crate::vault::{Provider, Secret};

/// The run id proposals are recorded under, as the admin reads are.
pub const RUN: &str = "admin";
/// How long the branch-protection check may take before it answers "unknown".
pub const PROTECTION_WAIT: Duration = Duration::from_secs(5);

/// What proposals need: GitHub, the content repository and branch, the layout, this
/// server's own deployed copy, and the rate limit.
pub struct ProposalsSetup {
    github: Arc<GitHub>,
    repo: String,
    branch: String,
    /// `Err`: the content or policies folder is the repository root or nested: every proposal
    /// is refused (`501 not_configured`).
    layout: Result<Layout, String>,
    content_root: PathBuf,
    policies_root: Option<PathBuf>,
    per_hour: u32,
    limiter: RateLimiter,
}

impl ProposalsSetup {
    /// From the GitHub setup (which needs a content repository), with `per_hour` and the
    /// checked-in policies root (`None`: the policies folder of the checkout the content root
    /// is in, when it exists).
    pub fn new(gh: &GitHubSetup, per_hour: u32, policies_root: Option<PathBuf>) -> Option<Self> {
        let repo = gh.content_repo.clone()?;
        let layout = Layout::new(&gh.content_dir, &gh.policies_dir);
        let policies_root = policies_root.or_else(|| {
            let layout = layout.as_ref().ok()?;
            let mut checkout = gh.content_root.canonicalize().ok()?;
            for _ in layout.content_dir().split('/') {
                checkout = checkout.parent()?.to_path_buf();
            }
            let dir = checkout.join(layout.policies_dir());
            dir.is_dir().then_some(dir)
        });
        Some(Self {
            github: gh.github.clone(),
            repo,
            branch: gh.content_branch.clone(),
            layout,
            content_root: gh.content_root.clone(),
            policies_root,
            per_hour,
            limiter: RateLimiter::new(per_hour),
        })
    }

    pub fn repo(&self) -> &str {
        &self.repo
    }
}

fn coded(status: StatusCode, code: &str, error: &str, extra: Value) -> Response {
    let mut body = json!({ "code": code, "error": error });
    if let (Some(b), Value::Object(extra)) = (body.as_object_mut(), extra) {
        b.extend(extra);
    }
    (status, Json(body)).into_response()
}

fn not_configured(why: &str) -> Response {
    coded(
        StatusCode::NOT_IMPLEMENTED,
        "not_configured",
        &format!("admin proposals are off on this tool server: {why}"),
        json!({}),
    )
}

fn github_not_linked() -> Response {
    coded(
        StatusCode::CONFLICT,
        "github_not_linked",
        "this admin hasn't linked GitHub in PM OS; proposals are opened with their own account",
        json!({}),
    )
}

fn kind_of(s: &str) -> Option<Kind> {
    match s {
        "policy" => Some(Kind::Policy),
        "content" => Some(Kind::Content),
        _ => None,
    }
}

fn action(kind: Kind) -> &'static str {
    match kind {
        Kind::Policy => "admin.policy_proposal",
        Kind::Content => "admin.content_proposal",
    }
}

/// The admin's GitHub token from the vault, or `None` when they haven't linked GitHub (or must
/// link it again). `Err` is an answer.
async fn token(oauth: Option<&Arc<OAuth>>, user: &str) -> Result<Option<Secret>, Box<Response>> {
    let Some(oauth) = oauth else {
        return Ok(None);
    };
    match oauth.token(user, Provider::Github).await {
        Ok(t) => Ok(Some(t.token)),
        Err(TokenError::NotConnected(_) | TokenError::Reconnect(_)) => Ok(None),
        Err(e) => {
            tracing::warn!(error = %e, "GitHub token unavailable");
            Err(Box::new(coded(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "the linked GitHub account can't be read right now",
                json!({}),
            )))
        }
    }
}

fn github_error(e: &GitHubError) -> Response {
    tracing::warn!(error = %e, "GitHub call for a proposal failed");
    match e {
        GitHubError::Reconnect => github_not_linked(),
        GitHubError::Forbidden(_) => coded(
            StatusCode::BAD_GATEWAY,
            "github_refused",
            "GitHub refused this admin's account for the repository",
            json!({}),
        ),
        GitHubError::RateLimited(s) => coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "github_rate_limited",
            "GitHub's rate limit was reached",
            json!({"retry_after": s}),
        ),
        _ => coded(
            StatusCode::BAD_GATEWAY,
            "github_unavailable",
            "GitHub didn't take this proposal",
            json!({}),
        ),
    }
}

/// What `POST /internal/admin/proposals/open` takes.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenRequest {
    /// `policy` or `content`.
    pub kind: String,
    pub title: String,
    pub reason: String,
    /// The deployed content revision the sender computed from, when it knows it: shown in the
    /// pull request, not trusted for anything.
    #[serde(default)]
    pub base_sha: Option<String>,
    pub changes: Vec<ProposedFile>,
}

/// One admin event: no sources, target or detail yet.
pub fn event(client: Client, user: &str, tool: &str, outcome: Outcome) -> AuditEvent {
    AuditEvent {
        at: Utc::now(),
        client,
        run_id: RUN.to_string(),
        user: user.to_string(),
        skill: None,
        bot: None,
        tool: tool.to_string(),
        outcome,
        sources: Vec::new(),
        source_titles: Default::default(),
        args_hash: None,
        output_ref: None,
        detail: None,
    }
}

/// Records one event; `Err` when it couldn't be recorded.
async fn record(svc: &ToolService, event: AuditEvent) -> Result<(), String> {
    svc.stores()
        .audit
        .record(event)
        .await
        .map_err(|e| e.to_string())
}

/// Record a refusal with a fixed note (best effort: the refusal stands either way).
async fn refusal(svc: &ToolService, client: Client, user: &str, tool: &str, note: &str) {
    let mut e = event(client, user, tool, Outcome::Denied);
    e.detail = Some(note.to_string());
    if let Err(e) = record(svc, e).await {
        tracing::error!(error = %e, tool, "AUDIT EVENT NOT RECORDED");
    }
}

/// The base a proposal is made on: the branch, its head commit, and the sender's content
/// revision when it gave one.
struct Base<'a> {
    branch: &'a str,
    commit: &'a str,
    revision: Option<&'a str>,
}

/// The pull request's body: who proposed it (their PM OS handle, in code so GitHub notifies
/// nobody), the reason (fenced, so it can't format or mention), the base, what the sandboxes
/// gain and lose, and the files.
fn pr_body(
    kind: Kind,
    handle: &str,
    reason: &str,
    base: &Base,
    e: &Evaluation,
    files: &[String],
) -> String {
    let (branch, commit, revision) = (base.branch, base.commit, base.revision);
    let mut b = format!(
        "<!-- pmos-proposal kind={} by={handle} base={commit} -->\n\
         **Proposed in PM OS by `{handle}`**, opened with their own linked GitHub account: they can't approve it themselves.\n\n\
         ### Reason\n\n```text\n{}\n```\n\n\
         ### Base\n\n`{branch}` at `{commit}`. Checked by the PM OS tool server against its deployed content{}.\n\n",
        kind.as_str(),
        reason.replace("```", "'''"),
        match revision {
            Some(r) => format!("; the web app computed it from `{r}`"),
            None => String::new(),
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
         PM OS never merges: another person reviews and merges this in GitHub.\n",
    );
    proposals::clean_text(&b, proposals::MAX_BODY)
}

/// A rejection as an answer.
fn rejected(r: &Rejection) -> Response {
    let status = StatusCode::from_u16(r.status).unwrap_or(StatusCode::BAD_REQUEST);
    (status, Json(r)).into_response()
}

/// `POST /internal/admin/proposals/open?user=<admin>`: see the module docs. `user` is the
/// assertion's verified subject.
pub async fn open(
    svc: &ToolService,
    oauth: Option<&Arc<OAuth>>,
    setup: Option<&ProposalsSetup>,
    client: Client,
    user: &str,
    raw: &[u8],
) -> Response {
    let Ok(req) = serde_json::from_slice::<OpenRequest>(raw) else {
        return coded(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "the body isn't a proposal",
            json!({}),
        );
    };
    let Some(kind) = kind_of(&req.kind) else {
        return coded(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "`kind` is `policy` or `content`",
            json!({}),
        );
    };
    let tool = action(kind);
    let need = match kind {
        Kind::Policy => Permission::EditPolicies,
        Kind::Content => Permission::EditContent,
    };
    let allowed = match svc.people().user(user).await {
        Ok(u) => u.is_some_and(|u| crate::users::access::permits(&u, need)),
        Err(e) => {
            tracing::error!(error = %e, "users unavailable");
            return coded(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "PM OS storage is unavailable",
                json!({}),
            );
        }
    };
    if !allowed {
        tracing::info!(viewer = %user, tool, "proposal refused: role");
        refusal(svc, client, user, tool, "role").await;
        return coded(
            StatusCode::FORBIDDEN,
            "forbidden",
            "this person's role doesn't allow this",
            json!({}),
        );
    }
    let Some(setup) = setup else {
        refusal(svc, client, user, tool, "not configured").await;
        return not_configured("it needs the GitHub App and PMOS_CONTENT_REPO");
    };
    let layout = match &setup.layout {
        Ok(l) => l.clone(),
        Err(why) => {
            refusal(svc, client, user, tool, "not configured").await;
            return not_configured(why);
        }
    };
    let title = proposals::clean_line(&req.title, proposals::MAX_TITLE);
    let reason = proposals::clean_text(&req.reason, proposals::MAX_REASON);
    let revision = req
        .base_sha
        .as_deref()
        .filter(|s| (7..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit()));
    if title.is_empty() || reason.is_empty() || (req.base_sha.is_some() && revision.is_none()) {
        refusal(svc, client, user, tool, "invalid").await;
        return coded(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "a proposal needs a title, a reason, and `base_sha` (if given) as a commit sha",
            json!({}),
        );
    }
    let Some(token) = (match token(oauth, user).await {
        Ok(t) => t,
        Err(answer) => return *answer,
    }) else {
        refusal(svc, client, user, tool, "github not linked").await;
        return github_not_linked();
    };

    // Everything again, from nothing, on this server's own deployed copy.
    let (content_root, policies_root) = (setup.content_root.clone(), setup.policies_root.clone());
    let files = req.changes;
    let layout2 = layout.clone();
    let checked = tokio::task::spawn_blocking(move || {
        let deployed = Deployed {
            content_root: &content_root,
            policies_root: policies_root.as_deref(),
        };
        changeset::recheck(kind, &layout2, &deployed, &files, Utc::now().date_naive())
    })
    .await;
    let checked = match checked {
        Ok(Ok(c)) => c,
        Ok(Err(r)) => {
            tracing::info!(viewer = %user, tool, code = r.code, "proposal refused");
            refusal(svc, client, user, tool, r.note()).await;
            return rejected(&r);
        }
        Err(_) => {
            return coded(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "the proposal couldn't be checked",
                json!({}),
            )
        }
    };
    let paths: Vec<String> = checked.files.iter().map(|(p, _)| p.clone()).collect();

    // The base: the content branch's head, which must have every file as this server has it.
    let gh = &setup.github;
    let repo = setup.repo.as_str();
    let head = match gh.head(&token, repo, &setup.branch).await {
        Ok(h) => h,
        Err(e) => return github_error(&e),
    };
    let tree = match gh.tree(&token, repo, &head.tree).await {
        Ok(t) => t,
        Err(e) => return github_error(&e),
    };
    let stale_check = if tree.truncated {
        tracing::warn!(viewer = %user, "GitHub truncated the base tree: stale-base check skipped");
        "skipped"
    } else {
        let on_branch: BTreeMap<&str, &str> = tree
            .entries
            .iter()
            .filter(|e| e.kind == "blob")
            .map(|e| (e.path.as_str(), e.sha.as_str()))
            .collect();
        let stale: Vec<&String> = checked
            .base_texts
            .iter()
            .filter(|(path, old)| {
                on_branch.get(path.as_str()).copied()
                    != old.as_ref().map(|t| git_blob_sha(t.as_bytes())).as_deref()
            })
            .map(|(p, _)| p)
            .collect();
        if !stale.is_empty() {
            tracing::info!(viewer = %user, tool, files = stale.len(), "proposal refused: stale base");
            refusal(svc, client, user, tool, "stale base").await;
            return coded(
                StatusCode::CONFLICT,
                "stale_base",
                "these files on the base branch differ from the deployed copy this change was made from",
                json!({"paths": stale, "base": {"branch": setup.branch, "commit": head.commit}}),
            );
        }
        "passed"
    };

    if let Err(wait) = setup.limiter.take(user, Utc::now()) {
        tracing::info!(viewer = %user, tool, "proposal refused: rate limit");
        refusal(svc, client, user, tool, "rate limited").await;
        let mut res = coded(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            &format!(
                "that's {} proposals this hour; try again in {} minutes",
                setup.per_hour,
                wait.div_ceil(60)
            ),
            json!({"retry_after": wait}),
        );
        if let Ok(v) = HeaderValue::from_str(&wait.to_string()) {
            res.headers_mut().insert(header::RETRY_AFTER, v);
        }
        return res;
    }

    // Audit first: nothing is written to GitHub unless this is recorded.
    let mut started = event(client, user, tool, Outcome::Started);
    started.sources = paths.clone();
    if let Err(e) = record(svc, started).await {
        tracing::error!(error = %e, "proposal not recorded: nothing was written to GitHub");
        return coded(
            StatusCode::BAD_GATEWAY,
            "audit_unavailable",
            "the proposal couldn't be recorded in the audit log, so it wasn't opened",
            json!({}),
        );
    }

    let branch = proposals::branch_name(kind, user, Utc::now());
    let full_title = format!("PM OS {}: {title}", kind.as_str());
    let message = format!("{full_title}\n\nProposed in PM OS by {user}.");
    let pr_body = pr_body(
        kind,
        user,
        &reason,
        &Base {
            branch: &setup.branch,
            commit: &head.commit,
            revision,
        },
        &checked.evaluation,
        &paths,
    );
    let changes: Vec<Change> = checked
        .files
        .iter()
        .map(|(path, text)| Change {
            path: path.clone(),
            text: text.clone(),
        })
        .collect();
    let opened = async {
        let commit = gh
            .create_commit(&token, repo, &head, &message, &changes)
            .await?;
        gh.create_branch(&token, repo, &branch, &commit).await?;
        gh.open_pull(&token, repo, &full_title, &branch, &setup.branch, &pr_body)
            .await
    }
    .await;
    let (number, url, author) = match opened {
        Ok(v) => v,
        Err(err) => {
            let mut failed = event(client, user, tool, Outcome::Error);
            failed.sources = paths.clone();
            if let Err(e) = record(svc, failed).await {
                tracing::error!(error = %e, "AUDIT EVENT NOT RECORDED");
            }
            return github_error(&err);
        }
    };
    let labels_added = match gh
        .add_labels(&token, repo, number, &[proposals::LABEL, kind.label()])
        .await
    {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!(error = %err, pr = number, "labels not added");
            false
        }
    };
    let mut done = event(client, user, tool, Outcome::Ok);
    done.sources = paths.clone();
    done.output_ref = Some(format!("github:{repo}#{number}"));
    if let Err(e) = record(svc, done).await {
        tracing::error!(error = %e, pr = number, "AUDIT EVENT NOT RECORDED");
    }
    tracing::info!(viewer = %user, kind = kind.as_str(), files = paths.len(), pr = number, "proposal opened as the admin");
    let e = &checked.evaluation;
    (
        StatusCode::CREATED,
        Json(json!({
            "number": number,
            "url": url,
            "author": author,
            "branch": branch,
            "base": {"branch": setup.branch, "commit": head.commit},
            "files": paths,
            "widening": e.widening,
            "allowance_changes": e.allowance_changes,
            "stale_check": stale_check,
            "labels_added": labels_added,
        })),
    )
        .into_response()
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

/// `GET /internal/admin/proposals?user=<viewer>&kind=`: the open pull requests this feature
/// made for `kind` (by branch prefix or label), read with the viewer's own linked GitHub
/// account. Without one: an empty list with `github: "not_linked"`.
pub async fn list(
    oauth: Option<&Arc<OAuth>>,
    setup: Option<&ProposalsSetup>,
    viewer: &str,
    kind: Kind,
) -> Response {
    let Some(setup) = setup else {
        return not_configured("it needs the GitHub App and PMOS_CONTENT_REPO");
    };
    let not_linked = || {
        Json(json!({"repo": setup.repo, "github": "not_linked", "proposals": []})).into_response()
    };
    let token = match token(oauth, viewer).await {
        Ok(Some(t)) => t,
        Ok(None) => return not_linked(),
        Err(answer) => return *answer,
    };
    let pulls = match setup.github.open_pulls(&token, &setup.repo).await {
        Ok(p) => p,
        Err(GitHubError::Reconnect) => return not_linked(),
        Err(e) => return github_error(&e),
    };
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
    Json(json!({"repo": setup.repo, "github": "linked", "proposals": items})).into_response()
}

/// `GET /internal/admin/protection?user=<viewer>`: whether the content branch requires
/// reviews, as far as the viewer's own GitHub account can read it. Best effort, never an
/// error: `verified: false` when it can't tell.
pub async fn protection(
    oauth: Option<&Arc<OAuth>>,
    setup: Option<&ProposalsSetup>,
    viewer: &str,
) -> Response {
    let Some(setup) = setup else {
        return Json(json!({"configured": false, "branch": null, "required_reviews": null, "verified": false, "github": null}))
            .into_response();
    };
    let configured = setup.layout.is_ok();
    let unknown = |github: &str| {
        Json(json!({"configured": configured, "branch": setup.branch, "required_reviews": null, "verified": false, "github": github}))
            .into_response()
    };
    let token = match token(oauth, viewer).await {
        Ok(Some(t)) => t,
        Ok(None) => return unknown("not_linked"),
        Err(_) => return unknown("unavailable"),
    };
    let read = setup.github.protection(&token, &setup.repo, &setup.branch);
    match tokio::time::timeout(PROTECTION_WAIT, read).await {
        Ok(Some(p)) => Json(json!({
            "configured": configured,
            "branch": setup.branch,
            "required_reviews": p.required_reviews,
            "verified": true,
            "source": p.source,
            "github": "linked",
        }))
        .into_response(),
        _ => unknown("linked"),
    }
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
