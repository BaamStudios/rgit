// SPDX-License-Identifier: MIT
//! Shell quoting of argument vectors as they appear in trace lines.
//!
//! Observed from git 2.56.0: an argument made only of ASCII letters, digits
//! and `-_./,:@+=^` is written as is. Every other argument (including the
//! empty one) is wrapped in single quotes; inside the quotes `'` and `!` are
//! written as `'\'` + char + `'`, i.e. the quote is closed, the character is
//! backslash-escaped and the quote is reopened.

use std::ffi::OsStr;

fn is_plain(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-_./,:@+=^".contains(&b)
}

/// Append one quoted argument to `out`.
pub fn quote_arg_into(out: &mut Vec<u8>, arg: &[u8]) {
    if !arg.is_empty() && arg.iter().copied().all(is_plain) {
        out.extend_from_slice(arg);
        return;
    }
    out.push(b'\'');
    for &b in arg {
        if b == b'\'' || b == b'!' {
            out.extend_from_slice(b"'\\");
            out.push(b);
            out.push(b'\'');
        } else {
            out.push(b);
        }
    }
    out.push(b'\'');
}

/// Quote all arguments, separated by single spaces.
pub fn quote_argv<I, S>(args: I) -> Vec<u8>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut out = Vec::new();
    for (i, a) in args.into_iter().enumerate() {
        if i > 0 {
            out.push(b' ');
        }
        quote_arg_into(&mut out, a.as_ref().as_encoded_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(args: &[&str]) -> String {
        String::from_utf8(quote_argv(args)).unwrap()
    }

    #[test]
    fn plain_and_quoted() {
        assert_eq!(q(&["git", "version"]), "git version");
        assert_eq!(q(&["a b"]), "'a b'");
        assert_eq!(q(&["it's"]), r"'it'\''s'");
        assert_eq!(q(&["!"]), r"''\!''");
        assert_eq!(q(&[""]), "''");
        assert_eq!(
            q(&["x=", "/", ".", ":", ",", "@", "+", "^", "_", "-x", "a1"]),
            "x= / . : , @ + ^ _ -x a1"
        );
        assert_eq!(
            q(&["%", "~", "*", "$", "\\", "\"", "é"]),
            "'%' '~' '*' '$' '\\' '\"' 'é'"
        );
    }
}
