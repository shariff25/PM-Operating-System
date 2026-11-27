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
use gov_core::revocation::{Revocable, RevocationSet, Revocations};
use gov_core::{open, random_id, Digest, Envelope, Error, Id, KeySet, PublicKey, Scope};

pub struct SessionsConfig {
    pub scope: Scope,
    pub gate: Id,
    /// The keys mandates are issued under. Until there is an Authority (G4) these are a test
    /// issuer's, from a file.
    pub issuers: KeySet,
    /// The templates of the policy bundle held: every mandate a session is opened for is
    /// held to the one it names (review finding GR-138). `None` only where no bundle was
    /// loaded.
    pub templates: Option<Vec<gov_core::mandate::Template>>,
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
            templates: None,
            max_proven: 4,
            max_unproven: 2,
            unproven_secs: 30,
            idle_secs: 3600,
            max_calls: 100_000,
            open_skew_secs: 120,
        }
    }
}

/// What the gate holds from its Authority at the moment of a call (spec §7.6.3, points 5
/// and 6). Given to [`Sessions::open`] and [`Sessions::prove`] by a gate that has an
/// Authority; a gate without one (tests, fake data) gives none, and neither is checked.
pub struct Trust<'a> {
    /// The gate holds a lease, the bundle it names and the feed through its number, and
    /// the lease is no older than it says it is good for.
    pub fresh: bool,
    /// Everything the Authority's feed has revoked, as far as this gate has read.
    pub revoked: &'a RevocationSet,
}

/// What a call presented, and the digest of the action the gate itself computed for it.
#[derive(Clone, Copy)]
pub struct Call<'a> {
    pub proof: Option<&'a Envelope>,
    pub salt: Option<&'a Salt>,
    pub digest: &'a Digest,
    /// False when the call's arguments have no canonical form.
    pub canonical: bool,
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

/// A call whose proof held. Only [`Sessions::prove`] makes one: its field is private, so
/// nothing outside this module can say a call was proven (review finding GR-101).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenCall {
    who: Presented,
}

impl ProvenCall {
    pub fn who(&self) -> &Presented {
        &self.who
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Proved {
    Proven(ProvenCall),
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
    /// The chain as it was presented, to check again when something is revoked.
    presented: Envelope,
    /// Set when the chain, its agent or its key was revoked after the session opened. The
    /// session is then closed to proof and kept as a marker: a call that still proves the
    /// key is recorded as this agent's, failed, with this reason (review finding GR-110).
    revoked: Option<ProofFailure>,
}

pub struct Sessions {
    config: SessionsConfig,
    open: BTreeMap<Id, Entry>,
    /// Nonces of requests to open, with when they were seen, kept for twice the skew.
    nonces: BTreeMap<Id, i64>,
    /// Requests to open that were refused, by why. The asker is told none of this; the
    /// operator is (review finding GR-155).
    refused: BTreeMap<&'static str, u64>,
}

/// Why a request to open was refused, for the operator's count.
pub mod why {
    /// The mandate chain does not hold: a signature, a time, a link.
    pub const MANDATE: &str = "mandate";
    /// The chain holds, and names a template the bundle lacks or goes wider than it. Signed
    /// with a real mandates key, this is what a taken key looks like (GR-138).
    pub const TEMPLATE: &str = "template";
    /// The request is not signed by the mandate's agent, or is for another gate, mandate
    /// or time.
    pub const REQUEST: &str = "request";
    /// The request has been seen before.
    pub const REPLAY: &str = "replay";
    /// The agent key has as many unproven sessions as it may.
    pub const BUSY: &str = "busy";
    /// The gate has no complete, fresh lease from its Authority, and so cannot say that
    /// nothing in the chain is revoked.
    pub const NO_LEASE: &str = "no_lease";
    /// The mandate, an agent in its chain, or a key is revoked.
    pub const REVOKED: &str = "revoked";
}

impl Sessions {
    pub fn new(config: SessionsConfig) -> Self {
        Sessions {
            config,
            open: BTreeMap::new(),
            nonces: BTreeMap::new(),
            refused: BTreeMap::new(),
        }
    }

    /// How many requests to open were refused, by why (see [`why`]).
    pub fn refusals(&self) -> BTreeMap<&'static str, u64> {
        self.refused.clone()
    }

    fn refuse(&mut self, why: &'static str) -> OpenRefused {
        let n = self.refused.entry(why).or_default();
        *n = n.saturating_add(1);
        if why == why::BUSY {
            OpenRefused::Busy
        } else {
            OpenRefused::Invalid
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
        self.open_with(mandate, request, now, None)
    }

    /// As [`Sessions::open`], for a gate that has an Authority: `trust` is what it holds
    /// from it now. Without a complete, fresh lease no session is opened, and none is
    /// opened for a chain any part of which is revoked.
    pub fn open_with(
        &mut self,
        mandate: &Envelope,
        request: &Envelope,
        now: i64,
        trust: Option<&Trust<'_>>,
    ) -> Result<(Id, Id), OpenRefused> {
        self.sweep(now);
        // With an Authority, no session is opened that it could not vouch for now.
        if trust.is_some_and(|t| !t.fresh) {
            return Err(self.refuse(why::NO_LEASE));
        }
        // Without one there is nobody to say what is revoked.
        let none_revoked = RevocationSet::default();
        let revoked = trust.map_or(&none_revoked, |t| t.revoked);
        let checked = {
            let c = &self.config;
            verify_chain(
                mandate,
                &ChainCheck {
                    scope: &c.scope,
                    authority: &c.issuers,
                    now,
                    revoked,
                    templates: c.templates.as_deref(),
                },
            )
        };
        let chain = match checked {
            Ok(chain) => chain,
            Err(Error::Revoked) => return Err(self.refuse(why::REVOKED)),
            Err(gov_core::Error::UnknownTemplate | gov_core::Error::WiderThanTemplate(_)) => {
                return Err(self.refuse(why::TEMPLATE))
            }
            Err(_) => return Err(self.refuse(why::MANDATE)),
        };
        let leaf = chain.leaf();
        let agent_key = KeySet::new([leaf.agent.key.clone()]);
        let Ok(asked) = open::<SessionOpen>(request, &agent_key, &self.config.scope) else {
            return Err(self.refuse(why::REQUEST));
        };
        let asked = asked.doc;
        let c = &self.config;
        if asked.gate != c.gate
            || asked.mandate != leaf.id
            // Compared without arithmetic that can overflow: a time at the far end of the
            // range is refused like any other, and never panics (review finding GR-102).
            || asked.at.abs_diff(now) > c.open_skew_secs.unsigned_abs()
        {
            return Err(self.refuse(why::REQUEST));
        }
        if self.nonces.contains_key(&asked.nonce) {
            return Err(self.refuse(why::REPLAY));
        }
        let key = leaf.agent.key.clone();
        let unproven = self
            .open
            .values()
            .filter(|e| e.key == key && !e.proven)
            .count();
        if unproven >= c.max_unproven {
            return Err(self.refuse(why::BUSY));
        }
        let c = &self.config;
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
                presented: mandate.clone(),
                revoked: None,
            },
        );
        Ok((id, challenge))
    }

    /// Something was revoked: close to proof every open session it touches, and keep each
    /// as a marker (spec §7.6.3, point 6). Every chain is checked again as it was when the
    /// session opened, against `revoked`. Returns how many sessions were closed.
    ///
    /// The reason names the nearest thing revoked: the session's own key, then an agent of
    /// the chain, then a mandate of it or the key one was signed with.
    pub fn revoke(&mut self, revoked: &RevocationSet, now: i64) -> usize {
        self.sweep(now);
        let c = &self.config;
        let mut closed = 0;
        for entry in self.open.values_mut() {
            if entry.revoked.is_some() {
                continue;
            }
            let check = ChainCheck {
                scope: &c.scope,
                authority: &c.issuers,
                now,
                revoked,
                templates: c.templates.as_deref(),
            };
            if !matches!(verify_chain(&entry.presented, &check), Err(Error::Revoked)) {
                continue;
            }
            // The chain as it stood at opening names what to look for.
            let none = RevocationSet::default();
            let links = verify_chain(
                &entry.presented,
                &ChainCheck {
                    revoked: &none,
                    ..check
                },
            )
            .map(|chain| chain.links().to_vec())
            .unwrap_or_else(|_| vec![entry.mandate.clone()]);
            let why = if revoked.is_revoked(Revocable::Key, &entry.key.kid) {
                ProofFailure::KeyRevoked
            } else if links
                .iter()
                .any(|m| revoked.is_revoked(Revocable::Agent, &m.agent.id))
            {
                ProofFailure::AgentRevoked
            } else {
                ProofFailure::MandateRevoked
            };
            entry.revoked = Some(why);
            closed += 1;
        }
        closed
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
        let call = Call {
            proof,
            salt,
            digest,
            canonical,
        };
        self.prove_with(session, &call, now, None)
    }

    /// As [`Sessions::prove`], for a gate that has an Authority. A proof that holds is
    /// recorded as proven only under a complete, fresh lease, and never for a session
    /// whose chain has been revoked: each of those is a failed proof with its own reason.
    pub fn prove_with(
        &mut self,
        session: &Id,
        call: &Call<'_>,
        now: i64,
        trust: Option<&Trust<'_>>,
    ) -> Proved {
        let Call {
            proof,
            salt,
            digest,
            canonical,
        } = *call;
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
        // The proof holds: the caller has the key. Whether that makes the call the agent's
        // act is another matter. A session whose chain was revoked says which part was;
        // and without a complete, fresh lease the gate cannot know that nothing was.
        // Neither is ever counted as proven, and neither pushes out a proven session.
        if let Some(why) = entry.revoked {
            return failed(&entry.who, why);
        }
        if trust.is_some_and(|t| !t.fresh) {
            return failed(&entry.who, ProofFailure::NoLease);
        }
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
        Proved::Proven(ProvenCall { who })
    }
}
