//! `gov verify` as an auditor runs it: files in, a verdict and an exit code out.

use std::path::Path;
use std::process::Command;

use gov_core::action::{CanonicalAction, Kind};
use gov_core::receipt::{
    Coverage, Decision, DecisionKind, Mode, Phase, Proof, Reason, Receipt, ReceiptAction,
    ReceiptAgent,
};
use gov_core::{open, seal, Id, KeySet, ResourcePath, Scope, SigningKey};
use gov_ledger::{DirWitness, Ledger, MemoryStore};
use serde_json::json;

const T0: i64 = 1_800_000_000;

fn id(s: &str) -> Id {
    Id::new(s).unwrap()
}

fn gov(dir: &Path, extra: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_gov"))
        .args(["verify", "--tenant", "acme", "--env", "prod"])
        .arg("--keys")
        .arg(dir.join("keys.json"))
        .arg("--export")
        .arg(dir.join("export.json"))
        .arg("--witness")
        .arg(dir.join("witness"))
        .args(extra)
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[tokio::test]
async fn an_auditor_verifies_with_files_alone() {
    let scope = Scope::new("acme", "prod").unwrap();
    let gate = SigningKey::from_seed(id("gate-1-key"), &[20; 32]).unwrap();
    let ledger_key = SigningKey::from_seed(id("ledger-1"), &[50; 32]).unwrap();
    let ledger = Ledger::new(MemoryStore::default(), scope.clone());
    ledger
        .enrol_gate_key(&id("gate-1"), &gate.public())
        .await
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let witness = DirWitness::new(dir.path().join("witness"));
    let mut prev = None;
    for seq in 1..=3u64 {
        let action = CanonicalAction::new(
            id("jira"),
            id("issue.read"),
            Kind::Read,
            ResourcePath::new("jira/project/PAY/issue/PAY-12").unwrap(),
            None,
            &json!({"n": seq}),
            &[9; 32],
        )
        .unwrap();
        let receipt = Receipt {
            v: 1,
            tenant: id("acme"),
            env: id("prod"),
            gate: id("gate-1"),
            seq,
            prev: prev.clone(),
            at: T0,
            at_clamped: false,
            phase: Phase::Intent,
            mode: Mode::Enforce,
            coverage: Coverage::Custody,
            agent: ReceiptAgent {
                id: id("agent-1"),
                proof: Proof::Proven,
                failure: None,
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
            outcome: None,
            intent: None,
        };
        let env = seal(&receipt, &gate).unwrap();
        prev = Some(
            open::<Receipt>(&env, &KeySet::new([gate.public()]), &scope)
                .unwrap()
                .hash,
        );
        ledger.ingest(&env, T0).await.unwrap();
    }
    ledger
        .checkpoint(&ledger_key, &id("ledger"), T0 + 60, &witness)
        .await
        .unwrap();

    let export = ledger.export().await.unwrap();
    let keys = ledger.keys(&ledger_key).await.unwrap();
    std::fs::write(
        dir.path().join("keys.json"),
        serde_json::to_vec(&keys).unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("export.json"),
        serde_json::to_vec(&export).unwrap(),
    )
    .unwrap();

    // `--now` defaults to this machine's clock; the tests name the time they mean.
    let soon = (T0 + 70).to_string();
    let (code, out) = gov(dir.path(), &["--now", &soon]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("3 receipts and 1 checkpoints verified"),
        "{out}"
    );
    assert!(out.contains("OK:"), "{out}");
    // The verdict says what it does not show.
    assert!(
        out.contains("does not show that a gate recorded everything"),
        "{out}"
    );

    // The Ledger has since stopped.
    let (code, out) = gov(dir.path(), &["--now", &(T0 + 600).to_string()]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("no checkpoint for more than twice the interval"),
        "{out}"
    );

    // With no --now the command uses the clock, so a witness that ends in the past is never
    // passed as current. (The test's checkpoint is dated in 2027; skip once that has passed.)
    let today = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    if today > T0 + 600 {
        let (code, out) = gov(dir.path(), &[]);
        assert_eq!(code, 1, "{out}");
    }

    // An empty witness is a failure, not "nothing to check".
    let empty = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_gov"))
        .args([
            "verify", "--tenant", "acme", "--env", "prod", "--now", &soon,
        ])
        .arg("--keys")
        .arg(dir.path().join("keys.json"))
        .arg("--export")
        .arg(dir.path().join("export.json"))
        .arg("--witness")
        .arg(empty.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("the witness holds no checkpoint"));

    // The Ledger says it pruned: without --retention-days that is a failure, and with it the
    // verdict no longer says "nothing removed".
    let pruned_at = T0 + 40 * 86_400;
    ledger
        .prune(
            &id("gate-1"),
            T0 + 1,
            &ledger_key,
            &id("ledger"),
            pruned_at,
            &witness,
        )
        .await
        .unwrap();
    let after = ledger.export().await.unwrap();
    std::fs::write(
        dir.path().join("export.json"),
        serde_json::to_vec(&after).unwrap(),
    )
    .unwrap();
    let then = (pruned_at + 10).to_string();
    let quiet = ["--now", then.as_str(), "--interval", "9999999"];
    let (code, out) = gov(dir.path(), &quiet);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("pass --retention-days"), "{out}");
    let (code, out) = gov(
        dir.path(),
        &[&quiet[..], &["--retention-days", "30"]].concat(),
    );
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("the Ledger says it deleted receipts 1 to 3"),
        "{out}"
    );
    assert!(
        !out.contains("nothing altered, removed or missing"),
        "{out}"
    );
    let (code, out) = gov(
        dir.path(),
        &[&quiet[..], &["--retention-days", "60"]].concat(),
    );
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("pruned sooner than the retention allows"),
        "{out}"
    );
    std::fs::write(
        dir.path().join("export.json"),
        serde_json::to_vec(&export).unwrap(),
    )
    .unwrap();

    // A receipt removed from the export.
    let mut cut = export.clone();
    cut.gates[0].receipts.remove(1);
    std::fs::write(
        dir.path().join("export.json"),
        serde_json::to_vec(&cut).unwrap(),
    )
    .unwrap();
    let (code, out) = gov(dir.path(), &[]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("PROBLEM gate gate-1 number 2: the chain is broken here"),
        "{out}"
    );
    assert!(out.contains("FAILED"), "{out}");

    // Files that are not what they should be: an error, not a verdict.
    std::fs::write(dir.path().join("export.json"), b"{}").unwrap();
    let (code, out) = gov(dir.path(), &["--now", &soon]);
    assert_eq!(code, 2, "{out}");
    assert!(!out.contains("OK:"), "{out}");
}
