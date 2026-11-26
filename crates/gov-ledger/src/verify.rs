//! Check an export against the witness, with nothing but public keys (spec §6.2). Pure: no
//! database, no network, no clock but the one passed in.
//!
//! It answers: is every signature valid, is every chain unbroken, does every checkpoint in
//! the witness match what the export holds, and has anything gone quiet. It cannot show that
//! a gate wrote a receipt for everything it did (threat T9); that is reconciliation's job.

use std::collections::BTreeMap;

use gov_core::lease::{Checkpoint, Pruned};
use gov_core::receipt::{follow, ChainHead, Receipt};
use gov_core::{open, Digest, Envelope, Id, KeySet, Scope};

use crate::export::{Export, TrustedKeys};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// The export names a gate the verifier has no keys for.
    UnknownGate,
    /// A receipt's signature, tenant, gate or own rules do not hold. The code says which.
    BadReceipt(&'static str),
    /// A receipt does not follow the one before it: removed, reordered, repeated or replaced.
    BrokenChain,
    BadPruneRecord(&'static str),
    BadCheckpoint(&'static str),
    /// The witness has no checkpoint with this number, though it has a later one.
    CheckpointMissing,
    /// A checkpoint says the chain reached a receipt that the export does not hold.
    ReceiptMissing,
    /// A checkpoint names a different hash for a receipt than the export's receipt has.
    CheckpointMismatch,
    /// A later checkpoint shows a chain shorter than an earlier one did, or without a gate.
    HeadWentBack,
    /// No checkpoint for more than twice the interval: the Ledger stopped.
    LedgerSilent,
    /// A gate's chain has not moved for longer than the caller allows.
    GateQuiet,
    /// The witness holds no checkpoint at all, so nothing in the export is covered.
    NoCheckpoint,
    /// A gate the verifier trusts keys for is not in the export.
    GateMissing,
    /// A gate has receipts and the latest checkpoint does not list it.
    GateNotWitnessed,
    /// A receipt older than the latest checkpoint by more than the interval is not covered.
    ReceiptNotWitnessed,
    /// The prune record reaches past what the witness ever covered, or its hash ties to no
    /// checkpoint and no receipt.
    PruneNotWitnessed,
    /// Receipts were pruned sooner after a checkpoint first covered them than retention allows.
    PrunedTooSoon,
    /// The export is not in a form this verifier reads, or lists a gate twice.
    BadExport(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub problem: Problem,
    pub gate: Option<Id>,
    /// A receipt number or a checkpoint number, as the problem says.
    pub seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateReport {
    pub gate: Id,
    /// The numbers of the first and last receipt that verified, if any did.
    pub verified: Option<(u64, u64)>,
    /// Receipts newer than the last checkpoint: valid, but not yet witnessed.
    pub unwitnessed: u64,
    /// The number the Ledger says it has deleted receipts through. 0 if none.
    pub pruned_through: u64,
    /// When, by the checkpoints, the chain last moved.
    pub last_moved_at: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub receipts_verified: u64,
    pub checkpoints_verified: u64,
    pub gates: Vec<GateReport>,
    pub findings: Vec<Finding>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.findings.is_empty()
    }
}

pub struct Check<'a> {
    pub scope: &'a Scope,
    pub keys: &'a TrustedKeys,
    pub export: &'a Export,
    /// The witness's checkpoints: number and bytes, in order of number.
    pub checkpoints: &'a [(u64, Vec<u8>)],
    /// How often the Ledger is meant to publish, in seconds.
    pub interval_secs: u32,
    /// Now, if the check should also say whether the Ledger has stopped since.
    pub now: Option<i64>,
    /// Report a gate whose chain has not moved for this long. Needs `now`.
    pub gate_quiet_secs: Option<u32>,
    /// The customer's retention, in seconds: report receipts pruned sooner than this after a
    /// checkpoint first covered them.
    pub retention_secs: Option<u64>,
}

struct Chain {
    pruned_through: u64,
    pruned_hash: Option<Digest>,
    pruned_at: i64,
    hashes: BTreeMap<u64, Digest>,
    /// The time each receipt carries.
    times: BTreeMap<u64, i64>,
    /// The first receipt links to the prune record's hash, so that hash is the gate's own.
    prune_tied: bool,
}

pub fn verify(check: &Check<'_>) -> Report {
    let mut report = Report::default();
    let mut find = |problem, gate: Option<&Id>, seq| {
        report.findings.push(Finding {
            problem,
            gate: gate.cloned(),
            seq,
        })
    };
    let ledger_keys = KeySet::new(check.keys.ledger.iter().cloned());
    let mut chains: BTreeMap<Id, Chain> = BTreeMap::new();
    let mut receipts_verified = 0;

    if check.export.v != gov_core::VERSION {
        find(Problem::BadExport("unknown_version"), None, None);
    }
    if check.export.tenant != check.scope.tenant || check.export.env != check.scope.env {
        find(Problem::BadExport("wrong_tenant"), None, None);
    }
    for gate in check.keys.gates.keys() {
        if !check.export.gates.iter().any(|g| &g.gate == gate) {
            find(Problem::GateMissing, Some(gate), None);
        }
    }

    for gate in &check.export.gates {
        if chains.contains_key(&gate.gate) {
            find(
                Problem::BadExport("gate_listed_twice"),
                Some(&gate.gate),
                None,
            );
            continue;
        }
        let Some(gate_keys) = check.keys.gates.get(&gate.gate) else {
            find(Problem::UnknownGate, Some(&gate.gate), None);
            continue;
        };
        let keys = KeySet::new(gate_keys.iter().cloned());
        let mut chain = Chain {
            pruned_through: 0,
            pruned_hash: None,
            pruned_at: 0,
            hashes: BTreeMap::new(),
            times: BTreeMap::new(),
            prune_tied: false,
        };
        let mut head: Option<ChainHead> = None;
        if let Some(record) = &gate.pruned {
            match open::<Pruned>(record, &ledger_keys, check.scope) {
                Ok(p) if p.doc.gate == gate.gate && p.doc.through_seq >= 1 => {
                    chain.pruned_through = p.doc.through_seq;
                    chain.pruned_hash = Some(p.doc.hash.clone());
                    chain.pruned_at = p.doc.at;
                    head = Some(ChainHead {
                        seq: p.doc.through_seq,
                        hash: p.doc.hash,
                    });
                }
                Ok(_) => find(
                    Problem::BadPruneRecord("wrong_gate"),
                    Some(&gate.gate),
                    None,
                ),
                Err(e) => find(Problem::BadPruneRecord(e.code()), Some(&gate.gate), None),
            }
        }
        for envelope in &gate.receipts {
            let expected = head.as_ref().map_or(1, |h| h.seq.saturating_add(1));
            let opened = match open::<Receipt>(envelope, &keys, check.scope) {
                Ok(o) if o.doc.gate == gate.gate => o,
                Ok(_) => {
                    find(
                        Problem::BadReceipt("wrong_gate"),
                        Some(&gate.gate),
                        Some(expected),
                    );
                    break;
                }
                Err(e) => {
                    find(
                        Problem::BadReceipt(e.code()),
                        Some(&gate.gate),
                        Some(expected),
                    );
                    break;
                }
            };
            match follow(head.as_ref(), &opened) {
                Ok(h) => {
                    if chain.pruned_hash.is_some()
                        && h.seq == chain.pruned_through.saturating_add(1)
                    {
                        chain.prune_tied = true;
                    }
                    chain.times.insert(h.seq, opened.doc.at);
                    chain.hashes.insert(h.seq, h.hash.clone());
                    head = Some(h);
                    receipts_verified += 1;
                }
                Err(gov_core::Error::BrokenChain) => {
                    find(Problem::BrokenChain, Some(&gate.gate), Some(expected));
                    break;
                }
                Err(e) => {
                    find(
                        Problem::BadReceipt(e.code()),
                        Some(&gate.gate),
                        Some(expected),
                    );
                    break;
                }
            }
        }
        chains.insert(gate.gate.clone(), chain);
    }

    // The checkpoints, in order.
    let mut previous: Option<Checkpoint> = None;
    let mut last_number: Option<u64> = None;
    let mut last_heads: BTreeMap<Id, u64> = BTreeMap::new();
    let mut last_moved: BTreeMap<Id, i64> = BTreeMap::new();
    // For each gate, every (head number, hash, checkpoint time) the witness holds.
    let mut witnessed: BTreeMap<Id, Vec<(u64, Digest, i64)>> = BTreeMap::new();
    let mut checkpoints_verified = 0;
    let silence = i64::from(check.interval_secs) * 2;
    if check.checkpoints.is_empty() {
        find(Problem::NoCheckpoint, None, None);
    }
    for (file_seq, bytes) in check.checkpoints {
        // Numbers must rise by one from 1. A repeated or falling number is not a checkpoint.
        let expected = last_number.map_or(1, |n| n.saturating_add(1));
        if *file_seq < expected {
            find(
                Problem::BadCheckpoint("number_repeated"),
                None,
                Some(*file_seq),
            );
            continue;
        }
        if *file_seq > expected {
            find(Problem::CheckpointMissing, None, Some(expected));
        }
        last_number = Some(*file_seq);
        let envelope: Envelope = match serde_json::from_slice(bytes) {
            Ok(e) => e,
            Err(_) => {
                find(
                    Problem::BadCheckpoint("bad_envelope"),
                    None,
                    Some(*file_seq),
                );
                continue;
            }
        };
        let cp = match open::<Checkpoint>(&envelope, &ledger_keys, check.scope) {
            Ok(o) => o.doc,
            Err(e) => {
                find(Problem::BadCheckpoint(e.code()), None, Some(*file_seq));
                continue;
            }
        };
        if cp.seq != *file_seq || cp.validate().is_err() {
            find(Problem::BadCheckpoint("malformed"), None, Some(*file_seq));
            continue;
        }
        if let Some(p) = &previous {
            if cp.at < p.at {
                find(Problem::BadCheckpoint("time_went_back"), None, Some(cp.seq));
            } else if cp.at.saturating_sub(p.at) > silence {
                find(Problem::LedgerSilent, None, Some(cp.seq));
            }
        }
        let mut seen: BTreeMap<Id, u64> = BTreeMap::new();
        for h in &cp.heads {
            seen.insert(h.gate.clone(), h.seq);
            witnessed
                .entry(h.gate.clone())
                .or_default()
                .push((h.seq, h.hash.clone(), cp.at));
            if last_heads.get(&h.gate).is_none_or(|last| h.seq > *last) {
                last_moved.insert(h.gate.clone(), cp.at);
            }
            let Some(chain) = chains.get(&h.gate) else {
                // The checkpoint knows a gate the export leaves out entirely.
                find(Problem::ReceiptMissing, Some(&h.gate), Some(h.seq));
                continue;
            };
            if h.seq < chain.pruned_through {
                continue;
            }
            let held = if h.seq == chain.pruned_through {
                chain.pruned_hash.as_ref()
            } else {
                chain.hashes.get(&h.seq)
            };
            match held {
                None => find(Problem::ReceiptMissing, Some(&h.gate), Some(h.seq)),
                Some(hash) if hash != &h.hash => {
                    find(Problem::CheckpointMismatch, Some(&h.gate), Some(h.seq))
                }
                Some(_) => {}
            }
        }
        for (gate, last) in &last_heads {
            if seen.get(gate).is_none_or(|now| now < last) {
                find(Problem::HeadWentBack, Some(gate), Some(cp.seq));
            }
        }
        last_heads = seen;
        checkpoints_verified += 1;
        previous = Some(cp);
    }
    if let (Some(now), Some(p)) = (check.now, &previous) {
        if now.saturating_sub(p.at) > silence {
            find(Problem::LedgerSilent, None, None);
        }
    }

    let mut gates = Vec::new();
    for (gate, chain) in &chains {
        let covered = last_heads.get(gate).copied();
        let witnessed_through = covered.unwrap_or(0);
        let moved = last_moved.get(gate).copied();
        if let (Some(now), Some(quiet), Some(moved)) = (check.now, check.gate_quiet_secs, moved) {
            if now.saturating_sub(moved) > i64::from(quiet) {
                find(Problem::GateQuiet, Some(gate), None);
            }
        }
        // A gate with receipts that the latest checkpoint leaves out could be erased whole
        // with nothing to show for it (review finding GR-4).
        if covered.is_none() && !chain.hashes.is_empty() && previous.is_some() {
            find(Problem::GateNotWitnessed, Some(gate), None);
        }
        // So could a receipt that has sat uncovered for longer than a checkpoint takes.
        if let Some(p) = &previous {
            let stale = chain.times.iter().find(|(seq, at)| {
                **seq > witnessed_through
                    && p.at.saturating_sub(**at) > i64::from(check.interval_secs)
            });
            if let Some((seq, _)) = stale {
                find(Problem::ReceiptNotWitnessed, Some(gate), Some(*seq));
            }
        }
        // The prune record is the Ledger's word alone (review finding GR-3). It is believed
        // only as far as the witness or the gate's own next receipt bears it out.
        if let Some(hash) = &chain.pruned_hash {
            let seen = witnessed.get(gate).map(Vec::as_slice).unwrap_or_default();
            let in_witness = seen
                .iter()
                .any(|(seq, h, _)| *seq == chain.pruned_through && h == hash);
            if chain.pruned_through > witnessed_through || !(in_witness || chain.prune_tied) {
                find(
                    Problem::PruneNotWitnessed,
                    Some(gate),
                    Some(chain.pruned_through),
                );
            }
            if let Some(retention) = check.retention_secs {
                // The first checkpoint that covered the last pruned receipt: it arrived no
                // earlier than the checkpoint before that one, so it was at most this old.
                let first_cover = seen
                    .iter()
                    .filter(|(seq, _, _)| *seq >= chain.pruned_through)
                    .map(|(_, _, at)| *at)
                    .min();
                let too_soon = first_cover.is_none_or(|covered_at| {
                    u64::try_from(chain.pruned_at.saturating_sub(covered_at)).unwrap_or(0)
                        < retention
                });
                if too_soon {
                    find(
                        Problem::PrunedTooSoon,
                        Some(gate),
                        Some(chain.pruned_through),
                    );
                }
            }
        }
        gates.push(GateReport {
            gate: gate.clone(),
            verified: chain
                .hashes
                .keys()
                .next()
                .zip(chain.hashes.keys().next_back())
                .map(|(a, b)| (*a, *b)),
            unwitnessed: chain
                .hashes
                .keys()
                .filter(|seq| **seq > witnessed_through)
                .count() as u64,
            pruned_through: chain.pruned_through,
            last_moved_at: moved,
        });
    }
    report.receipts_verified = receipts_verified;
    report.checkpoints_verified = checkpoints_verified;
    report.gates = gates;
    report
}
