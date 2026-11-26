//! The agent's proof that it holds the key its mandate names, for one call (spec §5.1 step 1).
//!
//! The gate issues a challenge when a session opens. Every call is signed over that
//! challenge, a counter that only rises, the mandate and the action's digest. A recorded
//! request therefore cannot be replayed on this gate, on another gate, or for another action
//! (threats T1 to T3).

use serde::{Deserialize, Serialize};

use crate::envelope::{document, open, Envelope};
use crate::error::{Error, Result};
use crate::ids::{Digest, Id, Scope};
use crate::keys::KeySet;
use crate::mandate::Mandate;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestProof {
    pub v: u32,
    pub tenant: Id,
    pub env: Id,
    pub gate: Id,
    pub session: Id,
    pub challenge: Id,
    pub counter: u64,
    pub mandate: Id,
    pub action_digest: Digest,
}

document!(RequestProof, "gov.request.v1");

/// One agent's session on one gate.
#[derive(Debug, Clone)]
pub struct Session {
    pub gate: Id,
    pub id: Id,
    /// Random, issued by the gate when the session opened.
    pub challenge: Id,
    last_counter: u64,
}

impl Session {
    pub fn new(gate: Id, id: Id, challenge: Id) -> Self {
        Session {
            gate,
            id,
            challenge,
            last_counter: 0,
        }
    }

    /// Check that `proof` was signed by `mandate`'s agent for this session, this mandate and
    /// this action, with a counter above the last one accepted. The counter moves only when
    /// everything else held.
    pub fn accept(
        &mut self,
        proof: &Envelope,
        mandate: &Mandate,
        action_digest: &Digest,
        scope: &Scope,
    ) -> Result<()> {
        let keys = KeySet::new([mandate.agent.key.clone()]);
        let p = open::<RequestProof>(proof, &keys, scope)?.doc;
        if p.gate != self.gate {
            return Err(Error::WrongGate);
        }
        if p.session != self.id || p.challenge != self.challenge {
            return Err(Error::WrongSession);
        }
        if p.mandate != mandate.id {
            return Err(Error::WrongMandate);
        }
        if &p.action_digest != action_digest {
            return Err(Error::WrongDigest);
        }
        if p.counter <= self.last_counter {
            return Err(Error::Replay);
        }
        self.last_counter = p.counter;
        Ok(())
    }
}
