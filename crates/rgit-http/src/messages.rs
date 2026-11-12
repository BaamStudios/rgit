// SPDX-License-Identifier: MIT
//! User-visible messages of the smart HTTP client.
//!
//! Strings that scripts and tests match are reproduced verbatim from the
//! observed output of git 2.56.0 (CLEANROOM.md, "User-visible messages").
//! The transport-level reasons follow the wording libcurl uses, because
//! git passes them through ("Failed to connect to ...", "SSL certificate
//! problem: ...").

/// `fatal: repository '<url>/' not found` (404 on discovery).
pub fn repository_not_found(url: &str) -> String {
    format!("repository '{url}' not found")
}

/// `fatal: Authentication failed for '<url>/'`.
pub fn authentication_failed(url: &str) -> String {
    format!("Authentication failed for '{url}'")
}

/// `fatal: unable to access '<url>/': <reason>`.
pub fn unable_to_access(url: &str, reason: &str) -> String {
    format!("unable to access '{url}': {reason}")
}

/// The reason for an HTTP error status, as libcurl words it.
pub fn returned_error(status: u16) -> String {
    format!("The requested URL returned error: {status}")
}

/// `fatal: <url>/info/refs not valid: is this a git repository?`
pub fn not_valid(refs_url: &str) -> String {
    format!("{refs_url} not valid: is this a git repository?")
}

/// rgit's own message: dumb HTTP is not implemented (issue #186).
pub fn dumb_not_supported(url: &str) -> String {
    format!("{url} is served by a dumb HTTP server, which rgit does not support")
}

/// The first packet of a smart reply is not `# service=<name>`.
pub fn invalid_service_line(got: &str) -> String {
    format!("invalid server response; expected service, got '{got}'")
}

/// A smart reply that ends before the service line.
pub const INVALID_SERVER_RESPONSE_FLUSH: &str =
    "invalid server response; expected service, got flush packet";

/// Any other malformed advertisement.
pub fn invalid_server_response(got: &str) -> String {
    format!("invalid server response; got '{got}'")
}

/// The redirected discovery URL does not end in the requested suffix.
pub fn redirect_base(asked: &str, redirect: &str) -> String {
    format!(
        "unable to update url base from redirection:\n  asked for: {asked}\n   redirect: {redirect}"
    )
}

/// `warning: redirecting to <url>/`.
pub fn redirecting_to(url: &str) -> String {
    format!("redirecting to {url}")
}

/// `error: RPC failed; HTTP <status> curl <code> <reason>`.
pub fn rpc_failed(status: Option<u16>, curl_code: u32, reason: &str) -> String {
    match status {
        Some(s) => format!("RPC failed; HTTP {s} curl {curl_code} {reason}"),
        None => format!("RPC failed; curl {curl_code} {reason}"),
    }
}

/// `POST git-upload-pack (<n> bytes)` (verbosity above 1).
pub fn post_bytes(service: &str, len: usize) -> String {
    format!("POST {service} ({len} bytes)")
}

/// `POST git-upload-pack (gzip <n> to <m> bytes)`.
pub fn post_gzip(service: &str, from: usize, to: usize) -> String {
    format!("POST {service} (gzip {from} to {to} bytes)")
}

/// `POST git-receive-pack (chunked)`.
pub fn post_chunked(service: &str) -> String {
    format!("POST {service} (chunked)")
}

/// `remote: <line>` for a plain-text error body.
pub fn remote_line(line: &str) -> String {
    format!("remote: {line}")
}

/// libcurl: connection refused or unreachable.
pub fn failed_to_connect(host: &str, port: u16, ms: u128) -> String {
    format!("Failed to connect to {host}:{port} after {ms} ms: Could not connect to server")
}

/// libcurl: name resolution failed.
pub fn could_not_resolve_host(host: &str) -> String {
    format!("Could not resolve host: {host}")
}

/// libcurl: proxy name resolution failed.
pub fn could_not_resolve_proxy(host: &str) -> String {
    format!("Could not resolve proxy: {host}")
}

/// libcurl: `CURLOPT_LOW_SPEED_*` triggered.
pub fn too_slow(limit: u64, secs: u64) -> String {
    format!("Operation too slow. Less than {limit} bytes/sec transferred the last {secs} seconds")
}

/// libcurl: the connection closed before a response.
pub const EMPTY_REPLY: &str = "Empty reply from server";

/// libcurl: more than the allowed number of redirects.
pub fn too_many_redirects(n: usize) -> String {
    format!("Maximum ({n}) redirects followed")
}

/// libcurl: a peer certificate that does not chain to a trusted root.
pub const SSL_UNKNOWN_ISSUER: &str =
    "SSL certificate problem: unable to get local issuer certificate";

/// libcurl: a self-signed peer certificate.
pub const SSL_SELF_SIGNED: &str = "SSL certificate problem: self-signed certificate";

/// libcurl: an expired peer certificate.
pub const SSL_EXPIRED: &str = "SSL certificate problem: certificate has expired";

/// libcurl: the certificate does not name the host.
pub fn ssl_name_mismatch(host: &str) -> String {
    format!("SSL: no alternative certificate subject name matches target host name '{host}'")
}

/// Any other certificate problem.
pub fn ssl_problem(detail: &str) -> String {
    format!("SSL certificate problem: {detail}")
}

/// TLS handshake failure that is not about the certificate.
pub fn ssl_connect_error(detail: &str) -> String {
    format!("TLS connect error: {detail}")
}

/// `CONNECT` through a proxy failed.
pub fn proxy_connect_failed(status: u16) -> String {
    format!("CONNECT tunnel failed, response {status}")
}

/// An unsupported proxy scheme (SOCKS, HTTPS proxies).
pub fn unsupported_proxy(proxy: &str) -> String {
    format!("Unsupported proxy syntax in '{proxy}'")
}

/// A response that is not valid HTTP/1.x.
pub fn bad_response(detail: &str) -> String {
    format!("Weird server reply: {detail}")
}

/// `http.sslCAInfo` or `http.sslCAPath` could not be read.
pub fn ca_file_problem(path: &str) -> String {
    format!("error setting certificate file: {path}")
}

/// Invalid URL.
pub fn invalid_url(url: &str) -> String {
    format!("invalid URL '{url}'")
}

/// Unsupported URL scheme.
pub fn unsupported_scheme(scheme: &str) -> String {
    format!("Unsupported protocol '{scheme}'")
}

/// `could not read Username for '<origin>': terminal prompts disabled`,
/// used when no credential provider is available.
pub fn no_username(origin: &str) -> String {
    format!("could not read Username for '{origin}': terminal prompts disabled")
}
