// SPDX-License-Identifier: MIT
//! `--format` strings of `ls-tree` and `ls-files`: literal text, `%%`,
//! `%n`, `%xNN` and `%(<field>)` (git-ls-tree(1), git-ls-files(1)).

use crate::messages as msg;

/// A parsed piece of a format string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece<F> {
    /// Text copied as is.
    Literal(Vec<u8>),
    /// A field to interpolate.
    Field(F),
}

/// Parse `fmt` for the command `cmd` (named in error messages); `field`
/// maps a name inside `%(...)` to a field. The error is the complete
/// message (without `fatal: `).
pub fn parse<F>(
    fmt: &str,
    cmd: &str,
    field: impl Fn(&str) -> Option<F>,
) -> Result<Vec<Piece<F>>, String> {
    let mut out = Vec::new();
    let mut lit = Vec::new();
    let b = fmt.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'%' {
            lit.push(b[i]);
            i += 1;
            continue;
        }
        let rest = &fmt[i + 1..];
        let rb = rest.as_bytes();
        match rb.first() {
            Some(b'%') => {
                lit.push(b'%');
                i += 2;
                continue;
            }
            Some(b'n') => {
                lit.push(b'\n');
                i += 2;
                continue;
            }
            Some(b'x')
                if rb.len() >= 3 && rb[1].is_ascii_hexdigit() && rb[2].is_ascii_hexdigit() =>
            {
                lit.push(u8::from_str_radix(&rest[1..3], 16).expect("two hex digits"));
                i += 4;
                continue;
            }
            _ => {}
        }
        if !rest.starts_with('(') {
            return Err(msg::bad_format_open(cmd, rest));
        }
        let Some(close) = rest.find(')') else {
            return Err(msg::bad_format_close(cmd, rest));
        };
        let Some(f) = field(&rest[1..close]) else {
            return Err(msg::bad_format_field(cmd, &rest[..=close]));
        };
        if !lit.is_empty() {
            out.push(Piece::Literal(std::mem::take(&mut lit)));
        }
        out.push(Piece::Field(f));
        i += close + 2;
    }
    if !lit.is_empty() {
        out.push(Piece::Literal(lit));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(fmt: &str) -> Result<Vec<Piece<u8>>, String> {
        parse(fmt, "ls-tree", |n| (n == "path").then_some(1))
    }

    #[test]
    fn formats() {
        assert_eq!(
            p("%(path)%x09%%%n").unwrap(),
            vec![Piece::Field(1), Piece::Literal(b"\t%\n".to_vec())]
        );
        assert_eq!(
            p("%x0|%(bogus)").unwrap_err(),
            msg::bad_format_open("ls-tree", "x0|%(bogus)")
        );
        assert_eq!(
            p("%(path").unwrap_err(),
            msg::bad_format_close("ls-tree", "(path")
        );
        assert_eq!(
            p("%(path x)").unwrap_err(),
            msg::bad_format_field("ls-tree", "(path x)")
        );
        assert_eq!(p("a%").unwrap_err(), msg::bad_format_open("ls-tree", ""));
    }
}
