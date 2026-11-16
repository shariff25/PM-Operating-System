//! Sign-in providers and the origin check for writes.
//!
//! A provider proves who the browser is and names them by a handle or an email. The app then
//! looks that up in `team.yaml` (re-read on every request) and starts a session; a provider
//! never decides membership or roles.

use async_trait::async_trait;
use axum::http::{header, HeaderMap};
use pmos_contract::model::{valid_email, valid_handle, TeamFile, TeamMember};

/// What the browser sent to sign in.
#[derive(Debug, Clone)]
pub enum SignInInput {
    /// `POST /api/auth/dev-login {handle}`: a handle, or a member's email.
    Dev { handle: String },
}

/// Who a provider says the person is: a PM OS handle or an email, to find in `team.yaml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claimed(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignInError {
    /// This provider doesn't take that kind of input.
    Unsupported,
    /// The input is malformed (`400`).
    Invalid(String),
}

/// One way to sign in. Entra ID will be the production provider (see [`EntraProvider`]).
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
        let SignInInput::Dev { handle } = input;
        let handle = handle.trim();
        if !valid_handle(handle) && !valid_email(handle) {
            return Err(SignInError::Invalid(
                "That isn't a PM OS handle or an email.".into(),
            ));
        }
        Ok(Claimed(handle.to_string()))
    }
}

/// **Not built.** The seam for Entra ID (Azure AD) sign-in, the production provider in
/// `docs/web-client.md`. It deliberately does not implement [`AuthProvider`] yet, and nothing
/// constructs it, so a server can't start believing it works.
///
/// What it will need:
/// - **Config:** tenant id, client id, a client secret or certificate (from the secret
///   store, never the environment file in git), and the redirect URL
///   (`<PMOS_WEB_PUBLIC_URL>/api/auth/entra/callback`).
/// - **Routes:** `GET /api/auth/entra/start` (authorization code flow with PKCE, a `state`
///   and `nonce` kept in a short-lived `__Host-` cookie) and `GET /api/auth/entra/callback`.
///   Both would add a [`SignInInput`] variant.
/// - **Checks:** the ID token's signature against the tenant's published keys, `iss`, `aud`,
///   `exp`, `nonce`; then map `preferred_username` or `email` (or better, the `oid` once the
///   user database stores it) to a member.
/// - **Not decided** (`docs/web-client.md`): which claims, group-to-role mapping, and the
///   fallback when Entra is down.
pub struct EntraProvider {
    pub tenant_id: String,
    pub client_id: String,
    pub redirect_url: String,
}

/// The member a claim names: by handle, or by email (case-insensitive) when it has an `@`.
pub fn find_member<'a>(team: &'a TeamFile, claimed: &str) -> Option<&'a TeamMember> {
    team.by_handle(claimed).or_else(|| {
        team.members.iter().find(|m| {
            claimed.contains('@')
                && m.email
                    .as_deref()
                    .is_some_and(|e| e.eq_ignore_ascii_case(claimed))
        })
    })
}

/// Whether a write came from the web app's own pages: never from another site, and with an
/// `Origin` (or else `Referer`) matching `public_url`, or the request's `Host` without it. A
/// request with neither header relies on the CSRF token and `SameSite=Lax` alone. The same
/// rule as the console's forms.
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
            None => return true,
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
        assert!(matches!(
            p.authenticate(&SignInInput::Dev {
                handle: "Ana Smith".into()
            })
            .await,
            Err(SignInError::Invalid(_))
        ));
    }

    #[test]
    fn finds_members_by_handle_or_email() {
        let team = TeamFile::from_yaml(
            "members:\n  - {handle: ana, slack-id: U0ANA, email: ana@example.com, roles: [pm]}\n",
        )
        .unwrap();
        assert!(find_member(&team, "ana").is_some());
        assert!(find_member(&team, "ANA@example.com").is_some());
        assert!(find_member(&team, "ben").is_none());
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
        assert!(same_origin(&headers(&[host]), None));
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
