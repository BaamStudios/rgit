// SPDX-License-Identifier: MIT
//! Reading a patch file: an mbox message as `format-patch` writes it, or
//! the older two-line format (first line Cc, second line subject).

/// A parsed patch file.
#[derive(Debug, Default, Clone)]
pub struct Patch {
    /// Header fields in order, unfolded (`name`, `value`).
    pub headers: Vec<(String, String)>,
    /// Everything after the header (the commit message and the diff).
    pub body: Vec<u8>,
    /// Line number (1-based) of the first body line in the file.
    pub body_line: usize,
    /// The file used the two-line format.
    pub lots: bool,
}

impl Patch {
    pub fn parse(data: &[u8]) -> Patch {
        let mut p = Patch::default();
        let mut pos = 0;
        let mut line_no = 0;
        let next_line = |pos: &mut usize| -> Option<&[u8]> {
            if *pos >= data.len() {
                return None;
            }
            let end = data[*pos..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(data.len(), |i| *pos + i + 1);
            let line = &data[*pos..end];
            *pos = end;
            Some(line)
        };
        let peek = |pos: usize| -> Option<&[u8]> {
            if pos >= data.len() {
                return None;
            }
            let end = data[pos..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(data.len(), |i| pos + i + 1);
            Some(&data[pos..end])
        };
        // The mbox separator line.
        if let Some(first) = peek(pos)
            && first.starts_with(b"From ")
        {
            next_line(&mut pos);
            line_no += 1;
        }
        let looks_like_header = |l: &[u8]| {
            let colon = l.iter().position(|&b| b == b':');
            colon.is_some_and(|c| {
                c > 0
                    && l[..c]
                        .iter()
                        .all(|&b| b.is_ascii_graphic() && b != b':')
            })
        };
        match peek(pos) {
            Some(l) if !looks_like_header(strip_eol(l)) && !strip_eol(l).is_empty() => {
                // Two-line format: Cc on the first line, subject on the second.
                p.lots = true;
                let cc = next_line(&mut pos).map(strip_eol).unwrap_or_default();
                line_no += 1;
                p.headers
                    .push(("Cc".into(), String::from_utf8_lossy(cc).into_owned()));
                if let Some(subj) = next_line(&mut pos) {
                    line_no += 1;
                    let s = String::from_utf8_lossy(strip_eol(subj)).into_owned();
                    let s = s.strip_prefix("Subject:").map_or(s.clone(), |x| x.trim_start().to_string());
                    p.headers.push(("Subject".into(), s));
                }
                p.body = data[pos..].to_vec();
                p.body_line = line_no + 1;
                return p;
            }
            _ => {}
        }
        while let Some(line) = next_line(&mut pos) {
            line_no += 1;
            let l = strip_eol(line);
            if l.is_empty() {
                break;
            }
            if l.starts_with(b" ") || l.starts_with(b"\t") {
                if let Some(last) = p.headers.last_mut() {
                    last.1.push(' ');
                    last.1.push_str(String::from_utf8_lossy(l).trim_start());
                }
                continue;
            }
            let text = String::from_utf8_lossy(l).into_owned();
            match text.split_once(':') {
                Some((name, value)) => p
                    .headers
                    .push((name.to_string(), value.trim_start().to_string())),
                None => p.headers.push((text, String::new())),
            }
        }
        p.body = data[pos..].to_vec();
        p.body_line = line_no + 1;
        p
    }

    /// The first value of header `name` (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// All values of header `name`.
    pub fn header_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.headers
            .iter()
            .filter(move |(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn subject(&self) -> Option<&str> {
        self.header("Subject")
    }

    /// The charset of a `Content-Type` header, if any.
    pub fn charset(&self) -> Option<String> {
        let ct = self.header("Content-Type")?;
        ct.split(';').find_map(|p| {
            let (k, v) = p.trim().split_once('=')?;
            k.trim()
                .eq_ignore_ascii_case("charset")
                .then(|| v.trim().trim_matches('"').to_string())
        })
    }
}

pub fn strip_eol(l: &[u8]) -> &[u8] {
    let l = l.strip_suffix(b"\n").unwrap_or(l);
    l.strip_suffix(b"\r").unwrap_or(l)
}

/// Whether `data` has bytes outside ASCII.
pub fn has_8bit(data: &[u8]) -> bool {
    !data.is_ascii()
}

/// The 1-based number of the first line of `data` longer than `max`
/// bytes (without the line ending).
pub fn first_long_line(data: &[u8], max: usize) -> Option<usize> {
    data.split(|&b| b == b'\n')
        .position(|l| strip_eol(l).len() > max)
        .map(|i| i + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mbox_headers_and_body() {
        let p = Patch::parse(
            b"From 123 Mon Sep 17 00:00:00 2001\nFrom: A <a@x>\nSubject: [PATCH] x\nCc: One <1@x>,\n    two@x\n\nbody\n",
        );
        assert_eq!(p.header("from"), Some("A <a@x>"));
        assert_eq!(p.header("Cc"), Some("One <1@x>, two@x"));
        assert_eq!(p.subject(), Some("[PATCH] x"));
        assert_eq!(p.body, b"body\n");
        assert_eq!(p.body_line, 7);
        assert!(!p.lots);
    }

    #[test]
    fn two_line_format() {
        let p = Patch::parse(b"cc@x\nThe subject\nbody\n");
        assert!(p.lots);
        assert_eq!(p.header("Cc"), Some("cc@x"));
        assert_eq!(p.subject(), Some("The subject"));
        let p = Patch::parse(b"This is the body.\n");
        assert!(p.lots);
        assert_eq!(p.subject(), None);
    }

    #[test]
    fn charsets_and_long_lines() {
        let p = Patch::parse(b"Content-Type: text/plain; charset=\"UTF-8\"\n\nx\n");
        assert_eq!(p.charset().as_deref(), Some("UTF-8"));
        assert_eq!(first_long_line(b"a\nbbbb\nc", 3), Some(2));
    }
}
