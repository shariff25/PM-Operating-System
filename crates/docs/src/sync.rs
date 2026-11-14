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
//!
//! The portal's content is untrusted input: redirects are followed only within the portal,
//! every body is read with a hard size cap, and a page must be served as markdown or text.

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
/// Caps on what the portal can make us read.
pub const MAX_ROBOTS_BYTES: usize = 512 * 1024;
pub const MAX_INDEX_BYTES: usize = 5 * 1024 * 1024;
/// Redirects followed per request, all within the portal.
const MAX_REDIRECTS: usize = 3;
/// More 401/403 answers than this in one run (at least 5, else 2% of the listed pages) look
/// like the portal blocking us rather than pages going private: keep every page.
fn restricted_storm(restricted: usize, listed: usize) -> bool {
    restricted > (listed / 50).max(5)
}
/// The slowest pace the sync backs off to.
pub const MAX_GAP: Duration = Duration::from_secs(10);
/// Retries for a page the portal rate-limited.
pub const MAX_RETRIES: usize = 3;
/// Wait this long after a 429 without `Retry-After`; never longer than 5 minutes.
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(60);
const MAX_RETRY_AFTER: Duration = Duration::from_secs(300);

/// One pace for every worker: requests start at most once per `gap`, and a 429 holds
/// everyone back for its `Retry-After` and slows the pace. Locks are never held while
/// sleeping, so a back-off applies at once, even to workers already waiting for a slot.
struct Pacer {
    next: Mutex<tokio::time::Instant>,
    hold_until: Mutex<tokio::time::Instant>,
    gap: Mutex<Duration>,
}

impl Pacer {
    fn new(gap: Duration) -> Self {
        let now = tokio::time::Instant::now();
        Self {
            next: Mutex::new(now),
            hold_until: Mutex::new(now),
            gap: Mutex::new(gap),
        }
    }

    /// Wait for this request's turn.
    async fn wait(&self) {
        loop {
            let slot = {
                let hold = *self.hold_until.lock().expect("pacer lock");
                let gap = *self.gap.lock().expect("pacer lock");
                let mut next = self.next.lock().expect("pacer lock");
                let slot = (*next).max(hold).max(tokio::time::Instant::now());
                *next = slot + gap;
                slot
            };
            tokio::time::sleep_until(slot).await;
            // A 429 while this worker slept moves everyone back; take a new slot.
            if *self.hold_until.lock().expect("pacer lock") <= tokio::time::Instant::now() {
                return;
            }
        }
    }

    fn back_off(&self, retry_after: Duration) {
        let until = tokio::time::Instant::now() + retry_after;
        {
            let mut hold = self.hold_until.lock().expect("pacer lock");
            if until > *hold {
                *hold = until;
            }
        }
        let mut gap = self.gap.lock().expect("pacer lock");
        *gap = (*gap * 2).clamp(Duration::from_millis(100), MAX_GAP);
    }
}

/// Read a body, refusing to buffer more than `max` bytes however the server sends it.
async fn read_capped(mut res: reqwest::Response, max: usize) -> Result<Vec<u8>, Capped> {
    if res.content_length().is_some_and(|n| n as usize > max) {
        return Err(Capped::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(|e| Capped::Io(e.to_string()))? {
        if body.len() + chunk.len() > max {
            return Err(Capped::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

enum Capped {
    TooLarge,
    Io(String),
}

impl Capped {
    fn describe(self) -> String {
        match self {
            Capped::TooLarge => "larger than the size cap".into(),
            Capped::Io(e) => e,
        }
    }
}

/// Markdown or plain text, as the portal serves its `.md` pages. A missing header is
/// accepted; HTML (a login page, an error page) is not.
fn is_text(res: &reqwest::Response) -> bool {
    match res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    {
        None => true,
        Some(ct) => {
            let mime = ct
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            matches!(
                mime.as_str(),
                "text/markdown" | "text/x-markdown" | "text/plain"
            )
        }
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
        // Follow redirects only within the portal (same scheme and host), so a hijacked page
        // can't point the crawler at an internal service.
        let base = reqwest::Url::parse(&config.base).ok();
        let redirects = reqwest::redirect::Policy::custom(move |attempt| {
            let same_site = base.as_ref().is_some_and(|b| {
                attempt.url().scheme() == b.scheme()
                    && attempt.url().host_str() == b.host_str()
                    && attempt.url().port_or_known_default() == b.port_or_known_default()
            });
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error("too many redirects")
            } else if same_site {
                attempt.follow()
            } else {
                attempt.error("redirect outside the portal")
            }
        });
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .redirect(redirects)
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
            let bytes = read_capped(res, MAX_ROBOTS_BYTES)
                .await
                .map_err(|e| SyncError::Robots(e.describe()))?;
            Ok(Robots::parse(&String::from_utf8_lossy(&bytes), AGENT))
        } else if status == StatusCode::TOO_MANY_REQUESTS {
            // Not "missing": the portal is asking us to slow down. Don't crawl this run.
            Err(SyncError::Robots(status.to_string()))
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
        let bytes = read_capped(res, MAX_INDEX_BYTES)
            .await
            .map_err(|e| SyncError::Index(e.describe()))?;
        Ok(parse_index(
            &String::from_utf8_lossy(&bytes),
            &self.config.base,
        ))
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
        let mut restricted = HashSet::new();
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
                    restricted.insert(url);
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

        // A page that now needs a login loses any copy from when it was public, unless so
        // many did at once that it looks like the portal is blocking us.
        let mut disallowed = disallowed.lock().expect("robots set").clone();
        if restricted_storm(restricted.len(), entries.len()) {
            tracing::warn!(
                restricted = restricted.len(),
                listed = entries.len(),
                "many pages answered 401/403; keeping their last copies this run"
            );
            report.errors.insert(
                0,
                format!(
                    "{} pages answered 401/403 at once: that looks like the portal blocking the crawler, so their last copies were kept",
                    restricted.len()
                ),
            );
            report.errors.truncate(MAX_ERRORS);
        } else {
            disallowed.extend(restricted);
        }

        // Keep every listed page we're allowed to read, including ones that failed this run
        // (their last good copy stays). Pages gone from the index, now disallowed, or now
        // login-only go.
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
        pacer.back_off(wait);
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
    if !is_text(&res) {
        // Such as an HTML login or error page served with 200: keep the last good copy.
        let ct = res
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        return Outcome::Failed(format!("served as `{ct}`, not markdown"));
    }
    let etag = res
        .headers()
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let bytes = match read_capped(res, cfg.max_page_bytes).await {
        Ok(b) => b,
        Err(Capped::TooLarge) => return Outcome::TooLarge,
        Err(Capped::Io(e)) => return Outcome::Failed(e),
    };
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
