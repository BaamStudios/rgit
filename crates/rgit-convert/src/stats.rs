// SPDX-License-Identifier: MIT

//! Counting line endings and telling text from binary content.
//!
//! The rules were determined with `git ls-files --eol` on files made for
//! the purpose:
//!
//! - A NUL byte or a carriage return that is not followed by a line feed
//!   ("lone CR") makes the content binary.
//! - Otherwise it is binary when it has more than one non-printable byte
//!   per 128 printable ones (`printable / 128 < non-printable`).
//!   Non-printable are the control bytes below 0x20 except backspace, tab,
//!   form feed and escape, and DEL (0x7f). Line feeds and carriage returns
//!   count as neither. Bytes from 0x80 up are printable.
//! - A single Ctrl-Z (0x1a) as the very last byte (the DOS end-of-file
//!   mark) is not counted at all; anywhere else it is non-printable.

/// What content contains.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// NUL bytes.
    pub nul: usize,
    /// Carriage returns not followed by a line feed.
    pub lone_cr: usize,
    /// Line feeds not preceded by a carriage return.
    pub lone_lf: usize,
    /// CR LF pairs.
    pub crlf: usize,
    /// Printable bytes.
    pub printable: usize,
    /// Non-printable bytes.
    pub nonprintable: usize,
}

impl Stats {
    /// Counts `data`.
    #[must_use]
    pub fn new(data: &[u8]) -> Stats {
        let mut s = Stats::default();
        let end = if data.last() == Some(&0x1a) {
            data.len() - 1
        } else {
            data.len()
        };
        let mut i = 0;
        while i < end {
            match data[i] {
                b'\r' => {
                    if data.get(i + 1) == Some(&b'\n') && i + 1 < end {
                        s.crlf += 1;
                        i += 2;
                        continue;
                    }
                    s.lone_cr += 1;
                }
                b'\n' => s.lone_lf += 1,
                0 => {
                    s.nul += 1;
                    s.nonprintable += 1;
                }
                0x08 | b'\t' | 0x0c | 0x1b => s.printable += 1,
                0x7f => s.nonprintable += 1,
                c if c < 0x20 => s.nonprintable += 1,
                _ => s.printable += 1,
            }
            i += 1;
        }
        s
    }

    /// Whether the content counts as binary (see the module documentation).
    #[must_use]
    pub fn is_binary(&self) -> bool {
        self.nul > 0 || self.lone_cr > 0 || (self.printable >> 7) < self.nonprintable
    }

    /// Whether the content is text with at least one CR LF.
    #[must_use]
    pub fn is_text_with_crlf(&self) -> bool {
        self.crlf > 0 && !self.is_binary()
    }

    /// The description `git ls-files --eol` shows: `-text`, `lf`, `crlf`,
    /// `mixed` or `none`.
    #[must_use]
    pub fn describe(&self) -> &'static str {
        if self.is_binary() {
            "-text"
        } else {
            match (self.lone_lf > 0, self.crlf > 0) {
                (true, true) => "mixed",
                (true, false) => "lf",
                (false, true) => "crlf",
                (false, false) => "none",
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts() {
        let s = Stats::new(b"a\r\nb\nc\rd\0");
        assert_eq!((s.crlf, s.lone_lf, s.lone_cr, s.nul), (1, 1, 1, 1));
        assert!(s.is_binary());
    }

    #[test]
    fn describe() {
        assert_eq!(Stats::new(b"").describe(), "none");
        assert_eq!(Stats::new(b"a\nb\n").describe(), "lf");
        assert_eq!(Stats::new(b"a\r\nb\r\n").describe(), "crlf");
        assert_eq!(Stats::new(b"a\r\nb\n").describe(), "mixed");
        assert_eq!(Stats::new(b"a\rb\n").describe(), "-text");
        assert_eq!(Stats::new(b"a\r").describe(), "-text");
    }

    #[test]
    fn threshold() {
        let mut d = vec![b'A'; 127];
        d.push(1);
        assert!(Stats::new(&d).is_binary());
        d.insert(0, b'A');
        assert!(!Stats::new(&d).is_binary());
    }

    #[test]
    fn eof_mark() {
        let mut d = vec![b'A'; 127];
        d.push(0x1a);
        assert!(!Stats::new(&d).is_binary());
        d.insert(0, 0x1a);
        assert!(Stats::new(&d).is_binary());
        assert_eq!(Stats::new(b"a\r\nb\x1a").describe(), "crlf");
    }
}
