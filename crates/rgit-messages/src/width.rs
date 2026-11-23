// SPDX-License-Identifier: MIT
//! Display width of text on a terminal, for column layouts, aligned
//! `--format` placeholders and graphs: East Asian wide characters take two
//! cells, combining and control characters none, and ANSI colour sequences
//! (`ESC [ ... m`) can be skipped.

use unicode_width::UnicodeWidthChar;

/// The cells `c` takes on a terminal (control characters take none).
pub fn char_width(c: char) -> usize {
    if c.is_control() {
        return 0;
    }
    UnicodeWidthChar::width(c).unwrap_or(0)
}

/// The width of `s`, colour sequences not counted.
pub fn display_width(s: &str) -> usize {
    width_of(s, true)
}

/// The width of `s` with colour sequences counted as text (their `ESC`
/// takes no cell).
pub fn display_width_raw(s: &str) -> usize {
    width_of(s, false)
}

fn width_of(s: &str, skip_ansi: bool) -> usize {
    let mut w = 0;
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        if skip_ansi && let Some(n) = ansi_len(rest) {
            rest = &rest[n..];
            continue;
        }
        w += char_width(c);
        rest = &rest[c.len_utf8()..];
    }
    w
}

/// The length in bytes of a colour sequence (`ESC [` digits and `;`,
/// then `m`) at the start of `s`.
pub fn ansi_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.len() < 3 || b[0] != 0x1b || b[1] != b'[' {
        return None;
    }
    let end = b[2..]
        .iter()
        .position(|&c| !(c.is_ascii_digit() || c == b';'))?;
    (b[2 + end] == b'm').then_some(3 + end)
}

/// Where [`align`] puts the text inside the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Middle,
    Right,
}

/// `s` padded with spaces to `width` cells (colour sequences not
/// counted); text that is already as wide or wider is returned as is.
/// In the middle, the extra space goes to the right.
pub fn align(s: &str, width: usize, how: Align) -> String {
    let w = display_width(s);
    if w >= width {
        return s.to_owned();
    }
    let pad = width - w;
    let (left, right) = match how {
        Align::Left => (0, pad),
        Align::Right => (pad, 0),
        Align::Middle => (pad / 2, pad - pad / 2),
    };
    let mut out = String::with_capacity(s.len() + pad);
    out.extend(std::iter::repeat_n(' ', left));
    out.push_str(s);
    out.extend(std::iter::repeat_n(' ', right));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths() {
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("\u{65e5}\u{672c}"), 4);
        assert_eq!(display_width("e\u{301}"), 1);
        assert_eq!(display_width("\x1b[1;31mred\x1b[m"), 3);
        assert_eq!(display_width_raw("\x1b[31mx"), 5);
        assert_eq!(display_width("tab\t"), 3);
        assert_eq!(ansi_len("\x1b[m"), Some(3));
        assert_eq!(ansi_len("\x1b[1x"), None);
    }

    #[test]
    fn alignment() {
        assert_eq!(align("ab", 5, Align::Left), "ab   ");
        assert_eq!(align("ab", 5, Align::Right), "   ab");
        assert_eq!(align("ab", 5, Align::Middle), " ab  ");
        assert_eq!(align("\u{65e5}", 4, Align::Right), "  \u{65e5}");
        assert_eq!(align("abcdef", 3, Align::Left), "abcdef");
    }
}
