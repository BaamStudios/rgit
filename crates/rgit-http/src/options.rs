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
    /// Host name.
    pub host: String,
    /// Port (libcurl's default is 1080).
    pub port: u16,
    /// User name for Basic proxy authentication.
    pub user: Option<String>,
    /// Password for Basic proxy authentication.
    pub password: Option<String>,
}

impl HttpOptions {
    /// Settings for `url` from the configuration and the environment.
    pub fn load(cfg: &ConfigSet, url: &Url, env: &dyn Environment) -> Result<HttpOptions, Error> {
        let mut o = HttpOptions::default();
        o.apply_config(cfg, url, env)?;
        o.apply_env(env);
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
            let cur = best.get(&key).copied().unwrap_or(Rank::NONE);
            if rank >= cur {
                best.insert(key.clone(), rank);
                winners.retain(|(k, _)| *k != key);
                winners.push((key, e));
            }
        }
        self.extra_headers.extend(headers);
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
                        HttpVersion::Default
                    };
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

    /// The proxy to use for `url`, if any.
    pub fn proxy_for(&self, url: &Url) -> Result<Option<Proxy>, Error> {
        if let Some(np) = &self.env_proxy.no_proxy {
            if no_proxy_matches(np, &url.host) {
                return Ok(None);
            }
        }
        let spec = match &self.proxy {
            Some(p) if p.is_empty() => return Ok(None),
            Some(p) => Some(p.clone()),
            None => match url.scheme {
                Scheme::Http => self.env_proxy.http.clone(),
                Scheme::Https => self.env_proxy.https.clone(),
            }
            .or_else(|| self.env_proxy.all.clone()),
        };
        spec.map(|s| parse_proxy(&s)).transpose()
    }
}

/// Parse `[protocol://][user[:password]@]host[:port][/path]`.
pub fn parse_proxy(spec: &str) -> Result<Proxy, Error> {
    let unsupported = || Error::Access {
        url: spec.to_string(),
        status: None,
        reason: crate::messages::unsupported_proxy(spec),
        remote: Vec::new(),
    };
    let rest = match spec.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("http") => rest,
        Some(_) => return Err(unsupported()),
        None => spec,
    };
    let authority = rest.split('/').next().unwrap_or("");
    let (userinfo, hostport) = match authority.rfind('@') {
        Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
        None => (None, authority),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(']') || p.parse::<u16>().is_ok() => (
            h.trim_matches(['[', ']']).to_string(),
            p.parse().map_err(|_| unsupported())?,
        ),
        _ => (hostport.trim_matches(['[', ']']).to_string(), 1080),
    };
    if host.is_empty() {
        return Err(unsupported());
    }
    let (user, password) = match userinfo {
        Some(ui) => match ui.split_once(':') {
            Some((u, p)) => (Some(percent_decode(u)), Some(percent_decode(p))),
            None => (Some(percent_decode(ui)), None),
        },
        None => (None, None),
    };
    Ok(Proxy {
        host,
        port,
        user,
        password,
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
                host: "proxy".into(),
                port: 3128,
                user: Some("u".into()),
                password: Some("p@".into())
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
        assert!(o.proxy_for(&Url::parse("http://x/r").unwrap()).is_err());
    }
}
