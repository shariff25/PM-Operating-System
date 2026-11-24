//! Slice MP6 (`docs/model-providers-spec.md` §4.10, MP-16): a benchmark claim holds only for
//! each role's primary; a run records which models answered it, and one answered by a
//! fallback, or by any label that isn't a primary's, counts for no bar. Judges are pinned.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{NaiveDate, Utc};
use pmos_evals::history::{counts, production_models, ProductionModels};
use pmos_evals::judge::{Gateway, Judge};
use pmos_evals::report::{sha256_hex, GatewayModels, Report, RouteRecord};
use serde_json::json;

fn report(route: Option<RouteRecord>) -> Report {
    let at = Utc::now();
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
            roles: [("default", "model-a"), ("extraction", "model-x")]
                .into_iter()
                .map(|(r, l)| (r.to_string(), l.to_string()))
                .collect(),
            upstreams: ["default", "extraction"]
                .into_iter()
                .map(|r| (r.to_string(), "local".to_string()))
                .collect(),
            stub: false,
            cloud_test: false,
            workarounds: true,
            class: "production".into(),
            model_sha256: [("default", "id-a"), ("extraction", "id-x")]
                .into_iter()
                .map(|(r, id)| (r.to_string(), sha256_hex(id)))
                .collect(),
        }),
        persona_version: None,
        persona_hash: None,
        repeats: 1,
        judges: Vec::new(),
        judges_skipped: None,
        isolation: None,
        route,
        tasks: Vec::new(),
    }
}

/// A production config whose `default` routes over two local models.
const ROUTED: &str = "class: production\n\
upstreams:\n  d: http://vllm-default.pmos.internal:8000/v1\n  e: http://vllm-extraction.pmos.internal:8000/v1\n\
models:\n  model-a: {upstream: d, model: id-a, features: [tools, stream]}\n  model-b: {upstream: e, model: id-b, features: [tools, stream]}\n\
roles:\n  default: {route: [model-a, model-b]}\n  extraction: {upstream: e, model: id-x, label: model-x}\n";

fn production(yaml: &str) -> ProductionModels {
    let dir = tempfile::tempdir().unwrap();
    let evals = dir.path().join("evals");
    std::fs::create_dir_all(&evals).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals");
    let config = std::fs::read_to_string(root.join("config.yaml"))
        .unwrap()
        .replace(
            "production_gateway: ../deploy/gateway.yaml",
            "production_gateway: prod-gateway.yaml",
        );
    std::fs::write(evals.join("config.yaml"), config).unwrap();
    std::fs::write(evals.join("prod-gateway.yaml"), yaml).unwrap();
    production_models(&evals).unwrap()
}

fn answered(pairs: &[(&str, &str, u64)], fallback: bool) -> RouteRecord {
    let mut r = RouteRecord {
        pinned: "primary".into(),
        ..Default::default()
    };
    for (label, id, calls) in pairs {
        r.add(label, Some(&sha256_hex(id)), *calls, false);
    }
    r.fallback_used = fallback;
    r
}

#[test]
fn a_routed_roles_production_model_is_its_primary() {
    let p = production(ROUTED);
    assert!(p.routed);
    assert_eq!(p.roles["default"], "model-a");
    assert_eq!(p.model_sha256["default"], sha256_hex("id-a"));
    assert_eq!(p.roles["extraction"], "model-x");
}

#[test]
fn only_a_run_answered_by_the_primaries_counts() {
    let p = production(ROUTED);
    let ok = answered(&[("model-a", "id-a", 7), ("model-x", "id-x", 2)], false);
    assert_eq!(counts(&report(Some(ok.clone())), &p, None), Ok(()));
    // A fallback answer, even marked only by the header.
    let why = counts(
        &report(Some(answered(&[("model-a", "id-a", 7)], true))),
        &p,
        None,
    )
    .unwrap_err();
    assert!(why.contains("fallback"), "{why}");
    // A label that isn't a primary (the fallback, unmarked).
    let why = counts(
        &report(Some(answered(&[("model-b", "id-b", 1)], false))),
        &p,
        None,
    )
    .unwrap_err();
    assert!(why.contains("`model-b`"), "{why}");
    // The primary's label with another model behind it.
    let why = counts(
        &report(Some(answered(&[("model-a", "id-z", 1)], false))),
        &p,
        None,
    )
    .unwrap_err();
    assert!(why.contains("`model-a`"), "{why}");
    // One label, two hashes in one run.
    let mut two = ok.clone();
    two.add("model-a", Some(&sha256_hex("id-z")), 1, false);
    assert!(two.fallback_used);
    // With routes in production, a run must say what answered every task.
    assert!(counts(&report(None), &p, None)
        .unwrap_err()
        .contains("routes"));
    let mut gaps = ok.clone();
    gaps.unrecorded = 1;
    assert!(counts(&report(Some(gaps)), &p, None)
        .unwrap_err()
        .contains("didn't record"));
    // Without routes in production, a report from before routes still counts as it did.
    let plain = production(
        "upstreams: {q: 'http://vllm:8000/v1'}\nroles:\n  default: {upstream: q, model: id-a, label: model-a}\n  extraction: {upstream: q, model: id-x, label: model-x}\n",
    );
    assert!(!plain.routed);
    assert_eq!(counts(&report(None), &plain, None), Ok(()));
    // ...but a recorded fallback never does.
    assert!(counts(&report(Some(answered(&[], true))), &plain, None).is_err());
}

#[tokio::test]
async fn a_judge_is_pinned_to_its_primary_and_refuses_a_fallback_answer() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let fallback: Arc<Mutex<&'static str>> = Arc::new(Mutex::new("0"));
    let app = Router::new()
        .route(
            "/v1/pmos/roles",
            get(|| async {
                Json(
                    json!({"class": "test", "model_source": "hosted-test", "stub": false,
                    "cloud_test": true, "workarounds": true,
                    "roles": [{"role": "default", "alias": "pmos/default", "model": "model-a",
                               "upstream": "remote-test", "model_sha256": sha256_hex("id-a")}]}),
                )
            }),
        )
        .route(
            "/v1/chat/completions",
            post({
                let seen = seen.clone();
                let fallback = fallback.clone();
                move |h: HeaderMap| async move {
                    seen.lock().unwrap().push(
                        h.get("x-pmos-fallback")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_string(),
                    );
                    let f = *fallback.lock().unwrap();
                    (
                        [
                            ("x-pmos-model-source", "hosted-test"),
                            ("x-pmos-model", if f == "1" { "model-b" } else { "model-a" }),
                            ("x-pmos-fallback", f),
                        ],
                        Json(json!({"choices": [{"message": {"content": "yes"}}]})),
                    )
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let gw = Gateway::new(&base, "evals-token-0123456789abcdef0123456789");
    let judge = Judge {
        name: "faithful".into(),
        failure_mode: "model/unfaithful-to-source".into(),
        question: "Is it faithful?".into(),
        alias: "pmos/default".into(),
        applies_to: vec!["docs-qa".into()],
        labels: "faithful.jsonl".into(),
        needs_sources: false,
    };
    assert!(gw.ask(&judge, "in", "out").await.is_ok());
    *fallback.lock().unwrap() = "1";
    let err = gw.ask(&judge, "in", "out").await.unwrap_err();
    assert!(err.contains("fallback"), "{err}");
    // Every judge call asked for the primary only.
    assert_eq!(*seen.lock().unwrap(), vec!["off", "off"]);
    let r = gw.route_record();
    assert!(r.fallback_used);
    assert_eq!(r.answered["model-a"].calls, 1);
    let _: BTreeMap<String, String> = BTreeMap::new();
}
