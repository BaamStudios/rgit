// SPDX-License-Identifier: MIT
//! `--stat`, `--numstat` and `--shortstat` (Documentation/diff-options.adoc).
//!
//! The layout of `--stat` was derived from the documentation and from
//! differential tests against the `git` binary: the line is
//! ` <name> | <count> <graph>`; when it does not fit into the width, the
//! graph shrinks first (down to 3/8 of the width minus the fixed columns),
//! then names are cut at the front to `...` plus a trailing part starting
//! at a `/`.

use std::io::Write;

use crate::messages::diff as msg;

/// One line of the stat.
pub struct FileStat {
    /// The name as shown (quoted).
    pub name: Vec<u8>,
    pub kind: StatKind,
}

pub enum StatKind {
    Unmerged,
    Binary { old_size: usize, new_size: usize },
    Text { added: usize, deleted: usize },
}

/// Width settings.
#[derive(Debug, Clone, Copy)]
pub struct StatLayout {
    pub width: usize,
    pub name_width: Option<usize>,
    pub graph_width: Option<usize>,
    pub count: Option<usize>,
}

fn display_width(name: &[u8]) -> usize {
    String::from_utf8_lossy(name).chars().count()
}

/// `count` scaled from `0..=max` to `0..=width`: at least one column for
/// a non-zero count.
fn scale(count: usize, width: usize, max: usize) -> usize {
    if count == 0 || max == 0 {
        return 0;
    }
    1 + count * width.saturating_sub(1) / max
}

/// Totals over all files: (files, insertions, deletions).
pub fn totals(files: &[FileStat]) -> (usize, usize, usize) {
    let mut t = (0, 0, 0);
    for f in files {
        match f.kind {
            StatKind::Unmerged => {}
            StatKind::Binary { .. } => t.0 += 1,
            StatKind::Text { added, deleted } => {
                t.0 += 1;
                t.1 += added;
                t.2 += deleted;
            }
        }
    }
    t
}

pub fn write_summary_line(out: &mut Vec<u8>, files: &[FileStat]) {
    let (n, ins, del) = totals(files);
    out.extend_from_slice(msg::stat_summary(n, ins, del).as_bytes());
    out.push(b'\n');
}

/// The cut-down name for a column of `width`.
fn truncate(name: &[u8], width: usize) -> Vec<u8> {
    let len = display_width(name);
    if len <= width {
        return name.to_vec();
    }
    let chars: Vec<char> = String::from_utf8_lossy(name).chars().collect();
    let keep = width.saturating_sub(3);
    let mut tail: &[char] = &chars[chars.len() - keep..];
    if let Some(p) = tail.iter().position(|&c| c == '/') {
        tail = &tail[p..];
    }
    let mut s = String::from("...");
    s.extend(tail.iter());
    s.into_bytes()
}

/// Colours of the stat graph: (added, removed), from `color.diff.new`
/// and `color.diff.old`.
pub type StatColors<'a> = Option<(&'a str, &'a str)>;

fn painted(out: &mut Vec<u8>, color: Option<&str>, text: &[u8]) {
    match color {
        Some(c) => {
            out.extend_from_slice(c.as_bytes());
            out.extend_from_slice(text);
            out.extend_from_slice(crate::color::RESET.as_bytes());
        }
        None => out.extend_from_slice(text),
    }
}

pub fn write_stat(
    out: &mut Vec<u8>,
    files: &[FileStat],
    layout: &StatLayout,
    colors: StatColors<'_>,
) {
    let (new_color, old_color) = (colors.map(|c| c.0), colors.map(|c| c.1));
    if files.is_empty() {
        return;
    }
    let shown = &files[..layout.count.unwrap_or(files.len()).min(files.len())];
    let mut max_len = 0;
    let mut max_change = 0;
    let mut number_width = 0;
    for f in shown {
        max_len = max_len.max(display_width(&f.name));
        match f.kind {
            StatKind::Text { added, deleted } => max_change = max_change.max(added + deleted),
            StatKind::Binary { .. } => number_width = 3,
            StatKind::Unmerged => {}
        }
    }
    number_width = number_width.max(max_change.to_string().len());
    let width = layout.width;
    let mut name_width = layout.name_width.map_or(max_len, |w| w.min(max_len));
    let mut graph_width = layout.graph_width.map_or(max_change, |w| w.min(max_change));
    let fixed = number_width + 6;
    if name_width + graph_width + fixed > width {
        let min_graph = (width * 3 / 8).saturating_sub(fixed);
        if graph_width > min_graph {
            graph_width = min_graph.max(width.saturating_sub(fixed + name_width));
        }
        name_width = name_width.min(width.saturating_sub(fixed + graph_width));
    }
    for f in shown {
        let name = truncate(&f.name, name_width);
        out.push(b' ');
        out.extend_from_slice(&name);
        for _ in display_width(&name)..name_width {
            out.push(b' ');
        }
        out.extend_from_slice(b" | ");
        match f.kind {
            StatKind::Unmerged => out.extend_from_slice(msg::UNMERGED_STAT.as_bytes()),
            StatKind::Binary { old_size, new_size } => {
                write!(out, "{:>number_width$}", "Bin").unwrap();
                if colors.is_some() {
                    let (old, new) = (old_size.to_string(), new_size.to_string());
                    let text = msg::binary_stat(old_size, new_size);
                    // "Bin <old> -> <new> bytes" with the sizes coloured.
                    let after_old = 4 + old.len();
                    let new_at = text[after_old..]
                        .find(&new)
                        .map_or(after_old, |i| i + after_old);
                    out.extend_from_slice(&text.as_bytes()[3..4]);
                    painted(out, old_color, old.as_bytes());
                    out.extend_from_slice(&text.as_bytes()[after_old..new_at]);
                    painted(out, new_color, new.as_bytes());
                    out.extend_from_slice(&text.as_bytes()[new_at + new.len()..]);
                } else {
                    write!(out, "{}", &msg::binary_stat(old_size, new_size)[3..]).unwrap();
                }
            }
            StatKind::Text { added, deleted } => {
                write!(out, "{:>number_width$}", added + deleted).unwrap();
                let (plus, minus) = bar_lengths(added, deleted, graph_width, max_change);
                if plus + minus > 0 {
                    out.push(b' ');
                }
                if plus > 0 {
                    painted(out, new_color, &vec![b'+'; plus]);
                }
                if minus > 0 {
                    painted(out, old_color, &vec![b'-'; minus]);
                }
            }
        }
        out.push(b'\n');
    }
    if shown.len() < files.len() {
        out.extend_from_slice(b" ...\n");
    }
    write_summary_line(out, files);
}

pub fn write_numstat(out: &mut Vec<u8>, files: &[FileStat], names: &[Vec<u8>], nul: bool) {
    for (f, name) in files.iter().zip(names) {
        match f.kind {
            StatKind::Unmerged => out.extend_from_slice(b"0\t0\t"),
            StatKind::Binary { .. } => out.extend_from_slice(b"-\t-\t"),
            StatKind::Text { added, deleted } => write!(out, "{added}\t{deleted}\t").unwrap(),
        }
        out.extend_from_slice(name);
        out.push(if nul { 0 } else { b'\n' });
    }
}

/// Splits the `+`/`-` bar of one stat line. When the largest change does not
/// fit into `width` columns, the whole bar is scaled first (keeping room for
/// one sign of each kind when both kinds are present) and the smaller side is
/// scaled on its own; the larger side receives the remaining columns.
fn bar_lengths(added: usize, deleted: usize, width: usize, max_change: usize) -> (usize, usize) {
    if width >= max_change {
        return (added, deleted);
    }
    let both = added > 0 && deleted > 0;
    let bar = scale(added + deleted, width, max_change).max(if both { 2 } else { 0 });
    let smaller_is_added = added < deleted;
    let small = scale(
        if smaller_is_added { added } else { deleted },
        width,
        max_change,
    );
    let large = bar - small;
    if smaller_is_added {
        (small, large)
    } else {
        (large, small)
    }
}
