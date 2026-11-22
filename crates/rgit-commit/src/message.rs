// SPDX-License-Identifier: MIT
//! Commit messages: cleanup modes (Documentation/git-commit.adoc,
//! `--cleanup`), `git-stripspace(1)` rules, the subject line and the
//! empty-message rule.

/// The scissors line without its comment prefix (git-commit(1),
/// `--cleanup=scissors`).
pub const SCISSORS: &str = " ------------------------ >8 ------------------------";

/// How the message is cleaned up (`--cleanup=<mode>`, `commit.cleanup`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cleanup {
    /// `strip` when the message is edited, `whitespace` otherwise.
    Default,
    /// Leave the message alone.
    Verbatim,
    /// Strip leading and trailing empty lines, trailing whitespace and
    /// repeated empty lines.
    Whitespace,
    /// `whitespace` and remove comment lines.
    Strip,
    /// `whitespace`, and when edited cut everything from the scissors line.
    Scissors,
}

impl Cleanup {
    /// Parse a mode name; `None` for an unknown one.
    pub fn parse(name: &str) -> Option<Cleanup> {
        Some(match name {
            "default" => Cleanup::Default,
            "verbatim" => Cleanup::Verbatim,
            "whitespace" => Cleanup::Whitespace,
            "strip" => Cleanup::Strip,
            "scissors" => Cleanup::Scissors,
            _ => return None,
        })
    }

    /// The effective mode once it is known whether the editor runs.
    pub fn resolve(self, edited: bool) -> Cleanup {
        match self {
            Cleanup::Default if edited => Cleanup::Strip,
            Cleanup::Default => Cleanup::Whitespace,
            Cleanup::Scissors if !edited => Cleanup::Whitespace,
            m => m,
        }
    }
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r')
}

/// Lines of `text` without their newlines (a final line without newline
/// counts; an empty text has no lines).
fn lines(text: &[u8]) -> impl Iterator<Item = &[u8]> {
    let body = text.strip_suffix(b"\n").unwrap_or(text);
    let empty = text.is_empty();
    body.split(|&b| b == b'\n').filter(move |_| !empty)
}

fn trim_end(line: &[u8]) -> &[u8] {
    let end = line
        .iter()
        .rposition(|&b| !is_space(b))
        .map_or(0, |p| p + 1);
    &line[..end]
}

/// `git stripspace`: remove trailing whitespace, collapse runs of empty
/// lines into one, drop empty lines at the start and end, end with a
/// newline. With `comment`, lines starting with it are removed first.
pub fn stripspace(text: &[u8], comment: Option<&[u8]>) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut empties = 0usize;
    for line in lines(text) {
        if let Some(c) = comment {
            if !c.is_empty() && line.starts_with(c) {
                continue;
            }
        }
        let line = trim_end(line);
        if line.is_empty() {
            empties += 1;
            continue;
        }
        if empties > 0 && !out.is_empty() {
            out.push(b'\n');
        }
        empties = 0;
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    out
}

/// `git stripspace -c`: prefix every line with `comment` and a space (just
/// `comment` for an empty line, no space before a tab); every line ends
/// with a newline.
pub fn comment_lines(text: &[u8], comment: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + text.len() / 8 + 8);
    for line in lines(text) {
        out.extend_from_slice(comment);
        if !line.is_empty() && line[0] != b'\t' {
            out.push(b' ');
        }
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    out
}

/// The scissors line for the comment string `comment`.
pub fn scissors_line(comment: &str) -> String {
    format!("{comment}{SCISSORS}")
}

/// Cut `text` at the scissors line (the whole line and everything after
/// it go). Returns whether a line was found.
pub fn cut_at_scissors(text: &mut Vec<u8>, comment: &str) -> bool {
    let line = scissors_line(comment);
    let mut start = 0;
    while start <= text.len() {
        let end = text[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(text.len(), |p| start + p);
        if &text[start..end] == line.as_bytes() {
            text.truncate(start);
            return true;
        }
        if end == text.len() {
            break;
        }
        start = end + 1;
    }
    false
}

/// Apply the resolved cleanup `mode` with the comment string `comment`.
pub fn cleanup(text: &[u8], mode: Cleanup, comment: &str) -> Vec<u8> {
    match mode {
        Cleanup::Verbatim => text.to_vec(),
        Cleanup::Whitespace | Cleanup::Default => stripspace(text, None),
        Cleanup::Strip => stripspace(text, Some(comment.as_bytes())),
        Cleanup::Scissors => {
            let mut t = text.to_vec();
            cut_at_scissors(&mut t, comment);
            stripspace(&t, None)
        }
    }
}

/// Whether `git commit` treats the cleaned message as empty: nothing but
/// blank lines, `Signed-off-by: ` lines and (with `comment`) comment lines.
pub fn is_empty(text: &[u8], comment: Option<&[u8]>) -> bool {
    lines(text).all(|line| {
        trim_end(line).iter().all(|&b| is_space(b))
            || line.starts_with(b"Signed-off-by: ")
            || comment.is_some_and(|c| !c.is_empty() && line.starts_with(c))
    })
}

/// The subject (`%s`): the first paragraph with its lines joined by spaces.
pub fn subject(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut started = false;
    for line in lines(text) {
        let line = trim_end(line);
        if line.is_empty() {
            if started {
                break;
            }
            continue;
        }
        if started {
            out.push(b' ');
        }
        started = true;
        out.extend_from_slice(line);
    }
    out
}

/// The first line of the message (for reflog entries).
pub fn first_line(text: &[u8]) -> &[u8] {
    match text.iter().position(|&b| b == b'\n') {
        Some(p) => &text[..p],
        None => text,
    }
}

/// Candidates for `core.commentChar=auto`, in order of preference.
pub const AUTO_COMMENT_CHARS: &[u8] = b"#;@!$%^&|:";

/// The comment character `core.commentChar=auto` picks for `text`: the
/// first candidate that starts no line; `None` when all are used.
pub fn auto_comment_char(text: &[u8]) -> Option<u8> {
    AUTO_COMMENT_CHARS
        .iter()
        .copied()
        .find(|&c| !lines(text).any(|l| l.first() == Some(&c)))
}

/// The code point of the UTF-8 sequence at the start of `b` and its
/// length; `None` for an invalid, overlong, surrogate or too large one.
fn next_char(b: &[u8]) -> Option<(char, usize)> {
    let n = match *b.first()? {
        0x00..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return None,
    };
    let s = std::str::from_utf8(b.get(..n)?).ok()?;
    s.chars().next().map(|c| (c, n))
}

/// Unicode non-characters (U+FDD0..U+FDEF and the last two code points
/// of every plane), which git does not accept in UTF-8 messages.
fn is_nonchar(c: char) -> bool {
    let v = u32::from(c);
    (0xfdd0..=0xfdef).contains(&v) || v & 0xfffe == 0xfffe
}

/// Whether `msg` is valid UTF-8 without non-characters.
#[must_use]
pub fn is_utf8(msg: &[u8]) -> bool {
    let mut i = 0;
    while i < msg.len() {
        match next_char(&msg[i..]) {
            Some((c, n)) if !is_nonchar(c) => i += n,
            _ => return false,
        }
    }
    true
}

/// `msg` with every byte that does not start a valid character
/// ([`is_utf8`]) converted from ISO-8859-1 to UTF-8.
#[must_use]
pub fn fix_utf8(msg: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(msg.len() + 8);
    let mut i = 0;
    while i < msg.len() {
        match next_char(&msg[i..]) {
            Some((c, n)) if !is_nonchar(c) => {
                out.extend_from_slice(&msg[i..i + n]);
                i += n;
            }
            _ => {
                let mut buf = [0; 2];
                out.extend_from_slice(char::from(msg[i]).encode_utf8(&mut buf).as_bytes());
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stripspace_rules() {
        assert_eq!(stripspace(b"", None), b"");
        assert_eq!(stripspace(b"\n\n  \n", None), b"");
        assert_eq!(stripspace(b"\n a  \n\n\n\nb\t\n\n", None), b" a\n\nb\n");
        assert_eq!(stripspace(b"a", None), b"a\n");
        assert_eq!(stripspace(b"a\n#c\n\nb", Some(b"#")), b"a\n\nb\n");
        assert_eq!(stripspace(b"#c\na\n#d\n", Some(b"#")), b"a\n");
        assert_eq!(stripspace(b"a\n#c\nb", None), b"a\n#c\nb\n");
    }

    #[test]
    fn commented() {
        assert_eq!(comment_lines(b"", b"#"), b"");
        assert_eq!(comment_lines(b"foo", b"#"), b"# foo\n");
        assert_eq!(comment_lines(b"foo\n\n", b";"), b"; foo\n;\n");
        assert_eq!(
            comment_lines(b"\tone\n\n two\n", b"#"),
            b"#\tone\n#\n#  two\n"
        );
    }

    #[test]
    fn scissors() {
        let mut t = b"msg\n# ------------------------ >8 ------------------------\nrest\n".to_vec();
        assert!(cut_at_scissors(&mut t, "#"));
        assert_eq!(t, b"msg\n");
        let mut t = b"no line\n".to_vec();
        assert!(!cut_at_scissors(&mut t, "#"));
    }

    #[test]
    fn modes() {
        assert_eq!(Cleanup::Default.resolve(true), Cleanup::Strip);
        assert_eq!(Cleanup::Default.resolve(false), Cleanup::Whitespace);
        assert_eq!(Cleanup::Scissors.resolve(false), Cleanup::Whitespace);
        assert_eq!(cleanup(b" x \n", Cleanup::Verbatim, "#"), b" x \n");
        assert_eq!(cleanup(b"#x\ny\n", Cleanup::Strip, "#"), b"y\n");
        assert_eq!(cleanup(b"#x\ny\n", Cleanup::Whitespace, "#"), b"#x\ny\n");
    }

    #[test]
    fn emptiness() {
        assert!(is_empty(b"", None));
        assert!(is_empty(b"\t \n\nSigned-off-by: x\n", None));
        assert!(!is_empty(b"a\n", None));
        assert!(is_empty(b"#a\n", Some(b"#")));
        assert!(!is_empty(b"#a\n", None));
    }

    #[test]
    fn subjects() {
        assert_eq!(subject(b"first\nline two\n\nbody\n"), b"first line two");
        assert_eq!(subject(b"\n\nx\n"), b"x");
        assert_eq!(subject(b""), b"");
        assert_eq!(first_line(b"a\nb\n"), b"a");
    }

    #[test]
    fn auto_char() {
        assert_eq!(auto_comment_char(b"a\n"), Some(b'#'));
        assert_eq!(auto_comment_char(b"#a\n;b\n"), Some(b'@'));
    }

    #[test]
    fn utf8_check_and_fix() {
        assert!(is_utf8("caf\u{e9}\n".as_bytes()));
        assert!(!is_utf8(b"\xed\xa0\x80"));
        assert!(!is_utf8(b"\xc0\xa0"));
        assert!(!is_utf8(b"\xf4\x8f\xbf\xbe"));
        assert!(!is_utf8(b"\xef\xb7\x90"));
        assert_eq!(fix_utf8(b"a \xff \xc3\xa9"), "a \u{ff} \u{e9}".as_bytes());
        assert_eq!(fix_utf8(b"\xe2\x82 x"), "\u{e2}\u{82} x".as_bytes());
    }
}
