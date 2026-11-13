// SPDX-License-Identifier: MIT
//! Which remote refs a fetch asks for and where they go: `ref-prefix`
//! arguments, the ref map from refspecs, opportunistic remote-tracking
//! updates, merge candidates and tag following.

use bstr::{BString, ByteSlice};
use rgit_hash::{HashKind, ObjectId};
use rgit_protocol::v2::RemoteRef;
use rgit_remote::refspec::{FetchMapping, fetch_map, remove_duplicate_destinations};
use rgit_remote::{Refspec, RefspecSet};

use crate::{Error, Result};

/// Why an entry is in the ref map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// From a refspec (command line or configuration).
    Refspec,
    /// A command-line ref also stored in its configured remote-tracking
    /// ref ("opportunistic" update). Not written to `FETCH_HEAD`.
    Opportunistic,
    /// `branch.<name>.merge` of the current branch, fetched for
    /// `FETCH_HEAD` only.
    Merge,
    /// A tag pointing into the fetched history.
    FollowedTag,
}

/// One remote ref to fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The remote ref name (or the object name of an exact-oid refspec).
    pub remote: BString,
    /// The object the remote ref points to.
    pub oid: ObjectId,
    /// The local ref to update; `None` stores into `FETCH_HEAD` only.
    pub local: Option<BString>,
    /// Non-fast-forward updates are allowed (`+`).
    pub force: bool,
    /// Why the entry exists.
    pub origin: Origin,
    /// A merge candidate (no `not-for-merge` in `FETCH_HEAD`).
    pub for_merge: bool,
}

/// The `ref-prefix` arguments for `ls-refs`: the literal part of every
/// pattern source, every way a short source name can be spelled, then
/// `extra` (merge refs, `refs/tags/`, `HEAD`); without duplicates.
pub fn ref_prefixes(specs: &RefspecSet, extra: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |p: String| {
        if !out.contains(&p) {
            out.push(p);
        }
    };
    for spec in specs.positive() {
        if spec.exact_oid {
            continue;
        }
        let src = spec.src.to_str_lossy();
        if spec.pattern {
            let star = src.find('*').unwrap_or(src.len());
            add(src[..star].to_string());
        } else {
            let src = if src.is_empty() { "HEAD".into() } else { src };
            for rule in [
                "{}",
                "refs/{}",
                "refs/tags/{}",
                "refs/heads/{}",
                "refs/remotes/{}",
                "refs/remotes/{}/HEAD",
            ] {
                add(rule.replace("{}", &src));
            }
        }
    }
    for e in extra {
        add(e.clone());
    }
    out
}

/// Look up the oid of the remote ref `name`.
pub fn advertised<'a>(refs: &'a [RemoteRef], name: &[u8]) -> Option<&'a RemoteRef> {
    refs.iter().find(|r| r.name == name)
}

/// Map the advertised refs through `specs`. `refs` without an object
/// (unborn `HEAD`) are not candidates. Messages for ignored refs are
/// appended to `errors`.
pub fn map_refspecs(
    specs: &RefspecSet,
    refs: &[RemoteRef],
    hash: HashKind,
    errors: &mut Vec<String>,
) -> Result<Vec<Entry>> {
    let names: Vec<&[u8]> = refs
        .iter()
        .filter(|r| r.oid.is_some())
        .map(|r| r.name.as_slice())
        .collect();
    let map = fetch_map(specs, &names, false).map_err(|e| Error::fatal(e.to_string()))?;
    errors.extend(map.errors);
    let mut out = Vec::new();
    for m in map.mappings {
        out.push(entry_for(&m, refs, hash)?);
    }
    Ok(out)
}

fn entry_for(m: &FetchMapping, refs: &[RemoteRef], hash: HashKind) -> Result<Entry> {
    let oid = match advertised(refs, &m.src).and_then(|r| r.oid) {
        Some(o) => o,
        None => ObjectId::from_hex(hash, &m.src)
            .map_err(|_| Error::fatal(format!("couldn't find remote ref {}", m.src)))?,
    };
    Ok(Entry {
        remote: m.src.clone(),
        oid,
        local: m.dst.as_ref().map(|d| qualify_dst(d, &m.src)),
        force: m.force,
        origin: Origin::Refspec,
        for_merge: false,
    })
}

/// A destination that is not a full ref name goes to the namespace of the
/// source: `main:x` stores `refs/heads/x`, a tag `refs/tags/x`.
fn qualify_dst(dst: &BString, src: &BString) -> BString {
    if dst.starts_with(b"refs/") || dst == "HEAD" {
        return dst.clone();
    }
    let ns: &[u8] = if src.starts_with(b"refs/tags/") {
        b"refs/tags/"
    } else {
        b"refs/heads/"
    };
    let mut out = BString::from(ns);
    out.extend_from_slice(dst);
    out
}

/// Drop entries that repeat an earlier destination with the same source;
/// two sources for one destination are an error.
pub fn dedup(entries: Vec<Entry>) -> Result<Vec<Entry>> {
    let as_mapping: Vec<FetchMapping> = entries
        .iter()
        .map(|e| FetchMapping {
            src: e.remote.clone(),
            dst: e.local.clone(),
            force: e.force,
            from_pattern: false,
        })
        .collect();
    remove_duplicate_destinations(as_mapping).map_err(|e| Error::fatal(e.to_string()))?;
    let mut out: Vec<Entry> = Vec::with_capacity(entries.len());
    for e in entries {
        if let Some(dst) = &e.local {
            if let Some(prev) = out.iter_mut().find(|o| o.local.as_ref() == Some(dst)) {
                prev.force |= e.force;
                continue;
            }
        }
        out.push(e);
    }
    Ok(out)
}

/// The configured remote-tracking refs for command-line entries: each
/// remote ref that a configured refspec maps to a local ref not already
/// updated gets an extra [`Origin::Opportunistic`] entry.
pub fn opportunistic(entries: &[Entry], configured: &RefspecSet) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for e in entries {
        if e.origin != Origin::Refspec {
            continue;
        }
        let Some(dst) = configured.map_src(&e.remote) else {
            continue;
        };
        let taken = entries
            .iter()
            .chain(out.iter())
            .any(|o| o.local.as_ref() == Some(&dst));
        if taken {
            continue;
        }
        let force = configured
            .positive()
            .find(|s: &&Refspec| s.map_src(&e.remote).is_some())
            .is_some_and(|s| s.force);
        out.push(Entry {
            remote: e.remote.clone(),
            oid: e.oid,
            local: Some(dst),
            force,
            origin: Origin::Opportunistic,
            for_merge: false,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgit_remote::Direction;

    #[test]
    fn prefixes() {
        let specs = RefspecSet::parse(
            ["+refs/heads/*:refs/remotes/origin/*", "side"],
            Direction::Fetch,
        )
        .unwrap();
        let p = ref_prefixes(&specs, &["refs/tags/".into(), "HEAD".into()]);
        assert_eq!(
            p,
            [
                "refs/heads/",
                "side",
                "refs/side",
                "refs/tags/side",
                "refs/heads/side",
                "refs/remotes/side",
                "refs/remotes/side/HEAD",
                "refs/tags/",
                "HEAD"
            ]
        );
    }
}
