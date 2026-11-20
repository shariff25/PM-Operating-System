//! What the policy and content admin routes share: audited role checks, the read rule for
//! sensitive files, running the file-system work off the async threads, and sending a
//! proposal to the tool server.
//!
//! **`pmos-web` opens nothing in GitHub and holds no GitHub token.** After its own
//! (interactive) validation, a proposal goes to the tool server as a change set
//! (`POST /internal/admin/proposals/open`, signed for the admin and bound to the body): every
//! file it would write, with the git blob id each was computed from. The tool server checks
//! the admin's role itself, applies the path allow-list and caps again, re-runs the content
//! lint and policy generation on its own deployed copy, checks the base against the default
//! branch, rate-limits, audits, and opens the pull request with the admin's own linked GitHub
//! account (`docs/policy-admin.md`, How a proposal is opened). This module relays its answer
//! with `pmos-web`'s own sentences.
//!
//! **Audited here** (`POST /internal/admin/actions`): a role refusal on any admin policy or
//! content route, a proposal refused by this server's own checks (invalid, stale against the
//! deployed copy), and an admin reading a PM's personal master file or restricted context
//! (through the file route, or named in a validation or proposal, whose diff shows the current
//! text). A sensitive read is recorded **before** the text is served; if it can't be, nothing
//! is served (`502 audit_unavailable`). Never file text; see [`record`] for what is.

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use pmos_contract::permissions::Permission;
use pmos_policy::admin::access::{self, Sensitive};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{ApiError, ApiResult, AppState, Signed};
use crate::admin_config::AdminConfig;
use crate::policy_admin::Evaluation;
use crate::proposals::{self, Change, Issue, Kind};
use crate::upstream::UpstreamError;

/// The largest request body the admin write routes take: 50 files of 256 KiB, as JSON.
pub const MAX_REQUEST: usize = 16 * 1024 * 1024;

pub struct Proposals {
    pub config: AdminConfig,
}

impl Proposals {
    pub fn new(config: AdminConfig) -> Result<Self, String> {
        Ok(Self { config })
    }
}

pub(super) fn forbidden() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "forbidden",
        "Your role in PM OS doesn't allow this.",
    )
}

/// What a viewer did, for the audit log: one of the tool server's fixed admin actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
    PolicyRead,
    PolicyValidate,
    PolicyProposals,
    PolicyProposal,
    ContentRead,
    ContentFile,
    ContentValidate,
    ContentProposals,
    ContentProposal,
    Harness,
}

impl Action {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::PolicyRead => "admin.policy_read",
            Self::PolicyValidate => "admin.policy_validate",
            Self::PolicyProposals => "admin.policy_proposals",
            Self::PolicyProposal => "admin.policy_proposal",
            Self::ContentRead => "admin.content_read",
            Self::ContentFile => "admin.content_file",
            Self::ContentValidate => "admin.content_validate",
            Self::ContentProposals => "admin.content_proposals",
            Self::ContentProposal => "admin.content_proposal",
            Self::Harness => "admin.harness",
        }
    }
}

/// The audit action for reading a sensitive file.
pub const SENSITIVE_READ: &str = "admin.content_sensitive_read";

/// Record an event at the tool server (`POST /internal/admin/actions?user=<viewer>`, signed
/// for the viewer and bound to the body): the action, the outcome (`denied` or `ok`), a fixed
/// note, how many files, and a target that names no content (`content:personal:<handle>`
/// for a personal master file; a restricted file's path is never sent, as its name can be
/// sensitive). Never file text or a path otherwise.
pub(super) async fn record(
    state: &AppState,
    signed: &Signed,
    action: &str,
    outcome: &str,
    detail: &str,
    count: usize,
    target: Option<String>,
) -> Result<(), UpstreamError> {
    signed
        .toolserver(state)
        .admin_record(
            &signed.member.handle,
            "actions",
            &json!({
                "action": action,
                "outcome": outcome,
                "detail": detail,
                "count": count,
                "target": target,
            }),
        )
        .await
        .map(|_| ())
}

/// Record a refusal; a failure to record is logged, and the refusal stands.
pub(super) async fn record_refusal(
    state: &AppState,
    signed: &Signed,
    action: Action,
    detail: &str,
    count: usize,
) {
    if let Err(e) = record(
        state,
        signed,
        action.as_str(),
        "denied",
        detail,
        count,
        None,
    )
    .await
    {
        tracing::warn!(error = ?e, action = action.as_str(), "admin refusal not recorded");
    }
}

/// The viewer has `p`, else `403 forbidden`, recorded at the tool server.
pub(super) async fn need(
    state: &AppState,
    signed: &Signed,
    p: Permission,
    action: Action,
) -> Result<(), ApiError> {
    if signed.member.allows(p) {
        return Ok(());
    }
    tracing::info!(user = %signed.member.handle, action = action.as_str(), "admin route refused: role");
    record_refusal(state, signed, action, "role", 0).await;
    Err(forbidden())
}

fn audit_unavailable() -> ApiError {
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        "audit_unavailable",
        "PM OS couldn't record this in the audit log, so it wasn't done. Try again in a minute.",
    )
}

/// The read rule for files whose current text a route would show: `files` are
/// `(content-relative path, current text)`. A viewer without `edit` may read none that are
/// sensitive (`403 forbidden`, recorded). An admin may, and each sensitive one is recorded
/// first (`admin.content_sensitive_read`); if that can't be recorded, `502
/// audit_unavailable` and nothing is shown.
pub(super) async fn guard_reads(
    state: &AppState,
    signed: &Signed,
    edit: Permission,
    action: Action,
    files: &[(String, Option<String>)],
) -> Result<(), ApiError> {
    let sensitive: Vec<(&str, Sensitive)> = files
        .iter()
        .filter_map(|(rel, text)| {
            access::sensitive(rel, text.as_deref()).map(|s| (rel.as_str(), s))
        })
        .collect();
    if sensitive.is_empty() {
        return Ok(());
    }
    if !signed.member.allows(edit) {
        tracing::info!(user = %signed.member.handle, action = action.as_str(), files = sensitive.len(), "sensitive read refused");
        record_refusal(state, signed, action, "role", sensitive.len()).await;
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "This includes a PM's own file or restricted context: only an admin can open it.",
        ));
    }
    for (rel, why) in sensitive {
        let target = match why {
            Sensitive::Personal => rel
                .split('/')
                .nth(2)
                .map(|handle| format!("content:personal:{handle}")),
            Sensitive::Restricted => None,
        };
        if let Err(e) = record(state, signed, SENSITIVE_READ, "ok", why.note(), 1, target).await {
            tracing::warn!(error = ?e, "sensitive read not recorded: not served");
            return Err(audit_unavailable());
        }
    }
    Ok(())
}

/// Findings on files the viewer may not read lose their message (lint and parser messages can
/// quote a file's values). An admin (`edit`) keeps every message.
pub(super) fn redact(
    issues: &mut [Issue],
    content_root: &std::path::Path,
    content_dir: &str,
    edit: bool,
) {
    if edit {
        return;
    }
    for i in issues.iter_mut() {
        let Some(rel) = i
            .path
            .strip_prefix(content_dir)
            .and_then(|r| r.strip_prefix('/'))
        else {
            continue;
        };
        if !access::may_read_on_disk(false, content_root, rel) {
            i.message = access::REDACTED_MESSAGE.to_string();
        }
    }
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

/// `pmos-web`'s own sentence for a code the tool server answers a proposal with, and the
/// extra fields of that answer the browser gets. `None`: not a code relayed as it is.
fn relayed(code: &str) -> Option<(&'static str, &'static [&'static str])> {
    Some(match code {
        "forbidden" => ("Your role in PM OS doesn't allow this.", &[]),
        "github_not_linked" => (
            "Link your GitHub account to PM OS first (in Slack: the PM OS app's Home tab, Accounts). Pull requests are opened as you, so GitHub can require someone else's review.",
            &[],
        ),
        "stale_base" => (
            "These files on the base branch differ from the deployed copy this change was made from. Wait for the deployment to catch up, or make the change in GitHub.",
            &["paths", "base"],
        ),
        "invalid" => (
            "This change doesn't pass validation; nothing was proposed.",
            &["errors", "warnings"],
        ),
        "policy_mismatch" => (
            "The tool server regenerates different policy files from this change: the web app and the tool server run different content or code. Wait for both to deploy the same revision.",
            &["paths"],
        ),
        "rate_limited" => (
            "That's this hour's proposals for you. Try again later.",
            &["retry_after"],
        ),
        "not_configured" => (
            "Proposals aren't set up on the tool server (the GitHub App and the content repository). Reading and validating still work.",
            &[],
        ),
        "no_change" => (
            "Every file is the same as the deployed one: there's nothing to propose.",
            &[],
        ),
        "bad_request" | "bad_path" | "path_not_allowed" | "not_text" | "not_found" => (
            "The tool server refused a file in this change; nothing was proposed.",
            &[],
        ),
        "too_large" => ("A file is over 256 KiB.", &[]),
        "too_many_files" => ("This change has too many files.", &[]),
        "audit_unavailable" => (
            "PM OS couldn't record this proposal in the audit log, so it wasn't opened. Try again in a minute.",
            &[],
        ),
        "github_refused" => (
            "GitHub refused your account for this repository: it needs write access to it (contents and pull requests). Nothing was opened.",
            &[],
        ),
        "github_unavailable" => (
            "GitHub didn't take this proposal. Try again in a minute.",
            &[],
        ),
        "github_rate_limited" => (
            "GitHub's rate limit was reached. Try again in a few minutes.",
            &["retry_after"],
        ),
        _ => return None,
    })
}

/// The tool server's answer to a proposal, as the browser gets it.
fn relay(status: u16, body: Value) -> ApiResult {
    if status == 201 {
        return Ok((StatusCode::CREATED, Json(body)).into_response());
    }
    let code = body.get("code").and_then(Value::as_str).unwrap_or_default();
    let Some((sentence, extras)) = relayed(code) else {
        tracing::warn!(status, code, "the tool server refused a proposal");
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "upstream_unavailable",
            "The tool server refused this. If it keeps happening, tell a platform owner.",
        ));
    };
    let status = StatusCode::from_u16(status)
        .ok()
        .filter(|s| s.is_client_error() || s.is_server_error())
        .unwrap_or(StatusCode::BAD_GATEWAY);
    let extra: serde_json::Map<String, Value> = extras
        .iter()
        .filter_map(|k| body.get(*k).map(|v| (k.to_string(), v.clone())))
        .collect();
    let retry = body.get("retry_after").and_then(Value::as_u64);
    let mut res = coded(status, code, sentence, Value::Object(extra));
    if let Some(secs) = retry {
        if let Ok(v) = HeaderValue::from_str(&secs.to_string()) {
            res.headers_mut().insert(header::RETRY_AFTER, v);
        }
    }
    Ok(res)
}

/// The deployed content's commit, when known: what the change was computed from.
fn content_revision(state: &AppState) -> Value {
    crate::content_admin::revision(
        &state.0.content_root,
        state.0.proposals.config.content_revision.as_deref(),
    )["commit"]
        .clone()
}

/// Send a validated change to the tool server to open as a pull request (see the module
/// docs). `content` is what the admin changed (content-relative); the regenerated policy
/// files come from `e`.
pub(super) async fn open(
    state: &AppState,
    signed: &Signed,
    kind: Kind,
    title: &str,
    reason: &str,
    content: &[Change],
    e: &Evaluation,
) -> ApiResult {
    let cfg = &state.0.proposals.config;
    let files = pmos_policy::admin::changeset::proposed_files(&cfg.layout, content, e);
    let body = json!({
        "kind": kind.as_str(),
        "title": title,
        "reason": reason,
        "base_sha": content_revision(state),
        "changes": files,
    });
    let handle = signed.member.handle.as_str();
    let (status, answer) = match signed.toolserver(state).open_proposal(handle, &body).await {
        Ok(v) => v,
        Err(UpstreamError::Unknown { why, .. }) => {
            tracing::warn!(why = %why, "proposal sent; the answer was lost");
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "unknown_outcome",
                "The proposal was sent, but the answer was lost. Reload the proposals list to see whether it opened.",
            ));
        }
        Err(e) => return Err(ApiError::upstream(e)),
    };
    let pr = answer.get("number").and_then(|n| n.as_u64());
    tracing::info!(user = %handle, kind = kind.as_str(), files = files.len(), status, pr = ?pr, "proposal sent to the tool server");
    relay(status, answer)
}

/// `GET …/proposals`: open pull requests this feature made for `kind`, read by the tool server
/// with the viewer's own linked GitHub account. Without one, an empty list that says so.
pub(super) async fn list(state: &AppState, signed: &Signed, kind: Kind) -> ApiResult {
    let handle = signed.member.handle.as_str();
    let (status, v) = signed
        .toolserver(state)
        .admin_raw(handle, "proposals", &[("kind", kind.as_str())])
        .await
        .map_err(ApiError::upstream)?;
    match status {
        200 => {
            let linked = v.get("github").and_then(Value::as_str) != Some("not_linked");
            tracing::info!(user = %handle, kind = kind.as_str(), linked, "admin read proposals");
            Ok(Json(json!({
                "repo": v.get("repo"),
                "github": if linked { "linked" } else { "not_linked" },
                "message": (!linked).then_some(
                    "Link your GitHub account to PM OS to see open proposals (in Slack: the PM OS app's Home tab, Accounts). They are read from GitHub as you."
                ),
                "proposals": v.get("proposals").cloned().unwrap_or(json!([])),
            }))
            .into_response())
        }
        403 => Err(forbidden()),
        _ => {
            // Not configured, or GitHub refused or failed: the same codes as a proposal.
            relay(status, v)
        }
    }
}

/// Whether the tool server could verify branch protection on the base branch with the
/// viewer's own account, and whether proposals are set up there. Best effort: anything that
/// goes wrong is `verified: false`, never an error.
pub(super) async fn protection(state: &AppState, signed: &Signed) -> (Value, bool) {
    let unknown = json!({"required_reviews": null, "verified": false, "admins_enforced": null, "bypass_allowed": null});
    match signed
        .toolserver(state)
        .admin(&signed.member.handle, "protection", &[])
        .await
    {
        Ok(v) => {
            let configured = v
                .get("configured")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let reviews = v.get("required_reviews").and_then(Value::as_u64);
            let verified = v.get("verified").and_then(Value::as_bool).unwrap_or(false);
            let flag = |k: &str| v.get(k).and_then(Value::as_bool);
            (
                json!({
                    "branch": v.get("branch"),
                    "required_reviews": reviews,
                    "verified": verified,
                    "admins_enforced": flag("admins_enforced"),
                    "bypass_allowed": flag("bypass_allowed"),
                    "github": v.get("github"),
                }),
                configured,
            )
        }
        Err(e) => {
            tracing::warn!(error = ?e, "branch protection unknown");
            (unknown, false)
        }
    }
}
