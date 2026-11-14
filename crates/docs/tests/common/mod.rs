#![allow(dead_code)]

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use pmos_docs::store::PgStore;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::Connection;
use tokio_util::sync::CancellationToken;

// ----- mock portal ------------------------------------------------------------------------

#[derive(Clone)]
pub struct MockPage {
    pub etag: String,
    pub body: String,
    pub status: u16,
}

#[derive(Default)]
pub struct Portal {
    pub robots: Mutex<(u16, String)>,
    pub index: Mutex<String>,
    pub pages: Mutex<HashMap<String, MockPage>>,
    /// Requests by path.
    pub hits: Mutex<HashMap<String, usize>>,
    /// `If-None-Match` values seen, by path.
    pub conditional: Mutex<HashMap<String, Vec<String>>>,
    /// Answer 429 (`Retry-After: 1`) this many more times, by path.
    pub throttle: Mutex<HashMap<String, usize>>,
    /// Serve these paths with this `Content-Type` instead.
    pub content_types: Mutex<HashMap<String, String>>,
    /// Answer these paths with a 302 to this location.
    pub redirects: Mutex<HashMap<String, String>>,
    /// Stream this many bytes for these paths, with no `Content-Length`.
    pub streamed: Mutex<HashMap<String, usize>>,
}

impl Portal {
    pub fn hits(&self, path: &str) -> usize {
        self.hits.lock().unwrap().get(path).copied().unwrap_or(0)
    }

    pub fn total_hits(&self) -> usize {
        self.hits.lock().unwrap().values().sum()
    }

    pub fn set_page(&self, slug: &str, etag: &str, body: &str) {
        self.pages.lock().unwrap().insert(
            format!("/docs/{slug}.md"),
            MockPage {
                etag: etag.into(),
                body: body.into(),
                status: 200,
            },
        );
    }

    pub fn set_status(&self, slug: &str, status: u16) {
        if let Some(p) = self
            .pages
            .lock()
            .unwrap()
            .get_mut(&format!("/docs/{slug}.md"))
        {
            p.status = status;
        }
    }

    /// Set the index to these pages (slug, title, description).
    pub fn set_index(&self, base: &str, entries: &[(&str, &str, Option<&str>)]) {
        let mut s =
            String::from("# Fortanix\n\n> Knowledge base documentation for Fortanix.\n\n## v1\n\n");
        for (slug, title, desc) in entries {
            s.push_str(&format!("- [{title}]({base}/docs/{slug}.md)"));
            if let Some(d) = desc {
                s.push_str(&format!(": {d}"));
            }
            s.push('\n');
        }
        *self.index.lock().unwrap() = s;
    }
}

pub fn page_md(title: &str, slug: &str, body: &str) -> String {
    format!(
        "---\ntitle: \"{title}\"\nslug: \"{slug}\"\nupdated: 2026-10-01T10:00:00Z\n---\n\n\
         > ## Documentation Index\n> Fetch the complete documentation index at: https://support.fortanix.com/llms.txt\n\n\
         # {title}\n\n{body}\n"
    )
}

async fn handle(State(p): State<Arc<Portal>>, uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path().to_string();
    *p.hits.lock().unwrap().entry(path.clone()).or_default() += 1;
    if let Some(v) = headers.get("if-none-match").and_then(|v| v.to_str().ok()) {
        p.conditional
            .lock()
            .unwrap()
            .entry(path.clone())
            .or_default()
            .push(v.to_string());
    }
    if let Some(to) = p.redirects.lock().unwrap().get(&path).cloned() {
        return (StatusCode::FOUND, [("location", to)]).into_response();
    }
    if let Some(n) = p.streamed.lock().unwrap().get(&path).copied() {
        // Chunked, so the size is only known by reading it.
        let chunks = (0..n.div_ceil(8192))
            .map(|_| Ok::<_, std::io::Error>(axum::body::Bytes::from(vec![b'x'; 8192])));
        return axum::body::Body::from_stream(futures_util::stream::iter(chunks)).into_response();
    }
    match path.as_str() {
        "/robots.txt" => {
            let (status, body) = p.robots.lock().unwrap().clone();
            (StatusCode::from_u16(status).unwrap(), body).into_response()
        }
        "/llms.txt" => p.index.lock().unwrap().clone().into_response(),
        _ if p
            .throttle
            .lock()
            .unwrap()
            .get(&path)
            .is_some_and(|n| *n > 0) =>
        {
            *p.throttle.lock().unwrap().get_mut(&path).unwrap() -= 1;
            (
                StatusCode::TOO_MANY_REQUESTS,
                [("retry-after", "1")],
                "slow down",
            )
                .into_response()
        }
        _ => {
            let page = p.pages.lock().unwrap().get(&path).cloned();
            let Some(page) = page else {
                return StatusCode::NOT_FOUND.into_response();
            };
            if page.status != 200 {
                return StatusCode::from_u16(page.status).unwrap().into_response();
            }
            let inm = headers.get("if-none-match").and_then(|v| v.to_str().ok());
            if inm == Some(page.etag.as_str()) {
                return StatusCode::NOT_MODIFIED.into_response();
            }
            let ct = p
                .content_types
                .lock()
                .unwrap()
                .get(&path)
                .cloned()
                .unwrap_or_else(|| "text/markdown; charset=utf-8".into());
            (
                [("etag", page.etag.clone()), ("content-type", ct)],
                page.body,
            )
                .into_response()
        }
    }
}

pub async fn start_portal() -> (String, Arc<Portal>, CancellationToken) {
    let portal = Arc::new(Portal::default());
    *portal.robots.lock().unwrap() = (200, "User-agent: *\nDisallow:\n".into());
    let app = Router::new().fallback(handle).with_state(portal.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let cancel = CancellationToken::new();
    let ct = cancel.clone();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { ct.cancelled_owned().await })
            .await
            .unwrap();
    });
    (base, portal, cancel)
}

/// Four pages: two DSM, one CCM, one general.
pub fn seed(base: &str, portal: &Portal) {
    portal.set_index(
        base,
        &[
            (
                "dsm-key-rotation",
                "Fortanix DSM Key Rotation",
                Some("Rotate keys in DSM"),
            ),
            ("dsm-backup", "Fortanix DSM Backup and Restore", None),
            (
                "ccm-overview",
                "Confidential Computing Manager Overview",
                None,
            ),
            ("getting-started", "Getting Started", None),
        ],
    );
    portal.set_page(
        "dsm-key-rotation",
        "\"r1\"",
        &page_md(
            "Fortanix DSM Key Rotation",
            "dsm-key-rotation",
            "## Rotate a key\n\nKey rotation creates a new key version.\n\n## Rotation policy\n\nSet a rotation interval.",
        ),
    );
    portal.set_page(
        "dsm-backup",
        "\"b1\"",
        &page_md(
            "Fortanix DSM Backup and Restore",
            "dsm-backup",
            "## Backup\n\nBack up the cluster. Keys are included.",
        ),
    );
    portal.set_page(
        "ccm-overview",
        "\"c1\"",
        &page_md(
            "Confidential Computing Manager Overview",
            "ccm-overview",
            "CCM runs enclaves and attests them.",
        ),
    );
    portal.set_page(
        "getting-started",
        "\"g1\"",
        &page_md(
            "Getting Started",
            "getting-started",
            "Sign up for a support account.",
        ),
    );
}

// ----- Postgres ---------------------------------------------------------------------------

/// A Postgres store in a fresh schema. Needs `PMOS_TEST_DATABASE_URL`; skipped locally
/// without it and fails in CI (`CI` set).
pub struct TestPg {
    pub store: PgStore,
    base: PgConnectOptions,
    schema: String,
}

impl TestPg {
    pub async fn new() -> Option<Self> {
        let Ok(url) = std::env::var("PMOS_TEST_DATABASE_URL") else {
            assert!(
                std::env::var_os("CI").is_none(),
                "PMOS_TEST_DATABASE_URL must be set in CI so the Postgres tests run"
            );
            eprintln!("skipping Postgres case: PMOS_TEST_DATABASE_URL is not set");
            return None;
        };
        let base = PgConnectOptions::from_str(&url).expect("PMOS_TEST_DATABASE_URL parses");
        let schema = format!("d_{}", uuid::Uuid::new_v4().simple());
        let mut admin = sqlx::PgConnection::connect_with(&base)
            .await
            .expect("connect");
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&mut admin)
            .await
            .expect("create schema");
        admin.close().await.ok();
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect_with(base.clone().options([("search_path", schema.as_str())]))
            .await
            .expect("connect to test schema");
        let store = PgStore::from_pool(pool);
        store.migrate().await.expect("migrations apply");
        Some(Self {
            store,
            base,
            schema,
        })
    }

    pub async fn drop_schema(self) {
        self.store.pool().close().await;
        if let Ok(mut admin) = sqlx::PgConnection::connect_with(&self.base).await {
            let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
                "DROP SCHEMA {} CASCADE",
                self.schema
            )))
            .execute(&mut admin)
            .await;
        }
    }
}
