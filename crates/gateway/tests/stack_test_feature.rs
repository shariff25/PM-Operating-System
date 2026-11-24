//! Slice H12 (spec §3.6, "How H12's stack test runs `hosted`"): the test-only list is compiled
//! in only by `deploy/test/Dockerfile.gateway-stack-test`. CI's check that no release build can
//! enable the `stack-test-list` feature: the release Dockerfile's `cargo` lines name no
//! features, and no workspace crate enables the feature in a normal (not dev) dependency.

use std::path::Path;

fn root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn the_release_dockerfiles_never_enable_a_feature() {
    for file in ["deploy/Dockerfile", "deploy/Dockerfile.worker"] {
        let text = std::fs::read_to_string(root().join(file)).unwrap();
        for line in text.lines().filter(|l| l.contains("cargo")) {
            for bad in ["--features", "--all-features", "-F ", "stack-test-list"] {
                assert!(!line.contains(bad), "{file}: `{line}` names `{bad}`");
            }
        }
        assert!(
            !text.contains("stack-test-list"),
            "{file} names the test-only feature"
        );
    }
    // The test Dockerfile is the one place it is named.
    let test =
        std::fs::read_to_string(root().join("deploy/test/Dockerfile.gateway-stack-test")).unwrap();
    assert!(test.contains("--features stack-test-list"));
}

#[test]
fn no_crate_enables_the_feature_in_a_normal_dependency() {
    let crates = std::fs::read_dir(root().join("crates")).unwrap();
    for entry in crates {
        let manifest = entry.unwrap().path().join("Cargo.toml");
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        // Everything before `[dev-dependencies]` (or the whole file without one), less the
        // gateway's own `[features]` table, which declares it.
        let mut section = String::new();
        for line in text.lines() {
            let t = line.trim();
            if t.starts_with('[') {
                section = t.to_string();
            }
            if section.starts_with("[dev-dependencies") || section == "[features]" {
                continue;
            }
            assert!(
                !line.contains("stack-test-list"),
                "{}: `{line}` enables the test-only feature",
                manifest.display()
            );
        }
    }
    let workspace = std::fs::read_to_string(root().join("Cargo.toml")).unwrap();
    assert!(!workspace.contains("stack-test-list"));
}
