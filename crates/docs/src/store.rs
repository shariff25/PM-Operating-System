//! Where indexed pages live: in memory (tests, `PMOS_DEV=1`) or in Postgres.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::RwLock;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pmos_contract::text::{snippet, terms};
use serde::Serialize;
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::Row;

use crate::page::DocPage;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("storage error: {0}")]
pub struct StoreError(pub String);

impl From<sqlx::Error> for StoreError {
    fn from(e: sqlx::Error) -> Self {
        StoreError(e.to_string())
    }
}

pub type StoreResult<T> = Result<T, StoreError>;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Hit {
    pub page: DocPage,
    pub score: f32,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProductCount {
    pub product: String,
    pub pages: i64,
}

#[async_trait]
pub trait DocsStore: Send + Sync {
    async fn upsert(&self, page: &DocPage) -> StoreResult<()>;
    async fn get(&self, url: &str) -> StoreResult<Option<DocPage>>;
    /// Stored ETags by URL, for conditional requests.
    async fn etags(&self) -> StoreResult<HashMap<String, String>>;
    /// Delete every page whose URL is not in `keep`. Returns how many went.
    async fn retain(&self, keep: &HashSet<String>) -> StoreResult<u64>;
    async fn count(&self) -> StoreResult<i64>;
    /// Best matches, title first, optionally within one product.
    async fn search(
        &self,
        query: &str,
        product: Option<&str>,
        limit: usize,
    ) -> StoreResult<Vec<Hit>>;
    async fn products(&self) -> StoreResult<Vec<ProductCount>>;
    /// Record a finished sync run, successful or not, with its report.
    async fn record_sync(
        &self,
        finished_at: DateTime<Utc>,
        ok: bool,
        report: &serde_json::Value,
    ) -> StoreResult<()>;
    /// When the last successful sync finished, across restarts.
    async fn last_success(&self) -> StoreResult<Option<DateTime<Utc>>>;
}

// ----- memory -----------------------------------------------------------------------------

#[derive(Default)]
pub struct MemoryStore {
    pages: RwLock<BTreeMap<String, DocPage>>,
    last_success: RwLock<Option<DateTime<Utc>>>,
}

#[async_trait]
impl DocsStore for MemoryStore {
    async fn upsert(&self, page: &DocPage) -> StoreResult<()> {
        self.pages
            .write()
            .expect("docs lock")
            .insert(page.url.clone(), page.clone());
        Ok(())
    }

    async fn get(&self, url: &str) -> StoreResult<Option<DocPage>> {
        Ok(self.pages.read().expect("docs lock").get(url).cloned())
    }

    async fn etags(&self) -> StoreResult<HashMap<String, String>> {
        Ok(self
            .pages
            .read()
            .expect("docs lock")
            .values()
            .filter_map(|p| p.etag.clone().map(|e| (p.url.clone(), e)))
            .collect())
    }

    async fn retain(&self, keep: &HashSet<String>) -> StoreResult<u64> {
        let mut pages = self.pages.write().expect("docs lock");
        let before = pages.len();
        pages.retain(|url, _| keep.contains(url));
        Ok((before - pages.len()) as u64)
    }

    async fn count(&self) -> StoreResult<i64> {
        Ok(self.pages.read().expect("docs lock").len() as i64)
    }

    async fn search(
        &self,
        query: &str,
        product: Option<&str>,
        limit: usize,
    ) -> StoreResult<Vec<Hit>> {
        let q = terms(query);
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let pages = self.pages.read().expect("docs lock");
        // (has every query word, hit)
        let mut ranked: Vec<(bool, Hit)> = pages
            .values()
            .filter(|p| product.is_none_or(|want| p.product == want))
            .filter_map(|p| {
                let description = p.description.as_deref().unwrap_or_default();
                let weigh = |text: &str, w: f32| {
                    let t = terms(text);
                    q.iter()
                        .map(|qt| w * t.iter().filter(|x| *x == qt).count() as f32)
                        .sum::<f32>()
                };
                let score = weigh(&p.title, 4.0)
                    + weigh(&p.headings, 2.0)
                    + weigh(description, 2.0)
                    + weigh(&p.body, 1.0);
                if score <= 0.0 {
                    return None;
                }
                let words: HashSet<String> = terms(&format!(
                    "{} {} {description} {}",
                    p.title, p.headings, p.body
                ))
                .into_iter()
                .collect();
                let every = q.iter().all(|t| words.contains(t));
                Some((
                    every,
                    Hit {
                        snippet: snippet(&p.body, &q),
                        page: p.clone(),
                        score,
                    },
                ))
            })
            .collect();
        // Pages with every query word first, then by score.
        ranked.sort_by(|(a_all, a), (b_all, b)| {
            b_all
                .cmp(a_all)
                .then_with(|| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| a.page.url.cmp(&b.page.url))
        });
        Ok(ranked.into_iter().take(limit).map(|(_, hit)| hit).collect())
    }

    async fn products(&self) -> StoreResult<Vec<ProductCount>> {
        let mut counts: BTreeMap<String, i64> = BTreeMap::new();
        for p in self.pages.read().expect("docs lock").values() {
            *counts.entry(p.product.clone()).or_default() += 1;
        }
        Ok(counts
            .into_iter()
            .map(|(product, pages)| ProductCount { product, pages })
            .collect())
    }

    async fn record_sync(
        &self,
        finished_at: DateTime<Utc>,
        ok: bool,
        _report: &serde_json::Value,
    ) -> StoreResult<()> {
        if ok {
            *self.last_success.write().expect("docs lock") = Some(finished_at);
        }
        Ok(())
    }

    async fn last_success(&self) -> StoreResult<Option<DateTime<Utc>>> {
        Ok(*self.last_success.read().expect("docs lock"))
    }
}

// ----- Postgres ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    pub async fn connect(url: &str) -> StoreResult<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .acquire_timeout(Duration::from_secs(5))
            .connect(url)
            .await?;
        Ok(Self { pool })
    }

    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn migrate(&self) -> StoreResult<()> {
        let mut migrator = sqlx::migrate!("./migrations");
        // Its own bookkeeping table, so sharing a database with the tool server (whose
        // migrations use the default `_sqlx_migrations`) can't clash.
        migrator.dangerous_set_table_name("_pmos_docs_migrations");
        migrator
            .run(&self.pool)
            .await
            .map_err(|e| StoreError(e.to_string()))
    }
}

const COLUMNS: &str =
    "url, slug, title, section, product, description, headings, body, updated, etag, fetched_at";

fn page_from_row(r: &PgRow) -> StoreResult<DocPage> {
    Ok(DocPage {
        url: r.try_get("url")?,
        slug: r.try_get("slug")?,
        title: r.try_get("title")?,
        section: r.try_get("section")?,
        product: r.try_get("product")?,
        description: r.try_get("description")?,
        headings: r.try_get("headings")?,
        body: r.try_get("body")?,
        updated: r.try_get("updated")?,
        etag: r.try_get("etag")?,
        fetched_at: r.try_get("fetched_at")?,
    })
}

#[async_trait]
impl DocsStore for PgStore {
    async fn upsert(&self, p: &DocPage) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO docs_pages
                 (url, slug, title, section, product, description, headings, body, updated, etag,
                  fetched_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
             ON CONFLICT (url) DO UPDATE SET
                 slug = EXCLUDED.slug, title = EXCLUDED.title, section = EXCLUDED.section,
                 product = EXCLUDED.product, description = EXCLUDED.description,
                 headings = EXCLUDED.headings, body = EXCLUDED.body, updated = EXCLUDED.updated,
                 etag = EXCLUDED.etag, fetched_at = EXCLUDED.fetched_at",
        )
        .bind(&p.url)
        .bind(&p.slug)
        .bind(&p.title)
        .bind(&p.section)
        .bind(&p.product)
        .bind(&p.description)
        .bind(&p.headings)
        .bind(&p.body)
        .bind(p.updated)
        .bind(&p.etag)
        .bind(p.fetched_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get(&self, url: &str) -> StoreResult<Option<DocPage>> {
        let sql = format!("SELECT {COLUMNS} FROM docs_pages WHERE url = $1");
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(url)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(page_from_row).transpose()
    }

    async fn etags(&self) -> StoreResult<HashMap<String, String>> {
        let rows = sqlx::query("SELECT url, etag FROM docs_pages WHERE etag IS NOT NULL")
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| Ok((r.try_get("url")?, r.try_get("etag")?)))
            .collect()
    }

    async fn retain(&self, keep: &HashSet<String>) -> StoreResult<u64> {
        let keep: Vec<&str> = keep.iter().map(String::as_str).collect();
        let done = sqlx::query("DELETE FROM docs_pages WHERE NOT (url = ANY($1))")
            .bind(&keep)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected())
    }

    async fn count(&self) -> StoreResult<i64> {
        Ok(sqlx::query_scalar("SELECT count(*) FROM docs_pages")
            .fetch_one(&self.pool)
            .await?)
    }

    async fn search(
        &self,
        query: &str,
        product: Option<&str>,
        limit: usize,
    ) -> StoreResult<Vec<Hit>> {
        // Terms are letters and digits only, so the tsquery can't carry operators.
        let q = terms(query);
        if q.is_empty() {
            return Ok(Vec::new());
        }
        // Any-word match, with pages matching every word first.
        let sql = format!(
            "SELECT {COLUMNS}, ts_rank_cd(tsv, q) AS score
             FROM docs_pages, to_tsquery('english', $1) AS q, to_tsquery('english', $4) AS every
             WHERE tsv @@ q AND ($2::text IS NULL OR product = $2)
             ORDER BY (tsv @@ every) DESC, score DESC, url
             LIMIT $3"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(q.join(" | "))
            .bind(product)
            .bind(limit as i64)
            .bind(q.join(" & "))
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| {
                let page = page_from_row(r)?;
                Ok(Hit {
                    snippet: snippet(&page.body, &q),
                    score: r.try_get::<f32, _>("score")?,
                    page,
                })
            })
            .collect()
    }

    async fn products(&self) -> StoreResult<Vec<ProductCount>> {
        let rows = sqlx::query(
            "SELECT product, count(*) AS pages FROM docs_pages GROUP BY product ORDER BY product",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(ProductCount {
                    product: r.try_get("product")?,
                    pages: r.try_get("pages")?,
                })
            })
            .collect()
    }

    async fn record_sync(
        &self,
        finished_at: DateTime<Utc>,
        ok: bool,
        report: &serde_json::Value,
    ) -> StoreResult<()> {
        sqlx::query("INSERT INTO docs_syncs (finished_at, ok, report) VALUES ($1, $2, $3)")
            .bind(finished_at)
            .bind(ok)
            .bind(report)
            .execute(&self.pool)
            .await?;
        // Keep a few weeks of history, not forever.
        sqlx::query(
            "DELETE FROM docs_syncs WHERE id NOT IN \
             (SELECT id FROM docs_syncs ORDER BY id DESC LIMIT 100)",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn last_success(&self) -> StoreResult<Option<DateTime<Utc>>> {
        Ok(
            sqlx::query_scalar("SELECT max(finished_at) FROM docs_syncs WHERE ok")
                .fetch_one(&self.pool)
                .await?,
        )
    }
}
