// SPDX-License-Identifier: MIT
//! Object filter specs (`--filter=<spec>`, `Documentation/rev-list-options.adoc`)
//! as far as bundles need them: parsed, checked, and written back the way
//! git writes them into the `@filter` capability (observed from git 2.56:
//! a single `blob:limit` is written with the limit in bytes, other specs
//! as given; several `--filter` options become `combine:<a>+<b>`).

use crate::messages;

/// One filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    /// `blob:none`.
    BlobNone,
    /// `blob:limit=<n>`: blobs of at least `n` bytes are left out.
    BlobLimit(u64),
    /// `tree:<depth>`: trees and blobs at depth `depth` or deeper are
    /// left out (the root tree has depth 0).
    Tree(u64),
    /// `sparse:oid=<blob-ish>`.
    SparseOid(String),
    /// `object:type=<type>`.
    ObjectType(String),
    /// `combine:<a>+<b>...`: all of them.
    Combine(Vec<Filter>),
}

/// A filter with the text it is written as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterSpec {
    text: String,
    pub filter: Filter,
}

fn parse_size(s: &str) -> Option<u64> {
    let (digits, factor) = match s.as_bytes().last()? {
        b'k' | b'K' => (&s[..s.len() - 1], 1u64 << 10),
        b'm' | b'M' => (&s[..s.len() - 1], 1 << 20),
        b'g' | b'G' => (&s[..s.len() - 1], 1 << 30),
        _ => (s, 1),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u64>().ok()?.checked_mul(factor)
}

fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c == '+' || c == '%' || c.is_whitespace() || c.is_control() {
            for b in c.to_string().bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn parse_one(spec: &str) -> Result<Filter, String> {
    if spec == "blob:none" {
        return Ok(Filter::BlobNone);
    }
    if let Some(v) = spec.strip_prefix("blob:limit=") {
        return parse_size(v)
            .map(Filter::BlobLimit)
            .ok_or_else(|| messages::invalid_filter_spec(spec));
    }
    if let Some(v) = spec.strip_prefix("tree:") {
        if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
            return Err(messages::EXPECTED_TREE_DEPTH.into());
        }
        return v
            .parse()
            .map(Filter::Tree)
            .map_err(|_| messages::EXPECTED_TREE_DEPTH.into());
    }
    if let Some(v) = spec.strip_prefix("sparse:oid=") {
        return Ok(Filter::SparseOid(v.to_owned()));
    }
    if spec.starts_with("sparse:path=") {
        return Err(messages::SPARSE_PATH_DROPPED.into());
    }
    if let Some(v) = spec.strip_prefix("object:type=") {
        return match v {
            "commit" | "tree" | "blob" | "tag" => Ok(Filter::ObjectType(v.to_owned())),
            _ => Err(messages::invalid_object_type(v)),
        };
    }
    if let Some(v) = spec.strip_prefix("combine:") {
        let mut parts = Vec::new();
        for p in v.split('+') {
            if p.is_empty() {
                return Err(messages::invalid_filter_spec(spec));
            }
            parts.push(parse_one(&url_decode(p))?);
        }
        return Ok(Filter::Combine(parts));
    }
    Err(messages::invalid_filter_spec(spec))
}

impl FilterSpec {
    /// Parse one `--filter` value; the error is git's `fatal:` message.
    pub fn parse(spec: &str) -> Result<FilterSpec, String> {
        let filter = parse_one(spec)?;
        let text = match filter {
            Filter::BlobLimit(n) => format!("blob:limit={n}"),
            _ => spec.to_owned(),
        };
        Ok(FilterSpec { text, filter })
    }

    /// Several `--filter` values (one gives that one).
    pub fn parse_all(specs: &[String]) -> Result<Option<FilterSpec>, String> {
        let mut parsed = Vec::new();
        for s in specs {
            parsed.push(FilterSpec::parse(s)?);
        }
        Ok(match parsed.len() {
            0 => None,
            1 => parsed.pop(),
            _ => {
                let text = format!(
                    "combine:{}",
                    specs
                        .iter()
                        .map(|s| url_encode(s))
                        .collect::<Vec<_>>()
                        .join("+")
                );
                Some(FilterSpec {
                    text,
                    filter: Filter::Combine(parsed.into_iter().map(|p| p.filter).collect()),
                })
            }
        })
    }

    /// The spec as written into a bundle header.
    pub fn as_str(&self) -> &str {
        &self.text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization() {
        assert_eq!(
            FilterSpec::parse("blob:limit=2m").unwrap().as_str(),
            "blob:limit=2097152"
        );
        assert_eq!(FilterSpec::parse("tree:01").unwrap().as_str(), "tree:01");
        let both =
            FilterSpec::parse_all(&["blob:limit=1k".into(), "object:type=commit".into()]).unwrap();
        assert_eq!(
            both.unwrap().as_str(),
            "combine:blob:limit=1k+object:type=commit"
        );
    }

    #[test]
    fn errors() {
        assert_eq!(
            FilterSpec::parse("bogus:x").unwrap_err(),
            "invalid filter-spec 'bogus:x'"
        );
        assert_eq!(
            FilterSpec::parse("tree:-1").unwrap_err(),
            "expected 'tree:<depth>'"
        );
        assert!(FilterSpec::parse("blob:limit=abc").is_err());
        assert!(FilterSpec::parse("object:type=bogus").is_err());
    }
}
