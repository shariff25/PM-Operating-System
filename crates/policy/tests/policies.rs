//! `pmos policy generate` and `check` over real directories: the committed policies are
//! current, and hand edits, planted files and removed bots are caught.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use pmos_policy::check::{
    CONTENT_INVALID, CRED_BINDING, CRED_PROVIDERS, FS_READABLE, FS_WRITABLE, NET_EGRESS,
    POLICY_MISSING, POLICY_ORPHAN, POLICY_STALE,
};
use pmos_policy::{check, generate, write};

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 14).unwrap()
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn copy_dir(from: &Path, to: &Path) {
    for entry in walkdir::WalkDir::new(from) {
        let entry = entry.unwrap();
        let target = to.join(entry.path().strip_prefix(from).unwrap());
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target).unwrap();
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// A copy of the repository's content plus a personal bot, with its policies generated.
struct Fixture {
    _dir: tempfile::TempDir,
    content: PathBuf,
    policies: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let content = dir.path().join("content");
        let policies = dir.path().join("policies");
        copy_dir(&repo().join("content"), &content);
        fs::create_dir_all(content.join("bots/pm-ana")).unwrap();
        fs::write(
            content.join("bots/pm-ana/BOT.md"),
            "---\nname: pm-ana\nrole: pm-assistant\nscope: personal\nowner: \"@ana\"\ndescription: Ana's assistant.\n---\n\n# Ana's bot\n\nHelp Ana.\n",
        )
        .unwrap();
        fs::create_dir_all(content.join("master/people/ana")).unwrap();
        fs::write(
            content.join("master/people/ana/AGENTS.md"),
            "# Ana's context\n",
        )
        .unwrap();
        let generated = generate(&content, today()).unwrap();
        write(&policies, &generated).unwrap();
        Self {
            _dir: dir,
            content,
            policies,
        }
    }

    fn check(&self) -> pmos_policy::CheckReport {
        check(&self.content, &self.policies, today())
    }

    fn edit(&self, rel: &str, f: impl FnOnce(String) -> String) {
        let path = self.policies.join(rel);
        let text = fs::read_to_string(&path).unwrap();
        fs::write(&path, f(text)).unwrap();
    }
}

#[test]
fn the_committed_policies_are_current_and_pass_every_rule() {
    let root = repo();
    let report = check(&root.join("content"), &root.join("policies"), today());
    assert!(
        report.passed(),
        "run `pmos policy generate`: {:#?}",
        report.violations
    );
    assert_eq!(report.bots, ["docs"]);
    assert_eq!(report.files_checked, 4);
}

#[test]
fn a_fresh_generation_passes_and_regenerating_changes_nothing() {
    let fx = Fixture::new();
    let report = fx.check();
    assert!(report.passed(), "{:#?}", report.violations);
    assert_eq!(report.bots, ["docs", "pm-ana"]);
    let again = write(&fx.policies, &generate(&fx.content, today()).unwrap()).unwrap();
    assert!(again.written.is_empty() && again.removed.is_empty());

    let ana = fs::read_to_string(fx.policies.join("bots/pm-ana.yaml")).unwrap();
    assert!(ana.contains("/sandbox/master/people/ana/AGENTS.md"));
    assert!(ana.contains("jira_create_issue"));
    // Bots don't see each other's content or credentials.
    let docs = fs::read_to_string(fx.policies.join("bots/docs.yaml")).unwrap();
    assert!(!docs.contains("people/ana") && !docs.contains("pm-ana"));
    assert!(!ana.contains("pmos-docs-"));
}

#[test]
fn a_hand_edit_fails_its_rule_and_as_stale() {
    let fx = Fixture::new();
    fx.edit("bots/docs.yaml", |t| {
        t.replacen(
            "network_policies:\n",
            "network_policies:\n  slack:\n    endpoints:\n      - host: slack.com\n        port: 443\n    binaries:\n      - path: /usr/local/bin/node\n",
            1,
        )
    });
    let report = fx.check();
    assert_eq!(report.rules(), [NET_EGRESS, POLICY_STALE]);
    assert_eq!(
        report.violations[0].path,
        format!("{}/bots/docs.yaml", fx.policies.display())
    );
}

#[test]
fn writable_content_and_borrowed_credentials_are_caught_in_place() {
    let fx = Fixture::new();
    fx.edit("bots/docs.yaml", |t| {
        t.replacen(
            "  read_write:\n",
            "  read_write:\n    - /sandbox/master/AGENTS.md\n",
            1,
        )
        .replace("pmos-docs-session", "pmos-pm-ana-session")
    });
    fx.edit("bots/docs.sandbox.yaml", |t| {
        t.replace("name: pmos-docs-gateway", "name: pmos-pm-ana-gateway")
    });
    let report = fx.check();
    let rules = report.rules();
    for rule in [FS_WRITABLE, CRED_BINDING, CRED_PROVIDERS, POLICY_STALE] {
        assert!(rules.contains(&rule), "{rule}: {:#?}", report.violations);
    }
}

#[test]
fn a_planted_policy_for_an_unknown_bot_is_an_orphan_and_fails_its_rules() {
    let fx = Fixture::new();
    let planted = fs::read_to_string(fx.policies.join("bots/pm-ana.yaml")).unwrap();
    fs::write(fx.policies.join("bots/evil.yaml"), planted).unwrap();
    let report = fx.check();
    let rules = report.rules();
    assert!(rules.contains(&POLICY_ORPHAN));
    assert!(rules.contains(&FS_READABLE));
    assert!(rules.contains(&CRED_BINDING));
    assert!(report
        .violations
        .iter()
        .all(|v| v.path.ends_with("bots/evil.yaml")));
}

#[test]
fn removing_a_bot_orphans_its_files_until_regenerated() {
    let fx = Fixture::new();
    fs::remove_dir_all(fx.content.join("bots/pm-ana")).unwrap();
    let report = fx.check();
    assert!(report.rules().contains(&POLICY_ORPHAN));
    assert!(report
        .violations
        .iter()
        .all(|v| v.path.contains("bots/pm-ana.")));

    let summary = write(&fx.policies, &generate(&fx.content, today()).unwrap()).unwrap();
    assert_eq!(
        summary.removed,
        ["bots/pm-ana.sandbox.yaml", "bots/pm-ana.yaml"]
    );
    assert!(fx.check().passed());
}

#[test]
fn a_missing_policy_is_reported() {
    let fx = Fixture::new();
    fs::remove_file(fx.policies.join("bots/docs.yaml")).unwrap();
    fs::remove_file(fx.policies.join("providers/pmos-session.yaml")).unwrap();
    let report = fx.check();
    assert_eq!(report.rules(), [POLICY_MISSING, POLICY_MISSING]);
}

#[test]
fn invalid_content_blocks_generation_and_fails_the_check() {
    let fx = Fixture::new();
    fs::write(
        fx.content.join("bots/pm-ana/BOT.md"),
        "---\nname: pm-ana\nrole: no-such-role\nscope: personal\nowner: ana\ndescription: x\n---\n\nbody\n",
    )
    .unwrap();
    assert!(generate(&fx.content, today()).is_err());
    let report = fx.check();
    assert!(!report.passed());
    assert!(report.rules().iter().all(|r| *r == CONTENT_INVALID));
}
