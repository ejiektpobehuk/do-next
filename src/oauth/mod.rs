//! Shared OAuth 2.0 flow plumbing: PKCE, CSRF state, percent-encoding, the
//! loopback callback listener, and the bridge that lets a synchronous flow
//! make HTTP calls from inside a Tokio runtime.
//!
//! Both the Atlassian flow (`crate::atlassian::oauth`) and the GitLab flow
//! (`crate::gitlab::oauth`) build on this, so the security-sensitive parts —
//! verifier generation, exact `state` comparison, single-shot listener — exist
//! once and are tested once.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngExt;
use sha2::{Digest, Sha256};

/// A PKCE verifier/challenge pair (S256).
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

/// Generate a PKCE verifier and its S256 challenge.
///
/// 32 random bytes base64url-encoded gives a 43-character verifier, which sits
/// at the bottom of RFC 7636's 43–128 range.
pub fn pkce() -> Pkce {
    let bytes: [u8; 32] = rand::rng().random();
    let verifier = URL_SAFE_NO_PAD.encode(bytes);
    let challenge = challenge_for(&verifier);
    Pkce {
        verifier,
        challenge,
    }
}

/// The S256 challenge for a verifier. Split out so the RFC 7636 test vector
/// can pin it.
fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Random CSRF `state` value, hex-encoded.
pub fn state() -> String {
    let bytes: [u8; 16] = rand::rng().random();
    hex_encode(&bytes)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(char::from(b"0123456789abcdef"[(b >> 4) as usize]));
        s.push(char::from(b"0123456789abcdef"[(b & 0x0F) as usize]));
    }
    s
}

/// Percent-encode a value for use in a URL query string or as a single path
/// segment. Everything outside the RFC 3986 unreserved set is escaped, so this
/// is safe for both.
pub fn percent_encode(s: &str) -> String {
    use std::fmt::Write;

    let mut out = String::with_capacity(s.len() * 2);
    for byte in s.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            // Writing to a String is infallible.
            other => {
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}

/// Run a future to completion from synchronous code that is itself running
/// inside a Tokio runtime.
///
/// The interactive flows are synchronous (they block on stdin and on a browser
/// round trip) but are called from `#[tokio::main]`, so they cannot simply
/// `block_on`. A dedicated thread with its own current-thread runtime keeps the
/// two from colliding.
pub fn blocking_http<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> std::pin::Pin<Box<dyn Future<Output = Result<T>>>> + Send + 'static,
    T: Send + 'static,
{
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("Failed to build HTTP runtime")?;
        rt.block_on(f())
    })
    .join()
    .map_err(|_| anyhow::anyhow!("OAuth HTTP thread panicked"))?
}

/// A one-shot loopback HTTP listener for an OAuth redirect.
pub struct LoopbackServer {
    server: tiny_http::Server,
    port: u16,
}

impl LoopbackServer {
    /// Bind `127.0.0.1:preferred`, falling back to an OS-assigned port when it
    /// is taken.
    ///
    /// The fallback only works when the registered redirect URI uses a literal
    /// loopback IP: Doorkeeper (and RFC 8252 §7.3 generally) ignores the port
    /// when comparing two `127.0.0.1`/`::1` URIs, but compares `localhost`
    /// URIs as plain strings. Callers that must keep an exact registered port
    /// should pass the port and check [`Self::port`] afterwards.
    pub fn bind(preferred: u16) -> Result<Self> {
        let server = match tiny_http::Server::http(("127.0.0.1", preferred)) {
            Ok(server) => server,
            Err(e) => {
                log::debug!("port {preferred} unavailable ({e}); asking the OS for a free one");
                tiny_http::Server::http(("127.0.0.1", 0)).map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to start the local callback server on 127.0.0.1: {e}\n\
                         Port {preferred} is in use and no fallback port could be bound."
                    )
                })?
            }
        };
        // Always read the port back off the socket. Trusting `preferred` would
        // report 0 when the caller asked the OS to choose, and the port ends up
        // in the `redirect_uri` the provider must match.
        let port = server
            .server_addr()
            .to_ip()
            .ok_or_else(|| anyhow::anyhow!("Callback server bound a non-IP address"))?
            .port();
        Ok(Self { server, port })
    }

    /// The port actually bound. Use it to build the `redirect_uri`.
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// Wait for the redirect, validate `state`, and return the `code`.
    ///
    /// Consumes the listener: the socket is released before the caller makes
    /// any outbound HTTP call.
    pub fn await_code(self, expected_state: &str, timeout: Duration) -> Result<String> {
        let request = self
            .server
            .recv_timeout(timeout)
            .context("Error waiting for the OAuth callback")?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Timed out waiting for authorization.\n\
                     Complete the authorization in your browser within {} seconds.\n\
                     Run `do-next auth` to try again.",
                    timeout.as_secs()
                )
            })?;

        let url = request.url().to_string();

        // Decide the verdict before answering, so the browser is never told
        // "authorization complete" for a callback we are about to reject.
        let outcome = parse_callback(&url).and_then(|callback| {
            if callback.state == expected_state {
                Ok(callback.code)
            } else {
                bail!(
                    "OAuth state mismatch — possible CSRF attack. \
                     Run `do-next auth` to try again."
                )
            }
        });

        match &outcome {
            Ok(_) => {
                let response =
                    tiny_http::Response::from_string(include_str!("authorization_complete.html"))
                        .with_header(
                            "Content-Type: text/html"
                                .parse::<tiny_http::Header>()
                                .expect("static header is valid"),
                        );
                let _ = request.respond(response);
            }
            Err(e) => {
                let response = tiny_http::Response::from_string(format!(
                    "Authorization failed.\n\n{e}\n\nYou can close this tab."
                ))
                .with_header(
                    "Content-Type: text/plain; charset=utf-8"
                        .parse::<tiny_http::Header>()
                        .expect("static header is valid"),
                )
                .with_status_code(400);
                let _ = request.respond(response);
            }
        }
        drop(self.server); // Release the listener before any outbound call.

        outcome
    }
}

/// The `code` and `state` carried by a successful callback.
#[derive(Debug)]
struct Callback {
    code: String,
    state: String,
}

/// Parse an OAuth callback URL's query string.
///
/// A provider-reported `error` takes precedence over a missing `code`, so a
/// denied consent says so instead of complaining about a malformed callback.
fn parse_callback(url: &str) -> Result<Callback> {
    let query = url.split_once('?').map_or("", |(_, q)| q);

    let mut code = None;
    let mut state = None;
    let mut error = None;
    let mut error_description = None;

    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            match key {
                "code" => code = Some(value.to_string()),
                "state" => state = Some(value.to_string()),
                "error" => error = Some(percent_decode(value)),
                "error_description" => error_description = Some(percent_decode(value)),
                _ => {}
            }
        }
    }

    if let Some(error) = error {
        let detail = error_description.map_or_else(String::new, |d| format!(": {d}"));
        bail!(
            "Authorization was refused ({error}{detail}).\n\
             Run `do-next auth` to try again."
        );
    }

    let code = code.filter(|c| !c.is_empty()).ok_or_else(|| {
        anyhow::anyhow!(
            "Authorization callback missing 'code' parameter.\n\
                 The authorization may have been denied. Run `do-next auth` to try again."
        )
    })?;
    let state =
        state.ok_or_else(|| anyhow::anyhow!("Authorization callback missing 'state' parameter"))?;

    Ok(Callback { code, state })
}

/// Decode `+` and `%XX` escapes in a query-string value, for display only.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    out.push(byte);
                    i += 3;
                } else {
                    // Not a real escape — keep the '%' as written.
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_the_rfc_7636_test_vector() {
        // Appendix B of RFC 7636 pins this verifier/challenge pair.
        assert_eq!(
            challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn a_generated_verifier_is_within_the_rfc_length_range() {
        let pkce = pkce();
        assert!(
            (43..=128).contains(&pkce.verifier.len()),
            "verifier length {} is outside RFC 7636's 43..=128",
            pkce.verifier.len()
        );
        assert_eq!(pkce.challenge, challenge_for(&pkce.verifier));
        // base64url alphabet only — no padding, nothing needing escaping.
        assert!(
            pkce.verifier
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')),
            "verifier {} is not base64url",
            pkce.verifier
        );
    }

    #[test]
    fn each_state_is_distinct_and_hex() {
        let (a, b) = (state(), state());
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn percent_encode_escapes_everything_outside_the_unreserved_set() {
        assert_eq!(percent_encode("backend/api"), "backend%2Fapi");
        assert_eq!(
            percent_encode("http://127.0.0.1:19873/callback"),
            "http%3A%2F%2F127.0.0.1%3A19873%2Fcallback"
        );
        assert_eq!(percent_encode("read_api openid"), "read_api%20openid");
        // The unreserved set survives untouched.
        assert_eq!(percent_encode("aZ09-_.~"), "aZ09-_.~");
    }

    #[test]
    fn a_callback_yields_its_code_and_state() {
        let cb = parse_callback("/callback?code=abc123&state=deadbeef").expect("parses");
        assert_eq!(cb.code, "abc123");
        assert_eq!(cb.state, "deadbeef");
    }

    #[test]
    fn a_callback_without_a_code_or_state_is_an_error() {
        assert!(parse_callback("/callback?state=deadbeef").is_err());
        assert!(parse_callback("/callback?code=abc123").is_err());
        // An empty code is as useless as a missing one.
        assert!(parse_callback("/callback?code=&state=deadbeef").is_err());
        assert!(parse_callback("/callback").is_err());
    }

    #[test]
    fn a_refused_authorization_reports_the_provider_error() {
        let err = parse_callback("/callback?error=access_denied&state=deadbeef")
            .expect_err("must reject")
            .to_string();
        assert!(err.contains("access_denied"), "{err}");
        // The error wins even when a code is somehow also present.
        let err = parse_callback(
            "/callback?error=invalid_scope&error_description=Bad+scope%3A+nope&code=x&state=y",
        )
        .expect_err("must reject")
        .to_string();
        assert!(err.contains("invalid_scope"), "{err}");
        assert!(err.contains("Bad scope: nope"), "{err}");
    }

    #[test]
    fn the_reported_port_is_the_one_actually_listening() {
        // The port goes into the redirect_uri the provider has to match, so a
        // wrong number here breaks the flow. Port 0 asks the OS to choose,
        // which must be resolved to the real port rather than echoed back.
        let server = LoopbackServer::bind(0).expect("binds an ephemeral port");
        assert_ne!(server.port(), 0, "port 0 must resolve to the bound port");
        std::net::TcpStream::connect(("127.0.0.1", server.port()))
            .expect("the reported port is accepting connections");
    }

    /// Drive a real callback through the listener and report what the browser
    /// saw plus what the flow returned.
    fn round_trip(query: &str, expected_state: &str) -> (u16, String, Result<String>) {
        let server = LoopbackServer::bind(0).expect("binds");
        let port = server.port();
        let url = format!("http://127.0.0.1:{port}/callback?{query}");

        // The listener is single-shot and blocking, so the request goes out on
        // another thread.
        let fetch = std::thread::spawn(move || {
            let mut stream = std::net::TcpStream::connect(("127.0.0.1", port))
                .expect("connects to the callback server");
            use std::io::{Read, Write};
            write!(
                stream,
                "GET /callback?{query} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
                query = url.split_once('?').expect("has a query").1
            )
            .expect("sends the request");
            let mut response = String::new();
            let _ = stream.read_to_string(&mut response);
            response
        });

        let outcome = server.await_code(expected_state, Duration::from_secs(10));
        let response = fetch.join().expect("the request thread finishes");
        (port, response, outcome)
    }

    #[test]
    fn a_real_callback_returns_the_code_and_shows_the_success_page() {
        let (_, response, outcome) = round_trip("code=the-code&state=st4te", "st4te");
        assert_eq!(outcome.expect("succeeds"), "the-code");
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(
            response.contains("authorization complete"),
            "the browser should see the success page"
        );
    }

    #[test]
    fn a_mismatched_state_is_rejected_and_the_browser_is_not_told_otherwise() {
        // The code must never be exchanged, and the page must not claim success.
        let (_, response, outcome) = round_trip("code=the-code&state=attacker", "st4te");
        let err = outcome.expect_err("must reject").to_string();
        assert!(err.contains("state mismatch"), "{err}");
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
        assert!(!response.contains("authorization complete"), "{response}");
    }

    #[test]
    fn a_denied_authorization_reaches_the_caller_as_an_error() {
        let (_, response, outcome) = round_trip("error=access_denied&state=st4te", "st4te");
        let err = outcome.expect_err("must reject").to_string();
        assert!(err.contains("access_denied"), "{err}");
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
    }

    #[test]
    fn a_busy_preferred_port_falls_back_to_another() {
        // Hold the port for the duration, so the conflict is guaranteed rather
        // than racing whatever else is on the machine.
        let held = LoopbackServer::bind(0).expect("binds an ephemeral port");

        let fallback = LoopbackServer::bind(held.port()).expect("falls back");
        assert_ne!(fallback.port(), held.port());
        assert_ne!(fallback.port(), 0);
        std::net::TcpStream::connect(("127.0.0.1", fallback.port()))
            .expect("the fallback port is accepting connections");
    }
}
