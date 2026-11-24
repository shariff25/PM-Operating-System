//! How the tool server reaches one MCP server (`docs/mcp-servers-spec.md` §2.3, threats 6 and
//! 12): an HTTP client built from the record alone.
//!
//! - **Remote** (`https://`): HTTPS only; the certificate is checked against the platform's
//!   roots for the record's host and, when the record pins public keys, the end certificate's
//!   SPKI must match one. The server's names resolve only to public addresses (a name that
//!   resolves to a loopback, private or link-local address is refused). Redirects are refused.
//! - **Internal** (`http://<name>.pmos.internal`): the name is never looked up; the client
//!   connects to the record's pinned address only, and never through a proxy.
//! - Connecting takes at most 3 seconds.
//!
//! The only thing that can change this is a test route (`TestRoutes`), which exists only in
//! test builds (`cfg(test)` or the `test-support` feature, never in a release binary): it sends
//! a remote record's host to a local fake server over plain HTTP, so the production-shaped
//! record is exercised against fakes.

use std::collections::BTreeSet;
#[cfg(any(test, feature = "test-support"))]
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use pmos_contract::mcp::{parse_url, ServerRecord};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};

/// How long connecting may take (§2.10).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Test builds only: hosts sent to a local address over plain HTTP.
#[derive(Debug, Clone, Default)]
pub struct TestRoutes {
    #[cfg(any(test, feature = "test-support"))]
    routes: HashMap<String, SocketAddr>,
}

impl TestRoutes {
    /// Send `host` to `addr` over plain HTTP (tests only).
    #[cfg(any(test, feature = "test-support"))]
    pub fn route(mut self, host: &str, addr: SocketAddr) -> Self {
        self.routes.insert(host.to_string(), addr);
        self
    }

    fn get(&self, host: &str) -> Option<SocketAddr> {
        #[cfg(any(test, feature = "test-support"))]
        {
            self.routes.get(host).copied()
        }
        #[cfg(not(any(test, feature = "test-support")))]
        {
            let _ = host;
            None
        }
    }

    /// The URL a request for `url` is sent to: itself, or in a test build the local route for its
    /// host over plain HTTP.
    pub fn rewrite(&self, url: &str) -> String {
        match parse_url(url) {
            Ok(u) if u.https => match self.get(&u.host) {
                Some(addr) => format!("http://{addr}{}", u.path),
                None => url.to_string(),
            },
            _ => url.to_string(),
        }
    }
}

/// `sha256:<hex>` of a DER certificate's SubjectPublicKeyInfo.
pub fn spki_digest(cert: &CertificateDer<'_>) -> Result<String, String> {
    let ee = webpki::EndEntityCert::try_from(cert).map_err(|e| format!("bad certificate: {e}"))?;
    let spki = ee.subject_public_key_info();
    let digest = Sha256::digest(spki.as_ref());
    Ok(format!(
        "sha256:{}",
        digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    ))
}

/// The usual certificate checks, then the end certificate's public key must be one of the
/// pins.
#[derive(Debug)]
pub struct PinnedVerifier {
    inner: Arc<dyn ServerCertVerifier>,
    pins: BTreeSet<String>,
}

impl PinnedVerifier {
    pub fn new(inner: Arc<dyn ServerCertVerifier>, pins: impl IntoIterator<Item = String>) -> Self {
        Self {
            inner,
            pins: pins.into_iter().collect(),
        }
    }
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let verified = self.inner.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        )?;
        let digest = spki_digest(end_entity).map_err(rustls::Error::General)?;
        if self.pins.contains(&digest) {
            Ok(verified)
        } else {
            Err(rustls::Error::General(
                "the server's public key doesn't match the record's pin".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// May a remote server's name resolve to `ip`? Never loopback, private, link-local (the cloud
/// metadata address included), unspecified, multicast or documentation ranges.
pub fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (64..=127).contains(&o[1]))
                || (o[0] == 198 && (18..=19).contains(&o[1])))
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return public_address(IpAddr::V4(v4));
            }
            let s = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80)
        }
    }
}

/// Resolves the record's own hosts to public addresses only; any other name (a proxy's) as
/// usual.
struct PublicOnly {
    hosts: BTreeSet<String>,
}

impl reqwest::dns::Resolve for PublicOnly {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        let checked = self.hosts.contains(&host);
        Box::pin(async move {
            let addrs: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            if checked && addrs.iter().any(|a| !public_address(a.ip())) {
                return Err(format!(
                    "{host} resolves to a non-public address; a remote MCP server must be public"
                )
                .into());
            }
            let out: reqwest::dns::Addrs = Box::new(addrs.into_iter());
            Ok(out)
        })
    }
}

/// The HTTP client for `record`'s server and authorization server.
pub fn http_client(record: &ServerRecord, routes: &TestRoutes) -> Result<reqwest::Client, String> {
    let url = parse_url(&record.url)?;
    let builder = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none());
    if !url.https {
        // Internal: the pinned address, never DNS, never a proxy.
        let addr: IpAddr = record
            .identity
            .address
            .as_deref()
            .and_then(|a| a.parse().ok())
            .ok_or_else(|| format!("`{}` has no pinned address", record.id))?;
        let port = url.port.unwrap_or(80);
        return builder
            .no_proxy()
            .resolve(&url.host, SocketAddr::new(addr, port))
            .build()
            .map_err(|e| format!("`{}`: {e}", record.id));
    }
    let hosts = record.hosts();
    let mut builder = builder;
    let routed: Vec<(String, SocketAddr)> = hosts
        .iter()
        .filter_map(|h| routes.get(h).map(|a| (h.clone(), a)))
        .collect();
    if routed.is_empty() {
        builder = builder.https_only(true).dns_resolver(Arc::new(PublicOnly {
            hosts: hosts.clone(),
        }));
        if !record.identity.spki_sha256.is_empty() {
            let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
            let platform = rustls_platform_verifier::Verifier::new(provider.clone())
                .map_err(|e| format!("`{}`: the platform's certificate roots: {e}", record.id))?;
            let verifier = PinnedVerifier::new(
                Arc::new(platform),
                record.identity.spki_sha256.iter().cloned(),
            );
            let tls = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .map_err(|e| format!("`{}`: TLS: {e}", record.id))?
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(verifier))
                .with_no_client_auth();
            builder = builder.tls_backend_preconfigured(tls);
        }
    } else {
        // A test build sending this record's hosts to local fakes.
        builder = builder.no_proxy();
        for (h, a) in routed {
            builder = builder.resolve(&h, a);
        }
    }
    builder.build().map_err(|e| format!("`{}`: {e}", record.id))
}

/// Every host the loaded records allow the tool server to reach (§5, the code allowlist).
pub fn allowed_hosts(records: &[ServerRecord]) -> BTreeSet<String> {
    records.iter().flat_map(|r| r.hosts()).collect()
}

/// Is `url` at a host `records` allow? Checked before every connection and OAuth request.
pub fn host_allowed(hosts: &BTreeSet<String>, url: &str) -> bool {
    parse_url(url).is_ok_and(|u| hosts.contains(&u.host))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A self-signed certificate for `pin.example.com` (test fixture, no private key kept).
    const CERT_PEM: &str = include_str!("testdata/pin-cert.pem");
    const CERT_SPKI: &str = include_str!("testdata/pin-cert.spki");

    fn cert() -> CertificateDer<'static> {
        let b64: String = CERT_PEM
            .lines()
            .filter(|l| !l.starts_with("-----"))
            .collect();
        use base64::Engine;
        CertificateDer::from(
            base64::engine::general_purpose::STANDARD
                .decode(b64)
                .unwrap(),
        )
    }

    /// Accepts every certificate: the pin is the only check left.
    #[derive(Debug)]
    struct AcceptAll;

    impl ServerCertVerifier for AcceptAll {
        fn verify_server_cert(
            &self,
            _: &CertificateDer<'_>,
            _: &[CertificateDer<'_>],
            _: &ServerName<'_>,
            _: &[u8],
            _: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            _: &[u8],
            _: &CertificateDer<'_>,
            _: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }
        fn verify_tls13_signature(
            &self,
            _: &[u8],
            _: &CertificateDer<'_>,
            _: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }
        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            vec![SignatureScheme::ECDSA_NISTP256_SHA256]
        }
    }

    /// Refuses every certificate, as a wrong host or an unknown root would be.
    #[derive(Debug)]
    struct RefuseAll;

    impl ServerCertVerifier for RefuseAll {
        fn verify_server_cert(
            &self,
            _: &CertificateDer<'_>,
            _: &[CertificateDer<'_>],
            _: &ServerName<'_>,
            _: &[u8],
            _: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            Err(rustls::Error::General("unknown issuer".into()))
        }
        fn verify_tls12_signature(
            &self,
            _: &[u8],
            _: &CertificateDer<'_>,
            _: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }
        fn verify_tls13_signature(
            &self,
            _: &[u8],
            _: &CertificateDer<'_>,
            _: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }
        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            vec![]
        }
    }

    fn verify(v: &PinnedVerifier) -> Result<ServerCertVerified, rustls::Error> {
        v.verify_server_cert(
            &cert(),
            &[],
            &ServerName::try_from("pin.example.com").unwrap(),
            &[],
            UnixTime::now(),
        )
    }

    #[test]
    fn a_pinned_key_must_match() {
        let spki = CERT_SPKI.trim().to_string();
        assert_eq!(spki_digest(&cert()).unwrap(), spki);
        assert!(verify(&PinnedVerifier::new(Arc::new(AcceptAll), [spki.clone()])).is_ok());
        let other = format!("sha256:{}", "0".repeat(64));
        assert!(
            verify(&PinnedVerifier::new(Arc::new(AcceptAll), [other])).is_err(),
            "wrong pin"
        );
        assert!(
            verify(&PinnedVerifier::new(Arc::new(RefuseAll), [spki])).is_err(),
            "the pin never replaces the usual checks"
        );
    }

    #[test]
    fn remote_names_resolve_to_public_addresses_only() {
        for bad in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!public_address(bad.parse().unwrap()), "{bad}");
        }
        for good in ["8.8.8.8", "2606:4700::1111"] {
            assert!(public_address(good.parse().unwrap()), "{good}");
        }
    }

    #[test]
    fn mcp_23_the_allowed_hosts_are_the_records_own() {
        let mut r = crate::mcp_client::fake::fake_records("127.0.0.1:9".parse().unwrap()).remove(0);
        let hosts = allowed_hosts(std::slice::from_ref(&r));
        assert_eq!(
            hosts.into_iter().collect::<Vec<_>>(),
            vec!["fake-mcp.pmos.internal"]
        );
        r.url = "https://mcp.example.com/mcp".into();
        r.oauth = Some(pmos_contract::mcp::OAuthSettings {
            client_id: "c".into(),
            client_secret_env: None,
            issuer: "https://auth.example.com".into(),
            authorization_endpoint: "https://auth.example.com/authorize".into(),
            token_endpoint: "https://login.example.com/token".into(),
            scopes: vec![],
            resource: r.url.clone(),
        });
        let hosts = allowed_hosts(std::slice::from_ref(&r));
        assert_eq!(
            hosts.iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["auth.example.com", "login.example.com", "mcp.example.com"]
        );
        assert!(host_allowed(&hosts, "https://mcp.example.com/other"));
        assert!(!host_allowed(&hosts, "https://evil.example.com/mcp"));
        assert!(!host_allowed(&hosts, "https://169.254.169.254/latest"));
    }

    #[test]
    fn test_routes_only_rewrite_listed_https_hosts() {
        let routes =
            TestRoutes::default().route("mcp.example.test", "127.0.0.1:9".parse().unwrap());
        assert_eq!(
            routes.rewrite("https://mcp.example.test/v1/mcp"),
            "http://127.0.0.1:9/v1/mcp"
        );
        assert_eq!(
            routes.rewrite("https://other.example.test/mcp"),
            "https://other.example.test/mcp"
        );
    }
}
