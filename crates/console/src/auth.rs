//! Who is looking, what they may see, and form (CSRF) protection.
//!
//! - **Identity:** `X-Forwarded-User` from the Spark's SSO proxy, trusted only with
//!   `PMOS_CONSOLE_TRUST_PROXY=1` and only on a request that also carries the proxy's shared
//!   secret in `X-Pmos-Proxy-Secret`; otherwise `PMOS_CONSOLE_DEV_USER`. The user header may
//!   carry a PM OS handle or the member's email. When the secret check fails, the user header
//!   is not read, and neither header's value is ever logged.
//! - **Membership:** the person must be in `team.yaml`, re-read on every request so removing
//!   someone takes effect at once.
//! - **Forms:** every POST carries a token bound to the person (an HMAC under a key made at
//!   start-up) and must come from the console's own origin.

use axum::http::{header, HeaderMap};
use hmac::{Hmac, Mac};
use pmos_contract::model::{Role, TeamFile};
use sha2::Sha256;

use crate::config::Identity;

pub const FORWARDED_USER: &str = "x-forwarded-user";
/// The reverse proxy's shared secret (`PMOS_CONSOLE_PROXY_SECRET`), on every request.
pub const PROXY_SECRET: &str = "x-pmos-proxy-secret";

/// The person using the console.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewer {
    pub handle: String,
    /// Role `admin` in `team.yaml`: sees everything.
    pub admin: bool,
}

impl Viewer {
    /// A PM sees their own bots plus the team bots; an admin sees every bot.
    pub fn sees_bot(&self, scope: &str, owner: &str) -> bool {
        self.admin || scope == "team" || owner == self.handle
    }

    /// Routines and tasks: a PM sees their own; an admin sees everyone's.
    pub fn sees_own(&self, handle: &str) -> bool {
        self.admin || handle == self.handle
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// Nobody is signed in (401).
    NoUser,
    /// The request didn't come through the proxy: its shared secret is missing or wrong
    /// (401). The forwarded user is not read.
    NotFromProxy,
    /// Signed in, but not on the team (403).
    NotMember(String),
}

/// Find the person behind a request, among `team`.
pub fn viewer(
    identity: &Identity,
    headers: &HeaderMap,
    team: &TeamFile,
) -> Result<Viewer, AuthError> {
    let presented = match identity {
        Identity::Proxy(secret) => {
            // The secret first: without it the user header isn't even read. Exactly one
            // secret header, so a client's own copy next to the proxy's is refused too.
            let mut presented = headers.get_all(PROXY_SECRET).iter();
            let from_proxy = match (presented.next(), presented.next()) {
                (Some(value), None) => secret.matches(value.as_bytes()),
                _ => false,
            };
            if !from_proxy {
                return Err(AuthError::NotFromProxy);
            }
            headers
                .get(FORWARDED_USER)
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .ok_or(AuthError::NoUser)?
                .to_string()
        }
        Identity::Dev(handle) => handle.clone(),
    };
    let member = team
        .by_handle(&presented)
        .or_else(|| {
            team.members.iter().find(|m| {
                presented.contains('@')
                    && m.email
                        .as_deref()
                        .is_some_and(|e| e.eq_ignore_ascii_case(&presented))
            })
        })
        .ok_or(AuthError::NotMember(presented))?;
    Ok(Viewer {
        handle: member.handle.clone(),
        admin: member.roles.contains(&Role::Admin),
    })
}

/// Signs form tokens. The key is made fresh at each start, so a restart invalidates forms
/// left open in a browser (the page just needs a reload).
#[derive(Clone)]
pub struct Csrf {
    key: [u8; 32],
}

impl Csrf {
    pub fn new() -> Self {
        let mut key = [0u8; 32];
        key[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        key[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        Self { key }
    }

    /// The form token for this person.
    pub fn token(&self, handle: &str) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).expect("any key length");
        mac.update(b"pmos-console form\0");
        mac.update(handle.as_bytes());
        mac.finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    pub fn verify(&self, handle: &str, presented: &str) -> bool {
        pmos_runtime::token_eq(presented, &self.token(handle))
    }
}

impl Default for Csrf {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether a form post came from the console's own pages: never from another site, and with
/// an `Origin` (or else `Referer`) matching `public_url`, or the request's `Host` without it.
/// A post with neither header relies on the form token alone.
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

    #[test]
    fn form_tokens_are_bound_to_the_person() {
        let csrf = Csrf::new();
        let ana = csrf.token("ana");
        assert_eq!(ana.len(), 64);
        assert!(csrf.verify("ana", &ana));
        assert!(!csrf.verify("ben", &ana));
        assert!(!csrf.verify("ana", ""));
        assert!(
            !Csrf::new().verify("ana", &ana),
            "a restart makes a new key"
        );
    }

    #[test]
    fn checks_where_a_form_came_from() {
        let host = ("host", "127.0.0.1:8760");
        assert!(same_origin(
            &headers(&[host, ("origin", "http://127.0.0.1:8760")]),
            None
        ));
        assert!(!same_origin(
            &headers(&[host, ("origin", "https://evil.example")]),
            None
        ));
        assert!(!same_origin(&headers(&[host, ("origin", "null")]), None));
        assert!(same_origin(
            &headers(&[host, ("referer", "http://127.0.0.1:8760/routines")]),
            None
        ));
        assert!(!same_origin(
            &headers(&[host, ("referer", "http://evil.example/x")]),
            None
        ));
        assert!(!same_origin(
            &headers(&[host, ("sec-fetch-site", "cross-site")]),
            None
        ));
        assert!(same_origin(&headers(&[host]), None));
        let public = Some("https://console.pmos.internal");
        assert!(same_origin(
            &headers(&[
                ("host", "10.0.0.2:8760"),
                ("origin", "https://console.pmos.internal")
            ]),
            public
        ));
        assert!(!same_origin(
            &headers(&[
                ("host", "10.0.0.2:8760"),
                ("origin", "http://10.0.0.2:8760")
            ]),
            public
        ));
    }
}
