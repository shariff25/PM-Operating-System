//! Keeps the index in step with the portal.
//!
//! One run:
//! 1. Read `robots.txt`. If it's missing (4xx), everything is allowed. If the portal can't
//!    say (5xx or no answer), the run stops and fetches nothing.
//! 2. Read the `llms.txt` index.
//! 3. Fetch each listed page's markdown, a few at a time with a pause between requests, as
//!    a conditional GET (`If-None-Match`), so unchanged pages cost a 304.
//! 4. Remove pages that left the index, unless the index shrank so much that it looks broken.
//!
//! A page that fails to fetch keeps its last good copy; one bad run never empties the index.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::{header, StatusCode};
use serde::Serialize;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::page::{parse_index, parse_page, product_of, DocPage, IndexEntry};
use crate::robots::Robots;
use crate::store::DocsStore;

/// Our token in `robots.txt` and the start of our user agent.
pub const AGENT: &str = "pmos-docs";

#[derive(Debug, Clone)]
pub struct SyncConfig {
    /// `https://support.acme.com`
    pub base: String,
    pub concurrency: usize,
    /// Minimum gap between request starts, across the whole sync (1 s = 1 request/second).
    /// Doubles, up to [`MAX_GAP`], each time the portal answers 429.
    pub delay: Duration,
    pub max_page_bytes: usize,
    /// Skip removals if fewer than this share of the stored pages would remain.
    pub min_keep_ratio: f64,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            base: "https://support.acme.com".into(),
            concurrency: 2,
            delay: Duration::from_millis(1000),
            max_page_bytes: 2 * 1024 * 1024,
            min_keep_ratio: 0.5,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SyncReport {
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    /// Pages in the index.
    pub listed: usize,
    /// Pages downloaded and stored (new or changed).
    pub fetched: usize,
    /// Pages the portal said hadn't changed (304).
    pub unchanged: usize,
    pub failed: usize,
    pub skipped_robots: usize,
    /// Pages that need a login (401/403). Not indexed: public docs only.
    pub restricted: usize,
    pub too_large: usize,
    pub removed: u64,
    /// Removals were skipped because the index looked too small.
    pub removal_skipped: bool,
    /// The first few errors, for the status page.
    pub errors: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("robots.txt could not be read ({0}); not fetching anything this run")]
    Robots(String),
    #[error("the documentation index could not be read: {0}")]
    Index(String),
    #[error(transparent)]
    Store(#[from] crate::store::StoreError),
}

const MAX_ERRORS: usize = 10;
/// The slowest pace the sync backs off to.
pub const MAX_GAP: Duration = Duration::from_secs(10);
/// Retries for a page the portal rate-limited.
pub const MAX_RETRIES: usize = 3;
/// Wait this long after a 429 without `Retry-After`; never longer than 5 minutes.
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(60);
const MAX_RETRY_AFTER: Duration = Duration::from_secs(300);

/// One pace for every worker: requests start at most once per `gap`, and a 429 holds
/// everyone back for its `Retry-After` and slows the pace.
struct Pacer {
    next: tokio::sync::Mutex<tokio::time::Instant>,
    gap: Mutex<Duration>,
}

impl Pacer {
    fn new(gap: Duration) -> Self {
        Self {
            next: tokio::sync::Mutex::new(tokio::time::Instant::now()),
            gap: Mutex::new(gap),
        }
    }

    /// Wait for this request's turn.
    async fn wait(&self) {
        let mut next = self.next.lock().await;
        tokio::time::sleep_until(*next).await;
        let gap = *self.gap.lock().expect("pacer lock");
        *next = tokio::time::Instant::now() + gap;
    }

    async fn back_off(&self, retry_after: Duration) {
        let mut next = self.next.lock().await;
        let until = tokio::time::Instant::now() + retry_after;
        if until > *next {
            *next = until;
        }
        let mut gap = self.gap.lock().expect("pacer lock");
        *gap = (*gap * 2).clamp(Duration::from_millis(100), MAX_GAP);
    }
}

fn retry_after(res: &reqwest::Response) -> Duration {
    res.headers()
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map_or(DEFAULT_RETRY_AFTER, Duration::from_secs)
        .min(MAX_RETRY_AFTER)
}

pub struct Syncer {
    config: SyncConfig,
    http: reqwest::Client,
    store: Arc<dyn DocsStore>,
}

enum Outcome {
    Fetched,
    Unchanged,
    Restricted,
    TooLarge,
    Failed(String),
}

impl Syncer {
    pub fn new(config: SyncConfig, store: Arc<dyn DocsStore>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(format!(
                "{AGENT}/{} (internal Acme PM OS docs index)",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .expect("http client builds");
        Self {
            config,
            http,
            store,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.config.base.trim_end_matches('/'))
    }

    async fn robots(&self) -> Result<Robots, SyncError> {
        let res = self
            .http
            .get(self.url("/robots.txt"))
            .send()
            .await
            .map_err(|e| SyncError::Robots(e.to_string()))?;
        let status = res.status();
        if status.is_success() {
            let text = res
                .text()
                .await
                .map_err(|e| SyncError::Robots(e.to_string()))?;
            Ok(Robots::parse(&text, AGENT))
        } else if status.is_client_error() {
            Ok(Robots::default())
        } else {
            Err(SyncError::Robots(status.to_string()))
        }
    }

    async fn index(&self) -> Result<Vec<IndexEntry>, SyncError> {
        let res = self
            .http
            .get(self.url("/llms.txt"))
            .send()
            .await
            .map_err(|e| SyncError::Index(e.to_string()))?;
        if !res.status().is_success() {
            return Err(SyncError::Index(res.status().to_string()));
        }
        let text = res
            .text()
            .await
            .map_err(|e| SyncError::Index(e.to_string()))?;
        Ok(parse_index(&text, &self.config.base))
    }

    pub async fn run(&self, now: DateTime<Utc>) -> Result<SyncReport, SyncError> {
        let mut report = SyncReport {
            started_at: Some(now),
            ..Default::default()
        };
        let robots = self.robots().await?;
        let entries = self.index().await?;
        report.listed = entries.len();
        let etags = Arc::new(self.store.etags().await?);

        let permits = Arc::new(Semaphore::new(self.config.concurrency.max(1)));
        let pacer = Arc::new(Pacer::new(self.config.delay));
        let mut tasks = JoinSet::new();
        let disallowed: Arc<Mutex<HashSet<String>>> = Arc::default();
        for entry in entries.iter().cloned() {
            let path = entry
                .url
                .strip_prefix(self.config.base.trim_end_matches('/'))
                .unwrap_or(&entry.url)
                .to_string();
            if !robots.allows(&path) {
                disallowed
                    .lock()
                    .expect("robots set")
                    .insert(entry.url.clone());
                report.skipped_robots += 1;
                continue;
            }
            let permits = permits.clone();
            let http = self.http.clone();
            let store = self.store.clone();
            let etags = etags.clone();
            let cfg = self.config.clone();
            let pacer = pacer.clone();
            tasks.spawn(async move {
                let _permit = permits.acquire_owned().await.expect("semaphore open");
                let outcome = fetch_one(
                    &http,
                    store.as_ref(),
                    &cfg,
                    &pacer,
                    &entry,
                    etags.get(&entry.url),
                    now,
                )
                .await;
                (entry.url, outcome)
            });
        }
        while let Some(done) = tasks.join_next().await {
            let (url, outcome) = done.map_err(|e| SyncError::Index(e.to_string()))?;
            match outcome {
                Outcome::Fetched => report.fetched += 1,
                Outcome::Unchanged => report.unchanged += 1,
                Outcome::Restricted => {
                    report.restricted += 1;
                    // Drop any copy from when the page was public.
                    disallowed.lock().expect("robots set").insert(url);
                }
                Outcome::TooLarge => {
                    report.too_large += 1;
                    if report.errors.len() < MAX_ERRORS {
                        report
                            .errors
                            .push(format!("{url}: larger than the size cap"));
                    }
                }
                Outcome::Failed(e) => {
                    report.failed += 1;
                    if report.errors.len() < MAX_ERRORS {
                        report.errors.push(format!("{url}: {e}"));
                    }
                }
            }
        }

        // Keep every listed page we're allowed to read, including ones that failed this run
        // (their last good copy stays). Pages gone from the index, or now disallowed, go.
        let disallowed = disallowed.lock().expect("robots set").clone();
        let keep: HashSet<String> = entries
            .iter()
            .map(|e| e.url.clone())
            .filter(|u| !disallowed.contains(u))
            .collect();
        let stored = self.store.count().await?;
        let plausible = !entries.is_empty()
            && (keep.len() as f64) >= self.config.min_keep_ratio * stored as f64;
        if plausible {
            report.removed = self.store.retain(&keep).await?;
        } else if stored > 0 {
            report.removal_skipped = true;
            tracing::warn!(
                listed = entries.len(),
                stored,
                "index looks too small; keeping existing pages this run"
            );
        }
        report.finished_at = Some(Utc::now());
        Ok(report)
    }
}

async fn fetch_one(
    http: &reqwest::Client,
    store: &dyn DocsStore,
    cfg: &SyncConfig,
    pacer: &Pacer,
    entry: &IndexEntry,
    etag: Option<&String>,
    now: DateTime<Utc>,
) -> Outcome {
    let mut attempt = 0;
    let res = loop {
        pacer.wait().await;
        let mut req = http.get(&entry.url);
        if let Some(etag) = etag {
            req = req.header(header::IF_NONE_MATCH, etag);
        }
        let res = match req.send().await {
            Ok(r) => r,
            Err(e) => return Outcome::Failed(e.to_string()),
        };
        if res.status() != StatusCode::TOO_MANY_REQUESTS {
            break res;
        }
        let wait = retry_after(&res);
        tracing::warn!(url = %entry.url, wait_s = wait.as_secs(), "rate limited; backing off");
        pacer.back_off(wait).await;
        attempt += 1;
        if attempt > MAX_RETRIES {
            return Outcome::Failed(format!("429 Too Many Requests after {MAX_RETRIES} retries"));
        }
    };
    match res.status() {
        StatusCode::NOT_MODIFIED => return Outcome::Unchanged,
        StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED => {
            // Login-only content: not indexed (public docs only).
            return Outcome::Restricted;
        }
        s if !s.is_success() => return Outcome::Failed(s.to_string()),
        _ => {}
    }
    if res
        .content_length()
        .is_some_and(|n| n as usize > cfg.max_page_bytes)
    {
        return Outcome::TooLarge;
    }
    let etag = res
        .headers()
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let bytes = match res.bytes().await {
        Ok(b) => b,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    if bytes.len() > cfg.max_page_bytes {
        return Outcome::TooLarge;
    }
    let text = String::from_utf8_lossy(&bytes);
    let parsed = parse_page(&text);
    let title = parsed.title.clone().unwrap_or_else(|| entry.title.clone());
    let slug = parsed.slug.clone().unwrap_or_else(|| {
        entry
            .url
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .trim_end_matches(".md")
            .to_string()
    });
    let page = DocPage {
        product: product_of(&title, &entry.url, parsed.api_title.as_deref()).to_string(),
        url: entry.url.clone(),
        slug,
        title,
        section: entry.section.clone(),
        description: entry.description.clone(),
        headings: parsed.headings,
        body: parsed.body,
        updated: parsed.updated,
        etag,
        fetched_at: now,
    };
    match store.upsert(&page).await {
        Ok(()) => Outcome::Fetched,
        Err(e) => Outcome::Failed(e.to_string()),
    }
}
