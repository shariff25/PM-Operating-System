//! The collector against a stand-in stream (`docs/openshell-prover-and-sandbox-audit-spec.md`
//! §B.9): OTLP/HTTP payloads built here from OCSF fixtures (OpenShell's documented examples and
//! PM OS's own denials from `policies/redteam/isolation.sh`), posted to the collector's real
//! router, and a stand-in tool server that records what it is sent. Nothing here talks to
//! OpenShell.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post as route_post;
use axum::{Json, Router};
use chrono::{Duration, Utc};
use pmos_sandbox_audit::app::{Collector, Config};
use pmos_sandbox_audit::otlp::{encode, LogRecord};
use pmos_sandbox_audit::wire::{Batch, Heartbeat, Kind, Note};
use serde_json::{json, Value};

const OTLP_TOKEN: &str = "otlp-token-for-tests-0123456789abcdef";
const TS_TOKEN: &str = "toolserver-token-for-tests-0123456789ab";

#[derive(Clone, Default)]
struct Seen {
    batches: Arc<Mutex<Vec<(String, Value)>>>,
    beats: Arc<Mutex<Vec<Heartbeat>>>,
    answer: Arc<Mutex<u16>>,
}

async fn fake_batch(
    State(s): State<Seen>,
    headers: axum::http::HeaderMap,
    Json(b): Json<Value>,
) -> StatusCode {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    s.batches.lock().unwrap().push((auth, b));
    StatusCode::from_u16(*s.answer.lock().unwrap()).unwrap()
}

async fn fake_beat(State(s): State<Seen>, Json(h): Json<Heartbeat>) -> StatusCode {
    s.beats.lock().unwrap().push(h);
    StatusCode::OK
}

struct Rig {
    url: String,
    collector: Collector,
    seen: Seen,
    _dir: tempfile::TempDir,
}

async fn rig(spool_max: u64) -> Rig {
    let seen = Seen::default();
    *seen.answer.lock().unwrap() = 200;
    let ts = Router::new()
        .route("/internal/sandbox-audit/batch", route_post(fake_batch))
        .route("/internal/sandbox-audit/heartbeat", route_post(fake_beat))
        .with_state(seen.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ts_url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, ts).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let config = Config::from_lookup(|k| match k {
        "PMOS_SANDBOX_AUDIT_OTLP_TOKEN" => Some(OTLP_TOKEN.into()),
        "PMOS_SANDBOX_AUDIT_TOKEN" => Some(TS_TOKEN.into()),
        "PMOS_TOOLSERVER_URL" => Some(ts_url.clone()),
        "PMOS_SANDBOX_AUDIT_SPOOL" => Some(dir.path().join("spool").display().to_string()),
        "PMOS_SANDBOX_AUDIT_SPOOL_MAX_BYTES" => Some(spool_max.to_string()),
        "PMOS_SANDBOX_AUDIT_FORMAT_VERIFIED" => Some("0.1.9".into()),
        _ => None,
    })
    .unwrap();
    let collector = Collector::new(config).unwrap();
    let app = collector.router();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    Rig {
        url,
        collector,
        seen,
        _dir: dir,
    }
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// OpenShell's documented deny (`OCSF NET:OPEN [MED] DENIED /usr/bin/curl(64) -> httpbin.org:443
/// [policy:- engine:opa] [reason:no matching policy]`), with everything that must be dropped.
fn documented_deny(uid: &str) -> Value {
    json!({
        "class_uid": 4001, "activity_name": "Open", "action": "Denied", "disposition": "Blocked",
        "time": now_ms(), "severity": "Medium",
        "status_detail": "no matching policy",
        "dst_endpoint": {"domain": "httpbin.org", "ip": "54.0.0.1", "port": 443},
        "src_endpoint": {"ip": "10.88.0.4", "port": 50123},
        "actor": {"process": {"name": "/usr/bin/curl", "pid": 64,
                  "cmd_line": "curl -H 'Authorization: Bearer FAKESECRET123' https://httpbin.org/get?api_key=FAKESECRET123"}},
        "firewall_rule": {"name": "-", "type": "opa"},
        "metadata": {"uid": uid, "product": {"version": "0.1.9"}, "correlation_uid": "corr-1"},
        "device": {"container": {"name": "pmos-docs", "uid": "ctr-docs"}},
        "message": "OCSF NET:OPEN [MED] DENIED /usr/bin/curl(64) -> httpbin.org:443 FAKESECRET123",
        "http_request": {"http_method": "GET", "url": {"url_string": "https://user:FAKESECRET123@httpbin.org/get?api_key=FAKESECRET123#frag", "path": "/get?api_key=FAKESECRET123"},
                         "http_headers": [{"name": "Authorization", "value": "Bearer FAKESECRET123"}]},
        "http_response": {"body": "FAKESECRET123"}
    })
}

/// PM OS's own: an HTTP deny on the tool server's MCP route (isolation.sh, RT-11).
fn pmos_http_deny(uid: &str) -> Value {
    json!({
        "class_uid": 4002, "action": "Denied", "time": now_ms(),
        "status_detail": "l7 deny",
        "dst_endpoint": {"domain": "toolserver.pmos.internal", "ip": "10.201.77.10", "port": 8710},
        "http_request": {"http_method": "POST", "url": {"path": "/mcp"}},
        "actor": {"process": {"name": "/usr/local/bin/node"}},
        "firewall_rule": {"name": "pmos_toolserver", "type": "opa"},
        "metadata": {"uid": uid, "product": {"version": "0.1.9"}},
        "device": {"container": {"name": "pmos-pm-ana", "uid": "ctr-ana"}}
    })
}

fn allow(uid: &str) -> Value {
    json!({
        "class_uid": 4002, "action": "Allowed", "time": now_ms(),
        "dst_endpoint": {"domain": "gateway.pmos.internal", "ip": "10.201.77.11", "port": 8740},
        "http_request": {"http_method": "POST", "url": {"path": "/v1/chat/completions"}},
        "firewall_rule": {"name": "pmos_gateway", "type": "opa"},
        "metadata": {"uid": uid, "product": {"version": "0.1.9"}},
        "device": {"container": {"name": "pmos-docs"}}
    })
}

fn record(event: Value) -> LogRecord {
    let uid = event["metadata"]["uid"].as_str().unwrap_or("").to_string();
    LogRecord {
        time_unix_nano: 1,
        event_name: None,
        body: Some(Value::String(event.to_string())),
        attributes: [("ocsf.metadata.uid".to_string(), json!(uid))].into(),
        resource: BTreeMap::new(),
    }
}

fn payload(events: Vec<Value>) -> Vec<u8> {
    let res: BTreeMap<String, Value> = [("openshell.sandbox.id".to_string(), json!("sb-1"))].into();
    let records: Vec<LogRecord> = events.into_iter().map(record).collect();
    encode(&records, &res)
}

async fn post(r: &Rig, body: Vec<u8>, token: Option<&str>, ct: &str) -> u16 {
    let mut req = reqwest::Client::new()
        .post(format!("{}/v1/logs", r.url))
        .header("content-type", ct)
        .body(body);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    req.send().await.unwrap().status().as_u16()
}

async fn send_all(r: &Rig) {
    while r.collector.send_one().await.unwrap() {}
}

fn sent_events(r: &Rig) -> Vec<Value> {
    r.seen
        .batches
        .lock()
        .unwrap()
        .iter()
        .flat_map(|(_, b)| b["events"].as_array().cloned().unwrap_or_default())
        .collect()
}

#[tokio::test]
async fn only_openshells_header_token_and_protobuf_are_taken() {
    let r = rig(1 << 20).await;
    let body = payload(vec![documented_deny("u-1")]);
    assert_eq!(
        post(&r, body.clone(), None, "application/x-protobuf").await,
        401
    );
    assert_eq!(
        post(
            &r,
            body.clone(),
            Some("wrong-token-0123456789abcdef-0123"),
            "application/x-protobuf"
        )
        .await,
        401
    );
    assert_eq!(
        post(&r, body.clone(), Some(TS_TOKEN), "application/x-protobuf").await,
        401,
        "the tool server's token isn't OpenShell's"
    );
    assert_eq!(
        post(&r, body.clone(), Some(OTLP_TOKEN), "application/json").await,
        415
    );
    let gz = reqwest::Client::new()
        .post(format!("{}/v1/logs", r.url))
        .header("content-type", "application/x-protobuf")
        .header("content-encoding", "gzip")
        .bearer_auth(OTLP_TOKEN)
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(gz.status().as_u16(), 415);
    assert_eq!(
        post(
            &r,
            vec![0x0a, 0xff],
            Some(OTLP_TOKEN),
            "application/x-protobuf"
        )
        .await,
        400
    );
    // Only /v1/logs.
    let other = reqwest::Client::new()
        .post(format!("{}/v1/traces", r.url))
        .bearer_auth(OTLP_TOKEN)
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(other.status().as_u16(), 404);
    assert!(
        r.collector.inner.lock().unwrap().spool.is_empty().unwrap(),
        "nothing refused was spooled"
    );
    assert_eq!(
        post(&r, body, Some(OTLP_TOKEN), "application/x-protobuf").await,
        200
    );
}

#[tokio::test]
async fn kept_fields_are_the_allowlist_and_no_secret_or_payload_gets_through() {
    let r = rig(1 << 20).await;
    let mut noisy = documented_deny("u-2");
    noisy["dst_endpoint"]["domain"] = json!(format!(
        "<script>alert(1)</script>\n\u{1b}[31m{}é",
        "h".repeat(10_000)
    ));
    noisy["actor"]["process"]["name"] = json!("bad\nname\u{1b}[0m");
    let body = payload(vec![documented_deny("u-1"), pmos_http_deny("u-3"), noisy]);
    assert_eq!(
        post(&r, body, Some(OTLP_TOKEN), "application/x-protobuf").await,
        200
    );
    send_all(&r).await;
    let batches = r.seen.batches.lock().unwrap().clone();
    assert_eq!(
        batches[0].0,
        format!("Bearer {TS_TOKEN}"),
        "its own token to the tool server"
    );
    let all =
        serde_json::to_string(&batches.iter().map(|(_, b)| b.clone()).collect::<Vec<_>>()).unwrap();
    for gone in [
        "FAKESECRET123",
        "cmd_line",
        "message",
        "50123",
        "10.88.0.4",
        "api_key",
        "frag",
        "Authorization",
        "body",
        "\\n",
        "\\u001b",
        "é",
        "OCSF NET:OPEN",
    ] {
        assert!(
            !all.contains(gone),
            "`{gone}` reached the tool server: {all}"
        );
    }
    let events = sent_events(&r);
    let first = &events[0];
    assert_eq!(first["kind"], "deny");
    assert_eq!(first["dest_host"], "httpbin.org");
    assert_eq!(first["dest_port"], 443);
    assert_eq!(first["http_path"], "/get");
    assert_eq!(first["http_method"], "GET");
    assert_eq!(first["reason"], "no_matching_policy");
    assert_eq!(first["sandbox"], "pmos-docs");
    assert_eq!(first["os_event_uid"], "u-1");
    assert_eq!(first["seq"], 1);
    let http = &events[1];
    assert_eq!(
        (
            http["rule"].as_str(),
            http["reason"].as_str(),
            http["http_path"].as_str()
        ),
        (Some("pmos_toolserver"), Some("l7_deny"), Some("/mcp"))
    );
    let noisy = &events[2];
    let host = noisy["dest_host"].as_str().unwrap();
    assert!(
        host.len() <= 253 && host.bytes().all(|b| (0x20..=0x7e).contains(&b)),
        "{host}"
    );
    assert!(!noisy["process"].as_str().unwrap().contains('\n'));
    // Every key the collector sends is one the tool server's batch shape knows.
    let allowed = [
        "seq",
        "kind",
        "at",
        "sandbox",
        "sandbox_uid",
        "rule",
        "reason",
        "dest_host",
        "dest_ip",
        "dest_port",
        "http_method",
        "http_path",
        "process",
        "os_event_uid",
        "os_correlation_uid",
        "os_version",
        "policy_hash",
        "count",
        "since",
        "rules",
        "note",
    ];
    for e in &events {
        for k in e.as_object().unwrap().keys() {
            assert!(allowed.contains(&k.as_str()), "{k}");
        }
    }
}

#[tokio::test]
async fn duplicates_by_uid_are_sent_once() {
    let r = rig(1 << 20).await;
    let body = payload(vec![documented_deny("u-1"), documented_deny("u-1")]);
    assert_eq!(
        post(&r, body.clone(), Some(OTLP_TOKEN), "application/x-protobuf").await,
        200
    );
    // OpenShell redelivers the whole request (at least once).
    assert_eq!(
        post(&r, body, Some(OTLP_TOKEN), "application/x-protobuf").await,
        200
    );
    send_all(&r).await;
    let denies = sent_events(&r)
        .into_iter()
        .filter(|e| e["kind"] == "deny")
        .count();
    assert_eq!(denies, 1);
}

#[tokio::test]
async fn a_thousand_denies_in_a_minute_are_a_hundred_rows_and_a_counted_summary() {
    let r = rig(16 << 20).await;
    let events: Vec<Value> = (0..1000)
        .map(|i| documented_deny(&format!("flood-{i}")))
        .collect();
    assert_eq!(
        post(
            &r,
            payload(events),
            Some(OTLP_TOKEN),
            "application/x-protobuf"
        )
        .await,
        200
    );
    r.collector
        .flush_at(Utc::now() + Duration::seconds(61), false)
        .unwrap();
    send_all(&r).await;
    let sent = sent_events(&r);
    let rows = sent.iter().filter(|e| e["kind"] == "deny").count();
    let summaries: Vec<&Value> = sent
        .iter()
        .filter(|e| e["kind"] == "deny_summary")
        .collect();
    assert_eq!(rows, 100);
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0]["count"], 900);
    // Sequence numbers run on with no hole across batches.
    let seqs: Vec<u64> = sent.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
    assert_eq!(seqs, (1..=seqs.len() as u64).collect::<Vec<_>>());
    // No batch is over the tool server's limits.
    for (_, b) in r.seen.batches.lock().unwrap().iter() {
        assert!(b["events"].as_array().unwrap().len() <= 500);
        assert!(serde_json::to_vec(b).unwrap().len() <= 256 * 1024);
    }
}

#[tokio::test]
async fn allows_are_summed_per_window() {
    let r = rig(1 << 20).await;
    let events: Vec<Value> = (0..40).map(|i| allow(&format!("a-{i}"))).collect();
    assert_eq!(
        post(
            &r,
            payload(events),
            Some(OTLP_TOKEN),
            "application/x-protobuf"
        )
        .await,
        200
    );
    send_all(&r).await;
    assert!(sent_events(&r).is_empty(), "no row per allow");
    r.collector
        .flush_at(Utc::now() + Duration::seconds(301), false)
        .unwrap();
    send_all(&r).await;
    let sent = sent_events(&r);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["kind"], "allow_summary");
    assert_eq!(sent[0]["count"], 40);
    assert!(sent[0].get("http_path").is_none());
}

#[tokio::test]
async fn gaps_unknown_formats_and_unparsed_records_are_sent_not_dropped() {
    let r = rig(1 << 20).await;
    let mut newer = documented_deny("u-9");
    newer["metadata"]["product"]["version"] = json!("0.2.0");
    let gap = LogRecord {
        time_unix_nano: 1,
        event_name: Some("openshell.ocsf.gap".into()),
        body: None,
        attributes: [
            ("openshell.ocsf.gap.dropped".to_string(), json!(17)),
            (
                "openshell.ocsf.gap.since_unix_ms".to_string(),
                json!(now_ms() - 60_000),
            ),
        ]
        .into(),
        resource: BTreeMap::new(),
    };
    let junk = LogRecord {
        time_unix_nano: 1,
        body: Some(json!("not the documented JSON")),
        ..LogRecord::default()
    };
    let mut records = vec![record(newer), gap, junk];
    records.push(record(
        json!({"class_uid": 4001, "metadata": {"uid": "u-10"}}),
    ));
    assert_eq!(
        post(
            &r,
            encode(&records, &BTreeMap::new()),
            Some(OTLP_TOKEN),
            "application/x-protobuf"
        )
        .await,
        200
    );
    r.collector.flush_at(Utc::now(), false).unwrap();
    send_all(&r).await;
    let sent = sent_events(&r);
    let kind = |k: &str| sent.iter().filter(|e| e["kind"] == k).collect::<Vec<_>>();
    assert_eq!(kind("source_gap")[0]["count"], 17);
    assert!(kind("unparsed")
        .iter()
        .any(|e| e["note"] == json!(Note::FormatUnverified) && e["os_version"] == "0.2.0"));
    assert!(kind("unparsed").iter().any(|e| e["count"] == 2), "{sent:?}");
}

#[tokio::test]
async fn the_spool_holds_until_the_tool_server_takes_it_and_a_full_spool_is_503() {
    let r = rig(1 << 20).await;
    *r.seen.answer.lock().unwrap() = 503;
    assert_eq!(
        post(
            &r,
            payload(vec![documented_deny("u-1")]),
            Some(OTLP_TOKEN),
            "application/x-protobuf"
        )
        .await,
        200
    );
    assert!(r.collector.send_one().await.is_err());
    assert_eq!(
        r.collector.inner.lock().unwrap().spool.len().unwrap(),
        1,
        "kept for the next try"
    );
    *r.seen.answer.lock().unwrap() = 200;
    send_all(&r).await;
    assert!(r.collector.inner.lock().unwrap().spool.is_empty().unwrap());
    assert_eq!(r.collector.inner.lock().unwrap().acked_seq, 1);
    r.collector.heartbeat().await.unwrap();
    let beats = r.seen.beats.lock().unwrap().clone();
    assert_eq!(beats[0].acked_seq, 1);
    assert!(beats[0].collector_run.starts_with('r') && beats[0].collector_run.len() == 25);
    // A refused batch is set aside, never retried forever.
    *r.seen.answer.lock().unwrap() = 400;
    assert_eq!(
        post(
            &r,
            payload(vec![documented_deny("u-2")]),
            Some(OTLP_TOKEN),
            "application/x-protobuf"
        )
        .await,
        200
    );
    assert!(r.collector.send_one().await.unwrap());
    assert!(r.collector.inner.lock().unwrap().spool.is_empty().unwrap());

    let tiny = rig(200).await;
    *tiny.seen.answer.lock().unwrap() = 503;
    let status = post(
        &tiny,
        payload(vec![documented_deny("u-1"), documented_deny("u-2")]),
        Some(OTLP_TOKEN),
        "application/x-protobuf",
    )
    .await;
    assert_eq!(status, 503, "a full spool tells OpenShell to retry");
    assert!(tiny
        .collector
        .inner
        .lock()
        .unwrap()
        .spool
        .is_empty()
        .unwrap());
    // Nothing was counted: the retry, with room, is taken whole.
    assert_eq!(tiny.collector.inner.lock().unwrap().next_seq, 1);
}

#[test]
fn the_configuration_refuses_weak_or_shared_tokens() {
    let base = |k: &str| match k {
        "PMOS_SANDBOX_AUDIT_OTLP_TOKEN" => Some(OTLP_TOKEN.to_string()),
        "PMOS_SANDBOX_AUDIT_TOKEN" => Some(TS_TOKEN.to_string()),
        _ => None,
    };
    let c = Config::from_lookup(base).unwrap();
    assert_eq!(c.bind.to_string(), "127.0.0.1:8780", "loopback by default");
    assert!(
        !format!("{c:?}").contains(OTLP_TOKEN),
        "the tokens never print"
    );
    assert!(Config::from_lookup(|k| if k == "PMOS_SANDBOX_AUDIT_TOKEN" {
        Some("short".into())
    } else {
        base(k)
    })
    .is_err());
    assert!(Config::from_lookup(|k| if k == "PMOS_SANDBOX_AUDIT_TOKEN" {
        Some(OTLP_TOKEN.into())
    } else {
        base(k)
    })
    .is_err());
    assert!(
        Config::from_lookup(|k| if k == "PMOS_SANDBOX_AUDIT_OTLP_TOKEN" {
            None
        } else {
            base(k)
        })
        .is_err()
    );
    // The headers file OpenShell reads, as a file.
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("otlp-ocsf.headers");
    std::fs::write(&f, format!("authorization=Bearer {OTLP_TOKEN}\n")).unwrap();
    let path = f.display().to_string();
    let c = Config::from_lookup(|k| match k {
        "PMOS_SANDBOX_AUDIT_OTLP_TOKEN_FILE" => Some(path.clone()),
        "PMOS_SANDBOX_AUDIT_OTLP_TOKEN" => None,
        k => base(k),
    })
    .unwrap();
    assert_eq!(c.otlp_token, OTLP_TOKEN);
    let _ = Kind::Deny;
    let _ = Batch {
        collector_run: String::new(),
        events: vec![],
    };
}
