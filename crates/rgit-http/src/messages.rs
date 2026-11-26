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

/// libcurl: a redirect to a protocol that is not allowed.
pub fn protocol_disabled(scheme: &str) -> String {
    format!("Protocol \"{scheme}\" not supported or disabled in libcurl")
}

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

/// `could not read Password for '<url>': terminal prompts disabled`.
pub fn no_password(url: &str) -> String {
    format!("could not read Password for '{url}': terminal prompts disabled")
}

/// `warning: unknown value given to http.version: '<v>'`.
pub fn unknown_http_version(v: &str) -> String {
    format!("unknown value given to http.version: '{v}'")
}

/// `warning: Unknown value for http.proactiveauth`.
pub const UNKNOWN_PROACTIVE_AUTH: &str = "Unknown value for http.proactiveauth";

/// `warning: Unknown delegation method '<v>': using default`.
pub fn unknown_delegation(v: &str) -> String {
    format!("Unknown delegation method '{v}': using default")
}

/// `warning: unsupported ssl version <v>: using default`.
pub fn unsupported_ssl_version(v: &str) -> String {
    format!("unsupported ssl version {v}: using default")
}

/// `warning: unsupported proxy authentication method <v>: using anyauth`.
pub fn unsupported_proxy_auth_method(v: &str) -> String {
    format!("unsupported proxy authentication method {v}: using anyauth")
}

/// `fatal: Unsupported SSL backend '<v>'. Supported SSL backends:` and
/// the list, one per line after a tab (rgit has one: rustls).
pub fn unsupported_ssl_backend(v: &str) -> String {
    format!("Unsupported SSL backend '{v}'. Supported SSL backends:\n\trustls")
}

/// `fatal: Invalid proxy URL '<url>': <reason>`.
pub fn invalid_proxy_url(url: &str, reason: &str) -> String {
    format!("Invalid proxy URL '{url}': {reason}")
}

/// Reason: a scheme git does not know.
pub fn unsupported_proxy_scheme(scheme: &str) -> String {
    format!("unsupported proxy scheme '{scheme}'")
}

/// Reason: a path on a non-SOCKS proxy.
pub const PROXY_PATH_NOT_SOCKS: &str = "only SOCKS proxies support paths";

/// Reason: a Unix socket path with a host other than localhost.
pub const PROXY_PATH_NOT_LOCALHOST: &str = "host must be localhost if a path is present";

/// libcurl: the proxy could not be reached.
pub fn failed_to_connect_over_proxy(host: &str, port: u16, proxy: &str, ms: u128) -> String {
    format!(
        "Failed to connect to {host}:{port} over proxy {proxy} after {ms} ms: Could not connect to server"
    )
}

/// libcurl: a malformed `http.curloptResolve` entry.
pub fn bad_resolve_entry(entry: &str) -> String {
    format!("Could not parse CURLOPT_RESOLVE entry '{entry}'")
}

/// `warning: rate limited, waiting <n> seconds before retry`.
pub fn rate_limited_waiting(secs: u64) -> String {
    format!("rate limited, waiting {secs} seconds before retry")
}

/// `error: response requested a delay greater than http.maxRetryTime (...)`.
pub fn retry_delay_exceeds(delay: u64, max: i64) -> String {
    format!("response requested a delay greater than http.maxRetryTime ({delay} > {max} seconds)")
}

/// `error: configured http.retryAfter exceeds http.maxRetryTime (...)`.
pub fn configured_retry_exceeds(delay: i64, max: i64) -> String {
    format!("configured http.retryAfter exceeds http.maxRetryTime ({delay} > {max} seconds)")
}

/// `fatal: rate limited by '<url>/', please try again in <n> seconds`.
pub fn rate_limited_by(url: &str, secs: u64) -> String {
    format!("rate limited by '{url}', please try again in {secs} seconds")
}

/// `transfer.credentialsInUrl`: `URL '<url>' uses plaintext credentials`.
pub fn plaintext_credentials(url: &str) -> String {
    format!("URL '{url}' uses plaintext credentials")
}

/// An unknown `transfer.credentialsInUrl` value.
pub fn unrecognized_credentials_in_url(v: &str) -> String {
    format!("unrecognized value transfer.credentialsInUrl: '{v}'")
}

/// libcurl: `http.pinnedPubkey` does not match the server's key.
pub const PINNED_KEY_MISMATCH: &str = "SSL: public key does not match pinned public key";

/// libcurl: the client certificate could not be loaded.
pub fn client_cert_problem(path: &str) -> String {
    format!("could not load PEM client certificate from {path}")
}

/// libcurl: the client key could not be loaded.
pub fn client_key_problem(path: &str) -> String {
    format!("unable to set private key file: '{path}' type PEM")
}

/// A certificate or key type rustls cannot load (only PEM is supported).
pub fn unsupported_cert_type(kind: &str) -> String {
    format!("not supported file type '{kind}' for certificate")
}

/// libcurl: none of the ciphers of `http.sslCipherList` is available.
pub fn cipher_list_failed(list: &str) -> String {
    format!("failed setting cipher list: {list}")
}

/// `http.sslVersion=sslv2|sslv3`: rustls only speaks TLS 1.2 and 1.3.
pub fn ssl_version_unsupported(v: &str) -> String {
    format!("TLS connect error: {v} is not supported by rgit's TLS backend (rustls)")
}

/// An `https://` proxy (TLS to the proxy itself).
pub const HTTPS_PROXY_UNSUPPORTED: &str =
    "HTTPS proxies are not supported by rgit; use an http:// or socks proxy";

/// A SOCKS proxy on a Unix domain socket.
pub const UNIX_SOCKET_PROXY_UNSUPPORTED: &str =
    "SOCKS proxies on Unix domain sockets are not supported by rgit";

/// libcurl: the SOCKS4 proxy refused the connection.
pub fn socks4_failed(target: &str) -> String {
    format!("SOCKS4 connection to {target} not successful: request rejected or failed.")
}

/// libcurl: the SOCKS5 proxy refused the connection.
pub fn socks5_failed(target: &str, code: u8) -> String {
    format!("Can't complete SOCKS5 connection to {target}. ({code})")
}

/// libcurl: the SOCKS5 proxy wants an authentication method we lack.
pub const SOCKS5_NO_AUTH: &str = "No authentication method was acceptable.";

/// libcurl: the SOCKS5 proxy rejected user name and password.
pub const SOCKS5_AUTH_FAILED: &str = "User was rejected by the SOCKS5 server.";

/// SOCKS4 needs an IPv4 address for the target.
pub fn socks4_no_ipv4(host: &str) -> String {
    format!("SOCKS4 connection to {host} not supported")
}

/// libcurl (CURLE_HTTP2): the HTTP/2 framing layer failed.
pub const H2_FRAMING: &str = "Error in the HTTP2 framing layer";

/// The server refused an HTTP/2 stream (`GOAWAY` or `REFUSED_STREAM`)
/// and the request could not be sent again.
pub const H2_REFUSED: &str = "HTTP/2 stream was refused by the server";

/// libcurl (CURLE_HTTP2_STREAM): the server reset the stream.
pub fn h2_stream_error(stream: u32, name: &str, code: u32) -> String {
    format!("HTTP/2 stream {stream} was not closed cleanly: {name} (err {code})")
}
