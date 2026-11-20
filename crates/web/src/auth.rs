//! Sign-in providers and the origin check for writes.
//!
//! A provider proves who the browser is: development sign-in names a handle, Entra ID an
//! Entra key `(tid, oid)` and a sign-in name. The app then looks the person up in the user
//! database (the tool server's `resolve`) and starts a session; a provider never decides
//! membership or roles.

use async_trait::async_trait;
use axum::http::{header, HeaderMap};
use chrono::{DateTime, Utc};
use pmos_contract::model::valid_handle;

use crate::entra::Failure;

/// What the browser sent to sign in.
#[derive(Clone)]
pub enum SignInInput {
    /// `POST /api/auth/dev-login {handle}`: a handle.
    Dev { handle: String },
    /// `GET /api/auth/callback`: the authorization code, with the `nonce` and PKCE verifier
    /// of the flow its `state` matched, and the time to check the ID token against.
    Entra {
        code: String,
        nonce: String,
        verifier: String,
        now: DateTime<Utc>,
    },
}

/// Never shows a code, nonce or verifier.
impl std::fmt::Debug for SignInInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignInInput::Dev { handle } => f.debug_struct("Dev").field("handle", handle).finish(),
            SignInInput::Entra { now, .. } => f
                .debug_struct("Entra")
                .field("code", &"<redacted>")
                .field("now", now)
                .finish(),
        }
    }
}

/// Who a provider says the person is: a PM OS handle (development sign-in) or an Entra
/// object id, to find in the user database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claimed(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignInError {
    /// This provider doesn't take that kind of input.
    Unsupported,
    /// The input is malformed (`400`).
    Invalid(String),
    /// The identity provider's answer didn't check out, or it couldn't be reached.
    Rejected(Failure),
}

/// One way to sign in: [`crate::entra::Entra`] (production) or [`DevLoginProvider`].
#[async_trait]
pub trait AuthProvider: Send + Sync {
    /// A short name for logs, such as `dev`.
    fn name(&self) -> &'static str;

    /// Whether this provider lets anyone who reaches the port sign in as anyone. The server
    /// logs a warning at start-up for such a provider.
    fn is_insecure(&self) -> bool;

    /// Check the input and say who it proves the person is.
    async fn authenticate(&self, input: &SignInInput) -> Result<Claimed, SignInError>;
}

/// Development sign-in (`PMOS_WEB_DEV_LOGIN=1`): the body names the handle and that is
/// believed. Never expose a server with it on: anyone who can reach the port can sign in as
/// any member, admins included.
pub struct DevLoginProvider;

#[async_trait]
impl AuthProvider for DevLoginProvider {
    fn name(&self) -> &'static str {
        "dev"
    }

    fn is_insecure(&self) -> bool {
        true
    }

    async fn authenticate(&self, input: &SignInInput) -> Result<Claimed, SignInError> {
        let SignInInput::Dev { handle } = input else {
            return Err(SignInError::Unsupported);
        };
        let handle = handle.trim();
        if !valid_handle(handle) {
            return Err(SignInError::Invalid("That isn't a PM OS handle.".into()));
        }
        Ok(Claimed(handle.to_string()))
    }
}

/// Password sign-in (`PMOS_WEB_PASSWORD_LOGIN=1`, `docs/password-sign-in.md`): listed so the
/// app shows its form. Its routes (`app::password`) hand what the person typed to the tool
/// server, which decides; this provider takes no input itself.
pub struct PasswordProvider;

#[async_trait]
impl AuthProvider for PasswordProvider {
    fn name(&self) -> &'static str {
        "password"
    }

    fn is_insecure(&self) -> bool {
        false
    }

    async fn authenticate(&self, _input: &SignInInput) -> Result<Claimed, SignInError> {
        Err(SignInError::Unsupported)
    }
}

/// Whether a write came from the web app's own pages: never from another site, and with an
/// `Origin` (or else `Referer`) matching `public_url`, or the request's `Host` without it. A
/// request with neither header is refused: every browser sends `Origin` on a `POST`, `PUT`
/// or `DELETE` from `fetch`, so a write without one isn't the app's (the CSRF token is
/// checked as well, not instead).
pub fn same_origin(headers: &HeaderMap, public_url: Option<&str>) -> bool {
    let get = |name: header::HeaderName| headers.get(name).and_then(|v| v.to_str().ok());
    if let Some(site) = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        if !matches!(site, "same-origin" | "none") {
            return false;
        }
    }
    let origin = match get(header::ORIGIN) {
        Some(o) => o.to_string(),
        None => match get(header::REFERER).and_then(|r| reqwest::Url::parse(r).ok()) {
            Some(url) => url.origin().ascii_serialization(),
            None => return false,
        },
    };
    if origin == "null" {
        return false;
    }
    match public_url {
        Some(public) => origin.eq_ignore_ascii_case(public.trim_end_matches('/')),
        None => {
            let Some(host) = get(header::HOST) else {
                return false;
            };
            let authority = origin
                .strip_prefix("https://")
                .or_else(|| origin.strip_prefix("http://"));
            authority.is_some_and(|a| a.eq_ignore_ascii_case(host))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[tokio::test]
    async fn dev_login_believes_a_well_formed_handle() {
        let p = DevLoginProvider;
        assert!(p.is_insecure());
        assert_eq!(
            p.authenticate(&SignInInput::Dev {
                handle: " ana ".into()
            })
            .await,
            Ok(Claimed("ana".into()))
        );
        for bad in ["Ana Smith", "ana@example.com"] {
            assert!(matches!(
                p.authenticate(&SignInInput::Dev { handle: bad.into() })
                    .await,
                Err(SignInError::Invalid(_))
            ));
        }
    }

    #[test]
    fn checks_where_a_write_came_from() {
        let host = ("host", "127.0.0.1:8770");
        assert!(same_origin(
            &headers(&[host, ("origin", "http://127.0.0.1:8770")]),
            None
        ));
        assert!(!same_origin(
            &headers(&[host, ("origin", "https://evil.example")]),
            None
        ));
        assert!(!same_origin(&headers(&[host, ("origin", "null")]), None));
        assert!(!same_origin(
            &headers(&[host, ("referer", "http://evil.example/x")]),
            None
        ));
        assert!(!same_origin(
            &headers(&[host, ("sec-fetch-site", "cross-site")]),
            None
        ));
        assert!(!same_origin(
            &headers(&[host, ("sec-fetch-site", "same-site")]),
            None
        ));
        // Neither `Origin` nor `Referer`: refused, with or without a public URL.
        assert!(!same_origin(&headers(&[host]), None));
        assert!(!same_origin(
            &headers(&[host, ("sec-fetch-site", "same-origin")]),
            Some("https://pmos.example.com")
        ));
        // A same-origin `Referer` alone will do.
        assert!(same_origin(
            &headers(&[host, ("referer", "http://127.0.0.1:8770/#/d/apr_1")]),
            None
        ));
        let public = Some("https://pmos.example.com");
        assert!(same_origin(
            &headers(&[
                ("host", "10.0.0.2:8770"),
                ("origin", "https://pmos.example.com")
            ]),
            public
        ));
        assert!(!same_origin(
            &headers(&[
                ("host", "10.0.0.2:8770"),
                ("origin", "http://10.0.0.2:8770")
            ]),
            public
        ));
    }
}
