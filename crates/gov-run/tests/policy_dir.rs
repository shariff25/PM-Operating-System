//! Slice G4b, step 2: a policy directory is checked, compared with the bundle before, and
//! signed with the root key (spec §7.6.1, review finding GR-128).

use std::path::Path;

use gov_core::action::{Kind, Reversal};
use gov_core::connector::{tool_digest, ResourceTemplate, ServerMap, ToolMap};
use gov_core::policy::{open_bundle, KeyRole};
use gov_core::{Id, Scope, SigningKey};
use gov_run::policy_dir::{check, describe, prepare, sign};
use serde_json::{json, Value};

const NOW: i64 = 1_800_000_000;

fn id(s: &str) -> Id {
    Id::new(s).unwrap()
}
fn key(kid: &str, seed: u8) -> SigningKey {
    SigningKey::from_seed(id(kid), &[seed; 32]).unwrap()
}
fn root() -> SigningKey {
    key("root-1", 70)
}
fn scope() -> Scope {
    Scope::new("acme", "prod").unwrap()
}

fn policy() -> Value {
    json!({
        "tenant": "acme",
        "env": "prod",
        "issuer": "authority",
        "version": "2026-11-26",
        "keys": {
            "mandates": [key("authority:mandates-1", 71).public()],
            "leases": [key("authority:leases-1", 72).public()],
        },
        "rules": [
            {"id": "read", "match": {"system": "jira", "kinds": ["read"]}, "then": {"effect": "allow"}},
            {"id": "no-pay", "match": {"resources": ["jira/project/pay/*"]}, "then": {"effect": "refuse"}},
        ],
    })
}

fn jira_map() -> ServerMap {
    let tool = |name: &str, operation: &str, kind, reversal| ToolMap {
        results: None,
        tool: id(name),
        digest: tool_digest(&json!({"name": name, "inputSchema": {"type": "object"}})).unwrap(),
        operation: id(operation),
        kind,
        reversal,
        outward: false,
        resource: ResourceTemplate::new("jira/project/{key:before(-)}/issue/{key}").unwrap(),
    };
    ServerMap {
        labels: Vec::new(),
        clearances: Vec::new(),
        server: id("jira-mcp"),
        system: id("jira"),
        fold_case: true,
        tools: vec![
            tool("jira_get_issue", "issue.read", Kind::Read, Reversal::None),
            tool(
                "jira_update_issue",
                "issue.update",
                Kind::Write,
                Reversal::Restore,
            ),
        ],
    }
}

fn samples() -> Value {
    json!([
        {"tool": "jira_get_issue", "arguments": {"key": "PAY-12"},
         "expect": {"operation": "issue.read", "kind": "read", "resource": "jira/project/pay/issue/pay-12"}},
        {"tool": "jira_update_issue", "arguments": {"key": "OPS-7", "summary": "x"},
         "expect": {"operation": "issue.update", "kind": "write", "resource": "jira/project/ops/issue/ops-7"}},
    ])
}

fn write(dir: &Path, name: &str, value: &Value) {
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

/// A policy directory that passes.
fn good() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "policy.json", &policy());
    write(
        dir.path(),
        "connectors/jira-mcp.json",
        &serde_json::to_value(jira_map()).unwrap(),
    );
    write(dir.path(), "connectors/jira-mcp.samples.json", &samples());
    dir
}

fn fails(dir: &Path, why: &str) {
    let err = check(dir, 1, NOW).expect_err(why);
    let text = format!("{err:#}");
    assert!(text.contains(why), "{text}");
}

#[test]
fn a_policy_directory_is_checked_as_a_gate_would_check_it() {
    let dir = good();
    let checked = check(dir.path(), 1, NOW).unwrap();
    assert_eq!(checked.samples, 2);
    assert_eq!(checked.bundle.rules.len(), 2);
    assert_eq!(checked.bundle.connectors, vec![jira_map()]);
    assert_eq!((checked.bundle.number, checked.bundle.issued_at), (1, NOW));

    // No connector map at all is a policy with nothing mapped, and passes.
    let bare = tempfile::tempdir().unwrap();
    write(bare.path(), "policy.json", &policy());
    assert_eq!(check(bare.path(), 1, NOW).unwrap().samples, 0);

    // The bundle's own rules are checked: a permitting rule that names no system.
    let dir = good();
    let mut p = policy();
    p["rules"][0]["match"] = json!({"kinds": ["read"]});
    write(dir.path(), "policy.json", &p);
    fails(dir.path(), "does not pass its checks");
    // An unknown field, and a key written twice, are not read past.
    let mut p = policy();
    p["overrides"] = json!([]);
    write(dir.path(), "policy.json", &p);
    fails(dir.path(), "not in the form expected");
    let twice = serde_json::to_string(&policy()).unwrap().replacen(
        "\"tenant\":\"acme\"",
        "\"tenant\":\"acme\",\"tenant\":\"acme\"",
        1,
    );
    assert!(twice.matches("\"tenant\"").count() == 2);
    std::fs::write(dir.path().join("policy.json"), twice).unwrap();
    fails(dir.path(), "not strict JSON");
}

#[test]
fn every_connector_map_comes_with_sample_calls_that_read_as_they_say() {
    // A map with no samples beside it.
    let dir = good();
    std::fs::remove_file(dir.path().join("connectors/jira-mcp.samples.json")).unwrap();
    fails(dir.path(), "has no jira-mcp.samples.json beside it");
    // Samples with no map, and a file that is neither.
    let dir = good();
    write(dir.path(), "connectors/other.samples.json", &json!([]));
    fails(dir.path(), "has no connector map beside it");
    let dir = good();
    std::fs::write(dir.path().join("connectors/notes.txt"), "x").unwrap();
    fails(dir.path(), "neither a map nor its samples");
    // A map filed under another server's name.
    let dir = good();
    write(
        dir.path(),
        "connectors/other.json",
        &serde_json::to_value(jira_map()).unwrap(),
    );
    write(dir.path(), "connectors/other.samples.json", &samples());
    fails(dir.path(), "holds the map of another server");

    let with = |change: &dyn Fn(&mut Value)| {
        let dir = good();
        let mut s = samples();
        change(&mut s);
        write(dir.path(), "connectors/jira-mcp.samples.json", &s);
        dir
    };
    // A tool of the map with no sample call.
    let dir = with(&|s| {
        s.as_array_mut().unwrap().pop();
    });
    fails(dir.path(), "has no sample call");
    // A sample for a tool the map does not have.
    let dir = with(&|s| s[0]["tool"] = json!("jira_delete_issue"));
    fails(dir.path(), "no tool of that name");
    // A sample the map reads otherwise: the operation, the kind, the resource.
    for (field, wrong) in [
        ("operation", "issue.update"),
        ("kind", "write"),
        ("resource", "jira/project/PAY/issue/PAY-12"),
    ] {
        let dir = with(&|s| s[0]["expect"][field] = json!(wrong));
        fails(dir.path(), "not what the sample expects");
    }
    // A sample whose arguments the resource cannot be built from.
    let dir = with(&|s| s[0]["arguments"] = json!({"id": 12}));
    fails(dir.path(), "cannot be read as an action");
}

#[test]
fn the_signer_checks_the_directory_itself_and_sees_what_changed() {
    let dir = good();
    let root_public = root().public();
    // The first bundle: number 1, none before it, everything new.
    let first = prepare(dir.path(), &root_public, 1, None, NOW).unwrap();
    assert_eq!(first.changes.rules_added, vec![id("read"), id("no-pay")]);
    assert_eq!(first.changes.maps_added, vec![id("jira-mcp")]);
    let (signed_1, opened_1) = sign(&first, &root()).unwrap();
    assert_eq!(
        open_bundle(&signed_1, &root_public, &scope()).unwrap(),
        opened_1
    );
    assert_eq!(opened_1.bundle.number, 1);

    // Number 1 takes no bundle before it; any other number needs one, and the next number.
    assert!(prepare(dir.path(), &root_public, 1, Some(&signed_1), NOW).is_err());
    assert!(prepare(dir.path(), &root_public, 2, None, NOW).is_err());
    assert!(prepare(dir.path(), &root_public, 3, Some(&signed_1), NOW).is_err());
    // The bundle before must open under this root: one signed by another key is no bundle.
    let other_root = key("root-1", 69);
    let (forged, _) = sign(&first, &other_root).unwrap();
    assert!(prepare(dir.path(), &root_public, 2, Some(&forged), NOW).is_err());

    // The same directory again: nothing to show but the number.
    let again = prepare(dir.path(), &root_public, 2, Some(&signed_1), NOW).unwrap();
    assert!(again.changes.is_empty());
    assert_eq!(
        describe(&again.changes),
        vec!["nothing changed but the number"]
    );
    // A difference the list does not name is said in capitals, not left out (GR-135).
    let unnamed = gov_core::policy::Changes {
        other: true,
        ..Default::default()
    };
    assert!(describe(&unnamed)[0].starts_with("OTHER CHANGES"));

    // What review finding GR-128 is about: a key of an attacker's added under `mandates`,
    // and a refusal dropped. The signer is shown both, by id.
    let mut p = policy();
    p["keys"]["mandates"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::to_value(key("authority:mandates-2", 66).public()).unwrap());
    p["rules"].as_array_mut().unwrap().pop();
    write(dir.path(), "policy.json", &p);
    let next = prepare(dir.path(), &root_public, 2, Some(&signed_1), NOW).unwrap();
    assert_eq!(
        next.changes.keys_added,
        vec![(KeyRole::Mandates, id("authority:mandates-2"))]
    );
    assert_eq!(next.changes.rules_removed, vec![id("no-pay")]);
    assert_eq!(
        describe(&next.changes),
        vec![
            "key added, for mandates: authority:mandates-2",
            "rule removed: no-pay"
        ]
    );
    let (signed_2, opened_2) = sign(&next, &root()).unwrap();
    assert_eq!(opened_2.bundle.number, 2);
    assert_ne!(opened_2.digest, opened_1.digest);
    assert_ne!(opened_2.policy_version(), opened_1.policy_version());
    assert!(prepare(dir.path(), &root_public, 3, Some(&signed_2), NOW).is_ok());

    // A directory that does not pass its checks is not prepared, so it is never signed:
    // the signer runs the check, and takes no built bundle.
    std::fs::remove_file(dir.path().join("connectors/jira-mcp.samples.json")).unwrap();
    assert!(prepare(dir.path(), &root_public, 3, Some(&signed_2), NOW).is_err());

    // A policy that lists the root key among the online keys is refused before signing.
    let dir = good();
    let mut p = policy();
    p["keys"]["leases"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::to_value(key("authority:leases-2", 70).public()).unwrap());
    write(dir.path(), "policy.json", &p);
    assert!(check(dir.path(), 1, NOW).is_ok());
    assert!(prepare(dir.path(), &root_public, 1, None, NOW).is_err());
}
