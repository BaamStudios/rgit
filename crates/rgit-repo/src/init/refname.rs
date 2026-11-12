// SPDX-License-Identifier: MIT
//! Reference name rules from `git-check-ref-format(1)`, as far as `init`
//! needs them to accept an initial branch name.

/// Whether `name` is a well-formed reference name (the rules listed in
/// `git-check-ref-format(1)`, without `--allow-onelevel`,
/// `--refspec-pattern` or `--normalize`).
pub fn is_valid_ref_name(name: &[u8]) -> bool {
    if name.is_empty() || name == b"@" {
        return false;
    }
    if name.starts_with(b"/") || name.ends_with(b"/") || name.ends_with(b".") {
        return false;
    }
    if !name.contains(&b'/') {
        return false;
    }
    if name
        .windows(2)
        .any(|w| w == b".." || w == b"//" || w == b"@{")
    {
        return false;
    }
    let forbidden = |c: u8| {
        c < 0x20 || c == 0x7f || matches!(c, b' ' | b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
    };
    if name.iter().any(|&c| forbidden(c)) {
        return false;
    }
    name.split(|&c| c == b'/')
        .all(|comp| !comp.starts_with(b".") && !comp.ends_with(b".lock"))
}

/// Whether `refs/heads/<name>` is a valid reference name.
pub fn is_valid_branch_name(name: &str) -> bool {
    let mut full = b"refs/heads/".to_vec();
    full.extend_from_slice(name.as_bytes());
    is_valid_ref_name(&full)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_names() {
        for ok in ["main", "HEAD", "-foo", "@", "a/b", "x@y", "a.b", "über"] {
            assert!(is_valid_branch_name(ok), "{ok}");
        }
        for bad in [
            "",
            "with space",
            "foo.lock",
            "a/.b",
            ".a",
            "a..b",
            "a//b",
            "a/",
            "a.",
            "a@{b",
            "a~1",
            "a^",
            "a:b",
            "a?",
            "a*",
            "a[",
            "a\\b",
            "a\tb",
            "a\x7fb",
        ] {
            assert!(!is_valid_branch_name(bad), "{bad:?}");
        }
        assert!(!is_valid_ref_name(b"@"));
        assert!(!is_valid_ref_name(b"onelevel"));
    }
}
