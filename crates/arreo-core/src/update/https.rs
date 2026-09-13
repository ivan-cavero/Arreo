//! The `https://` transport (T-0037): one GET, over the TLS stack this workspace
//! already ships.
//!
//! One sentence: fetch the bytes at an `https://` URL, following redirects, with
//! no HTTP client dependency — because the workspace has none and adding one
//! would grow the supply chain of the program that decides which binary this
//! machine runs.
//!
//! ## Why hand-written, and why that is not recklessness
//!
//! The alternative considered and rejected was shelling out to `curl`: it is
//! fewer lines, and it resolves the program that fetches an *executable* through
//! `$PATH`. That is the same argument that made the verifier's key a compiled-in
//! constant. The other alternative — a general-purpose HTTP crate — would add a
//! dependency tree (hyper, h2, http, http-body, …) to a client whose entire
//! network need is `GET`, and every crate in that tree would be in the update
//! path of every install.
//!
//! What is implemented here is the subset the channel needs, and no more:
//!
//! * `GET`, HTTP/1.1, `Connection: close` — one request per connection, so there
//!   is no pool, no keep-alive state machine, and no way to reuse a socket for a
//!   different origin;
//! * the status line and headers, `Content-Length`, chunked transfer encoding,
//!   and a close-delimited body;
//! * redirects (bounded), which is not optional: `github.com/.../latest/download/`
//!   answers `302` to the asset host;
//! * a connect timeout and a read/write timeout, so a dead host is an error in
//!   seconds rather than a hung CLI;
//! * **`https` only**. A redirect to `http://` is refused rather than followed —
//!   a downgrade here would silently undo the point of the transport.
//!
//! ## What it does not do
//!
//! No cookies, no proxies, no HTTP/2, no TLS 1.2 (the `rustls` in this workspace
//! is built for TLS 1.3, which every host the channel can point at speaks), and
//! no certificate pinning — the platform trust store is the roots, and the
//! *artifact* is what is pinned (a minisign signature), not the server. That is
//! the design: a compromised or wrong server can serve an old release or nothing,
//! but it cannot make this client install bytes the release key did not sign.
//!
//! The parsing half is written as a function over any [`BufRead`], so the whole
//! of the protocol handling is tested against canned responses with no network at
//! all (see the tests at the bottom); only the socket and the handshake are not
//! exercised by tests, and that gap is stated in the T-0037 report rather than
//! papered over.

use super::channel::Error;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

/// How long a connection may take to establish. Short: a channel that cannot be
/// reached should say so, not hang.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a single read or write may take. Generous, because an artifact is
/// tens of megabytes and a slow link is not an error.
const IO_TIMEOUT: Duration = Duration::from_secs(60);

/// How many redirects are followed. Three is what GitHub needs (release → asset
/// host → object); five leaves room without allowing a loop.
const MAX_REDIRECTS: usize = 5;

/// The largest response head this fetcher will read before calling it broken.
const MAX_HEAD: usize = 64 * 1024;

/// The longest single response line, for the same reason.
const MAX_LINE: usize = 8 * 1024;

/// Fetch `url` into `dest`, following redirects.
pub fn get(url: &str, dest: &mut dyn Write) -> Result<(), Error> {
    let mut url = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        match once(&url, dest)? {
            Outcome::Body => return Ok(()),
            Outcome::Redirect(location) => url = resolve(&url, &location)?,
        }
    }
    Err(fail(
        &url,
        format!("more than {MAX_REDIRECTS} redirects, which is a loop"),
    ))
}

/// What one response was.
#[derive(Debug)]
enum Outcome {
    /// The body was read into the destination.
    Body,
    /// The server sent the body elsewhere.
    Redirect(String),
}

/// One request over one connection.
fn once(url: &str, dest: &mut dyn Write) -> Result<Outcome, Error> {
    let (host, port, path) = split(url)?;
    let address = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| fail(url, format!("cannot resolve {host}: {e}")))?
        .next()
        .ok_or_else(|| fail(url, format!("{host} resolves to no address")))?;
    let stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)
        .map_err(|e| fail(url, format!("cannot connect to {host}:{port}: {e}")))?;
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));

    let server_name = rustls::pki_types::ServerName::try_from(host.clone())
        .map_err(|e| fail(url, format!("{host} is not a server name: {e}")))?;
    let connection = rustls::ClientConnection::new(config()?, server_name)
        .map_err(|e| fail(url, format!("cannot start a TLS session with {host}: {e}")))?;
    let mut tls = rustls::StreamOwned::new(connection, stream);

    write!(
        tls,
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: arreo/{}\r\nAccept: */*\r\n\
         Connection: close\r\n\r\n",
        env!("CARGO_PKG_VERSION")
    )
    .map_err(|e| fail(url, format!("cannot send the request: {e}")))?;
    tls.flush()
        .map_err(|e| fail(url, format!("cannot send the request: {e}")))?;

    let mut reader = BufReader::new(tls);
    read_response(url, &mut reader, dest)
}

/// Read one response, writing its body to `dest`.
///
/// Generic over [`BufRead`] so every branch below — status parsing, headers,
/// `Content-Length`, chunked, redirects, refusals — is tested against canned
/// bytes with no socket in sight.
fn read_response<R: BufRead>(
    url: &str,
    reader: &mut R,
    dest: &mut dyn Write,
) -> Result<Outcome, Error> {
    let status = read_line(url, reader)?;
    let mut parts = status.trim_end().splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/") {
        return Err(fail(url, format!("not an HTTP response: {status:?}")));
    }
    let code_text = parts
        .next()
        .ok_or_else(|| fail(url, format!("no status in {status:?}")))?;
    let code: u16 = code_text
        .parse()
        .map_err(|_| fail(url, format!("{code_text:?} is not an HTTP status")))?;
    let reason = parts.next().unwrap_or("").trim().to_string();

    let mut headers: Vec<(String, String)> = Vec::new();
    let mut head = status.len();
    loop {
        let line = read_line(url, reader)?;
        head += line.len();
        if head > MAX_HEAD {
            return Err(fail(
                url,
                "the response headers are longer than any real server's",
            ));
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    let header = |name: &str| -> Option<String> {
        headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };

    if (300..400).contains(&code) {
        if let Some(location) = header("location") {
            return Ok(Outcome::Redirect(location));
        }
    }
    if code == 404 {
        return Err(Error::NotFound {
            url: url.to_string(),
        });
    }
    if !(200..300).contains(&code) {
        return Err(fail(url, format!("the server answered {code} {reason}")));
    }

    let chunked = header("transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"));
    if chunked {
        copy_chunked(url, reader, dest)?;
    } else if let Some(length) = header("content-length") {
        let length: u64 = length
            .parse()
            .map_err(|_| fail(url, format!("{length:?} is not a content length")))?;
        copy_exact(url, reader, dest, length)?;
    } else {
        // `Connection: close` means the end of the body is the end of the stream,
        // which is exactly what an HTTP/1.0-style response relies on.
        std::io::copy(reader, dest).map_err(|e| fail(url, format!("reading the body: {e}")))?;
    }
    Ok(Outcome::Body)
}

/// Copy exactly `n` bytes, refusing a body that ends early — a truncated
/// download must not be mistaken for a complete one.
fn copy_exact<R: BufRead>(
    url: &str,
    reader: &mut R,
    dest: &mut dyn Write,
    n: u64,
) -> Result<(), Error> {
    let copied = std::io::copy(&mut std::io::Read::take(reader, n), dest)
        .map_err(|e| fail(url, format!("reading the body: {e}")))?;
    if copied != n {
        return Err(fail(
            url,
            format!("the body ended after {copied} of {n} bytes"),
        ));
    }
    Ok(())
}

/// Copy a chunked body: size line, bytes, blank line, until a zero-size chunk and
/// the trailer.
fn copy_chunked<R: BufRead>(url: &str, reader: &mut R, dest: &mut dyn Write) -> Result<(), Error> {
    loop {
        let line = read_line(url, reader)?;
        let size_text = line.trim().split(';').next().unwrap_or("").trim();
        let size = u64::from_str_radix(size_text, 16)
            .map_err(|_| fail(url, format!("{size_text:?} is not a chunk size")))?;
        if size == 0 {
            break;
        }
        copy_exact(url, reader, dest, size)?;
        if !read_line(url, reader)?.trim().is_empty() {
            return Err(fail(url, "a chunk was not followed by a line break"));
        }
    }
    // The trailer: header lines, ended by a blank one.
    loop {
        if read_line(url, reader)?.trim().is_empty() {
            break;
        }
    }
    Ok(())
}

/// One line, bounded, with a closed connection reported rather than treated as an
/// empty line.
fn read_line<R: BufRead>(url: &str, reader: &mut R) -> Result<String, Error> {
    let mut buf = Vec::new();
    let read = reader
        .read_until(b'\n', &mut buf)
        .map_err(|e| fail(url, format!("reading the response: {e}")))?;
    if read == 0 {
        return Err(fail(
            url,
            "the connection closed before the response was complete",
        ));
    }
    if read > MAX_LINE {
        return Err(fail(
            url,
            "a response line is longer than any real server's",
        ));
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// `https://host[:port]/path` → its three parts.
fn split(url: &str) -> Result<(String, u16, String), Error> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| fail(url, "not an https URL"))?;
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (
            host,
            port.parse::<u16>()
                .map_err(|_| fail(url, format!("{port:?} is not a port")))?,
        ),
        None => (authority, 443),
    };
    if host.is_empty() || host.contains('@') {
        return Err(fail(url, format!("{authority:?} is not a host")));
    }
    Ok((host.to_string(), port, path.to_string()))
}

/// Where a redirect points, resolved against the URL that produced it.
///
/// Absolute locations are taken as they are (and refused later if they are not
/// `https`, which is what keeps a downgrade from being followed); a
/// root-relative one is resolved against the same authority.
fn resolve(base: &str, location: &str) -> Result<String, Error> {
    if location.starts_with("https://") || location.starts_with("http://") {
        return Ok(location.to_string());
    }
    if let Some(path) = location.strip_prefix('/') {
        let (host, port, _) = split(base)?;
        let authority = if port == 443 {
            host
        } else {
            format!("{host}:{port}")
        };
        return Ok(format!("https://{authority}/{path}"));
    }
    Err(fail(
        base,
        format!("the redirect to {location:?} is not a URL this fetcher can follow"),
    ))
}

/// The TLS client configuration, built once per process.
///
/// Cached because the platform trust store is read from disk: a check and an
/// install make several connections, and re-reading the store for each one would
/// be work with no purpose. A build failure is cached too — it cannot succeed on
/// the second attempt, and the refusal is what the operator needs to see.
fn config() -> Result<Arc<rustls::ClientConfig>, Error> {
    static CONFIG: std::sync::LazyLock<Result<Arc<rustls::ClientConfig>, String>> =
        std::sync::LazyLock::new(build_config);
    match &*CONFIG {
        Ok(config) => Ok(config.clone()),
        Err(detail) => Err(fail("https://", detail)),
    }
}

fn build_config() -> Result<Arc<rustls::ClientConfig>, String> {
    let loaded = rustls_native_certs::load_native_certs();
    let mut roots = rustls::RootCertStore::empty();
    let (added, _ignored) = roots.add_parsable_certificates(loaded.certs);
    if added == 0 {
        return Err(
            "the platform trust store holds no usable CA certificate, so no https server can be \
             verified"
                .to_string(),
        );
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| format!("cannot build a TLS client: {e}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

fn fail(url: &str, detail: impl std::fmt::Display) -> Error {
    Error::Fetch {
        url: url.to_string(),
        detail: detail.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// One canned response, read the way a real one is.
    fn respond(response: &str) -> (Result<Outcome, Error>, Vec<u8>) {
        let mut body = Vec::new();
        let mut reader = Cursor::new(response.as_bytes().to_vec());
        let outcome = read_response("https://example.invalid/x", &mut reader, &mut body);
        (outcome, body)
    }

    /// The ordinary case: `Content-Length`, and the body is exactly it.
    #[test]
    fn a_content_length_body_is_read() {
        let (outcome, body) = respond(
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Type: application/octet-stream\r\n\r\nhello",
        );
        assert!(matches!(outcome, Ok(Outcome::Body)));
        assert_eq!(body, b"hello");
    }

    /// Chunked, with a chunk extension and a trailer — what a CDN sends when it
    /// does not know the length up front.
    #[test]
    fn a_chunked_body_is_read() {
        let (outcome, body) = respond(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5;name=value\r\nhello\r\n6\r\n world\r\n0\r\nX-Checksum: abc\r\n\r\n",
        );
        assert!(matches!(outcome, Ok(Outcome::Body)));
        assert_eq!(body, b"hello world");
    }

    /// No length and no chunking: `Connection: close` makes the end of the
    /// stream the end of the body.
    #[test]
    fn a_close_delimited_body_is_read() {
        let (outcome, body) = respond("HTTP/1.1 200 OK\r\n\r\nbody until the socket ends");
        assert!(matches!(outcome, Ok(Outcome::Body)));
        assert_eq!(body, b"body until the socket ends");
    }

    /// The redirect GitHub actually answers with, reported rather than followed
    /// inside the parser.
    #[test]
    fn a_redirect_is_reported_with_its_location() {
        let (outcome, body) = respond(
            "HTTP/1.1 302 Found\r\nLocation: https://objects.example.invalid/arreo\r\nContent-Length: 0\r\n\r\n",
        );
        match outcome {
            Ok(Outcome::Redirect(location)) => {
                assert_eq!(location, "https://objects.example.invalid/arreo");
            }
            other => panic!("expected a redirect, got {other:?}"),
        }
        assert!(body.is_empty());
    }

    /// A 404 is the *empty channel* signal, and it is its own variant so nothing
    /// can turn it into a generic failure (or the reverse).
    #[test]
    fn a_missing_file_is_not_found() {
        let (outcome, _) = respond("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
        assert!(
            matches!(outcome, Err(Error::NotFound { .. })),
            "{outcome:?}"
        );
    }

    /// Anything else is a failure that names the status.
    #[test]
    fn a_server_error_is_a_fetch_failure_naming_the_status() {
        let (outcome, _) = respond("HTTP/1.1 503 Service Unavailable\r\n\r\n");
        match outcome {
            Err(error) => assert!(
                error.to_string().contains("503"),
                "the refusal names the status: {error}"
            ),
            Ok(other) => panic!("expected a failure, got {other:?}"),
        }
    }

    /// A body that stops early is refused: a truncated download is not a
    /// download.
    #[test]
    fn a_truncated_body_is_refused() {
        let (outcome, _) = respond("HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nshort");
        match outcome {
            Err(error) => assert!(
                error.to_string().contains("ended after"),
                "the refusal says the body ended early: {error}"
            ),
            Ok(other) => panic!("expected a failure, got {other:?}"),
        }
    }

    /// A response that is not HTTP at all — a captive portal, a proxy error page
    /// written by hand — is refused rather than parsed as something.
    #[test]
    fn a_malformed_status_line_is_refused() {
        let (outcome, _) = respond("hello there\r\n\r\n");
        assert!(matches!(outcome, Err(Error::Fetch { .. })), "{outcome:?}");
    }

    /// A redirect to a relative path resolves against the authority it came from,
    /// and an absolute one is taken as it is.
    #[test]
    fn redirects_resolve_against_the_url_that_produced_them() {
        assert_eq!(
            resolve("https://example.invalid/a/b", "/c/d").unwrap(),
            "https://example.invalid/c/d"
        );
        assert_eq!(
            resolve("https://example.invalid/a/b", "https://other.invalid/x").unwrap(),
            "https://other.invalid/x"
        );
        assert_eq!(
            resolve("https://example.invalid:8443/a", "/b").unwrap(),
            "https://example.invalid:8443/b"
        );
        // A location that is not a URL is refused rather than guessed at.
        assert!(resolve("https://example.invalid/a", "elsewhere").is_err());
    }

    /// A URL is split into host, port and path, and a URL that is not `https` or
    /// has no host is refused — which is also what stops an `http://` redirect
    /// from being followed as a downgrade.
    #[test]
    fn a_url_is_split_and_only_https_is_accepted() {
        assert_eq!(
            split("https://example.invalid/a/b?c=d").unwrap(),
            ("example.invalid".to_string(), 443, "/a/b?c=d".to_string())
        );
        assert_eq!(
            split("https://example.invalid:8443/").unwrap(),
            ("example.invalid".to_string(), 8443, "/".to_string())
        );
        assert_eq!(
            split("https://example.invalid").unwrap().2,
            "/",
            "a bare host asks for the root"
        );
        for refused in [
            "http://example.invalid/",
            "https:///x",
            "https://user@host/x",
        ] {
            assert!(split(refused).is_err(), "{refused} must be refused");
        }
    }
}
