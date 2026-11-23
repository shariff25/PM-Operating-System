//! The host settings `pmos-web` enforces itself (slice H4, `docs/openshell-any-host-spec.md`
//! §3.1, §3.6 and §3.8), however the stack is started:
//!
//! - `PMOS_EXPOSURE` (required, no default): `private` or `public`. **Until slice H10, `public`
//!   is refused**: no build between H4 and H10 serves a public sign-in page with neither of the
//!   owner's sign-in answers (D2, D3) built.
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

/// Why `PMOS_EXPOSURE=public` won't start (spec §3.8).
pub const PUBLIC_REFUSED: &str = "PMOS_EXPOSURE=public: this build of pmos-web does not serve a \
     public host yet (spec slice H10). Reach it over a VPN or an SSH tunnel with \
     PMOS_EXPOSURE=private, or leave the web app off";

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
            Some("public") => return Err(PUBLIC_REFUSED.into()),
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
    fn exposure_is_required_and_public_is_refused_until_h10() {
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
        assert_eq!(with("PMOS_EXPOSURE", "public").unwrap_err(), PUBLIC_REFUSED);
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
