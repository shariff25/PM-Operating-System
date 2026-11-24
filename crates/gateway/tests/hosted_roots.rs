//! Slice H12 (spec §3.6, "Which roots the hosted client trusts", r7): a CA named by
//! `SSL_CERT_FILE` (as a file dropped into the system store would be) is trusted by a client on
//! the system store, and never by the hosted client, which trusts only its compiled-in roots.
//!
//! One test in its own binary, because it sets `SSL_CERT_FILE` for the whole process before any
//! client reads the system store.

use std::net::SocketAddr;

#[path = "support/hosted_mock.rs"]
mod hosted_mock;
use hosted_mock::{pki, start_mock, HOST};

#[tokio::test]
async fn a_ca_in_the_system_store_is_never_trusted_by_the_hosted_client() {
    let pki = pki();
    let dir = tempfile::tempdir().unwrap();
    let ca = dir.path().join("system-ca.pem");
    std::fs::write(&ca, &pki.ca_pem).unwrap();
    // Before any client exists in this process, so the system store is read with it.
    std::env::set_var("SSL_CERT_FILE", &ca);
    let mock = start_mock(&pki, "ok").await;
    let addr: SocketAddr = format!("127.0.0.1:{}", mock.port).parse().unwrap();
    let url = format!("{}/models", mock.url());

    // A client on the system store trusts the CA: the precondition, so the refusal below is
    // the hosted client's own doing, not a broken mock.
    let system = reqwest::Client::builder()
        .no_proxy()
        .resolve(HOST, addr)
        .build()
        .unwrap();
    let res = system.get(&url).send().await;
    assert!(
        res.is_ok_and(|r| r.status().is_success()),
        "a system-store client trusts SSL_CERT_FILE's CA"
    );

    // The hosted client, built as the gateway builds it (compiled-in roots), refuses it.
    let binding = pmos_gateway::hosted::Binding {
        name: "owner-hosted".into(),
        list_kind: pmos_gateway::hosted::ListKind::Release,
        list_sha256: String::new(),
        url: mock.url(),
        host: HOST.into(),
        key_file: dir.path().join("unused"),
        max_data_class: pmos_contract::model::Sensitivity::Restricted,
        tls_interception: pmos_gateway::hosted::TlsInterception::Refused,
        extra_ca: None,
        control_dir: dir.path().to_path_buf(),
        build: Default::default(),
    };
    let lookup: pmos_gateway::hosted::Lookup =
        std::sync::Arc::new(|_| Box::pin(async { Ok(vec!["127.0.0.1".parse().unwrap()]) }));
    let net = pmos_gateway::hosted::HostedNet {
        roots: None,
        lookup: Some(lookup),
        allow_private: true,
        no_proxy: true,
    };
    let hosted = pmos_gateway::hosted::client(&binding, &net, &Default::default(), true);
    let before = mock.probes();
    let err = hosted.get(&url).send().await.unwrap_err();
    assert!(
        err.is_connect() || err.is_request(),
        "{}",
        err.without_url()
    );
    assert_eq!(mock.probes(), before, "no request reached the mock");
    mock.stop.cancel();
}
