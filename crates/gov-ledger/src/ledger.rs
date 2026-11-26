//! Intake, checkpoints, pruning and export (spec §6).

use gov_core::lease::{Checkpoint, GateHead, Pruned};
use gov_core::receipt::{follow, Receipt};
use gov_core::{open, seal, Digest, Envelope, Error, Id, KeySet, Scope, SigningKey, VERSION};

use crate::export::{Export, GateExport, TrustedKeys};
use crate::store::{Refusal, Store, StoreError, StoredReceipt};
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
                    kid: envelope.kid.clone(),
                    code: code.to_string(),
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
        let opened = match open::<Receipt>(envelope, &KeySet::new([key]), &self.scope) {
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
            let new_head = match follow(head.as_ref(), &opened) {
                Ok(h) => h,
                Err(e) => return self.refuse(envelope, e.code(), true, now).await,
            };
            let stored = StoredReceipt {
                seq: new_head.seq,
                hash: new_head.hash,
                at: opened.doc.at,
                envelope: envelope.clone(),
            };
            match self
                .store
                .append(&self.scope, &gate, head.map_or(0, |h| h.seq), &stored)
                .await
            {
                Ok(()) => return Ok(Ingested::Added),
                Err(StoreError::HeadMoved) => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(StoreError::HeadMoved.into())
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

    /// Delete a gate's receipts older than `cutoff`, leaving a signed record of where the
    /// chain was cut (spec §6.3). Only receipts the last checkpoint already covers can go:
    /// nothing is deleted before the witness has its hash. Returns the number pruned through.
    pub async fn prune(
        &self,
        gate: &Id,
        cutoff: i64,
        key: &SigningKey,
        issuer: &Id,
        now: i64,
    ) -> Result<Option<u64>, LedgerError> {
        let Some((_, envelope)) = self.store.last_checkpoint(&self.scope).await? else {
            return Ok(None);
        };
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
            gates.push(GateExport {
                gate,
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
        Ok(TrustedKeys {
            ledger: vec![ledger.public()],
            gates: self.store.gate_keys(&self.scope).await?,
        })
    }
}
