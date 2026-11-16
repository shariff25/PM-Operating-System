//! `POST /internal/audit`: other services (the web console, bots, gateway, Slack bot) report
//! their security events into the tool server's hash-chained audit log.
//!
//! The route sits behind the same service-token check as the rest of `/internal`. The body is
//! a strict schema (`deny_unknown_fields`, at most [`MAX_BODY_BYTES`]), and nothing in it can
//! carry prose:
//! - every string is a short token (letters, digits and `_ . : @ / = + -`, no whitespace) or a
//!   64-character lowercase hex digest;
//! - `action` is a dotted verb in an allowed namespace; `reason_code` a snake_case code;
//! - there is no `detail`, `message`, `sources`, `tool` or timestamp field. The server stamps
//!   `at`, and the transport `client` comes from the token, never the body.
//!
//! The event is recorded before the call answers; if it can't be, the answer is `503`, so a
//! caller that must fail closed (a role change, a policy edit) refuses to proceed.
//! The Slack bot's token may only report `source` `slack` on channel `slack`.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Json, Router};
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;

use crate::audit::{AuditSink, Client, Outcome};
use crate::audit_chain::{
    Actor, ActorType, AuditEnvelope, Channel, RequestContext, Source, Target,
};

/// The most bytes one event may take. Real events are well under 1 KiB.
pub const MAX_BODY_BYTES: usize = 4 * 1024;

/// Action namespaces a caller may report. `audit.prune` and other system actions are not here:
/// only the tool server writes those.
pub const ALLOWED_NAMESPACES: &[&str] = &[
    "auth", "authz", "role", "policy", "connector", "approval", "member", "export", "audit",
    "routine", "bot", "tool",
];

/// The route, for merging into the `/internal` router (behind its client check).
pub fn router<S: Clone + Send + Sync + 'static>(sink: Arc<dyn AuditSink>) -> Router<S> {
    Router::new()
        .route("/audit", post(ingest))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(sink)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngestActor {
    #[serde(rename = "type")]
    pub kind: ActorType,
    pub handle: String,
    #[serde(default)]
    pub idp_oid: Option<String>,
    #[serde(default)]
    pub idp_tid: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngestRequest {
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub source_ip_hash: Option<String>,
    #[serde(default)]
    pub user_agent_hash: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngestTarget {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngestEvent {
    pub source: Source,
    pub actor: IngestActor,
    pub channel: Channel,
    pub action: String,
    #[serde(default)]
    pub target: Option<IngestTarget>,
    pub outcome: Outcome,
    #[serde(default)]
    pub reason_code: Option<String>,
    #[serde(default)]
    pub request: Option<IngestRequest>,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub args_hash: Option<String>,
    #[serde(default)]
    pub before_hash: Option<String>,
    #[serde(default)]
    pub after_hash: Option<String>,
    #[serde(default)]
    pub output_ref: Option<String>,
}

/// A field that failed validation, and why.
#[derive(Debug, PartialEq, Eq)]
pub struct Invalid {
    pub field: &'static str,
    pub reason: &'static str,
}

type Check<T> = Result<T, Invalid>;

fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '@' | '/' | '=' | '+' | '-')
}

/// An id or handle: short, no whitespace or control characters.
fn token(field: &'static str, s: &str, max: usize) -> Check<String> {
    if s.is_empty() || s.len() > max {
        return Err(Invalid {
            field,
            reason: "empty or too long",
        });
    }
    if !s.chars().all(token_char) {
        return Err(Invalid {
            field,
            reason: "only letters, digits and _ . : @ / = + - are allowed",
        });
    }
    Ok(s.to_string())
}

fn opt_token(field: &'static str, s: &Option<String>, max: usize) -> Check<Option<String>> {
    s.as_deref().map(|s| token(field, s, max)).transpose()
}

fn digest(field: &'static str, s: &Option<String>) -> Check<Option<String>> {
    match s {
        None => Ok(None),
        Some(s) if s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) => {
            Ok(Some(s.clone()))
        }
        Some(_) => Err(Invalid {
            field,
            reason: "must be a 64-character lowercase hex digest",
        }),
    }
}

fn snake(field: &'static str, s: &str, max: usize) -> Check<String> {
    let mut chars = s.chars();
    let ok = s.len() <= max
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if ok {
        Ok(s.to_string())
    } else {
        Err(Invalid {
            field,
            reason: "must be a snake_case code",
        })
    }
}

fn action(s: &str) -> Check<String> {
    let parts: Vec<&str> = s.split('.').collect();
    let shape = (2..=3).contains(&parts.len())
        && parts.iter().all(|p| snake("action", p, 32).is_ok())
        && ALLOWED_NAMESPACES.contains(&parts[0]);
    if shape && s.len() <= 64 {
        Ok(s.to_string())
    } else {
        Err(Invalid {
            field: "action",
            reason: "must be a dotted verb in an allowed namespace, such as `auth.sign_in`",
        })
    }
}

impl IngestEvent {
    /// Check every field and build the envelope. `client` is the caller's token: it fixes the
    /// transport client and, for the Slack bot, the source and channel.
    pub fn into_envelope(self, client: Client) -> Check<AuditEnvelope> {
        if client == Client::Bot && (self.source != Source::Slack || self.channel != Channel::Slack)
        {
            return Err(Invalid {
                field: "source",
                reason: "this token may only report source and channel `slack`",
            });
        }
        if self.source == Source::Toolserver {
            return Err(Invalid {
                field: "source",
                reason: "`toolserver` events are written by the tool server itself",
            });
        }
        let request = self.request.unwrap_or(IngestRequest {
            request_id: None,
            source_ip_hash: None,
            user_agent_hash: None,
        });
        Ok(AuditEnvelope {
            at: Utc::now(),
            source: self.source,
            actor: Actor {
                kind: self.actor.kind,
                handle: token("actor.handle", &self.actor.handle, 64)?,
                idp_oid: opt_token("actor.idp_oid", &self.actor.idp_oid, 64)?,
                idp_tid: opt_token("actor.idp_tid", &self.actor.idp_tid, 64)?,
                session_id: opt_token("actor.session_id", &self.actor.session_id, 128)?,
                client,
            },
            channel: self.channel,
            action: action(&self.action)?,
            target: self
                .target
                .map(|t| {
                    Ok(Target {
                        kind: snake("target.type", &t.kind, 32)?,
                        id: token("target.id", &t.id, 128)?,
                    })
                })
                .transpose()?,
            outcome: self.outcome,
            reason_code: self
                .reason_code
                .as_deref()
                .map(|c| snake("reason_code", c, 64))
                .transpose()?,
            request: RequestContext {
                request_id: opt_token("request.request_id", &request.request_id, 128)?,
                source_ip_hash: digest("request.source_ip_hash", &request.source_ip_hash)?,
                user_agent_hash: digest("request.user_agent_hash", &request.user_agent_hash)?,
            },
            run_id: opt_token("run_id", &self.run_id, 128)?,
            skill: None,
            bot: None,
            tool: None,
            sources: Vec::new(),
            args_hash: digest("args_hash", &self.args_hash)?,
            before_hash: digest("before_hash", &self.before_hash)?,
            after_hash: digest("after_hash", &self.after_hash)?,
            output_ref: opt_token("output_ref", &self.output_ref, 128)?,
            detail: None,
        })
    }
}

fn reject(status: StatusCode, message: String, field: Option<&str>) -> Response {
    (status, Json(json!({ "error": message, "field": field }))).into_response()
}

async fn ingest(
    axum::extract::State(sink): axum::extract::State<Arc<dyn AuditSink>>,
    Extension(client): Extension<Client>,
    body: Bytes,
) -> Response {
    let event: IngestEvent = match serde_json::from_slice(&body) {
        Ok(event) => event,
        Err(e) => {
            return reject(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("invalid audit event: {e}"),
                None,
            )
        }
    };
    let envelope = match event.into_envelope(client) {
        Ok(env) => env,
        Err(Invalid { field, reason }) => {
            return reject(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("invalid `{field}`: {reason}"),
                Some(field),
            )
        }
    };
    match sink.record_envelope(envelope).await {
        Ok(rec) => (
            StatusCode::CREATED,
            Json(json!({ "seq": rec.seq, "hash": rec.hash })),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "AUDIT EVENT NOT RECORDED (POST /internal/audit)");
            reject(
                StatusCode::SERVICE_UNAVAILABLE,
                "the audit event could not be recorded".into(),
                None,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn parse(v: Value) -> Result<IngestEvent, serde_json::Error> {
        serde_json::from_value(v)
    }

    fn good() -> Value {
        json!({
            "source": "console",
            "actor": {"type": "human", "handle": "ana", "idp_oid": "00000000-0000-0000-0000-000000000001", "session_id": "sid_abc"},
            "channel": "web",
            "action": "role.grant",
            "target": {"type": "member", "id": "ben"},
            "outcome": "ok",
            "reason_code": "admin_request",
            "request": {"request_id": "req-1", "source_ip_hash": "a".repeat(64)},
            "before_hash": "b".repeat(64),
            "after_hash": "c".repeat(64)
        })
    }

    fn check(v: Value, client: Client) -> Result<AuditEnvelope, Invalid> {
        parse(v).expect("schema").into_envelope(client)
    }

    #[test]
    fn a_complete_event_is_accepted_and_stamped_with_the_token_client() {
        let env = check(good(), Client::Orchestrator).unwrap();
        assert_eq!(env.actor.client, Client::Orchestrator);
        assert_eq!(env.action, "role.grant");
        assert!(env.detail.is_none() && env.sources.is_empty());
    }

    #[test]
    fn unknown_fields_are_refused() {
        for extra in ["detail", "message", "text", "tool", "at", "client", "sources"] {
            let mut v = good();
            v[extra] = json!("hello");
            assert!(parse(v).is_err(), "{extra} must be refused");
        }
        let mut v = good();
        v["actor"]["email"] = json!("a@b.c");
        assert!(parse(v).is_err());
        let mut v = good();
        v["request"]["note"] = json!("x");
        assert!(parse(v).is_err());
    }

    #[test]
    fn free_text_is_refused_in_every_string_field() {
        let prose = "the user pasted this sentence";
        for (path, field) in [
            ("/actor/handle", "actor.handle"),
            ("/actor/session_id", "actor.session_id"),
            ("/target/id", "target.id"),
            ("/target/type", "target.type"),
            ("/reason_code", "reason_code"),
            ("/request/request_id", "request.request_id"),
            ("/run_id", "run_id"),
            ("/output_ref", "output_ref"),
            ("/action", "action"),
            ("/before_hash", "before_hash"),
        ] {
            let mut v = good();
            let parts: Vec<&str> = path[1..].split('/').collect();
            match parts.as_slice() {
                [k] => v[*k] = json!(prose),
                [a, b] => v[*a][*b] = json!(prose),
                _ => unreachable!(),
            }
            let err = check(v, Client::Orchestrator).unwrap_err();
            assert_eq!(err.field, field, "{path}");
        }
    }

    #[test]
    fn length_and_shape_limits() {
        let mut v = good();
        v["actor"]["handle"] = json!("x".repeat(65));
        assert_eq!(check(v, Client::Orchestrator).unwrap_err().field, "actor.handle");
        let mut v = good();
        v["request"]["source_ip_hash"] = json!("203.0.113.7");
        assert_eq!(
            check(v, Client::Orchestrator).unwrap_err().field,
            "request.source_ip_hash"
        );
        let mut v = good();
        v["before_hash"] = json!("A".repeat(64));
        assert!(check(v, Client::Orchestrator).is_err(), "uppercase hex");
        for bad in ["root.grant", "audit", "role.Grant", "audit.prune.x.y", "role."] {
            let mut v = good();
            v["action"] = json!(bad);
            assert!(check(v, Client::Orchestrator).is_err(), "{bad}");
        }
    }

    #[test]
    fn enums_are_closed() {
        for (k, bad) in [("channel", "email"), ("outcome", "maybe"), ("source", "other")] {
            let mut v = good();
            v[k] = json!(bad);
            assert!(parse(v).is_err(), "{k}");
        }
        let mut v = good();
        v["actor"]["type"] = json!("alien");
        assert!(parse(v).is_err());
    }

    #[test]
    fn the_slack_bot_token_reports_slack_only() {
        assert!(check(good(), Client::Bot).is_err());
        let mut v = good();
        v["source"] = json!("slack");
        v["channel"] = json!("slack");
        v["action"] = json!("approval.decide");
        assert!(check(v, Client::Bot).is_ok());
        let mut v = good();
        v["source"] = json!("toolserver");
        assert!(check(v, Client::Orchestrator).is_err());
    }
}
