//! Slice H12 (`docs/openshell-any-host-spec.md` §3.6, "The hosted model as a production
//! source"; §6, the H12 rows): `PMOS_MODEL_SOURCE=hosted`.
//!
//! The shipped list is empty, so `hosted` refuses to start from a fresh checkout. Every other
//! path is exercised here with a fixture list (the same parser the compiled list goes through)
//! and a local TLS mock of the provider, whose certificate chains to a test CA made at run time
//! and given to the hosted client in place of the compiled-in roots. Fake values only: the
//! mock's name is under `.test`, and its "model id" is a marker string that must never reach a
//! caller, a report or a log.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use pmos_contract::model_source::ModelSource;
use pmos_gateway::config::{sha256_hex, Client, Config, Models};
use pmos_gateway::hosted::{self, ApprovedList, BuildInfo, HostedEnv, HostedNet, ListKind};
use pmos_gateway::usage::UsageLog;
use pmos_gateway::{router_with, GatewayConfig, RouterOptions, Timeouts};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "gateway-hosted-test-token-0123456789";
const KEY: &str = "hosted-key-marker-sk-0000000000000000";
const CONFIG: &str = "class: production\nupstreams: {h: 'hosted:owner-hosted'}\n\
roles:\n  default: {upstream: h, model: 'host:main', label: hosted-model-a}\n  \
extraction: {upstream: h, model: 'host:main', label: hosted-model-a}\n";

#[path = "support/hosted_mock.rs"]
mod hosted_mock;
use hosted_mock::{pki, start_mock, Mock, Pki, HOST, MARKER, MODEL_ID};

// --- the fixture list and the host's settings -----------------------------------------

fn list_for(url: &str, extra: &str) -> ApprovedList {
    let (_, normalised) = hosted::normalise_url(url).unwrap();
    let text = format!(
        "- name: owner-hosted\n  url_sha256: \"{}\"\n  model_sha256: [\"{}\"]\n  record_sha256: \
         \"{}\"\n  pull: 12\n{extra}",
        sha256_hex(&normalised),
        sha256_hex(MODEL_ID),
        sha256_hex("a record kept outside the repository"),
    );
    hosted::parse_list(ListKind::Release, &text).unwrap()
}

struct Host {
    _dir: tempfile::TempDir,
    key: PathBuf,
    control: PathBuf,
    ca: PathBuf,
}

fn host(pki: &Pki) -> Host {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("hosted-model.key");
    std::fs::write(&key, format!("{KEY}\n")).unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    let control = dir.path().join("hosted-control");
    std::fs::create_dir(&control).unwrap();
    let ca = dir.path().join("ca.pem");
    std::fs::write(&ca, &pki.ca_pem).unwrap();
    Host {
        _dir: dir,
        key,
        control,
        ca,
    }
}

fn env_for(h: &Host, url: &str) -> HostedEnv {
    HostedEnv {
        url: Some(url.to_string()),
        models: Some(format!("main={MODEL_ID}")),
        key_file: Some(h.key.clone()),
        ..Default::default()
    }
}

fn build() -> BuildInfo {
    BuildInfo {
        commit: Some("1111111111111111111111111111111111111111".into()),
        merged_commit: Some("2222222222222222222222222222222222222222".into()),
        list_checked_at: None,
    }
}

fn bind(list: &ApprovedList, env: &HostedEnv, control: &Path) -> Result<hosted::Binding, String> {
    let mut models = Models::parse(CONFIG, false).unwrap();
    hosted::bind(&mut models, list, env, control, build())
}

/// Every refusal names the setting, never the URL, the host or the model id.
fn assert_names_nothing(err: &str) {
    for secret in [HOST, MODEL_ID, KEY, "127.0.0.1", "hosted-mock"] {
        assert!(!err.contains(secret), "`{secret}` in: {err}");
    }
}

// --- a gateway on the mock -------------------------------------------------------------

struct Gw {
    base: String,
    stop: CancellationToken,
    usage: Arc<UsageLog>,
}

fn loopback() -> hosted::Lookup {
    Arc::new(|_| Box::pin(async { Ok(vec!["127.0.0.1".parse::<IpAddr>().unwrap()]) }))
}

/// The hosted client as the tests build it: the test CA in place of Mozilla's roots, the mock's
/// name answered with loopback (which the strict resolver refuses, so it is allowed here), and
/// no proxy.
fn test_net(pki: &Pki) -> HostedNet {
    HostedNet {
        roots: Some(reqwest::Certificate::from_pem_bundle(pki.ca_pem.as_bytes()).unwrap()),
        lookup: Some(loopback()),
        allow_private: true,
        no_proxy: true,
    }
}

async fn gateway(models: Models, binding: hosted::Binding, net: HostedNet) -> Gw {
    gateway_with(models, binding, net, Timeouts::default()).await
}

async fn gateway_with(
    models: Models,
    binding: hosted::Binding,
    net: HostedNet,
    timeouts: Timeouts,
) -> Gw {
    let usage = Arc::new(UsageLog::new(None).unwrap());
    let app = router_with(
        GatewayConfig {
            models,
            clients: vec![Client {
                name: "runtime".into(),
                token: TOKEN.into(),
            }],
            usage: usage.clone(),
            stub: false,
            workarounds_on: true,
            anthropic_key: String::new(),
            remote_key: String::new(),
            retry: pmos_gateway::retry::RetryPolicy {
                max_attempts: 1,
                ..Default::default()
            },
            timeouts,
            model_source: ModelSource::Hosted,
        },
        RouterOptions {
            hosted: Some(binding),
            hosted_net: Some(net),
            ..Default::default()
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let ct = stop.clone();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { ct.cancelled_owned().await })
            .await
            .unwrap();
    });
    Gw { base, stop, usage }
}

async fn bound_gateway(pki: &Pki, mock: &Mock, h: &Host) -> Gw {
    let list = list_for(&mock.url(), "");
    let mut models = Models::parse(CONFIG, false).unwrap();
    let binding = hosted::bind(
        &mut models,
        &list,
        &env_for(h, &mock.url()),
        &h.control,
        build(),
    )
    .unwrap();
    gateway(models, binding, test_net(pki)).await
}

fn call(gw: &Gw, source: &str, run: Option<&str>) -> reqwest::RequestBuilder {
    let mut b = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", gw.base))
        .bearer_auth(TOKEN)
        .header("x-pmos-model-source", source)
        .json(&json!({"model": "pmos/default",
                      "messages": [{"role": "user", "content": "fake data only"}]}));
    if let Some(r) = run {
        b = b.header("x-pmos-run", r);
    }
    b
}

async fn get_json(url: String, token: bool) -> (u16, Value) {
    let mut b = reqwest::Client::new().get(url);
    if token {
        b = b.bearer_auth(TOKEN);
    }
    let res = b.send().await.unwrap();
    let status = res.status().as_u16();
    (status, res.json().await.unwrap_or(Value::Null))
}

// --- the tests ---------------------------------------------------------------------------

/// The safety target: on a fresh checkout the compiled list is empty, so `hosted` refuses to
/// start, saying there is no approved entry, whatever the host's settings say.
#[test]
fn a_fresh_checkout_refuses_hosted_with_no_approved_entry() {
    let list = hosted::compiled().unwrap();
    assert_eq!(list.kind, ListKind::Release);
    assert!(list.entries.is_empty(), "the release list ships empty");
    let pki = pki();
    let h = host(&pki);
    let env = env_for(&h, &format!("https://{HOST}/v1"));
    let err = bind(&list, &env, &h.control).unwrap_err();
    assert!(err.contains("no approved entry"), "{err}");
    assert_names_nothing(&err);
    // The shipped hosted config is checked and refused the same way by --check-config.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cfg = root.join("deploy/gateway.hosted.yaml");
    let token = format!("runtime={TOKEN}");
    let url = format!("https://{HOST}/v1");
    let err = pmos_gateway::config::check_config(
        |k| match k {
            "PMOS_MODEL_SOURCE" => Some("hosted".into()),
            "PMOS_GATEWAY_TOKENS" => Some(token.clone()),
            "PMOS_GATEWAY_HOSTED_URL" => Some(url.clone()),
            "PMOS_GATEWAY_HOSTED_MODELS" => Some(format!("main={MODEL_ID}")),
            _ => None,
        },
        Some(cfg.clone()),
    )
    .unwrap_err();
    assert!(err.contains("no approved entry"), "{err}");
    // And the shipped config itself fits `hosted` and nothing else.
    let text = std::fs::read_to_string(&cfg).unwrap();
    let models = Models::parse(&text, false).unwrap();
    let config = |source: &str| {
        let token = token.clone();
        let source = source.to_string();
        Config::from_lookup(move |k| match k {
            "PMOS_MODEL_SOURCE" => Some(source.clone()),
            "PMOS_GATEWAY_TOKENS" => Some(token.clone()),
            _ => None,
        })
        .unwrap()
    };
    assert!(config("hosted").check_source(&models).is_ok());
    assert!(config("local").check_source(&models).is_err());
    assert!(config("hosted-test").check_source(&models).is_err());
    assert!(
        !text.contains("://"),
        "no URL in the committed hosted config"
    );
}

/// Every start-up refusal of §3.6, each naming the setting and never the URL or the id.
#[test]
fn hosted_refuses_to_start_on_each_unmet_condition() {
    let pki = pki();
    let h = host(&pki);
    let url = format!("https://{HOST}/v1");
    let list = list_for(&url, "");
    let ok_env = env_for(&h, &url);
    assert!(bind(&list, &ok_env, &h.control).is_ok());
    let refused = |env: HostedEnv, want: &str| {
        let err = bind(&list, &env, &h.control).unwrap_err();
        assert!(err.contains(want), "want `{want}`: {err}");
        assert_names_nothing(&err);
    };
    // The URL: missing, another endpoint's digest, an IP literal, not https.
    refused(
        HostedEnv {
            url: None,
            ..ok_env.clone()
        },
        "PMOS_GATEWAY_HOSTED_URL",
    );
    refused(
        HostedEnv {
            url: Some(format!("https://other.{HOST}/v1")),
            ..ok_env.clone()
        },
        "url_sha256",
    );
    refused(
        HostedEnv {
            url: Some("https://127.0.0.1/v1".into()),
            ..ok_env.clone()
        },
        "IP address",
    );
    refused(
        HostedEnv {
            url: Some(format!("http://{HOST}/v1")),
            ..ok_env.clone()
        },
        "https",
    );
    // The same URL written otherwise hashes the same (case, `:443`, a trailing dot and `/`).
    let same = format!("https://{}.:443/v1/", HOST.to_uppercase());
    assert!(bind(
        &list,
        &HostedEnv {
            url: Some(same),
            ..ok_env.clone()
        },
        &h.control
    )
    .is_ok());
    // The model ids: a ref missing, a malformed id, an id outside the entry.
    refused(
        HostedEnv {
            models: None,
            ..ok_env.clone()
        },
        "host:main",
    );
    refused(
        HostedEnv {
            models: Some("fast=x".into()),
            ..ok_env.clone()
        },
        "host:main",
    );
    refused(
        HostedEnv {
            models: Some(format!("main={MODEL_ID} x")),
            ..ok_env.clone()
        },
        "model id",
    );
    refused(
        HostedEnv {
            models: Some("main=another-model".into()),
            ..ok_env.clone()
        },
        "model_sha256",
    );
    // The key file: missing, empty, readable by others.
    refused(
        HostedEnv {
            key_file: None,
            ..ok_env.clone()
        },
        "PMOS_GATEWAY_HOSTED_KEY_FILE",
    );
    refused(
        HostedEnv {
            key_file: Some(h.control.join("none")),
            ..ok_env.clone()
        },
        "missing",
    );
    {
        use std::os::unix::fs::PermissionsExt;
        let empty = h.control.parent().unwrap().join("empty.key");
        std::fs::write(&empty, "").unwrap();
        std::fs::set_permissions(&empty, std::fs::Permissions::from_mode(0o600)).unwrap();
        refused(
            HostedEnv {
                key_file: Some(empty.clone()),
                ..ok_env.clone()
            },
            "empty",
        );
        std::fs::write(&empty, "k").unwrap();
        std::fs::set_permissions(&empty, std::fs::Permissions::from_mode(0o644)).unwrap();
        refused(
            HostedEnv {
                key_file: Some(empty),
                ..ok_env.clone()
            },
            "readable",
        );
    }
    // SSL_CERT_FILE or SSL_CERT_DIR; a run-time CA under `tls_interception: refused`.
    refused(
        HostedEnv {
            ssl_cert_env: true,
            ..ok_env.clone()
        },
        "SSL_CERT_FILE",
    );
    refused(
        HostedEnv {
            ca_file: Some(h.ca.clone()),
            ..ok_env.clone()
        },
        "tls_interception: refused",
    );
    let allowed = list_for(&url, "  tls_interception: allowed\n");
    let b = bind(
        &allowed,
        &HostedEnv {
            ca_file: Some(h.ca.clone()),
            ..ok_env.clone()
        },
        &h.control,
    )
    .unwrap();
    assert_eq!(
        b.extra_ca.as_ref().unwrap().1,
        hosted::sha256_bytes(pki.ca_pem.as_bytes())
    );
    // The ceiling may only be lowered.
    let ceiling = list_for(&url, "  max_data_class: internal\n");
    let err = bind(
        &ceiling,
        &HostedEnv {
            max_data_class: Some("restricted".into()),
            ..ok_env.clone()
        },
        &h.control,
    )
    .unwrap_err();
    assert!(err.contains("above"), "{err}");
    let b = bind(
        &ceiling,
        &HostedEnv {
            max_data_class: Some("public".into()),
            ..ok_env.clone()
        },
        &h.control,
    )
    .unwrap();
    assert_eq!(b.max_data_class.to_string(), "public");
    assert_eq!(
        bind(&ceiling, &ok_env, &h.control)
            .unwrap()
            .max_data_class
            .to_string(),
        "internal"
    );
    // The stop file present, or its directory not mounted.
    std::fs::write(h.control.join(hosted::STOP_FILE), "stopped for a test\n").unwrap();
    let err = bind(&list, &ok_env, &h.control).unwrap_err();
    assert!(err.contains("stopped"), "{err}");
    std::fs::remove_file(h.control.join(hosted::STOP_FILE)).unwrap();
    let err = bind(&list, &ok_env, &h.control.join("unmounted")).unwrap_err();
    assert!(err.contains("not mounted"), "{err}");
    // A name not on the list; a role label equal to the entry name.
    let mut other = Models::parse(&CONFIG.replace("owner-hosted", "someone-else"), false).unwrap();
    let err = hosted::bind(&mut other, &list, &ok_env, &h.control, build()).unwrap_err();
    assert!(
        err.contains("not on this gateway's compiled approved list"),
        "{err}"
    );
    let mut same_label = Models::parse(
        &CONFIG.replace("label: hosted-model-a", "label: owner-hosted"),
        false,
    )
    .unwrap();
    let err = hosted::bind(&mut same_label, &list, &ok_env, &h.control, build()).unwrap_err();
    assert!(err.contains("label equals"), "{err}");
}

/// The config forms `hosted` takes: one `hosted:<name>` upstream in a production config, roles
/// with labels and `host:<ref>` models, no CLOUD_TEST, no mix with local model servers, and no
/// route across the entry's models (MP9, not built).
#[test]
fn hosted_configs_take_one_form_only() {
    let token = format!("runtime={TOKEN}");
    let config = |extra: &[(&str, &str)]| {
        let token = token.clone();
        let extra: Vec<(String, String)> = extra
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(move |k| match k {
            "PMOS_MODEL_SOURCE" => Some("hosted".into()),
            "PMOS_GATEWAY_TOKENS" => Some(token.clone()),
            _ => extra.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()),
        })
        .unwrap()
    };
    let models = Models::parse(CONFIG, false).unwrap();
    assert!(config(&[]).check_source(&models).is_ok());
    let err = config(&[("PMOS_GATEWAY_CLOUD_TEST", "1")])
        .check_source(&models)
        .unwrap_err();
    assert!(err.contains("PMOS_GATEWAY_CLOUD_TEST"), "{err}");
    let production = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/gateway.yaml"),
    )
    .unwrap();
    let local = Models::parse(&production, false).unwrap();
    assert!(config(&[]).check_source(&local).is_err());
    for (bad, want) in [
        // No label.
        (CONFIG.replace(", label: hosted-model-a}", "}"), "role label"),
        // A model id in place of `host:<ref>`.
        (CONFIG.replace("'host:main'", "vendor-model"), "host:<ref>"),
        // A key file in the config.
        (
            CONFIG.replace("{h: 'hosted:owner-hosted'}", "{h: {url: 'hosted:owner-hosted', key-file: /k}}"),
            "key-file",
        ),
        // Two entry names.
        (
            CONFIG.replace(
                "upstreams: {h: 'hosted:owner-hosted'}",
                "upstreams: {h: 'hosted:owner-hosted', g: 'hosted:other'}",
            )
            .replace("extraction: {upstream: h", "extraction: {upstream: g"),
            "two",
        ),
        // Beside a local model server.
        (
            CONFIG.replace(
                "upstreams: {h: 'hosted:owner-hosted'}",
                "upstreams: {h: 'hosted:owner-hosted', l: 'http://vllm-default.pmos.internal:8000/v1'}",
            )
            .replace("extraction: {upstream: h, model: 'host:main', label: hosted-model-a}", "extraction: {upstream: l, model: m}"),
            "local",
        ),
        // `host:<ref>` on another upstream.
        (
            CONFIG.replace(
                "upstreams: {h: 'hosted:owner-hosted'}",
                "upstreams: {h: 'hosted:owner-hosted', l: 'http://vllm-default.pmos.internal:8000/v1'}",
            )
            .replace("extraction: {upstream: h", "extraction: {upstream: l"),
            "host:<ref>",
        ),
        // A route.
        (
            format!(
                "{}models:\n  a: {{upstream: h, model: 'host:main'}}\n",
                CONFIG.replace(
                    "extraction: {upstream: h, model: 'host:main', label: hosted-model-a}",
                    "extraction: {route: [a]}"
                )
            ),
            "MP9",
        ),
    ] {
        let err = Models::parse(&bad, true).unwrap_err();
        assert!(err.contains(want), "want `{want}`: {err}");
    }
}

/// `hosted` accepted on the fixture list: the call goes to the mock with the hosted key only,
/// the answer is relabelled, every answer says `hosted:owner-hosted`, and no URL, host, key or
/// model id reaches `/readyz`, `/v1/pmos/roles`, an answer, its headers or the usage log.
#[tokio::test]
async fn an_approved_entry_serves_and_names_nothing() {
    let pki = pki();
    let mock = start_mock(&pki, "ok").await;
    let h = host(&pki);
    let gw = bound_gateway(&pki, &mock, &h).await;
    // A call without X-Pmos-Run is refused under `hosted`, before anything is sent.
    let res = call(&gw, "hosted:owner-hosted", None).send().await.unwrap();
    assert_eq!(res.status(), 400);
    let v: Value = res.json().await.unwrap();
    assert_eq!(v["error"]["type"], "run_required");
    // A call naming another source is refused (X4).
    let res = call(&gw, "hosted-test", Some("run-1"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 409);
    assert!(mock.chats().is_empty(), "nothing was sent upstream");
    let res = call(&gw, "hosted:owner-hosted", Some("run-1"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["x-pmos-model-source"], "hosted:owner-hosted");
    assert_eq!(res.headers()["x-pmos-model"], "hosted-model-a");
    let headers = format!("{:?}", res.headers());
    let text = res.text().await.unwrap();
    let chats = mock.chats();
    assert_eq!(chats.len(), 1);
    assert_eq!(chats[0].method, "POST");
    assert_eq!(chats[0].path, "/v1/chat/completions");
    assert_eq!(chats[0].authorization, format!("Bearer {KEY}"));
    assert!(
        chats[0].body.contains(MODEL_ID),
        "the real id goes upstream"
    );
    assert!(
        !chats[0].body.contains(TOKEN),
        "the caller's token never goes upstream"
    );
    let answer: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(answer["model"], "hosted-model-a");
    assert_eq!(answer["choices"][0]["message"]["content"], "hosted ok");
    let (status, ready) = get_json(format!("{}/readyz", gw.base), false).await;
    assert_eq!(status, 200, "{ready}");
    assert_eq!(ready["model_source"], "hosted:owner-hosted");
    assert_eq!(ready["hosted_name"], "owner-hosted");
    assert_eq!(ready["hosted"], "ok");
    assert_eq!(ready["approved_list"], "release");
    assert_eq!(ready["approved_list_sha256"].as_str().unwrap().len(), 64);
    assert_eq!(ready["commit"], build().commit.unwrap());
    assert_eq!(ready["merged_commit"], build().merged_commit.unwrap());
    assert_eq!(ready["max_data_class"], "restricted");
    assert_eq!(ready["tls_interception"], "refused");
    assert_eq!(ready["upstreams"]["default"]["upstream"], "hosted");
    let (_, roles) = get_json(format!("{}/v1/pmos/roles", gw.base), true).await;
    assert_eq!(roles["model_source"], "hosted:owner-hosted");
    assert_eq!(roles["hosted_name"], "owner-hosted");
    assert_eq!(roles["roles"][0]["model"], "hosted-model-a");
    assert_eq!(roles["roles"][0]["model_sha256"], sha256_hex(MODEL_ID));
    assert_eq!(roles["roles"][0]["upstream"], "hosted");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let usage = gw.usage.recent();
    let line = usage
        .iter()
        .find(|e| e.run.as_deref() == Some("run-1") && e.status == 200)
        .unwrap();
    assert_eq!(line.upstream, "hosted:owner-hosted");
    assert_eq!(line.model_source, Some("hosted:owner-hosted"));
    assert_eq!(line.model, "hosted-model-a");
    let logged = serde_json::to_string(&usage).unwrap();
    for (what, s) in [
        ("answer", text.as_str()),
        ("headers", headers.as_str()),
        ("readyz", &ready.to_string()),
        ("roles", &roles.to_string()),
        ("usage", logged.as_str()),
    ] {
        for secret in [HOST, MODEL_ID, KEY, MARKER, "127.0.0.1"] {
            assert!(!s.contains(secret), "`{secret}` in {what}: {s}");
        }
    }
    gw.stop.cancel();
    mock.stop.cancel();
}

/// V3 under `hosted`: a provider's `401` body never reaches the caller; no redirect is followed
/// (a `307` is a `502`, its `Location` and body dropped, nothing sent on).
#[tokio::test]
async fn provider_errors_and_redirects_reach_no_caller() {
    let pki = pki();
    let mock = start_mock(&pki, "refuse").await;
    let h = host(&pki);
    let gw = bound_gateway(&pki, &mock, &h).await;
    let res = call(&gw, "hosted:owner-hosted", Some("run-401"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    let text = res.text().await.unwrap();
    assert!(!text.contains(MARKER), "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        v["error"],
        json!({"type": "auth", "status": 401,
        "message": "the model server answered 401 (auth)"})
    );
    *mock.mode.lock().unwrap() = "redirect";
    let res = call(&gw, "hosted:owner-hosted", Some("run-307"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 502);
    assert!(res.headers().get("location").is_none());
    let text = res.text().await.unwrap();
    assert!(
        !text.contains(MARKER) && !text.contains("elsewhere"),
        "{text}"
    );
    assert_eq!(mock.chats().len(), 2, "one call each, nothing followed");
    gw.stop.cancel();
    mock.stop.cancel();
}

/// The kill switch (V7): the stop file, made while the gateway runs, stops every hosted call
/// before anything is sent (`503 hosted_stopped`) and turns `/readyz` to `503 stopped`; removed,
/// calls go again. An emptied key file answers `503 hosted_key_missing`; a new key is read on
/// the next call (and on `SIGHUP`, `KeyStore::reload`).
#[tokio::test]
async fn the_stop_file_and_the_key_are_read_before_every_call() {
    use std::os::unix::fs::PermissionsExt;
    let pki = pki();
    let mock = start_mock(&pki, "ok").await;
    let h = host(&pki);
    let gw = bound_gateway(&pki, &mock, &h).await;
    assert_eq!(
        call(&gw, "hosted:owner-hosted", Some("r"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    std::fs::write(h.control.join(hosted::STOP_FILE), "2026-11-24 test stop\n").unwrap();
    let res = call(&gw, "hosted:owner-hosted", Some("r"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 503);
    let v: Value = res.json().await.unwrap();
    assert_eq!(v["error"]["type"], "hosted_stopped");
    let (status, ready) = get_json(format!("{}/readyz", gw.base), false).await;
    assert_eq!((status, ready["hosted"].as_str()), (503, Some("stopped")));
    assert_eq!(mock.chats().len(), 1, "nothing sent while stopped");
    std::fs::remove_file(h.control.join(hosted::STOP_FILE)).unwrap();
    assert_eq!(
        call(&gw, "hosted:owner-hosted", Some("r"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    // The key emptied: nothing sent, 503.
    std::fs::write(&h.key, "").unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let res = call(&gw, "hosted:owner-hosted", Some("r"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 503);
    let v: Value = res.json().await.unwrap();
    assert_eq!(v["error"]["type"], "hosted_key_missing");
    assert_eq!(mock.chats().len(), 2);
    // A new key: read on the next call (once a second at most), the old one never kept.
    std::fs::write(&h.key, "rotated-hosted-key-0000000000000000\n").unwrap();
    std::fs::set_permissions(&h.key, std::fs::Permissions::from_mode(0o600)).unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(
        call(&gw, "hosted:owner-hosted", Some("r"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let chats = mock.chats();
    assert_eq!(
        chats.last().unwrap().authorization,
        "Bearer rotated-hosted-key-0000000000000000"
    );
    gw.stop.cancel();
    mock.stop.cancel();
}

/// Which roots the hosted client trusts: with Mozilla's compiled-in roots only, the mock's test
/// CA is not trusted (a `502`, nothing answered); with `tls_interception: allowed`, exactly the
/// mounted bundle is added and the call goes through.
#[tokio::test]
async fn the_hosted_client_trusts_only_its_compiled_roots_and_an_allowed_bundle() {
    let pki = pki();
    let mock = start_mock(&pki, "ok").await;
    let h = host(&pki);
    let url = mock.url();
    let list = list_for(&url, "");
    let mut models = Models::parse(CONFIG, false).unwrap();
    let binding =
        hosted::bind(&mut models, &list, &env_for(&h, &url), &h.control, build()).unwrap();
    let compiled = HostedNet {
        roots: None,
        ..test_net(&pki)
    };
    let gw = gateway(models, binding, compiled.clone()).await;
    let res = call(&gw, "hosted:owner-hosted", Some("r"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 502);
    assert!(
        mock.chats().is_empty(),
        "no request completed over an untrusted connection"
    );
    gw.stop.cancel();
    let allowed = list_for(&url, "  tls_interception: allowed\n");
    let mut models = Models::parse(CONFIG, false).unwrap();
    let env = HostedEnv {
        ca_file: Some(h.ca.clone()),
        ..env_for(&h, &url)
    };
    let binding = hosted::bind(&mut models, &allowed, &env, &h.control, build()).unwrap();
    let gw = gateway(models, binding, compiled).await;
    let res = call(&gw, "hosted:owner-hosted", Some("r"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let (_, ready) = get_json(format!("{}/readyz", gw.base), false).await;
    assert_eq!(ready["tls_interception"], "allowed");
    assert_eq!(
        ready["ca_bundle_sha256"],
        hosted::sha256_bytes(pki.ca_pem.as_bytes())
    );
    gw.stop.cancel();
    mock.stop.cancel();
}

/// The hosted resolver, at every connection: the mock's name answered with loopback is refused
/// by the gateway's rule (a `502`, nothing sent), and the warning names no address.
#[tokio::test]
async fn a_private_answer_for_the_hosted_host_is_refused() {
    let pki = pki();
    let mock = start_mock(&pki, "ok").await;
    let h = host(&pki);
    let url = mock.url();
    let list = list_for(&url, "");
    let mut models = Models::parse(CONFIG, false).unwrap();
    let binding =
        hosted::bind(&mut models, &list, &env_for(&h, &url), &h.control, build()).unwrap();
    let strict = HostedNet {
        allow_private: false,
        ..test_net(&pki)
    };
    let gw = gateway(models, binding, strict).await;
    let res = call(&gw, "hosted:owner-hosted", Some("r"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 502);
    let text = res.text().await.unwrap();
    assert_names_nothing(&text);
    assert!(mock.chats().is_empty());
    gw.stop.cancel();
    mock.stop.cancel();
}

/// `/readyz` probes the hosted model's `/models` (with its key) at most once per 30 seconds,
/// however many unauthenticated callers ask.
#[tokio::test]
async fn readiness_probes_the_hosted_model_at_most_every_30_seconds() {
    let pki = pki();
    let mock = start_mock(&pki, "ok").await;
    let h = host(&pki);
    let url = mock.url();
    let list = list_for(&url, "");
    let mut models = Models::parse(CONFIG, false).unwrap();
    let binding =
        hosted::bind(&mut models, &list, &env_for(&h, &url), &h.control, build()).unwrap();
    let gw = gateway_with(
        models,
        binding,
        test_net(&pki),
        Timeouts {
            ready_cache: Duration::ZERO,
            ..Default::default()
        },
    )
    .await;
    let mut burst = Vec::new();
    for _ in 0..15 {
        burst.push(get_json(format!("{}/readyz", gw.base), false));
    }
    for (status, _) in futures_util::future::join_all(burst).await {
        assert_eq!(status, 200);
    }
    assert_eq!(mock.probes(), 1, "one probe in all");
    let probe = mock
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|s| s.path.ends_with("/models"))
        .cloned()
        .unwrap();
    assert_eq!(probe.authorization, format!("Bearer {KEY}"));
    gw.stop.cancel();
    mock.stop.cancel();
}

/// `pmos-gateway --digest-from-stdin` and `--model-digest-from-stdin`: the gateway's own
/// normalisation from standard input, the digest only on standard output, the gateway's
/// refusal otherwise.
#[test]
fn the_digest_commands_read_standard_input_only() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let bin = env!("CARGO_BIN_EXE_pmos-gateway");
    let run = |flag: &str, input: &str| {
        let mut child = Command::new(bin)
            .arg(flag)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };
    let (ok, out, _) = run(
        "--digest-from-stdin",
        &format!("https://{}.:443/v1/\n", HOST.to_uppercase()),
    );
    assert!(ok);
    assert_eq!(out.trim(), sha256_hex(&format!("https://{HOST}/v1")));
    let (ok, out, err) = run("--digest-from-stdin", "https://127.0.0.1/v1\n");
    assert!(!ok && out.is_empty());
    assert!(
        err.contains("IP address") && !err.contains("127.0.0.1"),
        "{err}"
    );
    let (ok, out, _) = run("--model-digest-from-stdin", &format!("{MODEL_ID}\n"));
    assert!(ok);
    assert_eq!(out.trim(), sha256_hex(MODEL_ID));
    let (ok, _, _) = run("--model-digest-from-stdin", "a b\n");
    assert!(!ok);
}
