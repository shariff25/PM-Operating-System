//! The Policy Admin: `GET /api/admin/policy`, `POST /api/admin/policy/validate`,
//! `POST /api/admin/policy/proposals` and `GET /api/admin/policy/proposals`
//! (`docs/web-api.md`, Admin: policy; `docs/policy-admin.md`).
//!
//! Read-and-propose: nothing here writes a policy or content file. A change is sent to the
//! tool server, which opens it as a GitHub pull request with the admin's own linked account
//! ([`super::proposal`]). Reading needs `view-policies` (admins and auditors); validating and
//! proposing need `edit-policies` (admins), since a validation's diff shows the current text
//! of the files it changes. Role refusals are recorded at the tool server.

use std::path::PathBuf;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use pmos_contract::permissions::Permission;
use serde::Deserialize;
use serde_json::{json, Value};

use super::proposal::{self, blocking, need, read_log, Action, MAX_REQUEST};
use super::{now, parse, ApiError, ApiResult, AppState, Signed};
use crate::content_admin;
use crate::policy_admin::{self, Roots, SpecPatch};
use crate::proposals::{Issue, Kind};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/policy", get(read))
        .route("/admin/policy/validate", post(validate))
        .route("/admin/policy/proposals", post(propose).get(list))
        .layer(DefaultBodyLimit::max(MAX_REQUEST))
}

/// Where the server reads from.
struct Where {
    content: PathBuf,
    policies: Option<PathBuf>,
    content_dir: String,
    policies_dir: String,
}

fn where_(state: &AppState) -> Where {
    let cfg = &state.0.proposals.config;
    Where {
        content: state.0.content_root.clone(),
        policies: cfg.policies_root.clone(),
        content_dir: cfg.layout.content_dir().to_string(),
        policies_dir: cfg.layout.policies_dir().to_string(),
    }
}

/// `GET /api/admin/policy`.
async fn read(State(state): State<AppState>, signed: Signed) -> ApiResult {
    need(
        &state,
        &signed,
        Permission::ViewPolicies,
        Action::PolicyRead,
    )
    .await?;
    read_log(&signed, "/api/admin/policy");
    let w = where_(&state);
    let today = now(&state).date_naive();
    let revision = state.0.proposals.config.content_revision.clone();
    let edit = signed.member.allows(Permission::EditContent);
    // The branch-protection check (a tool server call) runs beside the file-system work.
    let (protection, body) = tokio::join!(
        proposal::protection(&state, &signed),
        blocking(move || {
            let cur = policy_admin::current(&w.content, w.policies.as_deref(), today);
            let spec = policy_admin::current_spec(&cur.report);
            let generated = cur.generated.as_ref().ok();
            let files = policy_admin::files(generated, cur.checked_in.as_ref(), &w.policies_dir);
            let now_files = policy_admin::now_files(&cur);
            let allowances = policy_admin::allowances_of(&now_files);
            let mut generate_errors: Vec<Issue> = match &cur.generated {
                Ok(_) => Vec::new(),
                Err(errors) => errors
                    .iter()
                    .map(|d| Issue::from_lint(d, &w.content_dir))
                    .collect(),
            };
            // An auditor never sees a finding's message on a file they can't open.
            proposal::redact(&mut generate_errors, &w.content, &w.content_dir, edit);
            let violations = match (generated, &cur.checked_in) {
                (Some(g), Some(c)) => policy_admin::rule_violations(g, c, &w.policies_dir),
                _ => Vec::new(),
            };
            let in_sync = cur.checked_in.as_ref().map(|_| {
                files.iter().all(|f| f.state == "same")
                    && violations.is_empty()
                    && generated.is_some()
            });
            let bots: Vec<Value> = generated
                .map(|g| {
                    g.bots
                        .iter()
                        .map(|ctx| {
                            let mut v = policy_admin::bot_facts(ctx);
                            let rel = format!("bots/{}.yaml", ctx.bot);
                            let spec_rel = format!("bots/{}.sandbox.yaml", ctx.bot);
                            v["policy_file"] =
                                json!(crate::proposals::repo_path(&w.policies_dir, &rel));
                            v["launch_spec_file"] =
                                json!(crate::proposals::repo_path(&w.policies_dir, &spec_rel));
                            v["allowances"] = json!(allowances.get(&ctx.bot));
                            v["allowances_from"] = json!(if cur.checked_in.is_some() {
                                "checked_in"
                            } else {
                                "generated"
                            });
                            v
                        })
                        .collect()
                })
                .unwrap_or_default();
            json!({
                "revision": content_admin::revision(&w.content, revision.as_deref()),
                "spec": spec,
                "spec_sources": {
                    "bots": crate::proposals::repo_path(&w.content_dir, "bots/<bot>/BOT.md"),
                    "roles": crate::proposals::repo_path(&w.content_dir, "roles/<role>/ROLE.md"),
                    "skills": crate::proposals::repo_path(&w.content_dir, "skills/<skill>/SKILL.md"),
                },
                "fixed": policy_admin::fixed(),
                "bots": bots,
                "files": files,
                "drift": {
                    "checked_in_available": cur.checked_in.is_some(),
                    "in_sync": in_sync,
                    "violations": violations,
                },
                "generate_errors": generate_errors,
            })
        })
    );
    let (protection, configured) = protection;
    let mut body = body?;
    body["proposals_configured"] = json!(configured);
    body["protection"] = protection;
    Ok(Json(body).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidateBody {
    spec: SpecPatch,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposeBody {
    spec: SpecPatch,
    title: String,
    reason: String,
}

/// Validate `spec` against the deployed content: the content changes it makes, then the
/// whole evaluation. `Err` holds spec-level issues (nothing to evaluate).
async fn evaluate(
    state: &AppState,
    spec: SpecPatch,
) -> Result<Result<(Vec<crate::proposals::Change>, policy_admin::Evaluation), Vec<Issue>>, ApiError>
{
    let w = where_(state);
    let today = now(state).date_naive();
    blocking(move || {
        let report = policy_admin::today_report(&w.content, today);
        let changes = policy_admin::spec_changes(&w.content, &report, &spec)?;
        let roots = Roots {
            content: &w.content,
            policies: w.policies.as_deref(),
            content_dir: &w.content_dir,
            policies_dir: &w.policies_dir,
        };
        let e = policy_admin::evaluate(&roots, &changes, today).map_err(|i| vec![i])?;
        Ok((changes, e))
    })
    .await
}

/// The read rule on the bot, role and skill files a spec change shows the text of.
async fn guard(
    state: &AppState,
    signed: &Signed,
    action: Action,
    changes: &[crate::proposals::Change],
) -> Result<(), super::ApiError> {
    let root = state.0.content_root.clone();
    let rels: Vec<String> = changes.iter().map(|c| c.rel.clone()).collect();
    let named = blocking(move || {
        rels.into_iter()
            .map(|rel| {
                let text = crate::proposals::read_text(&root, &rel).ok().flatten();
                (rel, text)
            })
            .collect::<Vec<_>>()
    })
    .await?;
    proposal::guard_reads(state, signed, Permission::EditPolicies, action, &named).await
}

fn spec_invalid(issues: &[Issue]) -> axum::response::Response {
    proposal::coded(
        axum::http::StatusCode::UNPROCESSABLE_ENTITY,
        "invalid",
        "This spec doesn't pass validation; nothing was proposed.",
        json!({"errors": issues, "warnings": []}),
    )
}

/// `POST /api/admin/policy/validate`: `200` with the validation (whether or not it is valid).
async fn validate(State(state): State<AppState>, signed: Signed, body: Bytes) -> ApiResult {
    need(
        &state,
        &signed,
        Permission::EditPolicies,
        Action::PolicyValidate,
    )
    .await?;
    let b: ValidateBody = parse(&body)?;
    read_log(&signed, "/api/admin/policy/validate");
    let out = match evaluate(&state, b.spec).await? {
        Ok((changes, e)) => {
            guard(&state, &signed, Action::PolicyValidate, &changes).await?;
            proposal::evaluation_json(&e)
        }
        Err(issues) => json!({
            "valid": false,
            "errors": issues,
            "warnings": [],
            "content_changes": [],
            "policy_changes": [],
            "allowance_changes": [],
            "widening": false,
            "summary": {"wider": 0, "narrower": 0, "changed": 0},
        }),
    };
    Ok(Json(out).into_response())
}

/// `POST /api/admin/policy/proposals`: `201` with the pull request.
async fn propose(State(state): State<AppState>, signed: Signed, body: Bytes) -> ApiResult {
    need(
        &state,
        &signed,
        Permission::EditPolicies,
        Action::PolicyProposal,
    )
    .await?;
    let b: ProposeBody = parse(&body)?;
    let (title, reason) = proposal::check_meta(&proposal::Meta {
        title: b.title,
        reason: b.reason,
    })?;
    let (changes, e) = match evaluate(&state, b.spec).await? {
        Ok(v) => v,
        Err(issues) => {
            proposal::record_refusal(&state, &signed, Action::PolicyProposal, "invalid", 0).await;
            return Ok(spec_invalid(&issues));
        }
    };
    guard(&state, &signed, Action::PolicyProposal, &changes).await?;
    if !e.valid {
        proposal::record_refusal(
            &state,
            &signed,
            Action::PolicyProposal,
            "invalid",
            changes.len(),
        )
        .await;
        return Ok(proposal::invalid(&e));
    }
    if changes.is_empty() && e.policy_writes.is_empty() {
        return Err(ApiError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "no_change",
            "This spec is the same as the deployed one: there's nothing to propose.",
        ));
    }
    proposal::open(&state, &signed, Kind::Policy, &title, &reason, &changes, &e).await
}

/// `GET /api/admin/policy/proposals`.
async fn list(State(state): State<AppState>, signed: Signed) -> ApiResult {
    need(
        &state,
        &signed,
        Permission::ViewPolicies,
        Action::PolicyProposals,
    )
    .await?;
    proposal::list(&state, &signed, Kind::Policy).await
}
