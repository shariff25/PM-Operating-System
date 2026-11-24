//! Slice H12 (`docs/openshell-any-host-spec.md` §3.6, "Benchmarks under `hosted`"): a run counts
//! from a production gateway whose roles are all on the approved `hosted:<name>` upstream, when
//! `benchmark.production_gateway` names such a config: by role label and the approved entry's one
//! `model_sha256`, and only when every role was served by the `hosted` kind. With no approved
//! entry (the list ships empty) such a config can't be the production one at all.

use chrono::{NaiveDate, Utc};
use pmos_evals::history::{counts, production_models};
use pmos_evals::report::{GatewayModels, Report};

const HOSTED: &str = "class: production\nupstreams: {h: 'hosted:owner-hosted'}\n\
roles:\n  default: {upstream: h, model: 'host:main', label: hosted-model-a}\n  \
extraction: {upstream: h, model: 'host:main', label: hosted-model-a}\n";

fn evals_dir(list: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let evals = dir.path().join("evals");
    std::fs::create_dir_all(&evals).unwrap();
    std::fs::create_dir_all(dir.path().join("crates/gateway")).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals");
    let config = std::fs::read_to_string(root.join("config.yaml"))
        .unwrap()
        .replace(
            "production_gateway: ../deploy/gateway.yaml",
            "production_gateway: prod-gateway.yaml",
        );
    std::fs::write(evals.join("config.yaml"), config).unwrap();
    std::fs::write(evals.join("prod-gateway.yaml"), HOSTED).unwrap();
    std::fs::write(dir.path().join("crates/gateway/approved-hosted.yaml"), list).unwrap();
    dir
}

fn sha(c: char) -> String {
    std::iter::repeat_n(c, 64).collect()
}

fn entry(models: &[String]) -> String {
    format!(
        "- name: owner-hosted\n  url_sha256: \"{}\"\n  model_sha256: [{}]\n  record_sha256: \"{}\"\n  pull: 3\n",
        sha('a'),
        models.iter().map(|m| format!("\"{m}\"")).collect::<Vec<_>>().join(", "),
        sha('c')
    )
}

fn report(upstream: &str, model_sha: &str) -> Report {
    let at = Utc::now();
    let both = |v: &str| {
        ["default", "extraction"]
            .into_iter()
            .map(|r| (r.to_string(), v.to_string()))
            .collect()
    };
    Report {
        category: "docs-qa".into(),
        bot: "docs".into(),
        date: NaiveDate::from_ymd_opt(2026, 11, 24).unwrap(),
        started_at: at,
        finished_at: at,
        pass_bar: Some(0.8),
        runtime_url: "http://127.0.0.1:8750".into(),
        requested_by: "ana".into(),
        gateway_workarounds: None,
        note: None,
        gateway_models: Some(GatewayModels {
            roles: both("hosted-model-a"),
            upstreams: both(upstream),
            stub: false,
            cloud_test: false,
            workarounds: true,
            class: "production".into(),
            model_sha256: both(model_sha),
        }),
        persona_version: None,
        persona_hash: None,
        repeats: 1,
        judges: Vec::new(),
        judges_skipped: None,
        isolation: None,
        route: None,
        tasks: Vec::new(),
    }
}

#[test]
fn a_hosted_production_config_needs_an_approved_entry() {
    // The shipped list is empty: no hosted config is the production one.
    for empty in ["[]\n", "# nothing\n", ""] {
        let dir = evals_dir(empty);
        let err = production_models(&dir.path().join("evals")).unwrap_err();
        assert!(err.contains("no approved entry"), "{err}");
    }
    // An entry that pins two ids: which role uses which isn't committed, so nothing counts.
    let dir = evals_dir(&entry(&[sha('b'), sha('d')]));
    assert!(production_models(&dir.path().join("evals")).is_err());
}

#[test]
fn a_hosted_run_counts_by_label_and_the_entrys_hash() {
    let dir = evals_dir(&entry(&[sha('b')]));
    let p = production_models(&dir.path().join("evals")).unwrap();
    assert!(p.hosted);
    assert!(counts(&report("hosted", &sha('b')), &p, None).is_ok());
    // Another model behind the same label, a local or test upstream: none counts.
    assert!(counts(&report("hosted", &sha('e')), &p, None).is_err());
    assert!(counts(&report("local", &sha('b')), &p, None).is_err());
    assert!(counts(&report("remote-test", &sha('b')), &p, None).is_err());
}

#[test]
fn a_hosted_role_needs_a_label_and_no_role_may_be_elsewhere() {
    let dir = evals_dir(&entry(&[sha('b')]));
    let evals = dir.path().join("evals");
    std::fs::write(
        evals.join("prod-gateway.yaml"),
        HOSTED.replace(", label: hosted-model-a}\n  extraction", "}\n  extraction"),
    )
    .unwrap();
    assert!(production_models(&evals).is_err());
    std::fs::write(
        evals.join("prod-gateway.yaml"),
        format!(
            "{}  reasoning: {{upstream: l, model: m}}\n",
            HOSTED.replace(
                "upstreams: {h: 'hosted:owner-hosted'}",
                "upstreams: {h: 'hosted:owner-hosted', l: 'http://vllm-default.pmos.internal:8000/v1'}"
            )
        ),
    )
    .unwrap();
    assert!(production_models(&evals).is_err());
}
