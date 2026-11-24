//! A TLS mock of the hosted model (slice H12) for tests/hosted.rs and tests/hosted_roots.rs:
//! a test CA and a server certificate made at run time, served over rustls. Fake values only.
#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

pub const HOST: &str = "models.hosted-mock.test";
pub const MODEL_ID: &str = "vendor-model-id-marker-7781";
pub const MARKER: &str = "acct-7781-vendorname-sk-masked";

pub struct Pki {
    pub ca_pem: String,
    pub server: rustls::ServerConfig,
}

/// A test CA and a server certificate for [`HOST`] signed by it.
pub fn pki() -> Pki {
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut ca = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca.distinguished_name
        .push(rcgen::DnType::CommonName, "PM OS hosted mock test CA");
    let ca_cert = ca.self_signed(&ca_key).unwrap();
    let leaf_key = rcgen::KeyPair::generate().unwrap();
    let leaf = rcgen::CertificateParams::new(vec![HOST.to_string()]).unwrap();
    let leaf_cert = leaf.signed_by(&leaf_key, &ca_cert, &ca_key).unwrap();
    let server = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![leaf_cert.der().clone()],
        rustls::pki_types::PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into()),
    )
    .unwrap();
    Pki {
        ca_pem: ca_cert.pem(),
        server,
    }
}

/// One request the mock saw.
#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub authorization: String,
    pub body: String,
}

#[derive(Clone)]
pub struct Mock {
    pub port: u16,
    pub seen: Arc<Mutex<Vec<Seen>>>,
    pub mode: Arc<Mutex<&'static str>>,
    pub stop: CancellationToken,
}

impl Mock {
    pub fn chats(&self) -> Vec<Seen> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.path.ends_with("/chat/completions"))
            .cloned()
            .collect()
    }
    pub fn probes(&self) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.path.ends_with("/models"))
            .count()
    }
    pub fn url(&self) -> String {
        format!("https://{HOST}:{}/v1", self.port)
    }
}

pub async fn start_mock(pki: &Pki, mode: &'static str) -> Mock {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(pki.server.clone()));
    let mock = Mock {
        port,
        seen: Arc::default(),
        mode: Arc::new(Mutex::new(mode)),
        stop: CancellationToken::new(),
    };
    let m = mock.clone();
    tokio::spawn(async move {
        loop {
            let (tcp, _) = tokio::select! {
                _ = m.stop.cancelled() => break,
                a = listener.accept() => match a { Ok(a) => a, Err(_) => break },
            };
            let acceptor = acceptor.clone();
            let m = m.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let head_end = loop {
                    let Ok(n) = tls.read(&mut chunk).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let len: usize = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse().unwrap_or(0))
                    })
                    .unwrap_or(0);
                while buf.len() < head_end + len {
                    let Ok(n) = tls.read(&mut chunk).await else {
                        return;
                    };
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let mut first = head.lines().next().unwrap_or_default().split(' ');
                let method = first.next().unwrap_or_default().to_string();
                let path = first.next().unwrap_or_default().to_string();
                let authorization = head
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("authorization")
                            .then(|| v.trim().to_string())
                    })
                    .unwrap_or_default();
                let body = String::from_utf8_lossy(&buf[head_end..]).to_string();
                m.seen.lock().unwrap().push(Seen {
                    method,
                    path: path.clone(),
                    authorization,
                    body,
                });
                let mode = *m.mode.lock().unwrap();
                let (status, extra, text) = if path.ends_with("/models") {
                    ("200 OK", String::new(), json!({"data": []}).to_string())
                } else {
                    match mode {
                        "refuse" => (
                            "401 Unauthorized",
                            String::new(),
                            json!({"error": {"message": format!("bad key for {MARKER}")}})
                                .to_string(),
                        ),
                        "redirect" => (
                            "307 Temporary Redirect",
                            format!("location: https://elsewhere.hosted-mock.test/{MARKER}\r\n"),
                            format!("go to {MARKER}"),
                        ),
                        _ => (
                            "200 OK",
                            String::new(),
                            json!({"id": "x", "object": "chat.completion", "model": MODEL_ID,
                                   "system_fingerprint": MARKER,
                                   "choices": [{"index": 0, "message":
                                       {"role": "assistant", "content": "hosted ok"}}],
                                   "usage": {"prompt_tokens": 3, "completion_tokens": 2}})
                            .to_string(),
                        ),
                    }
                };
                let res = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n{extra}content-length: \
                     {}\r\nconnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = tls.write_all(res.as_bytes()).await;
                let _ = tls.shutdown().await;
            });
        }
    });
    mock
}
