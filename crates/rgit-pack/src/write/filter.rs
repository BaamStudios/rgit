// SPDX-License-Identifier: MIT

//! Object filters (`--filter=<filter-spec>` of `rev-list` and
//! `pack-objects`, the `filter` argument of a fetch): leaving objects out
//! of an enumeration for partial clones.
//!
//! The specs and their meaning are in
//! `Documentation/rev-list-options.adoc`; the messages for bad specs were
//! observed from git 2.56.

use std::fmt;

use super::messages as m;
use crate::ObjectKind;

/// A parsed filter spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectFilter {
    /// `blob:none`: no blobs.
    BlobNone,
    /// `blob:limit=<n>`: no blobs of at least `n` bytes.
    BlobLimit(u64),
    /// `tree:<depth>`: no trees and blobs at this depth or deeper (the
    /// root tree of a commit has depth 0).
    TreeDepth(u64),
    /// `object:type=<type>`: only objects of this type.
    ObjectType(ObjectKind),
    /// `sparse:oid=<blob-ish>`: only the blobs a sparse checkout with the
    /// patterns in that blob needs. Parsed, but the enumeration does not
    /// support it.
    SparseOid(String),
    /// `combine:<f1>+<f2>...`: objects every filter accepts.
    Combine(Vec<ObjectFilter>),
}

/// A filter spec that cannot be used. `Display` is git's message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterError {
    /// Not a filter spec.
    Invalid(String),
    /// `tree:` without a number.
    TreeDepth,
    /// `object:type=` with something that is no object type.
    ObjectType(String),
    /// `sparse:path=...`.
    SparsePath,
    /// `combine:` without filters.
    EmptyCombine,
    /// A reserved character in a filter inside `combine:`.
    Unescaped(char),
    /// A filter the enumeration cannot apply.
    Unsupported(String),
}

impl fmt::Display for FilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FilterError::Invalid(spec) => f.write_str(&m::invalid_filter_spec(spec)),
            FilterError::TreeDepth => f.write_str(m::EXPECTED_TREE_DEPTH),
            FilterError::ObjectType(t) => f.write_str(&m::invalid_filter_object_type(t)),
            FilterError::SparsePath => f.write_str(m::SPARSE_PATH_DROPPED),
            FilterError::EmptyCombine => f.write_str(m::EXPECTED_AFTER_COMBINE),
            FilterError::Unescaped(c) => f.write_str(&m::must_escape_char(*c)),
            FilterError::Unsupported(spec) => f.write_str(&m::filter_not_supported(spec)),
        }
    }
}

impl std::error::Error for FilterError {}

/// A number with an optional `k`, `m` or `g` suffix (powers of 1024).
fn parse_magnitude(s: &str) -> Option<u64> {
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

/// Characters that must be %-encoded in a filter inside `combine:`.
const RESERVED: &str = "~!@#$^&*()[]{}\\;\",<>?'`";

fn url_decode(s: &str) -> Result<String, FilterError> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%' {
            let value = bytes
                .get(i + 1..i + 3)
                .and_then(|hex| std::str::from_utf8(hex).ok())
                .and_then(|hex| u8::from_str_radix(hex, 16).ok());
            if let Some(v) = value {
                out.push(v);
                i += 3;
                continue;
            }
        }
        if b <= 0x20 || RESERVED.as_bytes().contains(&b) {
            return Err(FilterError::Unescaped(b as char));
        }
        out.push(b);
        i += 1;
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

impl ObjectFilter {
    /// Parse a filter spec.
    pub fn parse(spec: &str) -> Result<ObjectFilter, FilterError> {
        let invalid = || FilterError::Invalid(spec.to_owned());
        if spec == "blob:none" {
            return Ok(ObjectFilter::BlobNone);
        }
        if let Some(n) = spec.strip_prefix("blob:limit=") {
            return parse_magnitude(n)
                .map(ObjectFilter::BlobLimit)
                .ok_or_else(invalid);
        }
        if let Some(n) = spec.strip_prefix("tree:") {
            return parse_magnitude(n)
                .map(ObjectFilter::TreeDepth)
                .ok_or(FilterError::TreeDepth);
        }
        if let Some(t) = spec.strip_prefix("object:type=") {
            return ObjectKind::from_name(t.as_bytes())
                .map(ObjectFilter::ObjectType)
                .ok_or_else(|| FilterError::ObjectType(t.to_owned()));
        }
        if let Some(oid) = spec.strip_prefix("sparse:oid=") {
            return Ok(ObjectFilter::SparseOid(oid.to_owned()));
        }
        if spec.starts_with("sparse:path=") {
            return Err(FilterError::SparsePath);
        }
        if let Some(list) = spec.strip_prefix("combine:") {
            if list.is_empty() {
                return Err(FilterError::EmptyCombine);
            }
            let mut subs = Vec::new();
            for sub in list.split('+').filter(|s| !s.is_empty()) {
                subs.push(ObjectFilter::parse(&url_decode(sub)?)?);
            }
            return Ok(ObjectFilter::Combine(subs));
        }
        Err(invalid())
    }

    /// The spec with numbers written out (`blob:limit=1k` becomes
    /// `blob:limit=1024`), as one process passes it to another.
    pub fn to_spec(&self) -> String {
        match self {
            ObjectFilter::BlobNone => "blob:none".to_owned(),
            ObjectFilter::BlobLimit(n) => format!("blob:limit={n}"),
            ObjectFilter::TreeDepth(n) => format!("tree:{n}"),
            ObjectFilter::ObjectType(k) => format!("object:type={}", k.name()),
            ObjectFilter::SparseOid(o) => format!("sparse:oid={o}"),
            ObjectFilter::Combine(subs) => {
                let mut out = String::from("combine:");
                for (i, sub) in subs.iter().enumerate() {
                    if i > 0 {
                        out.push('+');
                    }
                    for b in sub.to_spec().bytes() {
                        if b <= 0x20 || b == b'%' || b == b'+' || RESERVED.as_bytes().contains(&b) {
                            out.push_str(&format!("%{b:02x}"));
                        } else {
                            out.push(b as char);
                        }
                    }
                }
                out
            }
        }
    }

    /// The name of this kind of filter in `uploadpackfilter.<name>.allow`.
    pub fn config_name(&self) -> &'static str {
        match self {
            ObjectFilter::BlobNone => "blob:none",
            ObjectFilter::BlobLimit(_) => "blob:limit",
            ObjectFilter::TreeDepth(_) => "tree",
            ObjectFilter::ObjectType(_) => "object:type",
            ObjectFilter::SparseOid(_) => "sparse:oid",
            ObjectFilter::Combine(_) => "combine",
        }
    }

    /// The form the enumeration applies.
    pub(crate) fn compile(&self) -> Result<Compiled, FilterError> {
        let mut c = Compiled::default();
        self.compile_into(&mut c)?;
        Ok(c)
    }

    fn compile_into(&self, c: &mut Compiled) -> Result<(), FilterError> {
        match self {
            ObjectFilter::BlobNone => c.blob_limit = Some(0),
            ObjectFilter::BlobLimit(n) => {
                c.blob_limit = Some(c.blob_limit.map_or(*n, |old| old.min(*n)));
            }
            ObjectFilter::TreeDepth(n) => {
                c.tree_depth = Some(c.tree_depth.map_or(*n, |old| old.min(*n)));
            }
            ObjectFilter::ObjectType(k) => {
                c.kinds = Some(match c.kinds {
                    None => vec![*k],
                    Some(ref old) => old.iter().copied().filter(|o| o == k).collect(),
                });
            }
            ObjectFilter::SparseOid(_) => {
                return Err(FilterError::Unsupported(self.to_spec()));
            }
            ObjectFilter::Combine(subs) => {
                for sub in subs {
                    sub.compile_into(c)?;
                }
            }
        }
        Ok(())
    }
}

/// The conditions of a filter, combined.
#[derive(Debug, Clone, Default)]
pub(crate) struct Compiled {
    /// Blobs of at least this size are left out (0: all blobs).
    pub blob_limit: Option<u64>,
    /// Trees and blobs at this depth or deeper are left out.
    pub tree_depth: Option<u64>,
    /// Only these types are listed.
    pub kinds: Option<Vec<ObjectKind>>,
}

impl Compiled {
    /// Whether objects of `kind` can be listed at all.
    pub fn allows(&self, kind: ObjectKind) -> bool {
        self.kinds.as_ref().is_none_or(|k| k.contains(&kind))
    }

    /// Whether the trees of commits have to be walked.
    pub fn walks_trees(&self) -> bool {
        self.tree_depth != Some(0)
            && (self.allows(ObjectKind::Tree)
                || (self.allows(ObjectKind::Blob) && self.blob_limit != Some(0)))
    }

    /// Whether trees the reader has are still good delta bases (the reader
    /// has every tree of the history it has).
    pub fn keeps_all_trees(&self) -> bool {
        self.tree_depth.is_none() && self.kinds.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_specs() {
        use ObjectFilter::*;
        assert_eq!(ObjectFilter::parse("blob:none"), Ok(BlobNone));
        assert_eq!(ObjectFilter::parse("blob:limit=1k"), Ok(BlobLimit(1024)));
        assert_eq!(ObjectFilter::parse("blob:limit=0"), Ok(BlobLimit(0)));
        assert_eq!(ObjectFilter::parse("tree:2"), Ok(TreeDepth(2)));
        assert_eq!(ObjectFilter::parse("tree:1k"), Ok(TreeDepth(1024)));
        assert_eq!(
            ObjectFilter::parse("object:type=tag"),
            Ok(ObjectType(ObjectKind::Tag))
        );
        assert_eq!(
            ObjectFilter::parse("combine:tree%3A2+blob%3anone+"),
            Ok(Combine(vec![TreeDepth(2), BlobNone]))
        );
        assert_eq!(
            ObjectFilter::parse("combine:blob:limit=1k+combine:tree:2+blob:none"),
            Ok(Combine(vec![
                BlobLimit(1024),
                Combine(vec![TreeDepth(2)]),
                BlobNone
            ]))
        );
    }

    #[test]
    fn rejects_bad_specs_with_git_messages() {
        let msg = |s: &str| ObjectFilter::parse(s).unwrap_err().to_string();
        assert_eq!(msg("bogus"), "invalid filter-spec 'bogus'");
        assert_eq!(msg(""), "invalid filter-spec ''");
        assert_eq!(msg("blob:limit=1t"), "invalid filter-spec 'blob:limit=1t'");
        assert_eq!(msg("blob:limit=-1"), "invalid filter-spec 'blob:limit=-1'");
        assert_eq!(msg("tree:x"), "expected 'tree:<depth>'");
        assert_eq!(
            msg("object:type=x"),
            "'x' for 'object:type=<type>' is not a valid object type"
        );
        assert_eq!(
            msg("sparse:path=x"),
            "sparse:path filters support has been dropped"
        );
        assert_eq!(msg("combine:"), "expected something after combine:");
        assert_eq!(
            msg("combine:tree:2+blob~x"),
            "must escape char in sub-filter-spec: '~'"
        );
        assert_eq!(msg("combine:bogus+tree:1"), "invalid filter-spec 'bogus'");
    }

    #[test]
    fn writes_specs_expanded() {
        for (spec, want) in [
            ("blob:limit=1k", "blob:limit=1024"),
            ("tree:3", "tree:3"),
            ("combine:tree:2+blob:none", "combine:tree:2+blob:none"),
            (
                "combine:blob:none+combine:tree:1%2Bobject:type=blob",
                "combine:blob:none+combine:tree:1%2bobject:type=blob",
            ),
        ] {
            let f = ObjectFilter::parse(spec).unwrap();
            assert_eq!(f.to_spec(), want);
            let again = ObjectFilter::parse(&f.to_spec()).unwrap();
            assert_eq!(again, f);
        }
        // A combined filter inside a combined filter keeps its own list.
        assert_eq!(
            ObjectFilter::parse("combine:blob:none+combine:tree:1%2bobject:type=blob"),
            Ok(ObjectFilter::Combine(vec![
                ObjectFilter::BlobNone,
                ObjectFilter::Combine(vec![
                    ObjectFilter::TreeDepth(1),
                    ObjectFilter::ObjectType(ObjectKind::Blob)
                ]),
            ]))
        );
    }

    #[test]
    fn combines_conditions() {
        let c = ObjectFilter::parse("combine:blob:limit=10+blob:none+tree:3+tree:2")
            .unwrap()
            .compile()
            .unwrap();
        assert_eq!(c.blob_limit, Some(0));
        assert_eq!(c.tree_depth, Some(2));
        assert!(c.walks_trees());
        let c = ObjectFilter::parse("object:type=commit")
            .unwrap()
            .compile()
            .unwrap();
        assert!(!c.walks_trees());
        assert!(c.allows(ObjectKind::Commit) && !c.allows(ObjectKind::Tree));
        assert!(
            ObjectFilter::parse("sparse:oid=HEAD:x")
                .unwrap()
                .compile()
                .is_err()
        );
    }
}
