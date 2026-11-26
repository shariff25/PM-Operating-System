//! Intake, checkpoints, pruning and export (spec §6).

use gov_core::admin::{AdminAction, AdminRecord, SubjectKind};
use gov_core::enrol::key_digest;
use gov_core::lease::{batch_digest, Checkpoint, GateHead, IntakeAck, Pruned, ShipRequest};
use gov_core::receipt::{follow, Phase, Receipt};
use gov_core::{
    open, seal, Digest, Envelope, Error, Id, KeySet, PublicKey, Scope, SigningKey, VERSION,
};

use crate::export::{Export, GateExport, TrustedKeys};
use crate::store::{ChainKind, Enrolment, Refusal, Store, StoreError, StoredReceipt};
use crate::witness::{Witness, WitnessError};

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    /// The receipt was refused, and the refusal recorded.
    #[error("refused: {0}")]
    Refused(&'static str),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Witness(#[from] WitnessError),
    #[error("{0}")]
    Core(#[from] Error),
}

/// What intake did with a receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ingested {
    Added,
    /// The Ledger already holds exactly this receipt. Gates resend batches; that is fine.
    AlreadyHeld,
}

pub struct Ledger<S> {
    store: S,
    scope: Scope,
}

fn envelope_hash(envelope: &Envelope) -> Digest {
    Digest::of(&[envelope.payload.as_bytes(), b".", envelope.sig.as_bytes()])
}

impl<S: Store> Ledger<S> {
    pub fn new(store: S, scope: Scope) -> Self {
        Ledger { store, scope }
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    /// Enrol a gate's receipt key. A key anyone could forge for is refused, as is a key or a
    /// key id already enrolled (review finding GR-6).
    pub async fn enrol_gate_key(&self, gate: &Id, key: &PublicKey) -> Result<(), LedgerError> {
        key.enrolable()?;
        self.store.add_gate_key(&self.scope, gate, key).await?;
        Ok(())
    }

    /// Enrol an Authority's key for its admin records. The same rules as for a gate's key,
    /// and the chain it names must not be a gate's.
    pub async fn enrol_admin_key(
        &self,
        authority: &Id,
        key: &PublicKey,
    ) -> Result<(), LedgerError> {
        key.enrolable()?;
        self.store
            .add_key(&self.scope, authority, ChainKind::Admin, key)
            .await?;
        Ok(())
    }

    async fn refuse(
        &self,
        envelope: &Envelope,
        code: &'static str,
        keep: bool,
        now: i64,
    ) -> Result<Ingested, LedgerError> {
        self.store
            .record_refusal(
                &self.scope,
                &Refusal {
                    at: now,
                    seen: 1,
                    last_at: now,
                    kid: envelope.kid.clone(),
                    code: code.to_string(),
                    taken: false,
                    envelope_hash: envelope_hash(envelope),
                    envelope: keep.then(|| envelope.clone()),
                },
            )
            .await?;
        Err(LedgerError::Refused(code))
    }

    /// Take in one receipt (threat T10). It must be signed by an enrolled gate key, name that
    /// key's gate, keep a receipt's own rules, and follow the gate's chain head exactly. A
    /// receipt that does not is refused and the refusal recorded; one whose signature held is
    /// kept whole, because a gate that signs a fork or a gap has left evidence.
    pub async fn ingest(&self, envelope: &Envelope, now: i64) -> Result<Ingested, LedgerError> {
        let Some((gate, key)) = self.store.gate_key(&self.scope, &envelope.kid).await? else {
            return self.refuse(envelope, "unknown_key", false, now).await;
        };
        let keys = KeySet::new([key]);
        // A chain holds one kind of document, and a key is enrolled for one kind. A key
        // enrolled for admin records is read as an admin record or refused, never as a
        // receipt; a gate's key, below, never as an admin record (review finding GR-108).
        if self.store.chain_kind(&self.scope, &gate).await? == Some(ChainKind::Admin) {
            return self.ingest_admin(envelope, &gate, &keys, now).await;
        }
        if self.store.key_retired(&self.scope, &envelope.kid).await? {
            return self.refuse(envelope, "retired_key", false, now).await;
        }
        let opened = match open::<Receipt>(envelope, &keys, &self.scope) {
            Ok(opened) => opened,
            Err(e) => return self.refuse(envelope, e.code(), false, now).await,
        };
        if opened.doc.gate != gate {
            return self.refuse(envelope, "wrong_gate", true, now).await;
        }
        // Two tries: once more if another writer moved the head in between.
        for _ in 0..2 {
            let head = self.store.head(&self.scope, &gate).await?;
            if let Some(h) = &head {
                if opened.doc.seq <= h.seq {
                    let held = self
                        .store
                        .receipt(&self.scope, &gate, opened.doc.seq)
                        .await?;
                    return match held {
                        Some(r) if r.hash == opened.hash => Ok(Ingested::AlreadyHeld),
                        // The gate signed two different receipts under one number.
                        Some(_) => self.refuse(envelope, "fork", true, now).await,
                        None => self.refuse(envelope, "already_pruned", true, now).await,
                    };
                }
            }
            // An outcome answers an intent this gate wrote, for the same action (finding
            // GR-46). The intent may have been pruned; then there is nothing to compare with.
            if let Some(intent_seq) = opened.doc.intent {
                if let Some(held) = self.store.receipt(&self.scope, &gate, intent_seq).await? {
                    let intent = open::<Receipt>(&held.envelope, &keys, &self.scope)?.doc;
                    if intent.phase != Phase::Intent
                        || intent.action.digest != opened.doc.action.digest
                    {
                        return self
                            .refuse(envelope, "outcome_without_intent", true, now)
                            .await;
                    }
                }
            }
            let new_head = match follow(head.as_ref(), &opened) {
                Ok(h) => h,
                Err(e) => return self.refuse(envelope, e.code(), true, now).await,
            };
            let stored = StoredReceipt {
                seq: new_head.seq,
                hash: new_head.hash,
                at: opened.doc.at,
                received_at: now,
                envelope: envelope.clone(),
            };
            // A gate's clock should not run backwards along its own chain (finding GR-16),
            // but refusing the receipt would end the chain: every later one links to it. A
            // slow lease renewal can step an honest gate's clock back a second or two (finding
            // GR-42). So the receipt is taken, the step back is recorded, and `verify`
            // reports it. The receipt before may have been pruned; then there is nothing to
            // compare with.
            let mut stepped_back = false;
            if let Some(h) = &head {
                if let Some(before) = self.store.receipt(&self.scope, &gate, h.seq).await? {
                    stepped_back = opened.doc.at < before.at;
                }
            }
            match self
                .store
                .append(&self.scope, &gate, head.map_or(0, |h| h.seq), &stored)
                .await
            {
                Ok(()) => {
                    if stepped_back {
                        self.store
                            .record_refusal(
                                &self.scope,
                                &Refusal {
                                    at: now,
                                    seen: 1,
                                    last_at: now,
                                    kid: envelope.kid.clone(),
                                    code: "time_went_back".to_string(),
                                    taken: true,
                                    envelope_hash: envelope_hash(envelope),
                                    envelope: None,
                                },
                            )
                            .await?;
                    }
                    return Ok(Ingested::Added);
                }
                Err(StoreError::HeadMoved) => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(StoreError::HeadMoved.into())
    }

    /// Take a gate's key, offered by anyone (spec §7.6, choice 3). No signature is asked
    /// for, because none is needed: the key is enrolled only if this Ledger's own copy of
    /// an Authority's chain holds the record of its enrolment, for a gate, with a key of
    /// exactly this digest, and nothing after it that disables the gate or revokes the
    /// key. That record is signed, numbered and under the witness. So the keys a Ledger
    /// takes receipts under are the ones the witness has seen enrolled, and offering a key
    /// twice, or offering someone else's, does nothing.
    pub async fn supply_key(&self, key: &PublicKey) -> Result<(), LedgerError> {
        let refused = || LedgerError::Refused("key_not_enrolled");
        let digest = key_digest(key)?;
        // One lookup in what the Authorities' records said as they were taken in, not a
        // reading of their chains (review finding GR-124).
        let gate = self
            .store
            .enrolment(&self.scope, &key.kid)
            .await?
            .filter(|e| !e.ended && e.digest == digest)
            .map(|e| e.gate);
        let Some(gate) = gate else {
            return Err(refused());
        };
        // Offered twice, it is enrolled once.
        if let Some((held_for, held)) = self.store.gate_key(&self.scope, &key.kid).await? {
            return if held_for == gate && &held == key {
                Ok(())
            } else {
                Err(refused())
            };
        }
        self.enrol_gate_key(&gate, key).await
    }

    /// What one of an Authority's records grants at the Ledger: an enrolment is noted, so
    /// that the key it names can be supplied. Done only for a record the chain holds, and
    /// again when the record comes again, in case the Ledger stopped between keeping it and
    /// noting it. Safe to do twice.
    async fn grant_trust(&self, authority: &Id, record: &AdminRecord) -> Result<(), LedgerError> {
        if let (AdminAction::KeyEnrolled, Some(about), Some(digest)) =
            (&record.action, &record.about, &record.value)
        {
            if about.kind == SubjectKind::Gate {
                let enrolment = Enrolment {
                    authority: authority.clone(),
                    gate: about.id.clone(),
                    digest: digest.clone(),
                    ended: false,
                };
                self.store
                    .note_enrolment(&self.scope, &record.subject.id, &enrolment)
                    .await?;
            }
        }
        Ok(())
    }

    /// What one of an Authority's records ends at the Ledger: a gate disabled or a key
    /// revoked ends enrolments and retires keys. Done **before** the record is kept, so that
    /// the Ledger stopping between the two steps leaves a key retired and a record to come
    /// again, never a record kept and a disabled gate's key in use (review finding GR-123);
    /// and done again when the record comes again. Safe to do twice.
    async fn end_trust(&self, authority: &Id, record: &AdminRecord) -> Result<(), LedgerError> {
        match record.action {
            AdminAction::GateDisabled => {
                self.store
                    .end_enrolments(&self.scope, authority, Some(&record.subject.id), None)
                    .await?;
                self.retire_gate_keys(&record.subject.id, None).await?;
            }
            AdminAction::Revoked if record.subject.kind == SubjectKind::Key => {
                self.store
                    .end_enrolments(&self.scope, authority, None, Some(&record.subject.id))
                    .await?;
                if let Some((gate, _)) =
                    self.store.gate_key(&self.scope, &record.subject.id).await?
                {
                    self.retire_gate_keys(&gate, Some(&record.subject.id))
                        .await?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Retire a gate's keys. Only a gate's: an Authority's word does not retire the keys of
    /// an Authority's own chain.
    async fn retire_gate_keys(&self, gate: &Id, kid: Option<&Id>) -> Result<(), LedgerError> {
        if self.store.chain_kind(&self.scope, gate).await? == Some(ChainKind::Receipt) {
            self.store.retire_keys(&self.scope, gate, kid).await?;
        }
        Ok(())
    }

    /// Take in one of an Authority's admin records: signed by a key enrolled for admin
    /// records, naming that key's Authority, keeping a record's own rules, and following
    /// the chain's head exactly.
    async fn ingest_admin(
        &self,
        envelope: &Envelope,
        authority: &Id,
        keys: &KeySet,
        now: i64,
    ) -> Result<Ingested, LedgerError> {
        let opened = match open::<AdminRecord>(envelope, keys, &self.scope) {
            Ok(opened) => opened,
            Err(e) => return self.refuse(envelope, e.code(), false, now).await,
        };
        if &opened.doc.authority != authority {
            return self.refuse(envelope, "wrong_gate", true, now).await;
        }
        for _ in 0..2 {
            let head = self.store.head(&self.scope, authority).await?;
            if let Some(h) = &head {
                if opened.doc.seq <= h.seq {
                    let held = self
                        .store
                        .receipt(&self.scope, authority, opened.doc.seq)
                        .await?;
                    return match held {
                        Some(r) if r.hash == opened.hash => {
                            // Sent again, perhaps because the Ledger stopped before it
                            // had acted on it: act on it again (GR-123).
                            self.end_trust(authority, &opened.doc).await?;
                            self.grant_trust(authority, &opened.doc).await?;
                            Ok(Ingested::AlreadyHeld)
                        }
                        Some(_) => self.refuse(envelope, "fork", true, now).await,
                        None => self.refuse(envelope, "already_pruned", true, now).await,
                    };
                }
            }
            let new_head = match gov_core::admin::follow(head.as_ref(), &opened) {
                Ok(h) => h,
                Err(e) => return self.refuse(envelope, e.code(), true, now).await,
            };
            self.end_trust(authority, &opened.doc).await?;
            let stored = StoredReceipt {
                seq: new_head.seq,
                hash: new_head.hash,
                at: opened.doc.at,
                received_at: now,
                envelope: envelope.clone(),
            };
            match self
                .store
                .append(&self.scope, authority, head.map_or(0, |h| h.seq), &stored)
                .await
            {
                Ok(()) => {
                    self.grant_trust(authority, &opened.doc).await?;
                    return Ok(Ingested::Added);
                }
                Err(StoreError::HeadMoved) => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(StoreError::HeadMoved.into())
    }

    /// Take a gate's shipment: its signed request and the batch it covers.
    ///
    /// The request must be signed by a key enrolled for the gate it names, and must fit the
    /// batch: the same count, the same digest, every receipt under one of that gate's keys.
    /// A request that does not is refused before anything is read into the chain and
    /// **nothing is recorded**, so a stranger can neither learn where a gate's chain stands
    /// nor fill the refusals (review findings GR-20 and GR-55). Returns the signed answer and
    /// the reason for the first receipt refused, if any was.
    pub async fn take_shipment(
        &self,
        request: &Envelope,
        batch: &[Envelope],
        key: &SigningKey,
        issuer: &Id,
        now: i64,
    ) -> Result<(Envelope, Option<&'static str>), LedgerError> {
        let Some((gate, gate_key)) = self.store.gate_key(&self.scope, &request.kid).await? else {
            return Err(LedgerError::Refused("unknown_sender"));
        };
        // A retired key gets no answer either: its gate is disabled or the key revoked.
        if self.store.key_retired(&self.scope, &request.kid).await? {
            return Err(LedgerError::Refused("unknown_sender"));
        }
        let asked = open::<ShipRequest>(request, &KeySet::new([gate_key]), &self.scope)
            .map_err(|_| LedgerError::Refused("unknown_sender"))?
            .doc;
        if asked.gate != gate
            || asked.count != batch.len() as u64
            || asked.batch != batch_digest(batch)
            || batch.is_empty()
        {
            return Err(LedgerError::Refused("request_does_not_fit_batch"));
        }
        for envelope in batch {
            match self.store.gate_key(&self.scope, &envelope.kid).await? {
                Some((g, _)) if g == gate => {}
                _ => return Err(LedgerError::Refused("request_does_not_fit_batch")),
            }
        }
        let (results, ack) = self
            .ingest_batch(batch, &asked.nonce, key, issuer, now)
            .await?;
        Ok((ack, results.into_iter().find_map(|r| r.err())))
    }

    /// Take in a batch of one gate's receipts, in order, and answer with a signed statement
    /// of where that gate's chain now stands here (review finding GR-10). Each receipt is
    /// taken or refused on its own; the answer says what the Ledger holds, whatever happened
    /// to the batch, so a gate learns of a refusal by seeing its chain has not moved.
    pub async fn ingest_batch(
        &self,
        batch: &[Envelope],
        nonce: &Id,
        key: &SigningKey,
        issuer: &Id,
        now: i64,
    ) -> Result<(Vec<Result<Ingested, &'static str>>, Envelope), LedgerError> {
        let Some(first) = batch.first() else {
            return Err(LedgerError::Refused("empty_batch"));
        };
        let Some((gate, _)) = self.store.gate_key(&self.scope, &first.kid).await? else {
            return Err(LedgerError::Refused("unknown_key"));
        };
        let mut results = Vec::with_capacity(batch.len());
        for envelope in batch {
            results.push(match self.ingest(envelope, now).await {
                Ok(done) => Ok(done),
                Err(LedgerError::Refused(code)) => Err(code),
                Err(other) => return Err(other),
            });
        }
        // The answer says what is held even when that is nothing: a gate whose Ledger was
        // restored from an older copy has to learn it, to send again (review finding GR-50).
        let head = self.store.head(&self.scope, &gate).await?;
        let ack = IntakeAck {
            v: VERSION,
            tenant: self.scope.tenant.clone(),
            env: self.scope.env.clone(),
            issuer: issuer.clone(),
            gate,
            nonce: nonce.clone(),
            seq: head.as_ref().map_or(0, |h| h.seq),
            hash: head.map(|h| h.hash),
            at: now,
        };
        Ok((results, seal(&ack, key)?))
    }

    /// Sign a checkpoint over every gate's chain head and write it to the witness. Called on a
    /// timer whether or not anything changed, so that silence shows (spec §6.2).
    pub async fn checkpoint(
        &self,
        key: &SigningKey,
        issuer: &Id,
        now: i64,
        witness: &dyn Witness,
    ) -> Result<Envelope, LedgerError> {
        // A checkpoint stored but not yet witnessed (a crash in between) goes out first.
        let last = self.store.last_checkpoint(&self.scope).await?;
        if let Some((seq, envelope)) = &last {
            witness.put(
                *seq,
                &serde_json::to_vec(envelope).expect("an envelope serialises"),
            )?;
        }
        let seq = last.map_or(1, |(seq, _)| seq + 1);
        let heads = self
            .store
            .heads(&self.scope)
            .await?
            .into_iter()
            .map(|(gate, head)| GateHead {
                gate,
                seq: head.seq,
                hash: head.hash,
            })
            .collect();
        let checkpoint = Checkpoint {
            v: VERSION,
            tenant: self.scope.tenant.clone(),
            env: self.scope.env.clone(),
            issuer: issuer.clone(),
            seq,
            at: now,
            heads,
        };
        checkpoint.validate()?;
        let envelope = seal(&checkpoint, key)?;
        self.store
            .add_checkpoint(&self.scope, seq, now, &envelope)
            .await?;
        witness.put(
            seq,
            &serde_json::to_vec(&envelope).expect("an envelope serialises"),
        )?;
        Ok(envelope)
    }

    /// Delete a gate's receipts that arrived before `cutoff`, leaving a signed record of where
    /// the chain was cut (spec §6.3). Age is the Ledger's own intake time, never the time the
    /// gate wrote. Only receipts the last checkpoint already covers can go, and that
    /// checkpoint is written to the witness first: nothing is deleted before the witness has
    /// its hash. Returns the number pruned through.
    pub async fn prune(
        &self,
        gate: &Id,
        cutoff: i64,
        key: &SigningKey,
        issuer: &Id,
        now: i64,
        witness: &dyn Witness,
    ) -> Result<Option<u64>, LedgerError> {
        // An Authority's chain is not pruned (review finding GR-122): it is what says which
        // gate keys are trusted and what an auditor derives them from. Until a signed
        // snapshot of the registry exists, its records are kept.
        if self.store.chain_kind(&self.scope, gate).await? == Some(ChainKind::Admin) {
            return Ok(None);
        }
        let Some((seq, envelope)) = self.store.last_checkpoint(&self.scope).await? else {
            return Ok(None);
        };
        // Stored is not witnessed: after a crash the last checkpoint may be in the database
        // only (review finding GR-14).
        witness.put(
            seq,
            &serde_json::to_vec(&envelope).expect("an envelope serialises"),
        )?;
        let keys = KeySet::new([key.public()]);
        let checkpoint = open::<Checkpoint>(&envelope, &keys, &self.scope)?.doc;
        let Some(witnessed) = checkpoint.heads.iter().find(|h| &h.gate == gate) else {
            return Ok(None);
        };
        let Some((through_seq, hash)) = self
            .store
            .prunable_through(&self.scope, gate, cutoff, witnessed.seq)
            .await?
        else {
            return Ok(None);
        };
        let record = Pruned {
            v: VERSION,
            tenant: self.scope.tenant.clone(),
            env: self.scope.env.clone(),
            issuer: issuer.clone(),
            gate: gate.clone(),
            through_seq,
            hash,
            at: now,
        };
        self.store
            .prune(&self.scope, gate, through_seq, &seal(&record, key)?)
            .await?;
        Ok(Some(through_seq))
    }

    /// Everything held, for an auditor.
    pub async fn export(&self) -> Result<Export, LedgerError> {
        let mut gates = Vec::new();
        for gate in self.store.gate_keys(&self.scope).await?.into_keys() {
            let receipts = self.store.receipts(&self.scope, &gate).await?;
            let pruned = self.store.pruned(&self.scope, &gate).await?;
            let kind = self
                .store
                .chain_kind(&self.scope, &gate)
                .await?
                .unwrap_or_default();
            gates.push(GateExport {
                gate,
                kind,
                pruned: pruned.map(|(_, envelope)| envelope),
                receipts: receipts.into_iter().map(|r| r.envelope).collect(),
            });
        }
        Ok(Export {
            v: VERSION,
            tenant: self.scope.tenant.clone(),
            env: self.scope.env.clone(),
            gates,
        })
    }

    /// The gate keys the Ledger holds, with `ledger` as its own. A convenience for tests and
    /// for setting up a stack; an auditor's trusted keys come from the customer's own record.
    pub async fn keys(&self, ledger: &SigningKey) -> Result<TrustedKeys, LedgerError> {
        let mut keys = TrustedKeys {
            ledger: vec![ledger.public()],
            ..TrustedKeys::default()
        };
        for (chain, held) in self.store.gate_keys(&self.scope).await? {
            match self.store.chain_kind(&self.scope, &chain).await? {
                Some(ChainKind::Admin) => keys.authorities.insert(chain, held),
                _ => keys.gates.insert(chain, held),
            };
        }
        Ok(keys)
    }
}
