//! The host settings `pmos-web` enforces itself (slice H4, `docs/openshell-any-host-spec.md`
//! §3.1, §3.6 and §3.8), however the stack is started:
//!
//! - `PMOS_EXPOSURE` (required, no default): `private` or `public`. Slice H10 serves `public`
//!   only together with the owner's two answers, checked by [`check_deployment`] before anything
//!   starts: the admin routes on their own door on every host (D3 (b)), and on `public` no
//!   development sign-in and password sign-in only from `PMOS_WEB_PASSWORD_CLIENTS` (D2 (a)).
//!   Under `private`, the public URL must not resolve to a public address
//!   ([`check_private_answers`], at start and every 10 minutes).
//! - `PMOS_MODEL_SOURCE` (required, no default). Under `hosted-test` (fake data only) the web
//!   app refuses content that isn't test content and a database that has held real data, and
//!   shows "test stack: fake data only" on every page ([`with_test_stack_banner`]), since what
//!   people type can't be checked by code.
//! - `PMOS_CONNECTORS` (required, `live` or `fake`): for the database's `held_real_data` marker
//!   only.

use axum::body::{to_bytes, Body};
use axum::extract::Request;
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;
use pmos_contract::model_source::ModelSource;

/// Who can reach the sign-in page (`PMOS_EXPOSURE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exposure {
    /// Only from a network the company controls.
    Private,
    /// A public address, or a network the company doesn't control.
    Public,
}

/// Why `pmos-web` won't start without the admin door (decision D3 (b), on every host).
pub const ADMIN_DOOR_REQUIRED: &str = "PMOS_WEB_ADMIN_BIND is required: the admin routes are \
     served only on their own listener, on every host (decision D3 (b), \
     docs/openshell-any-host-spec.md §3.8). Set PMOS_WEB_ADMIN_BIND (0.0.0.0:8772 in the deploy \
     kit) and PMOS_WEB_ADMIN_URL (docs/runbooks/install.md, \"The admin door\")";

/// Why `PMOS_EXPOSURE=public` won't start with password sign-in and no allow-list (D2 (a)).
pub const PUBLIC_PASSWORD_NEEDS_CLIENTS: &str = "PMOS_EXPOSURE=public with password sign-in: set \
     PMOS_WEB_PASSWORD_CLIENTS (a VPN or office range), or turn password sign-in off and use \
     Microsoft sign-in (decision D2)";

/// Why development sign-in won't start on a public host (D2 (a); H-DEVLOGIN says it first).
pub const PUBLIC_DEV_LOGIN: &str = "PMOS_EXPOSURE=public with development sign-in \
     (PMOS_WEB_DEV_LOGIN=1): anyone who reaches it signs in as anyone. Turn it off";

/// The rules `pmos-web` itself holds for the host before it serves anything (slice H10), however
/// the stack is started: the admin door on every host; on `public`, no development sign-in and
/// password sign-in only with an allow-list.
pub fn check_deployment(host: &HostSettings, config: &crate::Config) -> Result<(), String> {
    if config.admin.is_none() {
        return Err(ADMIN_DOOR_REQUIRED.into());
    }
    if host.exposure == Exposure::Public {
        if config.dev_login {
            return Err(PUBLIC_DEV_LOGIN.into());
        }
        if config.password_login && config.password_clients.is_empty() {
            return Err(PUBLIC_PASSWORD_NEEDS_CLIENTS.into());
        }
    }
    Ok(())
}

/// Under `PMOS_EXPOSURE=private`, the addresses `PMOS_WEB_PUBLIC_URL`'s host resolved to must all
/// be loopback, RFC 1918, `100.64.0.0/10`, link-local or IPv6 unique-local: a `private`
/// declaration whose public URL is on the internet is a contradiction the service can see (r6).
/// The error names the first public answer.
pub fn check_private_answers(host: &str, answers: &[std::net::IpAddr]) -> Result<(), String> {
    match answers
        .iter()
        .find(|ip| !crate::config::in_private_host_ranges(**ip))
    {
        None => Ok(()),
        Some(ip) => Err(format!(
            "PMOS_EXPOSURE=private, but PMOS_WEB_PUBLIC_URL's host {host} resolves to {ip}, a \
             public address: the host is reachable from the internet. Set PMOS_EXPOSURE=public \
             (decisions D2 and D3 then apply), or give the web app a private name"
        )),
    }
}

/// [`check_private_answers`] for `public_url`, resolving its host now. A name that doesn't
/// resolve is no answer and passes (with a warning in the log): only an answer outside the
/// private ranges is a contradiction.
pub async fn check_private_url(public_url: &str) -> Result<(), String> {
    let Ok(url) = reqwest::Url::parse(public_url) else {
        return Ok(());
    };
    let Some(host) = url.host_str() else {
        return Ok(());
    };
    let port = url.port_or_known_default().unwrap_or(443);
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let answers: Vec<std::net::IpAddr> = match tokio::net::lookup_host((bare, port)).await {
        Ok(a) => a.map(|s| s.ip()).collect(),
        Err(e) => {
            tracing::warn!(host = bare, error = %e, "PMOS_WEB_PUBLIC_URL's host didn't resolve: the private-exposure check saw no address");
            return Ok(());
        }
    };
    check_private_answers(bare, &answers)
}

/// The settings, read at start-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostSettings {
    pub exposure: Exposure,
    pub model_source: ModelSource,
    pub connectors_live: bool,
}

impl HostSettings {
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let exposure = match get("PMOS_EXPOSURE").as_deref().map(str::trim) {
            Some("private") => Exposure::Private,
            // Slice H10: only together with D2 (a) and D3 (b), which `check_deployment` holds.
            Some("public") => Exposure::Public,
            None | Some("") => {
                return Err(
                    "PMOS_EXPOSURE is required: `private` (reached only from a network the \
                     company controls) or `public`; run deploy/setup.sh, which writes it"
                        .into(),
                )
            }
            Some(other) => {
                return Err(format!(
                    "PMOS_EXPOSURE must be `private` or `public`, not `{other}`"
                ))
            }
        };
        let model_source = ModelSource::from_lookup(&get)?;
        let connectors_live = match get("PMOS_CONNECTORS").as_deref().map(str::trim) {
            Some("live") => true,
            Some("fake") => false,
            None | Some("") => return Err("PMOS_CONNECTORS is required (`live` or `fake`)".into()),
            Some(other) => {
                return Err(format!(
                    "PMOS_CONNECTORS must be `live` or `fake`, not `{other}`"
                ))
            }
        };
        Ok(Self {
            exposure,
            model_source,
            connectors_live,
        })
    }
}

/// The banner's text.
pub const BANNER_TEXT: &str =
    "Test stack: fake data only. Prompts go to an unapproved hosted model.";

/// Where the banner's stylesheet is served (the app's CSP allows same-origin styles only).
pub const BANNER_CSS_PATH: &str = "/__pmos/test-stack.css";

const BANNER_CSS: &str = ".pmos-test-stack{position:sticky;top:0;z-index:2147483647;\
     margin:0;padding:6px 12px;background:#9a3412;color:#fff;text-align:center;\
     font:600 13px/1.4 system-ui,sans-serif;letter-spacing:.01em}";

/// The largest page the banner is put into; a larger HTML answer goes out as it is.
const MAX_PAGE: usize = 4 * 1024 * 1024;

/// Every HTML page `router` serves gets the "test stack: fake data only" banner as the first
/// thing in its body, with a stylesheet at [`BANNER_CSS_PATH`] (spec §3.6, fake-data mode).
pub fn with_test_stack_banner(router: Router) -> Router {
    router
        .route(
            BANNER_CSS_PATH,
            axum::routing::get(|| async {
                (
                    [
                        (header::CONTENT_TYPE, "text/css; charset=utf-8"),
                        (header::CACHE_CONTROL, "no-cache"),
                    ],
                    BANNER_CSS,
                )
            }),
        )
        .layer(middleware::from_fn(banner))
}

async fn banner(req: Request, next: Next) -> Response {
    let res = next.run(req).await;
    let html = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.starts_with("text/html"));
    if !html || res.status() != StatusCode::OK {
        return res;
    }
    let (mut parts, body) = res.into_parts();
    let Ok(bytes) = to_bytes(body, MAX_PAGE).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let page = insert_banner(&bytes);
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.insert(
        "x-pmos-test-stack",
        HeaderValue::from_static("fake-data-only"),
    );
    Response::from_parts(parts, Body::from(page))
}

/// The page with the banner right after `<body …>` (or at the start, with no body tag) and
/// its stylesheet before `</head>`.
pub fn insert_banner(page: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(page);
    let link = format!("<link rel=\"stylesheet\" href=\"{BANNER_CSS_PATH}\">");
    let div = format!("<div class=\"pmos-test-stack\" role=\"alert\">{BANNER_TEXT}</div>");
    let lower = text.to_ascii_lowercase();
    let mut out = text.to_string();
    if let Some(body) = lower.find("<body") {
        if let Some(end) = lower[body..].find('>') {
            out.insert_str(body + end + 1, &div);
        } else {
            out.insert_str(0, &div);
        }
    } else {
        out.insert_str(0, &div);
    }
    match out.to_ascii_lowercase().find("</head>") {
        Some(i) => out.insert_str(i, &link),
        None => out.insert_str(0, &link),
    }
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(pairs: &[(&str, &str)]) -> Result<HostSettings, String> {
        HostSettings::from_lookup(|k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        })
    }

    #[test]
    fn exposure_is_required_and_public_is_read() {
        let ok = [
            ("PMOS_EXPOSURE", "private"),
            ("PMOS_MODEL_SOURCE", "hosted-test"),
            ("PMOS_CONNECTORS", "fake"),
        ];
        let s = read(&ok).unwrap();
        assert_eq!(s.exposure, Exposure::Private);
        assert!(s.model_source.fake_data_only() && !s.connectors_live);
        let with = |k: &str, v: &str| {
            let mut p: Vec<(&str, &str)> = ok.iter().copied().filter(|(n, _)| *n != k).collect();
            if !v.is_empty() {
                p.push((k, v));
            }
            read(&p)
        };
        // Slice H10: `public` is read; what it needs is `check_deployment`'s (tested below).
        assert_eq!(
            with("PMOS_EXPOSURE", "public").unwrap().exposure,
            Exposure::Public
        );
        assert!(with("PMOS_EXPOSURE", "")
            .unwrap_err()
            .contains("PMOS_EXPOSURE is required"));
        assert!(with("PMOS_EXPOSURE", "intranet")
            .unwrap_err()
            .contains("must be `private` or `public`"));
        assert!(with("PMOS_MODEL_SOURCE", "")
            .unwrap_err()
            .contains("PMOS_MODEL_SOURCE is required"));
        assert!(with("PMOS_MODEL_SOURCE", "hosted")
            .unwrap_err()
            .contains("not built yet"));
        assert!(with("PMOS_CONNECTORS", "")
            .unwrap_err()
            .contains("PMOS_CONNECTORS is required"));
        assert!(with("PMOS_CONNECTORS", "live").unwrap().connectors_live);
    }

    /// Slice H10: `pmos-web`'s own rules for the host, before it serves anything.
    #[test]
    fn the_deployment_rules_hold_for_public_and_private() {
        let cfg = |pairs: &[(&str, &str)]| {
            let mut all: Vec<(&str, &str)> = vec![
                (
                    "PMOS_WEB_BOTS_TOKEN",
                    "web-client-token-0123456789abcdef0123",
                ),
                (
                    "PMOS_WEB_TOOLSERVER_TOKEN",
                    "service-token-0123456789abcdef012345",
                ),
                (
                    "PMOS_ASSERTION_KEYS",
                    "web:toolserver:k1:AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=",
                ),
                ("PMOS_WEB_PASSWORD_LOGIN", "1"),
                ("PMOS_WEB_BIND", "0.0.0.0:8770"),
                ("PMOS_WEB_TRUSTED_PROXIES", "10.201.77.1"),
                ("PMOS_WEB_PUBLIC_URL", "https://pmos.example.com"),
                ("PMOS_WEB_ADMIN_BIND", "0.0.0.0:8772"),
                ("PMOS_WEB_ADMIN_URL", "http://pmos-prod.localhost:8772"),
                ("PMOS_WEB_ADMIN_TUNNEL_PEER", "10.201.77.1"),
            ];
            all.retain(|(k, _)| !pairs.iter().any(|(p, _)| p == k));
            all.extend(pairs.iter().filter(|(_, v)| !v.is_empty()));
            crate::Config::from_lookup(|k| {
                all.iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            })
            .unwrap()
        };
        let host = |exposure: &str| {
            read(&[
                ("PMOS_EXPOSURE", exposure),
                ("PMOS_MODEL_SOURCE", "hosted-test"),
                ("PMOS_CONNECTORS", "fake"),
            ])
            .unwrap()
        };
        // Public with password sign-in: the allow-list is required.
        assert_eq!(
            check_deployment(&host("public"), &cfg(&[])).unwrap_err(),
            PUBLIC_PASSWORD_NEEDS_CLIENTS
        );
        let listed = cfg(&[("PMOS_WEB_PASSWORD_CLIENTS", "10.8.0.0/16")]);
        assert!(check_deployment(&host("public"), &listed).is_ok());
        // Private: optional.
        assert!(check_deployment(&host("private"), &cfg(&[])).is_ok());
        // The admin door on every host.
        let no_door = cfg(&[
            ("PMOS_WEB_ADMIN_BIND", ""),
            ("PMOS_WEB_ADMIN_URL", ""),
            ("PMOS_WEB_ADMIN_TUNNEL_PEER", ""),
            ("PMOS_WEB_PASSWORD_CLIENTS", "10.8.0.0/16"),
        ]);
        for exposure in ["public", "private"] {
            assert_eq!(
                check_deployment(&host(exposure), &no_door).unwrap_err(),
                ADMIN_DOOR_REQUIRED,
                "{exposure}"
            );
        }
        // Development sign-in on public: refused by pmos-web itself.
        let dev = cfg(&[
            ("PMOS_WEB_DEV_LOGIN", "1"),
            ("PMOS_WEB_DEV_LOGIN_WITH_PASSWORD", "1"),
            ("PMOS_WEB_BIND", "127.0.0.1:8770"),
            ("PMOS_WEB_PASSWORD_CLIENTS", "10.8.0.0/16"),
        ]);
        assert_eq!(
            check_deployment(&host("public"), &dev).unwrap_err(),
            PUBLIC_DEV_LOGIN
        );
        assert!(check_deployment(&host("private"), &dev).is_ok());
    }

    /// The private-exposure check by answer: every private range passes; one public answer
    /// among them fails, naming it.
    #[test]
    fn a_private_host_s_public_url_must_resolve_privately() {
        let ip = |s: &str| s.parse::<std::net::IpAddr>().unwrap();
        for ok in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "100.64.0.1",
            "100.127.255.254",
            "169.254.1.1",
            "::1",
            "fd12:3456::1",
            "fe80::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(
                check_private_answers("pmos.corp", &[ip(ok)]).is_ok(),
                "{ok}"
            );
        }
        for bad in [
            "203.0.113.7",
            "172.32.0.1",
            "100.128.0.1",
            "8.8.8.8",
            "2001:db8::1",
            "::ffff:203.0.113.7",
        ] {
            let err = check_private_answers("pmos.corp", &[ip("10.0.0.1"), ip(bad)]).unwrap_err();
            assert!(
                err.contains(bad.trim_start_matches("::ffff:")) || err.contains(bad),
                "{err}"
            );
            assert!(err.contains("PMOS_EXPOSURE=private"), "{err}");
        }
    }

    #[tokio::test]
    async fn the_private_url_check_resolves_the_host() {
        assert!(check_private_url("http://127.0.0.1:8770").await.is_ok());
        assert!(check_private_url("http://[::1]:8770").await.is_ok());
        assert!(check_private_url("https://203.0.113.7")
            .await
            .unwrap_err()
            .contains("203.0.113.7"));
        // A name that doesn't resolve is no answer.
        assert!(check_private_url("https://pmos.invalid").await.is_ok());
    }

    #[test]
    fn the_banner_goes_first_in_the_body_with_its_stylesheet() {
        let page = b"<!doctype html><html><head><title>PM OS</title></head>\
                     <body class=\"x\"><div id=\"root\"></div></body></html>";
        let out = String::from_utf8(insert_banner(page)).unwrap();
        assert!(out.contains("<link rel=\"stylesheet\" href=\"/__pmos/test-stack.css\"></head>"));
        assert!(out.contains(&format!(
            "<body class=\"x\"><div class=\"pmos-test-stack\" role=\"alert\">{BANNER_TEXT}</div><div id=\"root\">"
        )));
        // No body or head: both go first.
        let out = String::from_utf8(insert_banner(b"<p>hi</p>")).unwrap();
        assert!(out.starts_with("<link"), "{out}");
        assert!(out.contains(BANNER_TEXT));
    }

    #[tokio::test]
    async fn every_html_page_gets_the_banner_and_nothing_else_changes() {
        use axum::routing::get;
        let app = with_test_stack_banner(
            Router::new()
                .route(
                    "/",
                    get(|| async {
                        axum::response::Html(
                            "<html><head></head><body><div id=root></div></body></html>",
                        )
                    }),
                )
                .route(
                    "/api/me",
                    get(|| async { axum::Json(serde_json::json!({"a": 1})) }),
                ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let get_ = |path: &'static str| {
            let url = format!("{base}{path}");
            async move {
                let res = reqwest::get(url).await.unwrap();
                let headers = res.headers().clone();
                (headers, res.text().await.unwrap())
            }
        };
        let (h, page) = get_("/").await;
        assert!(page.contains(BANNER_TEXT), "{page}");
        assert_eq!(h["x-pmos-test-stack"], "fake-data-only");
        let (h, json) = get_("/api/me").await;
        assert_eq!(json, "{\"a\":1}");
        assert!(h.get("x-pmos-test-stack").is_none());
        let (h, css) = get_(BANNER_CSS_PATH).await;
        assert!(css.starts_with(".pmos-test-stack{"));
        assert!(h[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/css"));
    }
}
