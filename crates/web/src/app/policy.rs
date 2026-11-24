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
    // The branch-protection check and the host proofs (tool server calls) run beside the
    // file-system work.
    let (protection, proofs, body) = tokio::join!(
        proposal::protection(&state, &signed),
        async {
            signed
                .toolserver(&state)
                .policy_proofs(&signed.member.handle)
                .await
                .map_err(|e| tracing::warn!(error = ?e, "policy proofs unknown"))
                .ok()
        },
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
                            // What a host proof must have proved to be current (§A.5).
                            let text = cur
                                .checked_in
                                .as_ref()
                                .and_then(|c| c.get(&rel))
                                .or_else(|| g.files.get(&rel));
                            v["policy_sha256"] =
                                json!(text.map(|t| pmos_policy::prove::policy_hash(t)));
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
    with_prover(&mut body, proofs.as_ref());
    Ok(Json(body).into_response())
}

/// What Policy Admin says of the prover on validate and proposals: it never runs here.
pub const PROVER_NOT_HERE: &str =
    "The prover runs on the pull request (CI check `policy-prover`). Not run here.";

/// Add each bot's `prover` and the fixed `prover_notes` to a policy read: the newest host proof
/// (`proofs`, from the tool server; `None` when it didn't answer) against the policy's own hash.
fn with_prover(body: &mut Value, proofs: Option<&Value>) {
    let empty = Vec::new();
    let rows = proofs
        .and_then(|p| p.get("proofs"))
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let loads = proofs.and_then(|p| p.get("policy_loads"));
    if let Some(bots) = body.get_mut("bots").and_then(Value::as_array_mut) {
        for b in bots {
            let name = b.get("bot").and_then(Value::as_str).unwrap_or_default();
            let proof = rows
                .iter()
                .find(|r| r.get("bot").and_then(Value::as_str) == Some(name));
            let loaded = loads
                .and_then(|l| l.get(name))
                .and_then(Value::as_str)
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|t| t.with_timezone(&chrono::Utc));
            let sha = b.get("policy_sha256").and_then(Value::as_str);
            b["prover"] = prover_view(proofs.is_some(), proof, sha, loaded);
        }
    }
    body["prover_notes"] = json!({
        "not_proved": pmos_policy::prove::NOT_PROVED,
        "domains": pmos_policy::prove::DOMAINS,
        "view": pmos_policy::prove::VIEW_NOTE,
        "not_run_here": PROVER_NOT_HERE,
    });
}

/// One bot's Prover row (spec §A.5, PRV-10). `state` is what the screen shows:
///
/// - `proved` only for a host proof whose status is `proved`, whose `candidate_sha256` is this
///   policy's hash, and which is not older than the sandbox's last policy load;
/// - `stale` for a `proved` row that fails either test: never shown as proved;
/// - the row's own status otherwise (`exceeds`, `unsupported`, `inconclusive`, `error`,
///   `not_available`, `version_mismatch`), each a failure;
/// - `missing` when the host has recorded no proof for this bot, and `unknown` when the tool
///   server didn't answer: neither is a pass.
pub(crate) fn prover_view(
    reachable: bool,
    proof: Option<&Value>,
    policy_sha256: Option<&str>,
    last_load: Option<chrono::DateTime<chrono::Utc>>,
) -> Value {
    if !reachable {
        return json!({"state": "unknown", "passed": false});
    }
    let Some(p) = proof else {
        return json!({"state": "missing", "passed": false});
    };
    let status = p.get("status").and_then(Value::as_str).unwrap_or("error");
    let known = pmos_policy::prove::Status::parse(status);
    let at = p
        .get("at")
        .and_then(Value::as_str)
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&chrono::Utc));
    let candidate = p.get("candidate_sha256").and_then(Value::as_str);
    let stale_reason = if candidate.is_none() || candidate != policy_sha256 {
        Some("policy_changed")
    } else if matches!((at, last_load), (Some(a), Some(l)) if a < l) || at.is_none() {
        Some("policy_loaded_after")
    } else {
        None
    };
    let state = match known {
        Some(pmos_policy::prove::Status::Proved) if stale_reason.is_none() => "proved",
        Some(pmos_policy::prove::Status::Proved) => "stale",
        Some(s) => s.as_str(),
        None => "error",
    };
    json!({
        "state": state,
        "passed": state == "proved",
        "status": status,
        "stale": stale_reason.is_some(),
        "stale_reason": stale_reason,
        "at": p.get("at"),
        "reason_code": p.get("reason_code"),
        "reason": p.get("reason"),
        "counterexample": p.get("counterexample"),
        "coverage": p.get("coverage"),
        "prover_version": p.get("prover_version"),
        "host_user": p.get("host_user"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proof(status: &str, sha: &str, at: &str) -> Value {
        json!({"bot": "docs", "status": status, "candidate_sha256": sha, "at": at,
               "coverage": ["filesystem", "landlock", "network_l4", "network_rest", "process"],
               "prover_version": "0.1.9"})
    }

    #[test]
    fn only_a_current_proof_is_proved() {
        let at = "2026-11-24T10:00:00Z";
        let sha = "a".repeat(64);
        let p = proof("proved", &sha, at);
        let v = prover_view(true, Some(&p), Some(&sha), None);
        assert_eq!(v["state"], "proved");
        assert_eq!(v["passed"], true);
        // Another policy since: stale.
        let v = prover_view(true, Some(&p), Some(&"b".repeat(64)), None);
        assert_eq!(v["state"], "stale");
        assert_eq!(v["passed"], false);
        assert_eq!(v["stale_reason"], "policy_changed");
        // A policy load after the proof: stale.
        let later = "2026-11-24T11:00:00Z".parse().unwrap();
        let v = prover_view(true, Some(&p), Some(&sha), Some(later));
        assert_eq!(v["state"], "stale");
        assert_eq!(v["stale_reason"], "policy_loaded_after");
        // No hash to compare: never proved.
        let v = prover_view(true, Some(&p), None, None);
        assert_eq!(v["state"], "stale");
        // Missing, unreachable.
        assert_eq!(
            prover_view(true, None, Some(&sha), None)["state"],
            "missing"
        );
        assert_eq!(
            prover_view(false, Some(&p), Some(&sha), None)["state"],
            "unknown"
        );
        for s in [
            "exceeds",
            "unsupported",
            "inconclusive",
            "error",
            "not_available",
            "version_mismatch",
        ] {
            let v = prover_view(true, Some(&proof(s, &sha, at)), Some(&sha), None);
            assert_eq!(v["state"], s);
            assert_eq!(v["passed"], false, "{s}");
        }
        let v = prover_view(true, Some(&proof("passed", &sha, at)), Some(&sha), None);
        assert_eq!(v["state"], "error", "an unknown status is never a pass");
    }

    #[test]
    fn every_bot_gets_a_prover_row_and_the_fixed_notes() {
        let mut body = json!({"bots": [{"bot": "docs", "policy_sha256": "a".repeat(64)}, {"bot": "pm-ana", "policy_sha256": null}]});
        let proofs = json!({"proofs": [proof("proved", &"a".repeat(64), "2026-11-24T10:00:00Z")]});
        with_prover(&mut body, Some(&proofs));
        assert_eq!(body["bots"][0]["prover"]["state"], "proved");
        assert_eq!(body["bots"][1]["prover"]["state"], "missing");
        assert_eq!(
            body["prover_notes"]["not_proved"].as_array().unwrap().len(),
            6
        );
        assert_eq!(body["prover_notes"]["not_run_here"], PROVER_NOT_HERE);
        let mut body = json!({"bots": [{"bot": "docs"}]});
        with_prover(&mut body, None);
        assert_eq!(body["bots"][0]["prover"]["state"], "unknown");
    }
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
    // A sensitive admin action: a sign-in no older than 5 minutes (r7, X6 item 5).
    signed.step_up(&state)?;
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
