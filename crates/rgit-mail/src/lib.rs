// SPDX-License-Identifier: MIT
//! Writing e-mail headers the way `git format-patch` does
//! (Documentation/git-format-patch.adoc, RFC 5322 and RFC 2047):
//!
//! * [`write_folded`]: plain text folded at spaces so that lines stay
//!   within [`FOLD_WIDTH`] columns;
//! * [`write_encoded`]: RFC 2047 "Q" encoded words, split so that lines
//!   stay within [`ENCODED_WIDTH`] columns;
//! * [`write_address`]: a `Name <email>` mailbox, the name quoted or
//!   encoded as needed.
//!
//! Widths count display columns of the line the text is appended to, so
//! the header name already in the buffer is taken into account.

use unicode_width::UnicodeWidthChar;

/// Lines of plain (not encoded) header text are folded before they would
/// exceed this many columns.
pub const FOLD_WIDTH: usize = 78;

/// Lines holding encoded words stay within this many columns.
pub const ENCODED_WIDTH: usize = 76;

/// Where an encoded word stands (RFC 2047, section 5), which decides the
/// characters that may appear unencoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Context {
    /// Unstructured text such as `Subject`.
    Text,
    /// A display name (`phrase`) in an address header.
    Phrase,
}

/// Whether `text` has to be written as encoded words: it contains bytes
/// outside ASCII or something a reader would take for an encoded word.
pub fn needs_encoding(text: &[u8]) -> bool {
    !text.is_ascii() || text.windows(2).any(|w| w == b"=?")
}

/// The RFC 5322 "specials" that may not appear unquoted in a phrase.
fn is_special(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b':' | b';' | b'@' | b'\\' | b',' | b'.' | b'"'
    )
}

/// Whether a display name has to be written as a quoted string.
pub fn needs_quoting(name: &[u8]) -> bool {
    name.iter().copied().any(is_special)
}

/// `name` as a quoted string: in double quotes, with `"` and `\` escaped.
pub fn quote(name: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len() + 2);
    out.push(b'"');
    for &b in name {
        if b == b'"' || b == b'\\' {
            out.push(b'\\');
        }
        out.push(b);
    }
    out.push(b'"');
    out
}

/// The display width of the last line of `buf`.
pub fn column(buf: &[u8]) -> usize {
    let start = buf.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    width(&buf[start..])
}

/// The display width of `text` (bytes that are not UTF-8 count one column
/// each).
pub fn width(text: &[u8]) -> usize {
    let mut w = 0;
    for chunk in text.utf8_chunks() {
        w += chunk
            .valid()
            .chars()
            .map(|c| c.width().unwrap_or(0))
            .sum::<usize>();
        w += chunk.invalid().len();
    }
    w
}

/// Append `text` to `out`, replacing a space by a line break and a space
/// wherever the next word would end beyond [`FOLD_WIDTH`]. When even the
/// first word does not fit on the current line, the break comes before it.
pub fn write_folded(out: &mut Vec<u8>, text: &[u8]) {
    let mut col = column(out);
    for (i, word) in text.split(|&b| b == b' ').enumerate() {
        let w = width(word);
        if i == 0 {
            if col + w > FOLD_WIDTH && col > 0 && !word.is_empty() {
                out.extend_from_slice(b"\n ");
                col = 1;
            }
        } else if col + 1 + w > FOLD_WIDTH {
            out.extend_from_slice(b"\n ");
            col = 1 + w;
            out.extend_from_slice(word);
            continue;
        } else {
            out.push(b' ');
            col += 1;
        }
        out.extend_from_slice(word);
        col += w;
    }
}

/// Whether `b` may stay unencoded inside an encoded word in `ctx`.
fn is_plain(b: u8, ctx: Context) -> bool {
    match ctx {
        Context::Text => b.is_ascii_graphic() && !matches!(b, b'=' | b'?' | b'_'),
        Context::Phrase => {
            b.is_ascii_alphanumeric() || matches!(b, b'!' | b'*' | b'+' | b'-' | b'/')
        }
    }
}

/// The length of the character starting `text` (one byte when it is not
/// valid UTF-8).
fn char_len(text: &[u8]) -> usize {
    let n = match text[0] {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    };
    if n > text.len() || std::str::from_utf8(&text[..n]).is_err() {
        1
    } else {
        n
    }
}

/// Append `text` to `out` as RFC 2047 "Q" encoded words in `charset`.
/// A new word on a continuation line starts whenever the next character
/// would make the line longer than [`ENCODED_WIDTH`]; characters are never
/// split between words.
pub fn write_encoded(out: &mut Vec<u8>, text: &[u8], charset: &str, ctx: Context) {
    let open = format!("=?{charset}?q?");
    out.extend_from_slice(open.as_bytes());
    let mut col = column(out);
    let mut empty = true;
    let mut rest = text;
    while !rest.is_empty() {
        let n = char_len(rest);
        let mut chunk = Vec::with_capacity(n * 3);
        for &b in &rest[..n] {
            if is_plain(b, ctx) {
                chunk.push(b);
            } else {
                chunk.extend_from_slice(format!("={b:02X}").as_bytes());
            }
        }
        if !empty && col + chunk.len() + 2 > ENCODED_WIDTH {
            out.extend_from_slice(b"?=\n ");
            out.extend_from_slice(open.as_bytes());
            col = 1 + open.len();
        }
        out.extend_from_slice(&chunk);
        col += chunk.len();
        empty = false;
        rest = &rest[n..];
    }
    out.extend_from_slice(b"?=");
}

/// Append header text: encoded when `encode` is set and the text needs
/// it, folded plain text otherwise.
pub fn write_text(out: &mut Vec<u8>, text: &[u8], charset: &str, encode: bool) {
    if encode && needs_encoding(text) {
        write_encoded(out, text, charset, Context::Text);
    } else {
        write_folded(out, text);
    }
}

/// Append the mailbox `name <email>`: the name encoded (when `encode` is
/// set and it needs it) or quoted when it contains specials, the whole
/// folded like plain text.
pub fn write_address(out: &mut Vec<u8>, name: &[u8], email: &[u8], charset: &str, encode: bool) {
    let mut addr = Vec::with_capacity(email.len() + 3);
    addr.push(b'<');
    addr.extend_from_slice(email);
    addr.push(b'>');
    if encode && needs_encoding(name) {
        write_encoded(out, name, charset, Context::Phrase);
        // The address is one more word to fold.
        if column(out) + 1 + width(&addr) > FOLD_WIDTH {
            out.extend_from_slice(b"\n ");
        } else {
            out.push(b' ');
        }
        out.extend_from_slice(&addr);
        return;
    }
    let mut text = if needs_quoting(name) {
        quote(name)
    } else {
        name.to_vec()
    };
    text.push(b' ');
    text.extend_from_slice(&addr);
    write_folded(out, &text);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(name: &str, f: impl FnOnce(&mut Vec<u8>)) -> String {
        let mut out = name.as_bytes().to_vec();
        f(&mut out);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn plain_text_folds_at_the_last_fitting_space() {
        let word = "x".repeat(56);
        let fits = header("Subject: [PATCH] ", |o| {
            write_folded(o, format!("aaaa {word}").as_bytes());
        });
        assert_eq!(fits.len(), 78);
        let word = "x".repeat(57);
        let folded = header("Subject: [PATCH] ", |o| {
            write_folded(o, format!("aaaa {word}").as_bytes());
        });
        assert_eq!(folded, format!("Subject: [PATCH] aaaa\n {word}"));
    }

    #[test]
    fn an_overlong_first_word_moves_to_the_next_line() {
        let word = "y".repeat(90);
        let s = header("Subject: [PATCH] ", |o| write_folded(o, word.as_bytes()));
        assert_eq!(s, format!("Subject: [PATCH] \n {word}"));
    }

    #[test]
    fn widths_count_columns_not_bytes() {
        let word = "ö".repeat(56);
        let s = header("Subject: [PATCH] ", |o| {
            write_folded(o, format!("aaaa {word}").as_bytes());
        });
        assert!(!s.contains('\n'));
    }

    #[test]
    fn encoded_words_split_between_characters() {
        let s = header("Subject: [PATCH] ", |o| {
            write_encoded(
                o,
                "ä yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy".as_bytes(),
                "UTF-8",
                Context::Text,
            );
        });
        for line in s.lines() {
            assert!(line.len() <= ENCODED_WIDTH, "{line}");
        }
        assert!(s.starts_with("Subject: [PATCH] =?UTF-8?q?=C3=A4=20yyy"));
        assert!(s.contains("?=\n =?UTF-8?q?y"));
    }

    #[test]
    fn text_and_phrase_encode_different_characters() {
        let s = header("", |o| {
            write_encoded(o, b"a_b c=d.e", "UTF-8", Context::Text)
        });
        assert_eq!(s, "=?UTF-8?q?a=5Fb=20c=3Dd.e?=");
        let s = header("", |o| {
            write_encoded(o, b"a_b c=d.e+f", "UTF-8", Context::Phrase)
        });
        assert_eq!(s, "=?UTF-8?q?a=5Fb=20c=3Dd=2Ee+f?=");
    }

    #[test]
    fn addresses_are_quoted_or_encoded() {
        let s = header("From: ", |o| {
            write_address(o, b"A U Thor", b"a@x", "UTF-8", true)
        });
        assert_eq!(s, "From: A U Thor <a@x>");
        let s = header("From: ", |o| {
            write_address(o, b"A. U. \"T\"", b"a@x", "UTF-8", true)
        });
        assert_eq!(s, "From: \"A. U. \\\"T\\\"\" <a@x>");
        let s = header("From: ", |o| {
            write_address(o, "Föo B. Bar".as_bytes(), b"a@x", "UTF-8", true)
        });
        assert_eq!(s, "From: =?UTF-8?q?F=C3=B6o=20B=2E=20Bar?= <a@x>");
        let s = header("From: ", |o| {
            write_address(o, "Föo".as_bytes(), b"a@x", "UTF-8", false)
        });
        assert_eq!(s, "From: Föo <a@x>");
    }

    #[test]
    fn a_long_address_goes_to_its_own_line() {
        let name = "foo_bar_".repeat(8);
        let s = header("From: ", |o| {
            write_address(o, name.as_bytes(), b"author@example.com", "UTF-8", true);
        });
        assert_eq!(s, format!("From: {name}\n <author@example.com>"));
    }
}
