//! Who is looking, what they may see, and form (CSRF) protection.
//!
//! - **Identity:** `X-Forwarded-User` from the Spark's SSO proxy, trusted only with
//!   `PMOS_CONSOLE_TRUST_PROXY=1`, and only on a request that also carries
//!   `X-Pmos-Proxy-Secret` equal to `PMOS_CONSOLE_PROXY_SECRET` (compared in constant time,
//!   before the user header is read); otherwise `PMOS_CONSOLE_DEV_USER`. The header may carry
//!   a PM OS handle or the person's sign-in name. Neither header's value is ever logged.
//! - **Who they are:** the PM OS user database, through the tool server's `resolve` route
//!   (`docs/user-database-spec.md` §2), by sign-in name or by handle. An answer is used for at
//!   most 10 seconds ([`pmos_directory::resolve::FRESH_FOR`]), so a deactivation or a removed
//!   role takes effect here within 10 seconds. There is no stale allowance: when the tool
//!   server can't give a fresh answer, the console refuses (`503 users_unavailable`), whatever
//!   answer it kept. The console reads no team file.
//! - **Who may use it:** an active user holding `pm`, `reviewer` or `admin`. An Admin sees
//!   everyone's; anyone else sees their own. An Auditor is not a PM: a person whose only role
//!   is `auditor` gets no console (`403`; the audit log is in the web app).
//! - **Forms:** every POST carries a token bound to the person (an HMAC under a key made at
//!   start-up) and must come from the console's own origin: `Origin`, or else `Referer`,
//!   must be present and match.

use axum::http::{header, HeaderMap};
use hmac::{Hmac, Mac};
use pmos_contract::model::Role;
use pmos_directory::resolve::{DirectoryUser, ResolveClient, ResolveError};
use sha2::Sha256;

use crate::config::Identity;

pub const FORWARDED_USER: &str = "x-forwarded-user";
/// The header the sign-in proxy sets to `PMOS_CONSOLE_PROXY_SECRET` on every request.
pub const PROXY_SECRET: &str = "x-pmos-proxy-secret";

/// The roles that may use the console. `auditor` alone is not one of them.
pub const CONSOLE_ROLES: [&str; 3] = ["pm", "reviewer", "admin"];

/// The longest sign-in name the console will look up.
const MAX_SIGN_IN_NAME: usize = 254;

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

/// The person using the console, as the user database answered for this request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewer {
    pub handle: String,
    /// Active and holding `admin`: sees everything.
    pub admin: bool,
    /// Their known roles, from the same answer: what the console's user assertions carry.
    pub roles: Vec<Role>,
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

    /// The console's decision on one directory answer: status first, then roles.
    pub fn from_user(user: &DirectoryUser) -> Result<Self, AuthError> {
        if !user.is_active() {
            return Err(AuthError::Deactivated);
        }
        if !CONSOLE_ROLES.iter().any(|r| user.has_role(r)) {
            return Err(AuthError::NoConsole);
        }
        Ok(Self {
            handle: user.handle.clone(),
            admin: user.has_role("admin"),
            roles: user.roles.iter().filter_map(|r| Role::parse(r)).collect(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// The request didn't come through the sign-in proxy: no or a wrong
    /// `X-Pmos-Proxy-Secret` (401).
    NotFromProxy,
    /// Nobody is signed in (401).
    NoUser,
    /// Signed in, but not a PM OS user (403).
    NotMember,
    /// A user, but not active (403).
    Deactivated,
    /// Found by a sign-in name that no admin has confirmed yet (403; spec §4).
    SignInUnconfirmed,
    /// An active user with no console role: an Auditor only, or no role at all (403).
    NoConsole,
    /// The tool server couldn't give a fresh answer (`503 users_unavailable`).
    Unavailable,
}

/// Who a request names: a sign-in name (the proxy's header, when it holds an `@`) or a
/// handle. Anything that is neither never reaches the tool server, so junk costs no miss.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Presented {
    SignInName(String),
    Handle(String),
    Junk,
}

fn presented(identity: &Identity, headers: &HeaderMap) -> Result<Presented, AuthError> {
    // The proxy's secret first: until it checks out, X-Forwarded-User is not read at all.
    if !from_proxy(identity, headers) {
        return Err(AuthError::NotFromProxy);
    }
    let value = match identity {
        Identity::Proxy { .. } => headers
            .get(FORWARDED_USER)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or(AuthError::NoUser)?
            .to_string(),
        Identity::Dev(handle) => return Ok(Presented::Handle(handle.clone())),
    };
    Ok(if value.contains('@') {
        let plain = !value.chars().any(|c| c.is_control() || c.is_whitespace());
        if plain && value.len() <= MAX_SIGN_IN_NAME {
            Presented::SignInName(value)
        } else {
            Presented::Junk
        }
    } else if pmos_contract::model::valid_handle(&value) {
        Presented::Handle(value)
    } else {
        Presented::Junk
    })
}

/// Find the person behind a request in the user database, fresh (≤ 10 s) for this request.
/// Logs (in the resolve client) name the kind of lookup, never the value looked up.
pub async fn viewer(
    identity: &Identity,
    headers: &HeaderMap,
    directory: &ResolveClient,
) -> Result<Viewer, AuthError> {
    let (answer, by_sign_in_name) = match presented(identity, headers)? {
        Presented::Junk => return Err(AuthError::NotMember),
        Presented::Handle(h) => (directory.by_handle(&h).await, false),
        Presented::SignInName(n) => (directory.by_sign_in_name(&n).await, true),
    };
    let user = answer.map_err(|e| match e {
        // `429` answers misses only (hits are always answered): nobody by that name.
        ResolveError::NotFound | ResolveError::Invalid | ResolveError::RateLimited => {
            AuthError::NotMember
        }
        // No stale allowance in the console: a kept answer never decides anything.
        ResolveError::Refused { .. } | ResolveError::Unavailable { .. } => AuthError::Unavailable,
    })?;
    if by_sign_in_name && user.is_active() && !user.sign_in_name_confirmed {
        return Err(AuthError::SignInUnconfirmed);
    }
    Viewer::from_user(&user)
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
    use pmos_directory::resolve::{EntraState, Status};

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    fn user(handle: &str, status: Status, roles: &[&str]) -> DirectoryUser {
        DirectoryUser {
            handle: handle.into(),
            display_name: handle.into(),
            status,
            roles: roles.iter().map(|r| r.to_string()).collect(),
            entra: EntraState::None,
            sign_in_name_confirmed: true,
            dual_role: false,
        }
    }

    #[test]
    fn the_forwarded_user_counts_only_with_the_proxy_secret() {
        let secret = "proxy-secret-0123456789abcdef01234";
        let proxy = Identity::Proxy {
            secret: secret.into(),
        };
        let user = ("x-forwarded-user", "ana");
        assert_eq!(
            presented(&proxy, &headers(&[user])),
            Err(AuthError::NotFromProxy),
            "no secret"
        );
        assert_eq!(
            presented(
                &proxy,
                &headers(&[
                    user,
                    ("x-pmos-proxy-secret", "proxy-secret-0123456789abcdef01235")
                ])
            ),
            Err(AuthError::NotFromProxy),
            "a wrong secret"
        );
        assert_eq!(
            presented(&proxy, &headers(&[("x-pmos-proxy-secret", secret)])),
            Err(AuthError::NoUser)
        );
        let with = |who: &str| {
            presented(
                &proxy,
                &headers(&[("x-forwarded-user", who), ("x-pmos-proxy-secret", secret)]),
            )
        };
        assert_eq!(with("ana"), Ok(Presented::Handle("ana".into())));
        assert_eq!(
            with("Ana@Example.com"),
            Ok(Presented::SignInName("Ana@Example.com".into()))
        );
        assert_eq!(with("Ana Smith"), Ok(Presented::Junk));
        assert_eq!(with("a b@example.com"), Ok(Presented::Junk));
        assert_eq!(
            with(&format!("{}@example.com", "a".repeat(250))),
            Ok(Presented::Junk)
        );
        assert!(from_proxy(&Identity::Dev("ana".into()), &headers(&[])));
        assert_eq!(
            presented(
                &Identity::Dev("ana".into()),
                &headers(&[("x-forwarded-user", "root")])
            ),
            Ok(Presented::Handle("ana".into())),
            "in development the header is ignored"
        );
    }

    #[test]
    fn the_viewer_is_decided_from_status_then_roles() {
        let v = Viewer::from_user(&user("ana", Status::Active, &["pm"])).unwrap();
        assert!(!v.admin);
        assert_eq!(v.roles, vec![Role::Pm]);
        assert!(
            Viewer::from_user(&user("root", Status::Active, &["pm", "admin"]))
                .unwrap()
                .admin
        );
        assert!(Viewer::from_user(&user("rae", Status::Active, &["reviewer"])).is_ok());
        // An Auditor is not a PM, and is not an admin either.
        let v = Viewer::from_user(&user("dot", Status::Active, &["pm", "auditor"])).unwrap();
        assert!(!v.admin);
        assert_eq!(
            Viewer::from_user(&user("aud", Status::Active, &["auditor"])),
            Err(AuthError::NoConsole)
        );
        assert_eq!(
            Viewer::from_user(&user("none", Status::Active, &[])),
            Err(AuthError::NoConsole)
        );
        // A deactivated user's kept roles grant nothing, admin included; so does a status
        // this build doesn't know.
        for status in [Status::Deactivated, Status::Other("suspended".into())] {
            assert_eq!(
                Viewer::from_user(&user("old", status, &["pm", "admin"])),
                Err(AuthError::Deactivated)
            );
        }
        // Unknown role names grant nothing and aren't signed.
        let v = Viewer::from_user(&user("ana", Status::Active, &["pm", "wizard"])).unwrap();
        assert_eq!(v.roles, vec![Role::Pm]);
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
