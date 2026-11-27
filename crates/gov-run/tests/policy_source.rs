//! Slice G4b, step 4: where the gate and the shim get their policy (spec §7.6.1, points 3
//! and 5). The signed bundle is the way; the unsigned files work only when asked for by
//! name, for fake data.

use std::path::Path;

use gov_core::action::{Kind, Reversal};
use gov_core::connector::{tool_digest, ResourceTemplate, ServerMap, ToolMap};
use gov_core::policy::{HeldBundle, OnlineKeys, PolicyBundle};
use gov_core::{seal, Id, Scope, SigningKey};
use gov_run::policy_source::{Policy, PolicyArgs, UNSIGNED};
use serde_json::json;

fn id(s: &str) -> Id {
    Id::new(s).unwrap()
}
fn key(kid: &str, seed: u8) -> SigningKey {
    SigningKey::from_seed(id(kid), &[seed; 32]).unwrap()
}
fn scope() -> Scope {
    Scope::new("acme", "prod").unwrap()
}
fn map(server: &str) -> ServerMap {
    ServerMap {
        server: id(server),
        system: id("jira"),
        fold_case: true,
        tools: vec![ToolMap {
            tool: id("jira_get_issue"),
            digest: tool_digest(&json!({"name": "jira_get_issue"})).unwrap(),
            operation: id("issue.read"),
            kind: Kind::Read,
            reversal: Reversal::None,
            outward: false,
            resource: ResourceTemplate::new("jira/project/{key:before(-)}/issue/{key}").unwrap(),
        }],
    }
}

/// Write a bundle with this number, signed by `signer`, and return its file's name.
fn bundle(dir: &Path, name: &str, number: u64, label: &str, signer: &SigningKey) -> String {
    let bundle = PolicyBundle {
        v: 1,
        tenant: id("acme"),
        env: id("prod"),
        issuer: id("authority"),
        version: id(label),
        number,
        issued_at: 1_800_000_000,
        keys: OnlineKeys {
            mandates: vec![key("authority:mandates-1", 71).public()],
            leases: vec![key("authority:leases-1", 72).public()],
        },
        freshness: Default::default(),
        rules: Vec::new(),
        connectors: vec![map("jira-mcp"), map("other-mcp")],
        templates: Vec::new(),
    };
    std::fs::write(
        dir.join(name),
        serde_json::to_vec(&seal(&bundle, signer).unwrap()).unwrap(),
    )
    .unwrap();
    name.to_string()
}

fn args(dir: &Path, bundle: &str) -> PolicyArgs {
    PolicyArgs {
        bundle: Some(dir.join(bundle)),
        root_key: Some(dir.join("root.pub.json")),
        bundle_state: Some(dir.join("state.json")),
        server: Some("jira-mcp".into()),
        unsigned_policy_for_tests: false,
        map: None,
        mandate_issuers: None,
    }
}

fn refused(r: anyhow::Result<Policy>, says: &str) {
    let text = format!("{:#}", r.expect_err(says));
    assert!(text.contains(says), "{text}");
}

// One test: it sets and unsets a variable of the process's environment.
#[test]
fn policy_comes_from_a_bundle_signed_by_the_root_and_does_not_go_back() {
    let site = tempfile::tempdir().unwrap();
    let dir = site.path();
    let root = key("root-1", 70);
    std::fs::write(
        dir.join("root.pub.json"),
        serde_json::to_vec(&root.public()).unwrap(),
    )
    .unwrap();
    let state = || -> Option<HeldBundle> {
        std::fs::read(dir.join("state.json"))
            .ok()
            .map(|b| serde_json::from_slice(&b).unwrap())
    };
    std::env::remove_var("GOV_DATA_CLASS");

    // Not signed by the root: by an online key the bundle names, or by another root. Nothing
    // is noted of a bundle that did not open.
    let online = bundle(
        dir,
        "online.json",
        3,
        "v3",
        &key("authority:mandates-1", 71),
    );
    refused(
        args(dir, &online).load(&scope()),
        "does not open under the root key",
    );
    let other = bundle(dir, "other.json", 3, "v3", &key("root-1", 69));
    refused(
        args(dir, &other).load(&scope()),
        "does not open under the root key",
    );
    assert!(state().is_none());
    // Another tenant's bundle.
    let b3 = bundle(dir, "b3.json", 3, "v3", &root);
    let elsewhere = Scope::new("other", "prod").unwrap();
    refused(
        args(dir, &b3).load(&elsewhere),
        "does not open under the root key",
    );
    assert!(state().is_none());

    // The bundle opens: the map is the named server's, the issuers' keys are the bundle's
    // `mandates` keys and not its `leases` keys, and what was accepted is noted.
    let policy = args(dir, &b3).load(&scope()).unwrap();
    assert_eq!(policy.map, map("jira-mcp"));
    assert_eq!(
        policy.mandate_issuers,
        vec![key("authority:mandates-1", 71).public()]
    );
    let held = state().unwrap();
    assert_eq!(held.number, 3);
    assert_eq!(
        policy.version.as_str(),
        format!("3-{}", &held.digest.as_str()[..16])
    );
    // The same bundle again is fine, as at every restart.
    args(dir, &b3).load(&scope()).unwrap();
    // A server the bundle has no map for.
    let mut unknown = args(dir, &b3);
    unknown.server = Some("github-mcp".into());
    refused(unknown.load(&scope()), "no connector map for the server");

    // It does not go back: an older bundle, and another under the same number, are refused,
    // and what is noted stays.
    let b2 = bundle(dir, "b2.json", 2, "v2", &root);
    refused(args(dir, &b2).load(&scope()), "older than number 3");
    let b3x = bundle(dir, "b3x.json", 3, "v3-other", &root);
    refused(args(dir, &b3x).load(&scope()), "ALARM");
    assert_eq!(state().unwrap(), held);
    // A later one is taken, whatever the gap, and noted.
    let b7 = bundle(dir, "b7.json", 7, "v7", &root);
    let policy = args(dir, &b7).load(&scope()).unwrap();
    assert!(policy.version.as_str().starts_with("7-"));
    assert_eq!(state().unwrap().number, 7);
    refused(args(dir, &b3).load(&scope()), "older than number 7");
    // A state file that cannot be read is not taken for no state.
    std::fs::write(dir.join("state.json"), "{").unwrap();
    refused(args(dir, &b7).load(&scope()), "the bundle state");
    std::fs::remove_file(dir.join("state.json")).unwrap();

    // Each of the four options is needed.
    for missing in 0..4 {
        let mut a = args(dir, &b7);
        match missing {
            0 => a.bundle = None,
            1 => a.root_key = None,
            2 => a.bundle_state = None,
            _ => a.server = None,
        }
        refused(a.load(&scope()), "are all needed");
    }

    // The unsigned files. Named without the flag they are refused, even beside a good
    // bundle: nothing unsigned rides along.
    std::fs::write(
        dir.join("map.json"),
        serde_json::to_vec(&map("jira-mcp")).unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.join("issuers.json"),
        serde_json::to_vec(&[key("issuer-1", 41).public()]).unwrap(),
    )
    .unwrap();
    let mut with_map = args(dir, &b7);
    with_map.map = Some(dir.join("map.json"));
    refused(with_map.load(&scope()), "unsigned files");
    let mut with_issuers = args(dir, &b7);
    with_issuers.mandate_issuers = Some(dir.join("issuers.json"));
    refused(with_issuers.load(&scope()), "unsigned files");
    // With the flag they are for fake data only.
    let unsigned = PolicyArgs {
        bundle: None,
        root_key: None,
        bundle_state: None,
        server: None,
        unsigned_policy_for_tests: true,
        map: Some(dir.join("map.json")),
        mandate_issuers: Some(dir.join("issuers.json")),
    };
    refused(unsigned.load(&scope()), "fake data only");
    std::env::set_var("GOV_DATA_CLASS", "real");
    refused(unsigned.load(&scope()), "fake data only");
    std::env::set_var("GOV_DATA_CLASS", "fake");
    // And what is recorded then says so.
    let policy = unsigned.load(&scope()).unwrap();
    assert_eq!(policy.version.as_str(), UNSIGNED);
    assert_eq!(policy.mandate_issuers, vec![key("issuer-1", 41).public()]);
    // The flag and a bundle together: one or the other.
    let mut both = unsigned.clone();
    both.bundle = Some(dir.join(&b7));
    refused(both.load(&scope()), "one or the other");
    let mut no_map = unsigned.clone();
    no_map.map = None;
    refused(no_map.load(&scope()), "needs --map");
    std::env::remove_var("GOV_DATA_CLASS");
}
