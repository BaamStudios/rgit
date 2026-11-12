// SPDX-License-Identifier: MIT
//! `GIT_TRACE_CURL` / `GIT_CURL_VERBOSE` output.
//!
//! git 2.56.0 prints libcurl's debug callback as trace lines (observed
//! with `GIT_TRACE_CURL=1`):
//!
//! ```text
//! == Info:   Trying 127.0.0.1:38119...
//! => Send header, 0000000244 bytes (0x000000f4)
//! => Send header: GET /smart/repo.git/info/refs?service=git-upload-pack HTTP/1.1
//! => Send header: Host: 127.0.0.1:38119
//! => Send header:
//! <= Recv header, 0000000017 bytes (0x00000011)
//! <= Recv header: HTTP/1.1 200 OK
//! => Send data, 0000000181 bytes (0x000000b5)
//! => Send data: 0014command=ls-refs.001aagent=git/2.56.0-Linux0016object-for
//! ```
//!
//! - the request header block is one event: a count line, then one line
//!   per header line and an empty `=> Send header:` for the blank line;
//! - every received header line is its own event;
//! - data events are split into lines of 60 bytes; bytes outside
//!   `0x20..=0x7e` print as `.`; `GIT_TRACE_CURL_NO_DATA` drops them;
//! - unless `GIT_TRACE_REDACT` is false, the values of `Authorization`
//!   and `Proxy-Authorization` (after the scheme) and of cookies are
//!   replaced by `<redacted>`.
//!
//! `GIT_CURL_VERBOSE` produces the same lines on stderr.

use std::ffi::OsStr;
use std::sync::OnceLock;

use rgit_trace::{TRACE_CURL, TraceKey};

/// `GIT_CURL_VERBOSE`, routed to stderr through the same line format.
static CURL_VERBOSE: TraceKey = TraceKey::new("GIT_CURL_VERBOSE");

/// Settings of the HTTP trace.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Tracer {
    no_data: bool,
    redact: bool,
}

fn env_bool(name: &str, default: bool) -> bool {
    match std::env::var_os(name) {
        Some(v) => rgit_config::value::parse_bool(Some(v.as_encoded_bytes())).unwrap_or(default),
        None => default,
    }
}

fn key() -> Option<&'static TraceKey> {
    if TRACE_CURL.enabled() {
        return Some(&TRACE_CURL);
    }
    static VERBOSE: OnceLock<bool> = OnceLock::new();
    let on = *VERBOSE.get_or_init(|| {
        let on = std::env::var_os("GIT_CURL_VERBOSE").is_some_and(|v| {
            rgit_config::value::parse_bool(Some(v.as_encoded_bytes())).unwrap_or(true)
        });
        CURL_VERBOSE.init_from(on.then_some(OsStr::new("2")));
        on
    });
    (on && CURL_VERBOSE.enabled()).then_some(&CURL_VERBOSE)
}

impl Tracer {
    /// Read `GIT_TRACE_CURL_NO_DATA` and `GIT_TRACE_REDACT`.
    pub(crate) fn from_env() -> Tracer {
        Tracer {
            no_data: env_bool("GIT_TRACE_CURL_NO_DATA", false),
            redact: env_bool("GIT_TRACE_REDACT", true),
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        key().is_some()
    }

    fn line(&self, msg: &[u8]) {
        if let Some(k) = key() {
            k.write_line(rgit_trace::location!(), msg);
        }
    }

    /// `== Info: <msg>`.
    pub(crate) fn info(&self, msg: &str) {
        if self.enabled() {
            self.line(format!("== Info: {msg}").as_bytes());
        }
    }

    /// The whole request header block (including the final CRLF CRLF).
    pub(crate) fn send_header(&self, block: &[u8]) {
        if !self.enabled() {
            return;
        }
        self.count_line("=> Send header", block.len());
        let text = block.strip_suffix(b"\r\n").unwrap_or(block);
        for l in text.split(|&b| b == b'\n') {
            let l = l.strip_suffix(b"\r").unwrap_or(l);
            self.header_line("=> Send header:", l);
        }
    }

    /// One received header line including its CRLF.
    pub(crate) fn recv_header(&self, raw: &[u8]) {
        if !self.enabled() {
            return;
        }
        self.count_line("<= Recv header", raw.len());
        let l = raw.strip_suffix(b"\n").unwrap_or(raw);
        let l = l.strip_suffix(b"\r").unwrap_or(l);
        self.header_line("<= Recv header:", l);
    }

    pub(crate) fn send_data(&self, data: &[u8]) {
        self.data("=> Send data", data);
    }

    pub(crate) fn recv_data(&self, data: &[u8]) {
        self.data("<= Recv data", data);
    }

    fn count_line(&self, what: &str, n: usize) {
        self.line(format!("{what}, {n:010} bytes (0x{n:08x})").as_bytes());
    }

    fn header_line(&self, prefix: &str, l: &[u8]) {
        let mut out = prefix.as_bytes().to_vec();
        if !l.is_empty() {
            out.push(b' ');
            if self.redact {
                out.extend_from_slice(&redact_header(l));
            } else {
                out.extend_from_slice(l);
            }
        }
        self.line(&out);
    }

    fn data(&self, what: &str, data: &[u8]) {
        if self.no_data || data.is_empty() || !self.enabled() {
            return;
        }
        self.count_line(what, data.len());
        for piece in data.chunks(60) {
            let mut out = format!("{what}: ").into_bytes();
            out.extend(
                piece
                    .iter()
                    .map(|&b| if (0x20..0x7f).contains(&b) { b } else { b'.' }),
            );
            self.line(&out);
        }
    }
}

/// Redact credentials in one header line (see the module documentation).
pub(crate) fn redact_header(line: &[u8]) -> Vec<u8> {
    let Some(colon) = line.iter().position(|&b| b == b':') else {
        return line.to_vec();
    };
    let name = &line[..colon];
    let value = &line[colon + 1..];
    let lead = value.iter().take_while(|&&b| b == b' ').count();
    let value_t = &value[lead..];
    let mut out = line[..colon + 1 + lead].to_vec();
    if name.eq_ignore_ascii_case(b"authorization")
        || name.eq_ignore_ascii_case(b"proxy-authorization")
    {
        if let Some(sp) = value_t.iter().position(|&b| b == b' ') {
            out.extend_from_slice(&value_t[..sp + 1]);
        }
        out.extend_from_slice(b"<redacted>");
        return out;
    }
    if name.eq_ignore_ascii_case(b"cookie") || name.eq_ignore_ascii_case(b"set-cookie") {
        let text = String::from_utf8_lossy(value_t);
        let parts: Vec<String> = text
            .split(';')
            .map(|p| match p.split_once('=') {
                Some((k, _)) => format!("{k}=<redacted>"),
                None => p.to_string(),
            })
            .collect();
        out.extend_from_slice(parts.join(";").as_bytes());
        return out;
    }
    line.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction() {
        assert_eq!(
            redact_header(b"Authorization: Basic dXNlcjpwYXNz"),
            b"Authorization: Basic <redacted>".to_vec()
        );
        assert_eq!(
            redact_header(b"Proxy-Authorization: token"),
            b"Proxy-Authorization: <redacted>".to_vec()
        );
        assert_eq!(
            redact_header(b"Cookie: Foo=1; Bar=2"),
            b"Cookie: Foo=<redacted>; Bar=<redacted>".to_vec()
        );
        assert_eq!(redact_header(b"Host: x"), b"Host: x".to_vec());
    }
}
