// SPDX-License-Identifier: MIT
//! The smart HTTP protocol client (`gitprotocol-http(5)`, "Smart Clients";
//! `gitprotocol-v2(5)`, "HTTP Transport").
//!
//! A [`SmartHttp`] session holds the repository URL, the HTTP settings,
//! the credential state and the connection pool:
//!
//! 1. [`SmartHttp::discover`] sends `GET $GIT_URL/info/refs?service=<svc>`
//!    (with `Pragma: no-cache` and, if set, `Git-Protocol`), follows
//!    redirects unless `http.followRedirects=false`, moves the repository
//!    URL to the redirect target (`warning: redirecting to ...`), checks
//!    the `application/x-<svc>-advertisement` content type and strips the
//!    `# service=<svc>` packet and its flush.
//! 2. [`SmartHttp::rpc`] sends one stateless `POST $GIT_URL/<svc>`. A body
//!    that fits into `http.postBuffer` is sent with `Content-Length`
//!    (upload-pack bodies over 1 KiB gzip-compressed); a larger one first
//!    sends a `0000` probe (so authentication happens on a body that can
//!    be replayed) and then streams with `Transfer-Encoding: chunked`.
//!    Redirects are only followed with `http.followRedirects=true`.
//!
//! A `401` asks the [`CredentialProvider`] (or uses the user name and
//! password from the URL), retries once with Basic auth, and approves or
//! rejects the credential. Header order and diagnostics match git 2.56.0
//! (see [`crate::client`] and [`crate::messages`]).

use std::io::{self, Read};

use crate::auth::{Credential, CredentialProvider};
use crate::client::{Client, ReqBody, Request, Response};
use crate::error::{Error, Result, Transport};
use crate::messages;
use crate::options::{FollowRedirects, HttpOptions};
use crate::url::Url;

/// libcurl's default redirect limit as set by git.
const MAX_REDIRECTS: usize = 20;

/// Bodies larger than this are gzip-compressed (upload-pack only).
const GZIP_THRESHOLD: usize = 1024;

/// The service a session talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    /// `git-upload-pack` (fetch, clone, ls-remote).
    UploadPack,
    /// `git-receive-pack` (push).
    ReceivePack,
}

impl Service {
    /// `git-upload-pack` or `git-receive-pack`.
    pub fn name(self) -> &'static str {
        match self {
            Service::UploadPack => "git-upload-pack",
            Service::ReceivePack => "git-receive-pack",
        }
    }
}

/// The result of discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advertisement {
    /// The pkt-line stream after the `# service=` packet and its flush
    /// (protocol v0/v1), or the whole body when the server sent no service
    /// packet (protocol v2 starts with `version 2`).
    pub data: Vec<u8>,
    /// Whether the `# service=` packet was present.
    pub had_service_line: bool,
}

/// A request body for [`SmartHttp::rpc`].
pub enum RpcBody<'b> {
    /// The whole request in memory.
    Bytes(&'b [u8]),
    /// A streamed request (a pack for push).
    Stream(&'b mut dyn Read),
}

/// The response of a POST; read it to the end.
pub struct RpcResponse {
    inner: Box<dyn Read + Send>,
    content_type: Option<String>,
}

impl RpcResponse {
    /// The response `Content-Type`.
    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }
}

impl Read for RpcResponse {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

/// A body during a request: replayable bytes or a one-shot stream.
enum Payload<'p> {
    None,
    Bytes(&'p [u8]),
    Chunked {
        head: &'p [u8],
        rest: &'p mut dyn Read,
    },
}

impl Payload<'_> {
    fn replayable(&self) -> bool {
        !matches!(self, Payload::Chunked { .. })
    }
}

/// A smart HTTP session for one remote repository.
pub struct SmartHttp<'a> {
    url: Url,
    client: Client,
    provider: Option<Box<dyn CredentialProvider + Send + 'a>>,
    cred: Option<Credential>,
    cred_from_provider: bool,
    approved: bool,
    git_protocol: Option<String>,
    diagnostics: Box<dyn FnMut(&str) + Send + 'a>,
}

fn print_diagnostic(line: &str) {
    eprintln!("{line}");
}

impl<'a> SmartHttp<'a> {
    /// A session for the repository at `url` with the given settings.
    pub fn new(url: &str, opts: HttpOptions) -> Result<SmartHttp<'a>> {
        let url = Url::parse(url)?;
        Ok(SmartHttp {
            url,
            client: Client::new(opts),
            provider: None,
            cred: None,
            cred_from_provider: false,
            approved: false,
            git_protocol: None,
            diagnostics: Box::new(print_diagnostic),
        })
    }

    /// Use `provider` to obtain credentials after a `401`.
    pub fn with_credentials(mut self, provider: impl CredentialProvider + Send + 'a) -> Self {
        self.provider = Some(Box::new(provider));
        self
    }

    /// The `Git-Protocol` header value (for example `version=2`), sent on
    /// discovery and on every POST.
    pub fn set_git_protocol(&mut self, value: Option<String>) {
        self.git_protocol = value;
    }

    /// Where warnings (`warning: redirecting to ...`) and verbose lines
    /// (`POST git-upload-pack (123 bytes)`) go; stderr by default.
    pub fn set_diagnostics(&mut self, f: impl FnMut(&str) + Send + 'a) {
        self.diagnostics = Box::new(f);
    }

    /// The current repository URL (after redirects).
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// The settings in use.
    pub fn options(&self) -> &HttpOptions {
        &self.client.opts
    }

    /// The repository URL as git prints it in messages: without
    /// credentials and with a trailing `/`.
    pub fn display_url(&self) -> String {
        display(&self.url)
    }

    fn common_headers(&self) -> Vec<(String, String)> {
        self.client
            .opts
            .extra_headers
            .iter()
            .filter_map(|h| h.split_once(':'))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect()
    }

    /// Discover the refs (and capabilities) of `service`.
    pub fn discover(&mut self, service: Service) -> Result<Advertisement> {
        let svc = service.name();
        let suffix = if self.client.opts.smart {
            format!("info/refs?service={svc}")
        } else {
            "info/refs".to_string()
        };
        let req_url = self.url.join(&suffix);
        let mut headers = self.common_headers();
        if let Some(l) = &self.client.opts.accept_language {
            headers.push(("Accept-Language".into(), l.clone()));
        }
        headers.push(("Pragma".into(), "no-cache".into()));
        if let Some(p) = &self.git_protocol {
            headers.push(("Git-Protocol".into(), p.clone()));
        }
        let follow = self.client.opts.follow_redirects != FollowRedirects::Never;
        let disp = self.display_url();
        let (mut resp, final_url) =
            match self.execute("GET", req_url.clone(), headers, true, Payload::None, follow) {
                Ok(r) => r,
                Err(ExecError::Transport(t)) => {
                    return Err(Error::Access {
                        url: disp,
                        status: None,
                        reason: t.reason,
                        remote: Vec::new(),
                    });
                }
                Err(ExecError::Other(e)) => return Err(e),
            };
        match resp.status {
            200..=299 | 304 => {}
            404 => return Err(Error::RepositoryNotFound { url: disp }),
            s => {
                let remote = remote_lines(&mut resp);
                return Err(Error::Access {
                    url: disp,
                    status: Some(s),
                    reason: messages::returned_error(s),
                    remote,
                });
            }
        }

        // Redirected: move the repository URL (git compares the effective
        // URL, which never contains credentials, with the URL it asked for).
        let final_url = final_url.without_credentials();
        if final_url.to_string() != req_url.to_string() {
            let Some(mut base) = final_url.strip_suffix(&suffix) else {
                return Err(Error::RedirectBase {
                    asked: req_url.without_credentials().to_string(),
                    redirect: final_url.to_string(),
                });
            };
            if base.same_origin(&self.url) {
                base.user = self.url.user.clone();
                base.password = self.url.password.clone();
            }
            let shown = display(&base);
            self.url = base;
            (self.diagnostics)(&format!("warning: {}", messages::redirecting_to(&shown)));
        }

        let content_type = resp.header("content-type").map(str::to_string);
        let body = resp.read_all().map_err(|t| Error::Access {
            url: self.display_url(),
            status: None,
            reason: t.reason,
            remote: Vec::new(),
        })?;
        let expected = format!("application/x-{svc}-advertisement");
        let smart = self.client.opts.smart
            && content_type
                .as_deref()
                .and_then(|c| c.split(';').next())
                .is_some_and(|c| c.trim().eq_ignore_ascii_case(&expected));
        if !smart {
            if looks_dumb(&body) {
                return Err(Error::DumbNotSupported {
                    url: self.display_url(),
                });
            }
            return Err(Error::NotValid {
                refs_url: format!("{}info/refs", self.display_url()),
            });
        }
        parse_advertisement(&body, svc)
    }

    /// One stateless request to `service`; returns the response body.
    pub fn rpc(&mut self, service: Service, body: RpcBody<'_>) -> Result<RpcResponse> {
        let url = self.url.join(service.name());
        let limit = self.client.opts.post_buffer;
        match body {
            RpcBody::Bytes(b) if b.len() > limit => {
                let (head, rest) = b.split_at(limit);
                self.rpc_chunked(service, url, head, &mut io::Cursor::new(rest))
            }
            RpcBody::Bytes(b) => self.rpc_small(service, url, b),
            RpcBody::Stream(r) => {
                let mut head = Vec::new();
                (&mut *r).take(limit as u64 + 1).read_to_end(&mut head)?;
                if head.len() <= limit {
                    self.rpc_small(service, url, &head)
                } else {
                    self.rpc_chunked(service, url, &head, r)
                }
            }
        }
    }

    fn rpc_small(&mut self, service: Service, url: Url, small: &[u8]) -> Result<RpcResponse> {
        let svc = service.name();
        let gzip = service == Service::UploadPack && small.len() > GZIP_THRESHOLD;
        let compressed;
        let payload: &[u8] = if gzip {
            compressed = gzip_bytes(small)?;
            if self.client.opts.verbosity > 1 {
                (self.diagnostics)(&messages::post_gzip(svc, small.len(), compressed.len()));
            }
            &compressed
        } else {
            if self.client.opts.verbosity > 1 {
                (self.diagnostics)(&messages::post_bytes(svc, small.len()));
            }
            small
        };
        let headers = self.post_headers(svc, false, gzip);
        let follow = self.client.opts.follow_redirects == FollowRedirects::Always;
        let res = self.execute("POST", url, headers, true, Payload::Bytes(payload), follow);
        self.finish_rpc(res)
    }

    fn rpc_chunked(
        &mut self,
        service: Service,
        url: Url,
        head: &[u8],
        rest: &mut dyn Read,
    ) -> Result<RpcResponse> {
        let svc = service.name();
        // Probe with a flush packet so a 401 happens on a replayable body.
        let mut probe_headers = self.common_headers();
        probe_headers.push((
            "Content-Type".into(),
            format!("application/x-{svc}-request"),
        ));
        probe_headers.push(("Accept".into(), format!("application/x-{svc}-result")));
        let follow = self.client.opts.follow_redirects == FollowRedirects::Always;
        let res = self.execute(
            "POST",
            url.clone(),
            probe_headers,
            false,
            Payload::Bytes(b"0000"),
            follow,
        );
        let mut probe = self.finish_rpc(res)?;
        io::copy(&mut probe, &mut io::sink())?;

        if self.client.opts.verbosity > 1 {
            (self.diagnostics)(&messages::post_chunked(svc));
        }
        let headers = self.post_headers(svc, true, false);
        let res = self.execute(
            "POST",
            url,
            headers,
            true,
            Payload::Chunked { head, rest },
            false,
        );
        self.finish_rpc(res)
    }

    fn post_headers(&self, svc: &str, chunked: bool, gzip: bool) -> Vec<(String, String)> {
        let mut h = self.common_headers();
        if chunked {
            h.push(("Transfer-Encoding".into(), "chunked".into()));
        }
        h.push((
            "Content-Type".into(),
            format!("application/x-{svc}-request"),
        ));
        h.push(("Accept".into(), format!("application/x-{svc}-result")));
        if let Some(l) = &self.client.opts.accept_language {
            h.push(("Accept-Language".into(), l.clone()));
        }
        if gzip {
            h.push(("Content-Encoding".into(), "gzip".into()));
        }
        if let Some(p) = &self.git_protocol {
            h.push(("Git-Protocol".into(), p.clone()));
        }
        h
    }

    fn finish_rpc(&mut self, res: Result<(Response, Url), ExecError>) -> Result<RpcResponse> {
        let mut resp = match res {
            Ok((r, _)) => r,
            Err(ExecError::Transport(t)) => {
                return Err(Error::Rpc {
                    status: None,
                    code: t.code,
                    reason: t.reason,
                    remote: Vec::new(),
                });
            }
            Err(ExecError::Other(e)) => return Err(e),
        };
        if resp.status != 200 {
            let remote = remote_lines(&mut resp);
            return Err(Error::Rpc {
                status: Some(resp.status),
                code: 22,
                reason: messages::returned_error(resp.status),
                remote,
            });
        }
        let content_type = resp.header("content-type").map(str::to_string);
        Ok(RpcResponse {
            inner: resp.body,
            content_type,
        })
    }

    /// Authorization for `url` if the current credential belongs to it.
    fn authorization_for(&self, url: &Url) -> Option<String> {
        let c = self.cred.as_ref()?;
        let same =
            c.protocol == url.scheme.as_str() && c.host.eq_ignore_ascii_case(&url.authority());
        (same && c.password.is_some()).then(|| c.basic_authorization())
    }

    /// Ask for credentials after a 401.
    fn obtain_credentials(&mut self, url: &Url) -> Result<()> {
        let mut cred = Credential {
            protocol: url.scheme.as_str().to_string(),
            host: url.authority(),
            path: Some(self.url.credential_path()),
            username: self.url.user.clone().filter(|_| self.url.same_origin(url)),
            password: self
                .url
                .password
                .clone()
                .filter(|_| self.url.same_origin(url)),
        };
        if cred.username.is_some() && cred.password.is_some() {
            self.cred_from_provider = false;
        } else if let Some(p) = self.provider.as_deref_mut() {
            p.fill(&mut cred).map_err(Error::Credential)?;
            self.cred_from_provider = true;
        } else {
            let msg = match &cred.username {
                Some(u) => {
                    let mut with_user = url.without_credentials();
                    with_user.user = Some(u.clone());
                    with_user.path = String::new();
                    format!(
                        "could not read Password for '{}': terminal prompts disabled",
                        with_user.to_string().trim_end_matches('/')
                    )
                }
                None => messages::no_username(&url.origin()),
            };
            return Err(Error::Credential(msg));
        }
        self.cred = Some(cred);
        self.approved = false;
        Ok(())
    }

    /// Send a request, handling `401` (once) and redirects.
    fn execute(
        &mut self,
        method: &'static str,
        mut url: Url,
        headers: Vec<(String, String)>,
        accept_encoding: bool,
        mut payload: Payload<'_>,
        follow: bool,
    ) -> std::result::Result<(Response, Url), ExecError> {
        let mut redirects = 0;
        let mut auth_retried = false;
        loop {
            let authorization = self.authorization_for(&url);
            let sent_auth = authorization.is_some();
            let body = match &mut payload {
                Payload::None => ReqBody::None,
                Payload::Bytes(b) => ReqBody::Bytes(b),
                Payload::Chunked { head, rest } => ReqBody::Chunked {
                    head,
                    rest: &mut **rest,
                },
            };
            let mut req = Request {
                method,
                url: &url,
                headers: headers.clone(),
                accept_encoding,
                authorization,
                body,
            };
            let mut resp = self.client.send(&mut req).map_err(ExecError::Transport)?;
            let status = resp.status;
            if status == 401 {
                drain(&mut resp);
                if sent_auth || auth_retried || !payload.replayable() {
                    if let Some(c) = self.cred.take() {
                        if self.cred_from_provider {
                            if let Some(p) = self.provider.as_deref_mut() {
                                p.reject(&c);
                            }
                        }
                    }
                    return Err(ExecError::Other(Error::AuthenticationFailed {
                        url: self.display_url(),
                    }));
                }
                self.obtain_credentials(&url).map_err(ExecError::Other)?;
                auth_retried = true;
                continue;
            }
            if follow && matches!(status, 301 | 302 | 303 | 307 | 308) && payload.replayable() {
                if let Some(loc) = resp.header("location").map(str::to_string) {
                    drain(&mut resp);
                    redirects += 1;
                    if redirects > MAX_REDIRECTS {
                        return Err(ExecError::Transport(Transport::new(
                            47,
                            messages::too_many_redirects(MAX_REDIRECTS),
                        )));
                    }
                    self.client
                        .tracer
                        .info(&format!("Issue another request to this URL: '{loc}'"));
                    url = url.resolve(&loc).map_err(ExecError::Other)?;
                    continue;
                }
            }
            if (200..300).contains(&status) && sent_auth && !self.approved {
                self.approved = true;
                if self.cred_from_provider {
                    if let (Some(p), Some(c)) = (self.provider.as_deref_mut(), self.cred.as_ref()) {
                        p.approve(c);
                    }
                }
            }
            return Ok((resp, url));
        }
    }
}

enum ExecError {
    Transport(Transport),
    Other(Error),
}

fn display(url: &Url) -> String {
    let s = url.without_credentials().to_string();
    let s = s.trim_end_matches('/');
    format!("{s}/")
}

fn drain(resp: &mut Response) {
    let _ = io::copy(&mut (&mut resp.body).take(1 << 20), &mut io::sink());
}

/// Lines of a `text/plain` error body (git prints them as `remote: ...`).
fn remote_lines(resp: &mut Response) -> Vec<String> {
    let plain = resp
        .header("content-type")
        .and_then(|c| c.split(';').next())
        .is_some_and(|c| c.trim().eq_ignore_ascii_case("text/plain"));
    if !plain {
        drain(resp);
        return Vec::new();
    }
    let mut buf = Vec::new();
    let _ = (&mut resp.body).take(64 * 1024).read_to_end(&mut buf);
    String::from_utf8_lossy(&buf)
        .lines()
        .map(|l| l.trim_end().to_string())
        .collect()
}

/// A dumb `info/refs`: lines of `<hex id>\t<refname>`.
fn looks_dumb(body: &[u8]) -> bool {
    let text = String::from_utf8_lossy(body);
    let mut any = false;
    for line in text.lines() {
        let Some((id, name)) = line.split_once('\t') else {
            return false;
        };
        if !(id.len() == 40 || id.len() == 64)
            || !id.bytes().all(|b| b.is_ascii_hexdigit())
            || name.is_empty()
        {
            return false;
        }
        any = true;
    }
    any
}

fn pkt_len(b: &[u8]) -> Option<usize> {
    let s = std::str::from_utf8(b.get(..4)?).ok()?;
    usize::from_str_radix(s, 16).ok()
}

/// Strip `# service=<svc>` and the following flush packet.
fn parse_advertisement(body: &[u8], svc: &str) -> Result<Advertisement> {
    let starts_with_service =
        body.len() >= 5 && body[..4].iter().all(|b| b.is_ascii_hexdigit()) && body[4] == b'#';
    if !starts_with_service {
        if pkt_len(body).is_some() {
            return Ok(Advertisement {
                data: body.to_vec(),
                had_service_line: false,
            });
        }
        let shown = String::from_utf8_lossy(&body[..body.len().min(4)]).into_owned();
        return Err(Error::InvalidServerResponse(
            messages::invalid_server_response(&shown),
        ));
    }
    let len = pkt_len(body).unwrap_or(0);
    if len < 4 || len > body.len() {
        return Err(Error::InvalidServerResponse(
            messages::invalid_server_response(&String::from_utf8_lossy(&body[..4])),
        ));
    }
    let payload = &body[4..len];
    let payload = payload.strip_suffix(b"\n").unwrap_or(payload);
    if payload != format!("# service={svc}").as_bytes() {
        return Err(Error::InvalidServerResponse(
            messages::invalid_service_line(&String::from_utf8_lossy(payload)),
        ));
    }
    let rest = &body[len..];
    // The service packet is followed by a flush packet.
    match rest.get(..4) {
        Some(b"0000") => Ok(Advertisement {
            data: rest[4..].to_vec(),
            had_service_line: true,
        }),
        _ => Err(Error::InvalidServerResponse(
            messages::invalid_server_response(&String::from_utf8_lossy(&rest[..rest.len().min(4)])),
        )),
    }
}

fn gzip_bytes(data: &[u8]) -> io::Result<Vec<u8>> {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;
    let mut e = GzEncoder::new(Vec::new(), Compression::default());
    e.write_all(data)?;
    e.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertisement_parsing() {
        let a = parse_advertisement(
            b"001e# service=git-upload-pack\n0000abcd",
            "git-upload-pack",
        )
        .unwrap();
        assert_eq!(a.data, b"abcd");
        assert!(a.had_service_line);
        let a = parse_advertisement(b"000eversion 2\n0000", "git-upload-pack").unwrap();
        assert!(!a.had_service_line);
        assert!(matches!(
            parse_advertisement(b"001f# service=git-receive-pack\n0000", "git-upload-pack"),
            Err(Error::InvalidServerResponse(_))
        ));
        assert!(parse_advertisement(b"<html>", "git-upload-pack").is_err());
        assert!(
            parse_advertisement(b"001e# service=git-upload-pack\nxxxx", "git-upload-pack").is_err()
        );
    }

    #[test]
    fn dumb_detection() {
        assert!(looks_dumb(
            b"95dcfa3633004da0049d3d0fa03f80589cbcaf31\trefs/heads/main\n"
        ));
        assert!(!looks_dumb(b""));
        assert!(!looks_dumb(b"<html></html>"));
    }

    #[test]
    fn display_url() {
        let u = Url::parse("http://u:p@h:1/r.git/").unwrap();
        assert_eq!(display(&u), "http://h:1/r.git/");
    }
}
