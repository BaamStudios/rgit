// SPDX-License-Identifier: MIT

//! `--pathspec-from-file` / `--pathspec-file-nul` input.

use crate::PathspecError;

/// Splits the contents of a `--pathspec-from-file` input into pathspec
/// arguments.
///
/// - Without `nul`: one pathspec per line (LF or CRLF, last terminator
///   optional). A line starting with `"` is unquoted C-style.
/// - With `nul`: NUL-terminated, taken literally (no unquoting, no CR
///   handling).
///
/// Empty entries are kept; they are rejected later by
/// [`crate::Pathspec::parse`] like an empty command line argument.
///
/// ```
/// use rgit_pathspec::parse_pathspec_file;
///
/// let items = parse_pathspec_file(b"a\r\n\"b\\101\"\n", false).unwrap();
/// assert_eq!(items, vec![b"a".to_vec(), b"bA".to_vec()]);
/// ```
///
/// # Errors
/// [`PathspecError::BadlyQuoted`] for a malformed quoted line.
pub fn parse_pathspec_file(data: &[u8], nul: bool) -> Result<Vec<Vec<u8>>, PathspecError> {
    let sep = if nul { b'\0' } else { b'\n' };
    let mut entries: Vec<&[u8]> = data.split(|&b| b == sep).collect();
    if entries.last().is_some_and(|e| e.is_empty()) {
        entries.pop();
    }
    if nul {
        return Ok(entries.into_iter().map(<[u8]>::to_vec).collect());
    }
    entries
        .into_iter()
        .map(|line| {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.first() == Some(&b'"') {
                unquote_c_style(line).ok_or_else(|| PathspecError::BadlyQuoted {
                    line: line.to_vec(),
                })
            } else {
                Ok(line.to_vec())
            }
        })
        .collect()
}

/// Removes C-style quoting from `s`, which starts with `"`. Text after the
/// closing quote is ignored. Returns `None` for malformed input.
///
/// Escapes: `\a \b \f \n \r \t \v \\ \"` and three-digit octal `\000` to
/// `\377`.
#[must_use]
pub fn unquote_c_style(s: &[u8]) -> Option<Vec<u8>> {
    let mut it = s.strip_prefix(b"\"")?.iter();
    let mut out = Vec::new();
    loop {
        match *it.next()? {
            b'"' => return Some(out),
            b'\\' => {
                let c = *it.next()?;
                out.push(match c {
                    b'a' => 0x07,
                    b'b' => 0x08,
                    b'f' => 0x0c,
                    b'n' => b'\n',
                    b'r' => b'\r',
                    b't' => b'\t',
                    b'v' => 0x0b,
                    b'\\' | b'"' => c,
                    b'0'..=b'3' => {
                        let d1 = octal(*it.next()?)?;
                        let d2 = octal(*it.next()?)?;
                        ((c - b'0') << 6) | (d1 << 3) | d2
                    }
                    _ => return None,
                });
            }
            c => out.push(c),
        }
    }
}

fn octal(c: u8) -> Option<u8> {
    matches!(c, b'0'..=b'7').then(|| c - b'0')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<Vec<u8>> {
        items.iter().map(|s| s.as_bytes().to_vec()).collect()
    }

    #[test]
    fn lines() {
        assert_eq!(
            parse_pathspec_file(b"fileA.t\nfileB.t", false).unwrap(),
            v(&["fileA.t", "fileB.t"])
        );
        assert_eq!(
            parse_pathspec_file(b"fileA.t\r\nfileB.t\r\n", false).unwrap(),
            v(&["fileA.t", "fileB.t"])
        );
        assert_eq!(parse_pathspec_file(b"a\r\r\n", false).unwrap(), v(&["a\r"]));
        assert_eq!(parse_pathspec_file(b"\n", false).unwrap(), v(&[""]));
        assert!(parse_pathspec_file(b"", false).unwrap().is_empty());
        assert_eq!(
            parse_pathspec_file(b"\"file\\101.t\"\n", false).unwrap(),
            v(&["fileA.t"])
        );
        assert_eq!(parse_pathspec_file(b"\"a\" x\n", false).unwrap(), v(&["a"]));
    }

    #[test]
    fn nul() {
        assert_eq!(
            parse_pathspec_file(b"fileA.t\0fileB.t\0", true).unwrap(),
            v(&["fileA.t", "fileB.t"])
        );
        assert_eq!(
            parse_pathspec_file(b"\"file\\101.t\"\n", true).unwrap(),
            v(&["\"file\\101.t\"\n"])
        );
    }

    #[test]
    fn bad_quotes() {
        for bad in ["\"abc", "\"a\\qbc\"", "\"\\400\"", "\"\\0\"", "\"\\x41\""] {
            let err = parse_pathspec_file(bad.as_bytes(), false).unwrap_err();
            assert_eq!(err.to_string(), format!("line is badly quoted: {bad}"));
        }
    }
}
