//! The value rules for every field. Nothing here is free text: each field has a small,
//! header-safe alphabet and a length bound.

use pmos_contract::model::{valid_handle, Role};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Longest key id, issuer or audience name.
pub const MAX_NAME_LEN: usize = 32;
/// Longest session id.
pub const MAX_SESSION_LEN: usize = 64;
/// Longest HTTP method in a request binding.
pub const MAX_METHOD_LEN: usize = 16;
/// Longest request target (path and query) in a request binding.
pub const MAX_PATH_LEN: usize = 4096;

/// A field value that breaks its rule. The input is never echoed back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FieldError {
    #[error("the issuer is not a valid client name")]
    Issuer,
    #[error("the audience is not a valid client name")]
    Audience,
    #[error("the subject is not a valid PM OS handle")]
    Subject,
    #[error("a role is not one this build knows")]
    Role,
    #[error("the session id is empty, too long or has a character outside A-Z a-z 0-9 _ -")]
    Session,
    #[error("the key id is not a valid name")]
    KeyId,
    #[error("the HTTP method is not 1 to 16 uppercase letters")]
    Method,
    #[error("the request path does not start with / or has a character outside visible ASCII")]
    Path,
}

/// A client name, audience or key id: 1 to 32 characters, a lowercase letter or digit first,
/// then lowercase letters, digits, `.`, `_` and `-`.
pub(crate) fn valid_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= MAX_NAME_LEN
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"._-".contains(&c))
}

/// A session id: opaque, 1 to 64 characters of `A-Z a-z 0-9 _ -`.
pub(crate) fn valid_session(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= MAX_SESSION_LEN
        && b.iter()
            .all(|&c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

/// A subject: the contract's handle rule (which also rejects the empty string).
pub(crate) fn valid_subject(s: &str) -> bool {
    !s.is_empty() && valid_handle(s)
}

/// Position of a known role in [`Role::KNOWN`]; the canonical order of the `roles` field.
pub(crate) fn role_rank(role: &Role) -> Option<usize> {
    if !role.is_known() {
        return None;
    }
    Role::KNOWN.iter().position(|k| *k == role.as_str())
}

/// Known roles, deduplicated, in canonical order. Unknown roles are refused.
pub(crate) fn canonical_roles(roles: &[Role]) -> Result<Vec<Role>, FieldError> {
    let mut ranked = roles
        .iter()
        .map(|r| role_rank(r).map(|i| (i, r.clone())).ok_or(FieldError::Role))
        .collect::<Result<Vec<_>, _>>()?;
    ranked.sort_by_key(|(i, _)| *i);
    ranked.dedup_by_key(|(i, _)| *i);
    Ok(ranked.into_iter().map(|(_, r)| r).collect())
}

/// What a service is allowed to do for whom: everything in an assertion except the key, the
/// times, the nonce and the request binding. Built with [`Claims::new`], which checks every
/// field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claims {
    pub(crate) issuer: String,
    pub(crate) audience: String,
    pub(crate) subject: String,
    pub(crate) roles: Vec<Role>,
    pub(crate) session: String,
}

impl Claims {
    /// - `issuer`: the calling service's client name, for example `web` or `console`.
    /// - `audience`: the service that must accept it, for example `bots` or `toolserver`.
    /// - `subject`: the signed-in person's PM OS handle.
    /// - `roles`: their known roles (deduplicated and put in canonical order; an unknown role
    ///   is refused, because it grants nothing and would not survive the strict parser).
    /// - `session`: the caller's opaque session id for the person.
    pub fn new(
        issuer: &str,
        audience: &str,
        subject: &str,
        roles: &[Role],
        session: &str,
    ) -> Result<Self, FieldError> {
        if !valid_name(issuer) {
            return Err(FieldError::Issuer);
        }
        if !valid_name(audience) {
            return Err(FieldError::Audience);
        }
        if !valid_subject(subject) {
            return Err(FieldError::Subject);
        }
        let roles = canonical_roles(roles)?;
        if !valid_session(session) {
            return Err(FieldError::Session);
        }
        Ok(Self {
            issuer: issuer.to_string(),
            audience: audience.to_string(),
            subject: subject.to_string(),
            roles,
            session: session.to_string(),
        })
    }

    pub fn issuer(&self) -> &str {
        &self.issuer
    }
    pub fn audience(&self) -> &str {
        &self.audience
    }
    pub fn subject(&self) -> &str {
        &self.subject
    }
    pub fn roles(&self) -> &[Role] {
        &self.roles
    }
    pub fn session(&self) -> &str {
        &self.session
    }
}

/// A digest of one HTTP request: its method, its target (path and query, exactly as the
/// receiving service sees it) and a SHA-256 of its body. An assertion bound to it is refused
/// on any other request.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RequestBinding {
    pub(crate) digest: [u8; 32],
}

impl std::fmt::Debug for RequestBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestBinding").finish_non_exhaustive()
    }
}

impl RequestBinding {
    /// Bind to `method` (uppercase, for example `POST`), `path` (starting with `/`, query
    /// included) and the raw `body` bytes (empty for none).
    pub fn new(method: &str, path: &str, body: &[u8]) -> Result<Self, FieldError> {
        Self::with_body_digest(method, path, Sha256::digest(body).into())
    }

    /// The same, with the body's SHA-256 already computed (for a streamed body).
    pub fn with_body_digest(
        method: &str,
        path: &str,
        body_sha256: [u8; 32],
    ) -> Result<Self, FieldError> {
        let m = method.as_bytes();
        if m.is_empty() || m.len() > MAX_METHOD_LEN || !m.iter().all(u8::is_ascii_uppercase) {
            return Err(FieldError::Method);
        }
        let p = path.as_bytes();
        if p.first() != Some(&b'/')
            || p.len() > MAX_PATH_LEN
            || !p.iter().all(|c| (0x21..=0x7e).contains(c))
        {
            return Err(FieldError::Path);
        }
        // Method and path cannot contain NUL, so the separators make the encoding injective.
        let mut h = Sha256::new();
        h.update(b"pmos-assertion request v1\0");
        h.update(m);
        h.update(b"\0");
        h.update(p);
        h.update(b"\0");
        h.update(body_sha256);
        Ok(Self {
            digest: h.finalize().into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for ok in [
            "web",
            "console",
            "bots",
            "toolserver",
            "k1",
            "2026-11",
            "a.b_c-d",
        ] {
            assert!(valid_name(ok), "{ok}");
        }
        let long = "a".repeat(MAX_NAME_LEN + 1);
        for bad in [
            "",
            "Web",
            "-web",
            ".web",
            "we b",
            "web;",
            "web=",
            "wéb",
            long.as_str(),
        ] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        assert!(valid_name(&"a".repeat(MAX_NAME_LEN)));
    }

    #[test]
    fn sessions() {
        assert!(valid_session("ses_0123ABCdef-_"));
        assert!(valid_session(&"x".repeat(MAX_SESSION_LEN)));
        for bad in ["", "a.b", "a;b", "a b", "a=b", "a,b"] {
            assert!(!valid_session(bad), "{bad:?}");
        }
        assert!(!valid_session(&"x".repeat(MAX_SESSION_LEN + 1)));
    }

    #[test]
    fn subjects_follow_the_contract_handle_rule() {
        for ok in ["ana", "ana.silva", "bots.docs", "a_b-c", "0ana"] {
            assert!(valid_subject(ok), "{ok}");
        }
        for bad in ["", "Ana", ".ana", "ana;x", "ana@x.io", "ana silva"] {
            assert!(!valid_subject(bad), "{bad:?}");
        }
        assert!(!valid_subject(&"a".repeat(65)));
    }

    #[test]
    fn claims_check_every_field() {
        let ok = |i, a, s, r: &[Role], d| Claims::new(i, a, s, r, d);
        assert!(ok("web", "bots", "ana", &[Role::Pm], "s1").is_ok());
        assert_eq!(
            ok("Web", "bots", "ana", &[], "s1").unwrap_err(),
            FieldError::Issuer
        );
        assert_eq!(
            ok("web", "", "ana", &[], "s1").unwrap_err(),
            FieldError::Audience
        );
        assert_eq!(
            ok("web", "bots", "Ana", &[], "s1").unwrap_err(),
            FieldError::Subject
        );
        assert_eq!(
            ok("web", "bots", "ana", &[Role::Unknown("boss".into())], "s1").unwrap_err(),
            FieldError::Role
        );
        assert_eq!(
            ok("web", "bots", "ana", &[], "s;1").unwrap_err(),
            FieldError::Session
        );
    }

    #[test]
    fn roles_are_deduplicated_and_ordered() {
        let c = Claims::new(
            "web",
            "bots",
            "ana",
            &[Role::Auditor, Role::Pm, Role::Admin, Role::Pm],
            "s1",
        )
        .unwrap();
        assert_eq!(c.roles(), &[Role::Pm, Role::Admin, Role::Auditor]);
    }

    #[test]
    fn request_bindings() {
        let a = RequestBinding::new("POST", "/v1/tasks", b"{}").unwrap();
        assert_eq!(a, RequestBinding::new("POST", "/v1/tasks", b"{}").unwrap());
        assert_ne!(a, RequestBinding::new("PUT", "/v1/tasks", b"{}").unwrap());
        assert_ne!(
            a,
            RequestBinding::new("POST", "/v1/tasks/x", b"{}").unwrap()
        );
        assert_ne!(a, RequestBinding::new("POST", "/v1/tasks", b"{ }").unwrap());
        assert_ne!(
            a,
            RequestBinding::new("POST", "/v1/tasks?x=1", b"{}").unwrap()
        );
        let digest: [u8; 32] = Sha256::digest(b"{}").into();
        assert_eq!(
            a,
            RequestBinding::with_body_digest("POST", "/v1/tasks", digest).unwrap()
        );
        assert!(RequestBinding::new(&"A".repeat(MAX_METHOD_LEN), "/", b"").is_ok());
        let long_method = "A".repeat(MAX_METHOD_LEN + 1);
        for m in ["", "post", "PO ST", "GET;", long_method.as_str()] {
            assert_eq!(
                RequestBinding::new(m, "/", b"").unwrap_err(),
                FieldError::Method
            );
        }
        let long = format!("/{}", "a".repeat(MAX_PATH_LEN));
        for p in ["", "v1", "/a b", "/a\nb", "/é", long.as_str()] {
            assert_eq!(
                RequestBinding::new("GET", p, b"").unwrap_err(),
                FieldError::Path,
                "{p:?}"
            );
        }
    }
}
