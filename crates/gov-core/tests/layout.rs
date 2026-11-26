//! The dependency rule (spec §9): no Agent Governance crate depends on a PM OS crate, so
//! Governance installs without PM OS. Later slices add the two named exceptions here, by name.

use std::fs;
use std::path::Path;

#[test]
fn no_governance_crate_depends_on_pm_os() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut seen = 0;
    for entry in fs::read_dir(crates).unwrap() {
        let dir = entry.unwrap().path();
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        if !name.starts_with("gov-") {
            continue;
        }
        seen += 1;
        let manifest = fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        for line in manifest.lines() {
            let line = line.trim();
            assert!(
                !line.starts_with("pmos-") && !line.contains("path = \"../"),
                "{name} depends on another crate by path or on PM OS: {line}"
            );
        }
    }
    assert!(seen >= 1);
}
