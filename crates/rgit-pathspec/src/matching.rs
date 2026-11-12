// SPDX-License-Identifier: MIT

//! Matching paths against parsed pathspecs.

use crate::parse::{ImplicitBase, ParseOptions, PathspecItem, parse_item};
use crate::wildmatch::{WildmatchFlags, wildmatch};
use crate::{AttrSpec, Magic, PathspecError};

/// How a path matched a pathspec item, from weakest to strongest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MatchKind {
    /// The pathspec names a leading directory of the path (`sub` matches
    /// `sub/file`), or is empty.
    Recursive,
    /// The path matched the wildcard pattern.
    Wildcard,
    /// The pathspec equals the path.
    Exact,
}

/// Whether anything below a directory can match (for pruning tree walks).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DirInterest {
    /// Nothing below the directory matches; skip it.
    None,
    /// Some paths below may match; descend and test each.
    Maybe,
    /// Every path below matches; descend without further tests.
    All,
}

/// A parsed list of pathspecs.
///
/// ```
/// use rgit_pathspec::{ParseOptions, Pathspec};
///
/// let opts = ParseOptions { prefix: b"sub/".to_vec(), ..Default::default() };
/// let ps = Pathspec::parse(["*.c", ":!gen/*", ":/README"], &opts).unwrap();
/// assert!(ps.matches(b"sub/a.c"));
/// assert!(ps.matches(b"sub/x/b.c"));
/// assert!(!ps.matches(b"sub/gen/b.c"));
/// assert!(ps.matches(b"README"));
/// assert!(!ps.matches(b"a.c"));
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pathspec {
    items: Vec<PathspecItem>,
    magic: Magic,
}

impl Pathspec {
    /// Parses command line pathspecs.
    ///
    /// An empty list gives an empty pathspec, which matches everything; the
    /// command decides whether that means the whole tree or the current
    /// directory. If all items are excludes, an implicit positive item for
    /// [`ParseOptions::implicit_base`] is added.
    ///
    /// # Errors
    /// Any [`PathspecError`] about global settings, magic, unsupported magic
    /// or paths outside the work tree. The first failing argument wins.
    pub fn parse<I, S>(args: I, opts: &ParseOptions) -> Result<Self, PathspecError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<[u8]>,
    {
        opts.globals.validate()?;
        let mut items = Vec::new();
        for arg in args {
            let item = parse_item(arg.as_ref(), opts)?;
            check_supported(&item, opts.allowed_magic)?;
            items.push(item);
        }
        if !items.is_empty() && items.iter().all(PathspecItem::is_exclude) {
            let (pattern, prefix_len) = match opts.implicit_base {
                ImplicitBase::Cwd => (opts.prefix.clone(), opts.prefix.len()),
                ImplicitBase::Top => (Vec::new(), 0),
            };
            items.push(PathspecItem {
                original: Vec::new(),
                nowildcard_len: pattern.len(),
                pattern,
                prefix_len,
                magic: Magic::NONE,
                attr: None,
                implicit: true,
            });
        }
        let magic = items.iter().fold(Magic::NONE, |m, i| m | i.magic);
        Ok(Self { items, magic })
    }

    /// The items in command line order (plus a trailing implicit item for
    /// exclude-only pathspecs).
    #[must_use]
    pub fn items(&self) -> &[PathspecItem] {
        &self.items
    }

    /// Whether no pathspec was given (matches everything).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Number of items.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Union of the magic of all items.
    #[must_use]
    pub fn magic(&self) -> Magic {
        self.magic
    }

    /// Whether `path` (relative to the top, without trailing slash) is
    /// selected: it matches a positive item and no exclude item.
    #[must_use]
    pub fn matches(&self, path: &[u8]) -> bool {
        self.match_path(path).is_some()
    }

    /// Like [`Self::matches`], returning the strongest positive match.
    /// An empty pathspec matches everything as [`MatchKind::Recursive`].
    #[must_use]
    pub fn match_path(&self, path: &[u8]) -> Option<MatchKind> {
        self.match_impl(path, None, &mut |_, _| true)
    }

    /// Like [`Self::match_path`], also recording in `seen` (one flag per
    /// item, see [`Self::items`]) which positive items matched. Use it with
    /// [`Self::unmatched`] for "did not match any files" errors.
    ///
    /// # Panics
    /// If `seen` is shorter than [`Self::len`].
    #[must_use]
    pub fn match_path_seen(&self, path: &[u8], seen: &mut [bool]) -> Option<MatchKind> {
        self.match_impl(path, Some(seen), &mut |_, _| true)
    }

    /// Full form of [`Self::match_path_seen`] for pathspecs with
    /// `attr:` magic: `attr_check(path, spec)` decides whether the path
    /// satisfies an item's attribute requirements. Without it, items with
    /// `attr:` match as if all requirements held.
    #[must_use]
    pub fn match_path_with(
        &self,
        path: &[u8],
        seen: Option<&mut [bool]>,
        attr_check: &mut dyn FnMut(&[u8], &AttrSpec) -> bool,
    ) -> Option<MatchKind> {
        self.match_impl(path, seen, attr_check)
    }

    fn match_impl(
        &self,
        path: &[u8],
        mut seen: Option<&mut [bool]>,
        attr_check: &mut dyn FnMut(&[u8], &AttrSpec) -> bool,
    ) -> Option<MatchKind> {
        if self.items.is_empty() {
            return Some(MatchKind::Recursive);
        }
        let mut best = None;
        for (i, item) in self.items.iter().enumerate() {
            if item.is_exclude() {
                continue;
            }
            let Some(kind) = item.match_path(path) else {
                continue;
            };
            if let Some(attr) = &item.attr {
                if !attr_check(path, attr) {
                    continue;
                }
            }
            if let Some(seen) = seen.as_deref_mut() {
                seen[i] = true;
            }
            best = best.max(Some(kind));
        }
        best?;
        for item in self.items.iter().filter(|i| i.is_exclude()) {
            if item.match_path(path).is_some()
                && item.attr.as_ref().is_none_or(|a| attr_check(path, a))
            {
                return None;
            }
        }
        best
    }

    /// Items that did not match anything according to `seen`, in order.
    /// Excludes and implicit items are never reported.
    pub fn unmatched<'a>(&'a self, seen: &'a [bool]) -> impl Iterator<Item = &'a PathspecItem> {
        self.items
            .iter()
            .zip(seen)
            .filter(|(item, seen)| !**seen && !item.is_exclude() && !item.implicit)
            .map(|(item, _)| item)
    }

    /// Whether paths below the directory `dir` (relative to the top, without
    /// trailing slash) can match. Conservative: never [`DirInterest::None`]
    /// when something below could match.
    #[must_use]
    pub fn dir_interest(&self, dir: &[u8]) -> DirInterest {
        if self.items.is_empty() {
            return DirInterest::All;
        }
        let mut best = DirInterest::None;
        for item in self.items.iter().filter(|i| !i.is_exclude()) {
            let mut interest = item.dir_interest(dir);
            if item.attr.is_some() {
                interest = interest.min(DirInterest::Maybe);
            }
            best = best.max(interest);
            if best == DirInterest::All {
                break;
            }
        }
        if best == DirInterest::None {
            return best;
        }
        for item in self.items.iter().filter(|i| i.is_exclude()) {
            match item.dir_interest(dir) {
                DirInterest::All if item.attr.is_none() => return DirInterest::None,
                DirInterest::None => {}
                _ => best = best.min(DirInterest::Maybe),
            }
        }
        best
    }

    /// The longest leading directory (ending in `/`, or empty) that every
    /// positive item's literal part shares, compared case-sensitively.
    /// Paths outside it cannot match; useful to narrow index scans.
    #[must_use]
    pub fn common_prefix(&self) -> &[u8] {
        let mut common: Option<&[u8]> = None;
        for item in self.items.iter().filter(|i| !i.is_exclude()) {
            let exact_len = if item.magic.contains(Magic::ICASE) {
                item.prefix_len
            } else {
                item.nowildcard_len
            };
            let lit = &item.pattern[..exact_len];
            let lit = match lit.iter().rposition(|&b| b == b'/') {
                Some(slash) => &lit[..=slash],
                None => &[][..],
            };
            common = Some(match common {
                None => lit,
                Some(c) => {
                    let n = c.iter().zip(lit).take_while(|(a, b)| a == b).count();
                    let n = c[..n].iter().rposition(|&b| b == b'/').map_or(0, |s| s + 1);
                    &c[..n]
                }
            });
        }
        common.unwrap_or_default()
    }
}

fn check_supported(item: &PathspecItem, allowed: Magic) -> Result<(), PathspecError> {
    let bad = item.magic & !allowed;
    if bad.is_empty() {
        return Ok(());
    }
    let list = bad
        .iter()
        .map(|w| match w.mnemonic() {
            Some(c) => format!("'{}' (mnemonic: '{c}')", w.name()),
            None => format!("'{}'", w.name()),
        })
        .collect::<Vec<_>>()
        .join(", ");
    Err(PathspecError::UnsupportedMagic {
        arg: item.original.clone(),
        list,
    })
}

impl PathspecItem {
    fn icase(&self) -> bool {
        self.magic.contains(Magic::ICASE)
    }

    /// Compares `pattern[..n]` with `text[..n]`: the prefix part exactly,
    /// the rest case-insensitively for `icase`.
    fn literal_eq(&self, text: &[u8], n: usize) -> bool {
        let p = &self.pattern[..n];
        let t = &text[..n];
        if !self.icase() {
            return p == t;
        }
        let split = self.prefix_len.min(n);
        p[..split] == t[..split] && p[split..].eq_ignore_ascii_case(&t[split..])
    }

    /// Matches this item alone against `path`, ignoring exclude semantics
    /// and attributes.
    #[must_use]
    pub fn match_path(&self, path: &[u8]) -> Option<MatchKind> {
        let pat = &self.pattern;
        if pat.is_empty() {
            return Some(MatchKind::Recursive);
        }
        if path.len() >= pat.len() && self.literal_eq(path, pat.len()) {
            if path.len() == pat.len() {
                return Some(MatchKind::Exact);
            }
            if pat.ends_with(b"/") || path[pat.len()] == b'/' {
                return Some(MatchKind::Recursive);
            }
        }
        if self.has_wildcard() {
            let n = self.nowildcard_len;
            if path.len() < n || !self.literal_eq(path, n) {
                return None;
            }
            let mut flags = WildmatchFlags::NONE;
            if self.magic.contains(Magic::GLOB) {
                flags |= WildmatchFlags::PATHNAME;
            }
            if self.icase() {
                flags |= WildmatchFlags::CASEFOLD;
            }
            if wildmatch(&pat[n..], &path[n..], flags) {
                return Some(MatchKind::Wildcard);
            }
        }
        None
    }

    fn dir_interest(&self, dir: &[u8]) -> DirInterest {
        // `dir` as a path prefix, with its slash.
        let mut d = dir.to_vec();
        d.push(b'/');
        if matches!(
            self.match_path(&d),
            Some(MatchKind::Exact | MatchKind::Recursive)
        ) {
            return DirInterest::All;
        }
        let n = self.nowildcard_len;
        let common = n.min(d.len());
        if !self.literal_eq(&d, common) {
            return DirInterest::None;
        }
        if n >= d.len() || self.has_wildcard() {
            DirInterest::Maybe
        } else {
            DirInterest::None
        }
    }
}
