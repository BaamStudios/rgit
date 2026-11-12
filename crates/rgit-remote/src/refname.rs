// SPDX-License-Identifier: MIT
//! Reference name validation as described in `git-check-ref-format(1)`.
//!
//! Refspec parsing needs the check with the `--allow-onelevel` and
//! `--refspec-pattern` options. This is a small local copy until a shared
//! refs crate provides it.

/// Options of the check.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RefnameFlags {
    /// Accept names without a `/` (`--allow-onelevel`).
    pub allow_onelevel: bool,
    /// Accept a single `*` anywhere (`--refspec-pattern`).
    pub refspec_pattern: bool,
}

impl RefnameFlags {
    /// The flags refspec sides are checked with.
    pub const REFSPEC: RefnameFlags = RefnameFlags {
        allow_onelevel: true,
        refspec_pattern: false,
    };

    /// [`RefnameFlags::REFSPEC`] plus one `*`.
    pub const REFSPEC_PATTERN: RefnameFlags = RefnameFlags {
        allow_onelevel: true,
        refspec_pattern: true,
    };
}

/// Whether `name` is an acceptable reference name.
pub fn check_refname_format(name: &[u8], flags: RefnameFlags) -> bool {
    if name.is_empty() || name == b"@" {
        return false;
    }
    if name.first() == Some(&b'/') || name.last() == Some(&b'/') || name.last() == Some(&b'.') {
        return false;
    }
    let mut stars = 0;
    let mut prev = 0u8;
    for &c in name {
        match c {
            0..=0x20 | 0x7f | b'~' | b'^' | b':' | b'?' | b'[' | b'\\' => return false,
            b'*' => {
                stars += 1;
                if !flags.refspec_pattern || stars > 1 {
                    return false;
                }
            }
            b'.' if prev == b'.' => return false,
            b'/' if prev == b'/' => return false,
            b'{' if prev == b'@' => return false,
            _ => {}
        }
        prev = c;
    }
    let mut components = 0;
    for comp in name.split(|&c| c == b'/') {
        components += 1;
        if comp.first() == Some(&b'.') || comp.ends_with(b".lock") {
            return false;
        }
    }
    flags.allow_onelevel || components > 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(name: &str) -> bool {
        check_refname_format(name.as_bytes(), RefnameFlags::REFSPEC_PATTERN)
    }

    #[test]
    fn rules() {
        for good in [
            "a",
            "refs/heads/main",
            "a/@",
            "@/a",
            "-a",
            "a@b",
            "a{",
            "refs/heads*/x",
            "*",
            "\u{c4}",
        ] {
            assert!(ok(good), "{good}");
        }
        for bad in [
            "", "@", "a b", "a~1", "a^", "a:b", "a?", "a[", "a\\b", "a@{1", "..", ".a", "a/.b",
            "a.lock", "a.lock/b", "a//b", "/a", "a/", "a.", "a..b", "**", "a*b*", "a\tb", "a\x7f",
        ] {
            assert!(!ok(bad), "{bad:?}");
        }
        assert!(!check_refname_format(b"a", RefnameFlags::default()));
        assert!(check_refname_format(b"a/b", RefnameFlags::default()));
        assert!(!check_refname_format(b"a/*", RefnameFlags::REFSPEC));
    }
}
