//! Slice MP1 (`docs/model-providers-spec.md` §4.1, §4.8, §4.12): the route form of a role, its
//! models and `routing:`, and every start-up refusal. The one-hop form is unchanged.

use pmos_gateway::config::{check_config, Config, Feature, Models, RouteRule, UpstreamKind};

const TOKEN: &str = "route-config-test-token-0123456789abcdef";

/// A hosted-test style config with two remote test upstreams and two models.
fn two_hop(extra_role: &str) -> String {
    format!(
        "class: test\n\
         upstreams:\n  \
           up-a: {{url: 'openai+https://a.example.invalid/v1', key-file: /run/secrets/pmos-upstream-up-a.key, max-in-flight: 4}}\n  \
           up-b: {{url: 'openai+https://b.example.invalid/v1', key-file: /run/secrets/pmos-upstream-up-b.key}}\n\
         models:\n  \
           hosted-model-a: {{upstream: up-a, model: 'id-a', features: [tools, stream, json], context-tokens: 200000}}\n  \
           hosted-model-b: {{upstream: up-b, model: 'id-b', features: [tools, stream, json], context-tokens: 128000, workarounds: [strip-think]}}\n\
         roles:\n  \
           default: {{route: [hosted-model-a, hosted-model-b], requires: [tools, stream], allow-smaller-context: true}}\n  \
           extraction: {{route: [hosted-model-b], requires: [json]}}\n{extra_role}\
         routing: {{same-hop-attempts: 2, budget-secs: 30, circuit: {{failures: 3}}}}\n\
         clients: {{evals: {{route: pin}}, runtime: {{trusted: true, route: primary-only}}}}\n"
    )
}

#[test]
fn a_route_of_labelled_models_parses_with_its_primary_as_the_roles_fields() {
    let m = Models::parse(&two_hop(""), true).unwrap();
    let d = m.role("pmos/default").unwrap();
    assert!(d.routed());
    let hops = d.hops();
    assert_eq!(hops.len(), 2);
    assert_eq!(hops[0].label.as_deref(), Some("hosted-model-a"));
    assert_eq!(hops[1].label.as_deref(), Some("hosted-model-b"));
    assert_eq!(hops[1].workarounds, vec!["strip-think".to_string()]);
    assert_eq!(hops[0].kind, UpstreamKind::RemoteOpenAi);
    assert_eq!(hops[0].upstream, "https://a.example.invalid/v1");
    assert!(hops[0].has(Feature::Tools) && !hops[0].has(Feature::Embeddings));
    // The role's own fields are its primary's: label, model, upstream.
    assert_eq!(d.label.as_deref(), Some("hosted-model-a"));
    assert_eq!(d.reported_model(), "hosted-model-a");
    assert_eq!(d.model, "id-a");
    assert_eq!(m.upstream_caps.get("up-a"), Some(&4));
    assert_eq!(m.upstream_caps.get("up-b"), None);
    assert_eq!(m.routing.same_hop_attempts, 2);
    assert_eq!(m.client_policy("evals").route, RouteRule::Pin);
    assert_eq!(m.client_policy("runtime").route, RouteRule::PrimaryOnly);
    assert_eq!(m.client_policy("bot-docs").route, RouteRule::Fallback);
    // A one-hop role is never skipped for a feature it didn't declare.
    let legacy = Models::from_yaml(
        "upstreams: {x: 'http://vllm/v1'}\nroles: {default: {upstream: x, model: m}}\n",
    )
    .unwrap();
    let r = legacy.role("pmos/default").unwrap();
    assert!(!r.routed());
    assert_eq!(r.hops().len(), 1);
    assert!(Feature::ALL.iter().all(|f| r.hops()[0].has(*f)));
}

#[test]
fn every_shipped_config_parses_to_the_same_one_hop_roles() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (path, cloud) in [
        ("deploy/gateway.yaml", false),
        ("config/gateway.yaml", false),
        ("config/gateway.claude-test.yaml", true),
        ("deploy/gateway.hosted-test.yaml", true),
    ] {
        let text = std::fs::read_to_string(root.join(path)).unwrap();
        let m = Models::parse(&text, cloud).unwrap();
        for r in m.roles.values() {
            assert!(!r.routed(), "{path}: {}", r.name);
            let hops = r.hops();
            assert_eq!(hops.len(), 1, "{path}");
            let h = &hops[0];
            assert_eq!(
                (&h.upstream, &h.model, &h.label, h.kind, &h.workarounds),
                (&r.upstream, &r.model, &r.label, r.kind, &r.workarounds),
                "{path}: {}",
                r.name
            );
        }
    }
}

#[test]
fn every_route_rule_is_refused_at_start_up_by_label_and_never_by_url_or_id() {
    let base = two_hop("");
    let cases: Vec<(String, &str)> = vec![
        // An unknown label in a route.
        (
            base.replace("route: [hosted-model-b], requires: [json]", "route: [nope]"),
            "isn't a model",
        ),
        // An empty route.
        (
            base.replace("route: [hosted-model-b], requires: [json]", "route: []"),
            "empty `route:`",
        ),
        // A duplicate label.
        (
            base.replace(
                "route: [hosted-model-b], requires: [json]",
                "route: [hosted-model-b, hosted-model-b]",
            ),
            "twice",
        ),
        // A missing `requires` feature.
        (
            base.replace(
                "route: [hosted-model-b], requires: [json]",
                "route: [hosted-model-b], requires: [embeddings]",
            ),
            "lacks `embeddings`",
        ),
        // A smaller context without `allow-smaller-context`.
        (
            base.replace(", allow-smaller-context: true", ""),
            "smaller `context-tokens`",
        ),
        // Both forms on one role.
        (
            base.replace(
                "route: [hosted-model-b], requires: [json]",
                "route: [hosted-model-b], label: x",
            ),
            "belong to each model",
        ),
        // `requires` on a one-hop role.
        (
            base.replace(
                "  extraction: {route: [hosted-model-b], requires: [json]}",
                "  extraction: {upstream: up-b, model: x, label: l, requires: [json]}",
            ),
            "are for a role with a `route:`",
        ),
        // A model label that is a role name.
        (
            base.replace("hosted-model-b: {upstream", "extraction: {upstream")
                .replace("[hosted-model-a, hosted-model-b]", "[hosted-model-a]"),
            "role's name",
        ),
        // A model's upstream that isn't listed.
        (
            base.replace(
                "{upstream: up-b, model: 'id-b'",
                "{upstream: up-c, model: 'id-b'",
            ),
            "isn't listed",
        ),
        // An unknown feature.
        (
            base.replace(
                "[tools, stream, json], context-tokens: 128000",
                "[tools, stream, json, telepathy], context-tokens: 128000",
            ),
            "unknown feature",
        ),
        // A plain-http remote test upstream that isn't local.
        (
            base.replace(
                "openai+https://b.example.invalid",
                "openai+http://b.example.invalid",
            ),
            "must use https",
        ),
        // Routing settings out of range.
        (
            base.replace("same-hop-attempts: 2", "same-hop-attempts: 0"),
            "same-hop-attempts",
        ),
        (
            base.replace("budget-secs: 30", "budget-secs: 0"),
            "budget-secs",
        ),
        // A client route rule that isn't one.
        (
            base.replace("route: pin", "route: anything"),
            "route must be",
        ),
        // A relative key file.
        (
            base.replace(
                "key-file: /run/secrets/pmos-upstream-up-b.key",
                "key-file: up-b.key",
            ),
            "absolute path",
        ),
        // A cap of zero.
        (
            base.replace("max-in-flight: 4", "max-in-flight: 0"),
            "at least 1",
        ),
        // A one-hop role whose label is also a model's.
        (
            two_hop("  reasoning: {upstream: up-a, model: other, label: hosted-model-a}\n"),
            "a label names one model",
        ),
    ];
    for (yaml, want) in cases {
        let err = Models::parse(&yaml, true).unwrap_err();
        assert!(err.contains(want), "want `{want}`: {err}");
        for secret in ["example.invalid", "id-a", "id-b", "up-a", "up-b", "up-c"] {
            assert!(!err.contains(secret), "a refusal names `{secret}`: {err}");
        }
    }
    // A testing-only hop needs the cloud-test switch, as a one-hop role does.
    let err = Models::parse(&base, false).unwrap_err();
    assert!(err.contains("PMOS_GATEWAY_CLOUD_TEST=1"), "{err}");
}

#[test]
fn a_local_route_with_a_remote_fallback_never_starts() {
    // MP-3, MP-4: a `class: production` (local) route that names a remote test upstream, or a
    // server that isn't one of the two pinned ones, refuses at start-up, never at call time.
    let local = "class: production\n\
        upstreams:\n  d: http://vllm-default.pmos.internal:8000/v1\n  e: http://vllm-extraction.pmos.internal:8000/v1\n  \
        far: openai+https://far.example.invalid/v1\n  other: http://vllm-other.pmos.internal:8000/v1\n\
        models:\n  default-model: {upstream: d, model: q, features: [tools, stream]}\n  \
        extraction-model: {upstream: e, model: n, features: [tools, stream, json]}\n";
    let ok = format!(
        "{local}roles:\n  default: {{route: [default-model, extraction-model]}}\n  \
         extraction: {{route: [extraction-model, default-model]}}\n"
    );
    let m = Models::parse(&ok, false).unwrap();
    assert_eq!(m.role("pmos/extraction").unwrap().hops().len(), 2);
    let cfg = config(&[("PMOS_MODEL_SOURCE", "local")]).unwrap();
    cfg.check_source(&m).unwrap();
    for (extra, cloud) in [
        (
            "  far-model: {upstream: far, model: f, features: [tools, stream]}\n",
            true,
        ),
        (
            "  far-model: {upstream: other, model: f, features: [tools, stream]}\n",
            false,
        ),
    ] {
        let yaml =
            format!("{local}{extra}roles:\n  default: {{route: [default-model, far-model]}}\n");
        let err = Models::parse(&yaml, cloud).unwrap_err();
        assert!(err.contains("models.far-model"), "{err}");
        assert!(!err.contains("far.example"), "{err}");
    }
}

#[test]
fn hosted_test_routes_carry_a_label_on_every_hop_and_one_key_per_upstream() {
    let m = Models::parse(&two_hop(""), true).unwrap();
    let ok = config(&[
        ("PMOS_MODEL_SOURCE", "hosted-test"),
        ("PMOS_GATEWAY_CLOUD_TEST", "1"),
    ])
    .unwrap();
    ok.check_source(&m).unwrap();
    ok.check_keys(&m).unwrap();
    // The one environment key is refused beside two remote upstreams.
    let both = config(&[
        ("PMOS_MODEL_SOURCE", "hosted-test"),
        ("PMOS_GATEWAY_CLOUD_TEST", "1"),
        ("PMOS_GATEWAY_REMOTE_KEY", "a-key-that-must-not-be-used"),
    ])
    .unwrap();
    let err = both.check_keys(&m).unwrap_err();
    assert!(err.contains("two or more"), "{err}");
    assert!(!err.contains("a-key-that"), "{err}");
    // Two remote upstreams without key files.
    let no_files = two_hop("").replace(", key-file: /run/secrets/pmos-upstream-up-b.key", "");
    let m2 = Models::parse(&no_files, true).unwrap();
    assert!(ok.check_keys(&m2).unwrap_err().contains("own `key-file:`"));
    // A local model server takes no key file.
    let err = Models::parse(
        "upstreams: {x: {url: 'http://vllm/v1', key-file: /k}}\nroles: {default: {upstream: x, model: m}}\n",
        false,
    )
    .unwrap_err();
    assert!(err.contains("takes no `key-file`"), "{err}");
    // One remote upstream: the environment key still works, as before.
    let one = "class: test\nupstreams: {r: 'openai+https://r.example.invalid/v1'}\n\
               roles: {default: {upstream: r, model: m, label: hosted-model-a}}\n";
    let m3 = Models::parse(one, true).unwrap();
    let with_env = config(&[
        ("PMOS_MODEL_SOURCE", "hosted-test"),
        ("PMOS_GATEWAY_CLOUD_TEST", "1"),
        ("PMOS_GATEWAY_REMOTE_KEY", "k"),
    ])
    .unwrap();
    with_env.check_keys(&m3).unwrap();
    assert!(ok.check_keys(&m3).is_err());
}

#[test]
fn check_config_reports_labels_only() {
    let dir = std::env::temp_dir().join(format!("pmos-check-config-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("gateway.yaml");
    std::fs::write(&path, two_hop("")).unwrap();
    let env = |k: &str| match k {
        "PMOS_MODEL_SOURCE" => Some("hosted-test".to_string()),
        "PMOS_GATEWAY_CLOUD_TEST" => Some("1".to_string()),
        _ => None,
    };
    let out = check_config(env, Some(path.clone())).unwrap();
    assert!(
        out.contains("role default: route hosted-model-a, hosted-model-b"),
        "{out}"
    );
    for secret in ["example.invalid", "id-a", "up-a", "/run/secrets"] {
        assert!(!out.contains(secret), "{out}");
    }
    // The same config under `local` is refused.
    let err = check_config(
        |k| (k == "PMOS_MODEL_SOURCE").then(|| "local".to_string()),
        Some(path),
    )
    .unwrap_err();
    assert!(err.contains("CLOUD_TEST") || err.contains("class"), "{err}");
    std::fs::remove_dir_all(&dir).ok();
}

fn config(pairs: &[(&str, &str)]) -> Result<Config, String> {
    let pairs: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Config::from_lookup(move |k| {
        if k == "PMOS_GATEWAY_TOKENS" {
            return Some(format!("runtime={TOKEN}"));
        }
        pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
    })
}
