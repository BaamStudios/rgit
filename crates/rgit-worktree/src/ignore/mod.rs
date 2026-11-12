// SPDX-License-Identifier: MIT

//! Ignore rules (gitignore(5)).
//!
//! ```
//! use rgit_worktree::ignore::{Excludes, PatternList};
//!
//! let mut ex = Excludes::new("/nonexistent/worktree");
//! ex.add_list(PatternList::from_bytes(b"*.o\n!keep.o\nbuild/\n", None, Vec::new()));
//! assert!(ex.is_excluded(b"src/main.o", false));
//! assert!(!ex.is_excluded(b"src/keep.o", false));
//! assert!(!ex.is_excluded(b"build", false));
//! assert!(ex.is_excluded(b"build", true));
//! // check-ignore: a file inside an excluded directory is excluded.
//! let m = ex.match_with_leading_dirs(b"build/keep.o", false).unwrap();
//! assert_eq!(m.pattern.text(), b"build/");
//! ```

mod pattern;
mod stack;

use std::ffi::OsString;
use std::path::PathBuf;

pub use pattern::{Pattern, PatternList};
pub use stack::{Excludes, MAX_PATTERN_FILE, Match};

/// The default of `core.excludesFile`: `$XDG_CONFIG_HOME/git/ignore`, or
/// `$HOME/.config/git/ignore` when `XDG_CONFIG_HOME` is unset or empty.
/// `None` without either variable. `env` looks up environment variables.
pub fn default_excludes_file(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    if let Some(xdg) = env("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        let mut p = PathBuf::from(xdg);
        p.push("git");
        p.push("ignore");
        return Some(p);
    }
    let home = env("HOME").filter(|v| !v.is_empty())?;
    let mut p = PathBuf::from(home);
    p.push(".config");
    p.push("git");
    p.push("ignore");
    Some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_file() {
        fn env(
            xdg: Option<&'static str>,
            home: Option<&'static str>,
        ) -> impl Fn(&str) -> Option<OsString> {
            move |k: &str| match k {
                "XDG_CONFIG_HOME" => xdg.map(OsString::from),
                "HOME" => home.map(OsString::from),
                _ => None,
            }
        }
        assert_eq!(
            default_excludes_file(env(Some("/x"), Some("/h"))),
            Some(PathBuf::from("/x/git/ignore"))
        );
        assert_eq!(
            default_excludes_file(env(Some(""), Some("/h"))),
            Some(PathBuf::from("/h/.config/git/ignore"))
        );
        assert_eq!(default_excludes_file(env(None, None)), None);
    }
}
