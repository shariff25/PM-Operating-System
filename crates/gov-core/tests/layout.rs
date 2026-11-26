//! The dependency rule (spec §9): no Agent Governance crate depends on a PM OS crate, so
//! Governance installs without PM OS. Later slices add the two named exceptions here, by name.
//!
//! Read from `cargo metadata`, which gives each dependency's real package name: a dependency
//! renamed in a manifest (`x = { package = "pmos-..." }`) is still seen.

use std::process::Command;

use serde_json::Value;

#[test]
fn no_governance_crate_depends_on_pm_os() {
    let out = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--offline",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: Value = serde_json::from_slice(&out.stdout).unwrap();
    let packages = meta["packages"].as_array().unwrap();
    let workspace: Vec<&str> = packages
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    let mut seen = 0;
    for package in packages {
        let name = package["name"].as_str().unwrap();
        if !name.starts_with("gov-") {
            continue;
        }
        seen += 1;
        for dep in package["dependencies"].as_array().unwrap() {
            let dep = dep["name"].as_str().unwrap();
            let inside = workspace.contains(&dep);
            assert!(
                !inside || dep.starts_with("gov-"),
                "{name} depends on {dep}, a crate of this workspace outside Governance"
            );
        }
    }
    assert!(seen >= 3, "found only {seen} Governance crates");
}
