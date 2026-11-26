//! Sessions with the shim (spec §5.3.1): who may open one, and what makes a call proven.
//!
//! A session belongs to one agent key and one mandate. Opening it proves only that the
//! opener holds the key (the request to open is signed); every call is then proven on its
//! own, by a signature over this gate's challenge, a rising counter, the mandate and the
//! digest of the action **the gate itself computed**. Sessions live in memory: a gate that
//! restarts knows none, and says so.

use std::collections::{BTreeMap, BTreeSet};

use gov_core::action::Salt;
use gov_core::mandate::{verify_chain, ChainCheck, Mandate};
use gov_core::receipt::ProofFailure;
use gov_core::request::{Session, SessionOpen};
use gov_core::revocation::RevocationSet;
use gov_core::{open, random_id, Digest, Envelope, Error, Id, KeySet, PublicKey, Scope};

pub struct SessionsConfig {
    pub scope: Scope,
    pub gate: Id,
    /// The keys mandates are issued under. Until there is an Authority (G4) these are a test
    /// issuer's, from a file.
    pub issuers: KeySet,
    /// Sessions one agent key may hold that have proven a call.
    pub max_proven: usize,
    /// Sessions one agent key may hold that have not yet.
    pub max_unproven: usize,
    /// How long a session may stay unproven, in seconds.
    pub unproven_secs: i64,
    /// How long a proven session may go unused.
    pub idle_secs: i64,
    /// Calls one session may make. Its record of salts is bounded by this.
    pub max_calls: u64,
    /// How far the time in a request to open may be from this gate's.
    pub open_skew_secs: i64,
}

impl SessionsConfig {
    pub fn new(scope: Scope, gate: Id, issuers: KeySet) -> Self {
        SessionsConfig {
            scope,
            gate,
            issuers,
            max_proven: 4,
            max_unproven: 2,
            unproven_secs: 30,
            idle_secs: 3600,
            max_calls: 100_000,
            open_skew_secs: 120,
        }
    }
}

/// Why a session was not opened. The answer says no more than this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenRefused {
    /// The mandate chain or the signed request does not hold.
    Invalid,
    /// This agent key already has as many unproven sessions as it may.
    Busy,
}

/// Who a session's mandate names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Presented {
    pub agent: Id,
    /// The mandate chain's ids, the presented one first.
    pub mandates: Vec<Id>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Proved {
    Proven(Presented),
    /// `who` is absent when the gate does not know the session at all.
    Failed {
        who: Option<Presented>,
        why: ProofFailure,
    },
}

struct Entry {
    session: Session,
    who: Presented,
    /// The first link of the chain: the mandate every proof names.
    mandate: Mandate,
    /// Sessions are counted per agent key.
    key: PublicKey,
    /// The earliest expiry in the chain.
    expires_at: i64,
    proven: bool,
    opened_at: i64,
    used_at: i64,
    calls: u64,
    salts: BTreeSet<Salt>,
}

pub struct Sessions {
    config: SessionsConfig,
    open: BTreeMap<Id, Entry>,
    /// Nonces of requests to open, with when they were seen, kept for twice the skew.
    nonces: BTreeMap<Id, i64>,
}

impl Sessions {
    pub fn new(config: SessionsConfig) -> Self {
        Sessions {
            config,
            open: BTreeMap::new(),
            nonces: BTreeMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.open.len()
    }

    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    /// Forget what has run out: unproven sessions after seconds, proven ones left idle,
    /// any whose mandate has expired, and old nonces.
    fn sweep(&mut self, now: i64) {
        let c = &self.config;
        self.open.retain(|_, e| {
            let fresh = if e.proven {
                now - e.used_at <= c.idle_secs
            } else {
                now - e.opened_at <= c.unproven_secs
            };
            fresh && now < e.expires_at
        });
        self.nonces
            .retain(|_, seen| now - *seen <= 2 * c.open_skew_secs);
    }

    /// Open a session for the mandate chain `mandate`, on a request signed by the key its
    /// first link names. Returns the session's id and challenge.
    pub fn open(
        &mut self,
        mandate: &Envelope,
        request: &Envelope,
        now: i64,
    ) -> Result<(Id, Id), OpenRefused> {
        self.sweep(now);
        let c = &self.config;
        // Revocation is not checked before there is an Authority to say what is revoked.
        let none_revoked = RevocationSet::default();
        let chain = verify_chain(
            mandate,
            &ChainCheck {
                scope: &c.scope,
                authority: &c.issuers,
                now,
                revoked: &none_revoked,
            },
        )
        .map_err(|_| OpenRefused::Invalid)?;
        let leaf = chain.leaf();
        let agent_key = KeySet::new([leaf.agent.key.clone()]);
        let asked = open::<SessionOpen>(request, &agent_key, &c.scope)
            .map_err(|_| OpenRefused::Invalid)?
            .doc;
        if asked.gate != c.gate
            || asked.mandate != leaf.id
            || (asked.at - now).abs() > c.open_skew_secs
            || self.nonces.contains_key(&asked.nonce)
        {
            return Err(OpenRefused::Invalid);
        }
        let key = leaf.agent.key.clone();
        let unproven = self
            .open
            .values()
            .filter(|e| e.key == key && !e.proven)
            .count();
        if unproven >= c.max_unproven {
            return Err(OpenRefused::Busy);
        }
        let id = random_id("sess").map_err(|_| OpenRefused::Invalid)?;
        let challenge = random_id("chal").map_err(|_| OpenRefused::Invalid)?;
        self.nonces.insert(asked.nonce, now);
        self.open.insert(
            id.clone(),
            Entry {
                session: Session::new(
                    c.gate.clone(),
                    id.clone(),
                    challenge.clone(),
                    leaf.agent.key.clone(),
                ),
                who: Presented {
                    agent: leaf.agent.id.clone(),
                    mandates: chain.ids(),
                },
                mandate: leaf.clone(),
                key,
                expires_at: chain
                    .links()
                    .iter()
                    .map(|m| m.expires_at)
                    .min()
                    .unwrap_or(0),
                proven: false,
                opened_at: now,
                used_at: now,
                calls: 0,
                salts: BTreeSet::new(),
            },
        );
        Ok((id, challenge))
    }

    /// Decide whether a call is proven. `digest` is of the action the gate itself computed
    /// from its own map, its own record of the listing, the arguments it parsed and the
    /// salt it was sent; `canonical` is false when those arguments have no canonical form.
    ///
    /// Proven is said here and nowhere else, and only when [`Session::accept`] returned
    /// success for that digest.
    pub fn prove(
        &mut self,
        session: &Id,
        proof: Option<&Envelope>,
        salt: Option<&Salt>,
        digest: &Digest,
        canonical: bool,
        now: i64,
    ) -> Proved {
        self.sweep(now);
        let c = &self.config;
        let Some(entry) = self.open.get_mut(session) else {
            // Swept, never opened, or lost in a restart: the gate cannot tell which.
            return Proved::Failed {
                who: None,
                why: ProofFailure::UnknownSession,
            };
        };
        let failed = |who: &Presented, why| Proved::Failed {
            who: Some(who.clone()),
            why,
        };
        // The mandate is checked on every call, not only when the session opened (GR-93).
        if now >= entry.expires_at {
            let who = entry.who.clone();
            self.open.remove(session);
            return failed(&who, ProofFailure::MandateExpired);
        }
        if entry.calls >= c.max_calls {
            return failed(&entry.who, ProofFailure::SessionSpent);
        }
        let (Some(proof), Some(salt)) = (proof, salt) else {
            return failed(&entry.who, ProofFailure::Unreadable);
        };
        if entry.salts.contains(salt) {
            return failed(&entry.who, ProofFailure::RepeatedSalt);
        }
        if let Err(e) = entry
            .session
            .accept(proof, &entry.mandate, digest, &c.scope)
        {
            let why = match e {
                Error::Replay => ProofFailure::Replay,
                Error::WrongDigest if !canonical => ProofFailure::NoCanonicalForm,
                Error::WrongDigest => ProofFailure::WrongDigest,
                Error::WrongGate | Error::WrongSession | Error::WrongMandate => {
                    ProofFailure::WrongSession
                }
                _ => ProofFailure::BadSignature,
            };
            return failed(&entry.who, why);
        }
        entry.salts.insert(*salt);
        entry.calls += 1;
        entry.used_at = now;
        let first = !entry.proven;
        entry.proven = true;
        let (who, key) = (entry.who.clone(), entry.key.clone());
        if first {
            // Only a proven session counts against the key's sessions, and only a proven one
            // is closed to make room: the one unused longest, never this one (GR-88).
            let mut proven: Vec<(i64, Id)> = self
                .open
                .iter()
                .filter(|(id, e)| e.proven && e.key == key && *id != session)
                .map(|(id, e)| (e.used_at, id.clone()))
                .collect();
            proven.sort();
            let over = (proven.len() + 1).saturating_sub(c.max_proven);
            for (_, id) in proven.into_iter().take(over) {
                self.open.remove(&id);
            }
        }
        Proved::Proven(who)
    }
}
