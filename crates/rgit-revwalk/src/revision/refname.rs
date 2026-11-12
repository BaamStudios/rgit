// SPDX-License-Identifier: MIT
//! Ref name lookup ("dwim") and the reverse: the shortest unambiguous name
//! of a full ref (`--abbrev-ref`).

use rgit_hash::ObjectId;

use super::{Error, Resolver};

/// The ref lookup rules of `gitrevisions(7)`, in order; `%s` is the name.
pub const REF_RULES: [&str; 6] = [
    "%s",
    "refs/%s",
    "refs/tags/%s",
    "refs/heads/%s",
    "refs/remotes/%s",
    "refs/remotes/%s/HEAD",
];

/// The result of looking up a short ref name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DwimRef {
    /// How many rules matched an existing ref.
    pub count: usize,
    /// The full name of the first match (after following symbolic refs).
    pub full_name: String,
    /// Its value.
    pub oid: Option<ObjectId>,
}

fn apply_rule(rule: &str, name: &str) -> String {
    rule.replace("%s", name)
}

/// The short name `full` has under `rule`, if it matches.
fn match_rule<'a>(rule: &str, full: &'a str) -> Option<&'a str> {
    let (pre, post) = rule.split_once("%s")?;
    let short = full.strip_prefix(pre)?.strip_suffix(post)?;
    if short.is_empty() { None } else { Some(short) }
}

/// Names that may live directly in `$GIT_DIR` (`HEAD`, `FETCH_HEAD`, ...).
fn is_root_ref_syntax(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b == b'_' || b == b'-')
}

/// A cheap version of `check-ref-format` that rejects names no ref can have,
/// so that revision syntax never reaches the ref store.
pub(crate) fn plausible_refname(name: &str) -> bool {
    if name.is_empty()
        || name.starts_with('/')
        || name.ends_with('/')
        || name.ends_with('.')
        || name.ends_with(".lock")
        || name.contains("..")
        || name.contains("@{")
        || name.contains("//")
        || name == "@"
    {
        return false;
    }
    if name.split('/').any(|c| c.starts_with('.')) {
        return false;
    }
    !name
        .bytes()
        .any(|b| b < 0x20 || b == 0x7f || b" ~^:?*[\\".contains(&b))
}

impl Resolver<'_> {
    /// Look `name` up with the ref rules (git's `expand_ref`). Stops at the
    /// first match unless `core.warnAmbiguousRefs` is set.
    pub(crate) fn expand_ref(&self, name: &str) -> Result<DwimRef, Error> {
        let mut out = DwimRef {
            count: 0,
            full_name: String::new(),
            oid: None,
        };
        for (i, rule) in REF_RULES.iter().enumerate() {
            if i == 0 && !(name.starts_with("refs/") || is_root_ref_syntax(name)) {
                continue;
            }
            let full = apply_rule(rule, name);
            if !plausible_refname(&full) {
                continue;
            }
            let Some(v) = self.repo.refs.resolve(&full)? else {
                continue;
            };
            let Some(oid) = v.oid else { continue };
            out.count += 1;
            if out.count == 1 {
                out.full_name = v.name;
                out.oid = Some(oid);
            }
            if !self.opts.warn_ambiguous_refs {
                break;
            }
        }
        Ok(out)
    }

    /// Look up a ref the way `--symbolic-full-name` does (git's
    /// `dwim_ref`): `@`, `@{-<n>}`, `<branch>@{upstream}` and
    /// `<branch>@{push}` are expanded first.
    pub fn dwim_ref(&self, name: &str) -> Result<DwimRef, Error> {
        if let Some(full) = self.interpret_branch_name(name)? {
            return match full {
                Interpreted::Full(full) => {
                    let v = self.repo.refs.resolve(&full)?;
                    match v.and_then(|v| v.oid.map(|o| (v.name, o))) {
                        Some((n, o)) => Ok(DwimRef {
                            count: 1,
                            full_name: n,
                            oid: Some(o),
                        }),
                        None => Ok(DwimRef {
                            count: 0,
                            full_name: String::new(),
                            oid: None,
                        }),
                    }
                }
                Interpreted::Name(short) => self.expand_ref(&short),
            };
        }
        self.expand_ref(name)
    }

    /// The shortest name that still resolves to `full` (git's
    /// `shorten_unambiguous_ref`). In strict mode the short name must not
    /// match any other rule; in loose mode only the earlier rules count.
    pub fn shorten_ref(&self, full: &str, strict: bool) -> Result<String, Error> {
        for i in (1..REF_RULES.len()).rev() {
            let Some(short) = match_rule(REF_RULES[i], full) else {
                continue;
            };
            let limit = if strict { REF_RULES.len() } else { i };
            let mut ambiguous = false;
            for (j, rule) in REF_RULES.iter().enumerate().take(limit) {
                if j == i {
                    continue;
                }
                let candidate = apply_rule(rule, short);
                if !plausible_refname(&candidate) {
                    continue;
                }
                if self.repo.refs.resolve(&candidate)?.is_some() {
                    ambiguous = true;
                    break;
                }
            }
            if !ambiguous {
                return Ok(short.to_string());
            }
        }
        Ok(full.to_string())
    }

    /// Expand `@`, `@{-<n>}`, `<branch>@{upstream}` and `<branch>@{push}`
    /// (git's `interpret_branch_name`). `Ok(None)` for other names.
    pub(crate) fn interpret_branch_name(&self, name: &str) -> Result<Option<Interpreted>, Error> {
        if name == "@" {
            return Ok(Some(Interpreted::Name("HEAD".to_string())));
        }
        if let Some(n) = parse_previous(name) {
            return Ok(self.previous_branch(n)?.map(Interpreted::Name));
        }
        if let Some((branch, kind)) = split_tracking(name) {
            return Ok(Some(Interpreted::Full(self.branch_tracking(branch, kind)?)));
        }
        Ok(None)
    }

    /// The `n`th branch checked out before the current one, from the
    /// `checkout: moving from A to B` entries of the `HEAD` reflog.
    pub(crate) fn previous_branch(&self, n: usize) -> Result<Option<String>, Error> {
        if n == 0 {
            return Ok(None);
        }
        let Some(log) = self.repo.refs.reflog("HEAD")? else {
            return Ok(None);
        };
        let mut seen = 0;
        for e in log.iter().rev() {
            let Some(rest) = e.message.strip_prefix("checkout: moving from ") else {
                continue;
            };
            let Some((from, _)) = rest.split_once(" to ") else {
                continue;
            };
            seen += 1;
            if seen == n {
                return Ok(Some(from.to_string()));
            }
        }
        Ok(None)
    }
}

/// The result of [`Resolver::interpret_branch_name`].
pub(crate) enum Interpreted {
    /// A name to look up further (`HEAD`, a branch name, a hex id).
    Name(String),
    /// A full ref name (`refs/remotes/origin/main`).
    Full(String),
}

/// `@{-<n>}` (the whole name).
pub(crate) fn parse_previous(name: &str) -> Option<usize> {
    let digits = name.strip_prefix("@{-")?.strip_suffix('}')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Which tracking branch `@{...}` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tracking {
    Upstream,
    Push,
}

/// `u}`, `upstream}` or `push}` (any case) at the start of `s` (the text
/// after `@{`): the kind and the keyword length.
pub(crate) fn tracking_keyword(s: &str) -> Option<(Tracking, usize)> {
    let end = s.find('}')?;
    let kw = &s[..end];
    if kw.eq_ignore_ascii_case("u") || kw.eq_ignore_ascii_case("upstream") {
        Some((Tracking::Upstream, end))
    } else if kw.eq_ignore_ascii_case("push") {
        Some((Tracking::Push, end))
    } else {
        None
    }
}

/// `<branch>@{upstream}`, `<branch>@{u}`, `<branch>@{push}` (any case),
/// split into branch part and kind.
pub(crate) fn split_tracking(name: &str) -> Option<(&str, Tracking)> {
    let inner = name.strip_suffix('}')?;
    let at = inner.rfind("@{")?;
    let kw = &inner[at + 2..];
    let kind = if kw.eq_ignore_ascii_case("u") || kw.eq_ignore_ascii_case("upstream") {
        Tracking::Upstream
    } else if kw.eq_ignore_ascii_case("push") {
        Tracking::Push
    } else {
        return None;
    };
    Some((&name[..at], kind))
}
