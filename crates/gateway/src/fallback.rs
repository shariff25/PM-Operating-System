//! What moves a routed role's call to its next hop (`docs/model-providers-spec.md` §4.4, §4.5),
//! slice MP3. Only failures on the upstream's side count: a client's own bad request, and a
//! model's refusal, are answers and are never routed around.
//!
//! The pure rules are here ([`classify_status`], [`model_unavailable`], [`serves`]) beside
//! [`first_content`], which holds a streamed answer's events until the first content (the
//! commit point): before it a failure may move to the next hop, after it nothing is repeated
//! and no other model's output is ever joined to it. The hop loop itself is `app::forward_routed`.

use std::time::Duration;

use axum::body::Bytes;
use axum::http::HeaderMap;
use futures_util::StreamExt;
use serde_json::Value;

use crate::route::{Feature, Hop};

/// Whether the same hop may be tried again for this failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Same {
    No,
    /// After the upstream's own `Retry-After`.
    After(Duration),
    /// After a backoff with jitter (`RetryPolicy`'s).
    Backoff,
}

/// What a failure does to a circuit (§4.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// Not counted (a short `Retry-After`).
    None,
    /// Counted: `circuit.failures` in a row open it.
    Count,
    /// Opens it until the `Retry-After` passes.
    OpenFor(Duration),
    /// Opens it at once, for its cool-down.
    OpenNow,
    /// Opens it at once, for `max-cool-down-secs`.
    OpenMax,
}

/// Which circuit a failure counts against: the upstream's covers all its models (connection,
/// key, redirect); the model's covers that model only (§4.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Model,
    Upstream,
}

/// A failure on the upstream's side: the next hop may be tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failure {
    /// For the usage log (§4.9): `connect`, `timeout`, `rate_limit`, `quota_exhausted`,
    /// `overloaded`, `server_error`, `model_unavailable`, `auth`, `redirect`, `bad_answer`,
    /// `first_token`.
    pub reason: &'static str,
    pub same: Same,
    pub hit: Hit,
    pub scope: Scope,
}

impl Failure {
    pub const fn new(reason: &'static str, same: Same, hit: Hit, scope: Scope) -> Self {
        Self {
            reason,
            same,
            hit,
            scope,
        }
    }
}

/// What a non-`2xx` status means for the route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The caller's own error (or one that another model would give the same): returned to the
    /// caller, never routed around.
    Answer,
    Fail(Failure),
}

/// §4.4, for an answer with a status line that isn't `2xx`. `body` is the upstream's own,
/// read before any neutral body replaces it; `short` is `routing.short-retry-after-secs`.
pub fn classify_status(status: u16, headers: &HeaderMap, body: &[u8], short: Duration) -> Verdict {
    use Hit::*;
    use Same::*;
    use Scope::*;
    let fail = |r, s, h, sc| Verdict::Fail(Failure::new(r, s, h, sc));
    match status {
        401 | 403 => fail("auth", No, OpenMax, Upstream),
        300..=399 => fail("redirect", No, OpenNow, Upstream),
        429 | 503 | 529 => {
            if crate::retry::unrecoverable(body) {
                return fail("quota_exhausted", No, OpenMax, Model);
            }
            let reason = if status == 429 {
                "rate_limit"
            } else {
                "overloaded"
            };
            match crate::retry::retry_after(headers, chrono::Utc::now()) {
                Some(d) if d <= short => fail(reason, After(d), None, Model),
                Some(d) => fail(reason, No, OpenFor(d), Model),
                Option::None => fail(reason, Backoff, Count, Model),
            }
        }
        404 => fail("model_unavailable", No, OpenNow, Model),
        400 | 422 if model_unavailable(body) => fail("model_unavailable", No, OpenNow, Model),
        500..=599 => fail("server_error", No, Count, Model),
        _ => Verdict::Answer,
    }
}

/// Whether a `400` or `422` body says the model itself is not found, retired or unavailable
/// (a vendor-neutral list, read before the body is replaced): such an answer moves to the next
/// hop, where any other `400` is the caller's. Only phrases about the model count, so "context
/// length exceeded" or a schema error stays the caller's.
pub fn model_unavailable(body: &[u8]) -> bool {
    let lower = String::from_utf8_lossy(body).to_ascii_lowercase();
    if lower.contains("model_not_found") || lower.contains("model_unavailable") {
        return true;
    }
    lower.contains("model")
        && [
            "not found",
            "does not exist",
            "no such model",
            "not available",
            "unavailable",
            "retired",
            "decommissioned",
            "deprecated",
        ]
        .iter()
        .any(|p| lower.contains(p))
}

/// Whether a hop may be sent this request (§4.8): a request with `tools` needs `tools`,
/// `response_format` needs `json`, `stream: true` needs `stream` unless the hop's workarounds
/// already ask for the whole answer, and embeddings need `embeddings`. A hop is never sent a
/// request it declares it can't serve.
pub fn serves(
    hop: &Hop,
    embeddings: bool,
    req: &Value,
    stream: bool,
    workarounds_on: bool,
) -> bool {
    if embeddings {
        return hop.has(Feature::Embeddings) && hop.kind != crate::config::UpstreamKind::ClaudeTest;
    }
    let tools = req["tools"].as_array().is_some_and(|t| !t.is_empty());
    if tools && !hop.has(Feature::Tools) {
        return false;
    }
    if !req["response_format"].is_null() && !hop.has(Feature::Json) {
        return false;
    }
    let whole = crate::workarounds::needs_whole_answer(&hop.workarounds, workarounds_on)
        || hop.kind == crate::config::UpstreamKind::ClaudeTest;
    !stream || whole || hop.has(Feature::Stream)
}

/// The most a stream's events before its first content may hold (§4.5).
pub const HOLD_MAX: usize = 64 * 1024;

/// Why a stream never reached its first content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoFirst {
    /// `first-token-secs` passed.
    Timeout,
    /// It ended, or sent `[DONE]`, before any content.
    Ended,
    /// It broke off before any content.
    Broken,
    /// An `error` event before any content.
    ErrorEvent,
    /// More than [`HOLD_MAX`] before any content.
    TooMuch,
}

type Upstream = std::pin::Pin<Box<dyn futures_util::Stream<Item = reqwest::Result<Bytes>> + Send>>;

/// Holds a streamed answer's events until one carries a non-empty `content`,
/// `reasoning_content` or `tool_calls` delta, or a finish reason (the commit point, §4.5), for
/// at most `limit` and [`HOLD_MAX`] bytes. `Ok`: everything held (to be sent first) and the
/// rest of the stream. Role-only and keep-alive events before the commit point are held, not
/// sent: until then the caller has received nothing, not even headers.
pub async fn first_content(
    mut up: Upstream,
    limit: Duration,
) -> Result<(Bytes, Upstream), NoFirst> {
    let deadline = tokio::time::Instant::now() + limit;
    let mut held: Vec<u8> = Vec::new();
    let mut scanned = 0usize;
    loop {
        let chunk = match tokio::time::timeout_at(deadline, up.next()).await {
            Err(_) => return Err(NoFirst::Timeout),
            Ok(None) => return Err(NoFirst::Ended),
            Ok(Some(Err(_))) => return Err(NoFirst::Broken),
            Ok(Some(Ok(b))) => b,
        };
        held.extend_from_slice(&chunk);
        // Whole lines only; a partial one waits for its end.
        while let Some(end) = held[scanned..].iter().position(|b| *b == b'\n') {
            let line = &held[scanned..scanned + end];
            scanned += end + 1;
            match event_kind(line) {
                Event::Content => return Ok((Bytes::from(held), up)),
                Event::Error => return Err(NoFirst::ErrorEvent),
                Event::Done => return Err(NoFirst::Ended),
                Event::Other => {}
            }
        }
        if held.len() > HOLD_MAX {
            return Err(NoFirst::TooMuch);
        }
    }
}

enum Event {
    Content,
    Error,
    Done,
    Other,
}

fn event_kind(line: &[u8]) -> Event {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let Some(data) = line.strip_prefix(b"data:") else {
        return Event::Other;
    };
    let data = data.trim_ascii();
    if data == b"[DONE]" {
        return Event::Done;
    }
    let Ok(v) = serde_json::from_slice::<Value>(data) else {
        return Event::Other;
    };
    if v.get("error").is_some_and(|e| !e.is_null()) {
        return Event::Error;
    }
    let content = v["choices"].as_array().into_iter().flatten().any(|c| {
        let d = &c["delta"];
        let text = |k: &str| d[k].as_str().is_some_and(|s| !s.is_empty());
        text("content")
            || text("reasoning_content")
            || d["tool_calls"].as_array().is_some_and(|t| !t.is_empty())
            || !c["finish_reason"].is_null()
    });
    if content {
        Event::Content
    } else {
        Event::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn ra(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("retry-after", HeaderValue::from_str(v).unwrap());
        h
    }

    #[test]
    fn every_row_of_the_table_classifies_as_the_spec_says() {
        let none = HeaderMap::new();
        let short = Duration::from_secs(5);
        let f = |s, h: &HeaderMap, b: &[u8]| match classify_status(s, h, b, short) {
            Verdict::Fail(f) => Some(f),
            Verdict::Answer => None,
        };
        // 429 with a short Retry-After: same hop after it, not counted.
        let x = f(429, &ra("2"), b"{}").unwrap();
        assert_eq!(
            (x.reason, x.same, x.hit),
            ("rate_limit", Same::After(Duration::from_secs(2)), Hit::None)
        );
        // 429 with a longer one: next hop at once, open until it passes.
        let x = f(429, &ra("60"), b"{}").unwrap();
        assert_eq!(
            (x.same, x.hit),
            (Same::No, Hit::OpenFor(Duration::from_secs(60)))
        );
        // 429 without one: backoff, counted.
        let x = f(429, &none, b"{}").unwrap();
        assert_eq!((x.same, x.hit), (Same::Backoff, Hit::Count));
        // A quota that won't recover.
        let x = f(429, &none, br#"{"error":{"code":"insufficient_quota"}}"#).unwrap();
        assert_eq!(
            (x.reason, x.same, x.hit),
            ("quota_exhausted", Same::No, Hit::OpenMax)
        );
        for s in [503, 529] {
            assert_eq!(f(s, &none, b"").unwrap().reason, "overloaded");
        }
        for s in [500, 502, 504, 599] {
            let x = f(s, &none, b"").unwrap();
            assert_eq!(
                (x.reason, x.same, x.hit),
                ("server_error", Same::No, Hit::Count)
            );
        }
        let x = f(404, &none, b"").unwrap();
        assert_eq!(
            (x.reason, x.hit, x.scope),
            ("model_unavailable", Hit::OpenNow, Scope::Model)
        );
        let x = f(
            400,
            &none,
            br#"{"error":{"message":"The model `x` does not exist"}}"#,
        )
        .unwrap();
        assert_eq!(x.reason, "model_unavailable");
        assert_eq!(
            f(422, &none, b"model_not_found").unwrap().reason,
            "model_unavailable"
        );
        for s in [401, 403] {
            let x = f(s, &none, b"").unwrap();
            assert_eq!(
                (x.reason, x.hit, x.scope),
                ("auth", Hit::OpenMax, Scope::Upstream)
            );
        }
        for s in [301, 302, 307, 308] {
            let x = f(s, &none, b"").unwrap();
            assert_eq!(
                (x.reason, x.hit, x.scope),
                ("redirect", Hit::OpenNow, Scope::Upstream)
            );
        }
        // The caller's own errors are answers, never routed around.
        for (s, b) in [
            (400, &b"maximum context length exceeded"[..]),
            (400, b"invalid tool schema"),
            (413, b""),
            (422, b"schema"),
            (409, b""),
        ] {
            assert!(f(s, &none, b).is_none(), "{s}");
        }
    }

    fn stream_of(chunks: Vec<&'static [u8]>) -> Upstream {
        Box::pin(futures_util::stream::iter(
            chunks.into_iter().map(|c| Ok(Bytes::from_static(c))),
        ))
    }

    #[tokio::test]
    async fn the_commit_point_is_the_first_content() {
        let t = Duration::from_secs(5);
        // Role-only and keep-alive events are held until the content.
        let (held, _) = first_content(
            stream_of(vec![
                b": keep-alive\n\n",
                b"data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n",
                b"data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n",
            ]),
            t,
        )
        .await
        .unwrap();
        assert!(held.ends_with(b"\"Hi\"}}]}\n\n"));
        // A tool call counts as content; so does a finish reason.
        assert!(first_content(
            stream_of(vec![
                b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0}]}}]}\n"
            ]),
            t
        )
        .await
        .is_ok());
        assert!(first_content(
            stream_of(vec![
                b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n"
            ]),
            t
        )
        .await
        .is_ok());
        // Ends, [DONE], an error event, or nothing but empty deltas: no commit.
        let ended = |c| async move { first_content(stream_of(c), t).await.err() };
        assert_eq!(
            ended(vec![
                b"data: {\"choices\":[{\"delta\":{\"content\":\"\"}}]}\n"
            ])
            .await,
            Some(NoFirst::Ended)
        );
        assert_eq!(ended(vec![b"data: [DONE]\n"]).await, Some(NoFirst::Ended));
        assert_eq!(
            ended(vec![b"data: {\"error\":{\"message\":\"x\"}}\n"]).await,
            Some(NoFirst::ErrorEvent)
        );
        let big: &'static [u8] = Box::leak(
            format!(": {}\n", "x".repeat(HOLD_MAX))
                .into_bytes()
                .into_boxed_slice(),
        );
        assert_eq!(ended(vec![big]).await, Some(NoFirst::TooMuch));
        // Silence past first-token-secs.
        let silent: Upstream = Box::pin(futures_util::stream::pending());
        assert_eq!(
            first_content(silent, Duration::from_millis(50)).await.err(),
            Some(NoFirst::Timeout)
        );
    }
}
