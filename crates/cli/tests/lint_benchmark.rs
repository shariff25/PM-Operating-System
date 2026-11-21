//! `pmos lint` refuses a marketplace bot offered before its benchmark passes (D10), unless it
//! is a Preview (the owner's exception of 2026-11-21; `docs/bot-marketplace-spec.md` §13,
//! "M1 is built"). The rule reads the evals beside the content root, or `--evals`.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_pmos");

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().filter_map(Result::ok) {
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if src.is_dir() {
            copy_tree(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

fn lint(content: &Path, evals: Option<&Path>) -> std::process::Output {
    let mut cmd = Command::new(BIN);
    cmd.args(["lint", "--root"]).arg(content);
    if let Some(evals) = evals {
        cmd.arg("--evals").arg(evals);
    }
    cmd.output().expect("run pmos")
}

#[test]
fn an_offered_bot_is_a_preview_until_its_benchmark_passes() {
    let dir = tempfile::tempdir().unwrap();
    let content = dir.path().join("content");
    copy_tree(&repo().join("content"), &content);
    copy_tree(&repo().join("evals"), &dir.path().join("evals"));

    // As in the repository: Chief, Sprout and Echo are Previews, so it passes.
    let out = lint(&content, None);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    // Chief offered in full with no passing report: an error naming the rule and the fix.
    let path = content.join("personas/chief/PERSONA.md");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("preview: true\n"));
    std::fs::write(&path, text.replace("preview: true\n", "")).unwrap();
    let out = lint(&content, None);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "{stdout}");
    assert!(
        stdout.contains("error[persona-benchmark] personas/chief/PERSONA.md"),
        "{stdout}"
    );
    assert!(stdout.contains("preview: true"), "{stdout}");

    // No evals directory at all: nothing is benchmarked, so it still fails (fail closed).
    let out = lint(&content, Some(&dir.path().join("no-evals")));
    assert!(!out.status.success());
}
