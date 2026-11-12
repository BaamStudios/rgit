// SPDX-License-Identifier: MIT
//! Reference name rules (`git check-ref-format`).
//!
//! Written from `Documentation/git-check-ref-format.adoc` and checked
//! against the behaviour of the reference `git` binary.

/// Options for [`check_refname_format`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RefNameFlags {
    /// Accept names without a `/` (`--allow-onelevel`).
    pub allow_onelevel: bool,
    /// Accept a single `*` anywhere in the name (`--refspec-pattern`).
    pub refspec_pattern: bool,
}

impl RefNameFlags {
    /// No exceptions: at least two components, no `*`.
    pub const STRICT: Self = Self {
        allow_onelevel: false,
        refspec_pattern: false,
    };
    /// One-level names such as `HEAD` or `foo` are accepted.
    pub const ONELEVEL: Self = Self {
        allow_onelevel: true,
        refspec_pattern: false,
    };
}

/// Why a name was rejected by [`check_refname_format`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefNameError {
    /// The name, or a component of it, is empty (leading, trailing or
    /// doubled `/`).
    EmptyComponent,
    /// A component starts with `.`.
    LeadingDot,
    /// A component ends with `.lock`.
    LockSuffix,
    /// The name contains `..`.
    DoubleDot,
    /// The name contains `@{`.
    AtBrace,
    /// The name is the single character `@`.
    LoneAt,
    /// The name ends with `.`.
    TrailingDot,
    /// A forbidden byte (control character, space, `~ ^ : ? [ \`, or a
    /// `*` that is not allowed).
    BadCharacter(u8),
    /// The name has a single component and one-level names are not allowed.
    OneLevel,
}

/// Checks `name` against git's reference name rules.
///
/// Bytes >= 0x80 are accepted (git does not interpret them).
pub fn check_refname_format(name: &[u8], flags: RefNameFlags) -> Result<(), RefNameError> {
    if name == b"@" {
        return Err(RefNameError::LoneAt);
    }
    let mut stars_left = if flags.refspec_pattern { 1 } else { 0 };
    let mut components = 0usize;
    for component in name.split(|&b| b == b'/') {
        components += 1;
        check_component(component, &mut stars_left)?;
    }
    if name.last() == Some(&b'.') {
        return Err(RefNameError::TrailingDot);
    }
    if components < 2 && !flags.allow_onelevel {
        return Err(RefNameError::OneLevel);
    }
    Ok(())
}

fn check_component(component: &[u8], stars_left: &mut u32) -> Result<(), RefNameError> {
    if component.is_empty() {
        return Err(RefNameError::EmptyComponent);
    }
    let mut prev = 0u8;
    for &b in component {
        match b {
            0..=0x1f | 0x7f | b' ' | b'~' | b'^' | b':' | b'?' | b'[' | b'\\' => {
                return Err(RefNameError::BadCharacter(b));
            }
            b'*' => {
                if *stars_left == 0 {
                    return Err(RefNameError::BadCharacter(b));
                }
                *stars_left -= 1;
            }
            b'.' if prev == b'.' => return Err(RefNameError::DoubleDot),
            b'{' if prev == b'@' => return Err(RefNameError::AtBrace),
            _ => {}
        }
        prev = b;
    }
    if component[0] == b'.' {
        return Err(RefNameError::LeadingDot);
    }
    if component.ends_with(b".lock") {
        return Err(RefNameError::LockSuffix);
    }
    Ok(())
}

/// `true` when [`check_refname_format`] accepts `name`.
pub fn is_valid_refname(name: &[u8], flags: RefNameFlags) -> bool {
    check_refname_format(name, flags).is_ok()
}

/// `--normalize`: removes leading slashes and collapses runs of slashes,
/// then checks the result. Returns the normalized name when it is valid.
pub fn normalize_refname(name: &[u8], flags: RefNameFlags) -> Result<Vec<u8>, RefNameError> {
    let mut out = Vec::with_capacity(name.len());
    for &b in name {
        if b == b'/' && (out.is_empty() || out.last() == Some(&b'/')) {
            continue;
        }
        out.push(b);
    }
    check_refname_format(&out, flags)?;
    Ok(out)
}

/// Whether `name` can be a branch name (`check-ref-format --branch`,
/// without the `@{-N}` expansion that needs a repository).
///
/// A branch name must not start with `-`, must not be `HEAD` and
/// `refs/heads/<name>` must be a valid reference name.
pub fn check_branch_name(name: &[u8]) -> Result<(), RefNameError> {
    if name.first() == Some(&b'-') {
        return Err(RefNameError::BadCharacter(b'-'));
    }
    if name == b"HEAD" {
        return Err(RefNameError::OneLevel);
    }
    let mut full = b"refs/heads/".to_vec();
    full.extend_from_slice(name);
    check_refname_format(&full, RefNameFlags::STRICT)
}

/// Root references live directly in `$GIT_DIR` (`HEAD`, `ORIG_HEAD`,
/// `MERGE_AUTOSTASH`, ...): only upper-case ASCII letters and `_`, and
/// either `HEAD`, ending in `_HEAD`, or one of the irregular names git
/// keeps there.
pub fn is_root_ref(name: &str) -> bool {
    const IRREGULAR: &[&str] = &[
        "AUTO_MERGE",
        "BISECT_EXPECTED_REV",
        "NOTES_MERGE_PARTIAL",
        "NOTES_MERGE_REF",
        "MERGE_AUTOSTASH",
    ];
    if !is_root_ref_syntax(name) {
        return false;
    }
    name == "HEAD" || name.ends_with("_HEAD") || IRREGULAR.contains(&name)
}

/// The spelling of a root reference: non-empty, only `A-Z`, `-` and `_`.
pub fn is_root_ref_syntax(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b == b'-' || b == b'_')
}

/// Whether the reference (and its reflog) belongs to one worktree and lives
/// in that worktree's own directory rather than the common directory
/// (`gitrepository-layout(5)`): names outside `refs/` such as `HEAD`, and
/// `refs/bisect/`, `refs/worktree/`, `refs/rewritten/`.
pub fn is_per_worktree(name: &str) -> bool {
    !name.starts_with("refs/")
        || name.starts_with("refs/bisect/")
        || name.starts_with("refs/worktree/")
        || name.starts_with("refs/rewritten/")
}

/// Pseudo-refs are files in `$GIT_DIR` with a format of their own that the
/// reference store does not manage (`FETCH_HEAD`, `MERGE_HEAD`).
pub fn is_pseudo_ref(name: &str) -> bool {
    name == "FETCH_HEAD" || name == "MERGE_HEAD"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(name: &str) -> bool {
        is_valid_refname(name.as_bytes(), RefNameFlags::STRICT)
    }

    #[test]
    fn basic_rules() {
        assert!(ok("refs/heads/main"));
        assert!(ok("heads/foo"));
        assert!(!ok("foo"));
        assert!(!ok("refs/heads/a..b"));
        assert!(!ok("refs/heads/.a"));
        assert!(!ok("refs/heads/a.lock"));
        assert!(!ok("refs/heads/a."));
        assert!(!ok("/refs/heads/a"));
        assert!(!ok("refs/heads/a/"));
        assert!(!ok("refs//heads"));
        assert!(!ok("refs/a@{b"));
        assert!(ok("refs/a@b"));
        assert!(!ok("@"));
        assert!(!ok("refs/a b"));
        assert!(!ok("refs/a*"));
        assert!(is_valid_refname(
            b"refs/a*",
            RefNameFlags {
                allow_onelevel: false,
                refspec_pattern: true
            }
        ));
        assert!(!is_valid_refname(
            b"refs/a*/b*",
            RefNameFlags {
                allow_onelevel: false,
                refspec_pattern: true
            }
        ));
    }

    #[test]
    fn normalize() {
        assert_eq!(
            normalize_refname(b"//refs///heads/x", RefNameFlags::STRICT).unwrap(),
            b"refs/heads/x"
        );
        assert!(normalize_refname(b"refs/heads/x/", RefNameFlags::STRICT).is_err());
    }

    #[test]
    fn root_refs() {
        assert!(is_root_ref("HEAD"));
        assert!(is_root_ref("ORIG_HEAD"));
        assert!(is_root_ref("AUTO_MERGE"));
        assert!(!is_root_ref("FOO"));
        assert!(!is_root_ref("head"));
    }
}
