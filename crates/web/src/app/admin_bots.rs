//! The Admin's view of the bot marketplace (`docs/web-api.md`, "Admin: bots";
//! `docs/bot-marketplace-spec.md` §8, "The admin view", slice A1): `GET /api/admin/bots` and
//! `POST /api/admin/bots/drafts/{id}/release`.
//!
//! **Read-only but for one thing.** The view lists every bot in the catalog, offered or not,
//! with how many people have it on their team (and who), its drafts in 30 days, its unmet
//! needs, people named like it (`name_clashes`), its `PERSONA.md`'s lint status, and its
//! **held** drafts (pending drafts of a bot no longer offered). Offering or withdrawing a bot
//! is a content change by pull request (the content admin); nothing here changes the catalog
//! or anyone's team. The one write is an Admin releasing a held draft, which the tool server
//! decides and records.
//!
//! **Who:** the view needs `view-audit-log` or `manage-members` (Admin and Auditor), as the
//! tool server's `GET /internal/admin/personas` does, which checks again and records the read
//! (`admin.personas_read`). A release needs `edit-content` (an Admin), checked here first and
//! again by the tool server (`POST /internal/admin/personas/drafts/{id}/release`), which also
//! needs assertion `require`.
//!
//! **Metadata only:** keys, handles, counts, tool names and times, re-picked field by field.
//! Never a draft's title, text or arguments; never a bot's summary or examples. A bot's
//! display name is catalog content, passed on as the string the tool server sent.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use pmos_contract::bots::valid_name;
use pmos_contract::permissions::Permission;
use serde_json::{json, Map, Value};

use super::proposal::blocking;
use super::{now, valid_id, ApiError, ApiResult, AppState, Signed};
use crate::upstream::UpstreamError;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/bots", get(list))
        .route("/admin/bots/drafts/{id}/release", post(release))
}

fn forbidden(sentence: &str) -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden", sentence)
}

fn bad_reply() -> ApiError {
    ApiError::upstream(UpstreamError::Failed {
        service: "tool server",
        why: "sent a reply pmos-web can't read".into(),
    })
}

/// A key as the tool server sent it, when it is shaped like one.
fn key_of(v: &Value) -> Option<String> {
    v.as_str().filter(|k| valid_name(k)).map(String::from)
}

/// A list of handles, each shaped like one; anything else is left out.
fn handles(v: &Value) -> Value {
    match v.as_array() {
        None => Value::Null,
        Some(list) => Value::Array(
            list.iter()
                .filter_map(|h| h.as_str())
                .filter(|h| pmos_contract::model::valid_handle(h))
                .map(|h| json!(h))
                .collect(),
        ),
    }
}

fn count(v: &Value) -> Value {
    v.as_u64().map_or(Value::Null, |n| json!(n))
}

/// One held draft: its id, whose it is, what it would write, and when it was drafted.
fn held_draft(v: &Value) -> Option<Value> {
    let id = v.get("id")?.as_str().filter(|id| valid_id(id))?;
    let user = v
        .get("user")?
        .as_str()
        .filter(|h| pmos_contract::model::valid_handle(h))?;
    let tool = v.get("tool")?.as_str()?;
    let created_at = v.get("created_at").and_then(Value::as_str);
    Some(json!({
        "id": id,
        "user": user,
        "kind": crate::drafts::kind(tool),
        "tool": tool.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_')).then_some(tool),
        "created_at": created_at,
    }))
}

fn held_list(v: Option<&Value>) -> Vec<Value> {
    v.and_then(Value::as_array)
        .map(|l| l.iter().filter_map(held_draft).collect())
        .unwrap_or_default()
}

/// The lint status of each `PERSONA.md` in the checkout, by key: `{status, errors, warnings}`,
/// never a finding's message. Empty when the content can't be read.
type LintByKey = std::collections::BTreeMap<String, Value>;

async fn lint_by_key(state: &AppState) -> LintByKey {
    let root = state.0.content_root.clone();
    let dir = state.0.proposals.config.layout.content_dir().to_string();
    let today = now(state).date_naive();
    let prefix = format!("{dir}/personas/");
    blocking(move || {
        let report = crate::policy_admin::today_report(&root, today);
        let (files, _) = crate::content_admin::tree(&root, &report, &dir, false);
        files
            .into_iter()
            .filter(|f| f.kind == "persona")
            .filter_map(|f| {
                let key = f
                    .path
                    .strip_prefix(&prefix)?
                    .strip_suffix("/PERSONA.md")?
                    .to_string();
                Some((
                    key,
                    json!({
                        "status": f.lint.status,
                        "errors": f.lint.errors,
                        "warnings": f.lint.warnings,
                    }),
                ))
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// One bot of the catalog as the view shows it, or `None` when the tool server's entry isn't
/// in the expected shape.
fn bot_row(p: &Value, lint: &LintByKey, dir: &str) -> Option<Value> {
    let key = key_of(p.get("key")?)?;
    let display_name = p.get("display_name")?.as_str()?;
    let offered = p.get("offered")?.as_bool()?;
    let always_on = p.get("always_on").and_then(Value::as_bool).unwrap_or(false);
    let preview = p.get("preview").and_then(Value::as_bool).unwrap_or(false);
    let state = if !offered {
        "withdrawn"
    } else if preview {
        "preview"
    } else {
        "offered"
    };
    let needs: Vec<Value> = p
        .get("needs")
        .and_then(Value::as_array)
        .map(|l| {
            l.iter()
                .filter_map(|n| {
                    Some(json!({
                        "source": n.get("source")?.as_str()?,
                        "label": n.get("label")?.as_str()?,
                        "met": n.get("met")?.as_bool()?,
                    }))
                })
                .collect()
        })
        .unwrap_or_default();
    let mut out = Map::new();
    out.insert("key".into(), json!(key));
    out.insert("display_name".into(), json!(display_name));
    out.insert(
        "mascot".into(),
        p.get("mascot")
            .and_then(key_of)
            .map_or(Value::Null, |m| json!(m)),
    );
    out.insert("state".into(), json!(state));
    out.insert("offered".into(), json!(offered));
    out.insert("always_on".into(), json!(always_on));
    out.insert("preview".into(), json!(preview));
    out.insert("users".into(), p.get("users").map_or(Value::Null, count));
    out.insert(
        "handles".into(),
        p.get("handles").map_or(Value::Null, handles),
    );
    out.insert("needs".into(), json!(needs));
    out.insert(
        "drafts_30d".into(),
        json!(p.get("drafts_30d").and_then(Value::as_u64).unwrap_or(0)),
    );
    out.insert(
        "name_clashes".into(),
        p.get("name_clashes").map_or(json!([]), handles),
    );
    out.insert("held".into(), json!(held_list(p.get("held"))));
    out.insert(
        "path".into(),
        json!(format!("{dir}/personas/{key}/PERSONA.md")),
    );
    out.insert(
        "lint".into(),
        lint.get(&key).cloned().unwrap_or(Value::Null),
    );
    Some(Value::Object(out))
}

/// A key no longer in the catalog that team rows or held drafts still name.
fn unknown_row(u: &Value) -> Option<Value> {
    let key = key_of(u.get("key")?)?;
    Some(json!({
        "key": key,
        "users": u.get("users").map_or(json!(0), count),
        "handles": u.get("handles").map_or(json!([]), handles),
        "drafts_30d": u.get("drafts_30d").and_then(Value::as_u64).unwrap_or(0),
        "held": held_list(u.get("held")),
    }))
}

/// `GET /api/admin/bots`.
async fn list(State(state): State<AppState>, signed: Signed) -> ApiResult {
    let may = |p| signed.member.allows(p);
    if !(may(Permission::ViewAuditLog) || may(Permission::ManageMembers)) {
        return Err(forbidden("Only an admin or an auditor can see this."));
    }
    tracing::info!(user = %signed.member.handle, route = "/api/admin/bots", "admin read");
    let tools = signed.toolserver(&state);
    let (read, lint) = tokio::join!(
        tools.admin(&signed.member.handle, "personas", &[]),
        lint_by_key(&state)
    );
    let v = read.map_err(|e| match e {
        UpstreamError::Refused {
            status: 403,
            code: Some(code),
            ..
        } if code == "forbidden" => forbidden("Only an admin or an auditor can see this."),
        other => ApiError::upstream(other),
    })?;
    let dir = state.0.proposals.config.layout.content_dir().to_string();
    let list = v
        .get("personas")
        .and_then(Value::as_array)
        .ok_or_else(bad_reply)?;
    let mut bots = Vec::new();
    for p in list {
        match bot_row(p, &lint, &dir) {
            Some(row) => bots.push(row),
            None => tracing::warn!(
                "the tool server listed a bot not in the admin view's shape; left out"
            ),
        }
    }
    let unknown: Vec<Value> = v
        .get("unknown")
        .and_then(Value::as_array)
        .map(|l| l.iter().filter_map(unknown_row).collect())
        .unwrap_or_default();
    let shown: usize = bots
        .iter()
        .chain(unknown.iter())
        .map(|b| b["held"].as_array().map_or(0, Vec::len))
        .sum();
    let held_total = v
        .get("held_total")
        .and_then(Value::as_u64)
        .map_or(shown as u64, |n| n.max(shown as u64));
    Ok(Json(json!({
        "bots": bots,
        "unknown": unknown,
        "held_total": held_total,
        "can_release": signed.member.allows(Permission::EditContent),
        "can_edit_content": signed.member.allows(Permission::EditContent),
    }))
    .into_response())
}

/// `POST /api/admin/bots/drafts/{id}/release`: an Admin releases a held draft, so its owner
/// can approve or edit it again under their own bot's rules. Answers `{id, released: true}`;
/// never the draft.
async fn release(
    State(state): State<AppState>,
    signed: Signed,
    Path(id): Path<String>,
) -> ApiResult {
    if !signed.member.allows(Permission::EditContent) {
        return Err(forbidden("Only an admin can release a held draft."));
    }
    if !valid_id(&id) || crate::drafts::is_practice_id(&id) {
        return Err(ApiError::not_found());
    }
    let (status, body) = signed
        .toolserver(&state)
        .admin_users(
            &signed.member.handle,
            reqwest::Method::POST,
            &["personas", "drafts", &id, "release"],
            None,
        )
        .await
        .map_err(ApiError::upstream)?;
    let code = body.get("code").and_then(Value::as_str);
    tracing::info!(user = %signed.member.handle, draft = %id, status, code = code.unwrap_or("none"), "held draft release");
    match (status, code) {
        (200, _) => Ok(Json(json!({ "id": id, "released": true })).into_response()),
        (403, Some("own_draft")) => Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "release_own_draft",
            "This is your own draft, so another Admin releases it. Nothing changed.",
        )),
        (403, _) => Err(forbidden(
            "The tool server doesn't count you as an admin who may release drafts. Nothing changed.",
        )),
        (404, _) => Err(ApiError::not_found()),
        (409, Some("not_held")) => Err(ApiError::new(
            StatusCode::CONFLICT,
            "not_held",
            "This draft isn't on hold any more: it was decided, its bot is offered again, or \
             someone already released it. Nothing changed.",
        )),
        (503, Some("assertion_required")) => Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "assertion_required",
            "Releasing a draft needs signed user assertions, and this tool server runs without \
             them. Nothing changed.",
        )),
        (503, _) => Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "PM OS can't release this right now. Nothing changed; try again in a minute.",
        )),
        (s, _) => Err(ApiError::upstream(UpstreamError::Refused {
            service: "tool server",
            status: s,
            code: None,
            error: None,
            audience: None,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_draft_is_metadata_only() {
        let row = held_draft(&json!({
            "id": "apr_1", "user": "ana", "tool": "confluence.create_draft",
            "created_at": "2026-11-20T09:00:00Z", "args": {"body": "secret"}, "title": "t"
        }))
        .unwrap();
        assert_eq!(
            row,
            json!({"id": "apr_1", "user": "ana", "kind": "confluence_page",
                   "tool": "confluence.create_draft", "created_at": "2026-11-20T09:00:00Z"})
        );
        assert!(held_draft(&json!({"id": "bad id", "user": "ana", "tool": "x"})).is_none());
        assert!(held_draft(&json!({"id": "apr_1", "user": "<b>", "tool": "x"})).is_none());
    }

    #[test]
    fn a_bots_state_follows_offered_and_preview() {
        let lint = LintByKey::new();
        let row = |offered, preview| {
            bot_row(
                &json!({"key": "chief", "display_name": "Chief", "offered": offered,
                        "preview": preview, "users": 2, "handles": ["ana", "<x>"]}),
                &lint,
                "content",
            )
            .unwrap()
        };
        assert_eq!(row(true, false)["state"], "offered");
        assert_eq!(row(true, true)["state"], "preview");
        assert_eq!(row(false, true)["state"], "withdrawn");
        assert_eq!(row(true, false)["handles"], json!(["ana"]));
        assert_eq!(
            row(true, false)["path"],
            "content/personas/chief/PERSONA.md"
        );
        assert!(bot_row(
            &json!({"key": "Bad Key", "display_name": "x", "offered": true}),
            &lint,
            "content"
        )
        .is_none());
    }
}
