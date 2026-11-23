// SPDX-License-Identifier: MIT
//! `--check` (Documentation/diff-options.adoc): whitespace errors of the
//! added lines, one report per line (`<path>:<line>: <errors>.` and the
//! line), then blank lines added at the end of the file. The order of the
//! error names and the wording were observed with the `git` binary.

use rgit_diff::ws_rule::{self, check_line};

use super::color::{WsContext, blank_at_eof_from};
use crate::messages::diff as msg;

/// Default length of conflict markers (the `conflict-marker-size`
/// attribute in Documentation/gitattributes.adoc).
pub const DEFAULT_MARKER_SIZE: usize = 7;

/// An added line that looks like a conflict marker left over from a
/// merge: exactly `size` of `<`, `>` followed by a space, or of `=`, `|`
/// followed by whitespace or the end of the line (observed with `git
/// diff --check`).
fn is_conflict_marker(text: &[u8], size: usize) -> bool {
    let Some(&c) = text.first() else {
        return false;
    };
    if size == 0 || !matches!(c, b'<' | b'>' | b'=' | b'|') || text.len() < size {
        return false;
    }
    if !text[..size].iter().all(|&b| b == c) {
        return false;
    }
    match (c, text.get(size)) {
        (b'<' | b'>', next) => next == Some(&b' '),
        (_, None) => true,
        (_, Some(b)) => b.is_ascii_whitespace() || *b == 0x0b,
    }
}

/// Writes the errors of one file's patch (`plain`) to `out`; returns
/// whether there were any.
pub fn check_patch(
    out: &mut Vec<u8>,
    plain: &[u8],
    ws: &WsContext<'_>,
    path: &str,
    marker_size: usize,
) -> bool {
    let blank_from = blank_at_eof_from(ws);
    let mut found = false;
    let mut blank_at: Option<usize> = None;
    let mut in_header = true;
    let mut new_line = 0usize;
    let mut previous = b' ';
    for raw in plain.split_inclusive(|&b| b == b'\n') {
        let line = raw.strip_suffix(b"\n").unwrap_or(raw);
        if in_header && !line.starts_with(b"@@") {
            continue;
        }
        in_header = false;
        let sign = line.first().copied().unwrap_or(b' ');
        match sign {
            b'@' => {
                new_line = std::str::from_utf8(line)
                    .ok()
                    .and_then(|h| h.split(' ').find_map(|w| w.strip_prefix('+')))
                    .and_then(|r| r.split(',').next()?.parse().ok())
                    .unwrap_or(0);
            }
            b'+' => {
                let text = &line[1..];
                if is_conflict_marker(text, marker_size) {
                    found = true;
                    out.extend_from_slice(msg::leftover_conflict_marker(path, new_line).as_bytes());
                }
                let errors = check_line(text, &ws.rule).errors;
                let mut names = Vec::new();
                for (bit, name) in [
                    (ws_rule::BLANK_AT_EOL, msg::WS_TRAILING),
                    (ws_rule::SPACE_BEFORE_TAB, msg::WS_SPACE_BEFORE_TAB),
                    (ws_rule::INDENT_WITH_NON_TAB, msg::WS_INDENT_WITH_SPACES),
                    (ws_rule::TAB_IN_INDENT, msg::WS_TAB_IN_INDENT),
                ] {
                    if errors & bit != 0 {
                        names.push(name);
                    }
                }
                if !names.is_empty() {
                    found = true;
                    out.extend_from_slice(
                        msg::ws_check_line(path, new_line, &names.join(", ")).as_bytes(),
                    );
                    out.extend_from_slice(line);
                    out.push(b'\n');
                }
                if blank_at.is_none() && new_line >= blank_from && ws_rule::is_blank(text) {
                    blank_at = Some(new_line);
                }
                new_line += 1;
            }
            b' ' => new_line += 1,
            b'\\'
                if previous == b'+' && ws.rule.has(ws_rule::INCOMPLETE_LINE) && !ws.new_symlink =>
            {
                found = true;
                out.extend_from_slice(
                    msg::ws_check_line(path, new_line.saturating_sub(1), msg::WS_INCOMPLETE)
                        .as_bytes(),
                );
            }
            _ => {}
        }
        if sign != b'\\' {
            previous = sign;
        }
    }
    if let Some(n) = blank_at {
        found = true;
        out.extend_from_slice(msg::ws_check_line(path, n, msg::WS_BLANK_AT_EOF).as_bytes());
    }
    found
}
