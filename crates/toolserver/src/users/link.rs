//! The first Entra link (`docs/user-database-spec.md` §7 step 2; slice U2).
//!
//! `pmos-web` found nobody by `(tid, oid)` and one user by the token's `preferred_username`
//! (`resolve?by=sign_in_name`); it now asks to store the token's `(tid, oid)` as that user's
//! `entra` identity. Since the review of `b469d5b` (U1) it sends the raw ID token, which the
//! route validates itself (`app/directory.rs`, `pmos_entra::Verifier`): the [`EntraLink`]
//! planned here is made only from a validated token's `tid`, `oid` and `preferred_username`,
//! never from values `pmos-web` states. [`plan`] re-checks every condition against the
//! directory read in the same transaction, under the users lock:
//!
//! - the user exists and their sign-in name is the one presented (else `not_found`, which
//!   counts as a miss for the caller's rate limit, so the route is no better an oracle than
//!   `resolve`);
//! - the user has no Entra identity yet (one with the same key is answered as already linked,
//!   writing nothing; a different one is a **conflict**, audited `users.identity_conflict`);
//! - no other user holds that `(tid, oid)` (else a conflict too);
//! - the user is `active`, isn't the `evals` account, holds neither `admin` nor `auditor`
//!   (those are linked on the host only, §5), and their sign-in name is **confirmed** (§4).
//!
//! Two links racing for one key: the database's unique key `(kind, issuer, subject)` on
//! `user_identities` lets exactly one commit; `(handle, kind)` does the same for two keys
//! racing for one person. The lock only makes the loser's refusal a clear one.
//!
//! Every outcome is audited with fixed details (`detail::is_safe` accepts them): `entra` for a
//! link and a conflict, `entra; <code>` for any other refusal. Never an address, a `tid` or an
//! `oid`. A refusal is recorded as the `anonymous` actor (the caller has proven nothing yet),
//! with the target handle only when that user exists.

use chrono::{DateTime, Utc};

use super::rules::{self, UserEvent, Write};
use super::{
    detail, Actor, ChangeKind, Directory, EntraKey, Identity, IdentityKind, RoleChange, User,
    UserRole, Via, EVALS,
};
use crate::audit::Outcome;
use crate::store::StoreError;

/// The actor a refused link is recorded as: the caller has proven nothing yet.
pub const ANONYMOUS: &str = "anonymous";

/// A first link to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntraLink {
    pub handle: String,
    /// The token's `preferred_username`: it must be the user's stored sign-in name.
    pub sign_in_name: String,
    pub key: EntraKey,
}

/// Why a first link was refused. [`LinkRefusal::code`] is the API's error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkRefusal {
    /// A malformed `tid`, `oid` or sign-in name.
    Invalid,
    /// No such user, or the sign-in name isn't theirs.
    NotFound,
    NotActive,
    /// The sign-in name came through the Slack bot client and no admin confirmed it (§4).
    Unconfirmed,
    /// An Admin or Auditor: the host sets their link (§5, §7).
    HostOnly,
    EvalsAccount,
    /// The user already has a different Entra identity (a conflict).
    Linked,
    /// Another user holds this `(tid, oid)` (a conflict).
    Taken,
    /// The ID token failed the route's own check (signature, issuer, audience, time, nonce,
    /// tenant, `oid`).
    InvalidToken,
    /// This ID token was already presented for a first link (a replay).
    TokenUsed,
    /// The ID token is a guest's (`acct`) or another identity provider's (`idp`).
    NotAMember,
    /// The ID token has no `acct` claim: the app registration lacks the optional claim.
    EntraMisconfigured,
}

impl LinkRefusal {
    pub const ALL: [LinkRefusal; 12] = [
        Self::Invalid,
        Self::NotFound,
        Self::NotActive,
        Self::Unconfirmed,
        Self::HostOnly,
        Self::EvalsAccount,
        Self::Linked,
        Self::Taken,
        Self::InvalidToken,
        Self::TokenUsed,
        Self::NotAMember,
        Self::EntraMisconfigured,
    ];

    /// The API's error code.
    pub fn code(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::NotFound => "not_found",
            Self::NotActive => "not_active",
            Self::Unconfirmed => "sign_in_unconfirmed",
            Self::HostOnly => "host_only",
            Self::EvalsAccount => "evals_account",
            Self::Linked => "linked",
            Self::Taken => "taken",
            Self::InvalidToken => "invalid_token",
            Self::TokenUsed => "token_used",
            Self::NotAMember => "not_a_member",
            Self::EntraMisconfigured => "entra_misconfigured",
        }
    }

    /// The refusal for a token the route's own check refused.
    pub fn from_token(f: &pmos_entra::Failure) -> Self {
        match f {
            pmos_entra::Failure::NotMember(_) => Self::NotAMember,
            pmos_entra::Failure::Misconfigured => Self::EntraMisconfigured,
            _ => Self::InvalidToken,
        }
    }

    /// A conflict (§7 step 3): audited `users.identity_conflict` and flagged for admins.
    pub fn conflict(self) -> bool {
        matches!(self, Self::Linked | Self::Taken)
    }

    /// The audit detail: `entra` for a conflict, else `entra; <part>` from `detail.rs`'s
    /// fixed grammar.
    pub fn detail(self) -> String {
        let part = match self {
            Self::Linked | Self::Taken => return "entra".into(),
            Self::Unconfirmed => "sign_in unconfirmed",
            other => other.code(),
        };
        format!("entra; {part}")
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::Invalid => {
                "the body is {\"id_token\", \"nonce\"}, and the token's sign-in name an email address"
            }
            Self::InvalidToken => "the ID token didn't pass this server's own check",
            Self::TokenUsed => "this ID token was already used for a first link",
            Self::NotAMember => "the ID token is a guest's or another identity provider's",
            Self::EntraMisconfigured => {
                "the ID token has no acct claim: add it in the app registration's Token configuration"
            }
            Self::NotFound => "no user has that handle and sign-in name",
            Self::NotActive => "this user is deactivated",
            Self::Unconfirmed => {
                "an admin must confirm this user's Microsoft sign-in name in the web app first"
            }
            Self::HostOnly => {
                "an Admin's or Auditor's Microsoft account is linked on the host (pmos-admin users set-entra)"
            }
            Self::EvalsAccount => "the evals account never has an Entra identity",
            Self::Linked => "this user is already linked to a different Microsoft account",
            Self::Taken => "this Microsoft account is linked to another user",
        }
    }
}

/// A refused link, or a storage failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    #[error("{}", .0.message())]
    Refused(LinkRefusal),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// What a link did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Linked {
    /// The user, with the identity.
    pub user: User,
    /// `false` when the user already had exactly this identity (nothing was written).
    pub first_link: bool,
}

pub type LinkResult = Result<Linked, LinkError>;

/// A planned link: the identity write, its `role_changes` row and its audit event.
#[derive(Debug, Clone)]
pub struct LinkPlan {
    pub write: Write,
    pub change: RoleChange,
    pub event: UserEvent,
    /// The actor the event is recorded as (the user, through `pmos-web`).
    pub actor: Actor,
    /// The user after the write.
    pub user: User,
}

/// What [`plan`] decided.
#[derive(Debug, Clone)]
pub enum Planned {
    Link(Box<LinkPlan>),
    /// Already linked with exactly this key: nothing to write.
    Same(Box<User>),
}

/// The values as stored: GUIDs and the sign-in name lower-case.
pub fn normalize(link: &EntraLink) -> Result<EntraLink, LinkRefusal> {
    let tid = rules::guid(&link.key.tid, "tenant id").map_err(|_| LinkRefusal::Invalid)?;
    let oid = rules::guid(&link.key.oid, "object id").map_err(|_| LinkRefusal::Invalid)?;
    let name =
        rules::address(&link.sign_in_name, "sign-in name").map_err(|_| LinkRefusal::Invalid)?;
    Ok(EntraLink {
        handle: link.handle.clone(),
        sign_in_name: name,
        key: EntraKey { tid, oid },
    })
}

/// Check every first-link condition against `dir` (read under the users lock).
pub fn plan(dir: &Directory, link: &EntraLink, now: DateTime<Utc>) -> Result<Planned, LinkRefusal> {
    let link = normalize(link)?;
    let user = dir
        .get(&link.handle)
        .filter(|u| u.sign_in_name.as_deref() == Some(link.sign_in_name.as_str()))
        .ok_or(LinkRefusal::NotFound)?;
    match user.identity(IdentityKind::Entra) {
        Some(e) if e.issuer == link.key.tid && e.subject == link.key.oid => {
            return Ok(Planned::Same(Box::new(user.clone())));
        }
        Some(_) => return Err(LinkRefusal::Linked),
        None => {}
    }
    let held_by_other = dir.users.values().any(|o| {
        o.handle != user.handle
            && o.identity(IdentityKind::Entra)
                .is_some_and(|e| e.issuer == link.key.tid && e.subject == link.key.oid)
    });
    if held_by_other {
        return Err(LinkRefusal::Taken);
    }
    if !user.active() {
        return Err(LinkRefusal::NotActive);
    }
    if user.handle == EVALS {
        return Err(LinkRefusal::EvalsAccount);
    }
    if user.has(UserRole::Admin) || user.has(UserRole::Auditor) {
        return Err(LinkRefusal::HostOnly);
    }
    if user.sign_in_name_confirmed_at.is_none() {
        return Err(LinkRefusal::Unconfirmed);
    }
    let identity = Identity {
        kind: IdentityKind::Entra,
        issuer: link.key.tid.clone(),
        subject: link.key.oid.clone(),
        linked_at: now,
        linked_by: user.handle.clone(),
        linked_via: Via::Link,
        host_confirmed: false,
    };
    let mut after = user.clone();
    after
        .identities
        .insert(IdentityKind::Entra, identity.clone());
    let actor = Actor::web(&user.handle);
    Ok(Planned::Link(Box::new(LinkPlan {
        write: Write::InsertIdentity {
            handle: user.handle.clone(),
            identity,
        },
        change: RoleChange {
            id: 0,
            at: now,
            actor: user.handle.clone(),
            // `role_changes.client` has no `link`: the link came through pmos-web.
            via: Via::Web,
            host_user: None,
            target: user.handle.clone(),
            change: ChangeKind::Link,
            role: None,
            detail: Some(IdentityKind::Entra.as_str().into()),
        },
        event: UserEvent {
            action: "users.identity_link",
            target: user.handle.clone(),
            outcome: Outcome::Ok,
            detail: Some(IdentityKind::Entra.as_str().into()),
        },
        actor,
        user: after,
    })))
}

/// The `denied` event a refused link records, and the actor it is recorded as. The target is
/// the handle only when that user exists (never an arbitrary path value).
pub fn refused_event(
    dir: Option<&Directory>,
    handle: &str,
    why: LinkRefusal,
) -> (Actor, UserEvent) {
    let exists = dir.is_some_and(|d| d.get(handle).is_some());
    let target = if exists && why != LinkRefusal::NotFound {
        handle.to_string()
    } else {
        String::new()
    };
    let action = if why.conflict() {
        "users.identity_conflict"
    } else {
        "users.identity_link"
    };
    let detail = why.detail();
    debug_assert!(detail::is_safe(&detail), "{detail}");
    (
        Actor {
            name: ANONYMOUS.into(),
            via: Via::Web,
            host_user: None,
        },
        UserEvent {
            action,
            target,
            outcome: Outcome::Denied,
            detail: Some(detail),
        },
    )
}

/// The audit event for a refused link: as [`super::audit_event`], with no `output_ref` when
/// the target is unknown.
pub(crate) fn audit(
    actor: &Actor,
    event: &UserEvent,
    now: DateTime<Utc>,
) -> crate::audit::AuditEvent {
    let mut e = super::audit_event(actor, event, now);
    if event.target.is_empty() {
        e.output_ref = None;
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_refusal_detail_is_a_fixed_format() {
        for r in LinkRefusal::ALL {
            assert!(detail::is_safe(&r.detail()), "{r:?}");
            assert!(!r.message().is_empty());
        }
        assert!(detail::is_safe("entra"));
    }
}
