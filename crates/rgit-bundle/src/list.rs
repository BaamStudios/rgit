// SPDX-License-Identifier: MIT
//! Bundle lists (`Documentation/technical/bundle-uri.adoc`,
//! `Documentation/config/bundle.adoc`).
//!
//! A list is a set of `bundle.*` keys: `bundle.version` (only 1),
//! `bundle.mode` (`all` or `any`), `bundle.heuristic` (`creationToken`)
//! and per bundle `bundle.<id>.uri`, `.creationToken`, `.filter`,
//! `.location`. They come from a file in git's config format (a bundle
//! URI that is not a bundle) or as `key=value` lines of a protocol v2
//! `bundle-uri` answer. Relative URIs are resolved against the location of
//! the list (see [`resolve_uri`]).

use std::path::Path;

use crate::messages;

/// `bundle.mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListMode {
    /// Every bundle is needed.
    #[default]
    All,
    /// Any one bundle is enough.
    Any,
}

/// One `bundle.<id>.*` group.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ListEntry {
    pub id: String,
    /// The resolved URI.
    pub uri: Option<String>,
    pub creation_token: Option<u64>,
    pub filter: Option<String>,
    pub location: Option<String>,
}

/// A bundle list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleList {
    pub version: u32,
    pub mode: ListMode,
    /// `bundle.heuristic`; only `creationToken` is understood.
    pub heuristic: Option<String>,
    /// The bundles in the order their first key appeared.
    pub entries: Vec<ListEntry>,
    /// What relative URIs are resolved against: the directory of the
    /// list.
    pub base: String,
}

/// Why one key could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyError {
    /// Unusable value or a repeated key: the caller reports the line.
    Bad,
    /// `fatal:` (a relative URI that leaves its base).
    Fatal(String),
}

impl BundleList {
    /// An empty list (version 1, mode `all`) for URIs relative to `base`.
    pub fn new(base: &str) -> BundleList {
        BundleList {
            version: 1,
            mode: ListMode::All,
            heuristic: None,
            entries: Vec::new(),
            base: base.to_owned(),
        }
    }

    /// Whether `bundle.heuristic=creationToken`.
    pub fn uses_creation_token(&self) -> bool {
        self.heuristic
            .as_deref()
            .is_some_and(|h| h.eq_ignore_ascii_case("creationtoken"))
    }

    fn entry(&mut self, id: &str) -> &mut ListEntry {
        if let Some(i) = self.entries.iter().position(|e| e.id == id) {
            return &mut self.entries[i];
        }
        self.entries.push(ListEntry {
            id: id.to_owned(),
            ..ListEntry::default()
        });
        self.entries.last_mut().expect("just pushed")
    }

    /// Apply one key (`bundle.<key>` or `bundle.<id>.<key>`, any case of
    /// the first and last part). Keys outside `bundle.` and unknown keys
    /// are ignored. A warning about a bad `creationToken` goes to
    /// `warn`.
    pub fn set(
        &mut self,
        key: &str,
        value: &str,
        warn: &mut dyn FnMut(String),
    ) -> Result<(), KeyError> {
        let Some(rest) = key
            .get(..7)
            .filter(|p| p.eq_ignore_ascii_case("bundle."))
            .map(|_| &key[7..])
        else {
            return Ok(());
        };
        let Some(dot) = rest.rfind('.') else {
            match rest.to_ascii_lowercase().as_str() {
                "version" => {
                    let v: u32 = value.trim().parse().map_err(|_| KeyError::Bad)?;
                    if v != 1 {
                        return Err(KeyError::Bad);
                    }
                    self.version = v;
                }
                "mode" => {
                    self.mode = match value {
                        "all" => ListMode::All,
                        "any" => ListMode::Any,
                        _ => return Err(KeyError::Bad),
                    };
                }
                "heuristic" => self.heuristic = Some(value.to_owned()),
                _ => {}
            }
            return Ok(());
        };
        let (id, sub) = (&rest[..dot], rest[dot + 1..].to_ascii_lowercase());
        match sub.as_str() {
            "uri" => {
                let resolved = resolve_uri(&self.base, value).map_err(KeyError::Fatal)?;
                let e = self.entry(id);
                if e.uri.is_some() {
                    return Err(KeyError::Bad);
                }
                e.uri = Some(resolved);
            }
            "creationtoken" => match value.trim().parse::<u64>() {
                Ok(n) => self.entry(id).creation_token = Some(n),
                Err(_) => warn(messages::bad_creation_token(value)),
            },
            "filter" => {
                let e = self.entry(id);
                if e.filter.is_some() {
                    return Err(KeyError::Bad);
                }
                e.filter = Some(value.to_owned());
            }
            "location" => {
                let e = self.entry(id);
                if e.location.is_some() {
                    return Err(KeyError::Bad);
                }
                e.location = Some(value.to_owned());
            }
            // Unknown keys still make the bundle known (it then lacks a
            // `uri`).
            _ => {
                self.entry(id);
            }
        }
        Ok(())
    }

    /// Parse one `key=value` line of a protocol v2 `bundle-uri` answer.
    /// Errors are git's `error:` lines (without prefix).
    pub fn parse_line(
        &mut self,
        line: &str,
        warn: &mut dyn FnMut(String),
    ) -> Result<(), Vec<String>> {
        let bad = |first: Option<&str>| {
            let mut v = Vec::new();
            if let Some(f) = first {
                v.push(f.to_owned());
            }
            v.push(messages::bad_line(line));
            v
        };
        if line.is_empty() {
            return Err(bad(Some(messages::EMPTY_LINE)));
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(bad(Some(messages::EMPTY_KEY_OR_VALUE)));
        };
        if key.is_empty() || value.is_empty() {
            return Err(bad(Some(messages::EMPTY_KEY_OR_VALUE)));
        }
        match self.set(key, value, warn) {
            Ok(()) => Ok(()),
            Err(KeyError::Bad) => Err(bad(None)),
            Err(KeyError::Fatal(m)) => Err(vec![format!("fatal: {m}")]),
        }
    }

    /// The ids of bundles without a URI (an error after parsing).
    pub fn missing_uris(&self) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|e| e.uri.is_none())
            .map(|e| e.id.as_str())
            .collect()
    }

    /// The list in git's config format.
    pub fn to_config_text(&self) -> String {
        let mut s = format!(
            "[bundle]\n\tversion = {}\n\tmode = {}\n",
            self.version,
            match self.mode {
                ListMode::All => "all",
                ListMode::Any => "any",
            }
        );
        if let Some(h) = &self.heuristic {
            s.push_str(&format!("\theuristic = {h}\n"));
        }
        for e in &self.entries {
            s.push_str(&format!("[bundle \"{}\"]\n", e.id));
            if let Some(u) = &e.uri {
                s.push_str(&format!("\turi = {u}\n"));
            }
            if let Some(t) = e.creation_token {
                s.push_str(&format!("\tcreationToken = {t}\n"));
            }
            if let Some(f) = &e.filter {
                s.push_str(&format!("\tfilter = {f}\n"));
            }
            if let Some(l) = &e.location {
                s.push_str(&format!("\tlocation = {l}\n"));
            }
        }
        s
    }
}

/// Why a bundle list file could not be used (the messages are printed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListError {
    /// `error:` lines were printed.
    Invalid,
    /// A `fatal:` line was printed (git dies here).
    Fatal,
}

/// Parse a bundle list file (`data`, saved at `path` for messages) whose
/// relative URIs are relative to `base`. Errors (`error:` lines) are
/// printed; warnings go to `warn`.
pub fn parse_config(
    base: &str,
    data: &[u8],
    path: &Path,
    warn: &mut dyn FnMut(String),
) -> Result<BundleList, ListError> {
    let set = rgit_config::parse_bytes(
        data,
        rgit_config::Origin::File(path.to_path_buf()),
        rgit_config::Scope::Local,
    );
    let set = match set {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return Err(ListError::Invalid);
        }
    };
    let mut list = BundleList::new(base);
    for e in set.entries() {
        let name = String::from_utf8_lossy(&e.name).into_owned();
        let value = e
            .value
            .as_ref()
            .map(|v| String::from_utf8_lossy(v).into_owned())
            .unwrap_or_default();
        // A key outside any section is a syntax error for git.
        let result = if name.contains('.') {
            list.set(&name, &value, warn)
        } else {
            Err(KeyError::Bad)
        };
        match result {
            Ok(()) => {}
            Err(KeyError::Bad) => {
                eprintln!(
                    "error: bad config line {} in file {}",
                    e.line,
                    path.display()
                );
                return Err(ListError::Invalid);
            }
            Err(KeyError::Fatal(m)) => {
                eprintln!("fatal: {m}");
                return Err(ListError::Fatal);
            }
        }
    }
    let missing = list.missing_uris();
    if !missing.is_empty() {
        for id in missing {
            eprintln!("error: {}", messages::bundle_has_no_uri(id));
        }
        return Err(ListError::Invalid);
    }
    Ok(list)
}

/// The directory part of a URI or path: everything before the last `/`
/// (the URI itself when it has none).
pub fn base_of(uri: &str) -> String {
    let trimmed = uri.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(i) => trimmed[..i].to_owned(),
        None => String::new(),
    }
}

fn has_scheme(s: &str) -> bool {
    match s.find("://") {
        Some(i) => {
            i > 0
                && s[..i]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'-' || b == b'.')
        }
        None => false,
    }
}

/// Resolve `uri` against the directory `base`: absolute URIs (with a
/// scheme, or a path starting with `/`) stay as they are; leading `./` is
/// dropped and each leading `../` removes the last component of `base`.
pub fn resolve_uri(base: &str, uri: &str) -> Result<String, String> {
    if has_scheme(uri) {
        return Ok(uri.to_owned());
    }
    if let Some(path) = uri.strip_prefix('/') {
        // Relative to the host of the base.
        if let Some(i) = base.find("://") {
            let after = &base[i + 3..];
            let host_end = after.find('/').map_or(base.len(), |j| i + 3 + j);
            return Ok(format!("{}/{path}", &base[..host_end]));
        }
        return Ok(uri.to_owned());
    }
    let mut base = base.trim_end_matches('/').to_owned();
    let mut rest = uri;
    loop {
        if let Some(r) = rest.strip_prefix("./") {
            rest = r;
        } else if let Some(r) = rest.strip_prefix("../") {
            if base.is_empty() {
                return Err(messages::cannot_strip_component(&base));
            }
            match base.rfind('/') {
                Some(i) if !base[..i].ends_with('/') => base.truncate(i),
                Some(_) => return Err(messages::cannot_strip_component(&base)),
                None => base.clear(),
            }
            rest = r;
        } else {
            break;
        }
    }
    if base.is_empty() {
        Ok(rest.to_owned())
    } else {
        Ok(format!("{base}/{rest}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative() {
        assert_eq!(
            resolve_uri("<uri>", "bundle.bdl").unwrap(),
            "<uri>/bundle.bdl"
        );
        assert_eq!(resolve_uri("<uri>", "../bundle.bdl").unwrap(), "bundle.bdl");
        assert_eq!(
            resolve_uri("<uri>", "sub/dir/bundle.bdl").unwrap(),
            "<uri>/sub/dir/bundle.bdl"
        );
        assert!(resolve_uri("<uri>", "../../bundle.bdl").is_err());
        assert_eq!(
            resolve_uri("http://h:1/a", "b.bundle").unwrap(),
            "http://h:1/a/b.bundle"
        );
        assert_eq!(resolve_uri("http://h:1/a/b", "/c").unwrap(), "http://h:1/c");
        assert_eq!(base_of("http://h/x/bundle-list"), "http://h/x");
    }

    #[test]
    fn lines() {
        let mut l = BundleList::new("<uri>");
        let mut w = |_| {};
        l.parse_line("bundle.one.uri=http://example.com/a", &mut w)
            .unwrap();
        assert!(
            l.parse_line("bundle.one.uri=http://example.com/b", &mut w)
                .is_err()
        );
        assert_eq!(
            l.parse_line("=x", &mut w).unwrap_err(),
            vec![
                "bundle-uri: line has empty key or value".to_owned(),
                "bad line: '=x'".to_owned()
            ]
        );
        assert_eq!(l.entries.len(), 1);
    }
}
