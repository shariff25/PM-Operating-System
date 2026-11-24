//! `pmos policy prove` against the stand-in prover (`deploy/test/stand-ins/openshell-prover`):
//! the classification matrix, the pin (hash and version), the network view, the disagreement
//! with `pmos policy check`, and the hand-written boundary (spec §A.8). Nothing here runs the
//! real prover; slice A2's CI job does, when the owner pins one.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use pmos_policy::prove::{
    self, network_view, network_view_text, prove_all, sha256_hex, Candidate, Finding, Lock,
    Prover, Status, VIEW_NOTE, VIEW_REMOVED_KEYS,
};
use pmos_policy::{openshell, Service};
use serde_json::{json, Value};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn docs_policy() -> String {
    fs::read_to_string(repo().join("policies/bots/docs.yaml")).unwrap()
}

/// A copy of the stand-in in its own directory (its `answers` and `version` files sit beside
/// it), with a lock that pins exactly that copy.
struct Stand {
    dir: tempfile::TempDir,
    prover: PathBuf,
}

impl Stand {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let prover = dir.path().join("openshell-prover");
        fs::copy(
            repo().join("deploy/test/stand-ins/openshell-prover"),
            &prover,
        )
        .unwrap();
        Self { dir, prover }
    }

    fn sha(&self) -> String {
        sha256_hex(&fs::read(&self.prover).unwrap())
    }

    fn lock(&self) -> Lock {
        Lock::parse(&format!(
            "PROVER_VERSION=0.1.9\nPROVER_BINARY_SHA256_{}={}\n",
            std::env::consts::ARCH.to_ascii_uppercase(),
            self.sha()
        ))
        .unwrap()
    }

    fn answer(&self, view_text: &str, answer: &str) {
        let line = format!("{} {answer}\n", sha256_hex(view_text.as_bytes()));
        let path = self.dir.path().join("answers");
        let mut all = fs::read_to_string(&path).unwrap_or_default();
        all.push_str(&line);
        fs::write(path, all).unwrap();
    }

    fn version(&self, v: &str) {
        fs::write(self.dir.path().join("version"), v).unwrap();
    }

    fn with(&self, lock: Lock) -> Prover {
        let mut p = Prover::new(self.prover.clone(), lock);
        p.kill_after = Duration::from_secs(3);
        p
    }
}

fn boundary() -> PathBuf {
    repo().join(prove::BOUNDARY_PATH)
}

fn candidate_from(dir: &Path, bot: &str, text: &str, failed: &[&str]) -> Candidate {
    let path = dir.join(format!("{bot}.yaml"));
    fs::write(&path, text).unwrap();
    Candidate {
        bot: bot.into(),
        label: path.display().to_string(),
        path,
        check_failed: failed.iter().map(|s| s.to_string()).collect(),
    }
}

fn prove_one(prover: &Prover, text: &str) -> prove::BotResult {
    let work = tempfile::tempdir().unwrap();
    let c = candidate_from(work.path(), "docs", text, &[]);
    let report = prove_all(prover, &boundary(), &[c], work.path());
    report.results.into_iter().next().unwrap()
}

#[test]
fn the_stand_in_matrix_only_within_is_proved() {
    let cases = [
        ("within", Status::Proved),
        ("exceeds", Status::Exceeds),
        ("unsupported", Status::Unsupported),
        ("inconclusive", Status::Inconclusive),
        ("exit2", Status::Error),
        ("exit0-exceeds", Status::Error),
        ("exit1-within", Status::Error),
        ("missing-domain", Status::Error),
        ("not-json", Status::Error),
        ("hang", Status::Error),
    ];
    let (view, changed) = network_view_text(&docs_policy()).unwrap();
    assert!(changed, "the Docs bot's tool-server endpoint is MCP");
    for (answer, want) in cases {
        let stand = Stand::new();
        stand.answer(&view, answer);
        let prover = stand.with(stand.lock());
        let r = prove_one(&prover, &docs_policy());
        assert_eq!(r.outcome.status, want, "answer {answer}: {:?}", r.outcome);
        assert_eq!(r.outcome.status.passes(), answer == "within", "{answer}");
        assert_eq!(r.view, Some(VIEW_NOTE));
        if answer == "exceeds" {
            assert_eq!(
                r.outcome.counterexample.as_deref(),
                Some("filesystem write /sandbox")
            );
        }
        if answer == "hang" {
            assert_eq!(r.outcome.reason_code.as_deref(), Some("timeout"));
        }
    }
}

#[test]
fn the_unmodified_policy_is_unsupported_and_its_view_is_what_gets_proved() {
    let stand = Stand::new();
    let prover = stand.with(stand.lock());
    // Straight to the prover, without the view: the MCP rules make it unsupported.
    let work = tempfile::tempdir().unwrap();
    let raw = work.path().join("docs.yaml");
    fs::write(&raw, docs_policy()).unwrap();
    let o = prover.check(&raw, &boundary());
    assert_eq!(o.status, Status::Unsupported);
    assert_eq!(o.reason_code.as_deref(), Some("unsupported_policy_shape"));
    // Through `prove_all`, the view.
    let r = prove_one(&prover, &docs_policy());
    assert_eq!(r.outcome.status, Status::Proved);
    assert_eq!(r.outcome.coverage.len(), 5);
    assert!(r.view_sha256.is_some());
    assert_eq!(r.candidate_sha256, prove::policy_hash(&docs_policy()));
}

#[test]
fn a_wrong_hash_a_missing_binary_or_no_pin_is_not_available() {
    let stand = Stand::new();
    // Another binary's hash pinned.
    let other = Lock::parse(&format!(
        "PROVER_VERSION=0.1.9\nPROVER_BINARY_SHA256_{}={}\n",
        std::env::consts::ARCH.to_ascii_uppercase(),
        "0".repeat(64)
    ))
    .unwrap();
    let r = prove_one(&stand.with(other), &docs_policy());
    assert_eq!(r.outcome.status, Status::NotAvailable);
    assert_eq!(r.outcome.reason_code.as_deref(), Some("hash_mismatch"));
    // Nothing pinned: the committed lock as it is today.
    let committed = Lock::read(&repo().join(prove::LOCK_PATH)).unwrap();
    let r = prove_one(&stand.with(committed), &docs_policy());
    assert_eq!(r.outcome.status, Status::NotAvailable);
    assert_eq!(r.outcome.reason_code.as_deref(), Some("not_pinned"));
    // No binary.
    let mut missing = stand.with(stand.lock());
    missing.path = stand.dir.path().join("nothing-here");
    let r = prove_one(&missing, &docs_policy());
    assert_eq!(r.outcome.status, Status::NotAvailable);
    assert_eq!(r.outcome.reason_code.as_deref(), Some("no_binary"));
}

#[test]
fn another_version_is_a_mismatch_older_says_too_old() {
    for (v, code) in [
        ("0.1.8", "version_too_old"),
        ("0.2.0", "version_different"),
        ("garbled", "version_unknown"),
    ] {
        let stand = Stand::new();
        stand.version(v);
        let r = prove_one(&stand.with(stand.lock()), &docs_policy());
        assert_eq!(r.outcome.status, Status::VersionMismatch, "{v}");
        assert_eq!(r.outcome.reason_code.as_deref(), Some(code), "{v}");
        if v == "0.1.8" {
            let reason = r.outcome.reason.unwrap();
            assert!(reason.contains("too old") && reason.contains("pinned 0.1.9"), "{reason}");
        }
    }
}

#[test]
fn the_prover_gets_no_environment_but_a_fixed_path() {
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("openshell-prover");
    let seen = dir.path().join("env.txt");
    fs::write(
        &fake,
        format!(
            "#!/usr/bin/env bash\nenv > '{}'\nif [ \"$1\" = --version ]; then echo 'openshell-prover 0.1.9'; exit 0; fi\nprintf '{{\"result\":\"within_boundary\",\"coverage\":{{\"domains\":[\"filesystem\",\"network_l4\",\"network_rest\",\"process\",\"landlock\"]}}}}\\n'\n",
            seen.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let lock = Lock::parse(&format!(
        "PROVER_VERSION=0.1.9\nPROVER_BINARY_SHA256_{}={}\n",
        std::env::consts::ARCH.to_ascii_uppercase(),
        sha256_hex(&fs::read(&fake).unwrap())
    ))
    .unwrap();
    let r = prove_one(&Prover::new(fake, lock), &docs_policy());
    assert_eq!(r.outcome.status, Status::Proved);
    let env = fs::read_to_string(&seen).unwrap();
    let names: BTreeSet<&str> = env
        .lines()
        .filter_map(|l| l.split_once('=').map(|(k, _)| k))
        .collect();
    // bash itself sets PWD, SHLVL and _; nothing comes from this process (HOME, tokens, CI).
    for name in &names {
        assert!(
            ["PATH", "PWD", "SHLVL", "_", "OLDPWD"].contains(name),
            "the prover saw {name}"
        );
    }
    assert!(names.contains("PATH"));
}

#[test]
fn a_policy_that_passes_the_check_but_exceeds_is_a_disagreement() {
    // A hand edit that every mapped rule accepts (a path inside /tmp is still writable only
    // where /tmp is), but which the stand-in is told exceeds.
    let edited = docs_policy().replace("    - /tmp\n", "    - /tmp\n    - /tmp/extra\n");
    assert_ne!(edited, docs_policy());
    let ctx = pmos_policy::generate(&repo().join("content"), chrono::Local::now().date_naive())
        .unwrap()
        .bots
        .into_iter()
        .find(|b| b.bot == "docs")
        .unwrap();
    let mapped = prove::all_mapped_rules();
    let failed: Vec<_> = pmos_policy::check_policy(&ctx, "docs.yaml", &edited)
        .into_iter()
        .filter(|v| mapped.contains(v.rule))
        .collect();
    assert!(failed.is_empty(), "{failed:?}");
    let (view, _) = network_view_text(&edited).unwrap();
    let stand = Stand::new();
    stand.answer(&view, "exceeds");
    let prover = stand.with(stand.lock());
    let work = tempfile::tempdir().unwrap();
    let c = candidate_from(work.path(), "docs", &edited, &[]);
    let report = prove_all(&prover, &boundary(), &[c], work.path());
    assert_eq!(report.results[0].finding, Some(Finding::Disagreement));
    assert!(!report.passed());
    assert!(prove::render_text(&report).contains("DISAGREEMENT"));
    // The same exceed with the filesystem rule failing is explained by the check.
    let c = candidate_from(work.path(), "docs", &edited, &["fs-writable"]);
    let report = prove_all(&prover, &boundary(), &[c], work.path());
    assert_eq!(report.results[0].finding, None);
    // A pass while a mapped rule fails: check-stricter, listed for review.
    let stand = Stand::new();
    let c = candidate_from(work.path(), "docs", &edited, &["net-allowed-ips"]);
    let report = prove_all(&stand.with(stand.lock()), &boundary(), &[c], work.path());
    assert_eq!(report.results[0].outcome.status, Status::Proved);
    assert_eq!(report.results[0].finding, Some(Finding::CheckStricter));
    assert!(prove::render_text(&report).contains("check-stricter"));
}

#[test]
fn every_bot_is_proved_in_order_and_the_report_says_what_is_never_proved() {
    let stand = Stand::new();
    let prover = stand.with(stand.lock());
    let work = tempfile::tempdir().unwrap();
    let a = candidate_from(work.path(), "zeta", &docs_policy(), &[]);
    let b = candidate_from(work.path(), "alpha", &docs_policy(), &[]);
    let report = prove_all(&prover, &boundary(), &[a, b], work.path());
    let bots: Vec<_> = report.results.iter().map(|r| r.bot.as_str()).collect();
    assert_eq!(bots, ["alpha", "zeta"]);
    assert!(report.passed());
    assert_eq!(report.prover.version.as_deref(), Some("0.1.9"));
    assert_eq!(report.prover.sha256.as_deref(), Some(stand.sha().as_str()));
    assert_eq!(
        report.boundary_sha256.as_deref(),
        Some(sha256_hex(&fs::read(boundary()).unwrap()).as_str())
    );
    let text = prove::render_text(&report);
    assert!(text.contains("2 of 2 bots proved; passed"));
    for n in prove::NOT_PROVED {
        assert!(text.contains(n));
    }
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["results"][0]["status"], "proved");
    assert_eq!(json["not_proved"].as_array().unwrap().len(), 6);
    // No candidates is never a pass.
    let empty = prove_all(&prover, &boundary(), &[], work.path());
    assert!(!empty.passed());
}

// The network view (PRV-4).

/// Every JSON path where `a` and `b` differ (a key in one and not the other, or a scalar).
fn diff(a: &Value, b: &Value, at: String, out: &mut Vec<String>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let keys: BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            for k in keys {
                match (x.get(k), y.get(k)) {
                    (Some(p), Some(q)) => diff(p, q, format!("{at}/{k}"), out),
                    _ => out.push(format!("{at}/{k}")),
                }
            }
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            for (i, (p, q)) in x.iter().zip(y).enumerate() {
                diff(p, q, format!("{at}/{i}"), out);
            }
        }
        _ if a == b => {}
        _ => out.push(at),
    }
}

/// A small deterministic generator, so the property cases are the same on every run.
struct Gen(u64);
impl Gen {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn pick<'a>(&mut self, items: &'a [&'a str]) -> &'a str {
        items[(self.next() % items.len() as u64) as usize]
    }
}

fn generated_policy(g: &mut Gen) -> Value {
    let mut policy: Value = serde_norway::from_str(&docs_policy()).unwrap();
    let rules = policy["network_policies"].as_object_mut().unwrap();
    for _ in 0..(g.next() % 4) {
        let protocol = g.pick(&["mcp", "rest", "none", "graphql"]);
        let mut endpoint = json!({
            "host": g.pick(&["toolserver.pmos.internal", "evil.example", "10.0.0.1", "*.internal"]),
            "port": 8000 + (g.next() % 1000),
            "allowed_ips": [g.pick(&["10.201.77.10/32", "10.0.0.0/8", "0.0.0.0/0"])],
            "path": "/mcp",
            "enforcement": g.pick(&["enforce", "audit"]),
            "rules": [{"allow": {"method": g.pick(&["tools/call", "POST", "GET"])}}],
            "mcp": {"versions": ["2025-06-18"]},
            "tls": "skip",
            "ports": [1, 2],
        });
        if protocol != "none" {
            endpoint["protocol"] = json!(protocol);
        }
        let name = format!("extra_{}", g.next() % 1000);
        rules.insert(
            name,
            json!({"binaries": [{"path": g.pick(&["/usr/local/bin/node", "/bin/sh"])}], "endpoints": [endpoint]}),
        );
    }
    // Sometimes the tool server's endpoint gains a second host, port or address.
    let ts = &mut policy["network_policies"]["pmos_toolserver"]["endpoints"][0];
    match g.next() % 4 {
        0 => ts["allowed_ips"] = json!(["10.201.77.10/32", "10.201.77.99/32"]),
        1 => ts["port"] = json!(9999),
        2 => ts["host"] = json!("evil.example"),
        _ => {}
    }
    policy
}

#[test]
fn the_view_only_ever_deletes_five_keys_of_mcp_endpoints() {
    let mut g = Gen(0x9e37_79b9_7f4a_7c15);
    for case in 0..300 {
        let policy = generated_policy(&mut g);
        let (view, changed) = network_view(&policy);
        let mut paths = Vec::new();
        diff(&policy, &view, String::new(), &mut paths);
        assert_eq!(changed, !paths.is_empty(), "case {case}");
        for p in &paths {
            // /network_policies/<rule>/endpoints/<i>/<key>
            let parts: Vec<&str> = p.split('/').collect();
            assert_eq!(parts.len(), 6, "case {case}: {p} isn't an endpoint key");
            assert_eq!(parts[1], "network_policies");
            assert_eq!(parts[3], "endpoints");
            assert!(
                VIEW_REMOVED_KEYS.contains(&parts[5]),
                "case {case}: the view changed {p}"
            );
            let endpoint = &policy["network_policies"][parts[2]]["endpoints"]
                [parts[4].parse::<usize>().unwrap()];
            assert_eq!(endpoint["protocol"], "mcp", "case {case}: {p} wasn't MCP");
            // Deleted, never changed: the view has no such key.
            let v_endpoint =
                &view["network_policies"][parts[2]]["endpoints"][parts[4].parse::<usize>().unwrap()];
            assert!(v_endpoint.get(parts[5]).is_none());
        }
        // The tool server's destination survives whatever it became.
        let ts = &policy["network_policies"]["pmos_toolserver"]["endpoints"][0];
        let vts = &view["network_policies"]["pmos_toolserver"]["endpoints"][0];
        for key in ["host", "port", "allowed_ips"] {
            assert_eq!(ts[key], vts[key], "case {case}: {key}");
        }
        assert_eq!(
            policy["network_policies"]["pmos_toolserver"]["binaries"],
            view["network_policies"]["pmos_toolserver"]["binaries"]
        );
    }
}

#[test]
fn the_view_of_a_non_mcp_policy_is_the_policy() {
    let text = docs_policy().replace("protocol: mcp", "protocol: rest");
    let value: Value = serde_norway::from_str(&text).unwrap();
    let (view, changed) = network_view(&value);
    assert!(!changed);
    assert_eq!(view, value);
}

// The boundary (PRV-2): hand written, but it must say what the code says.

#[test]
fn the_boundary_parses_as_a_policy_and_states_the_pinned_services() {
    let text = fs::read_to_string(boundary()).unwrap();
    assert!(
        !text.contains(pmos_policy::GENERATED_MARKER),
        "the boundary is never generated"
    );
    let b = openshell::parse(&text).expect("the boundary is a valid OpenShell policy");
    assert_eq!(b.version, 1);
    let fs = b.filesystem_policy.as_ref().unwrap();
    assert!(!fs.include_workdir);
    assert_eq!(fs.read_only, ["/"]);
    assert_eq!(fs.read_write, ["/dev/null", "/sandbox/workspace", "/tmp"]);
    assert_eq!(b.landlock.as_ref().unwrap().compatibility, "hard_requirement");
    let p = b.process.as_ref().unwrap();
    assert_eq!(p.run_as_user.as_deref(), Some(pmos_policy::RUN_AS));
    assert_eq!(p.run_as_group.as_deref(), Some(pmos_policy::RUN_AS));
    assert_eq!(b.network_policies.len(), 3);
    for s in Service::ALL {
        let rule = &b.network_policies[s.rule_name()];
        assert_eq!(rule.binaries.len(), 1);
        assert_eq!(rule.binaries[0].path, pmos_policy::WORKER_BINARY);
        assert_eq!(rule.endpoints.len(), 1);
        let e = &rule.endpoints[0];
        assert_eq!(e.host.as_deref(), Some(s.host()));
        assert_eq!(e.port, Some(s.port()));
        assert_eq!(e.allowed_ips, s.allowed_ips());
        // The tool server is plain L4 in the boundary (its view has no MCP rules).
        let value: Value = serde_json::to_value(e).unwrap();
        match s.routes() {
            None => {
                assert!(value.get("protocol").is_none(), "{value}");
                assert!(value.get("rules").is_none());
            }
            Some(routes) => {
                assert_eq!(value["protocol"], "rest");
                assert_eq!(value["enforcement"], "enforce");
                let mut got: Vec<(String, String)> = value["rules"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| {
                        (
                            r["allow"]["method"].as_str().unwrap().to_string(),
                            r["allow"]["path"].as_str().unwrap().to_string(),
                        )
                    })
                    .collect();
                got.sort();
                let mut want: Vec<(String, String)> = routes
                    .iter()
                    .map(|(m, p)| (m.to_string(), p.to_string()))
                    .collect();
                want.sort();
                assert_eq!(got, want, "{}", s.rule_name());
            }
        }
        // No credential binding: not in the prover's model.
        assert!(value.get("credential_binding").is_none());
    }
}

#[test]
fn every_checked_in_policys_view_keeps_only_boundary_shaped_endpoints() {
    // Without the real prover this is the closest check here: each committed policy's view has
    // exactly the boundary's three destinations, with no MCP keys left.
    for (bot, path) in prove::candidates(&repo().join("policies")).unwrap() {
        let (view, _) = network_view_text(&fs::read_to_string(&path).unwrap()).unwrap();
        let v: Value = serde_norway::from_str(&view).unwrap();
        for s in Service::ALL {
            let e = &v["network_policies"][s.rule_name()]["endpoints"][0];
            assert_eq!(e["host"], s.host(), "{bot}");
            assert_eq!(e["port"], s.port(), "{bot}");
            if s.routes().is_none() {
                for k in VIEW_REMOVED_KEYS {
                    assert!(e.get(k).is_none(), "{bot}: {k} left in the view");
                }
            }
        }
    }
}
