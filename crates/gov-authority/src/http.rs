//! The Authority's listener for gates (spec §7.6.3, point 11): one route, a gate's signed
//! request to renew. No admin action and no sign-in is served here.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use gov_core::Envelope;

use crate::{Authority, AuthorityError, Store};

/// A request is one signed envelope; an answer carries a whole bundle and the feed.
pub const MAX_REQUEST: usize = 64 * 1024;
pub const MAX_ANSWER: usize = 8 * 1024 * 1024;

/// Requests answered for one source address in one second, and the most sources
/// remembered in one second. Counted before a key is looked up or a signature checked
/// (review findings GR-169 and GR-174).
#[derive(Debug, Clone, Copy)]
pub struct SourceLimit {
    pub per_second: u32,
    pub sources: usize,
}

impl Default for SourceLimit {
    fn default() -> Self {
        SourceLimit {
            per_second: 20,
            sources: 4096,
        }
    }
}

#[derive(Debug, Default)]
pub struct Counters {
    pub renewed: AtomicU64,
    pub refused: AtomicU64,
    pub limited: AtomicU64,
    pub failed: AtomicU64,
}

pub struct Listener<S> {
    pub authority: Authority<S>,
    pub clock: fn() -> i64,
    pub limit: SourceLimit,
    pub counters: Counters,
    seen: Mutex<(i64, HashMap<IpAddr, u32>)>,
}

impl<S> Listener<S> {
    pub fn new(authority: Authority<S>, clock: fn() -> i64, limit: SourceLimit) -> Self {
        Listener {
            authority,
            clock,
            limit,
            counters: Counters::default(),
            seen: Mutex::default(),
        }
    }

    fn within_limit(&self, source: IpAddr, now: i64) -> bool {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        if seen.0 != now {
            *seen = (now, HashMap::new());
        }
        let sources = seen.1.len();
        match seen.1.get_mut(&source) {
            Some(count) if *count >= self.limit.per_second => false,
            Some(count) => {
                *count += 1;
                true
            }
            None if sources >= self.limit.sources => false,
            None => {
                seen.1.insert(source, 1);
                true
            }
        }
    }
}

/// Serve with `into_make_service_with_connect_info::<SocketAddr>()`: the limit is by the
/// caller's address.
pub fn router<S: Store + 'static>(listener: Arc<Listener<S>>) -> Router {
    Router::new()
        .route("/v1/renew", post(renew::<S>))
        .layer(DefaultBodyLimit::max(MAX_REQUEST))
        .with_state(listener)
}

/// Whatever is wrong with a request, its sender learns only that it was refused.
async fn renew<S: Store + 'static>(
    State(l): State<Arc<Listener<S>>>,
    ConnectInfo(from): ConnectInfo<SocketAddr>,
    body: Bytes,
) -> Response {
    let now = (l.clock)();
    if !l.within_limit(from.ip(), now) {
        l.counters.limited.fetch_add(1, Ordering::Relaxed);
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let refused = || {
        l.counters.refused.fetch_add(1, Ordering::Relaxed);
        StatusCode::FORBIDDEN.into_response()
    };
    let Ok(request) = serde_json::from_slice::<Envelope>(&body) else {
        return refused();
    };
    match l.authority.renew(&request, now).await {
        Ok(answer) => match serde_json::to_vec(&answer) {
            Ok(bytes) if bytes.len() <= MAX_ANSWER => {
                l.counters.renewed.fetch_add(1, Ordering::Relaxed);
                ([(header::CONTENT_TYPE, "application/json")], bytes).into_response()
            }
            _ => {
                l.counters.failed.fetch_add(1, Ordering::Relaxed);
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        },
        Err(AuthorityError::Refused) => refused(),
        Err(AuthorityError::TooSoon) => {
            l.counters.limited.fetch_add(1, Ordering::Relaxed);
            StatusCode::TOO_MANY_REQUESTS.into_response()
        }
        Err(_) => {
            l.counters.failed.fetch_add(1, Ordering::Relaxed);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
