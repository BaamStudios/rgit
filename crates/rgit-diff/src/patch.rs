// SPDX-License-Identifier: MIT

//! Per-file patch output: the `diff --git` header block followed by the
//! hunks (or the "Binary files ... differ" line).
//!
//! Format as described in git's `diff-generate-patch` documentation.

use crate::{DiffOptions, Stats, diff_text, is_binary, messages};
use rgit_hash::ObjectId;
use std::io::Write;

/// Mode of a symbolic link.
pub const MODE_SYMLINK: u32 = 0o120000;

/// One side of a file pair.
#[derive(Debug, Clone, Copy)]
pub struct PatchFile<'a> {
    /// Path relative to the repository root (raw bytes, `/` separated).
    pub path: &'a [u8],
    /// File mode, e.g. `0o100644`, `0o100755` or [`MODE_SYMLINK`].
    pub mode: u32,
    /// Object id of the contents.
    pub oid: ObjectId,
    /// The contents (for a symlink, the link target).
    pub data: &'a [u8],
}

/// Options for [`write_file_patch`].
#[derive(Debug, Clone)]
pub struct PatchOptions {
    pub diff: DiffOptions,
    /// Number of hex digits of the object ids on the `index` line.
    pub abbrev: usize,
    /// Quote paths with bytes >= 0x80 (`core.quotePath`, default true).
    pub quote_path: bool,
    /// Prefix of the old path (`a/`).
    pub src_prefix: Vec<u8>,
    /// Prefix of the new path (`b/`).
    pub dst_prefix: Vec<u8>,
}

impl Default for PatchOptions {
    fn default() -> Self {
        PatchOptions {
            diff: DiffOptions::default(),
            abbrev: 7,
            quote_path: true,
            src_prefix: b"a/".to_vec(),
            dst_prefix: b"b/".to_vec(),
        }
    }
}

/// Returns true if `name` must be written in C-style quotes.
fn needs_quote(name: &[u8], quote_high: bool) -> bool {
    name.iter()
        .any(|&c| c < 0x20 || c == b'"' || c == b'\\' || c == 0x7f || (quote_high && c >= 0x80))
}

/// Appends `name` to `out`, quoted C-style if needed.
pub fn quote_path(out: &mut Vec<u8>, name: &[u8], quote_high: bool) {
    if !needs_quote(name, quote_high) {
        out.extend_from_slice(name);
        return;
    }
    out.push(b'"');
    for &c in name {
        match c {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            0x07 => out.extend_from_slice(b"\\a"),
            0x08 => out.extend_from_slice(b"\\b"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\n' => out.extend_from_slice(b"\\n"),
            0x0b => out.extend_from_slice(b"\\v"),
            0x0c => out.extend_from_slice(b"\\f"),
            b'\r' => out.extend_from_slice(b"\\r"),
            c if c < 0x20 || c == 0x7f || (quote_high && c >= 0x80) => {
                write!(out, "\\{c:03o}").unwrap();
            }
            c => out.push(c),
        }
    }
    out.push(b'"');
}

/// `prefix` + `path`, quoted as a whole if needed.
fn prefixed(prefix: &[u8], path: &[u8], quote_high: bool) -> Vec<u8> {
    let mut full = prefix.to_vec();
    full.extend_from_slice(path);
    let mut out = Vec::new();
    quote_path(&mut out, &full, quote_high);
    out
}

/// Name on a `---`/`+++` line: a name containing a space gets a trailing
/// tab so that `patch` can tell where it ends.
fn file_line(out: &mut Vec<u8>, marker: &[u8], name: &[u8]) {
    out.extend_from_slice(marker);
    out.extend_from_slice(name);
    if name.contains(&b' ') {
        out.push(b'\t');
    }
    out.push(b'\n');
}

/// Writes the patch for one file pair. `None` stands for a missing side
/// (file added or deleted). Both sides must be of the same type (regular
/// file or symlink); a type change is shown as a deletion plus an addition.
///
/// Returns the insertion and deletion counts (zero for binary files).
pub fn write_file_patch(
    out: &mut Vec<u8>,
    old: Option<&PatchFile<'_>>,
    new: Option<&PatchFile<'_>>,
    opts: &PatchOptions,
) -> Stats {
    let abbrev = |id: &ObjectId| id.to_hex_with_len(opts.abbrev);
    write_file_patch_with(out, old, new, opts, &abbrev)
}

/// Like [`write_file_patch`], with the object ids on the `index` line
/// rendered by `abbrev` (for abbreviations that are unique in a
/// repository, or full ids).
pub fn write_file_patch_with(
    out: &mut Vec<u8>,
    old: Option<&PatchFile<'_>>,
    new: Option<&PatchFile<'_>>,
    opts: &PatchOptions,
    abbrev: &dyn Fn(&ObjectId) -> String,
) -> Stats {
    write_file_patch_extra(out, old, new, opts, abbrev, &PatchExtra::default())
}

/// What diffcore found out about a file pair, for the extended headers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PatchExtra {
    /// A rename (`false`) or copy (`true`) with its similarity in percent.
    pub rename: Option<(bool, u32)>,
    /// A complete rewrite with its dissimilarity in percent: the whole old
    /// contents are shown as removed and the whole new contents as added.
    pub rewrite: Option<u32>,
    /// `-D`: leave out the old contents of deletions (and of rewrites).
    pub irreversible_delete: bool,
}

/// Lines of a complete rewrite: `-` lines of the old, then `+` lines of
/// the new contents under one hunk header.
fn write_rewrite(out: &mut Vec<u8>, old: &[u8], new: &[u8], irreversible: bool) -> Stats {
    let la = crate::split_lines(old);
    let lb = crate::split_lines(new);
    let range = |n: usize| match n {
        0 => "0,0".to_string(),
        1 => "1".to_string(),
        n => format!("1,{n}"),
    };
    let old_range = if irreversible {
        "?,?".to_string()
    } else {
        range(la.len())
    };
    writeln!(out, "@@ -{old_range} +{} @@", range(lb.len())).unwrap();
    let mut lines = |sign: u8, lines: &[&[u8]]| {
        for l in lines {
            out.push(sign);
            out.extend_from_slice(l);
            if !l.ends_with(b"\n") {
                out.push(b'\n');
                out.extend_from_slice(messages::NO_NEWLINE_AT_EOF.as_bytes());
                out.push(b'\n');
            }
        }
    };
    if !irreversible {
        lines(b'-', &la);
    }
    lines(b'+', &lb);
    Stats {
        insertions: lb.len(),
        deletions: la.len(),
    }
}

/// Like [`write_file_patch_with`], with rename, copy and rewrite headers
/// and bodies.
pub fn write_file_patch_extra(
    out: &mut Vec<u8>,
    old: Option<&PatchFile<'_>>,
    new: Option<&PatchFile<'_>>,
    opts: &PatchOptions,
    abbrev: &dyn Fn(&ObjectId) -> String,
    extra: &PatchExtra,
) -> Stats {
    let q = opts.quote_path;
    let (old_path, new_path) = match (old, new) {
        (Some(o), Some(n)) => (o.path, n.path),
        (Some(o), None) => (o.path, o.path),
        (None, Some(n)) => (n.path, n.path),
        (None, None) => return Stats::default(),
    };
    let a_name = prefixed(&opts.src_prefix, old_path, q);
    let b_name = prefixed(&opts.dst_prefix, new_path, q);
    out.extend_from_slice(b"diff --git ");
    out.extend_from_slice(&a_name);
    out.push(b' ');
    out.extend_from_slice(&b_name);
    out.push(b'\n');

    let empty: &[u8] = b"";
    let old_data = old.map_or(empty, |f| f.data);
    let new_data = new.map_or(empty, |f| f.data);
    let content_changed = match (old, new) {
        (Some(o), Some(n)) => o.oid != n.oid,
        _ => true,
    };
    match (old, new) {
        (None, Some(n)) => {
            writeln!(out, "new file mode {:06o}", n.mode).unwrap();
        }
        (Some(o), None) => {
            writeln!(out, "deleted file mode {:06o}", o.mode).unwrap();
        }
        (Some(o), Some(n)) if o.mode != n.mode => {
            writeln!(out, "old mode {:06o}", o.mode).unwrap();
            writeln!(out, "new mode {:06o}", n.mode).unwrap();
        }
        _ => {}
    }
    if let Some((copy, pct)) = extra.rename {
        let word: &[u8] = if copy { b"copy" } else { b"rename" };
        writeln!(out, "similarity index {pct}%").unwrap();
        for (dir, path) in [(&b"from"[..], old_path), (&b"to"[..], new_path)] {
            out.extend_from_slice(word);
            out.push(b' ');
            out.extend_from_slice(dir);
            out.push(b' ');
            quote_path(out, path, q);
            out.push(b'\n');
        }
    } else if let Some(pct) = extra.rewrite {
        writeln!(out, "dissimilarity index {pct}%").unwrap();
    }
    if !content_changed {
        return Stats::default();
    }
    let kind = old.or(new).map(|f| f.oid.kind()).unwrap();
    let null = ObjectId::null(kind);
    let oa = old.map_or(null, |f| f.oid);
    let ob = new.map_or(null, |f| f.oid);
    write!(out, "index {}..{}", abbrev(&oa), abbrev(&ob)).unwrap();
    if let (Some(o), Some(n)) = (old, new) {
        if o.mode == n.mode {
            write!(out, " {:06o}", o.mode).unwrap();
        }
    }
    out.push(b'\n');

    let a_shown = if old.is_some() {
        a_name.clone()
    } else {
        b"/dev/null".to_vec()
    };
    let b_shown = if new.is_some() {
        b_name.clone()
    } else {
        b"/dev/null".to_vec()
    };
    if new.is_none() && extra.irreversible_delete {
        return Stats::default();
    }
    if is_binary(old_data) || is_binary(new_data) {
        messages::binary_files_differ(out, &a_shown, &b_shown);
        return Stats::default();
    }
    if extra.rewrite.is_some() {
        file_line(out, b"--- ", &a_shown);
        file_line(out, b"+++ ", &b_shown);
        return write_rewrite(out, old_data, new_data, extra.irreversible_delete);
    }
    let d = diff_text(old_data, new_data, &opts.diff);
    if d.hunks.is_empty() {
        return d.stats;
    }
    file_line(out, b"--- ", &a_shown);
    file_line(out, b"+++ ", &b_shown);
    out.extend_from_slice(&d.hunks);
    d.stats
}
