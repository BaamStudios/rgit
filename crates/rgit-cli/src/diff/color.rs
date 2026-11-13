// SPDX-License-Identifier: MIT
//! Coloured patches and stats (`color.diff`, `color.diff.<slot>` in
//! Documentation/config/color.adoc).
//!
//! The patch of one file is coloured line by line, as observed with the
//! `git` binary:
//!
//! * header lines in `meta` (the "Binary files" line stays plain);
//! * the `@@ ... @@` part of a hunk header in `frag`, the space before
//!   the function name in `context` and the function name in `func`;
//! * context lines and "\ No newline" in `context`, removed lines in
//!   `old`;
//! * added lines: the `+` in `new`, then the text in `new` with
//!   whitespace errors in `whitespace`. Without `core.whitespace` (#146)
//!   the errors are git's defaults: whitespace at the end of the line
//!   (`blank-at-eol`), spaces in the indentation before a tab
//!   (`space-before-tab`, the tabs themselves stay uncoloured) and blank
//!   lines added at the end of the file (`blank-at-eof`, the whole line).
//!
//! Every coloured part ends with a reset, even for an empty colour.

use rgit_config::ConfigSet;

use crate::Die;
use crate::color::{RESET, slot};

/// The colours of the slots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffColors {
    pub context: String,
    pub meta: String,
    pub frag: String,
    pub func: String,
    pub old: String,
    pub new: String,
    pub whitespace: String,
}

impl DiffColors {
    /// The slots from `color.diff.<slot>` with git's defaults.
    pub fn from_config(config: &ConfigSet) -> Result<DiffColors, Die> {
        // `plain` is the old name of `context`.
        let context = match config.get("color.diff.context") {
            Some(_) => slot(config, "color.diff.context", "")?,
            None => slot(config, "color.diff.plain", "")?,
        };
        Ok(DiffColors {
            context,
            meta: slot(config, "color.diff.meta", "\x1b[1m")?,
            frag: slot(config, "color.diff.frag", "\x1b[36m")?,
            func: slot(config, "color.diff.func", "")?,
            old: slot(config, "color.diff.old", "\x1b[31m")?,
            new: slot(config, "color.diff.new", "\x1b[32m")?,
            whitespace: slot(config, "color.diff.whitespace", "\x1b[41m")?,
        })
    }

    /// The default colours.
    #[cfg(test)]
    pub fn defaults() -> DiffColors {
        DiffColors::from_config(&ConfigSet::new()).expect("defaults")
    }
}

fn paint(out: &mut Vec<u8>, color: &str, text: &[u8]) {
    out.extend_from_slice(color.as_bytes());
    out.extend_from_slice(text);
    out.extend_from_slice(RESET.as_bytes());
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\x0b' | b'\x0c')
}

/// 1-based number of the first line of the blank lines at the end of
/// `data` (one past the last line when there are none).
fn trailing_blank_start(data: &[u8]) -> usize {
    let lines: Vec<&[u8]> = data.split_inclusive(|&b| b == b'\n').collect();
    let blank = lines
        .iter()
        .rev()
        .take_while(|l| l.iter().all(|&b| is_space(b) || b == b'\n'))
        .count();
    lines.len() - blank + 1
}

/// The text of an added line with its whitespace errors marked.
fn added_text(out: &mut Vec<u8>, c: &DiffColors, text: &[u8]) {
    let trail = text.len() - text.iter().rev().take_while(|&&b| is_space(b)).count();
    let body = &text[..trail];
    let indent = body
        .iter()
        .take_while(|&&b| b == b' ' || b == b'\t')
        .count();
    let region = body[..indent]
        .iter()
        .rposition(|&b| b == b'\t')
        .map_or(0, |i| i + 1);
    let mut i = 0;
    while i < region {
        let start = i;
        if body[i] == b'\t' {
            while i < region && body[i] == b'\t' {
                i += 1;
            }
            out.extend_from_slice(&body[start..i]);
        } else {
            while i < region && body[i] == b' ' {
                i += 1;
            }
            paint(out, &c.whitespace, &body[start..i]);
        }
    }
    if region < trail {
        paint(out, &c.new, &body[region..]);
    }
    if trail < text.len() {
        paint(out, &c.whitespace, &text[trail..]);
    }
}

/// Colour the patch of one file (`plain`, as written by
/// `rgit_diff::write_file_patch_with`) into `out`; `new_data` is the
/// new side's contents (for blank lines at its end).
pub fn patch(out: &mut Vec<u8>, c: &DiffColors, plain: &[u8], new_data: &[u8]) {
    let blank_from = trailing_blank_start(new_data);
    let mut in_header = true;
    let mut new_line = 0usize;
    for raw in plain.split_inclusive(|&b| b == b'\n') {
        let (line, eol) = match raw.strip_suffix(b"\n") {
            Some(l) => (l, &b"\n"[..]),
            None => (raw, &b""[..]),
        };
        if in_header && !line.starts_with(b"@@") {
            if line.starts_with(b"Binary files ") {
                out.extend_from_slice(line);
            } else {
                paint(out, &c.meta, line);
            }
            out.extend_from_slice(eol);
            continue;
        }
        in_header = false;
        match line.first() {
            Some(b'@') => {
                let end = line[2..]
                    .windows(2)
                    .position(|w| w == b"@@")
                    .map_or(line.len(), |p| p + 4);
                paint(out, &c.frag, &line[..end]);
                let rest = &line[end..];
                if let Some(func) = rest.strip_prefix(b" ") {
                    paint(out, &c.context, b" ");
                    paint(out, &c.func, func);
                } else if !rest.is_empty() {
                    paint(out, &c.context, rest);
                }
                // `+<start>,<len>`: the next new line.
                new_line = std::str::from_utf8(&line[..end])
                    .ok()
                    .and_then(|h| h.split(' ').find_map(|w| w.strip_prefix('+')))
                    .and_then(|r| r.split(',').next()?.parse().ok())
                    .unwrap_or(0);
            }
            Some(b'+') => {
                let text = &line[1..];
                if new_line >= blank_from && text.iter().all(|&b| is_space(b)) {
                    paint(out, &c.whitespace, line);
                } else {
                    paint(out, &c.new, b"+");
                    added_text(out, c, text);
                }
                new_line += 1;
            }
            Some(b'-') => paint(out, &c.old, line),
            Some(b' ') => {
                paint(out, &c.context, line);
                new_line += 1;
            }
            _ => paint(out, &c.context, line),
        }
        out.extend_from_slice(eol);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(b: &[u8]) -> String {
        String::from_utf8_lossy(b).replace('\x1b', "^[")
    }

    fn color_patch(plain: &str, new_data: &str) -> String {
        let mut out = Vec::new();
        patch(
            &mut out,
            &DiffColors::defaults(),
            plain.as_bytes(),
            new_data.as_bytes(),
        );
        show(&out)
    }

    #[test]
    fn header_and_hunks() {
        let plain = "diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n\
                     @@ -1,2 +1,2 @@ fn main\n a\n-b\n+c\n\\ No newline at end of file\n";
        assert_eq!(
            color_patch(plain, "a\nc"),
            "^[[1mdiff --git a/f b/f^[[m\n^[[1mindex 1..2 100644^[[m\n\
             ^[[1m--- a/f^[[m\n^[[1m+++ b/f^[[m\n\
             ^[[36m@@ -1,2 +1,2 @@^[[m ^[[mfn main^[[m\n a^[[m\n^[[31m-b^[[m\n\
             ^[[32m+^[[m^[[32mc^[[m\n\\ No newline at end of file^[[m\n"
        );
        let binary = "diff --git a/b b/b\nindex 1..2 100644\nBinary files a/b and b/b differ\n";
        assert_eq!(
            color_patch(binary, ""),
            "^[[1mdiff --git a/b b/b^[[m\n^[[1mindex 1..2 100644^[[m\n\
             Binary files a/b and b/b differ\n"
        );
    }

    #[test]
    fn whitespace_errors() {
        let plain = "@@ -1 +1,6 @@\n a\n+\t \tx\n+  \t\tx \n+\n+  \n+mid\r\n+\n";
        assert_eq!(
            color_patch(plain, "a\n\t \tx\n  \t\tx \n\n  \nmid\r\n\n"),
            "^[[36m@@ -1 +1,6 @@^[[m\n a^[[m\n\
             ^[[32m+^[[m\t^[[41m ^[[m\t^[[32mx^[[m\n\
             ^[[32m+^[[m^[[41m  ^[[m\t\t^[[32mx^[[m^[[41m ^[[m\n\
             ^[[32m+^[[m\n\
             ^[[32m+^[[m^[[41m  ^[[m\n\
             ^[[32m+^[[m^[[32mmid^[[m^[[41m\r^[[m\n\
             ^[[41m+^[[m\n"
        );
    }
}
