//! Who is looking, what they may see, and form (CSRF) protection.
//!
//! - **Identity:** `X-Forwarded-User` from the Spark's SSO proxy, trusted only with
//!   `PMOS_CONSOLE_TRUST_PROXY=1`, and only on a request that also carries
//!   `X-Pmos-Proxy-Secret` equal to `PMOS_CONSOLE_PROXY_SECRET` (compared in constant time,
//!   before the user header is read); otherwise `PMOS_CONSOLE_DEV_USER`. The header may carry
//!   a PM OS handle or the member's email. Neither header's value is ever logged.
//! - **Membership:** the person must be in `team.yaml`, re-read on every request so removing
//!   someone takes effect at once.
//! - **Forms:** every POST carries a token bound to the person (an HMAC under a key made at
//!   start-up) and must come from the console's own origin: `Origin`, or else `Referer`,
//!   must be present and match.

use axum::http::{header, HeaderMap};
use hmac::{Hmac, Mac};
use pmos_contract::model::{Role, TeamFile};
use sha2::Sha256;

use crate::config::Identity;

pub const FORWARDED_USER: &str = "x-forwarded-user";
/// The header the sign-in proxy sets to `PMOS_CONSOLE_PROXY_SECRET` on every request.
pub const PROXY_SECRET: &str = "x-pmos-proxy-secret";

/// Whether the request came through the sign-in proxy: with `PMOS_CONSOLE_TRUST_PROXY=1`, it
/// carries `X-Pmos-Proxy-Secret` equal to the configured secret (constant-time compare). In
/// development there is no proxy, so every request passes.
pub fn from_proxy(identity: &Identity, headers: &HeaderMap) -> bool {
    match identity {
        // Exactly one secret header, so a client's own copy next to the proxy's is refused.
        Identity::Proxy { secret } => {
            let mut presented = headers.get_all(PROXY_SECRET).iter();
            match (presented.next(), presented.next()) {
                (Some(value), None) => value
                    .to_str()
                    .is_ok_and(|presented| pmos_runtime::token_eq(presented, secret)),
                _ => false,
            }
        }
        Identity::Dev(_) => true,
    }
}

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
    /// The request didn't come through the sign-in proxy: no or a wrong
    /// `X-Pmos-Proxy-Secret` (401).
    NotFromProxy,
    /// Nobody is signed in (401).
    NoUser,
    /// Signed in, but not on the team (403).
    NotMember(String),
}

/// Find the person behind a request, among `team`.
pub fn viewer(
    identity: &Identity,
    headers: &HeaderMap,
    team: &TeamFile,
) -> Result<Viewer, AuthError> {
    // The proxy's secret first: until it checks out, X-Forwarded-User is not read at all.
    if !from_proxy(identity, headers) {
        return Err(AuthError::NotFromProxy);
    }
    let presented = match identity {
        Identity::Proxy { .. } => headers
            .get(FORWARDED_USER)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or(AuthError::NoUser)?
            .to_string(),
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
/// A post with neither header is refused: browsers send one on every form post.
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

    fn team() -> TeamFile {
        TeamFile::from_yaml(
            "members:\n  - handle: ana\n    slack-id: U0ANA\n    email: ana@example.com\n    roles: [pm]\n",
        )
        .unwrap()
    }

    #[test]
    fn the_forwarded_user_counts_only_with_the_proxy_secret() {
        let secret = "proxy-secret-0123456789abcdef01234";
        let proxy = Identity::Proxy {
            secret: secret.into(),
        };
        let team = team();
        let user = ("x-forwarded-user", "ana");
        assert_eq!(
            viewer(&proxy, &headers(&[user]), &team),
            Err(AuthError::NotFromProxy),
            "no secret"
        );
        assert_eq!(
            viewer(
                &proxy,
                &headers(&[
                    user,
                    ("x-pmos-proxy-secret", "proxy-secret-0123456789abcdef01235")
                ]),
                &team
            ),
            Err(AuthError::NotFromProxy),
            "a wrong secret"
        );
        assert_eq!(
            viewer(&proxy, &headers(&[("x-pmos-proxy-secret", secret)]), &team),
            Err(AuthError::NoUser)
        );
        let ok = viewer(
            &proxy,
            &headers(&[user, ("x-pmos-proxy-secret", secret)]),
            &team,
        )
        .unwrap();
        assert_eq!(ok.handle, "ana");
        assert!(from_proxy(&Identity::Dev("ana".into()), &headers(&[])));
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
        assert!(
            !same_origin(&headers(&[host]), None),
            "neither Origin nor Referer: refused"
        );
        assert!(!same_origin(
            &headers(&[host, ("referer", "not a url")]),
            None
        ));
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
