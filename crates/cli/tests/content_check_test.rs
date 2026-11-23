//! `pmos content check-test` (slice H4): fake-data mode's content check, which `pmos-admin
//! sandbox` runs before it uploads anything under `PMOS_MODEL_SOURCE=hosted-test`.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_pmos");

fn run(root: &std::path::Path) -> std::process::Output {
    Command::new(BIN)
        .args(["content", "check-test", "--root"])
        .arg(root)
        .output()
        .expect("run pmos")
}

#[test]
fn passes_test_content_and_refuses_anything_else_with_one_line_per_cause() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("context")).unwrap();
    std::fs::write(
        root.join("context/a.md"),
        "---\nsensitivity: internal\n---\na",
    )
    .unwrap();
    let out = run(root);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no TEST-CONTENT marker"), "{err}");
    assert!(err.contains("PMOS_MODEL_SOURCE=hosted-test"), "{err}");

    std::fs::write(root.join("TEST-CONTENT"), "test data only\n").unwrap();
    assert!(run(root).status.success());

    std::fs::write(
        root.join("context/b.md"),
        "---\nsensitivity: restricted\n---\nb",
    )
    .unwrap();
    let out = run(root);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(err.contains("context/b.md is marked restricted"), "{err}");

    // The repository's own seed library is test content.
    let seed = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
    assert!(run(&seed).status.success());
}
