//! A person's approval of one exact action (spec §4.4).

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::envelope::document;
use crate::error::{Error, Result};
use crate::ids::{Digest, Id};
use crate::jcs;
use crate::revocation::{Revocable, Revocations};

/// How strongly the approval is tied to the person. `Signed` is the stronger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    /// The Authority, or PM OS, states that a signed-in person approved.
    Attested,
    /// The approver's own passkey signed the challenge, and the gate checks that itself.
    Signed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approver {
    pub id: Id,
    /// The approver's groups when they approved.
    pub groups: Vec<Id>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Evidence {
    /// Who vouched, and the session they vouch for.
    Attested { by: Id, session: Id },
    /// The passkey credential and its assertion over [`Countersignature::challenge`].
    Passkey { credential: Id, assertion: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Countersignature {
    pub v: u32,
    pub tenant: Id,
    pub env: Id,
    pub id: Id,
    pub issuer: Id,
    pub issued_at: i64,
    pub expires_at: i64,
    /// The one gate this approval is for. That gate records its single use.
    pub gate: Id,
    pub mandate: Id,
    pub action_digest: Digest,
    pub approver: Approver,
    pub class: Class,
    pub evidence: Evidence,
}

document!(Countersignature, "gov.countersign.v1");

/// Checks a passkey assertion against the signed approver roster. Built with enforce mode
/// (slice G6); until then nothing can satisfy a `Signed` requirement.
pub trait PasskeyVerifier {
    fn verify(&self, approver: &Id, credential: &Id, assertion: &str, challenge: &Digest) -> bool;
}

/// Verifies nothing. The only verifier there is until slice G6.
pub struct NoPasskeys;

impl PasskeyVerifier for NoPasskeys {
    fn verify(&self, _: &Id, _: &Id, _: &str, _: &Digest) -> bool {
        false
    }
}

/// What policy asks for, and the action it is asked for.
pub struct Requirement<'a> {
    pub gate: &'a Id,
    pub mandate: &'a Id,
    pub action_digest: &'a Digest,
    pub class: Class,
    pub group: &'a Id,
    /// The person who issued the root mandate, when policy does not let them approve their
    /// own agent's action. The gate checks this itself; it is not left to the Authority.
    pub not_approver: Option<&'a Id>,
    /// The key that signed the countersignature, as `open` returned it.
    pub signed_by: &'a Id,
    pub now: i64,
}

impl Countersignature {
    /// What the approver's passkey signs: everything that binds the approval, and not the
    /// evidence itself.
    pub fn challenge(&self) -> Digest {
        let bound = json!({
            "tenant": self.tenant, "env": self.env, "id": self.id, "gate": self.gate,
            "mandate": self.mandate, "action_digest": self.action_digest,
            "approver": self.approver.id, "class": self.class,
            "issued_at": self.issued_at.to_string(), "expires_at": self.expires_at.to_string(),
        });
        let text = jcs::canonical(&bound).expect("no numbers");
        Digest::of(&[b"gov.countersign.challenge.v1\0", text.as_bytes()])
    }

    /// Does this (already opened) countersignature satisfy `want`? Single use is the gate's
    /// to record; everything else is here (threats T5, T7, T8).
    pub fn satisfies(
        &self,
        want: &Requirement<'_>,
        passkeys: &dyn PasskeyVerifier,
        revoked: &dyn Revocations,
    ) -> Result<()> {
        if &self.gate != want.gate {
            return Err(Error::WrongGate);
        }
        if &self.mandate != want.mandate {
            return Err(Error::WrongMandate);
        }
        if &self.action_digest != want.action_digest {
            return Err(Error::WrongDigest);
        }
        if want.now < self.issued_at {
            return Err(Error::NotYetValid);
        }
        if want.now >= self.expires_at {
            return Err(Error::Expired);
        }
        if revoked.is_revoked(Revocable::Countersignature, &self.id)
            || revoked.is_revoked(Revocable::Key, want.signed_by)
        {
            return Err(Error::Revoked);
        }
        if want.not_approver == Some(&self.approver.id) {
            return Err(Error::ApproverIsIssuer);
        }
        if !self.approver.groups.contains(want.group) {
            return Err(Error::WrongGroup);
        }
        // The class a countersignature claims must be the class its evidence is.
        match (&self.class, &self.evidence) {
            (Class::Attested, Evidence::Attested { .. }) => {}
            (
                Class::Signed,
                Evidence::Passkey {
                    credential,
                    assertion,
                },
            ) => {
                if !passkeys.verify(&self.approver.id, credential, assertion, &self.challenge()) {
                    return Err(Error::PasskeyUnverified);
                }
            }
            _ => return Err(Error::Malformed("class and evidence disagree")),
        }
        if self.class < want.class {
            return Err(Error::WrongClass);
        }
        Ok(())
    }
}
