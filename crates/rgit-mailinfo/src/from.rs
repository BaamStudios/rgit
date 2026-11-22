// SPDX-License-Identifier: MIT
//! Splitting a `From:` value into author name and e-mail address.
//!
//! Determined by running `git mailinfo` on many address forms:
//!
//! - Quoted strings lose their quotes (backslash escapes inside them are
//!   resolved); comments keep their parentheses, quotes inside a comment
//!   are kept. A backslash outside both is an ordinary character.
//! - The address is the run of characters around the first `@` up to
//!   whitespace or `<` before it and whitespace or `>` after it. It is cut
//!   out of the value together with directly surrounding `<` and `>`.
//!   Without an `@`, an address in `<...>` is taken and the name is what
//!   precedes it; without either, name and address are empty.
//! - Whitespace in the name is collapsed; a name that is one parenthesized
//!   comment loses the outer parentheses. An empty name, or one that still
//!   contains `@`, `<` or `>`, is replaced by the address.

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// Removes the quotes of quoted strings and resolves backslash escapes in
/// quoted strings and comments.
fn unquote(value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(value.len());
    let mut in_quote = false;
    let mut depth = 0usize;
    let mut i = 0;
    while i < value.len() {
        let c = value[i];
        i += 1;
        if in_quote {
            match c {
                b'\\' if i < value.len() => {
                    out.push(value[i]);
                    i += 1;
                }
                b'"' => in_quote = false,
                _ => out.push(c),
            }
        } else if depth > 0 {
            match c {
                b'\\' if i < value.len() => {
                    out.push(value[i]);
                    i += 1;
                }
                b'(' => {
                    depth += 1;
                    out.push(c);
                }
                b')' => {
                    depth -= 1;
                    out.push(c);
                }
                _ => out.push(c),
            }
        } else {
            match c {
                b'"' => in_quote = true,
                b'(' => {
                    depth = 1;
                    out.push(c);
                }
                _ => out.push(c),
            }
        }
    }
    out
}

/// Collapses runs of whitespace to one space and trims both ends.
pub(crate) fn collapse_trim(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut space = false;
    for &b in s {
        if is_space(b) {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(b' ');
        }
        space = false;
        out.push(b);
    }
    out
}

/// The (name, address) of a `From:` value (already decoded).
pub(crate) fn parse(value: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let s = unquote(value);
    let (mut name, email) = if let Some(at) = s.iter().position(|&b| b == b'@') {
        let mut start = at;
        while start > 0 && !is_space(s[start - 1]) && s[start - 1] != b'<' {
            start -= 1;
        }
        let mut end = at + 1;
        while end < s.len() && !is_space(s[end]) && s[end] != b'>' {
            end += 1;
        }
        let email = s[start..end].to_vec();
        let cut_start = if start > 0 && s[start - 1] == b'<' {
            start - 1
        } else {
            start
        };
        let cut_end = if end < s.len() && s[end] == b'>' {
            end + 1
        } else {
            end
        };
        let mut name = s[..cut_start].to_vec();
        name.push(b' ');
        name.extend_from_slice(&s[cut_end..]);
        (name, email)
    } else if let Some(lt) = s.iter().position(|&b| b == b'<') {
        match s[lt..].iter().position(|&b| b == b'>') {
            Some(gt) => (s[..lt].to_vec(), s[lt + 1..lt + gt].to_vec()),
            None => return (Vec::new(), Vec::new()),
        }
    } else {
        return (Vec::new(), Vec::new());
    };
    name = collapse_trim(&name);
    if name.len() >= 2 && name[0] == b'(' && name[name.len() - 1] == b')' {
        name = collapse_trim(&name[1..name.len() - 1]);
    }
    if name.is_empty() || name.iter().any(|&b| matches!(b, b'@' | b'<' | b'>')) {
        name = email.clone();
    }
    (name, email)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> (String, String) {
        let (n, e) = parse(s.as_bytes());
        (String::from_utf8(n).unwrap(), String::from_utf8(e).unwrap())
    }

    fn ok(input: &str, name: &str, email: &str) {
        assert_eq!(p(input), (name.to_owned(), email.to_owned()), "{input}");
    }

    #[test]
    fn forms() {
        ok("A U Thor <a@b>", "A U Thor", "a@b");
        ok("a@b", "a@b", "a@b");
        ok("<a@b>", "a@b", "a@b");
        ok("\"A B\" <a@b>", "A B", "a@b");
        ok("\"A \\\"q\\\" B\" <a@b>", "A \"q\" B", "a@b");
        ok("a@b (A B)", "A B", "a@b");
        ok("A (c) <a@b> (d)", "A (c) (d)", "a@b");
        ok("A <a@b> x", "A x", "a@b");
        ok("A <>", "A", "");
        ok("A <a>", "A", "a");
        ok("A <a b>", "A", "a b");
        ok("A <a", "", "");
        ok("bogosity - a list", "", "");
        ok("A@B C <a@b>", "A@B", "A@B");
        ok("a@b, c@d", "a@b,", "a@b,");
        ok("(A) (B) a@b", "A) (B", "a@b");
        ok("a@b ()", "a@b", "a@b");
        ok("A\\\\B <a@b>", "A\\\\B", "a@b");
        ok("A <<a@b>>", "a@b", "a@b");
        ok("A<a@b>B", "A B", "a@b");
        ok("a@b<c>", "a@b<c", "a@b<c");
        ok("(A \"x\") a@b", "A \"x\"", "a@b");
        ok("A (<c@d>)", "A ( )", "c@d");
        ok("(A\\) B) <c@d>", "A) B", "c@d");
        ok("A \\\"B <c@d>", "A \\B", "c@d");
        ok(
            "bob \"unterminated string smith <bob@x>",
            "bob unterminated string smith",
            "bob@x",
        );
        ok(
            "bob (unterminated comment smith <bob@x>",
            "bob (unterminated comment smith",
            "bob@x",
        );
    }
}
