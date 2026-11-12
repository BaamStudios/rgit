// SPDX-License-Identifier: MIT
//! `url.<base>.insteadOf` and `url.<base>.pushInsteadOf`
//! (`git-config(1)`): the longest matching prefix is replaced by `<base>`;
//! between equally long prefixes the first configured wins. Rewriting is
//! done once (no chains). See [`crate::remote::Remote::push_url`] for how
//! push URLs combine both.

use bstr::{BString, ByteSlice};
use rgit_config::{ConfigSet, key::split_canonical};

use crate::error::{ConfigError, string_value};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    base: BString,
    prefix: BString,
}

/// The rewrite rules of a configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UrlRewrites {
    fetch: Vec<Rule>,
    push: Vec<Rule>,
}

impl UrlRewrites {
    /// Collect the rules in configuration order.
    pub fn from_config(config: &ConfigSet) -> Result<UrlRewrites, ConfigError> {
        let mut out = UrlRewrites::default();
        for e in config.entries() {
            let (section, sub, key) = split_canonical(e.name.as_bstr());
            let Some(base) = sub else { continue };
            if section != "url" {
                continue;
            }
            let list = match key.as_bytes() {
                b"insteadof" => &mut out.fetch,
                b"pushinsteadof" => &mut out.push,
                _ => continue,
            };
            list.push(Rule {
                base: base.to_owned(),
                prefix: string_value(e)?.to_owned(),
            });
        }
        Ok(out)
    }

    /// Whether there are no rules.
    pub fn is_empty(&self) -> bool {
        self.fetch.is_empty() && self.push.is_empty()
    }

    /// Apply `insteadOf`.
    pub fn rewrite(&self, url: &[u8]) -> BString {
        apply(&self.fetch, url).unwrap_or_else(|| url.into())
    }

    /// Apply `pushInsteadOf`; `None` when no rule matches.
    pub fn rewrite_push(&self, url: &[u8]) -> Option<BString> {
        apply(&self.push, url)
    }
}

fn apply(rules: &[Rule], url: &[u8]) -> Option<BString> {
    let mut best: Option<&Rule> = None;
    for r in rules {
        if url.starts_with(&r.prefix) && best.is_none_or(|b| r.prefix.len() > b.prefix.len()) {
            best = Some(r);
        }
    }
    best.map(|r| {
        let mut out = r.base.clone();
        out.extend_from_slice(&url[r.prefix.len()..]);
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgit_config::{Origin, Scope, parse_bytes};

    fn rewrites(text: &str) -> UrlRewrites {
        let cfg = parse_bytes(text.as_bytes(), Origin::Stdin, Scope::Local).unwrap();
        UrlRewrites::from_config(&cfg).unwrap()
    }

    #[test]
    fn longest_first_wins() {
        let rw = rewrites(
            "[url \"B\"]\n insteadOf = x\n[url \"A\"]\n insteadOf = x\n[url \"C\"]\n insteadOf = xy\n\
             [url \"P\"]\n pushInsteadOf = x\n[url \"Q\"]\n pushInsteadOf = xyz\n[url \"Y\"]\n insteadOf = y\n",
        );
        assert_eq!(rw.rewrite(b"xfoo"), "Bfoo");
        assert_eq!(rw.rewrite(b"xyfoo"), "Cfoo");
        assert_eq!(rw.rewrite(b"other"), "other");
        assert_eq!(rw.rewrite_push(b"xyzfoo").unwrap(), "Qfoo");
        assert_eq!(rw.rewrite_push(b"xyq").unwrap(), "Pyq");
        assert_eq!(rw.rewrite_push(b"yfoo"), None);
    }

    #[test]
    fn empty_prefix_matches_everything() {
        let rw = rewrites("[url \"E\"]\n insteadOf =\n");
        assert_eq!(rw.rewrite(b"other"), "Eother");
    }

    #[test]
    fn missing_value() {
        let cfg = parse_bytes(b"[url \"E\"]\n insteadOf\n", Origin::Stdin, Scope::Local).unwrap();
        let err = UrlRewrites::from_config(&cfg).unwrap_err();
        assert_eq!(err.report()[0].1, "missing value for 'url.E.insteadof'");
    }
}
