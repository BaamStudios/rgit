// SPDX-License-Identifier: MIT

//! Parsing pathspec arguments: magic, prefix and normalisation.

use crate::attr::AttrSpec;
use crate::wildmatch::first_glob_special;
use crate::{GlobalFlags, Magic, MagicWord, PathspecError};

/// What an exclude-only pathspec (`:!foo` without any positive pathspec)
/// subtracts from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ImplicitBase {
    /// Everything below the current directory (`ls-files`, `archive`).
    #[default]
    Cwd,
    /// The whole work tree (`add`, `commit`, `reset`, `stash`, ...).
    Top,
}

/// Settings for [`crate::Pathspec::parse`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseOptions {
    /// Current directory relative to the top of the work tree: empty at the
    /// top, otherwise with a trailing slash (`"sub/dir/"`).
    pub prefix: Vec<u8>,
    /// Absolute path of the work tree. Absolute pathspecs below it are made
    /// relative; it also appears in "outside repository" messages.
    pub root: Option<Vec<u8>>,
    /// Global switches, usually [`GlobalFlags::from_env`].
    pub globals: GlobalFlags,
    /// Magic the command supports. Anything else is
    /// [`PathspecError::UnsupportedMagic`].
    pub allowed_magic: Magic,
    /// Base for exclude-only pathspecs.
    pub implicit_base: ImplicitBase,
    /// Resolve symbolic links in the directory part of absolute pathspecs
    /// through the file system before comparing them with
    /// [`Self::root`] (which should itself be canonical), as git does.
    pub resolve_absolute: bool,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            prefix: Vec::new(),
            root: None,
            globals: GlobalFlags::default(),
            allowed_magic: Magic::ALL,
            implicit_base: ImplicitBase::Cwd,
            resolve_absolute: true,
        }
    }
}

/// One parsed pathspec.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PathspecItem {
    pub(crate) original: Vec<u8>,
    pub(crate) pattern: Vec<u8>,
    pub(crate) prefix_len: usize,
    pub(crate) nowildcard_len: usize,
    pub(crate) magic: Magic,
    pub(crate) attr: Option<AttrSpec>,
    pub(crate) implicit: bool,
}

impl PathspecItem {
    /// The argument as given on the command line. Use it in messages such as
    /// [`crate::messages::did_not_match_any_files`].
    #[must_use]
    pub fn original(&self) -> &[u8] {
        &self.original
    }

    /// The pattern relative to the top of the work tree, normalised (git's
    /// "match" string). May end in `/`.
    #[must_use]
    pub fn pattern(&self) -> &[u8] {
        &self.pattern
    }

    /// Number of leading bytes of [`Self::pattern`] that came from the
    /// current directory. They are always matched literally and
    /// case-sensitively.
    #[must_use]
    pub fn prefix_len(&self) -> usize {
        self.prefix_len
    }

    /// Number of leading bytes of [`Self::pattern`] that contain no
    /// wildcard. Equal to the pattern length for literal pathspecs.
    #[must_use]
    pub fn nowildcard_len(&self) -> usize {
        self.nowildcard_len
    }

    /// The effective magic (including global settings).
    #[must_use]
    pub fn magic(&self) -> Magic {
        self.magic
    }

    /// The `attr:` requirements, if any.
    #[must_use]
    pub fn attr(&self) -> Option<&AttrSpec> {
        self.attr.as_ref()
    }

    /// Whether this is an exclude pathspec.
    #[must_use]
    pub fn is_exclude(&self) -> bool {
        self.magic.contains(Magic::EXCLUDE)
    }

    /// Whether the pattern has wildcards that are used.
    #[must_use]
    pub fn has_wildcard(&self) -> bool {
        self.nowildcard_len < self.pattern.len()
    }

    /// Whether the parser added this item for an exclude-only pathspec. Such
    /// items are never reported as unmatched.
    #[must_use]
    pub fn is_implicit(&self) -> bool {
        self.implicit
    }

    /// The long-form magic string for passing this pathspec to another git
    /// process, e.g. `:(icase,prefix:4)sub/foo`. The receiver must run at the
    /// top of the work tree.
    #[must_use]
    pub fn to_magic_string(&self) -> Vec<u8> {
        let mut out = b":(".to_vec();
        for word in self.magic.iter() {
            if word == MagicWord::Attr {
                continue;
            }
            out.extend_from_slice(word.name().as_bytes());
            out.push(b',');
        }
        if let Some(attr) = &self.attr {
            out.extend_from_slice(b"attr:");
            out.extend_from_slice(&attr.raw);
            out.push(b',');
        }
        out.extend_from_slice(format!("prefix:{})", self.prefix_len).as_bytes());
        out.extend_from_slice(&self.pattern);
        out
    }
}

/// Parses one argument.
pub(crate) fn parse_item(arg: &[u8], opts: &ParseOptions) -> Result<PathspecItem, PathspecError> {
    if arg.is_empty() {
        return Err(PathspecError::EmptyString);
    }
    let (mut magic, elt, attr, prefix_magic) = if opts.globals.literal {
        (Magic::LITERAL, arg, None, None)
    } else {
        parse_magic(arg)?
    };
    magic = opts.globals.apply(magic);
    if magic.contains(Magic::LITERAL) && magic.contains(Magic::GLOB) {
        return Err(PathspecError::LiteralAndGlob { arg: arg.to_vec() });
    }

    let (pattern, prefix_len) = if let Some(n) = prefix_magic {
        if n > elt.len() {
            return Err(PathspecError::PrefixTooLong {
                arg: arg.to_vec(),
                len: n,
            });
        }
        (elt.to_vec(), n)
    } else if magic.contains(Magic::TOP) {
        (elt.to_vec(), 0)
    } else {
        resolve(arg, elt, opts)?
    };

    let nowildcard_len = if magic.contains(Magic::LITERAL) {
        pattern.len()
    } else {
        first_glob_special(&pattern[prefix_len..]).map_or(pattern.len(), |i| prefix_len + i)
    };

    Ok(PathspecItem {
        original: arg.to_vec(),
        pattern,
        prefix_len,
        nowildcard_len,
        magic,
        attr,
        implicit: false,
    })
}

type Parsed<'a> = (Magic, &'a [u8], Option<AttrSpec>, Option<usize>);

fn parse_magic(arg: &[u8]) -> Result<Parsed<'_>, PathspecError> {
    if arg.first() != Some(&b':') {
        return Ok((Magic::NONE, arg, None, None));
    }
    if arg.get(1) == Some(&b'(') {
        parse_long_magic(arg)
    } else {
        parse_short_magic(arg)
    }
}

fn parse_long_magic(arg: &[u8]) -> Result<Parsed<'_>, PathspecError> {
    let mut magic = Magic::NONE;
    let mut attr = None;
    let mut prefix = None;
    let mut pos = 2;
    loop {
        // Find the end of this word: an unescaped ',' or ')'.
        let start = pos;
        let mut end = None;
        while pos < arg.len() {
            match arg[pos] {
                b'\\' => pos += 2,
                b',' | b')' => {
                    end = Some(pos);
                    break;
                }
                _ => pos += 1,
            }
        }
        let Some(end) = end else {
            return Err(PathspecError::MissingParen { arg: arg.to_vec() });
        };
        let word = &arg[start..end];
        if !word.is_empty() {
            if let Some(w) = MagicWord::from_name(word) {
                if w != MagicWord::Attr {
                    magic |= w.flag();
                }
            } else if let Some(n) = word.strip_prefix(b"prefix:") {
                prefix = Some(parse_prefix_len(n)?);
            } else if let Some(spec) = word.strip_prefix(b"attr:") {
                if attr.is_some() {
                    return Err(PathspecError::MultipleAttr);
                }
                attr = Some(AttrSpec::parse(spec)?);
                magic |= Magic::ATTR;
            } else {
                return Err(PathspecError::InvalidMagic {
                    word: word.to_vec(),
                    arg: arg.to_vec(),
                });
            }
        }
        pos = end + 1;
        if arg[end] == b')' {
            return Ok((magic, &arg[pos..], attr, prefix));
        }
    }
}

/// `strtol`-like: optional sign, decimal digits, nothing else.
fn parse_prefix_len(n: &[u8]) -> Result<usize, PathspecError> {
    let digits = n.strip_prefix(b"+").unwrap_or(n);
    if !digits.iter().all(u8::is_ascii_digit) {
        return Err(PathspecError::InvalidPrefixParameter);
    }
    if digits.is_empty() {
        return Ok(0);
    }
    std::str::from_utf8(digits)
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or(PathspecError::InvalidPrefixParameter)
}

/// Characters that end the short magic signature without being magic: the
/// glob and regex specials.
fn ends_short_magic(c: u8) -> bool {
    !c.is_ascii_punctuation() || b"$()*+.?[\\]{|}".contains(&c)
}

fn parse_short_magic(arg: &[u8]) -> Result<Parsed<'_>, PathspecError> {
    let mut magic = Magic::NONE;
    let mut pos = 1;
    while let Some(&c) = arg.get(pos) {
        match c {
            b':' => {
                pos += 1;
                break;
            }
            b'/' => magic |= Magic::TOP,
            b'!' | b'^' => magic |= Magic::EXCLUDE,
            _ if ends_short_magic(c) => break,
            _ => {
                return Err(PathspecError::UnimplementedMagic {
                    ch: c,
                    arg: arg.to_vec(),
                });
            }
        }
        pos += 1;
    }
    Ok((magic, &arg[pos..], None, None))
}

/// Makes `elt` relative to the top of the work tree: absolute paths are
/// stripped of the root, relative ones get the prefix, then `.`, `..` and
/// repeated slashes are resolved. Returns the pattern and how many leading
/// bytes still come from the prefix.
fn resolve(arg: &[u8], elt: &[u8], opts: &ParseOptions) -> Result<(Vec<u8>, usize), PathspecError> {
    let outside = || PathspecError::OutsideRepository {
        arg: arg.to_vec(),
        path: elt.to_vec(),
        root: opts.root.clone().unwrap_or_default(),
    };

    if elt.first() == Some(&b'/') {
        let real;
        let abs = if opts.resolve_absolute {
            real = realpath_parent(elt)?;
            &real
        } else {
            elt
        };
        let (abs, _) = normalize(&[], abs).ok_or_else(outside)?;
        let root = opts.root.as_deref().ok_or_else(outside)?;
        let (root, _) = normalize(&[], root).ok_or_else(outside)?;
        let root = root.strip_suffix(b"/").unwrap_or(&root);
        let rest = abs.strip_prefix(root).ok_or_else(outside)?;
        return match rest {
            [] => Ok((Vec::new(), 0)),
            [b'/', tail @ ..] => Ok((tail.to_vec(), 0)),
            _ => Err(outside()),
        };
    }
    normalize(&opts.prefix, elt).ok_or_else(outside)
}

/// Resolves the directory part of the absolute path `abs` component by
/// component (following symbolic links, applying `..` to the resolved
/// path) and appends the last component unchanged.
fn realpath_parent(abs: &[u8]) -> Result<Vec<u8>, PathspecError> {
    let comps: Vec<&[u8]> = abs.split(|&b| b == b'/').skip(1).collect();
    let (last, dirs) = comps
        .split_last()
        .map_or((&[][..], &[][..]), |(l, d)| (*l, d));
    let mut acc = std::path::PathBuf::from("/");
    for comp in dirs.iter().filter(|c| !c.is_empty() && **c != b".") {
        if *comp == b".." {
            acc.pop();
            continue;
        }
        acc.push(bytes_to_path(comp));
        acc = std::fs::canonicalize(&acc).map_err(|e| {
            let reason = e.to_string();
            // Rust appends " (os error N)" to the C library text.
            let reason = match reason.rfind(" (os error ") {
                Some(i) => reason[..i].to_owned(),
                None => reason,
            };
            PathspecError::InvalidPath {
                path: path_to_bytes(&acc),
                reason,
            }
        })?;
    }
    let mut out = path_to_bytes(&acc);
    if out.last() != Some(&b'/') {
        out.push(b'/');
    }
    out.extend_from_slice(last);
    Ok(out)
}

#[cfg(unix)]
fn bytes_to_path(b: &[u8]) -> std::path::PathBuf {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::OsStr::from_bytes(b).into()
}

#[cfg(not(unix))]
fn bytes_to_path(b: &[u8]) -> std::path::PathBuf {
    String::from_utf8_lossy(b).into_owned().into()
}

#[cfg(unix)]
fn path_to_bytes(p: &std::path::Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    p.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn path_to_bytes(p: &std::path::Path) -> Vec<u8> {
    p.to_string_lossy().replace('\\', "/").into_bytes()
}

/// Joins `prefix` and `path` and resolves `.`, `..` and empty components.
/// Returns `None` if `..` leaves the top. The result keeps a trailing slash
/// when `path` ends in `/`, `.` or `..` and is not empty.
fn normalize(prefix: &[u8], path: &[u8]) -> Option<(Vec<u8>, usize)> {
    let absolute = path.first() == Some(&b'/');
    // (component, from_prefix)
    let mut stack: Vec<(&[u8], bool)> = prefix
        .split(|&b| b == b'/')
        .filter(|c| !c.is_empty())
        .map(|c| (c, true))
        .collect();
    let mut trailing = false;
    for comp in path.split(|&b| b == b'/') {
        match comp {
            b"" => {
                trailing = true;
                continue;
            }
            b"." => trailing = true,
            b".." => {
                stack.pop()?;
                trailing = true;
            }
            _ => {
                stack.push((comp, false));
                trailing = false;
            }
        }
    }
    if path.is_empty() {
        trailing = true;
    }
    let mut out = Vec::new();
    if absolute {
        out.push(b'/');
    }
    let mut prefix_len = 0;
    for (i, (comp, from_prefix)) in stack.iter().enumerate() {
        if i > 0 {
            out.push(b'/');
        }
        out.extend_from_slice(comp);
        if *from_prefix {
            prefix_len = out.len() + 1;
        }
    }
    if trailing && !stack.is_empty() {
        out.push(b'/');
    }
    Some((out.clone(), prefix_len.min(out.len())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(prefix: &str, path: &str) -> Option<(String, usize)> {
        normalize(prefix.as_bytes(), path.as_bytes())
            .map(|(p, n)| (String::from_utf8(p).unwrap(), n))
    }

    #[test]
    fn normalisation() {
        assert_eq!(norm("", "."), Some((String::new(), 0)));
        assert_eq!(norm("", "a//b"), Some(("a/b".into(), 0)));
        assert_eq!(norm("", "sub/deep/.."), Some(("sub/".into(), 0)));
        assert_eq!(norm("sub/", "."), Some(("sub/".into(), 4)));
        assert_eq!(norm("sub/", "x"), Some(("sub/x".into(), 4)));
        assert_eq!(norm("fOo/", "../foo/bar"), Some(("foo/bar".into(), 0)));
        assert_eq!(norm("a/b/", "../c"), Some(("a/c".into(), 2)));
        assert_eq!(norm("sub/", "../.."), None);
        assert_eq!(norm("", ""), Some((String::new(), 0)));
        assert_eq!(norm("sub/", ""), Some(("sub/".into(), 4)));
    }
}
