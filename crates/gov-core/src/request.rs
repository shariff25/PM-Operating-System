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
use crate::keys::{KeySet, PublicKey};
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

/// The request to open a session, signed by the agent's key (review finding GR-97): only the
/// holder of the key a mandate names can open a session under it, so a copy of the mandate
/// alone opens nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionOpen {
    pub v: u32,
    pub tenant: Id,
    pub env: Id,
    pub gate: Id,
    /// The mandate the session is for.
    pub mandate: Id,
    /// Random, and never accepted twice by one gate.
    pub nonce: Id,
    /// When it was signed, in seconds. A gate takes it only near its own time.
    pub at: i64,
}

document!(SessionOpen, "gov.session.v1");

/// One agent's session on one gate.
#[derive(Debug, Clone)]
pub struct Session {
    pub gate: Id,
    pub id: Id,
    /// Random, issued by the gate when the session opened.
    pub challenge: Id,
    /// The one agent key this session belongs to. Another enrolled agent that learns the
    /// session's id and challenge cannot sign into it and run its counter out.
    agent_key: PublicKey,
    last_counter: u64,
}

impl Session {
    pub fn new(gate: Id, id: Id, challenge: Id, agent_key: PublicKey) -> Self {
        Session {
            gate,
            id,
            challenge,
            agent_key,
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
        if mandate.agent.key != self.agent_key {
            return Err(Error::WrongSession);
        }
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
