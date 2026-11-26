//! Shared by the Ledger's tests: keys, receipts, and a Postgres store in a fresh schema.
#![allow(dead_code)]

use std::str::FromStr;

use gov_core::action::{CanonicalAction, Kind};
use gov_core::receipt::{
    Coverage, Decision, DecisionKind, Mode, Outcome, OutcomeStatus, Phase, Proof, Reason, Receipt,
    ReceiptAction, ReceiptAgent,
};
use gov_core::{seal, Digest, Envelope, Id, ResourcePath, Scope, SigningKey};
use gov_ledger::pg::PgStore;
use serde_json::json;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions, Connection};

pub const T0: i64 = 1_800_000_000;

pub fn id(s: &str) -> Id {
    Id::new(s).unwrap()
}
pub fn scope() -> Scope {
    Scope::new("acme", "prod").unwrap()
}
pub fn key(kid: &str, seed: u8) -> SigningKey {
    SigningKey::from_seed(id(kid), &[seed; 32]).unwrap()
}
pub fn ledger_key() -> SigningKey {
    key("ledger-1", 50)
}

/// Receipt `seq` of `gate`'s chain, after `prev`, written at `at`. Odd numbers are intents and
/// even ones the outcomes of the intents before them.
pub fn receipt(gate: &str, seq: u64, prev: Option<Digest>, at: i64) -> Receipt {
    // An outcome is for the same action as the intent before it.
    let outcome = seq % 2 == 0;
    let call = if outcome { seq - 1 } else { seq };
    let action = CanonicalAction::new(
        id("jira"),
        id("issue.update"),
        Kind::Write,
        ResourcePath::new("jira/project/PAY/issue/PAY-12").unwrap(),
        None,
        &json!({"n": call}),
        &[9; 32],
    )
    .unwrap();
    Receipt {
        v: 1,
        tenant: id("acme"),
        env: id("prod"),
        gate: id(gate),
        seq,
        prev,
        at,
        at_clamped: false,
        phase: if outcome {
            Phase::Outcome
        } else {
            Phase::Intent
        },
        mode: Mode::Enforce,
        coverage: Coverage::Custody,
        agent: ReceiptAgent {
            id: id("agent-1"),
            proof: Proof::Proven,
        },
        mandate: vec![id("m-1")],
        policy_version: Some(id("7")),
        action: ReceiptAction::from(&action),
        reversal: gov_core::action::Reversal::Restore,
        before_digest: None,
        reverses: None,
        decision: Decision {
            kind: DecisionKind::Allow,
            reason: Reason::Ok,
        },
        would_decide: None,
        countersign: None,
        outcome: outcome.then(|| Outcome {
            status: OutcomeStatus::Ok,
            upstream_ref: Some(id("PAY-12")),
            response_digest: None,
        }),
        intent: outcome.then(|| seq - 1),
    }
}

/// The hash a chain links on, as the Ledger and a verifier compute it.
pub fn hash_of(envelope: &Envelope, gate_key: &SigningKey) -> Digest {
    let keys = gov_core::KeySet::new([gate_key.public()]);
    gov_core::open::<Receipt>(envelope, &keys, &scope())
        .unwrap()
        .hash
}

/// `n` sealed receipts for `gate`, one every `step` seconds from `start`.
pub fn chain(gate: &str, gate_key: &SigningKey, n: u64, start: i64, step: i64) -> Vec<Envelope> {
    let mut out = Vec::new();
    let mut prev = None;
    for seq in 1..=n {
        let env = seal(
            &receipt(gate, seq, prev.clone(), start + (seq as i64 - 1) * step),
            gate_key,
        )
        .unwrap();
        prev = Some(hash_of(&env, gate_key));
        out.push(env);
    }
    out
}

/// A Postgres store in a fresh schema of Agent Governance's own test database. Needs
/// `GOV_TEST_DATABASE_URL`; skipped locally without it and fails in CI (`CI` set).
pub struct TestPg {
    pub store: Option<PgStore>,
    base: PgConnectOptions,
    schema: String,
}

impl TestPg {
    pub async fn new() -> Option<Self> {
        let Ok(url) = std::env::var("GOV_TEST_DATABASE_URL") else {
            assert!(
                std::env::var_os("CI").is_none(),
                "GOV_TEST_DATABASE_URL must be set in CI so the Postgres tests run"
            );
            eprintln!("skipping Postgres case: GOV_TEST_DATABASE_URL is not set");
            return None;
        };
        let base = PgConnectOptions::from_str(&url).expect("GOV_TEST_DATABASE_URL parses");
        let schema = format!("g_{}", uuid::Uuid::new_v4().simple());
        let mut admin = base.connect().await.expect("connect");
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&mut admin)
            .await
            .expect("create schema");
        admin.close().await.ok();
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect_with(base.clone().options([("search_path", schema.as_str())]))
            .await
            .expect("connect to test schema");
        let store = PgStore::from_pool(pool);
        store.migrate().await.expect("migrations apply");
        Some(TestPg {
            store: Some(store),
            base,
            schema,
        })
    }

    pub async fn drop_schema(self) {
        let mut admin = self.base.connect().await.expect("connect");
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP SCHEMA {} CASCADE",
            self.schema
        )))
        .execute(&mut admin)
        .await
        .ok();
    }
}
