// SPDX-License-Identifier: MIT
//! The reference format language shared by `for-each-ref`, `tag --list`
//! and `branch --list` (Documentation/git-for-each-ref.adoc,
//! for-each-ref-options.adoc, config/versionsort.adoc).
//!
//! - [`format`]: parsing `--format` strings and sort keys into atoms;
//! - [`eval`]: the value of an atom for one reference and expanding a
//!   format (`%(if)`, `%(align)`, quoting, colours);
//! - here: sort keys and version order, and the small API the listing
//!   commands use ([`RefFormat`], [`SortKey`], [`RefValues`]).
//!
//! Not yet: `%(describe)` (needs `git describe`), `%(is-base:...)`,
//! `%(signature...)` (needs signature verification); they fail with an
//! rgit message.

pub(crate) mod eval;
pub(crate) mod format;

use std::cmp::Ordering;

use rgit_config::ConfigSet;
use rgit_hash::ObjectId;
use rgit_refs::files::FilesStore;

use crate::Die;
use crate::messages::ref_filter as msg;
use crate::refs_support::Objects;

pub(crate) use eval::{Env, ItemCache, Quote, RefItem, RepoParts, Value};
pub(crate) use format::{Atom, FormatError, Kind, Piece};

/// A parsed `--format`.
#[derive(Clone, Debug)]
pub(crate) struct RefFormat {
    pub(crate) pieces: Vec<Piece>,
    /// The last `%(color:...)` is not `reset`: colour output ends with a
    /// reset.
    pub(crate) reset_at_eol: bool,
}

impl RefFormat {
    /// Parses a format string; errors are fatal.
    pub(crate) fn parse(fmt: &str) -> Result<RefFormat, Die> {
        RefFormat::try_parse(fmt).map_err(|e| Die::Fatal(e.message()))
    }

    /// Parses a format string, telling a malformed string (a usage error
    /// in `for-each-ref`) from other errors.
    pub(crate) fn try_parse(fmt: &str) -> Result<RefFormat, FormatError> {
        let pieces = format::parse_format(fmt)?;
        let mut reset_at_eol = false;
        for p in &pieces {
            if let Piece::Atom(Atom {
                kind: Kind::Color(_),
                ..
            }) = p
            {
                reset_at_eol = !fmt_color_is_reset(p);
            }
        }
        Ok(RefFormat {
            pieces,
            reset_at_eol,
        })
    }

    /// The atoms of the format.
    pub(crate) fn atoms(&self) -> impl Iterator<Item = &Atom> {
        self.pieces.iter().filter_map(|p| match p {
            Piece::Atom(a) => Some(a),
            Piece::Lit(_) => None,
        })
    }

    /// Whether the format uses `%(rest)`, which listing commands reject.
    pub(crate) fn has_rest(&self) -> bool {
        self.atoms().any(|a| a.kind == Kind::Rest)
    }

    /// Whether the format uses `%(raw)` (not quotable for most languages).
    pub(crate) fn has_raw(&self) -> bool {
        self.atoms()
            .any(|a| matches!(a.kind, Kind::Raw { size: false }))
    }
}

fn fmt_color_is_reset(p: &Piece) -> bool {
    matches!(p, Piece::Atom(Atom { kind: Kind::Color(seq), .. }) if seq == crate::color::RESET)
}

/// One `--sort` key.
#[derive(Clone, Debug)]
pub(crate) struct SortKey {
    pub(crate) atom: Atom,
    pub(crate) reverse: bool,
    /// `version:` / `v:`.
    pub(crate) version: bool,
}

impl SortKey {
    /// `[-][version:|v:]<atom>`.
    pub(crate) fn parse(spec: &str) -> Result<SortKey, Die> {
        let (reverse, rest) = match spec.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, spec),
        };
        let (version, name) = match rest
            .strip_prefix("version:")
            .or_else(|| rest.strip_prefix("v:"))
        {
            Some(n) => (true, n),
            None => (false, rest),
        };
        let atom = format::parse_atom(name).map_err(|e| Die::Fatal(e.message()))?;
        Ok(SortKey {
            atom,
            reverse,
            version,
        })
    }
}

/// `--sort` options in command-line order: the primary key comes first in
/// the result.
pub(crate) fn parse_sort_keys(specs: &[String]) -> Result<Vec<SortKey>, Die> {
    specs.iter().rev().map(|s| SortKey::parse(s)).collect()
}

/// Sorts `items` by `keys` (ties: by refname); `values[i]` are the key
/// values of `items[i]`.
pub(crate) fn sort_items<T>(
    items: Vec<(Vec<Value>, T)>,
    keys: &[SortKey],
    icase: bool,
    suffixes: &[Vec<u8>],
    name: impl Fn(&T) -> &str,
) -> Vec<T> {
    let mut items = items;
    items.sort_by(|(va, a), (vb, b)| {
        for (k, key) in keys.iter().enumerate() {
            let o = if key.version {
                versioncmp(&va[k].bytes, &vb[k].bytes, suffixes)
            } else {
                va[k].compare(&vb[k], icase)
            };
            let o = if key.reverse { o.reverse() } else { o };
            if o != Ordering::Equal {
                return o;
            }
        }
        let (na, nb) = (name(a), name(b));
        if icase {
            na.to_ascii_lowercase()
                .cmp(&nb.to_ascii_lowercase())
                .then_with(|| na.cmp(nb))
        } else {
            na.as_bytes().cmp(nb.as_bytes())
        }
    });
    items.into_iter().map(|(_, t)| t).collect()
}

/// Expands atoms for the references of one repository.
pub(crate) struct RefValues<'a>(Env<'a>);

impl<'a> RefValues<'a> {
    pub(crate) fn new(objects: &'a Objects, store: &'a FilesStore) -> Self {
        RefValues(Env::new(objects, store))
    }

    /// Adds what `upstream`, `push`, `ahead-behind`, `worktreepath` and
    /// mailmap options need.
    pub(crate) fn with_repo(mut self, repo: RepoParts<'a>) -> Self {
        self.0.repo = Some(repo);
        self
    }

    /// Whether `%(color:...)` prints colours.
    pub(crate) fn with_color(mut self, color: bool) -> Self {
        self.0.color = color;
        self
    }

    /// Resolves what the atoms of `format` and the sort keys refer to.
    pub(crate) fn prepare<'f>(
        &mut self,
        atoms: impl IntoIterator<Item = &'f Atom>,
    ) -> Result<(), Die> {
        self.0.prepare(atoms)
    }

    fn item(name: &str, oid: ObjectId) -> RefItem {
        RefItem {
            name: name.to_owned(),
            oid,
            symref: None,
        }
    }

    /// The value of `atom` for reference `name` at `oid`.
    pub(crate) fn atom_value(&self, atom: &Atom, name: &str, oid: ObjectId) -> Result<Value, Die> {
        self.0
            .value(atom, &Self::item(name, oid), &ItemCache::default())
    }

    /// `format` expanded for reference `name` at `oid`.
    pub(crate) fn render(
        &self,
        format: &RefFormat,
        name: &str,
        oid: ObjectId,
    ) -> Result<Vec<u8>, Die> {
        self.0.render(
            &format.pieces,
            &Self::item(name, oid),
            &ItemCache::default(),
            format.reset_at_eol,
        )
    }
}

/// `versionsort.suffix`, else `versionsort.prereleaseSuffix`, in
/// configuration order. Entries without a value are reported.
pub(crate) fn version_suffixes(config: &ConfigSet) -> Vec<Vec<u8>> {
    let get = |key: &str| {
        let mut out = Vec::new();
        for e in config.entries().iter().filter(|e| e.name == key) {
            match &e.value {
                Some(v) => out.push(v.to_vec()),
                None => eprintln!("error: {}", msg::missing_value(key)),
            }
        }
        out
    };
    let suffix = get("versionsort.suffix");
    let pre = get("versionsort.prereleasesuffix");
    if suffix.is_empty() { pre } else { suffix }
}

/// The configured suffix that covers position `off` of `s`: the one
/// starting earliest, the longest of those. Returns its index.
fn suffix_at(s: &[u8], off: usize, suffixes: &[Vec<u8>]) -> Option<usize> {
    let mut best: Option<(usize, usize, usize)> = None;
    for (i, suf) in suffixes.iter().enumerate() {
        let lo = off.saturating_sub(suf.len());
        let start = (lo..=off.min(s.len())).find(|&p| s[p..].starts_with(suf));
        if let Some(p) = start {
            let better = match best {
                None => true,
                Some((bp, bl, _)) => p < bp || (p == bp && suf.len() > bl),
            };
            if better {
                best = Some((p, suf.len(), i));
            }
        }
    }
    best.map(|(_, _, i)| i)
}

/// Version order (`version:refname`): digit runs compare as numbers,
/// configured suffixes order the releases with the same base.
pub(crate) fn versioncmp(a: &[u8], b: &[u8], suffixes: &[Vec<u8>]) -> Ordering {
    let off = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    if off == a.len() && off == b.len() {
        return Ordering::Equal;
    }
    if !suffixes.is_empty() {
        match (suffix_at(a, off, suffixes), suffix_at(b, off, suffixes)) {
            (Some(x), Some(y)) if x != y => return x.cmp(&y),
            (Some(_), None) => return Ordering::Less,
            (None, Some(_)) => return Ordering::Greater,
            _ => {}
        }
    }
    natural_cmp(a, b, off)
}

/// Digit runs compare by value; a run with leading zeros counts as a
/// fraction (`000 < 00 < 01 < 010 < 09 < 0 < 1 < 9 < 10`). `off` is the
/// first differing position.
fn natural_cmp(a: &[u8], b: &[u8], off: usize) -> Ordering {
    let digit = |s: &[u8], i: usize| s.get(i).is_some_and(u8::is_ascii_digit);
    let mut start = off;
    while start > 0 && a[start - 1].is_ascii_digit() {
        start -= 1;
    }
    let (da, db) = (digit(a, off), digit(b, off));
    let in_run = start < off;
    if !in_run && !(da && db) {
        return a.get(off).cmp(&b.get(off));
    }
    if in_run && !da && !db {
        return a.get(off).cmp(&b.get(off));
    }
    let run = |s: &[u8]| {
        let mut e = start;
        while digit(s, e) {
            e += 1;
        }
        e
    };
    let (ea, eb) = (run(a), run(b));
    let (ra, rb) = (&a[start..ea], &b[start..eb]);
    // A run of several digits starting with 0 is a fraction; fractions
    // sort before whole numbers.
    let fraction = |r: &[u8]| r.len() > 1 && r[0] == b'0';
    let order = match (fraction(ra), fraction(rb)) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (true, true) => {
            let zeros = |r: &[u8]| r.iter().take_while(|&&c| c == b'0').count();
            let (za, zb) = (zeros(ra), zeros(rb));
            // More leading zeros: smaller.
            zb.cmp(&za).then_with(|| ra[za..].cmp(&rb[zb..]))
        }
        (false, false) => ra.len().cmp(&rb.len()).then_with(|| ra.cmp(rb)),
    };
    order.then_with(|| a[ea..].cmp(&b[eb..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_order() {
        let mut v = vec!["foo1.10", "foo1.3", "foo1.6"];
        v.sort_by(|a, b| versioncmp(a.as_bytes(), b.as_bytes(), &[]));
        assert_eq!(v, ["foo1.3", "foo1.6", "foo1.10"]);
    }

    #[test]
    fn color_reset_at_eol() {
        assert!(RefFormat::parse("%(color:red)x").unwrap().reset_at_eol);
        assert!(
            !RefFormat::parse("%(color:red)x%(color:reset)")
                .unwrap()
                .reset_at_eol
        );
        assert!(!RefFormat::parse("x").unwrap().reset_at_eol);
    }

    #[test]
    fn sort_key_prefixes() {
        let k = SortKey::parse("-version:refname").unwrap();
        assert!(k.reverse && k.version);
        assert!(SortKey::parse("bogus").is_err());
    }
}
