// SPDX-License-Identifier: MIT
//! Path glob matching as described in gitignore(5), used by `includeIf`.
//!
//! Written from the documented semantics: `*` and `?` and `[...]` do not
//! match `/`; `**` as a whole path component matches any number of
//! directories (`**/x`, `x/**`, `a/**/b`); elsewhere `**` acts like `*`.
//! A shared implementation may replace this once the ignore/attributes code
//! (rgit-worktree) has one.

/// Match `text` against `pattern` in pathname mode.
pub fn wildmatch(pattern: &[u8], text: &[u8], icase: bool) -> bool {
    Matcher { pattern, icase }.matches(0, text)
}

struct Matcher<'a> {
    pattern: &'a [u8],
    icase: bool,
}

impl Matcher<'_> {
    fn eq(&self, a: u8, b: u8) -> bool {
        if self.icase {
            a.eq_ignore_ascii_case(&b)
        } else {
            a == b
        }
    }

    fn matches(&self, pi: usize, text: &[u8]) -> bool {
        let p = self.pattern;
        if pi == p.len() {
            return text.is_empty();
        }
        match p[pi] {
            b'*' => {
                let mut end = pi;
                while end < p.len() && p[end] == b'*' {
                    end += 1;
                }
                let component_start = pi == 0 || p[pi - 1] == b'/';
                let component_end = end == p.len() || p[end] == b'/';
                if end - pi >= 2 && component_start && component_end {
                    if end == p.len() {
                        // Trailing `**`: everything.
                        return true;
                    }
                    // `**/`: zero or more leading directories.
                    let rest = end + 1;
                    if self.matches(rest, text) {
                        return true;
                    }
                    return text
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| **c == b'/')
                        .any(|(i, _)| self.matches(rest, &text[i + 1..]));
                }
                for i in 0..=text.len() {
                    if self.matches(end, &text[i..]) {
                        return true;
                    }
                    if i < text.len() && text[i] == b'/' {
                        return false;
                    }
                }
                false
            }
            b'?' => !text.is_empty() && text[0] != b'/' && self.matches(pi + 1, &text[1..]),
            b'[' => {
                let Some(&c) = text.first() else { return false };
                if c == b'/' {
                    return false;
                }
                match self.class(pi + 1, c) {
                    Some((true, next)) => self.matches(next, &text[1..]),
                    _ => false,
                }
            }
            b'\\' if pi + 1 < p.len() => {
                !text.is_empty() && self.eq(p[pi + 1], text[0]) && self.matches(pi + 2, &text[1..])
            }
            c => !text.is_empty() && self.eq(c, text[0]) && self.matches(pi + 1, &text[1..]),
        }
    }

    /// Evaluate a bracket expression starting after `[`. Returns whether
    /// `c` matched and the pattern index after `]`; `None` if unterminated.
    fn class(&self, mut i: usize, c: u8) -> Option<(bool, usize)> {
        let p = self.pattern;
        let negate = i < p.len() && (p[i] == b'!' || p[i] == b'^');
        if negate {
            i += 1;
        }
        let mut matched = false;
        let mut first = true;
        loop {
            if i >= p.len() {
                return None;
            }
            let mut ch = p[i];
            if ch == b']' && !first {
                return Some((matched != negate, i + 1));
            }
            first = false;
            if ch == b'[' && p.get(i + 1) == Some(&b':') {
                if let Some(len) = p[i + 2..].windows(2).position(|w| w == b":]") {
                    let name = &p[i + 2..i + 2 + len];
                    matched |= posix_class(name, c, self.icase);
                    i += 2 + len + 2;
                    continue;
                }
            }
            if ch == b'\\' {
                i += 1;
                ch = *p.get(i)?;
            }
            if p.get(i + 1) == Some(&b'-') && p.get(i + 2).is_some_and(|&e| e != b']') {
                let mut hi = p[i + 2];
                let mut skip = 3;
                if hi == b'\\' {
                    hi = *p.get(i + 3)?;
                    skip = 4;
                }
                let in_range = |x: u8| ch <= x && x <= hi;
                matched |= in_range(c)
                    || (self.icase
                        && (in_range(c.to_ascii_lowercase()) || in_range(c.to_ascii_uppercase())));
                i += skip;
                continue;
            }
            matched |= self.eq(ch, c);
            i += 1;
        }
    }
}

fn posix_class(name: &[u8], c: u8, icase: bool) -> bool {
    match name {
        b"alnum" => c.is_ascii_alphanumeric(),
        b"alpha" => c.is_ascii_alphabetic(),
        b"blank" => c == b' ' || c == b'\t',
        b"cntrl" => c.is_ascii_control(),
        b"digit" => c.is_ascii_digit(),
        b"graph" => c.is_ascii_graphic(),
        b"lower" => c.is_ascii_lowercase() || (icase && c.is_ascii_uppercase()),
        b"print" => c.is_ascii_graphic() || c == b' ',
        b"punct" => c.is_ascii_punctuation(),
        b"space" => matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r'),
        b"upper" => c.is_ascii_uppercase() || (icase && c.is_ascii_lowercase()),
        b"xdigit" => c.is_ascii_hexdigit(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str, t: &str) -> bool {
        wildmatch(p.as_bytes(), t.as_bytes(), false)
    }

    #[test]
    fn basics() {
        assert!(m("foo", "foo"));
        assert!(!m("foo", "foo/"));
        assert!(m("f*", "foo"));
        assert!(!m("f*", "foo/bar"));
        assert!(m("f?o", "foo"));
        assert!(!m("a?b", "a/b"));
        assert!(m("[a-c]x", "bx"));
        assert!(m("[!a-c]x", "dx"));
        assert!(m("[]]x", "]x"));
        assert!(m("[[:digit:]]", "5"));
        assert!(!m("[abc", "a"));
        assert!(m("\\*", "*"));
        assert!(wildmatch(b"FOO/**", b"foo/x/y", true));
    }

    #[test]
    fn double_star() {
        assert!(m("**/foo", "foo"));
        assert!(m("**/foo", "/a/b/foo"));
        assert!(m("**/foo/**", "/home/x/foo/.git"));
        assert!(m("foo/**", "foo/a/b"));
        assert!(!m("foo/**", "foo"));
        assert!(m("a/**/b", "a/b"));
        assert!(m("a/**/b", "a/x/y/b"));
        assert!(m("**/foo/**/bar/**", "/t/foo/bar/repo/.git"));
        assert!(m("?oo-*/**", "foo-branch/a/b/c"));
        assert!(!m("?oo-*/**", "not-foo-branch/a"));
        assert!(m("a**b", "axxb"));
        assert!(!m("a**b", "a/b"));
        assert!(m("**", "a/b"));
        assert!(m("**/**", "a/b"));
    }
}
