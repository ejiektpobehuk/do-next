//! Shared HTTP behaviour: bounded retries, and one place that turns a
//! response into JSON — or into an error a user can act on.
//!
//! Until standup mode, no single user action fired more than a handful of
//! requests, so a 429 simply surfaced as a source error. A standup collects
//! across four backends and fans out per candidate, which is exactly the burst
//! shape Jira's cost-based limiter reacts to — hence one bounded retry helper
//! every client can send through.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::{RequestBuilder, Response, StatusCode, Url, header::HeaderMap};

/// Retries after a 429. Deliberately small: the point is to ride out a brief
/// limiter trip, not to keep a wedged screen spinning.
const MAX_RETRIES: u32 = 2;

/// Ceiling on a single wait, even when `Retry-After` asks for longer. A standup
/// that takes a minute to appear is worse than one that reports what it got.
const MAX_BACKOFF_SECS: u64 = 10;

/// Longest body excerpt carried into an error message. Enough to recognise a
/// real API error payload, short enough to read in an error pane.
const MAX_BODY_SNIPPET: usize = 300;

/// What answered instead of the API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Interception {
    /// A named identity-aware proxy (Cloudflare Access and friends).
    Gate(&'static str),
    /// An HTML page from somewhere — a captive portal, a proxy notice, or an
    /// unrecognised login screen.
    Html,
}

/// Read a response as JSON, or fail with an error that says what actually
/// answered.
///
/// The failure worth special-casing: a self-hosted instance sitting behind an
/// identity-aware proxy. With the VPN off, the proxy answers every API call
/// with its own login page — HTTP 200, HTML body — and a plain
/// `serde` error ("expected value at line 1 column 1") plus the redirect URL
/// (a screenful of signed JWT) tells the user nothing about what to do.
///
/// `requested` is the URL we asked for; the response knows where it landed.
pub async fn json_response<T: serde::de::DeserializeOwned>(
    service: &str,
    requested: &str,
    resp: Response,
) -> Result<T> {
    let status = resp.status();
    let landed = resp.url().clone();
    let interception = intercepted_by(&landed, resp.headers());
    let body = resp.text().await.unwrap_or_default();

    if !status.is_success() {
        log::error!("{service} API error {status} for {requested}: {body}");
        if let Some(interception) = interception {
            bail!(explain(
                service,
                requested,
                &landed,
                interception,
                Some(status)
            ));
        }
        bail!("{service} API error {status}: {}", snippet(&body));
    }

    match serde_json::from_str::<T>(&body) {
        Ok(value) => Ok(value),
        Err(e) => {
            log::error!("{service} response for {requested} is not the expected JSON: {body}");
            if let Some(interception) = interception {
                bail!(explain(service, requested, &landed, interception, None));
            }
            Err(e).with_context(|| {
                format!(
                    "{service} answered {requested} with something other than \
                     the expected JSON: {}",
                    snippet(&body)
                )
            })
        }
    }
}

/// Recognise a response that came from an access gateway rather than the API.
fn intercepted_by(landed: &Url, headers: &HeaderMap) -> Option<Interception> {
    if let Some(name) = gateway_name(landed) {
        return Some(Interception::Gate(name));
    }
    // Cloudflare stamps this on its own challenge/block responses, which can
    // be served from the API host itself.
    if headers.contains_key("cf-mitigated") {
        return Some(Interception::Gate("Cloudflare"));
    }
    is_html(headers).then_some(Interception::Html)
}

/// The access product serving this URL, if we recognise it. Matched on the
/// URL we were redirected *to*, so a bare hostname match is the signal.
fn gateway_name(url: &Url) -> Option<&'static str> {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    // Cloudflare Access can serve its login page from the app's own hostname,
    // where only the reserved `/cdn-cgi/access/` path gives it away.
    if host.ends_with("cloudflareaccess.com") || url.path().starts_with("/cdn-cgi/access/") {
        return Some("Cloudflare Access");
    }
    if host == "login.microsoftonline.com" || host == "login.microsoft.com" {
        return Some("Microsoft Entra ID");
    }
    if host.ends_with(".okta.com") || host.ends_with(".okta-emea.com") {
        return Some("Okta");
    }
    if host == "accounts.google.com" {
        return Some("Google sign-in");
    }
    None
}

fn is_html(headers: &HeaderMap) -> bool {
    headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| {
            let ct = ct.to_ascii_lowercase();
            ct.starts_with("text/html") || ct.starts_with("application/xhtml")
        })
}

/// The user-facing explanation. Names the gate, says why the token is not the
/// problem, and gives the one action that fixes it.
fn explain(
    service: &str,
    requested: &str,
    landed: &Url,
    interception: Interception,
    status: Option<StatusCode>,
) -> String {
    let code = status.map_or_else(String::new, |s| format!(" (HTTP {})", s.as_u16()));
    match interception {
        Interception::Gate(name) => format!(
            "{service} is behind {name}: the request to {requested} was answered by a \
             sign-in page at {}{code}, not by {service}. That gate checks your network \
             session, not your API token — connect to the VPN or sign in with your \
             access client and try again.",
            landed.host_str().unwrap_or("another host"),
        ),
        // Unlike a named gate, an unrecognised HTML answer is ambiguous: a
        // VPN/SSO interstitial and a rejected credential both look like this,
        // so name both instead of sending the user after the wrong one.
        Interception::Html => format!(
            "{service} answered {requested} with an HTML page instead of JSON{code} — \
             usually a sign-in page. Either the instance sits behind a VPN or SSO \
             gateway (connect to it and try again) or the credentials were rejected; \
             if neither fits, check the configured base URL.",
        ),
    }
}

/// A body excerpt for an error message: one line, bounded length.
fn snippet(body: &str) -> String {
    let flat = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return "(empty body)".to_string();
    }
    if flat.chars().count() > MAX_BODY_SNIPPET {
        let short: String = flat.chars().take(MAX_BODY_SNIPPET).collect();
        format!("{short}…")
    } else {
        flat
    }
}

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
    use reqwest::header::{CONTENT_TYPE, HeaderValue, RETRY_AFTER};

    fn url(s: &str) -> Url {
        Url::parse(s).expect("valid url")
    }

    fn content_type(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(CONTENT_TYPE, HeaderValue::from_str(value).expect("valid"));
        h
    }

    // ── access-gate detection ─────────────────────────────────────────────

    /// The real shape of the failure: a GitLab API call redirected to a
    /// Cloudflare Access login page, JWT-laden query string and all.
    #[test]
    fn cloudflare_access_login_page_is_recognised() {
        let landed = url(
            "https://acme-prd.cloudflareaccess.com/cdn-cgi/access/login/gitlab.acme.com\
             ?kid=c5cf19&meta=eyJ0eXAiOiJKV1Qi&redirect_url=%2Fapi%2Fv4%2Fuser",
        );
        assert_eq!(
            intercepted_by(&landed, &content_type("text/html; charset=utf-8")),
            Some(Interception::Gate("Cloudflare Access"))
        );
        // Same gate served from the app's own hostname.
        assert_eq!(
            gateway_name(&url("https://gitlab.acme.com/cdn-cgi/access/login/x")),
            Some("Cloudflare Access")
        );
    }

    #[test]
    fn other_sso_login_hosts_are_named() {
        assert_eq!(
            gateway_name(&url(
                "https://login.microsoftonline.com/common/oauth2/authorize"
            )),
            Some("Microsoft Entra ID")
        );
        assert_eq!(
            gateway_name(&url("https://acme.okta.com/login")),
            Some("Okta")
        );
        assert_eq!(
            gateway_name(&url("https://accounts.google.com/ServiceLogin")),
            Some("Google sign-in")
        );
    }

    #[test]
    fn a_plain_api_url_is_not_a_gate() {
        assert_eq!(
            gateway_name(&url("https://gitlab.acme.com/api/v4/user")),
            None
        );
        assert_eq!(
            intercepted_by(
                &url("https://gitlab.acme.com/api/v4/user"),
                &content_type("application/json")
            ),
            None
        );
    }

    #[test]
    fn a_cloudflare_challenge_header_counts_even_without_a_redirect() {
        let mut headers = content_type("application/json");
        headers.insert("cf-mitigated", HeaderValue::from_static("challenge"));
        assert_eq!(
            intercepted_by(&url("https://gitlab.acme.com/api/v4/user"), &headers),
            Some(Interception::Gate("Cloudflare"))
        );
    }

    #[test]
    fn an_unrecognised_html_answer_falls_back_to_the_generic_case() {
        assert_eq!(
            intercepted_by(
                &url("https://gitlab.acme.com/users/sign_in"),
                &content_type("text/html")
            ),
            Some(Interception::Html)
        );
    }

    // ── the message ───────────────────────────────────────────────────────

    #[test]
    fn gate_message_names_the_gate_and_the_fix_not_the_token() {
        let msg = explain(
            "GitLab",
            "https://gitlab.acme.com/api/v4/user",
            &url("https://acme-prd.cloudflareaccess.com/cdn-cgi/access/login/gitlab?kid=abc"),
            Interception::Gate("Cloudflare Access"),
            None,
        );
        assert!(msg.contains("GitLab is behind Cloudflare Access"), "{msg}");
        assert!(msg.contains("acme-prd.cloudflareaccess.com"), "{msg}");
        assert!(msg.contains("connect to the VPN"), "{msg}");
        // The signed redirect query is exactly the noise this replaces.
        assert!(!msg.contains("kid=abc"), "{msg}");
    }

    #[test]
    fn html_message_mentions_the_status_when_there_is_one() {
        let msg = explain(
            "Grafana OnCall",
            "https://grafana.acme.com/api/v1/users/current",
            &url("https://grafana.acme.com/login"),
            Interception::Html,
            Some(StatusCode::FORBIDDEN),
        );
        assert!(msg.contains("(HTTP 403)"), "{msg}");
        assert!(msg.contains("HTML page instead of JSON"), "{msg}");
    }

    // ── body snippets ─────────────────────────────────────────────────────

    /// End to end over a real socket: the API path answers with a redirect to
    /// an access-login path, exactly as Cloudflare Access does, and the client
    /// follows it into an HTML page.
    #[tokio::test]
    async fn a_redirect_into_an_access_login_page_explains_itself() {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind");
        let port = server.server_addr().to_ip().expect("ip addr").port();
        let handle = std::thread::spawn(move || {
            for _ in 0..2 {
                let Ok(req) = server.recv() else { return };
                let login = "/cdn-cgi/access/login/gitlab.acme.com?kid=abc&meta=eyJ0eXAi";
                if req.url().starts_with("/api/") {
                    let redirect = tiny_http::Response::empty(302).with_header(
                        tiny_http::Header::from_bytes(&b"Location"[..], login.as_bytes())
                            .expect("header"),
                    );
                    let _ = req.respond(redirect);
                } else {
                    let page = tiny_http::Response::from_string("<html>Sign in</html>")
                        .with_header(
                            tiny_http::Header::from_bytes(
                                &b"Content-Type"[..],
                                &b"text/html; charset=utf-8"[..],
                            )
                            .expect("header"),
                        );
                    let _ = req.respond(page);
                }
            }
        });

        let requested = format!("http://127.0.0.1:{port}/api/v4/user");
        let resp = reqwest::Client::new()
            .get(&requested)
            .send()
            .await
            .expect("request");
        let err = json_response::<serde_json::Value>("GitLab", &requested, resp)
            .await
            .expect_err("an HTML login page is not JSON");
        let msg = format!("{err:#}");

        assert!(msg.contains("GitLab is behind Cloudflare Access"), "{msg}");
        assert!(msg.contains("connect to the VPN"), "{msg}");
        // None of the serde noise, and none of the signed redirect query.
        assert!(!msg.contains("expected value at line 1"), "{msg}");
        assert!(!msg.contains("kid=abc"), "{msg}");
        handle.join().expect("server thread");
    }

    #[test]
    fn snippet_flattens_and_bounds_the_body() {
        assert_eq!(
            snippet("  {\"error\":\n  \"nope\"}  "),
            "{\"error\": \"nope\"}"
        );
        assert_eq!(snippet("   "), "(empty body)");
        let long = "a".repeat(MAX_BODY_SNIPPET + 50);
        let short = snippet(&long);
        assert_eq!(short.chars().count(), MAX_BODY_SNIPPET + 1);
        assert!(short.ends_with('\u{2026}'));
    }

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
