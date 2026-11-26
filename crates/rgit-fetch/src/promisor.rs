// SPDX-License-Identifier: MIT
//! The client side of the `promisor-remote` capability
//! (gitprotocol-v2.adoc, config/promisor.adoc): which of the promisor
//! remotes a server advertises are accepted, the reply naming them, and
//! what the configuration learns from the advertisement.
//!
//! An advertised remote is accepted when
//!
//! - its URL matches a `promisor.acceptFromServerUrl` pattern (`[<name>=]
//!   <url-glob>`), unless a local remote of the advertised name has a
//!   different URL. A local remote of the advertised name with that URL
//!   is used as it is; otherwise the remote named by the pattern (else
//!   `promisor-auto-<url as name>`, `-1`, `-2`... while such a remote has
//!   another URL) is created or reused, with `promisor` and `advertisedAs`
//!   set;
//! - else `promisor.acceptFromServer` allows it: `all`, `knownName` (a
//!   local remote of that name has a URL), `knownUrl` (and it is the
//!   advertised one), never `none` (the default). Then every field of
//!   `promisor.checkFields` must be set locally, advertised and equal.
//!
//! For an accepted remote that exists locally the fields of
//! `promisor.storeFields` that the server sent with a new value are
//! stored, with a note on standard error.
//!
//! Patterns and URLs are compared after normalization: scheme and host
//! lowercased, user information dropped, `%xx` decoded, `.` and `..`
//! resolved. A `*` matches any text but never crosses from the host into
//! the path.
//!
//! The remotes accepted by this process are tried first when objects are
//! fetched lazily ([`accepted_remotes`]).

use std::path::Path;
use std::sync::Mutex;

use bstr::ByteSlice;
use rgit_config::ConfigSet;

use crate::messages;

/// One remote of a `promisor-remote` advertisement.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Advertised {
    pub name: String,
    pub url: String,
    pub partial_clone_filter: Option<String>,
    pub token: Option<String>,
}

/// The outcome of [`accept`].
#[derive(Debug, Clone, Default)]
pub struct Accepted {
    /// The accepted remotes as advertised, with the local remote name.
    pub remotes: Vec<(Advertised, String)>,
    /// The configuration to write (`key`, `value`), in this order: see
    /// [`Accepted::write`].
    pub config: Vec<(String, String)>,
}

impl Accepted {
    /// Write [`Accepted::config`] to the configuration file `path`.
    pub fn write(&self, path: &Path) {
        for (key, value) in &self.config {
            let req = rgit_config::edit::SetRequest {
                key: key.as_bytes(),
                value: Some(value.as_bytes()),
                pattern: &rgit_config::edit::ValuePattern::Any,
                all: false,
                comment: None,
            };
            let _ = rgit_config::write::set_in_file(path, &rgit_fs::LockOptions::default(), &req);
        }
    }

    /// The value of the `promisor-remote` reply, if anything was accepted.
    pub fn reply(&self) -> Option<String> {
        if self.remotes.is_empty() {
            return None;
        }
        Some(
            self.remotes
                .iter()
                .map(|(a, _)| encode(&a.name))
                .collect::<Vec<_>>()
                .join(";"),
        )
    }

    /// The filter for `--filter=auto`: the `partialCloneFilter` fields of
    /// the accepted remotes, combined when there are several.
    pub fn auto_filter(&self) -> Option<String> {
        let mut specs: Vec<String> = Vec::new();
        for (a, _) in &self.remotes {
            if let Some(f) = &a.partial_clone_filter
                && !specs.contains(f)
            {
                specs.push(f.clone());
            }
        }
        match specs.len() {
            0 => None,
            1 => specs.pop(),
            _ => {
                let parsed: Vec<rgit_pack::write::ObjectFilter> = specs
                    .iter()
                    .filter_map(|s| rgit_pack::write::ObjectFilter::parse(s).ok())
                    .collect();
                Some(rgit_pack::write::ObjectFilter::Combine(parsed).to_spec())
            }
        }
    }
}

static ACCEPTED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The local names of the promisor remotes this process accepted from a
/// server, in the order they were advertised.
pub fn accepted_remotes() -> Vec<String> {
    ACCEPTED.lock().map(|v| v.clone()).unwrap_or_default()
}

fn remember(names: impl IntoIterator<Item = String>) {
    if let Ok(mut v) = ACCEPTED.lock() {
        for n in names {
            if !v.contains(&n) {
                v.push(n);
            }
        }
    }
}

/// `%xx` for the bytes that cannot appear in a name or field value.
pub fn encode(s: &str) -> String {
    let mut out: Vec<u8> = Vec::with_capacity(s.len());
    for b in s.bytes() {
        if b <= b' ' || b == 0x7f || matches!(b, b'%' | b',' | b';') {
            out.extend_from_slice(format!("%{b:02x}").as_bytes());
        } else {
            out.push(b);
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Decode `%xx` sequences.
pub fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|d| d as u8)
}

/// Parse the value of the capability.
pub fn parse_advertisement(value: &str) -> Vec<Advertised> {
    let mut out = Vec::new();
    for part in value.split(';').filter(|p| !p.is_empty()) {
        let mut a = Advertised::default();
        for field in part.split(',') {
            let Some((k, v)) = field.split_once('=') else {
                continue;
            };
            let v = decode(v);
            match k {
                "name" => a.name = v,
                "url" => a.url = v,
                "partialCloneFilter" => a.partial_clone_filter = Some(v),
                "token" => a.token = Some(v),
                _ => {}
            }
        }
        if !a.name.is_empty() && !a.url.is_empty() {
            out.push(a);
        }
    }
    out
}

/// `promisor.acceptFromServer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Policy {
    None,
    KnownUrl,
    KnownName,
    All,
}

/// One `promisor.acceptFromServerUrl` pattern.
struct Pattern {
    name: Option<String>,
    url: Url,
}

/// A URL split for matching.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Url {
    scheme: String,
    host: String,
    port: Option<String>,
    path: String,
}

/// Split and normalize `url`; `None` when it has no `<scheme>://`.
fn parse_url(url: &str) -> Option<Url> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme.is_empty()
        || !scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b))
    {
        return None;
    }
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let hostport = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit() || b == b'*') => {
            (h, Some(p.to_owned()))
        }
        _ => (hostport, None),
    };
    Some(Url {
        scheme: scheme.to_ascii_lowercase(),
        host: decode(host).to_ascii_lowercase(),
        port,
        path: normalize_path(&decode(path)),
    })
}

/// Resolve `.` and `..` segments.
fn normalize_path(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    let mut parts: Vec<&str> = Vec::new();
    let segments: Vec<&str> = path.split('/').collect();
    for (i, seg) in segments.iter().enumerate() {
        match *seg {
            "." => {}
            ".." => {
                if parts.len() > 1 {
                    parts.pop();
                }
            }
            "" if i > 0 && i + 1 < segments.len() => {}
            s => parts.push(s),
        }
    }
    let mut out = parts.join("/");
    if !out.starts_with('/') {
        out.insert(0, '/');
    }
    out
}

/// `*` globbing.
fn glob(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, ti));
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

impl Pattern {
    fn matches(&self, url: &Url) -> bool {
        self.url.scheme == url.scheme
            && glob(&self.url.host, &url.host)
            && self.url.port == url.port
            && glob(&self.url.path, &url.path)
    }
}

/// Whether `name` can name a remote.
fn valid_remote_name(name: &str) -> bool {
    !name.is_empty()
        && rgit_refs::name::is_valid_refname(
            format!("refs/remotes/{name}/test").as_bytes(),
            rgit_refs::name::RefNameFlags::STRICT,
        )
}

/// The `promisor.acceptFromServerUrl` patterns; bad ones are reported.
fn patterns(config: &ConfigSet) -> Vec<Pattern> {
    let mut out = Vec::new();
    for e in config.get_all("promisor.acceptfromserverurl") {
        let value = e
            .value
            .as_ref()
            .map(|v| v.to_str_lossy().into_owned())
            .unwrap_or_default();
        let (name, pattern) = match value.split_once('=') {
            Some((n, p)) if !n.contains("://") => (Some(n.to_owned()), p.to_owned()),
            _ => (None, value.clone()),
        };
        if let Some(n) = &name
            && !valid_remote_name(n)
        {
            eprintln!(
                "warning: {}",
                messages::invalid_promisor_remote_name(n, &value)
            );
            continue;
        }
        let Some(url) = parse_url(&pattern) else {
            eprintln!(
                "warning: {}",
                messages::invalid_promisor_url_pattern(&pattern, &value)
            );
            continue;
        };
        out.push(Pattern { name, url });
    }
    out
}

/// A list of field names (`promisor.checkFields`, `promisor.storeFields`).
fn field_list(config: &ConfigSet, key: &str) -> Vec<String> {
    config
        .get_all(key)
        .filter_map(|e| e.value.as_ref())
        .flat_map(|v| {
            v.to_str_lossy()
                .split(',')
                .map(|f| f.trim().to_ascii_lowercase())
                .filter(|f| !f.is_empty())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn string(config: &ConfigSet, key: &str) -> Option<String> {
    config
        .get(key)
        .and_then(|e| e.value.as_ref())
        .map(|v| v.to_str_lossy().into_owned())
}

/// The URL of the local remote `name`, if it has a non-empty one.
fn local_url(config: &ConfigSet, written: &[(String, String)], name: &str) -> Option<String> {
    let key = format!("remote.{name}.url");
    written
        .iter()
        .rev()
        .find(|(k, _)| k.eq_ignore_ascii_case(&key))
        .map(|(_, v)| v.clone())
        .or_else(|| string(config, &key))
        .filter(|u| !u.is_empty())
}

/// The local name of a remote for `url`, from a pattern's name or the URL.
fn auto_name(pattern: &Pattern, url: &Url) -> String {
    if let Some(n) = &pattern.name {
        return n.clone();
    }
    let mut text = url.host.clone();
    if let Some(p) = &url.port {
        text.push(':');
        text.push_str(p);
    }
    let path = url.path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    text.push_str(path);
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    format!("promisor-auto-{}", out.trim_matches('-'))
}

/// Decide which of the remotes in `advertisement` to accept, configuring
/// them as described in the module documentation (the configuration to
/// write is returned in [`Accepted::config`]).
pub fn accept(config: &ConfigSet, advertisement: &str) -> Accepted {
    let advertised = parse_advertisement(advertisement);
    let policy = match string(config, "promisor.acceptfromserver")
        .map(|v| v.to_ascii_lowercase())
        .as_deref()
    {
        Some("all") => Policy::All,
        Some("knownname") => Policy::KnownName,
        Some("knownurl") => Policy::KnownUrl,
        _ => Policy::None,
    };
    let patterns = patterns(config);
    let check = field_list(config, "promisor.checkfields");
    let store = field_list(config, "promisor.storefields");
    // What this function wrote, for later lookups.
    let mut written: Vec<(String, String)> = Vec::new();
    let set = |written: &mut Vec<(String, String)>, key: String, value: &str| {
        written.push((key, value.to_owned()));
    };
    let mut out = Accepted::default();
    for a in advertised {
        let url = parse_url(&a.url);
        let pattern = url
            .as_ref()
            .and_then(|u| patterns.iter().find(|p| p.matches(u)));
        let mut local: Option<String> = None;
        let existing = local_url(config, &written, &a.name);
        if let (Some(p), Some(u)) = (pattern, url.as_ref()) {
            match &existing {
                Some(e) if *e == a.url => local = Some(a.name.clone()),
                Some(_) => {}
                None => {
                    let base = auto_name(p, u);
                    for n in 0..100 {
                        let name = if n == 0 {
                            base.clone()
                        } else {
                            format!("{base}-{n}")
                        };
                        match local_url(config, &written, &name) {
                            Some(e) if e == a.url => {
                                if string(config, &format!("remote.{name}.promisor")).is_none() {
                                    set(&mut written, format!("remote.{name}.promisor"), "true");
                                }
                                set(&mut written, format!("remote.{name}.advertisedAs"), &a.name);
                                local = Some(name);
                                break;
                            }
                            Some(_) => continue,
                            None => {
                                eprintln!("{}", messages::auto_creating_promisor(&name, &a.url));
                                set(&mut written, format!("remote.{name}.url"), &a.url);
                                set(&mut written, format!("remote.{name}.promisor"), "true");
                                set(&mut written, format!("remote.{name}.advertisedAs"), &a.name);
                                local = Some(name);
                                break;
                            }
                        }
                    }
                }
            }
        }
        if local.is_none() {
            let ok = match policy {
                Policy::None => false,
                Policy::All => true,
                Policy::KnownName => existing.is_some(),
                Policy::KnownUrl => existing.as_deref() == Some(a.url.as_str()),
            };
            let fields_ok = || {
                check.iter().all(|f| {
                    let (advertised, key) = match f.as_str() {
                        "partialclonefilter" => (&a.partial_clone_filter, "partialclonefilter"),
                        "token" => (&a.token, "token"),
                        _ => return true,
                    };
                    let mine = string(config, &format!("remote.{}.{key}", a.name));
                    mine.is_some() && advertised.is_some() && mine == *advertised
                })
            };
            if ok && fields_ok() {
                local = Some(a.name.clone());
            }
        }
        let Some(local) = local else {
            continue;
        };
        if local_url(config, &written, &local).is_some() {
            store_fields(config, &store, &a, &local, &mut |k, v| {
                set(&mut written, k, v)
            });
        }
        out.remotes.push((a, local));
    }
    remember(out.remotes.iter().map(|(_, l)| l.clone()));
    out.config = written;
    out
}

/// `promisor.storeFields` for the accepted remote `local`.
fn store_fields(
    config: &ConfigSet,
    store: &[String],
    a: &Advertised,
    local: &str,
    set: &mut dyn FnMut(String, &str),
) {
    for f in store {
        let (value, key, what) = match f.as_str() {
            "partialclonefilter" => (&a.partial_clone_filter, "partialCloneFilter", "filter"),
            "token" => (&a.token, "token", "token"),
            _ => continue,
        };
        let Some(value) = value else {
            continue;
        };
        let old = string(config, &format!("remote.{local}.{key}")).unwrap_or_default();
        if old == *value {
            continue;
        }
        if what == "filter" {
            if let Err(e) = rgit_pack::write::ObjectFilter::parse(value) {
                eprintln!(
                    "warning: {}",
                    messages::invalid_stored_filter(value, local, &e.to_string())
                );
                continue;
            }
        } else if value.bytes().any(|b| b.is_ascii_control()) {
            eprintln!("warning: {}", messages::invalid_stored_token(value, local));
            continue;
        }
        eprintln!("{}", messages::storing_field(what, local, &old, value));
        set(format!("remote.{local}.{key}"), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_advertisement() {
        let a = parse_advertisement(
            "name=lop,url=file:///a%20b,partialCloneFilter=blob:none;name=x,url=https://h,token=t%2c",
        );
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].url, "file:///a b");
        assert_eq!(a[0].partial_clone_filter.as_deref(), Some("blob:none"));
        assert_eq!(a[1].token.as_deref(), Some("t,"));
    }

    #[test]
    fn url_patterns() {
        let p = |s: &str| Pattern {
            name: None,
            url: parse_url(s).unwrap(),
        };
        let u = |s: &str| parse_url(s).unwrap();
        assert!(p("https://*.com/a/*").matches(&u("https://cdn.example.com/a/b")));
        assert!(!p("https://*/b").matches(&u("https://cdn.example.com/a/b")));
        assert!(p("https://cdn.example.com/z").matches(&u("https://CDN.example.com/x%41y/../z")));
        assert!(!p("https://h:8080/*").matches(&u("https://h:9999/a")));
        assert!(p("file:///t%20x/*").matches(&u("file:///t x/lop")));
        assert!(parse_url("nourl").is_none());
    }

    #[test]
    fn auto_names() {
        let p = Pattern {
            name: None,
            url: parse_url("https://*").unwrap(),
        };
        let u = parse_url("HTTPS://User:pw@CDN.Example.com:8080/a_b/c.git/").unwrap();
        assert_eq!(
            auto_name(&p, &u),
            "promisor-auto-cdn-example-com-8080-a-b-c"
        );
        let u = parse_url("file:///tmp/x-1/-home/lop").unwrap();
        assert_eq!(auto_name(&p, &u), "promisor-auto-tmp-x-1-home-lop");
    }
}
