// SPDX-License-Identifier: MIT
//! The references a client may see: those of the namespace in use
//! (`GIT_NAMESPACE`, gitnamespaces(7)), without the ones hidden by
//! `transfer.hideRefs` and `uploadpack.hideRefs`
//! (`Documentation/config/transfer.adoc`).
//!
//! A namespace `foo/bar` keeps its references below
//! `refs/namespaces/foo/refs/namespaces/bar/`, its `HEAD` included; the
//! client sees them without that prefix. A `hideRefs` entry names a
//! reference or a hierarchy; `!` in front exposes it again, `^` matches
//! the name before the namespace is stripped, and the last entry that
//! matches wins.

use rgit_config::ConfigSet;
use rgit_hash::ObjectId;
use rgit_refs::{IterItem, IterOptions, RefStore, Reference, Target};

struct HideRule {
    /// The reference or hierarchy, without a trailing `/`.
    prefix: String,
    /// `!`: expose instead of hide.
    negated: bool,
    /// `^`: match the full name, not the one the client sees.
    full: bool,
}

impl HideRule {
    fn parse(value: &str) -> HideRule {
        let (negated, rest) = match value.strip_prefix('!') {
            Some(r) => (true, r),
            None => (false, value),
        };
        let (full, rest) = match rest.strip_prefix('^') {
            Some(r) => (true, r),
            None => (false, rest),
        };
        HideRule {
            prefix: rest.trim_end_matches('/').to_owned(),
            negated,
            full,
        }
    }

    fn matches(&self, name: &str) -> bool {
        match name.strip_prefix(self.prefix.as_str()) {
            Some(rest) => rest.is_empty() || rest.starts_with('/'),
            None => false,
        }
    }
}

/// A reference as the client sees it.
pub(crate) struct Visible {
    /// The name without the namespace.
    pub name: String,
    /// What it points at.
    pub id: ObjectId,
    /// For a symbolic reference: the reference at the end of the chain,
    /// without the namespace.
    pub symref_target: Option<String>,
    /// The stored reference.
    pub reference: Option<Reference>,
}

/// `HEAD` when it is a symbolic reference to a branch that does not exist
/// yet.
pub(crate) struct Unborn {
    /// The branch, without the namespace.
    pub target: String,
}

/// The references of one namespace, minus the hidden ones.
pub(crate) struct RefView<'a> {
    store: &'a dyn RefStore,
    /// `refs/namespaces/<ns>/` for every part of the namespace, or empty.
    prefix: String,
    rules: Vec<HideRule>,
}

impl<'a> RefView<'a> {
    pub fn new(store: &'a dyn RefStore, config: &ConfigSet, namespace: Option<&str>) -> Self {
        let mut prefix = String::new();
        for part in namespace.unwrap_or("").split('/').filter(|p| !p.is_empty()) {
            prefix.push_str("refs/namespaces/");
            prefix.push_str(part);
            prefix.push('/');
        }
        // Both variables in the order of the configuration.
        let rules = config
            .entries()
            .iter()
            .filter(|e| e.name == "transfer.hiderefs" || e.name == "uploadpack.hiderefs")
            .filter_map(|e| e.value.as_ref())
            .map(|v| HideRule::parse(&v.to_string()))
            .collect();
        RefView {
            store,
            prefix,
            rules,
        }
    }

    /// Whether the reference the client calls `name` is hidden.
    fn hidden(&self, name: &str) -> bool {
        let full = format!("{}{name}", self.prefix);
        self.rules
            .iter()
            .rev()
            .find(|r| r.matches(if r.full { &full } else { name }))
            .is_some_and(|r| !r.negated)
    }

    fn strip<'n>(&self, full: &'n str) -> Option<&'n str> {
        full.strip_prefix(self.prefix.as_str())
    }

    /// What the reference the client calls `name` points at; `None` when
    /// it does not exist or is hidden.
    pub fn resolve(&self, name: &str) -> Option<ObjectId> {
        if self.hidden(name) {
            return None;
        }
        self.store
            .resolve_oid(&format!("{}{name}", self.prefix))
            .ok()
            .flatten()
    }

    /// `HEAD`: the reference, or the branch it will be once that exists.
    pub fn head(&self) -> (Option<Visible>, Option<Unborn>) {
        if self.hidden("HEAD") {
            return (None, None);
        }
        let full = format!("{}HEAD", self.prefix);
        let direct = self.store.read_raw(&full).ok().flatten();
        let resolved = self.store.resolve(&full).ok().flatten();
        let target = match (&direct, &resolved) {
            (Some(Target::Symbolic(_)), Some(r)) => self.strip(r.final_name()).map(str::to_owned),
            _ => None,
        };
        match resolved.and_then(|r| r.oid) {
            Some(id) => (
                Some(Visible {
                    name: "HEAD".to_owned(),
                    id,
                    symref_target: target,
                    reference: None,
                }),
                None,
            ),
            None => (None, target.map(|target| Unborn { target })),
        }
    }

    /// The references below `refs/`, in name order. A symbolic reference
    /// to nothing is left out.
    ///
    /// In a namespace these are all references below the namespace, so its
    /// `HEAD` is among them: git lists it a second time there.
    pub fn list(&self) -> Result<Vec<Visible>, rgit_refs::Error> {
        let all = IterOptions {
            prefix: if self.prefix.is_empty() {
                "refs/".to_owned()
            } else {
                self.prefix.clone()
            },
            include_root_refs: false,
        };
        let mut out = Vec::new();
        for item in self.store.iter(&all)? {
            let IterItem::Ref(r) = item else { continue };
            let Some(name) = self.strip(&r.name) else { continue };
            let Some(id) = r.oid else { continue };
            if self.hidden(name) {
                continue;
            }
            let symref_target = match &r.target {
                Target::Symbolic(_) => self
                    .store
                    .resolve(&r.name)
                    .ok()
                    .flatten()
                    .and_then(|res| self.strip(res.final_name()).map(str::to_owned)),
                Target::Object(_) => None,
            };
            out.push(Visible {
                name: name.to_owned(),
                id,
                symref_target,
                reference: Some(r),
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::HideRule;

    #[test]
    fn hide_rules() {
        let rule = HideRule::parse("refs/heads/hidden");
        assert!(!rule.negated && !rule.full);
        assert!(rule.matches("refs/heads/hidden"));
        assert!(rule.matches("refs/heads/hidden/below"));
        assert!(!rule.matches("refs/heads/hidden-not"));
        assert!(!rule.matches("refs/heads"));
        let rule = HideRule::parse("!^refs/namespaces/a/");
        assert!(rule.negated && rule.full);
        assert!(rule.matches("refs/namespaces/a/refs/heads/x"));
        assert!(!rule.matches("refs/namespaces/ab"));
    }
}
