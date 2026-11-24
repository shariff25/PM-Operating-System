//! What the tool server keeps about NVIDIA OpenShell (`docs/openshell-prover-and-sandbox-audit-
//! spec.md`): the host's policy proofs (Part A, [`proofs`]). The tool server runs no prover and
//! reads nothing from OpenShell itself: proofs arrive from a host command.
//!
//! One store seam, [`OpenShellStore`], with an in-memory implementation (tests and
//! `PMOS_DEV=1`) and a Postgres one (`pg.rs`), where proofs are `policy_proofs` rows.

pub mod pg;
pub mod proofs;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::audit::AuditSink;
use crate::store::StoreResult;
use proofs::ProofRow;

#[async_trait]
pub trait OpenShellStore: Send + Sync {
    /// Record `rows` and one `policy.prove` audit event each, all or nothing.
    async fn record_proofs(&self, rows: &[ProofRow]) -> StoreResult<()>;

    /// The newest proof of each bot, by bot.
    async fn latest_proofs(&self) -> StoreResult<Vec<ProofRow>>;
}

/// In memory, for tests and `PMOS_DEV=1`.
pub struct MemoryOpenShell {
    audit: Option<Arc<dyn AuditSink>>,
    proofs: Mutex<Vec<ProofRow>>,
}

impl Default for MemoryOpenShell {
    fn default() -> Self {
        Self::new(None)
    }
}

impl MemoryOpenShell {
    /// With `audit`, each recorded proof's `policy.prove` event goes there.
    pub fn new(audit: Option<Arc<dyn AuditSink>>) -> Self {
        Self {
            audit,
            proofs: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl OpenShellStore for MemoryOpenShell {
    async fn record_proofs(&self, rows: &[ProofRow]) -> StoreResult<()> {
        if let Some(audit) = &self.audit {
            for r in rows {
                audit.record(proofs::audit_event(r)).await?;
            }
        }
        self.proofs
            .lock()
            .expect("proofs lock")
            .extend(rows.iter().cloned());
        Ok(())
    }

    async fn latest_proofs(&self) -> StoreResult<Vec<ProofRow>> {
        Ok(proofs::newest_per_bot(
            &self.proofs.lock().expect("proofs lock"),
        ))
    }
}
