//! Harness and context-layer management: `GET /api/admin/content`,
//! `GET /api/admin/content/file?path=`, `POST /api/admin/content/validate`,
//! `POST /api/admin/content/proposals`, `GET /api/admin/content/proposals` and
//! `GET /api/admin/harness` (`docs/web-api.md`, Admin: content and harness;
//! `docs/content-admin.md`).
//!
//! Read-and-propose: nothing here writes a content file. Reading needs `view-content`
//! (admins and auditors; an auditor can't open a PM's personal master file or restricted
//! context, and the tree hides the lint messages of those files from them). Validating and
//! proposing need `edit-content` (admins): a validation's diff shows the current text of every
//! file it names. An admin opening a personal or restricted file, or naming one in a
//! validation or proposal, is recorded first (`proposal::guard_reads`). Role refusals are
//! recorded too.

use std::collections::BTreeSet;
use std::path::PathBuf;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use pmos_contract::permissions::Permission;
use serde::Deserialize;
use serde_json::{json, Value};

use super::proposal::{self, blocking, need, read_log, refused, Action, MAX_REQUEST};
use super::{now, parse, ApiError, ApiResult, AppState, Signed};
use crate::content_admin;
use crate::policy_admin::{self, Evaluation, Roots};
use crate::proposals::{self, Change, Kind};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/content", get(tree))
        .route("/admin/content/file", get(file))
        .route("/admin/content/validate", post(validate))
        .route("/admin/content/proposals", post(propose).get(list))
        .route("/admin/harness", get(harness))
        .layer(DefaultBodyLimit::max(MAX_REQUEST))
}

fn content_dir(state: &AppState) -> String {
    state.0.proposals.config.layout.content_dir().to_string()
}

/// `GET /api/admin/content`.
async fn tree(State(state): State<AppState>, signed: Signed) -> ApiResult {
    need(
        &state,
        &signed,
        Permission::ViewContent,
        Action::ContentRead,
    )
    .await?;
    read_log(&signed, "/api/admin/content");
    let root = state.0.content_root.clone();
    let dir = content_dir(&state);
    let today = now(&state).date_naive();
    let revision = state.0.proposals.config.content_revision.clone();
    let admin = signed.member.allows(Permission::EditContent);
    let checked_at = now(&state);
    // The branch-protection check (a tool server call) runs beside the file-system work.
    let (protection, body) = tokio::join!(
        proposal::protection(&state, &signed),
        blocking(move || {
            let report = policy_admin::today_report(&root, today);
            let (files, other) = content_admin::tree(&root, &report, &dir, admin);
            let mut lint = content_admin::lint_summary(&report);
            lint["checked_at"] = json!(checked_at);
            lint["other"] = json!(other);
            let mut out = content_admin::catalog(&report);
            out["revision"] = content_admin::revision(&root, revision.as_deref());
            out["root"] = json!(dir);
            out["lint"] = lint;
            out["files"] = json!(files);
            out
        })
    );
    let (protection, configured) = protection;
    let mut body = body?;
    body["proposals_configured"] = json!(configured);
    body["protection"] = protection;
    Ok(Json(body).into_response())
}

#[derive(Deserialize)]
struct FileQuery {
    path: Option<String>,
}

/// `GET /api/admin/content/file?path=content/…`.
async fn file(
    State(state): State<AppState>,
    signed: Signed,
    q: Result<Query<FileQuery>, axum::extract::rejection::QueryRejection>,
) -> ApiResult {
    need(
        &state,
        &signed,
        Permission::ViewContent,
        Action::ContentFile,
    )
    .await?;
    let Ok(Query(q)) = q else {
        return Err(ApiError::bad_request("These query parameters don't parse."));
    };
    let path = q.path.unwrap_or_default();
    let dir = content_dir(&state);
    let rel = proposals::content_rel(&path, &dir).map_err(|r| refused(r, ""))?;
    read_log(&signed, "/api/admin/content/file");
    let root = state.0.content_root.clone();
    let admin = signed.member.allows(Permission::EditContent);
    let rel2 = rel.clone();
    let text = blocking(move || proposals::read_text(&root, &rel2))
        .await?
        .map_err(|r| refused(r, ""))?
        .ok_or_else(ApiError::not_found)?;
    let view = content_admin::view(&rel, path, text);
    if !admin {
        // The one read rule, on the text itself: it fails closed when the front matter
        // doesn't parse and reads `restricted` in any letter case (UM3).
        if !pmos_policy::admin::access::may_read(false, &rel, Some(&view.text)) {
            proposal::record_refusal(&state, &signed, Action::ContentFile, "role", 1).await;
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "forbidden",
                "This file is a PM's own or restricted context: only an admin can open it.",
            ));
        }
    }
    // An admin: a sensitive file is recorded before its text is served.
    proposal::guard_reads(
        &state,
        &signed,
        Permission::EditContent,
        Action::ContentFile,
        &[(rel.clone(), Some(view.text.clone()))],
    )
    .await?;
    Ok(Json(view).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangeIn {
    path: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    delete: bool,
    /// The `sha256` the file had when the admin opened it (`GET …/file`); with it, a change to
    /// a file the server has since redeployed is refused `409 stale_base`.
    #[serde(default)]
    base_sha256: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidateBody {
    changes: Vec<ChangeIn>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposeBody {
    changes: Vec<ChangeIn>,
    title: String,
    reason: String,
}

/// Each named file's content-relative path and current text, for the read rule.
type Named = Vec<(String, Option<String>)>;

/// The changes, checked: count, paths, kind of file, text. `Ok` also has every named file's
/// current text. `Err` is the answer.
fn check_changes(state: &AppState, input: Vec<ChangeIn>) -> Result<(Vec<Change>, Named), ApiError> {
    if input.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "no_change",
            "There are no changes.",
        ));
    }
    if input.len() > proposals::MAX_FILES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_many_files",
            format!("A proposal changes at most {} files.", proposals::MAX_FILES),
        ));
    }
    let dir = content_dir(state);
    let root = &state.0.content_root;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let mut named = Vec::new();
    let mut stale = Vec::new();
    for c in input {
        let rel = proposals::content_rel(&c.path, &dir).map_err(|r| refused(r, &c.path))?;
        proposals::writable(&rel).map_err(|r| refused(r, &c.path))?;
        if !seen.insert(rel.clone()) {
            return Err(ApiError::bad_request(format!("{}: listed twice.", c.path)));
        }
        let text = match (c.text, c.delete) {
            (Some(t), false) => {
                proposals::check_text(&t).map_err(|r| refused(r, &c.path))?;
                Some(t)
            }
            (None, true) => None,
            _ => {
                return Err(ApiError::bad_request(format!(
                    "{}: give either `text` or `delete: true`.",
                    c.path
                )))
            }
        };
        // No symlink anywhere on the way, and what's there now, for the base check.
        let current = proposals::read_text(root, &rel).map_err(|r| refused(r, &c.path))?;
        if let Some(want) = &c.base_sha256 {
            let have = current
                .as_ref()
                .map(|t| proposals::sha256_hex(t.as_bytes()));
            if have.as_deref() != Some(want.to_ascii_lowercase().as_str()) {
                stale.push(c.path.clone());
            }
        }
        if current.is_none() && text.is_none() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "not_found",
                format!("{}: doesn't exist, so it can't be deleted.", c.path),
            ));
        }
        named.push((rel.clone(), current.clone()));
        if current.as_ref() == text.as_ref() {
            continue;
        }
        out.push(Change { rel, text });
    }
    if !stale.is_empty() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "stale_base",
            format!(
                "Changed on the server since you opened it: {}. Reload and make the change again.",
                stale.join(", ")
            ),
        ));
    }
    if out.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "no_change",
            "Every file is the same as the deployed one: there's nothing to propose.",
        ));
    }
    Ok((out, named))
}

async fn evaluate(
    state: &AppState,
    changes: Vec<Change>,
) -> Result<(Vec<Change>, Evaluation), ApiError> {
    let cfg = &state.0.proposals.config;
    let content: PathBuf = state.0.content_root.clone();
    let policies = cfg.policies_root.clone();
    let content_dir = cfg.layout.content_dir().to_string();
    let policies_dir = cfg.layout.policies_dir().to_string();
    let today = now(state).date_naive();
    blocking(move || {
        let roots = Roots {
            content: &content,
            policies: policies.as_deref(),
            content_dir: &content_dir,
            policies_dir: &policies_dir,
        };
        let e = policy_admin::evaluate(&roots, &changes, today)
            .map_err(|i| ApiError::new(StatusCode::BAD_REQUEST, &i.code, i.message))?;
        Ok((changes, e))
    })
    .await?
}

/// The changes, checked, with the read rule applied to every file they name; a refusal of
/// this server's own (a stale base) is recorded for `action`.
async fn checked(
    state: &AppState,
    signed: &Signed,
    action: Action,
    input: Vec<ChangeIn>,
) -> Result<Vec<Change>, ApiError> {
    let count = input.len();
    let (changes, named) = match check_changes(state, input) {
        Ok(v) => v,
        Err(e) => {
            if e.code() == "stale_base" {
                proposal::record_refusal(state, signed, action, "stale base", count).await;
            }
            return Err(e);
        }
    };
    proposal::guard_reads(state, signed, Permission::EditContent, action, &named).await?;
    Ok(changes)
}

/// `POST /api/admin/content/validate`: admins only, since the diffs show the current text of
/// every file named.
async fn validate(State(state): State<AppState>, signed: Signed, body: Bytes) -> ApiResult {
    need(
        &state,
        &signed,
        Permission::EditContent,
        Action::ContentValidate,
    )
    .await?;
    let b: ValidateBody = parse(&body)?;
    let changes = checked(&state, &signed, Action::ContentValidate, b.changes).await?;
    read_log(&signed, "/api/admin/content/validate");
    let (_, e) = evaluate(&state, changes).await?;
    Ok(Json(proposal::evaluation_json(&e)).into_response())
}

/// `POST /api/admin/content/proposals`.
async fn propose(State(state): State<AppState>, signed: Signed, body: Bytes) -> ApiResult {
    need(
        &state,
        &signed,
        Permission::EditContent,
        Action::ContentProposal,
    )
    .await?;
    let b: ProposeBody = parse(&body)?;
    let (title, reason) = proposal::check_meta(&proposal::Meta {
        title: b.title,
        reason: b.reason,
    })?;
    let changes = checked(&state, &signed, Action::ContentProposal, b.changes).await?;
    let (changes, e) = evaluate(&state, changes).await?;
    if !e.valid {
        proposal::record_refusal(
            &state,
            &signed,
            Action::ContentProposal,
            "invalid",
            changes.len(),
        )
        .await;
        return Ok(proposal::invalid(&e));
    }
    proposal::open(
        &state,
        &signed,
        Kind::Content,
        &title,
        &reason,
        &changes,
        &e,
    )
    .await
}

/// `GET /api/admin/content/proposals`.
async fn list(State(state): State<AppState>, signed: Signed) -> ApiResult {
    need(
        &state,
        &signed,
        Permission::ViewContent,
        Action::ContentProposals,
    )
    .await?;
    proposal::list(&state, &signed, Kind::Content).await
}

/// `GET /api/admin/harness`.
async fn harness(State(state): State<AppState>, signed: Signed) -> ApiResult {
    need(&state, &signed, Permission::ViewContent, Action::Harness).await?;
    read_log(&signed, "/api/admin/harness");
    let root = state.0.content_root.clone();
    let today = now(&state).date_naive();
    let gateway = state.0.proposals.config.gateway_config.clone();
    let revision = state.0.proposals.config.content_revision.clone();
    let (report, models, gateway_state, revision) = blocking(move || {
        let report = policy_admin::today_report(&root, today);
        let (models, gw) = match &gateway {
            None => (None, "not_configured"),
            Some(p) => match content_admin::gateway_roles(p) {
                Ok(m) => (Some(m), "ok"),
                Err(_) => (None, "unreadable"),
            },
        };
        let revision = content_admin::revision(&root, revision.as_deref());
        (report, models, gw, revision)
    })
    .await?;
    let runtime = match signed.runtime(&state).bots(signed.handle()).await {
        Ok(list) => Some(
            list.iter()
                .filter_map(|b| b.get("name").and_then(Value::as_str).map(String::from))
                .collect::<BTreeSet<String>>(),
        ),
        Err(e) => {
            tracing::warn!(error = ?e, "the runtime's bots are unavailable");
            None
        }
    };
    let bots: Vec<Value> = report
        .bots
        .iter()
        .map(|b| {
            let role = report.roles.iter().find(|r| r.name == b.role);
            let model_role = role.map(|r| r.model);
            let model_name = model_role.map(|m| {
                serde_json::to_value(m)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default()
            });
            let gateway_model = model_name.as_ref().and_then(|m| {
                models
                    .as_ref()
                    .and_then(|ms| ms.get(m))
                    .and_then(|v| v.get("model").cloned())
            });
            let tools = role
                .map(|r| pmos_contract::bots::role_tools(r, &report.skills))
                .unwrap_or_default();
            json!({
                "name": b.name,
                "role": b.role,
                "scope": match b.scope {
                    pmos_contract::bots::BotScope::Team => "team",
                    pmos_contract::bots::BotScope::Personal => "personal",
                },
                "owner": b.owner,
                "model_role": model_name,
                "gateway_alias": model_name.as_ref().map(|m| format!("pmos/{m}")),
                "gateway_model": gateway_model,
                "skills": role.map(|r| r.skills.clone()).unwrap_or_default(),
                "tools": tools,
                "loaded_by_runtime": runtime.as_ref().map(|r| r.contains(&b.name)),
            })
        })
        .collect();
    let model_roles: Option<Vec<Value>> = models.map(|m| {
        m.into_iter()
            .map(|(name, mut v)| {
                v["role"] = json!(name);
                v
            })
            .collect()
    });
    Ok(Json(json!({
        "bots": bots,
        "model_roles": model_roles,
        "gateway_config": gateway_state,
        "runtime": {
            "reachable": runtime.is_some(),
            "bots": runtime,
        },
        "content_revision": revision,
    }))
    .into_response())
}
