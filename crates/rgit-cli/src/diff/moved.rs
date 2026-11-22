// SPDX-License-Identifier: MIT
//! `--color-moved[=<mode>]` and `--color-moved-ws=<modes>`
//! (Documentation/diff-options.adoc, `diff.colorMoved`,
//! `diff.colorMovedWS`).
//!
//! The documentation describes the modes; the details below were
//! derived from the `git` binary and from the expectations of the
//! upstream test scripts:
//!
//! * all removed and added lines of the whole output are considered, in
//!   output order; anything else (context, hunk and file headers) ends a
//!   block;
//! * a line starts a block when lines of the other sign with the same
//!   text exist; the block goes on while the next line continues one of
//!   the matched runs (the line after a matched one has the same text);
//! * except in `plain` mode a block needs at least 20 alphanumeric
//!   characters; a shorter one is dropped and detection starts again at
//!   its second line;
//! * `zebra` switches to the alternative colour for a block that directly
//!   follows another block; `dimmed-zebra` dims all lines of a block but
//!   those bordering on an adjacent block;
//! * `allow-indentation-change` compares the lines without their
//!   indentation and keeps a block only while the change of indentation
//!   width (tabs to multiples of 8, form feeds and vertical tabs none) is
//!   the same; blank lines go with any
//!   change.

use std::collections::HashMap;

use crate::messages::diff as msg;

/// How moved lines are shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    No,
    Plain,
    Blocks,
    Zebra,
    DimmedZebra,
}

impl Mode {
    /// A `--color-moved=` / `diff.colorMoved` value (the configuration
    /// also takes booleans).
    pub fn parse(value: &str, config: bool) -> Option<Mode> {
        Some(match value {
            "no" => Mode::No,
            "default" | "zebra" => Mode::Zebra,
            "plain" => Mode::Plain,
            "blocks" => Mode::Blocks,
            "dimmed-zebra" | "dimmed_zebra" => Mode::DimmedZebra,
            _ if config => match rgit_config::value::parse_bool(Some(value.as_bytes())) {
                Some(true) => Mode::Zebra,
                Some(false) => Mode::No,
                None => return None,
            },
            _ => return None,
        })
    }
}

/// `--color-moved-ws`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MovedWs {
    pub ws: rgit_diff::Whitespace,
    pub indent: bool,
}

impl MovedWs {
    /// Parses a comma separated list; the error is the message.
    pub fn parse(value: &str) -> Result<MovedWs, String> {
        let mut m = MovedWs::default();
        for word in value.split(',') {
            match word.trim() {
                "no" => m = MovedWs::default(),
                "ignore-space-at-eol" => m.ws.at_eol = true,
                "ignore-space-change" => m.ws.change = true,
                "ignore-all-space" => m.ws.all = true,
                "allow-indentation-change" => m.indent = true,
                "" => {}
                other => return Err(msg::unknown_color_moved_ws(other)),
            }
        }
        if m.indent && m.ws.any() {
            return Err(msg::COLOR_MOVED_WS_INDENT_COMBINED.to_string());
        }
        Ok(m)
    }
}

/// The colour slot of a moved line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Moved,
    Alternative,
    Dimmed,
    AlternativeDimmed,
}

struct Entry<'a> {
    sign: u8,
    text: &'a [u8],
    /// Index of the file and of the line among its `+`/`-` lines.
    file: usize,
    nth: usize,
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\r' | b'\n' | b'\x0b' | b'\x0c')
}

/// Width of the indentation and the text after it.
fn indent(text: &[u8]) -> (i64, &[u8]) {
    let mut w = 0i64;
    let mut i = 0;
    while i < text.len() && is_space(text[i]) {
        // Form feeds and vertical tabs take no width.
        w = match text[i] {
            b'\t' => (w / 8 + 1) * 8,
            b' ' => w + 1,
            _ => w,
        };
        i += 1;
    }
    (w, &text[i..])
}

/// The colour slots of the `+`/`-` lines of each file's patch (`plains`,
/// as written by `rgit_diff::write_file_patch_extra`).
pub fn mark(plains: &[Vec<u8>], mode: Mode, ws: &MovedWs) -> Vec<Vec<Option<Slot>>> {
    let mut entries: Vec<Entry<'_>> = Vec::new();
    let mut result: Vec<Vec<Option<Slot>>> = Vec::with_capacity(plains.len());
    for (file, plain) in plains.iter().enumerate() {
        let mut in_header = true;
        let mut nth = 0;
        for raw in plain.split_inclusive(|&b| b == b'\n') {
            let line = raw.strip_suffix(b"\n").unwrap_or(raw);
            if in_header && !line.starts_with(b"@@") {
                continue;
            }
            in_header = false;
            match line.first() {
                Some(&s @ (b'+' | b'-')) => {
                    entries.push(Entry {
                        sign: s,
                        text: &line[1..],
                        file,
                        nth,
                    });
                    nth += 1;
                }
                _ => entries.push(Entry {
                    sign: 0,
                    text: b"",
                    file,
                    nth: 0,
                }),
            }
        }
        entries.push(Entry {
            sign: 0,
            text: b"",
            file,
            nth: 0,
        });
        result.push(vec![None; nth]);
    }
    if mode == Mode::No {
        return result;
    }
    let n = entries.len();
    let keys: Vec<Vec<u8>> = entries
        .iter()
        .map(|e| {
            if ws.indent {
                indent(e.text).1.to_vec()
            } else {
                rgit_diff::whitespace::key(e.text, &ws.ws).unwrap_or_else(|| e.text.to_vec())
            }
        })
        .collect();
    let widths: Vec<Option<i64>> = entries
        .iter()
        .map(|e| {
            let (w, rest) = indent(e.text);
            (!rest.is_empty()).then_some(w)
        })
        .collect();
    let mut index: HashMap<(u8, &[u8]), Vec<usize>> = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        if e.sign != 0 {
            index.entry((e.sign, &keys[i][..])).or_default().push(i);
        }
    }
    let opposite = |s: u8| if s == b'+' { b'-' } else { b'+' };
    // The indentation change of a pair (None for blank lines).
    let delta = |a: usize, b: usize| match (widths[a], widths[b]) {
        (Some(x), Some(y)) => Some(x - y),
        _ => None,
    };
    let alnum = |r: std::ops::Range<usize>| -> usize {
        r.map(|i| {
            entries[i]
                .text
                .iter()
                .filter(|c| c.is_ascii_alphanumeric())
                .count()
        })
        .sum()
    };
    // Block id per entry (0: not moved).
    let mut block = vec![0usize; n];
    let mut next_id = 1;
    let mut current: Option<(usize, usize)> = None;
    // Matched positions of the running block with their indentation
    // change.
    let mut pmb: Vec<(usize, Option<i64>)> = Vec::new();
    let mut i = 0;
    loop {
        if let Some((start, id)) = current {
            let continued: Vec<(usize, Option<i64>)> =
                if i < n && entries[i].sign != 0 && entries[i].sign == entries[i - 1].sign {
                    pmb.iter()
                        .filter_map(|&(p, d)| {
                            let q = p + 1;
                            if q >= n
                                || entries[q].sign != opposite(entries[i].sign)
                                || keys[q] != keys[i]
                            {
                                return None;
                            }
                            if !ws.indent {
                                return Some((q, d));
                            }
                            match (d, delta(i, q)) {
                                (Some(a), Some(b)) if a != b => None,
                                (None, b) => Some((q, b)),
                                (a, _) => Some((q, a)),
                            }
                        })
                        .collect()
                } else {
                    Vec::new()
                };
            if !continued.is_empty() {
                pmb = continued;
                block[i] = id;
                i += 1;
                continue;
            }
            current = None;
            pmb.clear();
            if alnum(start..i) < 20 {
                for b in &mut block[start..i] {
                    *b = 0;
                }
                i = start + 1;
                continue;
            }
        }
        if i >= n {
            break;
        }
        let e = &entries[i];
        if e.sign == 0 {
            i += 1;
            continue;
        }
        let Some(cands) = index.get(&(opposite(e.sign), &keys[i][..])) else {
            i += 1;
            continue;
        };
        if mode == Mode::Plain {
            block[i] = next_id;
            next_id += 1;
            i += 1;
            continue;
        }
        current = Some((i, next_id));
        block[i] = next_id;
        next_id += 1;
        pmb = cands
            .iter()
            .map(|&c| (c, if ws.indent { delta(i, c) } else { None }))
            .collect();
        i += 1;
    }
    // Colours: alternate for a block right after another one; dim lines
    // that do not border on another block.
    let mut alt = vec![false; n];
    for i in 0..n {
        if block[i] == 0 {
            continue;
        }
        alt[i] = if i > 0 && block[i - 1] == block[i] {
            alt[i - 1]
        } else if i > 0 && block[i - 1] != 0 && entries[i - 1].sign == entries[i].sign {
            !alt[i - 1]
        } else {
            false
        };
    }
    for i in 0..n {
        if block[i] == 0 {
            continue;
        }
        let e = &entries[i];
        let slot = match mode {
            Mode::Plain | Mode::Blocks => Slot::Moved,
            Mode::Zebra => {
                if alt[i] {
                    Slot::Alternative
                } else {
                    Slot::Moved
                }
            }
            Mode::DimmedZebra => {
                let prev = i > 0
                    && block[i - 1] != 0
                    && block[i - 1] != block[i]
                    && entries[i - 1].sign == e.sign;
                let next = i + 1 < n
                    && block[i + 1] != 0
                    && block[i + 1] != block[i]
                    && entries[i + 1].sign == e.sign;
                match (prev || next, alt[i]) {
                    (true, false) => Slot::Moved,
                    (true, true) => Slot::Alternative,
                    (false, false) => Slot::Dimmed,
                    (false, true) => Slot::AlternativeDimmed,
                }
            }
            Mode::No => continue,
        };
        result[e.file][e.nth] = Some(slot);
    }
    result
}
