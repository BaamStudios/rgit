// SPDX-License-Identifier: MIT
//! Cutting an mbox into messages (Documentation/git-mailsplit.adoc).
//!
//! Behaviour determined with `git mailsplit`:
//!
//! - Lines consisting only of whitespace before the first message are
//!   skipped. A mailbox with nothing else is empty.
//! - The first line must be a `From ` line (see [`is_from_line`]),
//!   otherwise the mailbox is corrupt, unless a bare message is allowed
//!   (`-b`): then the whole input is one message.
//! - Every `From ` line starts a message and is kept in it.
//! - Without `--keep-cr` one `\r` before the `\n` of each line is removed.
//! - With `--mboxrd`, lines matching `^>+From ` lose one `>`.

/// How to split.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Keep `\r` before `\n`.
    pub keep_cr: bool,
    /// Undo the `>From ` quoting of mboxrd.
    pub mboxrd: bool,
    /// A file that does not start with a `From ` line is one message.
    pub allow_bare: bool,
}

/// Why a mailbox could not be split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Nothing but whitespace.
    Empty,
    /// The first line is not a `From ` line.
    Corrupt,
}

fn is_digit(line: &[u8], i: Option<usize>) -> bool {
    i.and_then(|i| line.get(i)).is_some_and(u8::is_ascii_digit)
}

/// The number at the start of `s` the way `strtol` reads it (leading
/// whitespace and a sign allowed); 0 when there is none.
fn leading_number(s: &[u8]) -> i64 {
    let mut i = 0;
    while i < s.len() && s[i].is_ascii_whitespace() {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    let mut n: i64 = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        n = n.saturating_mul(10).saturating_add(i64::from(s[i] - b'0'));
        i += 1;
    }
    if neg { -n } else { n }
}

/// Whether `line` (without its newline) starts a message: `From `, at
/// least 19 characters, and around the last `:` a time followed by a year
/// (`?d:dd:dd yyyy` with the digits at those places and a number above 90
/// after the time), as observed with `git mailsplit`.
pub fn is_from_line(line: &[u8]) -> bool {
    if line.len() < 19 || !line.starts_with(b"From ") {
        return false;
    }
    let Some(colon) = line.iter().rposition(|&b| b == b':') else {
        return false;
    };
    let ok = is_digit(line, colon.checked_sub(4))
        && is_digit(line, colon.checked_sub(2))
        && is_digit(line, colon.checked_sub(1))
        && is_digit(line, Some(colon + 1))
        && is_digit(line, Some(colon + 2));
    ok && leading_number(line.get(colon + 3..).unwrap_or_default()) > 90
}

/// One line of `data` starting at `pos` (with its newline, if any).
fn next_line(data: &[u8], pos: usize) -> &[u8] {
    match data[pos..].iter().position(|&b| b == b'\n') {
        Some(n) => &data[pos..pos + n + 1],
        None => &data[pos..],
    }
}

fn strip_newline(line: &[u8]) -> &[u8] {
    let l = line.strip_suffix(b"\n").unwrap_or(line);
    l.strip_suffix(b"\r").unwrap_or(l)
}

/// Appends `line` to `out`, removing `\r` before the newline unless
/// `keep_cr`.
fn push_line(out: &mut Vec<u8>, line: &[u8], keep_cr: bool) {
    if !keep_cr && line.ends_with(b"\r\n") {
        out.extend_from_slice(&line[..line.len() - 2]);
        out.push(b'\n');
    } else {
        out.extend_from_slice(line);
    }
}

/// Splits one mailbox into its messages.
pub fn split(data: &[u8], opts: &Options) -> Result<Vec<Vec<u8>>, Error> {
    let mut pos = 0;
    while pos < data.len() {
        let line = next_line(data, pos);
        if !line.iter().all(u8::is_ascii_whitespace) {
            break;
        }
        pos += line.len();
    }
    if pos >= data.len() {
        return Err(Error::Empty);
    }
    let first = next_line(data, pos);
    let mbox = is_from_line(strip_newline(first));
    if !mbox && !opts.allow_bare {
        return Err(Error::Corrupt);
    }
    let mut mails: Vec<Vec<u8>> = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut started = false;
    while pos < data.len() {
        let line = next_line(data, pos);
        pos += line.len();
        if mbox && is_from_line(strip_newline(line)) {
            if started {
                mails.push(std::mem::take(&mut cur));
            }
            started = true;
            push_line(&mut cur, line, opts.keep_cr);
            continue;
        }
        started = true;
        if mbox && opts.mboxrd {
            let gts = line.iter().take_while(|&&b| b == b'>').count();
            if gts > 0 && line[gts..].starts_with(b"From ") {
                push_line(&mut cur, &line[1..], opts.keep_cr);
                continue;
            }
        }
        push_line(&mut cur, line, opts.keep_cr);
    }
    mails.push(cur);
    Ok(mails)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_lines() {
        for ok in [
            "From x Mon Sep 17 00:00:00 2001",
            "From  Mon Sep 17 00:00:00 2001",
            "From x 17 00:00 2001",
            "From mailer:x Mon Sep 17 00:00:00 2001",
            "From x Mon Sep 17 0:00:00 2001",
            "From xMonSep1700:00:002001",
            "From x Mon Sep 17 00:00:00 2001 extra",
            "From x 1 00:00 +2001",
            "From  00:00:00 2001",
            "From xxxxxxxxxxxxxxxxxxx 1 00:00 91",
        ] {
            assert!(is_from_line(ok.as_bytes()), "{ok}");
        }
        for bad in [
            "From a",
            "From x Mon Sep 17 00:00:00",
            "From x Mon Sep 17 00:00:00 01",
            "From x Mon Sep 17 00-00-00 2001",
            "From x x x 00:00 2001",
            "From x 17 00:00 2001 a:b",
            "From  1 00:00 2001",
            "From xxxxxxxxxxxxxxxxxxx 1 00:00 90",
            "From x Mon Sep 17 00:00:00 -2001",
            "from x Mon Sep 17 00:00:00 2001",
        ] {
            assert!(!is_from_line(bad.as_bytes()), "{bad}");
        }
    }

    const F: &str = "From a Mon Sep 17 00:00:00 2001\n";

    #[test]
    fn splits_and_keeps_from_lines() {
        let data = format!("\n  \n{F}x\r\n{F}y\n>From z\n");
        let mails = split(data.as_bytes(), &Options::default()).unwrap();
        assert_eq!(mails.len(), 2);
        assert_eq!(mails[0], format!("{F}x\n").as_bytes());
        assert_eq!(mails[1], format!("{F}y\n>From z\n").as_bytes());
        let o = Options {
            keep_cr: true,
            mboxrd: true,
            ..Options::default()
        };
        let mails = split(data.as_bytes(), &o).unwrap();
        assert_eq!(mails[0], format!("{F}x\r\n").as_bytes());
        assert_eq!(mails[1], format!("{F}y\nFrom z\n").as_bytes());
    }

    #[test]
    fn bare_and_errors() {
        assert_eq!(split(b"", &Options::default()), Err(Error::Empty));
        assert_eq!(split(b" \n\n", &Options::default()), Err(Error::Empty));
        assert_eq!(split(b"Subject: x\n", &Options::default()), Err(Error::Corrupt));
        let o = Options {
            allow_bare: true,
            ..Options::default()
        };
        assert_eq!(
            split(b"\nSubject: x\n\nbody", &o).unwrap(),
            vec![b"Subject: x\n\nbody".to_vec()]
        );
    }
}
