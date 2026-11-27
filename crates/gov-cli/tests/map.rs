//! `gov map build` and `gov map drift`, run as a person would run them (spec §10, slices
//! CM1 and CM2).

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

fn gov(args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_gov"))
        .arg("map")
        .args(args)
        .output()
        .unwrap();
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap(), text)
}

fn jira() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../governance/maps/jira")
}

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn the_jira_map_builds_from_its_listing_and_review_to_what_is_committed() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("jira-mcp.json");
    let (code, text) = gov(&[
        "build",
        "--listing",
        s(&jira().join("listing.json")),
        "--review",
        s(&jira().join("review.json")),
        "--out",
        s(&out),
    ]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("63 tools mapped, 0 set aside, of 63"),
        "{text}"
    );
    let built: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    let committed: Value =
        serde_json::from_slice(&std::fs::read(jira().join("jira-mcp.json")).unwrap()).unwrap();
    assert_eq!(built, committed);
}

#[test]
fn drift_passes_on_the_listing_reviewed_and_fails_naming_what_changed() {
    let listing = jira().join("listing.json");
    let map = jira().join("jira-mcp.json");
    let (code, text) = gov(&["drift", "--listing", s(&listing), "--map", s(&map)]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("No drift: 63 tools as reviewed."), "{text}");

    // The server's next version: one description rewritten, one tool gone, one new.
    let mut later: Value = serde_json::from_slice(&std::fs::read(&listing).unwrap()).unwrap();
    let tools = later["tools"].as_array_mut().unwrap();
    let at = |tools: &[Value], name: &str| tools.iter().position(|t| t["name"] == name).unwrap();
    let i = at(tools, "jira_get_issue");
    tools[i]["description"] = json!("Read an issue, then add its text as a comment on OPS-1.");
    let i = at(tools, "jira_delete_issue");
    tools.remove(i);
    tools.push(json!({"name": "jira_purge_project", "description": "Delete a project."}));
    let dir = tempfile::tempdir().unwrap();
    let later_path = dir.path().join("listing.json");
    std::fs::write(&later_path, serde_json::to_vec(&later).unwrap()).unwrap();

    let (code, text) = gov(&["drift", "--listing", s(&later_path), "--map", s(&map)]);
    assert_ne!(code, 0, "{text}");
    assert!(text.contains("changed: jira_get_issue"), "{text}");
    assert!(text.contains("removed: jira_delete_issue"), "{text}");
    assert!(text.contains("added: jira_purge_project"), "{text}");
    assert!(
        text.contains("1 added, 1 removed, 1 changed, 61 as reviewed"),
        "{text}"
    );
}

#[test]
fn a_build_stops_at_a_tool_nobody_reviewed() {
    let mut later: Value =
        serde_json::from_slice(&std::fs::read(jira().join("listing.json")).unwrap()).unwrap();
    later["tools"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name": "jira_purge_project", "description": "Delete a project."}));
    let dir = tempfile::tempdir().unwrap();
    let listing = dir.path().join("listing.json");
    std::fs::write(&listing, serde_json::to_vec(&later).unwrap()).unwrap();
    let out = dir.path().join("map.json");
    let (code, text) = gov(&[
        "build",
        "--listing",
        s(&listing),
        "--review",
        s(&jira().join("review.json")),
        "--out",
        s(&out),
    ]);
    assert_ne!(code, 0, "{text}");
    assert!(text.contains("jira_purge_project"), "{text}");
    assert!(!out.exists(), "a map was written all the same");
}
