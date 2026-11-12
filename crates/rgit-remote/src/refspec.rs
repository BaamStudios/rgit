// SPDX-License-Identifier: MIT
//! Refspecs: `[+]<src>[:<dst>]`, patterns with one `*`, negative refspecs
//! (`^<pattern>`), validation for fetch and push, matching and mapping.
//!
//! Syntax and semantics follow `git-fetch(1)` / `git-push(1)` ("refspec")
//! and were checked against the `git` binary (which inputs it rejects with
//! `invalid refspec`).

use std::fmt;

use bstr::{BStr, BString, ByteSlice};
use rgit_hash::HashKind;

use crate::messages;
use crate::refname::{RefnameFlags, check_refname_format};

/// Whether a refspec is used for fetching or pushing; the validation
/// differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// `remote.<name>.fetch`, `git fetch <refspec>`.
    Fetch,
    /// `remote.<name>.push`, `git push <refspec>`.
    Push,
}

/// A refspec git rejects. `Display` is git's message
/// (`invalid refspec '<spec>'`, printed with `fatal: `).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", messages::invalid_refspec(.0.as_bstr()))]
pub struct InvalidRefspec(pub BString);

/// One parsed refspec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refspec {
    /// Leading `+`: allow non-fast-forward updates.
    pub force: bool,
    /// Leading `^`: a negative refspec (only `src`, never forced).
    pub negative: bool,
    /// `src` (and `dst`, if any) contain one `*`.
    pub pattern: bool,
    /// Push only: the `:` / `+:` "matching branches" refspec.
    pub matching: bool,
    /// `src` is a full hexadecimal object name.
    pub exact_oid: bool,
    /// The source side. `@` is stored as `HEAD`. Empty for `:dst`
    /// (push: delete; fetch: `HEAD`).
    pub src: BString,
    /// The destination after the last `:`; `None` without a colon.
    pub dst: Option<BString>,
    raw: BString,
}

impl Refspec {
    /// Parse `input` for `direction` in a SHA-1 repository.
    pub fn parse(input: impl AsRef<[u8]>, direction: Direction) -> Result<Refspec, InvalidRefspec> {
        Self::parse_with_hash(input, direction, HashKind::Sha1)
    }

    /// Parse `input` for `direction`; `hash` decides what a full object
    /// name looks like.
    pub fn parse_with_hash(
        input: impl AsRef<[u8]>,
        direction: Direction,
        hash: HashKind,
    ) -> Result<Refspec, InvalidRefspec> {
        let raw = input.as_ref();
        let invalid = || InvalidRefspec(raw.into());
        // A leading `^` makes a negative refspec (which cannot be forced);
        // a `^` after `+` is just part of the source.
        let negative = raw.first() == Some(&b'^');
        let force = !negative && raw.first() == Some(&b'+');
        let rest = if negative || force { &raw[1..] } else { raw };
        let (src, dst) = match rest.rfind_byte(b':') {
            Some(i) => (&rest[..i], Some(&rest[i + 1..])),
            None => (rest, None),
        };
        if negative && (dst.is_some() || src.is_empty()) {
            return Err(invalid());
        }
        let src: &[u8] = if src == b"@" { b"HEAD" } else { src };
        let src_glob = src.contains(&b'*');
        if let Some(d) = dst {
            if src_glob != d.contains(&b'*') {
                return Err(invalid());
            }
        }
        if direction == Direction::Fetch && dst.is_none() && src_glob && !negative {
            return Err(invalid());
        }
        let flags = if src_glob {
            RefnameFlags::REFSPEC_PATTERN
        } else {
            RefnameFlags::REFSPEC
        };
        let exact_oid = !src_glob && is_full_hex(src, hash);
        if negative {
            if exact_oid || !check_refname_format(src, flags) {
                return Err(invalid());
            }
        } else if src.is_empty() {
            if direction == Direction::Push && dst.is_none() {
                return Err(invalid());
            }
        } else {
            // Push sources with an explicit destination may be any revision
            // expression (`main~1:refs/heads/x`); everything else must be a
            // reference name.
            let unchecked = direction == Direction::Push && dst.is_some() && !src_glob;
            if !unchecked && !check_refname_format(src, flags) {
                return Err(invalid());
            }
        }
        let dst_ok = match dst {
            None => true,
            // `src:` deletes nothing and updates nothing: fine for fetch,
            // invalid for push except for the matching refspec `:`.
            Some([]) => direction == Direction::Fetch || src.is_empty(),
            Some(d) => check_refname_format(d, flags),
        };
        if !dst_ok {
            return Err(invalid());
        }
        let matching = direction == Direction::Push && src.is_empty() && dst == Some(&[][..]);
        Ok(Refspec {
            force,
            negative,
            pattern: src_glob,
            matching,
            exact_oid,
            src: src.into(),
            dst: dst.map(BString::from),
            raw: raw.into(),
        })
    }

    /// The text the refspec was parsed from.
    pub fn as_bstr(&self) -> &BStr {
        self.raw.as_bstr()
    }

    /// Whether `name` matches the source side (a pattern or an exact name).
    pub fn src_matches(&self, name: &[u8]) -> bool {
        if self.pattern {
            match_pattern(&self.src, name).is_some()
        } else {
            self.src == name
        }
    }

    /// Forward mapping: the destination for source `name`, if the source
    /// side matches. Without a destination the source maps to itself.
    pub fn map_src(&self, name: &[u8]) -> Option<BString> {
        if self.negative {
            return None;
        }
        let dst: &[u8] = self
            .dst
            .as_ref()
            .map_or(self.src.as_slice(), |d| d.as_slice());
        if self.pattern {
            let star = match_pattern(&self.src, name)?;
            Some(expand_pattern(dst, star))
        } else if self.src == name {
            Some(dst.into())
        } else {
            None
        }
    }

    /// Reverse mapping: the source for destination `name`.
    pub fn map_dst(&self, name: &[u8]) -> Option<BString> {
        if self.negative {
            return None;
        }
        let dst: &[u8] = self
            .dst
            .as_ref()
            .map_or(self.src.as_slice(), |d| d.as_slice());
        if self.pattern {
            let star = match_pattern(dst, name)?;
            Some(expand_pattern(&self.src, star))
        } else if dst == name {
            Some(self.src.clone())
        } else {
            None
        }
    }
}

impl fmt::Display for Refspec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.raw)
    }
}

fn is_full_hex(s: &[u8], hash: HashKind) -> bool {
    s.len() == hash.hex_len() && s.iter().all(u8::is_ascii_hexdigit)
}

/// Match `name` against a pattern with one `*` (which matches any sequence,
/// including an empty one and `/`). Returns what the `*` matched.
pub fn match_pattern<'a>(pattern: &[u8], name: &'a [u8]) -> Option<&'a [u8]> {
    let star = pattern.find_byte(b'*')?;
    let (prefix, suffix) = (&pattern[..star], &pattern[star + 1..]);
    if name.len() < prefix.len() + suffix.len()
        || !name.starts_with(prefix)
        || !name.ends_with(suffix)
    {
        return None;
    }
    Some(&name[prefix.len()..name.len() - suffix.len()])
}

/// Replace the `*` of `pattern` with `star`.
pub fn expand_pattern(pattern: &[u8], star: &[u8]) -> BString {
    match pattern.find_byte(b'*') {
        Some(i) => {
            let mut out = BString::from(&pattern[..i]);
            out.extend_from_slice(star);
            out.extend_from_slice(&pattern[i + 1..]);
            out
        }
        None => pattern.into(),
    }
}

/// The rules an abbreviated ref name is expanded with (`gitrevisions(7)`),
/// in priority order.
pub const REF_REV_PARSE_RULES: [(&str, &str); 6] = [
    ("", ""),
    ("refs/", ""),
    ("refs/tags/", ""),
    ("refs/heads/", ""),
    ("refs/remotes/", ""),
    ("refs/remotes/", "/HEAD"),
];

/// The index of the first rule under which `abbrev` names `full`.
pub fn refname_match_rule(abbrev: &[u8], full: &[u8]) -> Option<usize> {
    REF_REV_PARSE_RULES.iter().position(|(pre, post)| {
        full.len() == pre.len() + abbrev.len() + post.len()
            && full.starts_with(pre.as_bytes())
            && full.ends_with(post.as_bytes())
            && &full[pre.len()..pre.len() + abbrev.len()] == abbrev
    })
}

/// Whether `abbrev` names `full` under one of the expansion rules.
pub fn refname_match(abbrev: &[u8], full: &[u8]) -> bool {
    refname_match_rule(abbrev, full).is_some()
}

/// The ref among `refs` that `abbrev` names best (lowest rule; the first
/// one on a tie).
pub fn find_by_abbrev<'a, S: AsRef<[u8]>>(abbrev: &[u8], refs: &'a [S]) -> Option<&'a S> {
    refs.iter()
        .filter_map(|r| refname_match_rule(abbrev, r.as_ref()).map(|k| (k, r)))
        .min_by_key(|(k, _)| *k)
        .map(|(_, r)| r)
}

/// An ordered list of refspecs of one direction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefspecSet {
    /// The refspecs in configuration / command-line order.
    pub items: Vec<Refspec>,
}

impl RefspecSet {
    /// Parse every item; the first invalid one is the error.
    pub fn parse<I, S>(items: I, direction: Direction) -> Result<RefspecSet, InvalidRefspec>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<[u8]>,
    {
        let items = items
            .into_iter()
            .map(|s| Refspec::parse(s, direction))
            .collect::<Result<_, _>>()?;
        Ok(RefspecSet { items })
    }

    /// Whether there are no refspecs.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The positive refspecs.
    pub fn positive(&self) -> impl Iterator<Item = &Refspec> {
        self.items.iter().filter(|r| !r.negative)
    }

    /// Whether `name` is excluded by a negative refspec.
    pub fn is_excluded(&self, name: &[u8]) -> bool {
        self.items.iter().any(|r| r.negative && r.src_matches(name))
    }

    /// Forward mapping through the first positive refspec whose source
    /// matches `name`; `None` if none does or a negative refspec excludes
    /// it. Used to find the remote-tracking ref of a remote branch.
    pub fn map_src(&self, name: &[u8]) -> Option<BString> {
        if self.is_excluded(name) {
            return None;
        }
        self.positive().find_map(|r| {
            r.dst.as_ref()?;
            r.map_src(name)
        })
    }

    /// Reverse mapping: the remote ref whose destination is `name`.
    pub fn map_dst(&self, name: &[u8]) -> Option<BString> {
        let src = self.positive().find_map(|r| {
            r.dst.as_ref()?;
            r.map_dst(name)
        })?;
        (!self.is_excluded(&src)).then_some(src)
    }
}

/// One planned ref update of a fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchMapping {
    /// The remote ref name, or the full object name for an `exact_oid`
    /// refspec.
    pub src: BString,
    /// The local ref to update, if any.
    pub dst: Option<BString>,
    /// From a `+` refspec.
    pub force: bool,
    /// From a pattern refspec.
    pub from_pattern: bool,
}

/// Errors of [`fetch_map`] and [`remove_duplicate_destinations`];
/// `Display` gives git's message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FetchMapError {
    /// An exact source that the remote does not advertise.
    #[error("{}", messages::couldnt_find_remote_ref(.0.as_bstr()))]
    MissingRemoteRef(BString),
    /// Two different sources map to the same destination.
    #[error("{}", messages::cannot_fetch_both(.first.as_bstr(), .second.as_bstr(), .dst.as_bstr()))]
    DuplicateDestination {
        /// The first source.
        first: BString,
        /// The second source.
        second: BString,
        /// The shared destination.
        dst: BString,
    },
}

/// The result of [`fetch_map`]: the mappings plus `error:` lines git prints
/// without failing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchMap {
    /// Planned updates in refspec order.
    pub mappings: Vec<FetchMapping>,
    /// Messages to print with `error: `.
    pub errors: Vec<String>,
}

/// Map the refs a remote advertises through fetch refspecs.
///
/// Exact sources are looked up with the abbreviation rules
/// ([`find_by_abbrev`]); an empty source means `HEAD`. Pattern refspecs
/// produce one mapping per matching ref, in `remote_refs` order; a mapped
/// destination that is not a valid ref name is dropped with
/// `* Ignoring funny ref '<dst>' locally`. Negative refspecs then remove
/// matching sources. With `missing_ok`, unknown exact sources are skipped
/// instead of failing.
pub fn fetch_map<S: AsRef<[u8]>>(
    specs: &RefspecSet,
    remote_refs: &[S],
    missing_ok: bool,
) -> Result<FetchMap, FetchMapError> {
    let mut out = FetchMap::default();
    for spec in specs.positive() {
        if spec.pattern {
            for r in remote_refs {
                let name = r.as_ref();
                let Some(star) = match_pattern(&spec.src, name) else {
                    continue;
                };
                let dst = spec.dst.as_deref().map(|d| expand_pattern(d, star));
                if let Some(d) = &dst {
                    if !check_refname_format(d, RefnameFlags::default()) {
                        out.errors.push(messages::ignoring_funny_ref(d.as_bstr()));
                        continue;
                    }
                }
                out.mappings.push(FetchMapping {
                    src: name.into(),
                    dst,
                    force: spec.force,
                    from_pattern: true,
                });
            }
            continue;
        }
        let dst = spec.dst.clone().filter(|d| !d.is_empty());
        if spec.exact_oid {
            out.mappings.push(FetchMapping {
                src: spec.src.clone(),
                dst,
                force: spec.force,
                from_pattern: false,
            });
            continue;
        }
        let want: &[u8] = if spec.src.is_empty() {
            b"HEAD"
        } else {
            &spec.src
        };
        match find_by_abbrev(want, remote_refs) {
            Some(r) => out.mappings.push(FetchMapping {
                src: r.as_ref().into(),
                dst,
                force: spec.force,
                from_pattern: false,
            }),
            None if missing_ok => {}
            None => return Err(FetchMapError::MissingRemoteRef(want.into())),
        }
    }
    out.mappings.retain(|m| !specs.is_excluded(&m.src));
    Ok(out)
}

/// Drop mappings that repeat an earlier one (same source and destination)
/// and fail when two different sources would update the same destination.
pub fn remove_duplicate_destinations(
    mappings: Vec<FetchMapping>,
) -> Result<Vec<FetchMapping>, FetchMapError> {
    let mut out: Vec<FetchMapping> = Vec::with_capacity(mappings.len());
    for m in mappings {
        let Some(dst) = &m.dst else {
            out.push(m);
            continue;
        };
        match out.iter_mut().find(|o| o.dst.as_ref() == Some(dst)) {
            Some(prev) if prev.src == m.src => prev.force |= m.force,
            Some(prev) => {
                return Err(FetchMapError::DuplicateDestination {
                    first: prev.src.clone(),
                    second: m.src.clone(),
                    dst: dst.clone(),
                });
            }
            None => out.push(m),
        }
    }
    Ok(out)
}

/// The default fetch refspec of a new remote:
/// `+refs/heads/*:refs/remotes/<name>/*`.
pub fn default_fetch_refspec(remote: &str) -> String {
    format!("+refs/heads/*:refs/remotes/{remote}/*")
}

/// What `git clone` needs to fetch and what it records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloneRefspec {
    /// The refspec the initial fetch uses.
    pub fetch: String,
    /// The value written to `remote.<name>.fetch` (`None` for a bare,
    /// non-mirror clone).
    pub configured: Option<String>,
}

/// Options of [`clone_refspec`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloneOptions<'a> {
    /// `--bare`.
    pub bare: bool,
    /// `--mirror` (implies bare).
    pub mirror: bool,
    /// `--single-branch` with the full ref to clone (`refs/heads/<b>` or
    /// `refs/tags/<t>`).
    pub single: Option<&'a str>,
}

/// The refspec of `git clone` for remote `remote`.
pub fn clone_refspec(remote: &str, opts: &CloneOptions<'_>) -> CloneRefspec {
    if opts.mirror {
        let s = "+refs/*:refs/*".to_string();
        return CloneRefspec {
            fetch: s.clone(),
            configured: Some(s),
        };
    }
    let fetch = match opts.single {
        Some(r) if r.starts_with("refs/tags/") => format!("+{r}:{r}"),
        Some(r) => {
            let b = r.strip_prefix("refs/heads/").unwrap_or(r);
            if opts.bare {
                format!("+refs/heads/{b}:refs/heads/{b}")
            } else {
                format!("+refs/heads/{b}:refs/remotes/{remote}/{b}")
            }
        }
        None if opts.bare => "+refs/heads/*:refs/heads/*".to_string(),
        None => default_fetch_refspec(remote),
    };
    CloneRefspec {
        configured: (!opts.bare).then(|| fetch.clone()),
        fetch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid(dir: Direction, s: &str) -> bool {
        Refspec::parse(s, dir).is_ok()
    }

    #[test]
    fn t5511_table() {
        use Direction::{Fetch, Push};
        let cases: &[(Direction, &str, bool)] = &[
            (Push, "", false),
            (Push, ":", true),
            (Push, "::", false),
            (Push, "+:", true),
            (Fetch, "", true),
            (Fetch, ":", true),
            (Fetch, "::", false),
            (Push, "refs/heads/*:refs/remotes/frotz/*", true),
            (Push, "refs/heads/*:refs/remotes/frotz", false),
            (Push, "refs/heads:refs/remotes/frotz/*", false),
            (Push, "refs/heads/main:refs/remotes/frotz/xyzzy", true),
            (Fetch, "refs/heads/*:refs/remotes/frotz/*", true),
            (Fetch, "refs/heads/*:refs/remotes/frotz", false),
            (Fetch, "refs/heads:refs/remotes/frotz/*", false),
            (Fetch, "refs/heads/main:refs/remotes/frotz/xyzzy", true),
            (Fetch, "refs/heads/main::refs/remotes/frotz/xyzzy", false),
            (Fetch, "refs/heads/maste :refs/remotes/frotz/xyzzy", false),
            (Push, "main~1:refs/remotes/frotz/backup", true),
            (Fetch, "main~1:refs/remotes/frotz/backup", false),
            (Push, "HEAD~4:refs/remotes/frotz/new", true),
            (Fetch, "HEAD~4:refs/remotes/frotz/new", false),
            (Push, "HEAD", true),
            (Fetch, "HEAD", true),
            (Push, "@", true),
            (Fetch, "@", true),
            (Push, "refs/heads/ nitfol", false),
            (Fetch, "refs/heads/ nitfol", false),
            (Push, "HEAD:", false),
            (Fetch, "HEAD:", true),
            (Push, "refs/heads/ nitfol:", false),
            (Fetch, "refs/heads/ nitfol:", false),
            (Push, ":refs/remotes/frotz/deleteme", true),
            (Fetch, ":refs/remotes/frotz/HEAD-to-me", true),
            (Push, ":refs/remotes/frotz/delete me", false),
            (Fetch, ":refs/remotes/frotz/HEAD to me", false),
            (
                Fetch,
                "refs/heads/*/for-linus:refs/remotes/mine/*-blah",
                true,
            ),
            (
                Push,
                "refs/heads/*/for-linus:refs/remotes/mine/*-blah",
                true,
            ),
            (Fetch, "refs/heads*/for-linus:refs/remotes/mine/*", true),
            (Push, "refs/heads*/for-linus:refs/remotes/mine/*", true),
            (Fetch, "refs/heads/*/*/for-linus:refs/remotes/mine/*", false),
            (Push, "refs/heads/*/*/for-linus:refs/remotes/mine/*", false),
            (Fetch, "refs/heads/*g*/for-linus:refs/remotes/mine/*", false),
            (Push, "refs/heads/*g*/for-linus:refs/remotes/mine/*", false),
            (Fetch, "refs/heads/*/for-linus:refs/remotes/mine/*", true),
            (Push, "refs/heads/*/for-linus:refs/remotes/mine/*", true),
            (Fetch, "refs/heads/\u{c4}", true),
            (Fetch, "refs/heads/\ttab", false),
        ];
        for &(dir, spec, ok) in cases {
            assert_eq!(valid(dir, spec), ok, "{dir:?} {spec:?}");
        }
    }

    #[test]
    fn fields() {
        let r = Refspec::parse("+refs/heads/*:refs/remotes/o/*", Direction::Fetch).unwrap();
        assert!(r.force && r.pattern && !r.negative);
        assert_eq!(r.map_src(b"refs/heads/a/b").unwrap(), "refs/remotes/o/a/b");
        assert_eq!(r.map_dst(b"refs/remotes/o/x").unwrap(), "refs/heads/x");
        assert_eq!(r.map_src(b"refs/tags/x"), None);
        let r = Refspec::parse("@:x", Direction::Fetch).unwrap();
        assert_eq!(r.src, "HEAD");
        let r = Refspec::parse("+:", Direction::Push).unwrap();
        assert!(r.matching && r.force);
        let r = Refspec::parse("^refs/heads/dev*", Direction::Fetch).unwrap();
        assert!(r.negative && r.pattern);
        let hex = "0123456789012345678901234567890123456789";
        assert!(Refspec::parse(hex, Direction::Fetch).unwrap().exact_oid);
        assert!(Refspec::parse(format!("^{hex}"), Direction::Fetch).is_err());
        assert_eq!(
            Refspec::parse("a:b:c", Direction::Fetch)
                .unwrap_err()
                .to_string(),
            "invalid refspec 'a:b:c'"
        );
        let r = Refspec::parse("a:b:c", Direction::Push).unwrap();
        assert_eq!((r.src.as_slice(), r.dst), (&b"a:b"[..], Some("c".into())));
    }

    #[test]
    fn star_matches_empty_and_slashes() {
        assert_eq!(
            match_pattern(b"refs/heads/a*", b"refs/heads/a"),
            Some(&b""[..])
        );
        assert_eq!(
            match_pattern(b"refs/heads/*x", b"refs/heads/a/b/x"),
            Some(&b"a/b/"[..])
        );
        assert_eq!(match_pattern(b"refs/heads/a*a", b"refs/heads/a"), None);
    }

    #[test]
    fn mapping_with_negatives() {
        let set = RefspecSet::parse(
            [
                "+refs/heads/*:refs/remotes/o/*",
                "^refs/heads/dev*",
                "refs/heads/main:refs/m",
                "^refs/heads/main",
            ],
            Direction::Fetch,
        )
        .unwrap();
        let refs = [
            "HEAD",
            "refs/heads/main",
            "refs/heads/dev-1",
            "refs/heads/x",
        ];
        let map = fetch_map(&set, &refs, false).unwrap();
        let got: Vec<_> = map.mappings.iter().map(|m| m.src.to_string()).collect();
        assert_eq!(got, ["refs/heads/x"]);
        assert_eq!(set.map_src(b"refs/heads/x").unwrap(), "refs/remotes/o/x");
        assert_eq!(set.map_src(b"refs/heads/dev-2"), None);
        assert_eq!(set.map_dst(b"refs/remotes/o/x").unwrap(), "refs/heads/x");
    }

    #[test]
    fn fetch_map_abbrev_and_errors() {
        let set = RefspecSet::parse(["main:refs/q", ":"], Direction::Fetch).unwrap();
        let refs = ["HEAD", "refs/tags/main", "refs/heads/main"];
        let map = fetch_map(&set, &refs, false).unwrap();
        assert_eq!(map.mappings[0].src, "refs/tags/main");
        assert_eq!(map.mappings[1].src, "HEAD");
        assert_eq!(map.mappings[1].dst, None);
        let set = RefspecSet::parse(["nonexist"], Direction::Fetch).unwrap();
        assert_eq!(
            fetch_map(&set, &refs, false).unwrap_err().to_string(),
            "couldn't find remote ref nonexist"
        );
        let set = RefspecSet::parse(
            ["refs/heads/*for-linus:refs/remotes/fl/*"],
            Direction::Fetch,
        )
        .unwrap();
        let map = fetch_map(&set, &["refs/heads/for-linus"], false).unwrap();
        assert!(map.mappings.is_empty());
        assert_eq!(
            map.errors,
            ["* Ignoring funny ref 'refs/remotes/fl/' locally"]
        );
    }

    #[test]
    fn duplicates() {
        let m = |s: &str, d: &str| FetchMapping {
            src: s.into(),
            dst: Some(d.into()),
            force: false,
            from_pattern: false,
        };
        let ok =
            remove_duplicate_destinations(vec![m("refs/heads/a", "x"), m("refs/heads/a", "x")])
                .unwrap();
        assert_eq!(ok.len(), 1);
        let err = remove_duplicate_destinations(vec![
            m("refs/heads/a", "refs/remotes/x"),
            m("refs/heads/ab", "refs/remotes/x"),
        ])
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "Cannot fetch both refs/heads/a and refs/heads/ab to refs/remotes/x"
        );
    }

    #[test]
    fn clone_specs() {
        let d = CloneOptions::default();
        assert_eq!(
            clone_refspec("origin", &d).configured.as_deref(),
            Some("+refs/heads/*:refs/remotes/origin/*")
        );
        let bare = CloneOptions {
            bare: true,
            ..d.clone()
        };
        assert_eq!(clone_refspec("origin", &bare).configured, None);
        assert_eq!(
            clone_refspec("origin", &bare).fetch,
            "+refs/heads/*:refs/heads/*"
        );
        let mirror = CloneOptions {
            mirror: true,
            bare: true,
            ..d.clone()
        };
        assert_eq!(
            clone_refspec("o", &mirror).configured.as_deref(),
            Some("+refs/*:refs/*")
        );
        let single = CloneOptions {
            single: Some("refs/heads/a"),
            ..d.clone()
        };
        assert_eq!(
            clone_refspec("up", &single).fetch,
            "+refs/heads/a:refs/remotes/up/a"
        );
        let tag = CloneOptions {
            single: Some("refs/tags/v1"),
            ..d
        };
        assert_eq!(clone_refspec("o", &tag).fetch, "+refs/tags/v1:refs/tags/v1");
    }
}
