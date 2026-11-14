//! The live connectors and account linking, against a local mock of Atlassian (auth and
//! REST APIs) and Slack. The mock records every request so the tests can check exactly what
//! would go over the wire: whose token, which path, which body.

mod common;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use base64::Engine;
use chrono::{Duration, Utc};
use pmos_toolserver::audit::Outcome;
use pmos_toolserver::catalog::Catalog;
use pmos_toolserver::connectors::atlassian::{Confluence, Jira};
use pmos_toolserver::connectors::slack::Slack;
use pmos_toolserver::connectors::{Caller, Connector, ConnectorError};
use pmos_toolserver::oauth::{
    AppCredentials, Endpoints, OAuth, OAuthConfig, OAuthError, TokenError, TokenSource,
};
use pmos_toolserver::service::ToolError;
use pmos_toolserver::team::Team;
use pmos_toolserver::vault::{Credential, KeyRing, MemoryCredentials, Provider, Secret, Vault};
use pmos_toolserver::{router, AppConfig, Stores, ToolService};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

// ----- the mock ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Req {
    method: String,
    path: String,
    query: HashMap<String, String>,
    auth: Option<String>,
    body: String,
}

impl Req {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }

    fn form(&self) -> HashMap<String, String> {
        url_decode_pairs(&self.body)
    }
}

fn url_decode_pairs(s: &str) -> HashMap<String, String> {
    reqwest::Url::parse(&format!("http://x/?{s}"))
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

#[derive(Default)]
struct Mock {
    log: Mutex<Vec<Req>>,
    refreshes: AtomicUsize,
}

impl Mock {
    fn exact(&self, path: &str) -> Vec<Req> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.path == path)
            .cloned()
            .collect()
    }

    fn requests(&self, path_contains: &str) -> Vec<Req> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.path.contains(path_contains))
            .cloned()
            .collect()
    }
}

fn reply(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

async fn handle(
    State(mock): State<Arc<Mock>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let req = Req {
        method: method.to_string(),
        path: uri.path().to_string(),
        query: uri.query().map(url_decode_pairs).unwrap_or_default(),
        auth: headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(String::from),
        body: String::from_utf8_lossy(&body).into_owned(),
    };
    mock.log.lock().unwrap().push(req.clone());
    let auth = req.auth.clone().unwrap_or_default();
    let ok = StatusCode::OK;

    match (req.method.as_str(), req.path.as_str()) {
        // Atlassian auth
        ("POST", "/oauth/token") => {
            let b = req.json();
            match b["grant_type"].as_str() {
                Some("authorization_code") if b["code"] == "good-code" => reply(
                    ok,
                    json!({"access_token": "at-1", "refresh_token": "rt-1", "expires_in": 3600,
                           "scope": "read:jira-work offline_access"}),
                ),
                Some("refresh_token") if b["refresh_token"] == "rt-badclient" => reply(
                    StatusCode::UNAUTHORIZED,
                    json!({"error": "invalid_client", "error_description": "Client authentication failed."}),
                ),
                Some("refresh_token") if b["refresh_token"] == "rt-revoked" => reply(
                    StatusCode::FORBIDDEN,
                    json!({"error": "invalid_grant", "error_description": "Unknown or invalid refresh token."}),
                ),
                Some("refresh_token") => {
                    let n = mock.refreshes.fetch_add(1, Ordering::SeqCst) + 1;
                    // Widen the window so concurrent callers would overlap if unguarded.
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    reply(
                        ok,
                        json!({"access_token": format!("at-r{n}"), "refresh_token": format!("rt-r{n}"),
                               "expires_in": 3600, "scope": "read:jira-work offline_access"}),
                    )
                }
                _ => reply(StatusCode::FORBIDDEN, json!({"error": "invalid_grant"})),
            }
        }
        ("GET", "/oauth/token/accessible-resources") => reply(
            ok,
            json!([{"id": "cloud-1", "url": "https://example.atlassian.net", "name": "example",
                    "scopes": ["read:jira-work"]}]),
        ),

        // The signed-in Atlassian account's profile.
        ("GET", "/me") => match auth.as_str() {
            "Bearer at-1" => reply(
                ok,
                json!({"account_id": "acc-ana", "email": "Ana@Example.com", "name": "Ana"}),
            ),
            _ => reply(StatusCode::UNAUTHORIZED, json!({"code": 401})),
        },

        // Atlassian APIs: status overrides by token
        (_, p) if p.starts_with("/ex/") && auth == "Bearer at-401" => reply(
            StatusCode::UNAUTHORIZED,
            json!({"code": 401, "message": "Unauthorized"}),
        ),
        (_, p) if p.starts_with("/ex/") && auth == "Bearer at-403" => reply(
            StatusCode::FORBIDDEN,
            json!({"errorMessages": ["No permission"]}),
        ),
        (_, p) if p.starts_with("/ex/") && auth == "Bearer at-429" => (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", "7")],
            Json(json!({})),
        )
            .into_response(),

        // Jira
        ("GET", "/ex/jira/cloud-1/rest/api/3/search/jql") => reply(
            ok,
            json!({"isLast": true, "issues": [{"key": "PM-1", "fields": {
                "summary": "SSO for DSM", "status": {"name": "To Do"}, "issuetype": {"name": "Story"},
                "assignee": {"displayName": "Ana"}, "updated": "2026-11-10T10:00:00.000+0000"}}]}),
        ),
        ("GET", "/ex/jira/cloud-1/rest/api/3/issue/PM-1") => reply(
            ok,
            json!({"key": "PM-1", "fields": {
                "summary": "SSO for DSM", "status": {"name": "To Do"}, "issuetype": {"name": "Story"},
                "description": {"type": "doc", "version": 1, "content": [
                    {"type": "paragraph", "content": [{"type": "text", "text": "Customers want SAML."}]}]},
                "parent": {"key": "PM-100"},
                "comment": {"comments": [{"author": {"displayName": "Bo"}, "created": "2026-11-11",
                    "body": {"type": "doc", "version": 1, "content": [{"type": "paragraph",
                        "content": [{"type": "text", "text": "Also OIDC?"}]}]}}]}}}),
        ),
        // A moved issue: asked for as PM-9, it comes back under its new key.
        ("GET", "/ex/jira/cloud-1/rest/api/3/issue/PM-9") => reply(
            ok,
            json!({"key": "SECRET-9", "fields": {"summary": "Moved", "status": {"name": "To Do"},
                "issuetype": {"name": "Story"}}}),
        ),
        ("GET", p) if p.starts_with("/ex/jira/cloud-1/rest/api/3/issue/") => reply(
            StatusCode::NOT_FOUND,
            json!({"errorMessages": ["Issue does not exist"]}),
        ),
        ("POST", "/ex/jira/cloud-1/rest/api/3/issue") => reply(
            StatusCode::CREATED,
            json!({"id": "10001", "key": "PM-200", "self": "x"}),
        ),

        // Confluence
        ("GET", "/ex/confluence/cloud-1/wiki/api/v2/spaces") => {
            // `keys` is a comma-separated list; PM is id 98765 (a string), HR 11111 (a number).
            let keys = req.query.get("keys").cloned().unwrap_or_default();
            let results: Vec<Value> = keys
                .split(',')
                .filter_map(|k| match k {
                    "PM" => Some(json!({"id": "98765", "key": "PM"})),
                    "HR" => Some(json!({"id": 11111, "key": "HR"})),
                    _ => None,
                })
                .collect();
            reply(ok, json!({ "results": results }))
        }
        ("GET", "/ex/confluence/cloud-1/wiki/api/v2/pages/557") => reply(
            ok,
            json!({"id": "557", "title": "Salary bands", "status": "current", "spaceId": "11111",
                   "version": {"number": 1}, "body": {"storage": {"value": "<p>secret</p>"}},
                   "_links": {"webui": "/spaces/HR/pages/557"}}),
        ),
        ("POST", "/ex/confluence/cloud-1/wiki/api/v2/pages") => reply(
            ok,
            json!({"id": "555", "status": "draft", "_links": {"webui": "/spaces/PM/pages/555/PRD"}}),
        ),
        ("GET", "/ex/confluence/cloud-1/wiki/api/v2/pages/555") => reply(
            ok,
            json!({"id": "555", "title": "PRD: SSO", "status": "current", "spaceId": "98765",
                   "version": {"number": 3},
                   "body": {"storage": {"value": "<h1>PRD</h1><p>Hello &amp; welcome</p>"}},
                   "_links": {"webui": "/spaces/PM/pages/555/PRD"}}),
        ),
        ("GET", "/ex/confluence/cloud-1/wiki/api/v2/pages/556") => reply(
            ok,
            json!({"id": "556", "title": "Draft PRD", "status": "draft", "version": {"number": 1},
                   "body": {"storage": {"value": "<p>draft</p>"}},
                   "_links": {"webui": "/spaces/PM/pages/556"}}),
        ),
        ("PUT", "/ex/confluence/cloud-1/wiki/api/v2/pages/556") => reply(
            StatusCode::CONFLICT,
            json!({"errors": [{"status": 409, "title": "Version must be incremented when updating a page"}],
                   "errorMessages": ["Version must be incremented when updating a page. Current Version: [2]."]}),
        ),
        ("PUT", "/ex/confluence/cloud-1/wiki/api/v2/pages/555") => reply(
            ok,
            json!({"id": "555", "_links": {"webui": "/spaces/PM/pages/555/PRD"}}),
        ),
        ("GET", "/ex/confluence/cloud-1/wiki/rest/api/search") => reply(
            ok,
            json!({"results": [{"content": {"id": "9001", "type": "page", "title": "Call notes"},
                "title": "Call notes", "excerpt": "<p>two SSO asks</p>", "url": "/spaces/PM/pages/9001",
                "lastModified": "2026-11-01T00:00:00Z"}], "size": 1}),
        ),

        // Slack
        ("POST", "/api/oauth.v2.access") => {
            let basic = format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode("slk:slk-secret")
            );
            if auth != basic {
                return reply(ok, json!({"ok": false, "error": "invalid_client"}));
            }
            match req.form().get("code").map(String::as_str) {
                Some("good-code") => reply(
                    ok,
                    json!({"ok": true, "access_token": "xoxb-bot", "token_type": "bot",
                           "team": {"id": "T1"},
                           "authed_user": {"id": "U0ANA", "scope": "chat:write,search:read.public",
                                           "access_token": "xoxp-ana", "token_type": "user"}}),
                ),
                _ => reply(ok, json!({"ok": false, "error": "invalid_code"})),
            }
        }
        ("POST", p) if p.starts_with("/api/") && auth == "Bearer xoxp-revoked" => {
            reply(ok, json!({"ok": false, "error": "token_revoked"}))
        }
        ("POST", "/api/assistant.search.context") => {
            if req.form().get("query").map(String::as_str) == Some("ratelimit") {
                return (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "7")], "").into_response();
            }
            if req.form().get("query").map(String::as_str) == Some("launch") {
                return reply(
                    ok,
                    json!({"ok": true, "results": {"messages": [
                        {"author_user_id": "U2", "channel_id": "C0SECRET", "channel_name": "exec",
                         "message_ts": "1731500001.000100", "content": "Launch slips a month"},
                        {"author_user_id": "U3", "channel_id": "C0FIELD", "channel_name": "field-questions",
                         "message_ts": "1731500002.000100", "content": "When is the launch?"}]}}),
                );
            }
            reply(
                ok,
                json!({"ok": true, "results": {"messages": [{"author_user_id": "U2",
                    "author_name": "Sam SE", "channel_id": "C0FIELD", "channel_name": "field-questions",
                    "message_ts": "1731500000.000100", "content": "Does DSM support SAML SSO yet?",
                    "is_author_bot": false,
                    "permalink": "https://example.slack.com/archives/C0FIELD/p1731500000000100"}]},
                    "response_metadata": {"next_cursor": ""}}),
            )
        }
        ("POST", "/api/conversations.replies") => reply(
            ok,
            json!({"ok": true, "messages": [
                {"user": "U2", "ts": "1731500000.000100", "text": "Does DSM support SAML SSO yet?"},
                {"user": "U3", "ts": "1731500005.000100", "text": "+1"}]}),
        ),
        ("POST", "/api/chat.postMessage") => {
            let f = req.form();
            if f.get("channel").map(String::as_str) == Some("C0GONE") {
                return reply(ok, json!({"ok": false, "error": "channel_not_found"}));
            }
            reply(
                ok,
                json!({"ok": true, "channel": f.get("channel"), "ts": "1731500099.000300"}),
            )
        }
        _ => reply(StatusCode::NOT_FOUND, json!({"error": "mock: no route"})),
    }
}

async fn start_mock() -> (String, Arc<Mock>, CancellationToken) {
    let mock = Arc::new(Mock::default());
    let app = Router::new().fallback(handle).with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let cancel = CancellationToken::new();
    let ct = cancel.clone();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { ct.cancelled_owned().await })
            .await
            .unwrap();
    });
    (base, mock, cancel)
}

// ----- the system under test --------------------------------------------------------------

struct Live {
    base: String,
    mock: Arc<Mock>,
    oauth: Arc<OAuth>,
    vault: Vault,
    jira: Jira,
    confluence: Confluence,
    slack: Slack,
    cancel: CancellationToken,
}

async fn live() -> Live {
    live_with(None).await
}

/// Live connectors with `team.yaml` enforced, as in production.
async fn live_with(team: Option<Arc<Team>>) -> Live {
    let (base, mock, cancel) = start_mock().await;
    let vault = Vault::new(
        KeyRing::from_spec(&KeyRing::generate_spec("k1")).unwrap(),
        Arc::new(MemoryCredentials::default()),
    );
    let oauth = OAuth::new(
        OAuthConfig {
            public_base_url: "https://pmos.example.com".into(),
            atlassian: Some(AppCredentials {
                client_id: "atl".into(),
                client_secret: Secret::new("atl-secret"),
                scopes: vec!["read:jira-work".into()],
            }),
            atlassian_site: None,
            slack: Some(AppCredentials {
                client_id: "slk".into(),
                client_secret: Secret::new("slk-secret"),
                scopes: vec!["chat:write".into()],
            }),
            github: None,
            github_org: None,
            state_key: b"test-state-key".to_vec(),
            endpoints: Endpoints {
                atlassian_auth: base.clone(),
                atlassian_api: base.clone(),
                slack: base.clone(),
                ..Endpoints::default()
            },
        },
        vault.clone(),
    );
    let oauth = Arc::new(match team {
        Some(team) => oauth.with_team(team),
        None => oauth,
    });
    let tokens: Arc<dyn TokenSource> = oauth.clone();
    Live {
        jira: Jira::new(tokens.clone(), base.clone()),
        confluence: Confluence::new(tokens.clone(), base.clone()),
        slack: Slack::new(tokens, base.clone()),
        base,
        mock,
        oauth,
        vault,
        cancel,
    }
}

fn state_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned()
}

async fn link(l: &Live, user: &str, provider: Provider) {
    let url = l.oauth.authorize_url(user, provider, Utc::now()).unwrap();
    l.oauth
        .complete(provider, "good-code", &state_of(&url), Utc::now())
        .await
        .unwrap();
}

fn ana() -> Caller<'static> {
    Caller::new("ana", "run-7")
}

/// Store an Atlassian credential directly, for expiry and failure cases.
async fn put_atlassian(l: &Live, user: &str, access: &str, refresh: &str, expires_in: i64) {
    l.vault
        .put(&Credential {
            user: user.into(),
            provider: Provider::Atlassian,
            access_token: Secret::new(access),
            refresh_token: Some(Secret::new(refresh)),
            expires_at: Some(Utc::now() + Duration::seconds(expires_in)),
            scopes: "read:jira-work".into(),
            cloud_id: Some("cloud-1".into()),
            site_url: Some("https://example.atlassian.net".into()),
            external_id: None,
            updated_at: Utc::now(),
        })
        .await
        .unwrap();
}

// ----- account linking --------------------------------------------------------------------

#[tokio::test]
async fn atlassian_linking_stores_the_site_and_the_users_own_tokens() {
    let l = live().await;
    link(&l, "ana", Provider::Atlassian).await;

    let exchange = &l.mock.exact("/oauth/token")[0].json();
    assert_eq!(exchange["grant_type"], "authorization_code");
    assert_eq!(exchange["client_id"], "atl");
    assert_eq!(
        exchange["redirect_uri"],
        "https://pmos.example.com/oauth/atlassian/callback"
    );
    let resources = &l.mock.requests("accessible-resources")[0];
    assert_eq!(resources.auth.as_deref(), Some("Bearer at-1"));

    let cred = l
        .vault
        .get("ana", Provider::Atlassian)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cred.access_token.expose(), "at-1");
    assert_eq!(cred.refresh_token.unwrap().expose(), "rt-1");
    assert_eq!(cred.cloud_id.as_deref(), Some("cloud-1"));
    assert!(cred.expires_at.unwrap() > Utc::now() + Duration::minutes(59));

    // A bad code is reported, and nothing is stored for that user.
    let url = l
        .oauth
        .authorize_url("bo", Provider::Atlassian, Utc::now())
        .unwrap();
    let err = l
        .oauth
        .complete(Provider::Atlassian, "bad-code", &state_of(&url), Utc::now())
        .await
        .unwrap_err();
    assert!(matches!(err, OAuthError::Exchange(Provider::Atlassian, _)));
    assert!(l
        .vault
        .get("bo", Provider::Atlassian)
        .await
        .unwrap()
        .is_none());
    l.cancel.cancel();
}

#[tokio::test]
async fn slack_linking_keeps_the_user_token_never_the_bot_token() {
    let l = live().await;
    link(&l, "ana", Provider::Slack).await;
    let exchange = &l.mock.requests("oauth.v2.access")[0];
    assert!(
        exchange.auth.as_deref().unwrap().starts_with("Basic "),
        "client auth via Basic"
    );
    assert!(
        !exchange.body.contains("slk-secret"),
        "secret not in the form body"
    );
    let cred = l.vault.get("ana", Provider::Slack).await.unwrap().unwrap();
    assert_eq!(cred.access_token.expose(), "xoxp-ana");
    assert_eq!(cred.external_id.as_deref(), Some("U0ANA"));
    l.cancel.cancel();
}

#[tokio::test]
async fn a_state_cannot_be_replayed() {
    let l = live().await;
    let url = l
        .oauth
        .authorize_url("ana", Provider::Atlassian, Utc::now())
        .unwrap();
    let state = state_of(&url);
    l.oauth
        .complete(Provider::Atlassian, "good-code", &state, Utc::now())
        .await
        .unwrap();
    let again = l
        .oauth
        .complete(Provider::Atlassian, "good-code", &state, Utc::now())
        .await;
    assert_eq!(again.unwrap_err(), OAuthError::BadState);
    assert_eq!(l.mock.exact("/oauth/token").len(), 1, "no second exchange");
    l.cancel.cancel();
}

// ----- tokens -----------------------------------------------------------------------------

#[tokio::test]
async fn every_call_uses_the_callers_own_token() {
    let l = live().await;
    put_atlassian(&l, "ana", "at-ana", "rt-ana", 3600).await;
    put_atlassian(&l, "bo", "at-bo", "rt-bo", 3600).await;
    l.jira
        .read(ana(), "search", &json!({"jql": "project = PM"}))
        .await
        .unwrap();
    let bo = Caller::new("bo", "run-8");
    l.jira
        .read(bo, "search", &json!({"jql": "project = PM"}))
        .await
        .unwrap();
    let auths: Vec<_> = l
        .mock
        .requests("search/jql")
        .into_iter()
        .map(|r| r.auth.unwrap())
        .collect();
    assert_eq!(auths, vec!["Bearer at-ana", "Bearer at-bo"]);

    let cy = Caller::new("cy", "run-9");
    let err = l
        .jira
        .read(cy, "search", &json!({"jql": "x"}))
        .await
        .unwrap_err();
    assert_eq!(
        err,
        ConnectorError::NotConnected(
            "connect Atlassian (Jira and Confluence) in PM OS first".into()
        )
    );
    assert_eq!(
        l.mock.requests("search/jql").len(),
        2,
        "no call without a token"
    );
    l.cancel.cancel();
}

#[tokio::test]
async fn an_expiring_token_is_refreshed_once_for_concurrent_calls() {
    let l = Arc::new(live().await);
    put_atlassian(&l, "ana", "at-old", "rt-old", 30).await; // inside the refresh margin

    let mut handles = Vec::new();
    for _ in 0..5 {
        let l = l.clone();
        handles.push(tokio::spawn(async move {
            l.jira.read(ana(), "read", &json!({"key": "PM-1"})).await
        }));
    }
    for h in handles {
        h.await.unwrap().unwrap();
    }
    assert_eq!(
        l.mock.refreshes.load(Ordering::SeqCst),
        1,
        "one refresh, not five"
    );
    let refresh = &l.mock.exact("/oauth/token")[0].json();
    assert_eq!(refresh["grant_type"], "refresh_token");
    assert_eq!(refresh["refresh_token"], "rt-old");
    let auths: Vec<_> = l
        .mock
        .requests("issue/PM-1")
        .into_iter()
        .map(|r| r.auth.unwrap())
        .collect();
    assert_eq!(auths, vec!["Bearer at-r1"; 5]);

    // The rotated refresh token replaced the old one.
    let cred = l
        .vault
        .get("ana", Provider::Atlassian)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cred.refresh_token.unwrap().expose(), "rt-r1");
    l.cancel.cancel();
}

#[tokio::test]
async fn a_refused_refresh_unlinks_the_account() {
    let l = live().await;
    put_atlassian(&l, "ana", "at-old", "rt-revoked", -10).await;
    let err = l.oauth.token("ana", Provider::Atlassian).await.unwrap_err();
    assert_eq!(err, TokenError::Reconnect(Provider::Atlassian));
    assert!(l
        .vault
        .get("ana", Provider::Atlassian)
        .await
        .unwrap()
        .is_none());
    let err = l.oauth.token("ana", Provider::Atlassian).await.unwrap_err();
    assert_eq!(err, TokenError::NotConnected(Provider::Atlassian));
    l.cancel.cancel();
}

#[tokio::test]
async fn a_client_error_on_refresh_keeps_the_account() {
    // A mistyped or rotated client secret is ours to fix; unlinking would make every PM
    // reconnect.
    let l = live().await;
    put_atlassian(&l, "ana", "at-old", "rt-badclient", -10).await;
    let err = l.oauth.token("ana", Provider::Atlassian).await.unwrap_err();
    assert!(
        matches!(err, TokenError::Unavailable(Provider::Atlassian, ref e) if e == "invalid_client"),
        "{err:?}"
    );
    let kept = l.vault.get("ana", Provider::Atlassian).await.unwrap();
    assert_eq!(
        kept.unwrap().refresh_token.unwrap().expose(),
        "rt-badclient"
    );
    l.cancel.cancel();
}

#[tokio::test]
async fn unlinking_during_a_refresh_stays_unlinked() {
    let l = Arc::new(live().await);
    put_atlassian(&l, "ana", "at-old", "rt-old", -10).await;
    // The mock holds each refresh for 100 ms; unlink while this one is in flight.
    let refreshing = {
        let l = l.clone();
        tokio::spawn(async move { l.oauth.token("ana", Provider::Atlassian).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert!(l
        .oauth
        .unlink_account("ana", Provider::Atlassian)
        .await
        .unwrap());
    let _ = refreshing.await.unwrap();
    assert!(
        l.vault
            .get("ana", Provider::Atlassian)
            .await
            .unwrap()
            .is_none(),
        "the refresh must not write the account back"
    );
    l.cancel.cancel();
}

#[tokio::test]
async fn a_stale_write_cannot_replace_a_newer_credential() {
    let l = live().await;
    put_atlassian(&l, "ana", "at-1", "rt-1", 3600).await;
    let old = l
        .vault
        .get("ana", Provider::Atlassian)
        .await
        .unwrap()
        .unwrap();
    let newer = Credential {
        refresh_token: Some(Secret::new("rt-2")),
        updated_at: old.updated_at + Duration::seconds(1),
        ..old.clone()
    };
    assert!(l.vault.replace(&newer, old.updated_at).await.unwrap());
    // A write based on the old row (such as a re-seal or a slower refresh) is refused.
    let stale = Credential {
        refresh_token: Some(Secret::new("rt-1")),
        ..old.clone()
    };
    assert!(!l.vault.replace(&stale, old.updated_at).await.unwrap());
    let now = l
        .vault
        .get("ana", Provider::Atlassian)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(now.refresh_token.unwrap().expose(), "rt-2");
    l.cancel.cancel();
}

fn member(handle: &str, slack_id: &str, email: Option<&str>) -> pmos_contract::model::TeamMember {
    pmos_contract::model::TeamMember {
        handle: handle.into(),
        slack_id: slack_id.into(),
        email: email.map(String::from),
        github: None,
        roles: vec![pmos_contract::model::Role::Pm],
    }
}

#[tokio::test]
async fn a_linked_account_must_belong_to_the_pm_it_is_linked_for() {
    // The mock signs in Slack user U0ANA and Atlassian user Ana@Example.com.
    let team = Team::of(vec![
        member("ana", "U0ANA", Some("ana@example.com")),
        member("bo", "U0BO", Some("bo@example.com")),
        member("cy", "U0CY", None),
    ]);
    let l = live_with(Some(team)).await;
    let try_link = |user: &'static str, provider| {
        let url = l.oauth.authorize_url(user, provider, Utc::now()).unwrap();
        let oauth = l.oauth.clone();
        async move {
            oauth
                .complete(provider, "good-code", &state_of(&url), Utc::now())
                .await
        }
    };

    // Ana's own accounts link.
    try_link("ana", Provider::Slack).await.unwrap();
    try_link("ana", Provider::Atlassian).await.unwrap();

    // Bo's link, opened by Ana (a forwarded link), attaches nothing to Bo.
    for provider in [Provider::Slack, Provider::Atlassian] {
        let err = try_link("bo", provider).await.unwrap_err();
        assert!(
            matches!(err, OAuthError::WrongAccount(p, _) if p == provider),
            "{err:?}"
        );
        assert!(l.vault.get("bo", provider).await.unwrap().is_none());
    }
    // Atlassian needs an email in team.yaml to check against; nobody outside the team links.
    assert!(matches!(
        try_link("cy", Provider::Atlassian).await.unwrap_err(),
        OAuthError::WrongAccount(..)
    ));
    assert!(matches!(
        try_link("eve", Provider::Slack).await.unwrap_err(),
        OAuthError::WrongAccount(..)
    ));
    l.cancel.cancel();
}

#[tokio::test]
async fn api_errors_map_to_actionable_messages() {
    let l = live().await;
    let args = json!({"key": "PM-1"});
    put_atlassian(&l, "ana", "at-401", "rt", 3600).await;
    assert!(matches!(
        l.jira.read(ana(), "read", &args).await,
        Err(ConnectorError::Reconnect(_))
    ));
    put_atlassian(&l, "ana", "at-403", "rt", 3600).await;
    assert!(matches!(
        l.jira.read(ana(), "read", &args).await,
        Err(ConnectorError::Forbidden(_))
    ));
    put_atlassian(&l, "ana", "at-429", "rt", 3600).await;
    assert_eq!(
        l.jira.read(ana(), "read", &args).await,
        Err(ConnectorError::RateLimited(7))
    );
    put_atlassian(&l, "ana", "at-ok", "rt", 3600).await;
    assert_eq!(
        l.jira.read(ana(), "read", &json!({"key": "PM-404"})).await,
        Err(ConnectorError::NotFound("issue PM-404".into()))
    );
    // A key that could escape the path never reaches the API.
    let before = l.mock.log.lock().unwrap().len();
    assert!(matches!(
        l.jira
            .read(ana(), "read", &json!({"key": "PM-1/../../admin"}))
            .await,
        Err(ConnectorError::BadArgs(_))
    ));
    assert_eq!(l.mock.log.lock().unwrap().len(), before);
    l.cancel.cancel();
}

// ----- Jira -------------------------------------------------------------------------------

#[tokio::test]
async fn jira_search_read_and_create() {
    let l = live().await;
    put_atlassian(&l, "ana", "at-ana", "rt", 3600).await;

    let hits = l
        .jira
        .read(
            ana(),
            "search",
            &json!({"jql": "project = PM AND text ~ SSO", "limit": 5}),
        )
        .await
        .unwrap();
    assert_eq!(hits[0].source.id, "PM-1");
    assert_eq!(
        hits[0].source.url,
        "https://example.atlassian.net/browse/PM-1"
    );
    assert_eq!(hits[0].title, "PM-1: SSO for DSM");
    let search = &l.mock.requests("search/jql")[0];
    assert_eq!(search.query["jql"], "project = PM AND text ~ SSO");
    assert_eq!(search.query["maxResults"], "5");

    let issue = &l
        .jira
        .read(ana(), "read", &json!({"key": "PM-1"}))
        .await
        .unwrap()[0];
    assert_eq!(issue.content["description"], "Customers want SAML.");
    assert_eq!(issue.content["parent"], "PM-100");
    assert_eq!(issue.content["comments"][0]["text"], "Also OIDC?");

    let created = l
        .jira
        .write(
            ana(),
            "create_issue",
            &json!({"project": "PM", "issue_type": "Story", "summary": "SCIM provisioning",
                    "description": "Provision users from the IdP.", "epic": "PM-100"}),
        )
        .await
        .unwrap();
    assert_eq!(created.id, "PM-200");
    assert_eq!(created.url, "https://example.atlassian.net/browse/PM-200");
    let body = l
        .mock
        .requests("rest/api/3/issue")
        .into_iter()
        .find(|r| r.method == "POST")
        .unwrap()
        .json();
    let f = &body["fields"];
    assert_eq!(f["project"]["key"], "PM");
    assert_eq!(f["issuetype"]["name"], "Story");
    assert_eq!(f["parent"]["key"], "PM-100");
    assert_eq!(f["description"]["type"], "doc");
    let text = serde_json::to_string(&f["description"]).unwrap();
    assert!(text.contains("Provision users from the IdP."));
    assert!(
        text.contains("Drafted with PM OS · run run-7"),
        "marker and run id"
    );
    l.cancel.cancel();
}

// ----- Confluence -------------------------------------------------------------------------

#[tokio::test]
async fn confluence_draft_finds_the_space_and_escapes_raw_html() {
    let l = live().await;
    put_atlassian(&l, "ana", "at-ana", "rt", 3600).await;
    let created = l
        .confluence
        .write(
            ana(),
            "create_draft",
            &json!({"space": "PM", "title": "PRD: SSO", "body": "# PRD\n\nShip **SSO**.\n\n<script>x</script>"}),
        )
        .await
        .unwrap();
    assert_eq!(created.id, "555");
    assert_eq!(
        created.url,
        "https://example.atlassian.net/wiki/spaces/PM/pages/555/PRD"
    );
    let space = &l.mock.requests("api/v2/spaces")[0];
    assert_eq!(space.query["keys"], "PM");
    let post = l
        .mock
        .requests("api/v2/pages")
        .into_iter()
        .find(|r| r.method == "POST")
        .unwrap()
        .json();
    assert_eq!(post["spaceId"], "98765");
    assert_eq!(post["status"], "draft", "a draft, never published");
    assert_eq!(post["body"]["representation"], "storage");
    let value = post["body"]["value"].as_str().unwrap();
    assert!(value.contains("<strong>SSO</strong>"));
    assert!(!value.contains("<script>"));
    assert!(value.contains("Drafted with PM OS · run run-7"));

    let missing = l
        .confluence
        .write(
            ana(),
            "create_draft",
            &json!({"space": "NOPE", "title": "t", "body": "b"}),
        )
        .await;
    assert_eq!(
        missing,
        Err(ConnectorError::NotFound("Confluence space NOPE".into()))
    );
    l.cancel.cancel();
}

#[tokio::test]
async fn confluence_update_bumps_the_version_and_read_returns_text() {
    let l = live().await;
    put_atlassian(&l, "ana", "at-ana", "rt", 3600).await;
    let page = &l
        .confluence
        .read(ana(), "read", &json!({"page_id": "555"}))
        .await
        .unwrap()[0];
    assert_eq!(page.content["body"], "PRD\nHello & welcome");
    assert_eq!(page.content["version"], 3);

    l.confluence
        .write(
            ana(),
            "update_page",
            &json!({"page_id": "555", "body": "New body"}),
        )
        .await
        .unwrap();
    let put = l
        .mock
        .requests("pages/555")
        .into_iter()
        .find(|r| r.method == "PUT")
        .unwrap()
        .json();
    assert_eq!(put["version"]["number"], 4);
    assert_eq!(put["title"], "PRD: SSO", "title kept");
    assert_eq!(put["status"], "current", "a published page stays published");

    // A draft stays a draft, and a version clash says what to do.
    let err = l
        .confluence
        .write(
            ana(),
            "update_page",
            &json!({"page_id": "556", "body": "New draft"}),
        )
        .await
        .unwrap_err();
    let put = l
        .mock
        .requests("pages/556")
        .into_iter()
        .find(|r| r.method == "PUT")
        .unwrap()
        .json();
    assert_eq!(
        put["status"], "draft",
        "updating a draft must not publish it"
    );
    assert!(
        matches!(&err, ConnectorError::BadArgs(m) if m.contains("changed since PM OS read it")),
        "{err:?}"
    );

    let hits = l
        .confluence
        .read(ana(), "search", &json!({"cql": "text ~ SSO"}))
        .await
        .unwrap();
    assert_eq!(hits[0].source.id, "9001");
    assert_eq!(hits[0].content["excerpt"], "two SSO asks");
    assert_eq!(
        l.mock.requests("rest/api/search")[0].query["cql"],
        "text ~ SSO"
    );
    l.cancel.cancel();
}

// ----- Slack ------------------------------------------------------------------------------

async fn put_slack(l: &Live, user: &str, token: &str) {
    l.vault
        .put(&Credential {
            user: user.into(),
            provider: Provider::Slack,
            access_token: Secret::new(token),
            refresh_token: None,
            expires_at: None,
            scopes: "chat:write".into(),
            cloud_id: None,
            site_url: None,
            external_id: Some("U0ANA".into()),
            updated_at: Utc::now(),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn slack_search_thread_and_reply_as_the_pm() {
    let l = live().await;
    put_slack(&l, "ana", "xoxp-ana").await;

    let hits = l
        .slack
        .read(ana(), "search", &json!({"query": "SAML SSO"}))
        .await
        .unwrap();
    assert_eq!(hits[0].source.id, "C0FIELD:1731500000.000100");
    assert_eq!(hits[0].content["text"], "Does DSM support SAML SSO yet?");
    let search = &l.mock.requests("assistant.search.context")[0];
    assert_eq!(search.auth.as_deref(), Some("Bearer xoxp-ana"));
    assert_eq!(search.form()["query"], "SAML SSO");

    let thread = &l
        .slack
        .read(
            ana(),
            "read_thread",
            &json!({"channel": "C0FIELD", "ts": "1731500000.000100"}),
        )
        .await
        .unwrap()[0];
    assert_eq!(thread.content["messages"].as_array().unwrap().len(), 2);

    let posted = l
        .slack
        .write(
            ana(),
            "post_reply",
            &json!({"channel": "C0FIELD", "thread_ts": "1731500000.000100", "text": "Yes, in 4.2."}),
        )
        .await
        .unwrap();
    assert_eq!(posted.id, "C0FIELD:1731500099.000300");
    let post = &l.mock.requests("chat.postMessage")[0];
    assert_eq!(
        post.auth.as_deref(),
        Some("Bearer xoxp-ana"),
        "the PM's own token"
    );
    let form = post.form();
    assert_eq!(form["thread_ts"], "1731500000.000100");
    assert_eq!(form["text"], "Yes, in 4.2.\n\n_Drafted with PM OS_");
    l.cancel.cancel();
}

#[tokio::test]
async fn slack_errors_map_to_actionable_messages() {
    let l = live().await;
    put_slack(&l, "ana", "xoxp-ana").await;
    assert_eq!(
        l.slack
            .read(ana(), "search", &json!({"query": "ratelimit"}))
            .await,
        Err(ConnectorError::RateLimited(7))
    );
    assert!(matches!(
        l.slack
            .write(
                ana(),
                "send_message",
                &json!({"channel": "C0GONE", "text": "hi"})
            )
            .await,
        Err(ConnectorError::NotFound(_))
    ));
    put_slack(&l, "ana", "xoxp-revoked").await;
    assert!(matches!(
        l.slack.read(ana(), "search", &json!({"query": "x"})).await,
        Err(ConnectorError::Reconnect(_))
    ));
    l.cancel.cancel();
}

// ----- through the tool server ------------------------------------------------------------

const SERVICE_TOKEN: &str = "test-service-token-0123456789abcdef";
const BOT_TOKEN: &str = "test-bot-token-0123456789abcdef-0123";

#[tokio::test]
async fn link_over_http_then_call_through_the_tool_server() {
    let l = live().await;
    let content = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
    let catalog = Catalog::load(&content, Utc::now().date_naive()).unwrap();
    let (stores, audit) = Stores::in_memory(&content);
    let tokens: Arc<dyn TokenSource> = l.oauth.clone();
    let connectors: Vec<Arc<dyn Connector>> = vec![
        Arc::new(Jira::new(tokens.clone(), l.base.clone())),
        Arc::new(Confluence::new(tokens.clone(), l.base.clone())),
        Arc::new(Slack::new(tokens, l.base.clone())),
        Arc::new(pmos_toolserver::connectors::fake::FakeConnector::docs()),
    ];
    let svc = Arc::new(ToolService::new(catalog, stores, connectors).unwrap());
    let cancel = CancellationToken::new();
    let app = router(
        svc.clone(),
        AppConfig {
            service_token: SERVICE_TOKEN.into(),
            bot: None,
            content_root: content.clone(),
            allowed_hosts: vec!["127.0.0.1".into()],
            oauth: Some(l.oauth.clone()),
        },
        cancel.clone(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ts = format!("http://{}", listener.local_addr().unwrap());
    let ct = cancel.clone();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { ct.cancelled_owned().await })
            .await
            .unwrap();
    });
    let http = reqwest::Client::new();

    // The tool server hands out the authorize URL…
    let start: Value = http
        .get(format!("{ts}/internal/oauth/atlassian/start?user=ana"))
        .bearer_auth(SERVICE_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let state = state_of(start["url"].as_str().unwrap());
    // …and the PM's browser comes back to the public callback.
    let back = http
        .get(format!("{ts}/oauth/atlassian/callback"))
        .query(&[("code", "good-code"), ("state", state.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(back.status(), 200);
    assert!(back.text().await.unwrap().contains("connected"));

    let replay = http
        .get(format!("{ts}/oauth/atlassian/callback"))
        .query(&[("code", "good-code"), ("state", state.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), 400);

    let accounts: Value = http
        .get(format!("{ts}/internal/credentials?user=ana"))
        .bearer_auth(SERVICE_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(accounts["accounts"][0]["provider"], "atlassian");
    assert!(
        !accounts.to_string().contains("at-1"),
        "no tokens in the listing"
    );
    let unauth = http
        .get(format!("{ts}/internal/oauth/atlassian/start?user=ana"))
        .send()
        .await
        .unwrap();
    assert_eq!(unauth.status(), 401);

    // Ana's tool call reaches Jira with her token, wrapped as untrusted.
    let s = svc
        .create_session("ana", "ask", "run-x", None, Utc::now())
        .await
        .unwrap();
    let out = svc
        .call(
            &s,
            "jira_search",
            json!({"jql": "project = PM"}),
            Utc::now(),
        )
        .await
        .unwrap();
    assert_eq!(out["results"][0]["untrusted"], true);
    assert_eq!(out["results"][0]["source"]["id"], "PM-1");
    assert_eq!(
        l.mock.requests("search/jql")[0].auth.as_deref(),
        Some("Bearer at-1")
    );
    assert_eq!(audit.events().last().unwrap().sources, vec!["jira:PM-1"]);

    // Bo hasn't linked: a readable error, no call made.
    let s = svc
        .create_session("bo", "ask", "run-y", None, Utc::now())
        .await
        .unwrap();
    let err = svc
        .call(
            &s,
            "jira_search",
            json!({"jql": "project = PM"}),
            Utc::now(),
        )
        .await
        .unwrap_err();
    assert!(err
        .to_string()
        .contains("connect Atlassian (Jira and Confluence) in PM OS first"));
    assert_eq!(l.mock.requests("search/jql").len(), 1);

    // Unlinking stops further calls.
    let del = http
        .delete(format!("{ts}/internal/credentials/ana/atlassian"))
        .bearer_auth(SERVICE_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), 204);
    let s = svc
        .create_session("ana", "ask", "run-z", None, Utc::now())
        .await
        .unwrap();
    assert!(svc
        .call(&s, "jira_search", json!({"jql": "x"}), Utc::now())
        .await
        .is_err());

    cancel.cancel();
    l.cancel.cancel();
}

// ----- team bots --------------------------------------------------------------------------

/// The scope of the fixture's team bot `scout`, except Confluence `PM` so pages can match.
fn scout_reads() -> pmos_contract::bots::Reads {
    pmos_contract::bots::Reads {
        jira_projects: vec!["PM".into()],
        confluence_spaces: vec!["PM".into()],
        slack_channels: vec!["C0FIELD".into()],
    }
}

#[tokio::test]
async fn a_team_bot_reads_with_its_own_account_inside_its_scope() {
    use pmos_toolserver::connectors::TeamBot;
    let l = live().await;
    put_atlassian(&l, "ana", "at-ana", "rt", 3600).await;
    put_atlassian(&l, "bots.scout", "at-bot", "rt", 3600).await;
    put_slack(&l, "ana", "xoxp-ana").await;
    put_slack(&l, "bots.scout", "xoxp-bot").await;
    let reads = scout_reads();
    let scout = Caller::new("ana", "tsk_1").for_team_bot(TeamBot {
        name: "scout",
        identity: "bots.scout",
        reads: &reads,
    });
    let sent = |path: &str| l.mock.requests(path).len();

    // Jira search: the JQL is wrapped in the scope, and runs with the bot's token.
    l.jira
        .read(
            scout,
            "search",
            &json!({"jql": "text ~ SSO ORDER BY updated DESC"}),
        )
        .await
        .unwrap();
    let search = l.mock.requests("search/jql").pop().unwrap();
    assert_eq!(
        search.query["jql"],
        r#"project in ("PM") AND (text ~ SSO) ORDER BY updated DESC"#
    );
    assert_eq!(search.auth.as_deref(), Some("Bearer at-bot"));
    // A query that would close the group is refused before anything is sent.
    let err = l
        .jira
        .read(scout, "search", &json!({"jql": "x) OR (project = HR"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ConnectorError::BadArgs(_)), "{err:?}");
    assert_eq!(sent("search/jql"), 1);

    // Jira read: the key's project is checked before the call, and again on the issue that
    // comes back (a moved issue changes key).
    let issue = l
        .jira
        .read(scout, "read", &json!({"key": "PM-1"}))
        .await
        .unwrap();
    assert_eq!(issue[0].source.id, "PM-1");
    assert_eq!(
        l.mock.requests("issue/PM-1")[0].auth.as_deref(),
        Some("Bearer at-bot")
    );
    assert!(matches!(
        l.jira.read(scout, "read", &json!({"key": "HR-1"})).await,
        Err(ConnectorError::Forbidden(_))
    ));
    assert_eq!(sent("issue/HR-1"), 0, "no call outside the scope");
    let err = l
        .jira
        .read(scout, "read", &json!({"key": "PM-9"}))
        .await
        .unwrap_err();
    assert!(
        matches!(err, ConnectorError::Forbidden(ref m) if m.contains("SECRET-9")),
        "{err:?}"
    );

    // Confluence search: the CQL is wrapped.
    l.confluence
        .read(scout, "search", &json!({"cql": "type = page"}))
        .await
        .unwrap();
    assert_eq!(
        l.mock.requests("rest/api/search")[0].query["cql"],
        r#"space in ("PM") AND (type = page)"#
    );
    // Confluence read: the page's space is checked after the fetch, before anything returns.
    let page = l
        .confluence
        .read(scout, "read", &json!({"page_id": "555"}))
        .await
        .unwrap();
    assert_eq!(page[0].title, "PRD: SSO");
    let spaces = l.mock.exact("/ex/confluence/cloud-1/wiki/api/v2/spaces");
    assert_eq!(spaces[0].query["keys"], "PM");
    assert_eq!(spaces[0].auth.as_deref(), Some("Bearer at-bot"));
    for (id, why) in [("557", "in HR"), ("556", "no space id")] {
        let err = l
            .confluence
            .read(scout, "read", &json!({"page_id": id}))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ConnectorError::Forbidden(ref m) if m.contains(&format!("page {id}"))),
            "{why}: {err:?}"
        );
        assert_eq!(
            sent(&format!("pages/{id}")),
            1,
            "{why}: fetched, then refused"
        );
    }

    // Slack search: results outside the listed channels are dropped.
    let hits = l
        .slack
        .read(scout, "search", &json!({"query": "launch"}))
        .await
        .unwrap();
    let ids: Vec<_> = hits.iter().map(|h| h.source.id.as_str()).collect();
    assert_eq!(ids, vec!["C0FIELD:1731500002.000100"]);
    assert_eq!(
        l.mock.requests("assistant.search.context")[0]
            .auth
            .as_deref(),
        Some("Bearer xoxp-bot")
    );
    // The PM's own search keeps everything they can see.
    let all = l
        .slack
        .read(ana(), "search", &json!({"query": "launch"}))
        .await
        .unwrap();
    assert_eq!(all.len(), 2);
    // Slack thread: the channel must be listed.
    assert!(matches!(
        l.slack
            .read(
                scout,
                "read_thread",
                &json!({"channel": "C0SECRET", "ts": "1731500001.000100"})
            )
            .await,
        Err(ConnectorError::Forbidden(_))
    ));
    assert_eq!(sent("conversations.replies"), 0);
    l.slack
        .read(
            scout,
            "read_thread",
            &json!({"channel": "C0FIELD", "ts": "1731500000.000100"}),
        )
        .await
        .unwrap();
    assert_eq!(
        l.mock.requests("conversations.replies")[0].auth.as_deref(),
        Some("Bearer xoxp-bot")
    );
    l.cancel.cancel();
}

#[tokio::test]
async fn a_team_bots_own_account_links_without_the_team_yaml_check() {
    // team.yaml is enforced and lists only Ana; the mock signs in as Ana's accounts.
    let team = Team::of(vec![member("ana", "U0ANA", Some("ana@example.com"))]);
    let l = live_with(Some(team)).await;
    link(&l, "bots.scout", Provider::Atlassian).await;
    link(&l, "bots.scout", Provider::Slack).await;
    for provider in [Provider::Atlassian, Provider::Slack] {
        assert!(l.vault.get("bots.scout", provider).await.unwrap().is_some());
    }
    // Only `bots.<valid bot name>` skips the check; a PM's link is still checked.
    for bad in ["bots.", "bots.Scout", "bots.a--b"] {
        assert_eq!(
            l.oauth.authorize_url(bad, Provider::Slack, Utc::now()),
            Err(OAuthError::InvalidUser),
            "{bad}"
        );
    }
    let url = l
        .oauth
        .authorize_url("eve", Provider::Slack, Utc::now())
        .unwrap();
    assert!(matches!(
        l.oauth
            .complete(Provider::Slack, "good-code", &state_of(&url), Utc::now())
            .await,
        Err(OAuthError::WrongAccount(..))
    ));
    l.cancel.cancel();
}

#[tokio::test]
async fn a_team_bot_session_reads_as_the_bot_and_writes_as_the_pm() {
    let l = live().await;
    let (_dir, content) = common::content_tree();
    let catalog = Catalog::load(&content, Utc::now().date_naive()).unwrap();
    let (stores, audit) = Stores::in_memory(&content);
    let tokens: Arc<dyn TokenSource> = l.oauth.clone();
    let connectors: Vec<Arc<dyn Connector>> = vec![
        Arc::new(Jira::new(tokens.clone(), l.base.clone())),
        Arc::new(Confluence::new(tokens.clone(), l.base.clone())),
        Arc::new(Slack::new(tokens, l.base.clone())),
        Arc::new(pmos_toolserver::connectors::fake::FakeConnector::docs()),
    ];
    let svc = Arc::new(ToolService::new(catalog, stores, connectors).unwrap());
    put_atlassian(&l, "ana", "at-ana", "rt", 3600).await;
    put_atlassian(&l, "bots.scout", "at-bot", "rt", 3600).await;
    put_slack(&l, "ana", "xoxp-ana").await;

    let s = svc
        .create_bot_session("ana", "scout", "tsk_7", None, Utc::now())
        .await
        .unwrap();
    let out = svc
        .call(&s, "jira_search", json!({"jql": "text ~ SSO"}), Utc::now())
        .await
        .unwrap();
    assert_eq!(out["results"][0]["untrusted"], true);
    let search = l.mock.requests("search/jql").pop().unwrap();
    assert_eq!(search.auth.as_deref(), Some("Bearer at-bot"));
    assert_eq!(search.query["jql"], r#"project in ("PM") AND (text ~ SSO)"#);

    // scout reads Confluence ENG only: page 555 is in PM.
    let err = svc
        .call(&s, "confluence_read", json!({"page_id": "555"}), Utc::now())
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Forbidden(_)), "{err:?}");
    assert_eq!(audit.events().last().unwrap().outcome, Outcome::Denied);

    // The bot has no Slack account linked: the error says who links it, and Ana's own token
    // is never used instead.
    let err = svc
        .call(
            &s,
            "slack_read_thread",
            json!({"channel": "C0FIELD", "ts": "1731500000.000100"}),
            Utc::now(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "the scout bot has no working Slack account; ask a platform owner to link one for `bots.scout`"
    );
    assert!(l.mock.requests("conversations.replies").is_empty());

    // A write waits for Ana, then runs with Ana's token, not the bot's.
    let args = json!({"project": "PM", "issue_type": "Task", "summary": "Follow up"});
    let out = svc
        .call(&s, "jira_create_issue", args, Utc::now())
        .await
        .unwrap();
    let id = out["approval_id"].as_str().unwrap();
    svc.decide(
        id,
        "ana",
        pmos_toolserver::approvals::Decision::Approve,
        None,
        Utc::now(),
    )
    .await
    .unwrap();
    let created = l.mock.exact("/ex/jira/cloud-1/rest/api/3/issue");
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].auth.as_deref(), Some("Bearer at-ana"));

    // A personal bot reads as its owner, with no scope.
    let mine = svc
        .create_bot_session("ana", "pm-ana", "tsk_8", None, Utc::now())
        .await
        .unwrap();
    svc.call(
        &mine,
        "jira_search",
        json!({"jql": "text ~ SSO"}),
        Utc::now(),
    )
    .await
    .unwrap();
    let search = l.mock.requests("search/jql").pop().unwrap();
    assert_eq!(search.auth.as_deref(), Some("Bearer at-ana"));
    assert_eq!(search.query["jql"], "text ~ SSO");

    // Linking a bot's account: only a team bot, and only with the service token.
    let cancel = CancellationToken::new();
    let app = router(
        svc.clone(),
        AppConfig {
            service_token: SERVICE_TOKEN.into(),
            bot: Some(pmos_toolserver::app::BotAccess {
                token: BOT_TOKEN.into(),
                skills: vec!["docs".into()],
            }),
            content_root: content.clone(),
            allowed_hosts: vec!["127.0.0.1".into()],
            oauth: Some(l.oauth.clone()),
        },
        cancel.clone(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ts = format!("http://{}", listener.local_addr().unwrap());
    let ct = cancel.clone();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { ct.cancelled_owned().await })
            .await
            .unwrap();
    });
    let http = reqwest::Client::new();
    let start = |user: &'static str, token: &'static str| {
        http.get(format!("{ts}/internal/oauth/slack/start?user={user}"))
            .bearer_auth(token)
            .send()
    };
    let res = start("bots.scout", SERVICE_TOKEN).await.unwrap();
    assert_eq!(res.status(), 200);
    let state = state_of(res.json::<Value>().await.unwrap()["url"].as_str().unwrap());
    let back = http
        .get(format!("{ts}/oauth/slack/callback"))
        .query(&[("code", "good-code"), ("state", state.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(back.status(), 200);
    assert!(l
        .vault
        .get("bots.scout", Provider::Slack)
        .await
        .unwrap()
        .is_some());
    for user in ["bots.nobody", "bots.pm-ana"] {
        assert_eq!(
            start(user, SERVICE_TOKEN).await.unwrap().status(),
            404,
            "{user}"
        );
    }
    assert_eq!(start("bots.scout", BOT_TOKEN).await.unwrap().status(), 403);

    cancel.cancel();
    l.cancel.cancel();
}
