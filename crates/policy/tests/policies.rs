//! `pmos policy generate` and `check` over real directories: the committed policies are
//! current, and hand edits, planted files and removed bots are caught.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use pmos_policy::check::{
    CONTENT_INVALID, CRED_BINDING, CRED_PROVIDERS, FS_READABLE, FS_UPLOADS, FS_WRITABLE,
    NET_EGRESS, NET_RUNTIME_ROUTES, POLICY_MISSING, POLICY_ORPHAN, POLICY_STALE,
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
        .replace("provider: pmos-docs-worker", "provider: pmos-pm-ana-worker")
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
fn a_hand_edit_reading_tasks_on_the_runtime_fails_net_runtime_routes() {
    let fx = Fixture::new();
    fx.edit("bots/docs.yaml", |t| {
        t.replacen(
            "              path: /v1/worker/claim\n",
            "              path: /v1/worker/claim\n          - allow:\n              method: GET\n              path: /v1/tasks\n          - allow:\n              method: GET\n              path: /v1/routines\n",
            1,
        )
    });
    let report = fx.check();
    assert_eq!(
        report.rules(),
        [NET_RUNTIME_ROUTES, NET_RUNTIME_ROUTES, POLICY_STALE],
        "{:#?}",
        report.violations
    );
}

#[test]
fn the_old_session_provider_is_an_orphan() {
    let fx = Fixture::new();
    fs::write(
        fx.policies.join("providers/pmos-session.yaml"),
        "# Generated by `pmos policy generate`. NVIDIA OpenShell provider profile.\nid: pmos-session\n",
    )
    .unwrap();
    assert_eq!(fx.check().rules(), [POLICY_ORPHAN]);
    let summary = write(&fx.policies, &generate(&fx.content, today()).unwrap()).unwrap();
    assert_eq!(summary.removed, ["providers/pmos-session.yaml"]);
}

#[test]
fn a_missing_policy_is_reported() {
    let fx = Fixture::new();
    fs::remove_file(fx.policies.join("bots/docs.yaml")).unwrap();
    fs::remove_file(fx.policies.join("providers/pmos-worker.yaml")).unwrap();
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

// The bot marketplace's proof (`docs/bot-marketplace-spec.md` §4): a persona adds read-only
// skill folders to a personal bot of the personal role, and nothing else.

const PREP_SKILL: &str = "---\nname: fixture-prep\ndescription: Prepares the PM for a meeting (a test fixture).\nmetadata:\n  owner: \"@pm\"\n  version: \"0.1.0\"\n  model: \"default\"\n  data-class: \"restricted\"\n  tools: \"context.search jira.search confluence.create_draft\"\n  surfaces: \"slack web\"\n---\n\n# Prep\n\nSteps.\n";

const CHIEF_ROLE: &str = "---\nname: fixture-chief\ndescription: Runs the PM's week (a test fixture).\nowner: \"@pm\"\nversion: \"0.1.0\"\nskills: \"ask daily-brief fixture-prep\"\ndata-class: \"restricted\"\nmodel: \"reasoning\"\nactions:\n  slack.post_reply: approve\n---\n\n# Chief\n\nRun the PM's week.\n";

fn chief_persona(offered: bool) -> String {
    format!("---\nname: chief\ndisplay-name: Chief\ntitle: Chief of Staff\nrole: fixture-chief\nmascot: chief\nowner: \"@pm\"\nversion: \"0.1.0\"\noffered: {offered}\norder: 20\nsummary: Runs your week.\nexamples:\n  - Prep me for the staff meeting\n---\n\n# Chief\n")
}

/// A copy of the repository's content with two personal bots, `pm-ana` (the personal role)
/// and `pm-cy` (another role), and, when `persona` is given, the Chief fixture: a role with
/// one new skill, `fixture-prep` (whose text is `prep`), and a persona of that role.
fn marketplace_content(dir: &Path, persona: Option<String>, prep: &str) -> PathBuf {
    let content = dir.join("content");
    copy_dir(&repo().join("content"), &content);
    // The fixtures stand in for the catalog: Nudge alone, plus the Chief fixture when asked.
    // The repository's own specialists are left out (their proof is
    // `the_repositorys_specialists_add_only_read_only_skill_folders`).
    for key in ["chief", "sprout", "echo"] {
        let _ = fs::remove_dir_all(content.join("personas").join(key));
    }
    let put = |rel: &str, text: &str| {
        let path = content.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    };
    put(
        "bots/pm-ana/BOT.md",
        "---\nname: pm-ana\nrole: pm-assistant\nscope: personal\nowner: \"@ana\"\ndescription: Ana's assistant.\n---\n\n# Ana's bot\n\nHelp Ana.\n",
    );
    put(
        "bots/pm-cy/BOT.md",
        "---\nname: pm-cy\nrole: docs-helper\nscope: personal\nowner: \"@cy\"\ndescription: Cy's docs helper.\n---\n\n# Cy's bot\n\nHelp Cy.\n",
    );
    if let Some(persona) = persona {
        put("skills/fixture-prep/SKILL.md", prep);
        put("roles/fixture-chief/ROLE.md", CHIEF_ROLE);
        put("personas/chief/PERSONA.md", &persona);
    }
    content
}

/// The top-level `key:` block of a generated YAML file, as text.
fn section(text: &str, key: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for line in text.lines() {
        if !line.starts_with(' ') && !line.starts_with('#') {
            inside = line.starts_with(&format!("{key}:"));
        }
        if inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    assert!(!out.is_empty(), "no `{key}` in the file");
    out
}

#[test]
fn personas_add_only_read_only_skill_folders_to_the_personal_bots() {
    let without_dir = tempfile::tempdir().unwrap();
    let with_dir = tempfile::tempdir().unwrap();
    let without = marketplace_content(without_dir.path(), None, PREP_SKILL);
    let with = marketplace_content(with_dir.path(), Some(chief_persona(true)), PREP_SKILL);
    let before = generate(&without, today()).unwrap();
    let after = generate(&with, today()).unwrap();

    // The same files; everything but pm-ana's two is byte for byte the same: the team bot, a
    // personal bot of another role, and the provider profiles.
    assert_eq!(
        before.files.keys().collect::<Vec<_>>(),
        after.files.keys().collect::<Vec<_>>()
    );
    for (rel, text) in &before.files {
        if rel.starts_with("bots/pm-ana.") {
            continue;
        }
        assert_eq!(text, &after.files[rel], "{rel} changed");
    }

    // pm-ana's policy: the network section is byte-identical, and so are the process,
    // Landlock and the version. Only the read-only list differs, by the new folder.
    let (p0, p1) = (
        &before.files["bots/pm-ana.yaml"],
        &after.files["bots/pm-ana.yaml"],
    );
    for key in ["network_policies", "process", "landlock", "version"] {
        assert_eq!(section(p0, key), section(p1, key), "{key}");
    }
    let (a, b) = (
        pmos_policy::openshell::parse(p0).unwrap(),
        pmos_policy::openshell::parse(p1).unwrap(),
    );
    assert_eq!(a.network_policies, b.network_policies);
    assert_eq!(a.network_middlewares, b.network_middlewares);
    let (fa, fb) = (
        a.filesystem_policy.clone().unwrap(),
        b.filesystem_policy.clone().unwrap(),
    );
    assert_eq!(fa.read_write, fb.read_write);
    assert_eq!(fa.include_workdir, fb.include_workdir);
    let ro0: BTreeSet<&String> = fa.read_only.iter().collect();
    let ro1: BTreeSet<&String> = fb.read_only.iter().collect();
    assert!(ro0.is_subset(&ro1));
    assert_eq!(
        ro1.difference(&ro0).map(|p| p.as_str()).collect::<Vec<_>>(),
        ["/sandbox/skills/fixture-prep"]
    );
    // Take the added path out and the two policies are equal in full.
    let mut stripped = b.clone();
    stripped
        .filesystem_policy
        .as_mut()
        .unwrap()
        .read_only
        .retain(|p| p != "/sandbox/skills/fixture-prep");
    assert_eq!(stripped, a);
    // The header names the catalog as a source.
    assert!(p1.contains("personas/*/PERSONA.md") && !p0.contains("personas/"));

    // The launch spec: the same providers and identity; the uploads differ by the one folder.
    let s0 = pmos_policy::spec::parse(&before.files["bots/pm-ana.sandbox.yaml"]).unwrap();
    let s1 = pmos_policy::spec::parse(&after.files["bots/pm-ana.sandbox.yaml"]).unwrap();
    assert_eq!(s0.providers, s1.providers);
    let added: Vec<String> = s1
        .uploads
        .iter()
        .filter(|u| !s0.uploads.contains(u))
        .map(|u| format!("{}:{}", u.from, u.to))
        .collect();
    assert_eq!(added, ["skills/fixture-prep:/sandbox/skills/fixture-prep"]);
    assert!(s0.uploads.iter().all(|u| s1.uploads.contains(u)));
    let mut s1_stripped = s1.clone();
    s1_stripped
        .uploads
        .retain(|u| u.from != "skills/fixture-prep");
    assert_eq!(s1_stripped, s0);

    // The bot contexts say the same: tools unchanged, one persona skill.
    let ctx = |g: &pmos_policy::Generated| g.bots.iter().find(|b| b.bot == "pm-ana").cloned();
    let (c0, c1) = (ctx(&before).unwrap(), ctx(&after).unwrap());
    assert_eq!(c0.mcp_tools, c1.mcp_tools);
    assert!(c0.persona_skills.is_empty());
    assert_eq!(c1.persona_skills, ["fixture-prep"]);
    let cy = after.bots.iter().find(|b| b.bot == "pm-cy").unwrap();
    assert!(cy.persona_skills.is_empty() && !cy.skills.iter().any(|s| s == "fixture-prep"));

    // Written out, the policies with the persona pass every rule; the old ones fail
    // `pmos policy check` against the new content, for exactly pm-ana's two files.
    let policies = with_dir.path().join("policies");
    write(&policies, &after).unwrap();
    let report = check(&with, &policies, today());
    assert!(report.passed(), "{:#?}", report.violations);
    let stale_dir = without_dir.path().join("policies");
    write(&stale_dir, &before).unwrap();
    let report = check(&with, &stale_dir, today());
    assert_eq!(
        report.rules(),
        [POLICY_STALE, POLICY_STALE],
        "{:#?}",
        report.violations
    );
    assert!(report
        .violations
        .iter()
        .all(|v| v.path.contains("bots/pm-ana.")));
    // The other way round, the persona's folder without the persona in the content is a
    // grant nothing gives: it fails its rules as well as `policy-stale`.
    let report = check(&without, &policies, today());
    let mut rules = report.rules();
    rules.sort();
    assert_eq!(
        rules,
        [FS_READABLE, FS_UPLOADS, POLICY_STALE, POLICY_STALE],
        "{:#?}",
        report.violations
    );
    assert!(report
        .violations
        .iter()
        .any(|v| v.message.contains("or any persona in the catalog")));
}

/// The real catalog (Chief, Sprout, Echo): for a personal bot of the personal role, the policy
/// differs from the one without them only by the specialists' new read-only skill folders and
/// the header's source line. No network rule, process, Landlock or writable path changes.
#[test]
fn the_repositorys_specialists_add_only_read_only_skill_folders() {
    let with_dir = tempfile::tempdir().unwrap();
    let without_dir = tempfile::tempdir().unwrap();
    let with = marketplace_content(with_dir.path(), None, PREP_SKILL);
    let without = marketplace_content(without_dir.path(), None, PREP_SKILL);
    for key in ["chief", "sprout", "echo"] {
        copy_dir(
            &repo().join("content/personas").join(key),
            &with.join("personas").join(key),
        );
    }
    let before = generate(&without, today()).unwrap();
    let after = generate(&with, today()).unwrap();
    for (rel, text) in &before.files {
        if !rel.starts_with("bots/pm-ana.") {
            assert_eq!(text, &after.files[rel], "{rel} changed");
        }
    }
    let (p0, p1) = (
        &before.files["bots/pm-ana.yaml"],
        &after.files["bots/pm-ana.yaml"],
    );
    for key in ["network_policies", "process", "landlock", "version"] {
        assert_eq!(section(p0, key), section(p1, key), "{key}");
    }
    let (a, b) = (
        pmos_policy::openshell::parse(p0).unwrap(),
        pmos_policy::openshell::parse(p1).unwrap(),
    );
    assert_eq!(a.network_policies, b.network_policies);
    assert_eq!(a.network_middlewares, b.network_middlewares);
    let (fa, fb) = (a.filesystem_policy.unwrap(), b.filesystem_policy.unwrap());
    assert_eq!(fa.read_write, fb.read_write);
    let ro0: BTreeSet<&String> = fa.read_only.iter().collect();
    let ro1: BTreeSet<&String> = fb.read_only.iter().collect();
    assert!(ro0.is_subset(&ro1));
    assert_eq!(
        ro1.difference(&ro0).map(|p| p.as_str()).collect::<Vec<_>>(),
        [
            "/sandbox/skills/battlecard",
            "/sandbox/skills/experiment-backlog",
            "/sandbox/skills/funnel-diagnosis",
            "/sandbox/skills/meeting-prep",
            "/sandbox/skills/positioning-brief",
            "/sandbox/skills/week-review",
        ]
    );
    let ctx = |g: &pmos_policy::Generated| g.bots.iter().find(|b| b.bot == "pm-ana").cloned();
    let (c0, c1) = (ctx(&before).unwrap(), ctx(&after).unwrap());
    assert_eq!(c0.mcp_tools, c1.mcp_tools);
}

#[test]
fn offering_a_persona_changes_no_policy() {
    let offered = tempfile::tempdir().unwrap();
    let withdrawn = tempfile::tempdir().unwrap();
    let a = generate(
        &marketplace_content(offered.path(), Some(chief_persona(true)), PREP_SKILL),
        today(),
    )
    .unwrap();
    let b = generate(
        &marketplace_content(withdrawn.path(), Some(chief_persona(false)), PREP_SKILL),
        today(),
    )
    .unwrap();
    assert_eq!(a.files, b.files);
}

#[test]
fn nudge_alone_changes_no_policy_file() {
    // The repository's catalog is Nudge alone, whose role is the personal role: with it or
    // without any catalog, every generated file is the same, byte for byte.
    let with_dir = tempfile::tempdir().unwrap();
    let without_dir = tempfile::tempdir().unwrap();
    let with = marketplace_content(with_dir.path(), None, PREP_SKILL);
    let without = marketplace_content(without_dir.path(), None, PREP_SKILL);
    assert!(with.join("personas/nudge/PERSONA.md").is_file());
    fs::remove_dir_all(without.join("personas")).unwrap();
    let a = generate(&with, today()).unwrap();
    let b = generate(&without, today()).unwrap();
    assert_eq!(a.files, b.files);
}

#[test]
fn a_persona_reaching_a_new_tool_is_refused_before_generation() {
    // `confluence.update_page` is not a tool of the personal role.
    let wider = PREP_SKILL.replace(
        "tools: \"context.search jira.search confluence.create_draft\"",
        "tools: \"context.search confluence.update_page\"",
    );
    let dir = tempfile::tempdir().unwrap();
    let content = marketplace_content(dir.path(), Some(chief_persona(true)), &wider);
    let errors = generate(&content, today()).unwrap_err();
    assert!(
        errors.iter().any(|d| d.code == "persona-exceeds-base"
            && d.path == "personas/chief/PERSONA.md"
            && d.message.contains("confluence.update_page")),
        "{errors:#?}"
    );
    let report = check(&content, &dir.path().join("policies"), today());
    assert!(!report.passed());
    assert!(report.rules().iter().all(|r| *r == CONTENT_INVALID));
}
