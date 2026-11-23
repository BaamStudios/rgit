// SPDX-License-Identifier: MIT
//! `http.*` configuration and the environment variables that override it.
//!
//! Behaviour follows `Documentation/config/http.adoc` and `git.adoc`:
//!
//! - `http.<url>.<key>` applies when the URL matches (scheme, host with
//!   `*` per label, port with defaults, path prefix on `/` boundaries, user
//!   name); a longer path match wins over a user match, any URL match wins
//!   over the plain `http.<key>`, and later entries win among equals;
//! - environment variables win over configuration: `GIT_SSL_NO_VERIFY`,
//!   `GIT_SSL_CAINFO`, `GIT_SSL_CAPATH`, `GIT_HTTP_USER_AGENT`,
//!   `GIT_HTTP_LOW_SPEED_LIMIT`, `GIT_HTTP_LOW_SPEED_TIME`, `GIT_SMART_HTTP`;
//! - the proxy comes from `http.proxy` (or the caller, for
//!   `remote.<name>.proxy`), otherwise from `http_proxy` (lower case only),
//!   `https_proxy`/`HTTPS_PROXY` and `all_proxy`/`ALL_PROXY`, the way
//!   libcurl reads them; `no_proxy`/`NO_PROXY` always applies;
//! - `Accept-Language` is built from `LANGUAGE`, `LC_ALL`, `LC_MESSAGES`,
//!   `LANG` (first non-empty), as observed from git 2.56.0:
//!   `ko_KR.UTF-8:de_DE:fr` gives `ko-KR, de-DE;q=0.9, fr;q=0.8, *;q=0.7`,
//!   and with ten or more entries the weights get two decimals.

use std::path::PathBuf;

use bstr::ByteSlice;
use rgit_config::value::{parse_bool, parse_int};
use rgit_config::{ConfigSet, Entry, Environment, IntWidth};

use crate::error::Error;
use crate::messages;
use crate::url::{Scheme, Url, percent_decode};

/// `http.followRedirects`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowRedirects {
    /// `false`: every redirect is an error.
    Never,
    /// `initial` (default): only the discovery request follows redirects.
    Initial,
    /// `true`: every request follows redirects.
    Always,
}

/// `http.version`. rgit speaks HTTP/1.1 only; `HTTP/2` is accepted and
/// falls back to HTTP/1.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpVersion {
    /// Not configured.
    Default,
    /// `HTTP/1.1`.
    Http11,
    /// `HTTP/2` (served as HTTP/1.1).
    Http2,
}

/// `http.emptyAuth`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyAuth {
    /// `auto` (default): only when the server offers a scheme that needs it.
    Auto,
    /// `true`: send empty credentials with the first request.
    Always,
    /// `false`.
    Never,
}

/// `http.proactiveAuth`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProactiveAuth {
    /// `none` (default): authenticate after a `401` only.
    None,
    /// `basic`: ask the helper for a user name and password up front.
    Basic,
    /// `auto`: let the helper pick the scheme up front.
    Auto,
}

/// `http.proxyAuthMethod`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyAuthMethod {
    /// `anyauth` (default).
    Any,
    /// `basic`.
    Basic,
    /// `digest`.
    Digest,
    /// `negotiate`.
    Negotiate,
    /// `ntlm`.
    Ntlm,
}

/// `http.sslVersion`: the lowest TLS version to accept. rustls speaks
/// TLS 1.2 and 1.3 only, so every TLS version below 1.3 means "1.2 or
/// later".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SslVersion {
    /// Not configured (or `GIT_SSL_VERSION` is empty).
    Default,
    /// `tlsv1`, `tlsv1.0`, `tlsv1.1`, `tlsv1.2`.
    Tls12,
    /// `tlsv1.3`.
    Tls13,
    /// `sslv2`, `sslv3`: protocols rustls does not implement.
    Ssl,
}

/// Effective HTTP settings for one remote URL.
#[derive(Debug, Clone)]
pub struct HttpOptions {
    /// `http.sslVerify` / `GIT_SSL_NO_VERIFY`.
    pub ssl_verify: bool,
    /// `http.sslCAInfo` / `GIT_SSL_CAINFO`: replaces the system roots.
    pub ssl_ca_info: Option<PathBuf>,
    /// `http.sslCAPath` / `GIT_SSL_CAPATH`: replaces the system roots.
    pub ssl_ca_path: Option<PathBuf>,
    /// `http.proxy` (or `remote.<name>.proxy`); `Some("")` disables proxies.
    pub proxy: Option<String>,
    /// Proxy variables from the environment.
    pub env_proxy: EnvProxy,
    /// `http.extraHeader` lines.
    pub extra_headers: Vec<String>,
    /// `User-Agent`.
    pub user_agent: String,
    /// `http.lowSpeedLimit` (bytes per second; 0 = off).
    pub low_speed_limit: u64,
    /// `http.lowSpeedTime` (seconds; 0 = off).
    pub low_speed_time: u64,
    /// `http.postBuffer` (default 1 MiB).
    pub post_buffer: usize,
    /// `http.followRedirects`.
    pub follow_redirects: FollowRedirects,
    /// `http.version`.
    pub version: HttpVersion,
    /// `Accept-Language` value, if any.
    pub accept_language: Option<String>,
    /// `GIT_SMART_HTTP` (default true). When false, discovery asks for the
    /// dumb `info/refs`, which rgit then rejects.
    pub smart: bool,
    /// Verbosity of the calling command; above 1, POST sizes are reported
    /// (`POST git-receive-pack (123 bytes)`).
    pub verbosity: i32,
    /// Schemes a redirect may lead to: those that `protocol.allow`,
    /// `protocol.<name>.allow` and `GIT_ALLOW_PROTOCOL` allow for a URL
    /// that does not come from the user (`user` does not count).
    pub redirect_schemes: Vec<&'static str>,
    /// `http.cookieFile`; `Some("")` keeps cookies in memory only.
    pub cookie_file: Option<PathBuf>,
    /// `http.saveCookies`.
    pub save_cookies: bool,
    /// `http.curloptResolve` entries in order (`[+]HOST:PORT:ADDR[,ADDR]`,
    /// `-HOST:PORT`).
    pub resolve: Vec<String>,
    /// `http.maxRetries` / `GIT_HTTP_MAX_RETRIES` (0 = no retries on 429).
    pub max_retries: i64,
    /// `http.retryAfter` / `GIT_HTTP_RETRY_AFTER` (seconds).
    pub retry_after: i64,
    /// `http.maxRetryTime` / `GIT_HTTP_MAX_RETRY_TIME` (seconds).
    pub max_retry_time: i64,
    /// `http.sslCert` / `GIT_SSL_CERT`.
    pub ssl_cert: Option<PathBuf>,
    /// `http.sslKey` / `GIT_SSL_KEY`.
    pub ssl_key: Option<PathBuf>,
    /// `http.sslCertPasswordProtected` / `GIT_SSL_CERT_PASSWORD_PROTECTED`.
    pub ssl_cert_password_protected: bool,
    /// `http.sslCertType` / `GIT_SSL_CERT_TYPE`.
    pub ssl_cert_type: Option<String>,
    /// `http.sslKeyType` / `GIT_SSL_KEY_TYPE`.
    pub ssl_key_type: Option<String>,
    /// `http.sslVersion` / `GIT_SSL_VERSION`.
    pub ssl_version: SslVersion,
    /// `http.sslCipherList` / `GIT_SSL_CIPHER_LIST`.
    pub ssl_cipher_list: Option<String>,
    /// `http.pinnedPubkey`.
    pub pinned_pubkey: Option<String>,
    /// `http.sslBackend`.
    pub ssl_backend: Option<String>,
    /// `http.emptyAuth`.
    pub empty_auth: EmptyAuth,
    /// `http.proactiveAuth`.
    pub proactive_auth: ProactiveAuth,
    /// `http.proxyAuthMethod` / `GIT_HTTP_PROXY_AUTHMETHOD`.
    pub proxy_auth_method: ProxyAuthMethod,
    /// Warnings git prints for unusable values (`unsupported ssl version
    /// ...: using default`); the caller prints them as `warning: <line>`.
    pub warnings: Vec<String>,
}

/// `http_proxy`, `https_proxy`, `all_proxy` and `no_proxy` as libcurl
/// reads them.
#[derive(Debug, Clone, Default)]
pub struct EnvProxy {
    /// `http_proxy` (lower case only).
    pub http: Option<String>,
    /// `https_proxy` or `HTTPS_PROXY`.
    pub https: Option<String>,
    /// `all_proxy` or `ALL_PROXY`.
    pub all: Option<String>,
    /// `no_proxy` or `NO_PROXY`.
    pub no_proxy: Option<String>,
}

/// Default `http.postBuffer`.
pub const DEFAULT_POST_BUFFER: usize = 1024 * 1024;

/// Default `http.maxRetryTime` (seconds).
pub const DEFAULT_MAX_RETRY_TIME: i64 = 300;

impl Default for HttpOptions {
    fn default() -> Self {
        HttpOptions {
            ssl_verify: true,
            ssl_ca_info: None,
            ssl_ca_path: None,
            proxy: None,
            env_proxy: EnvProxy::default(),
            extra_headers: Vec::new(),
            user_agent: default_user_agent(),
            low_speed_limit: 0,
            low_speed_time: 0,
            post_buffer: DEFAULT_POST_BUFFER,
            follow_redirects: FollowRedirects::Initial,
            version: HttpVersion::Default,
            accept_language: None,
            smart: true,
            verbosity: 0,
            redirect_schemes: vec!["http", "https"],
            cookie_file: None,
            save_cookies: false,
            resolve: Vec::new(),
            max_retries: 0,
            retry_after: 0,
            max_retry_time: DEFAULT_MAX_RETRY_TIME,
            ssl_cert: None,
            ssl_key: None,
            ssl_cert_password_protected: false,
            ssl_cert_type: None,
            ssl_key_type: None,
            ssl_version: SslVersion::Default,
            ssl_cipher_list: None,
            pinned_pubkey: None,
            ssl_backend: None,
            empty_auth: EmptyAuth::Auto,
            proactive_auth: ProactiveAuth::None,
            proxy_auth_method: ProxyAuthMethod::Any,
            warnings: Vec::new(),
        }
    }
}

/// `git/<version>`, the default `User-Agent`.
pub fn default_user_agent() -> String {
    format!("git/{}", rgit_protocol::agent::version())
}

/// A proxy to send requests through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proxy {
    /// The protocol spoken with the proxy.
    pub kind: ProxyKind,
    /// Host name.
    pub host: String,
    /// Port (libcurl's default is 1080).
    pub port: u16,
    /// User name for Basic proxy authentication.
    pub user: Option<String>,
    /// Password for Basic proxy authentication.
    pub password: Option<String>,
    /// A Unix socket path (`socks5://localhost/path/to/socket`).
    pub socket: Option<String>,
}

/// The proxy protocols git accepts (`[protocol://]` of `http.proxy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyKind {
    /// `http://` (and no protocol).
    Http,
    /// `https://`: TLS to the proxy (not supported by rgit).
    Https,
    /// `socks4://`: the client resolves the target to IPv4.
    Socks4,
    /// `socks4a://`: the proxy resolves the target.
    Socks4a,
    /// `socks5://`: the client resolves the target.
    Socks5,
    /// `socks5h://`: the proxy resolves the target.
    Socks5h,
}

impl ProxyKind {
    /// Whether this is one of the SOCKS protocols.
    pub fn is_socks(self) -> bool {
        !matches!(self, ProxyKind::Http | ProxyKind::Https)
    }

    /// The scheme as written in the proxy URL.
    pub fn scheme(self) -> &'static str {
        match self {
            ProxyKind::Http => "http",
            ProxyKind::Https => "https",
            ProxyKind::Socks4 => "socks4",
            ProxyKind::Socks4a => "socks4a",
            ProxyKind::Socks5 => "socks5",
            ProxyKind::Socks5h => "socks5h",
        }
    }
}

impl HttpOptions {
    /// Settings for `url` from the configuration and the environment.
    pub fn load(cfg: &ConfigSet, url: &Url, env: &dyn Environment) -> Result<HttpOptions, Error> {
        let mut o = HttpOptions::default();
        o.apply_config(cfg, url, env)?;
        o.apply_env(env);
        o.redirect_schemes = ["http", "https"]
            .into_iter()
            .filter(|s| {
                matches!(
                    rgit_remote::protocol::policy(s.as_bytes(), cfg, env),
                    Ok(rgit_remote::Policy::Always)
                )
            })
            .collect();
        Ok(o)
    }

    /// Apply `http.*` and matching `http.<url>.*` entries.
    pub fn apply_config(
        &mut self,
        cfg: &ConfigSet,
        url: &Url,
        env: &dyn Environment,
    ) -> Result<(), Error> {
        let cerr = |e: rgit_config::ValueError| Error::Config(e.to_string());
        let mut best: std::collections::HashMap<Vec<u8>, Rank> = Default::default();
        let mut winners: Vec<(Vec<u8>, &Entry)> = Vec::new();
        let mut headers: Vec<String> = Vec::new();
        let mut resolve: Vec<String> = Vec::new();
        for e in cfg.entries() {
            let (section, sub, key) = rgit_config::key::split_canonical(e.name.as_bstr());
            if section != "http" {
                continue;
            }
            let rank = match sub {
                None => Rank::PLAIN,
                Some(pat) => match url_match(&pat.to_str_lossy(), url) {
                    Some(r) => r,
                    None => continue,
                },
            };
            let key = key.to_vec();
            if key == b"extraheader" {
                match e.value.as_deref() {
                    Some(v) if !v.is_empty() => headers.push(v.to_str_lossy().into_owned()),
                    Some(_) => headers.clear(),
                    None => return Err(cerr(e.string().unwrap_err())),
                }
                continue;
            }
            if key == b"curloptresolve" {
                match e.value.as_deref() {
                    Some(v) if !v.is_empty() => resolve.push(v.to_str_lossy().into_owned()),
                    Some(_) => resolve.clear(),
                    None => return Err(cerr(e.string().unwrap_err())),
                }
                continue;
            }
            let cur = best.get(&key).copied().unwrap_or(Rank::NONE);
            if rank >= cur {
                best.insert(key.clone(), rank);
                winners.retain(|(k, _)| *k != key);
                winners.push((key, e));
            }
        }
        self.extra_headers.extend(headers);
        self.resolve.extend(resolve);
        let string = |e: &Entry| -> Result<String, Error> {
            Ok(e.string().map_err(cerr)?.to_str_lossy().into_owned())
        };
        for (key, e) in winners {
            match key.as_slice() {
                b"sslverify" => self.ssl_verify = e.bool().map_err(cerr)?,
                b"sslcainfo" => self.ssl_ca_info = e.path(env).map_err(cerr)?,
                b"sslcapath" => self.ssl_ca_path = e.path(env).map_err(cerr)?,
                b"proxy" => {
                    self.proxy = Some(e.string().map_err(cerr)?.to_str_lossy().into_owned())
                }
                b"useragent" => {
                    self.user_agent = e.string().map_err(cerr)?.to_str_lossy().into_owned()
                }
                b"lowspeedlimit" => {
                    self.low_speed_limit = e.int(IntWidth::I64).map_err(cerr)?.max(0) as u64
                }
                b"lowspeedtime" => {
                    self.low_speed_time = e.int(IntWidth::I64).map_err(cerr)?.max(0) as u64
                }
                b"postbuffer" => {
                    let v = e.int(IntWidth::I64).map_err(cerr)?;
                    self.post_buffer = v.max(1) as usize;
                }
                b"followredirects" => {
                    let v = e.value.as_deref().map(|v| v.as_bytes());
                    self.follow_redirects = if v.is_some_and(|v| v.eq_ignore_ascii_case(b"initial"))
                    {
                        FollowRedirects::Initial
                    } else {
                        match e.bool() {
                            Ok(true) => FollowRedirects::Always,
                            Ok(false) => FollowRedirects::Never,
                            // git warns and keeps the default; stay quiet.
                            Err(_) => FollowRedirects::Initial,
                        }
                    };
                }
                b"version" => {
                    let v = e.string().map_err(cerr)?;
                    self.version = if v == "HTTP/2" {
                        HttpVersion::Http2
                    } else if v == "HTTP/1.1" {
                        HttpVersion::Http11
                    } else {
                        self.warnings
                            .push(messages::unknown_http_version(&v.to_str_lossy()));
                        HttpVersion::Default
                    };
                }
                b"cookiefile" => self.cookie_file = e.path(env).map_err(cerr)?,
                b"savecookies" => self.save_cookies = e.bool().map_err(cerr)?,
                b"maxretries" => self.max_retries = e.int(IntWidth::I64).map_err(cerr)?,
                b"retryafter" => self.retry_after = e.int(IntWidth::I64).map_err(cerr)?,
                b"maxretrytime" => self.max_retry_time = e.int(IntWidth::I64).map_err(cerr)?,
                b"sslcert" => self.ssl_cert = e.path(env).map_err(cerr)?,
                b"sslkey" => self.ssl_key = e.path(env).map_err(cerr)?,
                b"sslcertpasswordprotected" => {
                    self.ssl_cert_password_protected = e.bool().map_err(cerr)?
                }
                b"sslcerttype" => self.ssl_cert_type = Some(string(e)?),
                b"sslkeytype" => self.ssl_key_type = Some(string(e)?),
                b"sslversion" => self.set_ssl_version(&string(e)?),
                b"sslcipherlist" => self.ssl_cipher_list = Some(string(e)?),
                b"pinnedpubkey" => self.pinned_pubkey = Some(string(e)?),
                b"sslbackend" => self.ssl_backend = Some(string(e)?),
                b"emptyauth" => {
                    let v = e.value.as_deref().map(|v| v.as_bytes());
                    self.empty_auth = if v.is_some_and(|v| v.eq_ignore_ascii_case(b"auto")) {
                        EmptyAuth::Auto
                    } else if e.bool().map_err(cerr)? {
                        EmptyAuth::Always
                    } else {
                        EmptyAuth::Never
                    };
                }
                b"proactiveauth" => {
                    self.proactive_auth = match string(e)?.as_str() {
                        "basic" => ProactiveAuth::Basic,
                        "auto" => ProactiveAuth::Auto,
                        "none" => ProactiveAuth::None,
                        _ => {
                            self.warnings
                                .push(messages::UNKNOWN_PROACTIVE_AUTH.to_string());
                            ProactiveAuth::None
                        }
                    };
                }
                b"proxyauthmethod" => self.set_proxy_auth_method(&string(e)?),
                b"delegation" => {
                    let v = string(e)?;
                    if !matches!(v.as_str(), "none" | "policy" | "always") {
                        self.warnings.push(messages::unknown_delegation(&v));
                    }
                }
                // Checked like git, without an effect on this client (see
                // docs/http-config.md).
                b"maxrequests" | b"minsessions" | b"keepaliveidle" | b"keepaliveinterval"
                | b"keepalivecount" => {
                    e.int(IntWidth::I32).map_err(cerr)?;
                }
                b"noepsv"
                | b"ssltry"
                | b"schannelcheckrevoke"
                | b"schannelusesslcainfo"
                | b"proxysslcertpasswordprotected" => {
                    e.bool().map_err(cerr)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Apply environment overrides and read proxy and language variables.
    pub fn apply_env(&mut self, env: &dyn Environment) {
        let var = |n: &str| env.var(n).map(|v| v.to_string_lossy().into_owned());
        if env.var("GIT_SSL_NO_VERIFY").is_some() {
            self.ssl_verify = false;
        }
        if let Some(v) = var("GIT_SSL_CAINFO") {
            self.ssl_ca_info = Some(PathBuf::from(v));
        }
        if let Some(v) = var("GIT_SSL_CAPATH") {
            self.ssl_ca_path = Some(PathBuf::from(v));
        }
        if let Some(v) = var("GIT_HTTP_USER_AGENT") {
            self.user_agent = v;
        }
        if let Some(v) = var("GIT_HTTP_LOW_SPEED_LIMIT")
            .and_then(|v| parse_int(v.as_bytes(), IntWidth::I64).ok())
        {
            self.low_speed_limit = v.max(0) as u64;
        }
        if let Some(v) =
            var("GIT_HTTP_LOW_SPEED_TIME").and_then(|v| parse_int(v.as_bytes(), IntWidth::I64).ok())
        {
            self.low_speed_time = v.max(0) as u64;
        }
        let int = |n: &str| var(n).and_then(|v| parse_int(v.as_bytes(), IntWidth::I64).ok());
        if let Some(v) = int("GIT_HTTP_MAX_RETRIES") {
            self.max_retries = v;
        }
        if let Some(v) = int("GIT_HTTP_RETRY_AFTER") {
            self.retry_after = v;
        }
        if let Some(v) = int("GIT_HTTP_MAX_RETRY_TIME") {
            self.max_retry_time = v;
        }
        if let Some(v) = var("GIT_SSL_CERT") {
            self.ssl_cert = Some(PathBuf::from(v));
        }
        if let Some(v) = var("GIT_SSL_KEY") {
            self.ssl_key = Some(PathBuf::from(v));
        }
        if let Some(v) = var("GIT_SSL_CERT_TYPE") {
            self.ssl_cert_type = Some(v);
        }
        if let Some(v) = var("GIT_SSL_KEY_TYPE") {
            self.ssl_key_type = Some(v);
        }
        if let Some(v) = var("GIT_SSL_CERT_PASSWORD_PROTECTED") {
            self.ssl_cert_password_protected = parse_bool(Some(v.as_bytes())).unwrap_or(false);
        }
        if let Some(v) = var("GIT_SSL_VERSION") {
            if v.is_empty() {
                self.ssl_version = SslVersion::Default;
            } else {
                self.set_ssl_version(&v);
            }
        }
        if let Some(v) = var("GIT_SSL_CIPHER_LIST") {
            self.ssl_cipher_list = Some(v).filter(|v| !v.is_empty());
        }
        if let Some(v) = var("GIT_HTTP_PROXY_AUTHMETHOD") {
            self.set_proxy_auth_method(&v);
        }
        if let Some(v) = var("GIT_SMART_HTTP") {
            self.smart = parse_bool(Some(v.as_bytes())).unwrap_or(true);
        }
        let either = |a: &str, b: &str| var(a).or_else(|| var(b)).filter(|v| !v.is_empty());
        self.env_proxy = EnvProxy {
            http: var("http_proxy").filter(|v| !v.is_empty()),
            https: either("https_proxy", "HTTPS_PROXY"),
            all: either("all_proxy", "ALL_PROXY"),
            no_proxy: either("no_proxy", "NO_PROXY"),
        };
        self.accept_language = accept_language(env);
    }

    fn set_ssl_version(&mut self, v: &str) {
        self.ssl_version = match v {
            "tlsv1" | "tlsv1.0" | "tlsv1.1" | "tlsv1.2" => SslVersion::Tls12,
            "tlsv1.3" => SslVersion::Tls13,
            "sslv2" | "sslv3" => SslVersion::Ssl,
            _ => {
                self.warnings.push(messages::unsupported_ssl_version(v));
                SslVersion::Default
            }
        };
    }

    pub(crate) fn set_proxy_auth_method(&mut self, v: &str) {
        self.proxy_auth_method = match v {
            "anyauth" => ProxyAuthMethod::Any,
            "basic" => ProxyAuthMethod::Basic,
            "digest" => ProxyAuthMethod::Digest,
            "negotiate" => ProxyAuthMethod::Negotiate,
            "ntlm" => ProxyAuthMethod::Ntlm,
            _ => {
                self.warnings
                    .push(messages::unsupported_proxy_auth_method(v));
                ProxyAuthMethod::Any
            }
        };
    }

    /// The proxy to use for `url`, if any.
    pub fn proxy_for(&self, url: &Url) -> Result<Option<Proxy>, Error> {
        if let Some(np) = &self.env_proxy.no_proxy {
            if no_proxy_matches(np, &url.host) {
                return Ok(None);
            }
        }
        let (spec, strict) = match &self.proxy {
            Some(p) if p.is_empty() => return Ok(None),
            Some(p) => (Some(p.clone()), true),
            None => (
                match url.scheme {
                    Scheme::Http => self.env_proxy.http.clone(),
                    Scheme::Https => self.env_proxy.https.clone(),
                }
                .or_else(|| self.env_proxy.all.clone()),
                false,
            ),
        };
        spec.map(|s| parse_proxy_spec(&s, strict)).transpose()
    }
}

/// Parse `[protocol://][user[:password]@]host[:port][/path]` as git
/// checks `http.proxy`: the protocol is one of `http`, `https`, `socks4`,
/// `socks4a`, `socks5`, `socks5h` (lower case), and only SOCKS proxies on
/// `localhost` may have a path (a Unix socket).
pub fn parse_proxy(spec: &str) -> Result<Proxy, Error> {
    parse_proxy_spec(spec, true)
}

fn parse_proxy_spec(spec: &str, strict: bool) -> Result<Proxy, Error> {
    let invalid = |reason: &str| Error::Config(messages::invalid_proxy_url(spec, reason));
    let (kind, rest) = match spec.split_once("://") {
        Some((scheme, rest)) => {
            let s = if strict {
                scheme.to_string()
            } else {
                scheme.to_ascii_lowercase()
            };
            let kind = match s.as_str() {
                "http" => ProxyKind::Http,
                "https" => ProxyKind::Https,
                "socks4" => ProxyKind::Socks4,
                "socks4a" => ProxyKind::Socks4a,
                "socks5" => ProxyKind::Socks5,
                "socks5h" => ProxyKind::Socks5h,
                _ => return Err(invalid(&messages::unsupported_proxy_scheme(scheme))),
            };
            (kind, rest)
        }
        None => (ProxyKind::Http, spec),
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let unsupported = || Error::Config(messages::invalid_proxy_url(spec, "bad host or port"));
    let (userinfo, hostport) = match authority.rfind('@') {
        Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
        None => (None, authority),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(']') || p.parse::<u16>().is_ok() => (
            h.trim_matches(['[', ']']).to_string(),
            p.parse().map_err(|_| unsupported())?,
        ),
        _ => (
            hostport.trim_matches(['[', ']']).to_string(),
            if kind == ProxyKind::Https { 443 } else { 1080 },
        ),
    };
    if host.is_empty() {
        return Err(unsupported());
    }
    let socket = if path.is_empty() || path == "/" {
        None
    } else if !kind.is_socks() {
        return Err(invalid(messages::PROXY_PATH_NOT_SOCKS));
    } else if host != "localhost" {
        return Err(invalid(messages::PROXY_PATH_NOT_LOCALHOST));
    } else {
        Some(percent_decode(path))
    };
    let (user, password) = match userinfo {
        Some(ui) => match ui.split_once(':') {
            Some((u, p)) => (Some(percent_decode(u)), Some(percent_decode(p))),
            None => (Some(percent_decode(ui)), None),
        },
        None => (None, None),
    };
    Ok(Proxy {
        kind,
        host,
        port,
        user,
        password,
        socket,
    })
}

fn no_proxy_matches(list: &str, host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    list.split([',', ' '])
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .any(|e| {
            if e == "*" {
                return true;
            }
            let e = e.trim_start_matches('.').to_ascii_lowercase();
            let e = e.split(':').next().unwrap_or("").to_string();
            host == e || host.ends_with(&format!(".{e}"))
        })
}

/// How well a configuration key's URL matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Rank {
    url: bool,
    path_len: usize,
    user: bool,
}

impl Rank {
    const NONE: Rank = Rank {
        url: false,
        path_len: 0,
        user: false,
    };
    const PLAIN: Rank = Rank::NONE;
}

/// Match a `http.<url>.*` subsection against a URL.
fn url_match(pattern: &str, url: &Url) -> Option<Rank> {
    let (pscheme, rest) = pattern.split_once("://")?;
    if !pscheme.eq_ignore_ascii_case(url.scheme.as_str()) {
        return None;
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, ppath) = rest.split_at(end);
    let (puser, hostport) = match authority.rfind('@') {
        Some(i) => {
            let ui = &authority[..i];
            (
                Some(percent_decode(ui.split(':').next().unwrap_or(""))),
                &authority[i + 1..],
            )
        }
        None => (None, authority),
    };
    let (phost, pport) = if let Some(v6) = hostport.strip_prefix('[') {
        let close = v6.find(']')?;
        let port = v6[close + 1..].strip_prefix(':').filter(|p| !p.is_empty());
        (
            v6[..close].to_string(),
            port.map(|p| p.parse::<u16>()).transpose().ok()?,
        )
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) if !p.is_empty() => (h.to_string(), Some(p.parse::<u16>().ok()?)),
            Some((h, _)) => (h.to_string(), None),
            None => (hostport.to_string(), None),
        }
    };
    if !host_matches(&phost, &url.host) {
        return None;
    }
    if pport.unwrap_or(url.scheme.default_port()) != url.port_or_default() {
        return None;
    }
    let ppath = percent_decode(ppath.split(['?', '#']).next().unwrap_or(""));
    let ppath = ppath.trim_end_matches('/');
    let upath = percent_decode(url.path.split('?').next().unwrap_or(""));
    let upath = upath.trim_end_matches('/');
    let path_len = if ppath.is_empty() {
        0
    } else if upath == ppath || upath.starts_with(&format!("{ppath}/")) {
        ppath.len()
    } else {
        return None;
    };
    let user = match puser {
        Some(u) => {
            if url.user.as_deref() != Some(u.as_str()) {
                return None;
            }
            true
        }
        None => false,
    };
    Some(Rank {
        url: true,
        path_len,
        user,
    })
}

fn host_matches(pattern: &str, host: &str) -> bool {
    let p: Vec<&str> = pattern.split('.').collect();
    let h: Vec<&str> = host.split('.').collect();
    p.len() == h.len()
        && p.iter()
            .zip(&h)
            .all(|(p, h)| label_glob(&p.to_ascii_lowercase(), &h.to_ascii_lowercase()))
}

fn label_glob(p: &str, h: &str) -> bool {
    match p.find('*') {
        None => p == h,
        Some(i) => {
            let (pre, post) = (&p[..i], &p[i + 1..]);
            if !h.starts_with(pre) {
                return false;
            }
            let rest = &h[pre.len()..];
            (0..=rest.len()).any(|k| rest.is_char_boundary(k) && label_glob(post, &rest[k..]))
        }
    }
}

/// Build the `Accept-Language` value from the locale variables.
pub fn accept_language(env: &dyn Environment) -> Option<String> {
    let raw = ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|n| env.var(n))
        .map(|v| v.to_string_lossy().into_owned())
        .find(|v| !v.is_empty())?;
    let langs: Vec<String> = raw
        .split(':')
        .filter_map(|l| {
            let l = l.split(['.', '@']).next().unwrap_or("");
            if l.is_empty() || l == "C" || l == "POSIX" {
                None
            } else {
                Some(l.replace('_', "-"))
            }
        })
        .collect();
    if langs.is_empty() {
        return None;
    }
    let total = langs.len() + 1;
    let decimals = total.to_string().len() as u32;
    let scale = 10u32.pow(decimals);
    let mut out = String::new();
    for (i, l) in langs
        .iter()
        .chain(std::iter::once(&"*".to_string()))
        .enumerate()
    {
        if i == 0 {
            out.push_str(l);
            continue;
        }
        let q = scale - i as u32;
        out.push_str(&format!(", {l};q=0.{q:0width$}", width = decimals as usize));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgit_config::{MapEnv, Origin, Scope, parse_bytes};

    fn cfg(text: &str) -> ConfigSet {
        parse_bytes(text.as_bytes(), Origin::CommandLine, Scope::Command).unwrap()
    }

    #[test]
    fn language() {
        let env = MapEnv::new([("LANGUAGE", "ko_KR.UTF-8:de_DE:fr")]);
        assert_eq!(
            accept_language(&env).unwrap(),
            "ko-KR, de-DE;q=0.9, fr;q=0.8, *;q=0.7"
        );
        let env = MapEnv::new([("LANGUAGE", ""), ("LC_ALL", "C")]);
        assert_eq!(accept_language(&env), None);
        let env = MapEnv::new([("LANG", "de_DE.UTF-8")]);
        assert_eq!(accept_language(&env).unwrap(), "de-DE, *;q=0.9");
        let env = MapEnv::new([("LANGUAGE", "a:b:c:d:e:f:g:h:i:j:k:l")]);
        assert_eq!(
            accept_language(&env).unwrap(),
            "a, b;q=0.99, c;q=0.98, d;q=0.97, e;q=0.96, f;q=0.95, g;q=0.94, h;q=0.93, i;q=0.92, j;q=0.91, k;q=0.90, l;q=0.89, *;q=0.88"
        );
    }

    #[test]
    fn redirect_protocols() {
        let url = Url::parse("http://h/r").unwrap();
        let none = MapEnv::new::<&str, &str>([]);
        let o = HttpOptions::load(&cfg(""), &url, &none).unwrap();
        assert_eq!(o.redirect_schemes, ["http", "https"]);
        let c = cfg("[protocol \"http\"]\n\tallow = user\n");
        let o = HttpOptions::load(&c, &url, &none).unwrap();
        assert_eq!(o.redirect_schemes, ["https"]);
        let env = MapEnv::new([("GIT_ALLOW_PROTOCOL", "http")]);
        let o = HttpOptions::load(&cfg(""), &url, &env).unwrap();
        assert_eq!(o.redirect_schemes, ["http"]);
    }

    #[test]
    fn url_matching_precedence() {
        let c = cfg("[http]\n\tsslVerify = false\n\
             [http \"https://example.com\"]\n\tsslVerify = true\n\tuserAgent = host\n\
             [http \"https://example.com/foo\"]\n\tuserAgent = path\n\
             [http \"https://user@example.com\"]\n\tuserAgent = user\n\
             [http \"https://*.example.com\"]\n\tpostBuffer = 10\n\
             [http \"https://other.com\"]\n\tpostBuffer = 20\n");
        let env = MapEnv::new::<&str, &str>([]);
        let url = Url::parse("https://user@example.com/foo/bar").unwrap();
        let o = HttpOptions::load(&c, &url, &env).unwrap();
        assert!(o.ssl_verify);
        assert_eq!(o.user_agent, "path");
        assert_eq!(o.post_buffer, DEFAULT_POST_BUFFER);
        let url = Url::parse("https://user@example.com/x").unwrap();
        assert_eq!(
            HttpOptions::load(&c, &url, &env).unwrap().user_agent,
            "user"
        );
        let url = Url::parse("https://a.example.com/x").unwrap();
        let o = HttpOptions::load(&c, &url, &env).unwrap();
        assert_eq!(o.post_buffer, 10);
        assert!(!o.ssl_verify);
        let url = Url::parse("https://example.com:443/foobar").unwrap();
        assert_eq!(
            HttpOptions::load(&c, &url, &env).unwrap().user_agent,
            "host"
        );
        let url = Url::parse("http://example.com/foo").unwrap();
        assert!(!HttpOptions::load(&c, &url, &env).unwrap().ssl_verify);
    }

    #[test]
    fn extra_headers_and_env() {
        let c = cfg(
            "[http]\n\textraHeader = A: 1\n\textraHeader =\n\textraHeader = B: 2\n\
             \tfollowRedirects = true\n\tlowSpeedLimit = 1k\n",
        );
        let env = MapEnv::new([
            ("GIT_SSL_NO_VERIFY", "1"),
            ("GIT_HTTP_USER_AGENT", "ua/1"),
            ("GIT_HTTP_LOW_SPEED_TIME", "3"),
        ]);
        let url = Url::parse("https://example.com/r").unwrap();
        let o = HttpOptions::load(&c, &url, &env).unwrap();
        assert_eq!(o.extra_headers, vec!["B: 2".to_string()]);
        assert_eq!(o.follow_redirects, FollowRedirects::Always);
        assert_eq!(o.low_speed_limit, 1024);
        assert_eq!(o.low_speed_time, 3);
        assert!(!o.ssl_verify);
        assert_eq!(o.user_agent, "ua/1");
    }

    #[test]
    fn proxies() {
        let env = MapEnv::new([
            ("http_proxy", "http://u:p%40@proxy:3128"),
            ("HTTPS_PROXY", "proxy2"),
            ("no_proxy", "internal.example, .corp"),
        ]);
        let mut o = HttpOptions::default();
        o.apply_env(&env);
        let p = o
            .proxy_for(&Url::parse("http://x/r").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(
            p,
            Proxy {
                kind: ProxyKind::Http,
                host: "proxy".into(),
                port: 3128,
                user: Some("u".into()),
                password: Some("p@".into()),
                socket: None,
            }
        );
        let p = o
            .proxy_for(&Url::parse("https://x/r").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!((p.host.as_str(), p.port), ("proxy2", 1080));
        assert!(
            o.proxy_for(&Url::parse("http://a.corp/r").unwrap())
                .unwrap()
                .is_none()
        );
        assert!(
            o.proxy_for(&Url::parse("http://internal.example/r").unwrap())
                .unwrap()
                .is_none()
        );
        o.proxy = Some(String::new());
        assert!(
            o.proxy_for(&Url::parse("http://x/r").unwrap())
                .unwrap()
                .is_none()
        );
        o.proxy = Some("socks5://h".into());
        let p = o
            .proxy_for(&Url::parse("http://x/r").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!((p.kind, p.port), (ProxyKind::Socks5, 1080));
        let err = |spec: &str| parse_proxy(spec).unwrap_err().to_string();
        assert_eq!(
            err("htpp://127.0.0.1"),
            "Invalid proxy URL 'htpp://127.0.0.1': unsupported proxy scheme 'htpp'"
        );
        assert_eq!(
            err("HTTP://127.0.0.1"),
            "Invalid proxy URL 'HTTP://127.0.0.1': unsupported proxy scheme 'HTTP'"
        );
        assert_eq!(
            err("localhost/path"),
            "Invalid proxy URL 'localhost/path': only SOCKS proxies support paths"
        );
        assert_eq!(
            err("socks4://127.0.0.1/path"),
            "Invalid proxy URL 'socks4://127.0.0.1/path': host must be localhost if a path is present"
        );
        let p = parse_proxy("socks5h://localhost/tmp/%30.sock").unwrap();
        assert_eq!(p.socket.as_deref(), Some("/tmp/0.sock"));
    }

    #[test]
    fn retry_and_tls_settings() {
        let c = cfg(
            "[http]\n\tmaxRetries = 3\n\tretryAfter = 2\n\tsslVersion = tlsv1.3\n\
             \tsslVersion = bogus\n\tcurloptResolve = a:1:127.0.0.1\n\tcurloptResolve =\n\
             \tcurloptResolve = b:2:127.0.0.1\n\tproactiveAuth = basic\n\temptyAuth = true\n",
        );
        let env = MapEnv::new([
            ("GIT_HTTP_RETRY_AFTER", "1"),
            ("GIT_HTTP_MAX_RETRY_TIME", "9"),
        ]);
        let url = Url::parse("https://example.com/r").unwrap();
        let o = HttpOptions::load(&c, &url, &env).unwrap();
        assert_eq!((o.max_retries, o.retry_after, o.max_retry_time), (3, 1, 9));
        assert_eq!(o.ssl_version, SslVersion::Default);
        assert_eq!(
            o.warnings,
            vec!["unsupported ssl version bogus: using default".to_string()]
        );
        assert_eq!(o.resolve, vec!["b:2:127.0.0.1".to_string()]);
        assert_eq!(o.proactive_auth, ProactiveAuth::Basic);
        assert_eq!(o.empty_auth, EmptyAuth::Always);
        let c = cfg("[http]\n\tmaxRequests = x\n");
        assert!(HttpOptions::load(&c, &url, &env).is_err());
    }
}
