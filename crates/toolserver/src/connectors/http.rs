//! Shared HTTP handling for the live connectors: status codes to [`ConnectorError`], and
//! the id checks that keep caller-supplied ids out of URL paths they don't belong in.

use std::time::Duration;

use reqwest::{RequestBuilder, Response, StatusCode};
use serde_json::Value;

use super::ConnectorError;

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("pmos-toolserver/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client builds")
}

fn retry_after(res: &Response) -> u64 {
    res.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(30)
}

/// Send a request to a REST API (Atlassian) and decode the JSON body, mapping failures.
/// `what` names the call in error messages, such as `"Jira issue PM-1"`.
pub async fn send_json(
    req: RequestBuilder,
    system: &str,
    what: &str,
) -> Result<Value, ConnectorError> {
    let res = send(req, system).await?;
    decode_json(res, system, what).await
}

/// Send a request to a REST API, asking for JSON. Only a failure to get any answer is an
/// error here.
pub async fn send(req: RequestBuilder, system: &str) -> Result<Response, ConnectorError> {
    req.header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| ConnectorError::Unavailable(format!("{system}: {e}")))
}

/// Decode an answer from [`send`], mapping failures as [`send_json`] does.
pub async fn decode_json(res: Response, system: &str, what: &str) -> Result<Value, ConnectorError> {
    let status = res.status();
    match status {
        s if s.is_success() => {
            if s == StatusCode::NO_CONTENT {
                return Ok(Value::Null);
            }
            res.json()
                .await
                .map_err(|e| ConnectorError::Unavailable(format!("{system}: bad response: {e}")))
        }
        StatusCode::UNAUTHORIZED => Err(ConnectorError::Reconnect(format!(
            "{system} rejected your sign-in; reconnect {system} in PM OS"
        ))),
        StatusCode::FORBIDDEN => Err(ConnectorError::Forbidden(format!(
            "you don't have access to {what} in {system}"
        ))),
        StatusCode::NOT_FOUND => Err(ConnectorError::NotFound(what.to_string())),
        StatusCode::TOO_MANY_REQUESTS => Err(ConnectorError::RateLimited(retry_after(&res))),
        StatusCode::BAD_REQUEST | StatusCode::CONFLICT | StatusCode::UNPROCESSABLE_ENTITY => {
            let body: Value = res.json().await.unwrap_or(Value::Null);
            Err(ConnectorError::BadArgs(format!(
                "{system} refused {what}: {}",
                error_text(&body)
            )))
        }
        s => Err(ConnectorError::Unavailable(format!(
            "{system} returned {s}"
        ))),
    }
}

/// The readable part of an Atlassian error body.
fn error_text(body: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(msgs) = body["errorMessages"].as_array() {
        parts.extend(msgs.iter().filter_map(|m| m.as_str().map(String::from)));
    }
    if let Some(errs) = body["errors"].as_object() {
        parts.extend(
            errs.iter()
                .map(|(k, v)| format!("{k}: {}", v.as_str().unwrap_or("invalid"))),
        );
    }
    if let Some(errs) = body["errors"].as_array() {
        parts.extend(
            errs.iter()
                .filter_map(|e| e["title"].as_str().map(String::from)),
        );
    }
    if let Some(m) = body["message"].as_str() {
        parts.push(m.to_string());
    }
    if parts.is_empty() {
        "no detail".into()
    } else {
        parts.join("; ")
    }
}

/// A Jira issue key, such as `PM-101`.
pub fn issue_key(value: &str) -> Result<&str, ConnectorError> {
    let ok = value.split_once('-').is_some_and(|(project, n)| {
        !project.is_empty()
            && project
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_uppercase())
            && project
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            && !n.is_empty()
            && n.chars().all(|c| c.is_ascii_digit())
    });
    if ok {
        Ok(value)
    } else {
        Err(ConnectorError::BadArgs(format!(
            "`{value}` is not a Jira issue key"
        )))
    }
}

/// A numeric id, such as a Confluence page id.
pub fn numeric_id<'a>(value: &'a str, what: &str) -> Result<&'a str, ConnectorError> {
    if !value.is_empty() && value.len() <= 20 && value.chars().all(|c| c.is_ascii_digit()) {
        Ok(value)
    } else {
        Err(ConnectorError::BadArgs(format!(
            "`{value}` is not a {what}"
        )))
    }
}

/// A Confluence space key or Jira project key: letters, digits, `_` and `~`.
pub fn key_like<'a>(value: &'a str, what: &str) -> Result<&'a str, ConnectorError> {
    if !value.is_empty()
        && value.len() <= 255
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '~')
    {
        Ok(value)
    } else {
        Err(ConnectorError::BadArgs(format!(
            "`{value}` is not a {what}"
        )))
    }
}

/// A required string argument.
pub fn arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, ConnectorError> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ConnectorError::BadArgs(format!("`{key}` is required")))
}

pub fn limit(args: &Value, default: u64, max: u64) -> u64 {
    args.get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(default)
        .clamp(1, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        assert!(issue_key("PM-101").is_ok());
        assert!(issue_key("AB_2-7").is_ok());
        for bad in ["pm-1", "PM-", "-1", "PM-1/../x", "PM-1?x", "PM 1"] {
            assert!(issue_key(bad).is_err(), "{bad}");
        }
        assert!(numeric_id("12345", "page id").is_ok());
        assert!(numeric_id("12/../3", "page id").is_err());
        assert!(key_like("PM", "space key").is_ok());
        assert!(key_like("~ana", "space key").is_ok());
        assert!(key_like("PM&x=1", "space key").is_err());
    }

    #[test]
    fn atlassian_error_text() {
        let body = serde_json::json!({"errorMessages": ["Field 'x' is required"], "errors": {"summary": "too long"}});
        assert_eq!(
            error_text(&body),
            "Field 'x' is required; summary: too long"
        );
        assert_eq!(error_text(&Value::Null), "no detail");
    }
}
