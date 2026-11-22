// SPDX-License-Identifier: MIT
//! Cleaning up the `Subject:` for the commit title (git-mailinfo.adoc,
//! `-k`, `-b`).
//!
//! Without `-k`, leading `Re:` (any case), `:` and bracketed strings are
//! removed until none remain, then whitespace runs become one space and
//! the ends are trimmed. With `-b` bracketed strings without the word
//! `PATCH` (case-sensitive) are kept, together with the whitespace around
//! them (determined with `git mailinfo -b`).

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// The cleaned-up title.
pub(crate) fn cleanup(subject: &[u8], keep_non_patch: bool) -> Vec<u8> {
    let mut kept: Vec<u8> = Vec::new();
    let mut i = 0;
    loop {
        let start = i;
        while i < subject.len() && is_space(subject[i]) {
            i += 1;
        }
        if !kept.is_empty() {
            kept.extend_from_slice(&subject[start..i]);
        }
        let rest = &subject[i..];
        if rest.len() >= 3 && rest[..3].eq_ignore_ascii_case(b"re:") {
            i += 3;
            continue;
        }
        if rest.first() == Some(&b':') {
            i += 1;
            continue;
        }
        if rest.first() == Some(&b'[') {
            if let Some(close) = rest.iter().position(|&b| b == b']') {
                let bracket = &rest[..=close];
                if keep_non_patch && !contains(bracket, b"PATCH") {
                    kept.extend_from_slice(bracket);
                }
                i += close + 1;
                continue;
            }
        }
        break;
    }
    kept.extend_from_slice(&subject[i..]);
    crate::from::collapse_trim(&kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str, b: bool) -> String {
        String::from_utf8(cleanup(s.as_bytes(), b)).unwrap()
    }

    #[test]
    fn plain() {
        assert_eq!(c("[PATCH] [other] [PATCH] message", false), "message");
        assert_eq!(c("Re: Re: re: x", false), "x");
        assert_eq!(c("RE: x", false), "x");
        assert_eq!(c("Re : x", false), "Re : x");
        assert_eq!(c(": : x", false), "x");
        assert_eq!(c("[PATCH x", false), "[PATCH x");
        assert_eq!(c("  a    b\tc  ", false), "a b c");
        assert_eq!(c("Fwd: x", false), "Fwd: x");
        assert_eq!(c("[PATCH]", false), "");
    }

    #[test]
    fn keep_non_patch() {
        assert_eq!(c("[other] message", true), "[other] message");
        assert_eq!(c("[PATCH] [other] message", true), "[other] message");
        assert_eq!(c("[PATCH] [PATCH] message", true), "message");
        assert_eq!(c("[other] [PATCH] message", true), "[other] message");
        assert_eq!(
            c("[PATCH] [other] [PATCH] message", true),
            "[other] message"
        );
        assert_eq!(c("[a]b c", true), "[a]b c");
        assert_eq!(c("re: [a] re: [b] c", true), "[a] [b] c");
        assert_eq!(c("[patch] x", true), "[patch] x");
        assert_eq!(c("[RFC/PATCH] x", true), "x");
    }
}
