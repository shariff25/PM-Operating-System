//! Host proofs (`docs/openshell-prover-and-sandbox-audit-spec.md` §A.5, slice A3): what
//! `pmos-admin policy prove` found for each bot's running sandbox, recorded append-only in
//! `policy_proofs` with an audit event `policy.prove`.
//!
//! The tool server never runs the prover (PRV-1): it only stores what the host command hands it,
//! after checking every field (a status from the fixed set, a valid bot name, hashes that are
//! hashes, a counterexample cut to 256 printable characters), and serves the newest row per bot
//! to Policy Admin. A row that claims `proved` without all five domains is refused here and by
//! the table's own check.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::audit::{AuditEvent, Client, Outcome};
use crate::users::HostUser;
use pmos_policy::prove::{clean, Status, DOMAINS, MAX_COUNTEREXAMPLE, MAX_REASON};

/// The audit action of a recorded proof.
pub const TOOL: &str = "policy.prove";

/// The run id proofs are recorded under.
pub const RUN: &str = "policy-proof";

/// The most bots one record call takes.
pub const MAX_BOTS: usize = 500;

/// One proof, as stored and served.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProofRow {
    pub at: DateTime<Utc>,
    pub bot: String,
    pub status: Status,
    pub reason_code: Option<String>,
    pub reason: Option<String>,
    pub coverage: Vec<String>,
    pub counterexample: Option<String>,
    pub prover_version: Option<String>,
    pub prover_sha256: Option<String>,
    pub boundary_sha256: Option<String>,
    pub candidate_sha256: Option<String>,
    pub host_user: String,
}

fn hash(v: Option<&Value>) -> Result<Option<String>, String> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::String(s))
            if s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) =>
        {
            Ok(Some(s.clone()))
        }
        Some(_) => Err("a SHA-256 isn't 64 lowercase hex characters".into()),
    }
}

fn text(v: Option<&Value>, max: usize) -> Option<String> {
    v.and_then(Value::as_str)
        .map(|s| clean(s, max))
        .filter(|s| !s.is_empty())
}

/// The rows of a `pmos policy prove --format json` report, checked field by field. `Err` (and
/// nothing is recorded) when any part isn't what the report's shape allows.
pub fn rows_from_report(
    report: &Value,
    host_user: &HostUser,
    at: DateTime<Utc>,
) -> Result<Vec<ProofRow>, String> {
    let prover = report.get("prover").ok_or("the report has no `prover`")?;
    let prover_version = text(prover.get("version"), 64);
    let prover_sha256 = hash(prover.get("sha256"))?;
    let boundary_sha256 = hash(report.get("boundary_sha256"))?;
    let results = report
        .get("results")
        .and_then(Value::as_array)
        .ok_or("the report has no `results`")?;
    if results.is_empty() {
        return Err("the report has no results".into());
    }
    if results.len() > MAX_BOTS {
        return Err(format!("at most {MAX_BOTS} bots at once"));
    }
    let mut rows = Vec::with_capacity(results.len());
    for r in results {
        let bot = r
            .get("bot")
            .and_then(Value::as_str)
            .filter(|b| pmos_contract::bots::valid_name(b))
            .ok_or("a result's `bot` isn't a bot name")?;
        let status = r
            .get("status")
            .and_then(Value::as_str)
            .and_then(Status::parse)
            .ok_or("a result's `status` isn't one PM OS knows")?;
        let mut coverage: Vec<String> = r
            .get("coverage")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .filter(|d| DOMAINS.contains(d))
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        coverage.sort();
        coverage.dedup();
        if status == Status::Proved && coverage.len() != DOMAINS.len() {
            return Err(format!(
                "`{bot}` is reported proved without all five domains"
            ));
        }
        let candidate_sha256 = match r.get("candidate_sha256") {
            Some(Value::String(s)) if s.is_empty() => None,
            other => hash(other)?,
        };
        rows.push(ProofRow {
            at,
            bot: bot.to_string(),
            status,
            reason_code: text(r.get("reason_code"), MAX_REASON),
            reason: text(r.get("reason"), MAX_REASON),
            coverage,
            counterexample: text(r.get("counterexample"), MAX_COUNTEREXAMPLE),
            prover_version: prover_version.clone(),
            prover_sha256: prover_sha256.clone(),
            boundary_sha256: boundary_sha256.clone(),
            candidate_sha256,
            host_user: host_user.as_str().to_string(),
        });
    }
    Ok(rows)
}

/// A proof's audit outcome: `ok` for `proved`, `denied` for `exceeds`, `error` otherwise.
pub fn outcome_of(status: Status) -> Outcome {
    match status {
        Status::Proved => Outcome::Ok,
        Status::Exceeds => Outcome::Denied,
        _ => Outcome::Error,
    }
}

/// The audit event of one recorded proof: client `host`, the bot, the status as `output_ref`,
/// and the OS user in the fixed detail `host_user <name>`. No counterexample, no reason.
pub fn audit_event(row: &ProofRow) -> AuditEvent {
    AuditEvent {
        at: row.at,
        client: Client::Host,
        run_id: RUN.into(),
        user: "host".into(),
        skill: None,
        bot: Some(row.bot.clone()),
        persona: None,
        tool: TOOL.into(),
        outcome: outcome_of(row.status),
        sources: Vec::new(),
        source_titles: Default::default(),
        args_hash: None,
        output_ref: Some(row.status.as_str().into()),
        detail: Some(format!("host_user {}", row.host_user)),
    }
}

/// The largest report `policy-proof record` reads.
pub const MAX_REPORT: usize = 4 << 20;

/// `pmos-toolserver policy-proof record`: the host command `pmos-admin policy prove` runs in a
/// one-off tool server container. Reads a `pmos policy prove --format json` report on standard
/// input and records one row per bot with `PMOS_HOST_USER`. Exit 0 done; 1 failed (database,
/// a report that isn't one); 2 usage (`PMOS_HOST_USER`, `PMOS_DATABASE_URL`, arguments).
pub async fn host_main(args: Vec<String>) -> i32 {
    use std::io::Read;
    if args != ["record"] {
        eprintln!("usage: pmos-toolserver policy-proof record < report.json");
        return 2;
    }
    let Some(host_user) = std::env::var("PMOS_HOST_USER")
        .ok()
        .filter(|s| !s.is_empty())
    else {
        eprintln!(
            "PMOS_HOST_USER is required: run this through `deploy/pmos-admin policy prove`, which passes the OS user who ran it"
        );
        return 2;
    };
    let Some(host_user) = HostUser::parse(&host_user) else {
        eprintln!("PMOS_HOST_USER must be an OS user name (^[a-z_][a-z0-9_.-]{{0,31}}$)");
        return 2;
    };
    let Some(url) = std::env::var("PMOS_DATABASE_URL")
        .ok()
        .filter(|s| !s.is_empty())
    else {
        eprintln!("policy-proof record needs PMOS_DATABASE_URL");
        return 2;
    };
    let mut raw = Vec::new();
    if std::io::stdin()
        .take(MAX_REPORT as u64 + 1)
        .read_to_end(&mut raw)
        .is_err()
        || raw.len() > MAX_REPORT
    {
        eprintln!("the report on standard input couldn't be read (at most 4 MiB)");
        return 1;
    }
    let report: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("standard input isn't a `pmos policy prove --format json` report");
            return 1;
        }
    };
    let rows = match rows_from_report(&report, &host_user, Utc::now()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("the report was refused, nothing recorded: {e}");
            return 1;
        }
    };
    let store = match crate::pg::PgStore::connect(&url).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("can't reach the tool server's database: {e}");
            return 1;
        }
    };
    if let Err(e) = store.migrate().await {
        eprintln!("applying the tool server's migrations failed: {e}");
        return 1;
    }
    use super::OpenShellStore;
    if let Err(e) = store.record_proofs(&rows).await {
        eprintln!("recording the proofs failed, nothing recorded: {e}");
        return 1;
    }
    for r in &rows {
        println!("recorded {}: {}", r.bot, r.status.as_str());
    }
    0
}

/// The newest row per bot, from rows in any order.
pub fn newest_per_bot(rows: &[ProofRow]) -> Vec<ProofRow> {
    let mut by_bot: std::collections::BTreeMap<&str, &ProofRow> = Default::default();
    for r in rows {
        match by_bot.get(r.bot.as_str()) {
            Some(prev) if prev.at > r.at => {}
            _ => {
                by_bot.insert(&r.bot, r);
            }
        }
    }
    by_bot.into_values().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn raj() -> HostUser {
        HostUser::parse("raj").unwrap()
    }

    fn report(results: Value) -> Value {
        json!({
            "prover": {"version": "0.1.9", "sha256": "a".repeat(64), "pinned_version": "0.1.9"},
            "boundary_sha256": "b".repeat(64),
            "results": results,
        })
    }

    #[test]
    fn a_report_becomes_checked_rows() {
        let at = Utc::now();
        let r = report(json!([
            {"bot": "docs", "status": "proved", "candidate_sha256": "c".repeat(64),
             "coverage": ["filesystem", "network_l4", "network_rest", "process", "landlock"]},
            {"bot": "pm-ana", "status": "exceeds", "counterexample": "filesystem write /sandbox\n<x>",
             "coverage": []},
            {"bot": "pm-raj", "status": "not_available", "reason_code": "no_binary", "candidate_sha256": ""},
        ]));
        let rows = rows_from_report(&r, &raj(), at).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].status, Status::Proved);
        assert_eq!(
            rows[1].counterexample.as_deref(),
            Some("filesystem write /sandbox <x>")
        );
        assert_eq!(rows[2].candidate_sha256, None);
        assert!(rows.iter().all(|r| r.host_user == "raj"));
        let e = audit_event(&rows[1]);
        assert_eq!(e.outcome, Outcome::Denied);
        assert_eq!(e.client, Client::Host);
        assert_eq!(e.detail.as_deref(), Some("host_user raj"));
        assert!(crate::users::detail::is_safe("host_user raj"));
        assert_eq!(audit_event(&rows[2]).outcome, Outcome::Error);
        assert_eq!(audit_event(&rows[0]).outcome, Outcome::Ok);
    }

    #[test]
    fn anything_off_shape_is_refused_whole() {
        let bad = [
            json!([{"bot": "Docs", "status": "proved"}]),
            json!([{"bot": "docs", "status": "passed"}]),
            json!([{"bot": "docs", "status": "proved", "coverage": ["filesystem"]}]),
            json!([{"bot": "docs", "status": "error", "candidate_sha256": "nothex"}]),
            json!([]),
        ];
        for results in bad {
            assert!(
                rows_from_report(&report(results.clone()), &raj(), Utc::now()).is_err(),
                "{results}"
            );
        }
        assert!(rows_from_report(&json!({"results": []}), &raj(), Utc::now()).is_err());
    }
}
