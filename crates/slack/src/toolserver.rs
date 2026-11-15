//! The tool server's internal API, as the bot uses it: a short session for the PM and the
//! skill, one tool call, then the session is revoked. The tool server applies the skill's
//! allowlist and audits the call, exactly as for an agent.
//!
//! Approval cards use the same token to read one inbox item and record the PM's decision
//! (spec §2): `decided_by` is the clicking PM's handle, and the tool server checks it.
//!
//! The triage digest reads the feedback ledger (`GET /internal/ledger`,
//! `docs/improvement-loop-spec.md` §1) with it too.
//!
//! Onboarding (`docs/onboarding-spec.md` §3) uses the same token: access requests and their
//! decisions, removing a member, the setup checklist's state, OAuth start links and the
//! memory repo. Those answers carry a `code` the bot turns into a sentence.
//!
//! The bot has its own token (`PMOS_BOT_TOKEN`), which the tool server limits to sessions
//! for the bot's skills. Session tokens go in the `X-Pmos-Session` header, never in a URL.

use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{json, Value};

/// The header that carries a session token on the tool server's `/internal` API.
const SESSION_HEADER: &str = "x-pmos-session";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CallError {
    #[error("PM OS is unavailable right now")]
    Unavailable,
    #[error("{0}")]
    Refused(String),
}

/// A refusal from the onboarding endpoints, with its status and `code`.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ApiError {
    #[error("PM OS is unavailable right now")]
    Unavailable,
    #[error("{error}")]
    Refused {
        status: u16,
        /// The answer's `code`, such as `github_not_linked`; empty if it had none.
        code: String,
        /// The answer's `error`, for a person; may be empty.
        error: String,
        /// The whole answer, for the `request` some refusals carry.
        body: Value,
    },
}

impl ApiError {
    pub fn code(&self) -> Option<&str> {
        match self {
            ApiError::Refused { code, .. } if !code.is_empty() => Some(code),
            _ => None,
        }
    }

    pub fn status(&self) -> Option<u16> {
        match self {
            ApiError::Refused { status, .. } => Some(*status),
            ApiError::Unavailable => None,
        }
    }
}

/// The OAuth providers the setup checklist links.
pub const PROVIDERS: [&str; 3] = ["atlassian", "slack", "github"];

/// A request object, from an answer that wraps it (`{request}`) or is it.
pub fn request_of(body: &Value) -> &Value {
    match body.get("request") {
        Some(r) if r.is_object() => r,
        _ => body,
    }
}

#[derive(Clone)]
pub struct ToolServer {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl ToolServer {
    pub fn new(base: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            token: token.into(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .expect("http client builds"),
        }
    }

    async fn post(
        &self,
        path: &str,
        session: Option<&str>,
        body: Value,
    ) -> Result<Value, CallError> {
        let mut req = self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .json(&body);
        if let Some(token) = session {
            req = req.header(SESSION_HEADER, token);
        }
        Self::finish(req).await
    }

    async fn finish(req: reqwest::RequestBuilder) -> Result<Value, CallError> {
        let res = req.send().await.map_err(|e| {
            tracing::warn!(error = %e, "tool server unreachable");
            CallError::Unavailable
        })?;
        let status = res.status();
        let json: Value = res.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            Ok(json)
        } else if status.is_server_error() && status.as_u16() != 502 {
            tracing::warn!(%status, "tool server error");
            Err(CallError::Unavailable)
        } else {
            Err(CallError::Refused(
                json["error"].as_str().unwrap_or("refused").to_string(),
            ))
        }
    }

    /// Run `tool` once as `user` under `skill`, in a session that ends with the call.
    pub async fn call_once(
        &self,
        user: &str,
        skill: &str,
        run_id: &str,
        tool: &str,
        args: Value,
    ) -> Result<Value, CallError> {
        let session = self
            .post(
                "/internal/sessions",
                None,
                json!({"user": user, "skill": skill, "run_id": run_id, "ttl_minutes": 5}),
            )
            .await?;
        let token = session["token"]
            .as_str()
            .ok_or(CallError::Unavailable)?
            .to_string();
        let result = self
            .post(
                "/internal/call",
                Some(&token),
                json!({"tool": tool, "args": args}),
            )
            .await;
        // End the session either way; it was for this one call.
        let _ = self
            .http
            .delete(format!("{}/internal/session", self.base))
            .bearer_auth(&self.token)
            .header(SESSION_HEADER, &token)
            .send()
            .await;
        Ok(result?["result"].clone())
    }

    /// `GET /internal/approvals/{id}`: one inbox item.
    pub async fn approval(&self, id: &str) -> Result<Value, CallError> {
        if !approval_id_ok(id) {
            return Err(CallError::Refused("not an approval id".into()));
        }
        Self::finish(
            self.http
                .get(format!("{}/internal/approvals/{id}", self.base))
                .bearer_auth(&self.token)
                .timeout(Duration::from_secs(5)),
        )
        .await
    }

    /// `POST /internal/approvals/{id}/decision`. `decision` is `approve`, `edit` (with `args`)
    /// or `reject` (with an optional `reason`, one of [`crate::cards::REASONS`]; the tool
    /// server takes a reason only with `reject`). `confirm_external` is the second
    /// confirmation for a Slack write to an externally shared channel
    /// (`docs/slack-reply-spec.md` §3); it is sent only when `true`. Returns the item as
    /// decided; an approved write has run by then.
    ///
    /// A refusal keeps its status, `code` and body: the reply card turns `audience_blocked`,
    /// `needs_external_confirm` and `expired` (`409`) and `audience_unavailable` (`503`) into
    /// their own sentences. Any other `5xx`, or no answer, is [`ApiError::Unavailable`].
    pub async fn decide(
        &self,
        id: &str,
        decided_by: &str,
        decision: &str,
        args: Option<Value>,
        reason: Option<&str>,
        confirm_external: bool,
    ) -> Result<Value, ApiError> {
        if !approval_id_ok(id) {
            return Err(refused_locally("not an approval id"));
        }
        let mut body = json!({"decided_by": decided_by, "decision": decision});
        if let Some(args) = args {
            body["args"] = args;
        }
        if let Some(reason) = reason {
            body["reason"] = json!(reason);
        }
        if confirm_external {
            body["confirm_external"] = json!(true);
        }
        let res = self
            .http
            .post(format!("{}/internal/approvals/{id}/decision", self.base))
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(60))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "tool server unreachable");
                ApiError::Unavailable
            })?;
        let status = res.status();
        let json: Value = res.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(json);
        }
        let code = json["code"].as_str().unwrap_or_default().to_string();
        if status.is_server_error() && status.as_u16() != 502 && code.is_empty() {
            tracing::warn!(%status, "tool server error");
            return Err(ApiError::Unavailable);
        }
        Err(ApiError::Refused {
            status: status.as_u16(),
            code,
            error: json["error"].as_str().unwrap_or("refused").to_string(),
            body: json,
        })
    }

    /// `GET /internal/ledger`: the feedback ledger's rows (labels and metadata, never message
    /// text), newest first.
    pub async fn ledger(&self, query: &LedgerQuery) -> Result<Vec<Value>, CallError> {
        let mut params: Vec<(&str, String)> = vec![
            ("since", rfc3339(query.since)),
            ("until", rfc3339(query.until)),
        ];
        if let Some(bot) = &query.bot {
            params.push(("bot", bot.clone()));
        }
        if !query.outcomes.is_empty() {
            params.push(("outcome", query.outcomes.join(",")));
        }
        if let Some(min) = query.min_edit {
            params.push(("min_edit", min.to_string()));
        }
        params.push(("limit", query.limit.clamp(1, LEDGER_LIMIT).to_string()));
        let body = Self::finish(
            self.http
                .get(format!("{}/internal/ledger", self.base))
                .bearer_auth(&self.token)
                .timeout(Duration::from_secs(20))
                .query(&params),
        )
        .await?;
        Ok(body["rows"].as_array().cloned().unwrap_or_default())
    }
}

/// The most rows `GET /internal/ledger` returns at once.
pub const LEDGER_LIMIT: usize = 1000;

/// A `GET /internal/ledger` query.
#[derive(Debug, Clone, PartialEq)]
pub struct LedgerQuery {
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub bot: Option<String>,
    /// Outcomes to include (`rejected`, `edited`, …); empty for all.
    pub outcomes: Vec<&'static str>,
    /// Only rows with `edit_distance` at least this.
    pub min_edit: Option<f64>,
    pub limit: usize,
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

impl ToolServer {
    /// Send an onboarding request: the status and body of a success, or the refusal. A `502`
    /// is a refusal too (a failed approval answers `502 {request}`); other `5xx` and network
    /// errors are [`ApiError::Unavailable`].
    async fn api(req: reqwest::RequestBuilder) -> Result<(u16, Value), ApiError> {
        let res = req.send().await.map_err(|e| {
            tracing::warn!(error = %e, "tool server unreachable");
            ApiError::Unavailable
        })?;
        let status = res.status();
        let json: Value = res.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            return Ok((status.as_u16(), json));
        }
        if status.is_server_error() && status.as_u16() != 502 {
            tracing::warn!(%status, "tool server error");
            return Err(ApiError::Unavailable);
        }
        Err(ApiError::Refused {
            status: status.as_u16(),
            code: json["code"].as_str().unwrap_or_default().to_string(),
            error: json["error"].as_str().unwrap_or_default().to_string(),
            body: json,
        })
    }

    fn get(&self, path: &str, timeout: Duration) -> reqwest::RequestBuilder {
        self.http
            .get(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .timeout(timeout)
    }

    fn post_json(&self, path: &str, timeout: Duration, body: &Value) -> reqwest::RequestBuilder {
        self.http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .timeout(timeout)
            .json(body)
    }

    /// `POST /internal/members/requests`. `true` with a new request (`201`), `false` with the
    /// one already pending for this Slack id (`200`).
    pub async fn request_access(
        &self,
        slack_id: &str,
        email: &str,
        name: &str,
    ) -> Result<(bool, Value), ApiError> {
        let (status, body) = Self::api(self.post_json(
            "/internal/members/requests",
            Duration::from_secs(10),
            &json!({"slack_id": slack_id, "email": email, "name": name}),
        ))
        .await?;
        Ok((status == 201, request_of(&body).clone()))
    }

    /// `GET /internal/members/requests?status=pending`.
    pub async fn pending_requests(&self) -> Result<Vec<Value>, ApiError> {
        let (_, body) = Self::api(
            self.get("/internal/members/requests", Duration::from_secs(10))
                .query(&[("status", "pending")]),
        )
        .await?;
        Ok(body["requests"].as_array().cloned().unwrap_or_default())
    }

    /// `POST /internal/members/requests/{id}/decision`: `approve` (with `handle` in place of
    /// the proposed one) or `reject`. Returns the request as decided. Approving commits to
    /// the content repo, so this can take a while.
    pub async fn decide_member(
        &self,
        id: &str,
        decided_by: &str,
        decision: &str,
        handle: Option<&str>,
    ) -> Result<Value, ApiError> {
        if !request_id_ok(id) {
            return Err(refused_locally("not a request id"));
        }
        let mut body = json!({"decision": decision, "decided_by": decided_by});
        if let Some(handle) = handle {
            body["handle"] = json!(handle);
        }
        let (_, body) = Self::api(self.post_json(
            &format!("/internal/members/requests/{id}/decision"),
            Duration::from_secs(60),
            &body,
        ))
        .await?;
        Ok(request_of(&body).clone())
    }

    /// `POST /internal/members/{handle}/remove`. Returns `{handle, commit}`.
    pub async fn remove_member(&self, handle: &str, decided_by: &str) -> Result<Value, ApiError> {
        if !pmos_contract::model::valid_handle(handle) {
            return Err(refused_locally("not a handle"));
        }
        let (_, body) = Self::api(self.post_json(
            &format!("/internal/members/{handle}/remove"),
            Duration::from_secs(60),
            &json!({"decided_by": decided_by}),
        ))
        .await?;
        Ok(body)
    }

    /// `GET /internal/onboarding?user=`: what a member has set up.
    pub async fn onboarding(&self, handle: &str) -> Result<Value, ApiError> {
        let (_, body) = Self::api(
            self.get("/internal/onboarding", Duration::from_secs(10))
                .query(&[("user", handle)]),
        )
        .await?;
        Ok(body)
    }

    /// `GET /internal/oauth/{provider}/start?user=`: the link that starts linking `provider`
    /// for a member.
    pub async fn oauth_start(&self, provider: &str, handle: &str) -> Result<String, ApiError> {
        if !PROVIDERS.contains(&provider) {
            return Err(refused_locally("not a provider"));
        }
        let (_, body) = Self::api(
            self.get(
                &format!("/internal/oauth/{provider}/start"),
                Duration::from_secs(10),
            )
            .query(&[("user", handle)]),
        )
        .await?;
        body["url"]
            .as_str()
            .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
            .map(String::from)
            .ok_or(ApiError::Unavailable)
    }

    /// `POST /internal/memory/repo`: set a member's memory repo. The tool server checks it
    /// with their GitHub token. `timeout` is short when the answer must fit in an ack.
    pub async fn set_memory_repo(
        &self,
        handle: &str,
        repo: &str,
        timeout: Duration,
    ) -> Result<Value, ApiError> {
        let (_, body) = Self::api(self.post_json(
            "/internal/memory/repo",
            timeout,
            &json!({"user": handle, "repo": repo}),
        ))
        .await?;
        Ok(body)
    }
}

fn refused_locally(error: &str) -> ApiError {
    ApiError::Refused {
        status: 400,
        code: String::new(),
        error: error.into(),
        body: Value::Null,
    }
}

/// A request id is a UUID; anything else never reaches a URL.
pub fn request_id_ok(id: &str) -> bool {
    id.len() <= 36
        && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
        && uuid::Uuid::parse_str(id).is_ok()
}

/// `apr_` and hex, as the tool server makes them; anything else never reaches a URL.
pub fn approval_id_ok(id: &str) -> bool {
    id.strip_prefix("apr_").is_some_and(|rest| {
        !rest.is_empty() && rest.len() <= 64 && rest.chars().all(|c| c.is_ascii_alphanumeric())
    })
}
