//! `gov policy check` and `gov policy sign`, run as the release step runs them (spec §7.6.1).

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use gov_core::policy::open_bundle;
use gov_core::{Envelope, Id, Scope, SigningKey};
use serde_json::json;

fn id(s: &str) -> Id {
    Id::new(s).unwrap()
}
fn public(kid: &str, seed: u8) -> gov_core::PublicKey {
    SigningKey::from_seed(id(kid), &[seed; 32])
        .unwrap()
        .public()
}

/// Run `gov policy ...`, typing `typed` at it. Returns the exit code and what it printed.
fn gov(args: &[&str], class: Option<&str>, typed: &str) -> (i32, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_gov"));
    command
        .arg("policy")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("GOV_DATA_CLASS");
    if let Some(class) = class {
        command.env("GOV_DATA_CLASS", class);
    }
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(typed.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

fn write_policy(dir: &Path, rules: serde_json::Value) {
    let policy = json!({
        "tenant": "acme", "env": "prod", "issuer": "authority", "version": "v1",
        "keys": {
            "mandates": [public("authority:mandates-1", 71)],
            "leases": [public("authority:leases-1", 72)],
        },
        "rules": rules,
    });
    std::fs::write(
        dir.join("policy.json"),
        serde_json::to_vec(&policy).unwrap(),
    )
    .unwrap();
}

#[test]
fn policy_is_checked_without_a_key_and_signed_with_the_root_key() {
    let site = tempfile::tempdir().unwrap();
    let at = |name: &str| site.path().join(name).to_str().unwrap().to_string();
    let dir = site.path().join("policy");
    std::fs::create_dir(&dir).unwrap();
    let read = json!({"id": "read", "match": {"system": "jira", "kinds": ["read"]}, "then": {"effect": "allow"}});
    let no_pay = json!({"id": "no-pay", "match": {"resources": ["jira/project/pay/*"]}, "then": {"effect": "refuse"}});
    write_policy(&dir, json!([read, no_pay]));
    let dir = at("policy");

    // Checking needs no key and no word about the data.
    let (code, said) = gov(&["check", "--dir", &dir], None, "");
    assert_eq!(code, 0, "{said}");
    assert!(said.contains("passes its checks: 2 rules"), "{said}");

    // The root key is a file, so signing is for fake data only, like every key in a file.
    std::env::set_var("GOV_DATA_CLASS", "fake");
    let root = gov_run::keygen("root-1", &site.path().join("root.key")).unwrap();
    let sign = |number: &str, out: &str, extra: &[&str], class: Option<&str>, typed: &str| {
        let mut args = vec![
            "sign",
            "--dir",
            &dir,
            "--root-key",
            "",
            "--number",
            number,
            "--out",
            out,
        ];
        let key = at("root.key");
        args[4] = &key;
        args.extend_from_slice(extra);
        gov(&args, class, typed)
    };
    let (code, said) = sign("1", &at("b1.json"), &["--yes"], None, "");
    assert_ne!(code, 0);
    assert!(said.contains("fake data only"), "{said}");
    assert!(!site.path().join("b1.json").exists());

    // The signer is shown what is new and asked; anything but yes signs nothing.
    let (code, said) = sign("1", &at("b1.json"), &[], Some("fake"), "y\n");
    assert_ne!(code, 0);
    assert!(
        said.contains("rule added: no-pay") && said.contains("Not signed."),
        "{said}"
    );
    assert!(
        said.contains("key added, for mandates: authority:mandates-1"),
        "{said}"
    );
    assert!(!site.path().join("b1.json").exists());
    let (code, said) = sign("1", &at("b1.json"), &[], Some("fake"), "yes\n");
    assert_eq!(code, 0, "{said}");
    let open = |name: &str| {
        let envelope: Envelope =
            serde_json::from_slice(&std::fs::read(site.path().join(name)).unwrap()).unwrap();
        open_bundle(&envelope, &root, &Scope::new("acme", "prod").unwrap()).unwrap()
    };
    let first = open("b1.json");
    assert_eq!(first.bundle.number, 1);
    // The digest it signed is printed, for the publisher to compare.
    assert!(said.contains(first.digest.as_str()), "{said}");

    // A signed bundle is never written over.
    let before = std::fs::read(site.path().join("b1.json")).unwrap();
    let (code, said) = sign("1", &at("b1.json"), &["--yes"], Some("fake"), "");
    assert_ne!(code, 0);
    assert!(said.contains("exists already"), "{said}");
    assert_eq!(std::fs::read(site.path().join("b1.json")).unwrap(), before);

    // The next bundle needs the one before, and shows what changed since: here a refusal
    // that was dropped.
    write_policy(Path::new(&dir), json!([read]));
    let (code, _) = sign("2", &at("b2.json"), &["--yes"], Some("fake"), "");
    assert_ne!(code, 0);
    let previous = at("b1.json");
    let (code, said) = sign(
        "2",
        &at("b2.json"),
        &["--yes", "--previous", &previous],
        Some("fake"),
        "",
    );
    assert_eq!(code, 0, "{said}");
    assert!(said.contains("rule removed: no-pay"), "{said}");
    assert_eq!(open("b2.json").bundle.number, 2);

    // Nothing the command printed holds the key file's contents.
    let key_text = std::fs::read_to_string(site.path().join("root.key")).unwrap();
    let seed = key_text.split('"').rev().nth(1).unwrap();
    assert!(seed.len() > 20 && !said.contains(seed));
}
