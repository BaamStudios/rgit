// SPDX-License-Identifier: MIT
//! Normalisers for output that legitimately differs between git and rgit.
//!
//! Nothing is normalised implicitly: every test lists the normalisers it
//! needs, so each accepted difference is visible where it is accepted.

use std::fmt;
use std::sync::Arc;

use regex::Regex;

type NormalizeFn = dyn Fn(&str) -> String + Send + Sync;

/// A named text transformation applied to both sides before comparing.
#[derive(Clone)]
pub struct Normalizer {
    name: String,
    f: Arc<NormalizeFn>,
}

impl fmt::Debug for Normalizer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Normalizer").field(&self.name).finish()
    }
}

impl Normalizer {
    /// A normaliser from an arbitrary function. `name` appears in mismatch reports.
    pub fn new(
        name: impl Into<String>,
        f: impl Fn(&str) -> String + Send + Sync + 'static,
    ) -> Self {
        Normalizer {
            name: name.into(),
            f: Arc::new(f),
        }
    }

    /// Replaces every occurrence of `from` with `to`.
    pub fn replace(from: impl Into<String>, to: impl Into<String>) -> Self {
        let (from, to) = (from.into(), to.into());
        Normalizer::new(format!("replace {from:?} -> {to:?}"), move |s| {
            s.replace(&from, &to)
        })
    }

    /// Replaces every match of the regular expression `pattern` with
    /// `replacement` (which may use `$1`, `${name}`, see [`regex::Regex::replace_all`]).
    ///
    /// # Panics
    /// If `pattern` is not a valid regular expression.
    pub fn regex(pattern: &str, replacement: impl Into<String>) -> Self {
        let re = Regex::new(pattern).unwrap_or_else(|e| panic!("bad normaliser regex: {e}"));
        let replacement = replacement.into();
        Normalizer::new(format!("regex {pattern:?} -> {replacement:?}"), move |s| {
            re.replace_all(s, replacement.as_str()).into_owned()
        })
    }

    /// Removes every line (including its newline) that matches `pattern`.
    ///
    /// # Panics
    /// If `pattern` is not a valid regular expression.
    pub fn drop_lines(pattern: &str) -> Self {
        let re = Regex::new(pattern).unwrap_or_else(|e| panic!("bad normaliser regex: {e}"));
        Normalizer::new(format!("drop lines matching {pattern:?}"), move |s| {
            s.split_inclusive('\n')
                .filter(|line| !re.is_match(line.trim_end_matches('\n')))
                .collect()
        })
    }

    /// The version line: `git version 2.56.1` and `git version 2.56.1.rgit.0.1.0`
    /// both become `git version <VERSION>`.
    pub fn version_line() -> Self {
        let re = Regex::new(r"(?m)^git version [^\n]*$").expect("valid regex");
        Normalizer::new("version line", move |s| {
            re.replace_all(s, "git version <VERSION>").into_owned()
        })
    }

    /// Pack, index and bitmap file names, which contain a checksum:
    /// `pack-<hex>.pack` becomes `pack-<HASH>.pack`.
    pub fn pack_names() -> Self {
        Normalizer::regex(r"pack-[0-9a-f]{40}(?:[0-9a-f]{24})?", "pack-<HASH>")
    }

    /// The name shown in mismatch reports.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Applies the normaliser.
    pub fn apply(&self, text: &str) -> String {
        (self.f)(text)
    }
}

#[cfg(test)]
mod tests {
    use super::Normalizer;

    #[test]
    fn version_line() {
        let n = Normalizer::version_line();
        assert_eq!(n.apply("git version 2.56.0\n"), "git version <VERSION>\n");
        assert_eq!(
            n.apply("git version 2.56.1.rgit.0.1.0\n"),
            "git version <VERSION>\n"
        );
        assert_eq!(n.apply("rgit version 1\n"), "rgit version 1\n");
    }

    #[test]
    fn drop_lines() {
        let n = Normalizer::drop_lines("^hint:");
        assert_eq!(n.apply("a\nhint: x\nb"), "a\nb");
    }

    #[test]
    fn pack_names() {
        let n = Normalizer::pack_names();
        let s = format!("pack-{}.idx", "ab".repeat(20));
        assert_eq!(n.apply(&s), "pack-<HASH>.idx");
    }
}
