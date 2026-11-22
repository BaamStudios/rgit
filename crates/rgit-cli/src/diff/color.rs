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
//! * lines whose kind is highlighted (`--ws-error-highlight`, by default
//!   added lines only): the sign in the line's colour, then the text in
//!   the line's colour with the whitespace errors of the path's rule
//!   (`core.whitespace`, the `whitespace` attribute) in `whitespace`;
//!   indent characters before the last tab that are no error stay
//!   uncoloured; blank lines added at the end of the file are coloured
//!   as a whole;
//! * other lines: sign and text in one colour;
//! * a carriage return at the end of a line is written after the reset
//!   unless it is highlighted as an error;
//! * "\ No newline" in `whitespace` after a highlighted line when
//!   `incomplete-line` is on (not for symbolic links).
//!
//! Every coloured part ends with a reset, even for an empty colour.

use rgit_config::ConfigSet;

use crate::Die;
use rgit_diff::ws_rule::{self, Part, WsRule};

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
    /// `oldMoved`, `oldMovedAlternative`, `oldMovedDimmed`,
    /// `oldMovedAlternativeDimmed`, in [`super::moved::Slot`] order.
    pub old_moved: [String; 4],
    /// The same for `newMoved...`.
    pub new_moved: [String; 4],
}

impl DiffColors {
    /// The slots from `color.diff.<slot>` with git's defaults.
    pub fn from_config(config: &ConfigSet) -> Result<DiffColors, Die> {
        // `diff.color.<slot>` is the old name of `color.diff.<slot>`,
        // and `plain` the old name of `context`.
        let get = |names: &[&str], default: &str| -> Result<String, Die> {
            for name in names {
                for key in [format!("color.diff.{name}"), format!("diff.color.{name}")] {
                    if config.get(&key).is_some() {
                        return slot(config, &key, default);
                    }
                }
            }
            Ok(default.to_owned())
        };
        Ok(DiffColors {
            context: get(&["context", "plain"], "")?,
            meta: get(&["meta"], "\x1b[1m")?,
            frag: get(&["frag"], "\x1b[36m")?,
            func: get(&["func"], "")?,
            old: get(&["old"], "\x1b[31m")?,
            new: get(&["new"], "\x1b[32m")?,
            whitespace: get(&["whitespace"], "\x1b[41m")?,
            old_moved: [
                get(&["oldmoved"], "\x1b[1;35m")?,
                get(&["oldmovedalternative"], "\x1b[1;34m")?,
                get(&["oldmoveddimmed"], "\x1b[2m")?,
                get(&["oldmovedalternativedimmed"], "\x1b[2;3m")?,
            ],
            new_moved: [
                get(&["newmoved"], "\x1b[1;36m")?,
                get(&["newmovedalternative"], "\x1b[1;33m")?,
                get(&["newmoveddimmed"], "\x1b[2m")?,
                get(&["newmovedalternativedimmed"], "\x1b[2;3m")?,
            ],
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

/// Which kinds of lines get their whitespace errors highlighted.
pub const HIGHLIGHT_OLD: u8 = 1;
pub const HIGHLIGHT_NEW: u8 = 2;
pub const HIGHLIGHT_CONTEXT: u8 = 4;

/// Parses a `--ws-error-highlight` / `diff.wsErrorHighlight` value.
pub fn parse_highlight(value: &str) -> Option<u8> {
    let mut kinds = 0;
    for word in value.split(',') {
        match word.trim() {
            "none" => kinds = 0,
            "default" => kinds = HIGHLIGHT_NEW,
            "all" => kinds = HIGHLIGHT_OLD | HIGHLIGHT_NEW | HIGHLIGHT_CONTEXT,
            "old" => kinds |= HIGHLIGHT_OLD,
            "new" => kinds |= HIGHLIGHT_NEW,
            "context" => kinds |= HIGHLIGHT_CONTEXT,
            _ => return None,
        }
    }
    Some(kinds)
}

/// The whitespace settings of one file pair.
pub struct WsContext<'a> {
    pub rule: WsRule,
    /// `HIGHLIGHT_*` bits.
    pub highlight: u8,
    pub old_data: &'a [u8],
    pub new_data: &'a [u8],
    /// The new side is a symbolic link (no `incomplete-line` errors).
    pub new_symlink: bool,
    pub old_symlink: bool,
}

/// Number of blank lines at the end of `data`.
fn trailing_blanks(data: &[u8]) -> usize {
    data.split_inclusive(|&b| b == b'\n')
        .rev()
        .take_while(|l| ws_rule::is_blank(l))
        .count()
}

/// 1-based number of the first new line that counts as a blank line
/// added at the end of the file, or `usize::MAX` when there is none:
/// only when the new side ends in more blank lines than the old one.
pub fn blank_at_eof_from(ws: &WsContext<'_>) -> usize {
    if !ws.rule.has(ws_rule::BLANK_AT_EOF) {
        return usize::MAX;
    }
    let new_blank = trailing_blanks(ws.new_data);
    if new_blank <= trailing_blanks(ws.old_data) {
        return usize::MAX;
    }
    let lines = ws.new_data.split_inclusive(|&b| b == b'\n').count();
    lines - new_blank + 1
}

/// The text of a line with its whitespace errors marked; `color` is the
/// colour of the text.
fn checked_text(out: &mut Vec<u8>, c: &DiffColors, color: &str, text: &[u8], rule: &WsRule) {
    let check = ws_rule::check_line(text, rule);
    for (part, range) in &check.parts {
        match part {
            Part::Plain => out.extend_from_slice(&text[range.clone()]),
            Part::Error => paint(out, &c.whitespace, &text[range.clone()]),
            Part::Text => paint(out, color, &text[range.clone()]),
        }
    }
    out.extend_from_slice(&text[check.body_end..]);
}

/// A whole line in one colour, with a final carriage return after the
/// reset.
fn whole(out: &mut Vec<u8>, color: &str, line: &[u8]) {
    match line.strip_suffix(b"\r") {
        Some(l) => {
            paint(out, color, l);
            out.push(b'\r');
        }
        None => paint(out, color, line),
    }
}

/// Colour the patch of one file (`plain`, as written by
/// `rgit_diff::write_file_patch_with`) into `out`.
pub fn patch(
    out: &mut Vec<u8>,
    c: &DiffColors,
    plain: &[u8],
    ws: &WsContext<'_>,
    moved: &[Option<super::moved::Slot>],
) {
    let mut nth = 0;
    let blank_from = blank_at_eof_from(ws);
    let mut in_header = true;
    let mut new_line = 0usize;
    // The sign of the previous line, for "\ No newline".
    let mut previous = b' ';
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
        let sign = line.first().copied().unwrap_or(b' ');
        let (mut color, kind) = match sign {
            b'+' => (&c.new, HIGHLIGHT_NEW),
            b'-' => (&c.old, HIGHLIGHT_OLD),
            _ => (&c.context, HIGHLIGHT_CONTEXT),
        };
        if sign == b'+' || sign == b'-' {
            if let Some(Some(slot)) = moved.get(nth) {
                let set = if sign == b'+' {
                    &c.new_moved
                } else {
                    &c.old_moved
                };
                color = &set[*slot as usize];
            }
            nth += 1;
        }
        match sign {
            b'@' => {
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
            b'\\' => {
                let symlink = if previous == b'-' {
                    ws.old_symlink
                } else {
                    ws.new_symlink
                };
                let kind = match previous {
                    b'+' => HIGHLIGHT_NEW,
                    b'-' => HIGHLIGHT_OLD,
                    _ => HIGHLIGHT_CONTEXT,
                };
                if ws.rule.has(ws_rule::INCOMPLETE_LINE)
                    && !symlink
                    && previous != b' '
                    && ws.highlight & kind != 0
                {
                    paint(out, &c.whitespace, line);
                } else {
                    paint(out, &c.context, line);
                }
            }
            b'+' | b'-' | b' ' => {
                let text = &line[1..];
                let highlighted = ws.highlight & kind != 0;
                if sign == b'+' && highlighted && new_line >= blank_from && ws_rule::is_blank(text)
                {
                    whole(out, &c.whitespace, line);
                } else if highlighted {
                    paint(out, color, &line[..1]);
                    checked_text(out, c, color, text, &ws.rule);
                } else {
                    whole(out, color, line);
                }
                if sign != b'-' {
                    new_line += 1;
                }
            }
            _ => paint(out, &c.context, line),
        }
        if sign != b'\\' {
            previous = sign;
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
        let ws = WsContext {
            rule: WsRule::default(),
            highlight: HIGHLIGHT_NEW,
            old_data: b"",
            new_data: new_data.as_bytes(),
            new_symlink: false,
            old_symlink: false,
        };
        patch(
            &mut out,
            &DiffColors::defaults(),
            plain.as_bytes(),
            &ws,
            &[],
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
