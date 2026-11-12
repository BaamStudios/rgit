// SPDX-License-Identifier: MIT
//! One reflog line: `<old> SP <new> SP <name> <<email>> SP <time> SP <tz> [TAB <message>] LF`.

use rgit_hash::{HashKind, ObjectId};

/// One reflog entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Value before the update (null if the reference did not exist).
    pub old: ObjectId,
    /// Value after the update.
    pub new: ObjectId,
    /// Who made the update: `Name <email>`, exactly as stored (the `>` is
    /// included).
    pub ident: Vec<u8>,
    /// Seconds since the epoch.
    pub time: u64,
    /// Offset in git's decimal `±hhmm` form (`-0700` is `-700`).
    pub tz: i32,
    /// The message, without the separating TAB and the line feed. Empty when
    /// the line carries none.
    pub message: Vec<u8>,
}

/// The committer of a reflog entry: identity plus date.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Committer {
    /// `Name <email>`.
    pub ident: Vec<u8>,
    /// Seconds since the epoch.
    pub time: u64,
    /// Offset in `±hhmm` form.
    pub tz: i32,
}

impl Committer {
    /// Builds `Name <email>` from its parts.
    pub fn new(name: &[u8], email: &[u8], time: u64, tz: i32) -> Self {
        let mut ident = Vec::with_capacity(name.len() + email.len() + 3);
        ident.extend_from_slice(name);
        ident.extend_from_slice(b" <");
        ident.extend_from_slice(email);
        ident.push(b'>');
        Committer { ident, time, tz }
    }
}

/// Whitespace for message normalisation: space, TAB, LF and CR (not VT or FF).
fn is_msg_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// Whitespace skipped before the timestamp: the C locale's `isspace` set.
fn is_c_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// Normalises a message for the reflog: leading and trailing whitespace is
/// dropped and every run of whitespace inside becomes one space, so the
/// entry stays on one line.
pub fn normalize_message(msg: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(msg.len());
    let mut pending_space = false;
    for &b in msg {
        if is_msg_space(b) {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(b' ');
            pending_space = false;
        }
        out.push(b);
    }
    out
}

impl Entry {
    /// Builds an entry for an update made by `committer`. The message is
    /// normalised with [`normalize_message`].
    pub fn new(old: ObjectId, new: ObjectId, committer: &Committer, message: &[u8]) -> Self {
        Entry {
            old,
            new,
            ident: committer.ident.clone(),
            time: committer.time,
            tz: committer.tz,
            message: normalize_message(message),
        }
    }

    /// Parses one line (without its line feed). Returns `None` for lines git
    /// skips when reading a reflog.
    pub fn parse(line: &[u8], kind: HashKind) -> Option<Entry> {
        let hl = kind.hex_len();
        if line.len() < 2 * hl + 2 || line[hl] != b' ' || line[2 * hl + 1] != b' ' {
            return None;
        }
        let old = ObjectId::from_hex(kind, &line[..hl]).ok()?;
        let new = ObjectId::from_hex(kind, &line[hl + 1..2 * hl + 1]).ok()?;
        let rest = &line[2 * hl + 2..];
        let gt = rest.iter().position(|&b| b == b'>')?;
        let ident = rest[..=gt].to_vec();
        let after = &rest[gt + 1..];
        if after.first() != Some(&b' ') {
            return None;
        }
        let mut p = 1;
        while p < after.len() && is_c_space(after[p]) {
            p += 1;
        }
        let mut negative = false;
        match after.get(p) {
            Some(b'+') => p += 1,
            Some(b'-') => {
                negative = true;
                p += 1;
            }
            _ => {}
        }
        let digits = p;
        let mut time: u64 = 0;
        let mut overflow = false;
        while let Some(&b) = after.get(p).filter(|b| b.is_ascii_digit()) {
            match time
                .checked_mul(10)
                .and_then(|t| t.checked_add(u64::from(b - b'0')))
            {
                Some(t) => time = t,
                None => overflow = true,
            }
            p += 1;
        }
        if p == digits {
            return None;
        }
        if overflow {
            time = u64::MAX;
        } else if negative {
            time = time.wrapping_neg();
        }
        if time == 0 {
            return None;
        }
        // " +hhmm"
        let tzf = after.get(p..p + 6)?;
        if tzf[0] != b' ' || !matches!(tzf[1], b'+' | b'-') {
            return None;
        }
        if !tzf[2..].iter().all(u8::is_ascii_digit) {
            return None;
        }
        let mut tz = tzf[2..]
            .iter()
            .fold(0i32, |acc, &b| acc * 10 + i32::from(b - b'0'));
        if tzf[1] == b'-' {
            tz = -tz;
        }
        p += 6;
        if after.get(p) == Some(&b'\t') {
            p += 1;
        }
        Some(Entry {
            old,
            new,
            ident,
            time,
            tz,
            message: after[p..].to_vec(),
        })
    }

    /// Parses a whole reflog file, oldest entry first. Lines git skips
    /// (malformed, empty, or an unterminated last line) are left out.
    pub fn parse_all(data: &[u8], kind: HashKind) -> Vec<Entry> {
        let mut out = Vec::new();
        let mut rest = data;
        while let Some(nl) = rest.iter().position(|&b| b == b'\n') {
            if let Some(e) = Entry::parse(&rest[..nl], kind) {
                out.push(e);
            }
            rest = &rest[nl + 1..];
        }
        out
    }

    /// The line as written to the file, including the line feed. An empty
    /// message leaves out the TAB as well.
    pub fn to_line(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(2 * self.old.kind().hex_len() + self.ident.len() + 40);
        out.extend_from_slice(self.old.to_hex().as_bytes());
        out.push(b' ');
        out.extend_from_slice(self.new.to_hex().as_bytes());
        out.push(b' ');
        out.extend_from_slice(&self.ident);
        out.push(b' ');
        out.extend_from_slice(rgit_date::format_raw(self.time, self.tz).as_bytes());
        if !self.message.is_empty() {
            out.push(b'\t');
            out.extend_from_slice(&self.message);
        }
        out.push(b'\n');
        out
    }

    /// The name part of the identity (`%gn`): everything before `<`, with
    /// trailing spaces removed.
    pub fn name(&self) -> &[u8] {
        let end = self
            .ident
            .iter()
            .position(|&b| b == b'<')
            .unwrap_or(self.ident.len());
        let mut name = &self.ident[..end];
        while let [rest @ .., b' '] = name {
            name = rest;
        }
        name
    }

    /// The e-mail part of the identity (`%ge`): between `<` and `>`.
    pub fn email(&self) -> &[u8] {
        let start = self
            .ident
            .iter()
            .position(|&b| b == b'<')
            .map_or(0, |p| p + 1);
        let end = self.ident.len().saturating_sub(1).max(start);
        &self.ident[start..end]
    }

    /// For a `checkout: moving from <from> to <to>` message, `<from>`.
    pub fn checkout_from(&self) -> Option<&[u8]> {
        let rest = self.message.strip_prefix(b"checkout: moving from ")?;
        let pos = rest.windows(4).position(|w| w == b" to ")?;
        Some(&rest[..pos])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "044914ec5a64ebe824fccd7e5e93e7e2fcb7e5d3";
    const B: &str = "d76260c32be1d2ada5abb7f70e6bdd385ac72493";

    fn line(rest: &str) -> Vec<u8> {
        format!("{A} {B} {rest}").into_bytes()
    }

    fn parse(rest: &str) -> Option<Entry> {
        Entry::parse(&line(rest), HashKind::Sha1)
    }

    #[test]
    fn normalizes_messages() {
        assert_eq!(
            normalize_message(b"  multi\n line\t tab  \n\n"),
            b"multi line tab"
        );
        assert_eq!(normalize_message(b"a\rb\x0bc\x0cd"), b"a b\x0bc\x0cd");
        assert_eq!(normalize_message(b" \n "), b"");
        assert_eq!(normalize_message(b"commit: x"), b"commit: x");
    }

    #[test]
    fn parses_standard_lines() {
        let e = parse("C O Mitter <c@x> 1112911993 -0700\tcommit: two").unwrap();
        assert_eq!(e.ident, b"C O Mitter <c@x>");
        assert_eq!(e.time, 1112911993);
        assert_eq!(e.tz, -700);
        assert_eq!(e.message, b"commit: two");
        assert_eq!(e.name(), b"C O Mitter");
        assert_eq!(e.email(), b"c@x");
        let mut l = line("C O Mitter <c@x> 1112911993 -0700\tcommit: two");
        l.push(b'\n');
        assert_eq!(e.to_line(), l);
    }

    #[test]
    fn empty_message_has_no_tab() {
        let e = parse("N <e> 5 +0100").unwrap();
        assert!(e.message.is_empty());
        assert_eq!(e.to_line(), format!("{A} {B} N <e> 5 +0100\n").into_bytes());
    }

    #[test]
    fn tolerance_matches_git() {
        // accepted, with the message git reports
        let cases: &[(&str, &[u8], u64, i32)] = &[
            ("N <e> 1 +0000 junk", b" junk", 1, 0),
            ("N <e> 1 +0000junk\tm", b"junk\tm", 1, 0),
            ("N <e> 1 +00000\tm", b"0\tm", 1, 0),
            ("N <e> 1 +0000 \tm", b" \tm", 1, 0),
            ("N <e>  1 +0000\tm", b"m", 1, 0),
            ("N <e> \t5 +0000\tm", b"m", 5, 0),
            ("N <e> +5 +0000\tm", b"m", 5, 0),
            ("N <e> -5 +0000\tm", b"m", u64::MAX - 4, 0),
            ("N <e> 99999999999999999999 +0000\tm", b"m", u64::MAX, 0),
            ("N <e> 5 +9999\tm", b"m", 5, 9999),
            ("N <e> 5 -0000\tm", b"m", 5, 0),
            ("N <e> 5 +0000\t\tm", b"\tm", 5, 0),
            ("N <e> 5 +0000\r", b"\r", 5, 0),
            ("<e> 5 +0000\tm", b"m", 5, 0),
        ];
        for (rest, msg, time, tz) in cases {
            let e = parse(rest).unwrap_or_else(|| panic!("rejected {rest:?}"));
            assert_eq!(&e.message[..], *msg, "{rest:?}");
            assert_eq!(e.time, *time, "{rest:?}");
            assert_eq!(e.tz, *tz, "{rest:?}");
        }
        for rest in [
            "N <e> 1 +000\tm",
            "N <e> 1 0000\tm",
            "N <e>1 +0000\tm",
            "N <e> 1  +0000\tm",
            "N e 1 +0000\tm",
            "N <e> 0 +0000\tm",
            "N <e> 00 +0000\tm",
            "N <a>b> 1 +0000\tm",
            "N <e> 5x +0000\tm",
            "N <e> 5 +12a4\tm",
            "N <e> 5 +-123\tm",
            "N <e> 5",
        ] {
            assert!(parse(rest).is_none(), "accepted {rest:?}");
        }
        assert!(
            Entry::parse(format!("{A}  {B} N <e> 1 +0000").as_bytes(), HashKind::Sha1).is_none()
        );
        assert!(
            Entry::parse(
                format!("{} {B} N <e> 1 +0000", &A[..39]).as_bytes(),
                HashKind::Sha1
            )
            .is_none()
        );
        let upper = A.to_ascii_uppercase();
        assert!(
            Entry::parse(
                format!("{upper} {B} N <e> 1 +0000").as_bytes(),
                HashKind::Sha1
            )
            .is_some()
        );
    }

    #[test]
    fn parse_all_skips_bad_and_unterminated_lines() {
        let data =
            format!("garbage\n{A} {B} N <e> 1 +0000\tm1\n\n{A} {B} bad\n{A} {B} N <e> 2 +0000\tm2");
        let all = Entry::parse_all(data.as_bytes(), HashKind::Sha1);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].message, b"m1");
    }

    #[test]
    fn identity_parts() {
        let e = parse(" C O  Mitter  <c@x> 5 +0000").unwrap();
        assert_eq!(e.name(), b" C O  Mitter");
        assert_eq!(e.email(), b"c@x");
    }

    #[test]
    fn checkout_messages() {
        let mut e = parse("N <e> 5 +0000\tcheckout: moving from main to topic").unwrap();
        assert_eq!(e.checkout_from(), Some(&b"main"[..]));
        e.message = b"commit: x".to_vec();
        assert_eq!(e.checkout_from(), None);
    }

    #[test]
    fn committer_ident() {
        let c = Committer::new(b"C O Mitter", b"c@x", 7, -700);
        let e = Entry::new(
            HashKind::Sha1.null(),
            ObjectId::from_hex(HashKind::Sha1, B.as_bytes()).unwrap(),
            &c,
            b" commit (initial): one\n",
        );
        assert_eq!(
            e.to_line(),
            format!("0000000000000000000000000000000000000000 {B} C O Mitter <c@x> 7 -0700\tcommit (initial): one\n")
                .into_bytes()
        );
    }
}
