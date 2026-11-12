// SPDX-License-Identifier: MIT
//! Reference names as far as reflogs need them: validity
//! (`git-check-ref-format(1)`, one-level names allowed) and which names are
//! private to a worktree (`gitrepository-layout(5)`).
//!
//! Interim copy until `rgit-refs` (#29) is merged; then this module defers
//! to `rgit_refs::name`.

/// Whether `name` is a valid reference name, allowing one-level names such
/// as `HEAD` (rules from `git-check-ref-format(1)`).
pub fn is_valid_refname(name: &str) -> bool {
    if name.is_empty() || name == "@" || name.starts_with('/') || name.ends_with('/') {
        return false;
    }
    if name.ends_with('.') || name.contains("..") || name.contains("@{") {
        return false;
    }
    if name.bytes().any(|b| {
        b < 0x20 || b == 0x7f || matches!(b, b' ' | b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
    }) {
        return false;
    }
    name.split('/')
        .all(|c| !c.is_empty() && !c.starts_with('.') && !c.ends_with(".lock"))
}

/// Whether the reference (and its log) lives in the worktree's own git
/// directory rather than in the common directory: `HEAD` and the other
/// names outside `refs/`, and `refs/bisect/`, `refs/worktree/`,
/// `refs/rewritten/`.
pub fn is_per_worktree(name: &str) -> bool {
    !name.starts_with("refs/")
        || name.starts_with("refs/bisect/")
        || name.starts_with("refs/worktree/")
        || name.starts_with("refs/rewritten/")
}

/// `core.logAllRefUpdates`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogAllRefUpdates {
    /// Only append to reflogs that already exist.
    False,
    /// Also create reflogs for `HEAD`, `refs/heads/`, `refs/remotes/` and
    /// `refs/notes/`.
    True,
    /// Create reflogs for every reference.
    Always,
}

impl LogAllRefUpdates {
    /// The default: `true` with a working tree, `false` in a bare repository.
    pub fn default_for(bare: bool) -> Self {
        if bare { Self::False } else { Self::True }
    }

    /// Parses a configuration value (boolean spellings or `always`).
    pub fn parse(value: Option<&str>) -> Option<Self> {
        let Some(v) = value else {
            return Some(Self::True);
        };
        match v.to_ascii_lowercase().as_str() {
            "always" => Some(Self::Always),
            "true" | "yes" | "on" | "" => Some(Self::True),
            "false" | "no" | "off" => Some(Self::False),
            n => n.parse::<i64>().ok().map(|n| if n != 0 { Self::True } else { Self::False }),
        }
    }

    /// Whether an update of `name` creates a missing reflog. Other root
    /// references such as `ORIG_HEAD` get one only with `always`.
    pub fn should_autocreate(self, name: &str) -> bool {
        match self {
            Self::False => false,
            Self::Always => true,
            Self::True => {
                name == "HEAD"
                    || name.starts_with("refs/heads/")
                    || name.starts_with("refs/remotes/")
                    || name.starts_with("refs/notes/")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names() {
        for n in ["HEAD", "lowercase", "foo/bar", "FOO-BAR", "refs/heads/main", "Mixed_HEAD"] {
            assert!(is_valid_refname(n), "{n}");
        }
        for n in [
            "refs/heads/a.lock",
            "refs/heads/.hidden",
            "refs/heads/b..c",
            "refs/heads/sp ace",
            "refs/heads/x@{y}",
            "../config",
            "@",
            "a//b",
            "a/",
            "a.",
            "",
        ] {
            assert!(!is_valid_refname(n), "{n}");
        }
    }

    #[test]
    fn autocreate() {
        let t = LogAllRefUpdates::True;
        for n in ["HEAD", "refs/heads/a", "refs/remotes/o/a", "refs/notes/n"] {
            assert!(t.should_autocreate(n), "{n}");
        }
        for n in ["ORIG_HEAD", "FOO_HEAD", "AUTO_MERGE", "refs/tags/x", "refs/stash", "refs/bisect/x"] {
            assert!(!t.should_autocreate(n), "{n}");
            assert!(LogAllRefUpdates::Always.should_autocreate(n), "{n}");
        }
        assert!(!LogAllRefUpdates::False.should_autocreate("HEAD"));
        assert_eq!(LogAllRefUpdates::default_for(true), LogAllRefUpdates::False);
        assert_eq!(LogAllRefUpdates::parse(Some("ALWAYS")), Some(LogAllRefUpdates::Always));
        assert_eq!(LogAllRefUpdates::parse(None), Some(LogAllRefUpdates::True));
        assert_eq!(LogAllRefUpdates::parse(Some("0")), Some(LogAllRefUpdates::False));
        assert_eq!(LogAllRefUpdates::parse(Some("bogus")), None);
    }
}
