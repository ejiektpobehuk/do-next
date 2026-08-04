//! Shared HTTP retry behaviour.
//!
//! Until standup mode, no single user action fired more than a handful of
//! requests, so a 429 simply surfaced as a source error. A standup collects
//! across four backends and fans out per candidate, which is exactly the burst
//! shape Jira's cost-based limiter reacts to — hence one bounded retry helper
//! every client can send through.

use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::{RequestBuilder, Response, StatusCode, header::HeaderMap};

/// Retries after a 429. Deliberately small: the point is to ride out a brief
/// limiter trip, not to keep a wedged screen spinning.
const MAX_RETRIES: u32 = 2;

/// Ceiling on a single wait, even when `Retry-After` asks for longer. A standup
/// that takes a minute to appear is worse than one that reports what it got.
const MAX_BACKOFF_SECS: u64 = 10;

/// Send a request, retrying while the server answers 429.
///
/// Honours `Retry-After` when present, otherwise backs off 1s then 2s. Any
/// other status — including other errors — is returned to the caller untouched,
/// so existing error handling is unchanged.
pub async fn send_with_retry(req: RequestBuilder) -> Result<Response> {
    let mut attempt = 0;
    loop {
        // `try_clone` returns None only for streaming bodies, which none of the
        // retried calls use. Failing loudly beats silently not retrying.
        let this_attempt = req
            .try_clone()
            .context("request body is not replayable, cannot retry")?;
        let resp = this_attempt.send().await?;

        if resp.status() != StatusCode::TOO_MANY_REQUESTS || attempt >= MAX_RETRIES {
            return Ok(resp);
        }

        let wait = retry_after_secs(resp.headers())
            .unwrap_or(1u64 << attempt)
            .clamp(1, MAX_BACKOFF_SECS);
        log::warn!(
            "HTTP 429, retrying in {wait}s (attempt {}/{MAX_RETRIES})",
            attempt + 1
        );
        tokio::time::sleep(Duration::from_secs(wait)).await;
        attempt += 1;
    }
}

/// Seconds to wait per `Retry-After`.
///
/// Only the delta-seconds form is understood; the HTTP-date form is rare in
/// practice and a wrong parse would be worse than the caller's own backoff.
pub fn retry_after_secs(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderValue, RETRY_AFTER};

    fn headers_with(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, HeaderValue::from_str(value).expect("valid"));
        h
    }

    #[test]
    fn parses_delta_seconds() {
        assert_eq!(retry_after_secs(&headers_with("5")), Some(5));
        assert_eq!(retry_after_secs(&headers_with("  7 ")), Some(7));
    }

    #[test]
    fn ignores_http_date_and_garbage() {
        assert_eq!(
            retry_after_secs(&headers_with("Wed, 21 Oct 2026 07:28:00 GMT")),
            None
        );
        assert_eq!(retry_after_secs(&headers_with("soon")), None);
    }

    #[test]
    fn absent_header_is_none() {
        assert_eq!(retry_after_secs(&HeaderMap::new()), None);
    }
}
