//! Slice U7's proof, the source half (`docs/user-database-spec.md` §12, "Proving no service
//! decides from the file", part 3): no crate but the tool server's import/seed module names
//! the team file's model, and no code anywhere names `team.yaml` except the few reviewed lines
//! below, none of which reads it. The stacks and CI no longer write one or run
//! `pmos member add`. The behaviour half is `tests/stack.rs`
//! (`a_hostile_team_yaml_decides_nothing_in_any_service`).
//!
//! What counts as code: every `.rs` file under `crates/*/src` and each crate's `build.rs`,
//! except a `#[cfg(test)] mod …` block (unit tests may name the file to plant one), with
//! comments removed. Comments may say the file is gone. In the scripts, deploy kit, CI workflows and
//! harness, `#` comments are removed the same way.
//!
//! This replaces nothing: the per-service scans (`crates/bots/tests/directory.rs`,
//! `crates/slack/tests/no_team_file.rs`, `crates/console/tests/console.rs`) also ban each
//! service's own old lookups (`load_team`, `by_handle(`, the console's content root), which
//! this one doesn't know about.

use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// The one module that parses the old file (the import and the dev seed).
const IMPORT_MODULE: &str = "crates/toolserver/src/users/import.rs";

/// Code lines outside the import module that may name `team.yaml`, each with why. None reads
/// the file. Every entry must still match, so a stale one fails too.
const REVIEWED: [(&str, &str, &str); 7] = [
    (
        "crates/contract/src/lint.rs",
        "const RETIRED_TEAM_FILE: &str = \"team.yaml\";",
        "the team-file-retired lint finds the file to refuse it; it never opens it",
    ),
    (
        "crates/contract/src/lint.rs",
        "\"team.yaml is retired: users and roles are in the PM OS user database, and \\",
        "the lint's message",
    ),
    (
        "crates/toolserver/src/users/detail.rs",
        "\"team.yaml sha256 {sha256} commit {}\",",
        "the import's own role_changes detail (spec §3), written by the import",
    ),
    (
        "crates/toolserver/src/app/admin.rs",
        "\"decision by someone not in team.yaml\",",
        "a fixed audit note of events written before slice U3, still served as metadata",
    ),
    (
        "crates/policy/src/admin/access.rs",
        "[\"team.yaml\"] => \"team\",",
        "names the kind of a path, so an old file shows as what it is; nothing is read",
    ),
    (
        "crates/policy/src/admin/proposals.rs",
        "\"team.yaml\",",
        "a content proposal may never write the file",
    ),
    (
        "crates/policy/src/admin/proposals.rs",
        "\"team.yaml is retired: users and roles are in the PM OS user database, not content\",",
        "the refusal's reason",
    ),
];

/// `line` without its comment: a whole-line `//` comment, or one after ` //`.
fn rust_code(line: &str) -> &str {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") {
        return "";
    }
    line.split(" //").next().unwrap_or_default()
}

/// The Rust files under `crates/*/src`, and each crate's `build.rs`.
fn rust_sources(repo: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for krate in std::fs::read_dir(repo.join("crates")).unwrap() {
        let krate = krate.unwrap().path();
        let build = krate.join("build.rs");
        if build.is_file() {
            files.push(build);
        }
        for entry in walkdir::WalkDir::new(krate.join("src")) {
            let entry = entry.unwrap();
            if entry.path().extension().and_then(|e| e.to_str()) == Some("rs") {
                files.push(entry.into_path());
            }
        }
    }
    files.sort();
    files
}

fn rel(repo: &Path, path: &Path) -> String {
    path.strip_prefix(repo)
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/")
}

#[test]
fn no_crate_but_the_import_names_the_team_file() {
    let repo = repo();
    let files = rust_sources(&repo);
    assert!(
        files.len() > 200,
        "read the workspace ({} files)",
        files.len()
    );
    assert!(files.iter().any(|f| rel(&repo, f) == IMPORT_MODULE));
    let mut used = [false; REVIEWED.len()];
    let mut found = Vec::new();
    for path in &files {
        let rel = rel(&repo, path);
        let text = std::fs::read_to_string(path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        // Inside a unit-test module: (its opening line, the brace depth reached so far).
        let mut in_tests: Option<(usize, i64)> = None;
        for (n, line) in lines.iter().enumerate() {
            // A unit-test module: tests may plant a file by name. Skipped to its closing
            // brace (counted on code, comments removed), so code after it is still read.
            if in_tests.is_none()
                && line.trim() == "#[cfg(test)]"
                && lines
                    .get(n + 1)
                    .is_some_and(|next| next.trim_start().starts_with("mod "))
            {
                in_tests = Some((n + 1, 0));
                continue;
            }
            if let Some((start, depth)) = in_tests.as_mut() {
                let code = rust_code(line);
                *depth += code.matches('{').count() as i64 - code.matches('}').count() as i64;
                if n >= *start && *depth <= 0 && code.contains('}') {
                    in_tests = None;
                }
                continue;
            }
            let code = rust_code(line);
            if rel == IMPORT_MODULE {
                continue;
            }
            for model in ["TeamFile", "TeamMember", "render_team_yaml"] {
                if code.contains(model) {
                    found.push(format!("{rel}:{}: `{model}`: {}", n + 1, line.trim()));
                }
            }
            if code.contains("team.yaml") {
                match REVIEWED
                    .iter()
                    .position(|(file, text, _)| *file == rel && line.trim() == *text)
                {
                    Some(i) => used[i] = true,
                    None => found.push(format!("{rel}:{}: {}", n + 1, line.trim())),
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "the team file is named outside the import module:\n  {}",
        found.join("\n  ")
    );
    for (i, (file, text, why)) in REVIEWED.iter().enumerate() {
        assert!(used[i], "a stale reviewed line ({why}): {file}: {text}");
    }
}

/// None of the reviewed lines reads the file: they hold no file access at all.
#[test]
fn the_reviewed_lines_read_nothing() {
    for (file, text, _) in REVIEWED {
        for io in ["read", "open", "load", "fs::", "File"] {
            assert!(!text.contains(io), "{file}: {text}");
        }
    }
}

/// The stacks, the deploy kit, CI and the harness keep only the import or seed and
/// `pmos bot add-personal`: nothing writes, mounts or seeds from a `team.yaml`, and
/// `pmos member add` is gone.
#[test]
fn the_stacks_and_ci_never_write_or_read_a_team_file() {
    let repo = repo();
    let mut files: Vec<PathBuf> = Vec::new();
    for dir in ["deploy", "scripts", ".github/workflows", "harness/src"] {
        for entry in walkdir::WalkDir::new(repo.join(dir))
            .into_iter()
            .filter_entry(|e| e.file_name() != "node_modules" && e.file_name() != "backups")
        {
            let entry = entry.unwrap();
            if entry.file_type().is_file() {
                files.push(entry.into_path());
            }
        }
    }
    assert!(files.len() > 10, "read the kit ({} files)", files.len());
    let mut found = Vec::new();
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let ts = path
            .extension()
            .is_some_and(|e| e == "ts" || e == "js" || e == "mjs");
        for (n, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            let code = if ts {
                if trimmed.starts_with("//") || trimmed.starts_with('*') {
                    ""
                } else {
                    line
                }
            } else if trimmed.starts_with('#') {
                ""
            } else {
                line.split(" #").next().unwrap_or_default()
            };
            for banned in ["team.yaml", "member add", "TeamFile"] {
                if code.contains(banned) {
                    found.push(format!("{}:{}: {}", rel(&repo, path), n + 1, line.trim()));
                }
            }
        }
    }
    assert!(found.is_empty(), "\n  {}", found.join("\n  "));
    // The repo's own content has none (the lint refuses one; this says it here too).
    assert!(!repo.join("content/team.yaml").exists());
}
