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
//! rejects the credential.
//!
//! GSS-Negotiate (RFC 4559) follows git with libcurl and `http.emptyAuth`:
//! with `true` empty credentials are in place from the start, with `auto`
//! (the default) after a `401` that offers `Negotiate`, which is then
//! retried once; with empty credentials, a `401` offering `Negotiate` is
//! answered once with `Authorization: Negotiate <token>` from the
//! [`Negotiator`] (resent without a token if none can be made, as libcurl
//! does). Only then are the credential helpers asked. After a successful
//! Negotiate exchange, later requests to the host carry a fresh token up
//! front (a streamed body cannot be replayed after a `401`). Header order and diagnostics match git 2.56.0
//! (see [`crate::client`] and [`crate::messages`]).

use std::io::{self, Read};

use crate::auth::{Credential, CredentialProvider};
use crate::client::{Client, ReqBody, Request, Response};
use crate::error::{Error, Result, Transport};
use crate::gss::{self, Negotiator};
use crate::messages;
use crate::options::{EmptyAuth, FollowRedirects, HttpOptions, ProactiveAuth};
use crate::url::Url;

/// libcurl's default redirect limit as set by git.
const MAX_REDIRECTS: usize = 20;

/// Bodies larger than this are gzip-compressed (upload-pack only).
const GZIP_THRESHOLD: usize = 1024;

/// The largest pkt-line (`gitprotocol-common(5)`).
const LARGE_PACKET_MAX: usize = 65520;

/// How many rounds a multistage scheme (`continue=1`) may take.
const MAX_AUTH_STAGES: usize = 8;

/// The service a session talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    /// `git-upload-pack` (fetch, clone, ls-remote).
    UploadPack,
    /// `git-receive-pack` (push).
    ReceivePack,
    /// `git-upload-archive` (`archive --remote`, protocol v2 only; its
    /// discovery goes to `git-upload-pack`).
    UploadArchive,
}

impl Service {
    /// `git-upload-pack` or `git-receive-pack`.
    pub fn name(self) -> &'static str {
        match self {
            Service::UploadPack => "git-upload-pack",
            Service::ReceivePack => "git-receive-pack",
            Service::UploadArchive => "git-upload-archive",
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
    prepared: bool,
    proactive_done: bool,
    proxy_cred: Option<Credential>,
    proxy_approved: bool,
    negotiator: Box<dyn Negotiator + 'a>,
    /// Empty credentials are in place (`http.emptyAuth`): Negotiate may
    /// be answered.
    empty_auth: bool,
    /// The host (`host:port`) where Negotiate worked.
    negotiate_host: Option<String>,
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
            prepared: false,
            proactive_done: false,
            proxy_cred: None,
            proxy_approved: false,
            negotiator: gss::system(),
            empty_auth: false,
            negotiate_host: None,
        })
    }

    /// Use `provider` to obtain credentials after a `401`.
    /// Use `n` for GSS-Negotiate instead of the system GSS-API library.
    pub fn with_negotiator(mut self, n: impl Negotiator + 'a) -> Self {
        self.negotiator = Box::new(n);
        self
    }

    /// An `Authorization: Negotiate` value for `url`, or `None` (with the
    /// reason in the curl trace).
    fn negotiate_token(&mut self, url: &Url) -> Option<String> {
        match self
            .negotiator
            .token(&url.host, self.client.opts.delegation)
        {
            Ok(t) => {
                self.client
                    .tracer
                    .info("Server auth using Negotiate with user ''");
                Some(format!("Negotiate {}", crate::auth::base64(&t)))
            }
            Err(why) => {
                self.client.tracer.info(&why);
                None
            }
        }
    }

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

    /// GET the URL of this session as a plain file (a bundle or bundle
    /// list of a bundle URI) and return the body of a `2xx` answer.
    pub fn get_file(&mut self) -> Result<Vec<u8>> {
        let url = self.url.clone();
        let headers = self.common_headers();
        let follow = self.client.opts.follow_redirects != FollowRedirects::Never;
        let disp = self.url.without_credentials().to_string();
        let (mut resp, _) = match self.execute("GET", url, headers, true, Payload::None, follow, "")
        {
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
            200..=299 => {}
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
        resp.read_all().map_err(|t| Error::Access {
            url: disp,
            status: None,
            reason: t.reason,
            remote: Vec::new(),
        })
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
        let (mut resp, final_url) = match self.execute(
            "GET",
            req_url.clone(),
            headers,
            true,
            Payload::None,
            follow,
            &suffix,
        ) {
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
        // git reads a request into a buffer of `http.postBuffer` bytes and
        // streams it chunked as soon as the buffer could not take another
        // packet of the maximum size (observed with git 2.56.1: a 250-byte
        // request is chunked with a 65536-byte buffer, not with 100000).
        let fits = |n: usize| n + LARGE_PACKET_MAX <= limit;
        match body {
            RpcBody::Bytes(b) if !fits(b.len()) => {
                let (head, rest) = b.split_at(b.len().min(limit));
                self.rpc_chunked(service, url, head, &mut io::Cursor::new(rest))
            }
            RpcBody::Bytes(b) => self.rpc_small(service, url, b),
            RpcBody::Stream(r) => {
                let mut head = Vec::new();
                (&mut *r).take(limit as u64 + 1).read_to_end(&mut head)?;
                if fits(head.len()) {
                    self.rpc_small(service, url, &head)
                } else {
                    self.rpc_chunked(service, url, &head, r)
                }
            }
        }
    }

    /// `GET` the session URL itself: a file such as a pack named in a
    /// `packfile-uris` section, following redirects as configured. A
    /// failure is an [`Error::Access`] with libcurl's reason
    /// (`The requested URL returned error: 404`).
    pub fn download(&mut self) -> Result<RpcResponse> {
        let url = self.url.clone();
        let shown = url.without_credentials().to_string();
        let headers = self.common_headers();
        let follow = self.client.opts.follow_redirects != FollowRedirects::Never;
        match self.execute("GET", url, headers, false, Payload::None, follow, "") {
            Ok((resp, _)) if (200..300).contains(&resp.status) => {
                let content_type = resp.header("content-type").map(str::to_string);
                Ok(RpcResponse {
                    inner: resp.body,
                    content_type,
                })
            }
            Ok((mut resp, _)) => {
                let status = resp.status;
                drain(&mut resp);
                Err(Error::Access {
                    url: shown,
                    status: Some(status),
                    reason: messages::returned_error(status),
                    remote: Vec::new(),
                })
            }
            Err(ExecError::Transport(t)) => Err(Error::Access {
                url: shown,
                status: None,
                reason: t.reason,
                remote: Vec::new(),
            }),
            Err(ExecError::Other(e)) => Err(e),
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
        let res = self.execute(
            "POST",
            url,
            headers,
            true,
            Payload::Bytes(payload),
            follow,
            svc,
        );
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
            svc,
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
            svc,
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
        if !same {
            return None;
        }
        c.authorization()
    }

    /// `http.proactiveAuth`: obtain a credential before the first request.
    fn proactive(&mut self, url: &Url) -> Result<()> {
        let mode = self.client.opts.proactive_auth;
        if self.cred.is_some()
            || self.proactive_done
            || mode == ProactiveAuth::None
            || self.client.opts.empty_auth == EmptyAuth::Always
        {
            return Ok(());
        }
        self.proactive_done = true;
        let wwwauth = if mode == ProactiveAuth::Basic {
            vec!["Basic".to_string()]
        } else {
            Vec::new()
        };
        let base = self.url.clone();
        self.obtain_credentials(url, &base, wwwauth)?;
        if let Some(c) = self.cred.as_mut() {
            c.wwwauth.clear();
        }
        Ok(())
    }

    /// Hand the current credential back to the provider as rejected.
    fn reject_credential(&mut self, wwwauth: Vec<String>) {
        if let Some(mut c) = self.cred.take() {
            if self.cred_from_provider {
                c.wwwauth = wwwauth;
                if let Some(p) = self.provider.as_deref_mut() {
                    p.reject(&c);
                }
            }
        }
    }

    /// Checks before the first request: `http.sslBackend`, the proxy
    /// URL, and the proxy password when the proxy URL names a user only.
    fn prepare(&mut self) -> Result<()> {
        if self.prepared {
            return Ok(());
        }
        self.prepared = true;
        if let Some(b) = &self.client.opts.ssl_backend {
            if self.url.scheme == crate::url::Scheme::Https
                && !matches!(b.as_str(), "rustls" | "openssl")
            {
                return Err(Error::Config(messages::unsupported_ssl_backend(b)));
            }
        }
        let Some(p) = self.client.opts.proxy_for(&self.url)? else {
            return Ok(());
        };
        let (Some(user), None) = (&p.user, &p.password) else {
            return Ok(());
        };
        let mut cred = Credential {
            protocol: p.kind.scheme().to_string(),
            host: format!("{}:{}", p.host, p.port),
            path: None,
            username: Some(user.clone()),
            ..Default::default()
        };
        match self.provider.as_deref_mut() {
            Some(prov) => prov.fill(&mut cred).map_err(Error::Credential)?,
            None => {
                return Err(Error::Credential(messages::no_password(&format!(
                    "{}://{}@{}",
                    cred.protocol, user, cred.host
                ))));
            }
        }
        self.client.proxy_password = cred.password.clone();
        self.proxy_cred = Some(cred);
        Ok(())
    }

    /// Ask for credentials after a 401. `base` is the repository URL the
    /// request went to (after redirects).
    fn obtain_credentials(&mut self, url: &Url, base: &Url, wwwauth: Vec<String>) -> Result<()> {
        let mut cred = Credential {
            protocol: url.scheme.as_str().to_string(),
            host: url.authority(),
            path: Some(base.credential_path()),
            username: self.url.user.clone().filter(|_| self.url.same_origin(url)),
            password: self
                .url
                .password
                .clone()
                .filter(|_| self.url.same_origin(url)),
            wwwauth,
            ..Default::default()
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
                    messages::no_password(with_user.to_string().trim_end_matches('/'))
                }
                None => messages::no_username(&url.origin()),
            };
            return Err(Error::Credential(msg));
        }
        self.cred = Some(cred);
        self.approved = false;
        Ok(())
    }

    /// What to do with a `429`: `Ok(Some(delay))` to retry after `delay`
    /// seconds, `Ok(None)` to fail with the response as it is.
    fn rate_limit(
        &mut self,
        resp: &mut Response,
        attempts: &mut i64,
    ) -> std::result::Result<Option<u64>, ExecError> {
        let o = &self.client.opts;
        let (max, max_time, configured) = (o.max_retries, o.max_retry_time, o.retry_after);
        if max == 0 {
            return Ok(None);
        }
        let delay = match resp.header("retry-after").and_then(parse_retry_after) {
            Some(d) => {
                if i64::try_from(d).unwrap_or(i64::MAX) > max_time {
                    (self.diagnostics)(&format!(
                        "error: {}",
                        messages::retry_delay_exceeds(d, max_time)
                    ));
                    drain(resp);
                    return Ok(None);
                }
                d
            }
            None => {
                if configured > max_time {
                    (self.diagnostics)(&format!(
                        "error: {}",
                        messages::configured_retry_exceeds(configured, max_time)
                    ));
                    drain(resp);
                    return Ok(None);
                }
                configured.max(0) as u64
            }
        };
        *attempts += 1;
        if max > 0 && *attempts >= max {
            let remote = remote_lines(resp);
            return Err(ExecError::Other(Error::RateLimited {
                url: self.display_url(),
                secs: delay,
                remote,
            }));
        }
        drain(resp);
        if delay > 0 {
            (self.diagnostics)(&format!(
                "warning: {}",
                messages::rate_limited_waiting(delay)
            ));
            std::thread::sleep(std::time::Duration::from_secs(delay));
        }
        Ok(Some(delay))
    }

    /// Send a request, handling `401` (once), `429` (`http.maxRetries`)
    /// and redirects. `suffix` is what the request URL adds to the
    /// repository URL (to find the repository after a redirect).
    #[allow(clippy::too_many_arguments)]
    fn execute(
        &mut self,
        method: &'static str,
        mut url: Url,
        headers: Vec<(String, String)>,
        accept_encoding: bool,
        mut payload: Payload<'_>,
        follow: bool,
        suffix: &str,
    ) -> std::result::Result<(Response, Url), ExecError> {
        self.prepare().map_err(ExecError::Other)?;
        self.proactive(&url).map_err(ExecError::Other)?;
        let mut redirects = 0;
        let mut auth_retried = false;
        let mut stages = 0;
        let mut attempts_429 = 0;
        if self.client.opts.empty_auth == EmptyAuth::Always && self.url.user.is_none() {
            self.empty_auth = true;
        }
        let mut negotiate_tried = false;
        let mut negotiate_auth: Option<String> = None;
        // The last 401 offered Negotiate only: like libcurl, do not send a
        // user name and password (Basic) the server did not ask for.
        let mut negotiate_only = false;
        loop {
            let mut sent_negotiate = false;
            let mut authorization = negotiate_auth.take();
            if authorization.is_some() {
                sent_negotiate = true;
            } else {
                authorization = self.authorization_for(&url).filter(|_| {
                    !negotiate_only || self.cred.as_ref().is_some_and(|c| c.credential.is_some())
                });
            }
            if authorization.is_none()
                && !negotiate_tried
                && self.negotiate_host.as_deref() == Some(url.authority().as_str())
            {
                negotiate_tried = true;
                authorization = self.negotiate_token(&url);
                sent_negotiate = authorization.is_some();
            }
            let sent_auth = authorization.is_some() && !sent_negotiate;
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
                let wwwauth = www_authenticate(&resp);
                drain(&mut resp);
                // Multistage schemes: the helper asked for another round.
                let multistage = sent_auth
                    && self.cred_from_provider
                    && self.cred.as_ref().is_some_and(|c| c.multistage);
                if multistage && stages < MAX_AUTH_STAGES && payload.replayable() {
                    stages += 1;
                    if let (Some(c), Some(p)) = (self.cred.as_mut(), self.provider.as_deref_mut()) {
                        c.wwwauth = wwwauth;
                        c.credential = None;
                        c.multistage = false;
                        p.fill(c)
                            .map_err(|e| ExecError::Other(Error::Credential(e)))?;
                    }
                    continue;
                }
                // GSS-Negotiate with empty credentials (http.emptyAuth).
                let offers_negotiate = wwwauth.iter().any(|v| {
                    v.split_whitespace()
                        .next()
                        .is_some_and(|s| s.eq_ignore_ascii_case("negotiate"))
                });
                negotiate_only = offers_negotiate
                    && wwwauth.iter().all(|v| {
                        v.split_whitespace()
                            .next()
                            .is_some_and(|s| s.eq_ignore_ascii_case("negotiate"))
                    });
                if sent_negotiate {
                    self.negotiate_host = None;
                }
                if offers_negotiate
                    && !sent_auth
                    && self.cred.is_none()
                    && self.url.user.is_none()
                    && payload.replayable()
                    && self.client.opts.empty_auth != EmptyAuth::Never
                {
                    if self.empty_auth && !negotiate_tried {
                        negotiate_tried = true;
                        negotiate_auth = self.negotiate_token(&url);
                        self.client.tracer.info(&format!(
                            "Issue another request to this URL: '{}'",
                            url.without_credentials()
                        ));
                        continue;
                    }
                    if !self.empty_auth {
                        self.empty_auth = true;
                        continue;
                    }
                }
                if sent_auth || auth_retried || !payload.replayable() {
                    self.reject_credential(wwwauth);
                    return Err(ExecError::Other(Error::AuthenticationFailed {
                        url: self.display_url(),
                    }));
                }
                let base = if redirects > 0 {
                    url.without_credentials()
                        .strip_suffix(suffix)
                        .unwrap_or_else(|| self.url.clone())
                } else {
                    self.url.clone()
                };
                self.obtain_credentials(&url, &base, wwwauth)
                    .map_err(ExecError::Other)?;
                auth_retried = true;
                continue;
            }
            if status == 407 {
                if let Some(c) = self.proxy_cred.take() {
                    if let Some(p) = self.provider.as_deref_mut() {
                        p.reject(&c);
                    }
                }
            }
            if status == 429
                && payload.replayable()
                && self.rate_limit(&mut resp, &mut attempts_429)?.is_some()
            {
                continue;
            }
            if (200..300).contains(&status) && !self.proxy_approved {
                if let Some(c) = &self.proxy_cred {
                    self.proxy_approved = true;
                    if let Some(p) = self.provider.as_deref_mut() {
                        p.approve(c);
                    }
                }
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
                    // Only protocols allowed for URLs that do not come
                    // from the user (protocol.<name>.allow).
                    let scheme = match loc.trim().split_once("://") {
                        Some((s, _)) => s.to_ascii_lowercase(),
                        None => url.scheme.as_str().to_owned(),
                    };
                    if !self.client.opts.redirect_schemes.contains(&scheme.as_str()) {
                        return Err(ExecError::Transport(Transport::new(
                            1,
                            messages::protocol_disabled(&scheme),
                        )));
                    }
                    self.client
                        .tracer
                        .info(&format!("Issue another request to this URL: '{loc}'"));
                    url = url.resolve(&loc).map_err(ExecError::Other)?;
                    continue;
                }
            }
            if (200..300).contains(&status) && sent_negotiate {
                self.negotiate_host = Some(url.authority());
            }
            if (200..300).contains(&status) && sent_auth && !self.approved {
                self.approved = true;
                if self.cred_from_provider {
                    let wwwauth = www_authenticate(&resp);
                    if let (Some(p), Some(c)) = (self.provider.as_deref_mut(), self.cred.as_mut()) {
                        c.wwwauth = wwwauth;
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

/// The `WWW-Authenticate` values of a response, in order.
fn www_authenticate(resp: &Response) -> Vec<String> {
    resp.headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("www-authenticate"))
        .map(|(_, v)| v.clone())
        .collect()
}

/// `Retry-After`: seconds, or an HTTP date (a date in the past means now).
fn parse_retry_after(v: &str) -> Option<u64> {
    let v = v.trim();
    if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) {
        return v.parse().ok();
    }
    let t = crate::cookies::http_date(v)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Some(t.saturating_sub(now))
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
