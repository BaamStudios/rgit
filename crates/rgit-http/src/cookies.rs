// SPDX-License-Identifier: MIT
//! `http.cookieFile` and `http.saveCookies`: a small cookie jar.
//!
//! Behaviour follows `Documentation/config/http.adoc`, RFC 6265 and the
//! cookie file formats described in `curl(1)`:
//!
//! - the file holds Netscape/Mozilla lines (`domain`, include-subdomains
//!   flag, `path`, secure flag, expiry, `name`, `value`, separated by tabs;
//!   `#HttpOnly_` before the domain marks HTTP-only cookies) or plain
//!   `Set-Cookie:` header lines; a header line without `Domain` applies to
//!   every host;
//! - a `Set-Cookie` response header without `Domain` is a host-only cookie
//!   of the request host; without `Path` its path is the directory of the
//!   request path (everything before the last `/`);
//! - with `http.saveCookies` the jar is written back in the Netscape format
//!   after every response that set a cookie (session cookies included,
//!   expiry `0`).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// One cookie.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cookie {
    /// Domain without a leading dot; empty matches every host.
    domain: String,
    /// Subdomains match too.
    subdomains: bool,
    path: String,
    secure: bool,
    http_only: bool,
    /// Expiry as seconds since the epoch; 0 for a session cookie.
    expires: u64,
    name: String,
    value: String,
}

impl Cookie {
    fn matches(&self, host: &str, path: &str, https: bool, now: u64) -> bool {
        if self.secure && !https {
            return false;
        }
        if self.expires != 0 && self.expires <= now {
            return false;
        }
        let host = host.to_ascii_lowercase();
        let domain = self.domain.to_ascii_lowercase();
        let domain_ok = domain.is_empty()
            || host == domain
            || (self.subdomains && host.ends_with(&format!(".{domain}")));
        domain_ok && path_matches(&self.path, path)
    }
}

/// RFC 6265, section 5.1.4.
fn path_matches(cookie_path: &str, request_path: &str) -> bool {
    let cp = if cookie_path.is_empty() {
        "/"
    } else {
        cookie_path
    };
    if request_path == cp {
        return true;
    }
    request_path.starts_with(cp)
        && (cp.ends_with('/') || request_path.as_bytes().get(cp.len()) == Some(&b'/'))
}

/// RFC 6265, section 5.1.4: the default path of a cookie set by a
/// response to `request_path`.
fn default_path(request_path: &str) -> String {
    let p = request_path.split('?').next().unwrap_or("");
    if !p.starts_with('/') {
        return "/".into();
    }
    match p.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => p[..i].to_string(),
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The cookies of one session.
#[derive(Debug, Default)]
pub(crate) struct CookieJar {
    cookies: Vec<Cookie>,
    save_to: Option<PathBuf>,
}

impl CookieJar {
    /// Load `file` (missing or unreadable files give an empty jar); with
    /// `save`, cookies are written back to it.
    pub(crate) fn load(file: &Path, save: bool) -> CookieJar {
        let mut jar = CookieJar::default();
        if file.as_os_str().is_empty() {
            return jar;
        }
        if let Ok(data) = std::fs::read(file) {
            for line in String::from_utf8_lossy(&data).lines() {
                jar.read_line(line);
            }
        }
        if save {
            jar.save_to = Some(file.to_path_buf());
        }
        jar
    }

    fn read_line(&mut self, line: &str) {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = strip_prefix_ci(line, "set-cookie:") {
            if let Some(c) = parse_set_cookie(rest, "", "/") {
                self.insert(c);
            }
            return;
        }
        let (line, http_only) = match line.strip_prefix("#HttpOnly_") {
            Some(l) => (l, true),
            None => (line, false),
        };
        if line.starts_with('#') || line.trim().is_empty() {
            return;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 7 {
            return;
        }
        let domain = f[0].trim_start_matches('.').to_string();
        self.insert(Cookie {
            domain,
            subdomains: f[1].eq_ignore_ascii_case("TRUE"),
            path: f[2].to_string(),
            secure: f[3].eq_ignore_ascii_case("TRUE"),
            http_only,
            expires: f[4].parse().unwrap_or(0),
            name: f[5].to_string(),
            value: f[6..].join("\t"),
        });
    }

    fn insert(&mut self, c: Cookie) {
        self.cookies.retain(|o| {
            !(o.name == c.name && o.path == c.path && o.domain.eq_ignore_ascii_case(&c.domain))
        });
        self.cookies.push(c);
    }

    /// The `Cookie` header value for a request, if any cookie matches.
    pub(crate) fn header(&self, host: &str, path: &str, https: bool) -> Option<String> {
        let path = path.split('?').next().unwrap_or("");
        let now = now();
        let mut found: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|c| c.matches(host, path, https, now))
            .collect();
        if found.is_empty() {
            return None;
        }
        // Longer paths first (RFC 6265, section 5.4).
        found.sort_by_key(|c| std::cmp::Reverse(c.path.len()));
        Some(
            found
                .iter()
                .map(|c| format!("{}={}", c.name, c.value))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }

    /// Take the `Set-Cookie` headers of a response to `host` / `path`.
    pub(crate) fn store(&mut self, host: &str, path: &str, headers: &[(String, String)]) {
        let mut changed = false;
        let default = default_path(path);
        for (k, v) in headers {
            if !k.eq_ignore_ascii_case("set-cookie") {
                continue;
            }
            if let Some(mut c) = parse_set_cookie(v, host, &default) {
                if c.domain.is_empty() {
                    c.domain = host.to_string();
                }
                self.insert(c);
                changed = true;
            }
        }
        if changed {
            self.save();
        }
    }

    fn save(&self) {
        let Some(path) = &self.save_to else {
            return;
        };
        let mut out = String::from(
            "# Netscape HTTP Cookie File\n\
             # https://curl.se/docs/http-cookies.html\n\
             # This file was generated by rgit. Edit at your own risk.\n\n",
        );
        for c in &self.cookies {
            let domain = if c.subdomains {
                format!(".{}", c.domain)
            } else {
                c.domain.clone()
            };
            out.push_str(&format!(
                "{}{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                if c.http_only { "#HttpOnly_" } else { "" },
                domain,
                if c.subdomains { "TRUE" } else { "FALSE" },
                c.path,
                if c.secure { "TRUE" } else { "FALSE" },
                c.expires,
                c.name,
                c.value
            ));
        }
        let _ = std::fs::write(path, out);
    }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    (s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix))
        .then(|| &s[prefix.len()..])
}

/// Parse a `Set-Cookie` value. `host` is the request host ("" for cookie
/// file lines), `default_path` the path to use without a `Path` attribute.
fn parse_set_cookie(value: &str, host: &str, default_path: &str) -> Option<Cookie> {
    let mut parts = value.split(';');
    let (name, val) = parts.next()?.trim().split_once('=')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let mut c = Cookie {
        domain: String::new(),
        subdomains: false,
        path: default_path.to_string(),
        secure: false,
        http_only: false,
        expires: 0,
        name: name.to_string(),
        value: val.trim().to_string(),
    };
    let mut max_age: Option<i64> = None;
    for attr in parts {
        let (k, v) = match attr.split_once('=') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => (attr.trim(), ""),
        };
        if k.eq_ignore_ascii_case("domain") && !v.is_empty() {
            let d = v.trim_start_matches('.').to_ascii_lowercase();
            // A response may only set cookies for its own domain.
            if !host.is_empty() {
                let h = host.to_ascii_lowercase();
                if h != d && !h.ends_with(&format!(".{d}")) {
                    return None;
                }
            }
            c.domain = d;
            c.subdomains = true;
        } else if k.eq_ignore_ascii_case("path") && v.starts_with('/') {
            c.path = v.to_string();
        } else if k.eq_ignore_ascii_case("secure") {
            c.secure = true;
        } else if k.eq_ignore_ascii_case("httponly") {
            c.http_only = true;
        } else if k.eq_ignore_ascii_case("max-age") {
            max_age = v.parse().ok();
        } else if k.eq_ignore_ascii_case("expires") {
            if let Some(t) = http_date(v) {
                c.expires = t.max(1);
            }
        }
    }
    if let Some(age) = max_age {
        c.expires = if age <= 0 { 1 } else { now() + age as u64 };
    }
    Some(c)
}

/// An HTTP date (`Sun, 06 Nov 1994 08:49:37 GMT`, also with `-` between
/// the date fields) as seconds since the epoch.
pub(crate) fn http_date(s: &str) -> Option<u64> {
    let s = s.trim();
    let s = s.split_once(',').map_or(s, |(_, r)| r).trim();
    // `06-Nov-94 08:49:37 GMT` (RFC 850) has the date in one word.
    let mut words: Vec<&str> = Vec::new();
    for (i, w) in s.split_whitespace().enumerate() {
        if i == 0 && w.contains('-') {
            words.extend(w.split('-'));
        } else {
            words.push(w);
        }
    }
    let mut it = words.into_iter();
    let day: u64 = it.next()?.parse().ok()?;
    let month = match it.next()?.to_ascii_lowercase().as_str() {
        "jan" => 1,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        "dec" => 12,
        _ => return None,
    };
    let mut year: i64 = it.next()?.parse().ok()?;
    if year < 100 {
        year += if year < 70 { 2000 } else { 1900 };
    }
    let mut hms = it.next()?.split(':');
    let h: u64 = hms.next()?.parse().ok()?;
    let m: u64 = hms.next()?.parse().ok()?;
    let sec: u64 = hms.next()?.parse().ok()?;
    let offset: i64 = match it.next() {
        None => 0,
        Some(z) if z.eq_ignore_ascii_case("GMT") || z.eq_ignore_ascii_case("UTC") => 0,
        Some(z) => {
            let (sign, digits) = match z.as_bytes().first()? {
                b'+' => (1, &z[1..]),
                b'-' => (-1, &z[1..]),
                _ => return None,
            };
            if digits.len() != 4 {
                return None;
            }
            let hh: i64 = digits[..2].parse().ok()?;
            let mm: i64 = digits[2..].parse().ok()?;
            sign * (hh * 3600 + mm * 60)
        }
    };
    let days = days_from_civil(year, month, day as i64);
    let t = days * 86400 + (h * 3600 + m * 60 + sec) as i64 - offset;
    u64::try_from(t).ok()
}

/// Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant's
/// `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn netscape_and_header_lines() {
        let mut jar = CookieJar::default();
        jar.read_line("127.0.0.1\tFALSE\t/smart_cookies\tFALSE\t0\tothername\tothervalue");
        jar.read_line("Set-Cookie: Foo=1");
        jar.read_line("# comment");
        assert_eq!(
            jar.header("127.0.0.1", "/smart_cookies/repo.git/info/refs?x=y", false)
                .unwrap(),
            "othername=othervalue; Foo=1"
        );
        assert_eq!(jar.header("example.com", "/x", false).unwrap(), "Foo=1");
        assert!(jar.header("127.0.0.1", "/smart_cookiesX", false).is_some());
    }

    #[test]
    fn store_default_path() {
        let mut jar = CookieJar::default();
        let h = vec![("Set-Cookie".to_string(), "name=value".to_string())];
        jar.store(
            "127.0.0.1",
            "/smart_cookies/repo.git/info/refs?service=x",
            &h,
        );
        jar.store("127.0.0.1", "/smart_cookies/repo.git/git-upload-pack", &h);
        let paths: Vec<&str> = jar.cookies.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(
            paths,
            ["/smart_cookies/repo.git/info", "/smart_cookies/repo.git"]
        );
        assert!(
            jar.header("other", "/smart_cookies/repo.git", false)
                .is_none()
        );
        let h = vec![(
            "set-cookie".to_string(),
            "a=b; Domain=evil.example".to_string(),
        )];
        jar.store("127.0.0.1", "/", &h);
        assert_eq!(jar.cookies.len(), 2);
    }

    #[test]
    fn http_dates() {
        assert_eq!(http_date("Sun, 06 Nov 1994 08:49:37 GMT"), Some(784111777));
        assert_eq!(
            http_date("Sun, 06 Nov 1994 09:49:37 +0100"),
            Some(784111777)
        );
        assert_eq!(http_date("Sunday, 06-Nov-94 08:49:37 GMT"), Some(784111777));
        assert_eq!(http_date("invalid-format-123abc"), None);
    }
}
