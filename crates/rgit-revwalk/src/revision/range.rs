// SPDX-License-Identifier: MIT
//! Range notations: `A..B`, `A...B`, `<rev>^@`, `<rev>^!`, `<rev>^-<n>`.

use rgit_hash::ObjectId;

use super::{Error, Flags, Hint, Resolver};

/// One object a revision argument stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevItem {
    /// The object (not peeled: `v1..v2` keeps the tag ids).
    pub oid: ObjectId,
    /// Excluded (`^<rev>`).
    pub negated: bool,
    /// The name as the user could have written it (`HEAD^1` for the first
    /// parent in `HEAD^!`); `None` for computed objects such as merge bases.
    pub name: Option<String>,
}

impl RevItem {
    fn new(oid: ObjectId, negated: bool, name: Option<String>) -> RevItem {
        RevItem { oid, negated, name }
    }
}

impl Resolver<'_> {
    /// `A..B` and `A...B` (an empty side means `HEAD`).
    ///
    /// `Ok(None)` when `arg` is not a range or one side does not resolve;
    /// the caller then treats `arg` as a single revision.
    pub fn try_range(&self, arg: &str, flags: Flags) -> Result<Option<Vec<RevItem>>, Error> {
        if arg == ".." {
            return Ok(None);
        }
        let Some(pos) = arg.find("..") else {
            return Ok(None);
        };
        let symmetric = arg[pos + 2..].starts_with('.');
        let a = &arg[..pos];
        let b = &arg[pos + if symmetric { 3 } else { 2 }..];
        let a = if a.is_empty() { "HEAD" } else { a };
        let b = if b.is_empty() { "HEAD" } else { b };
        let f = Flags {
            hint: Hint::Committish,
            ..flags
        };
        let a_oid = match self.get_oid(a, f) {
            Ok(o) => o,
            Err(Error::NotFound) => return Ok(None),
            Err(e) => return Err(e),
        };
        let b_oid = match self.get_oid(b, f) {
            Ok(o) => o,
            Err(Error::NotFound) => return Ok(None),
            Err(e) => return Err(e),
        };
        if !symmetric {
            return Ok(Some(vec![
                RevItem::new(b_oid, false, Some(b.to_string())),
                RevItem::new(a_oid, true, Some(a.to_string())),
            ]));
        }
        let (Ok(ca), Ok(cb)) = (self.peel_to_commit(&a_oid), self.peel_to_commit(&b_oid)) else {
            return Ok(None);
        };
        let mut items = vec![
            RevItem::new(b_oid, false, Some(b.to_string())),
            RevItem::new(a_oid, false, Some(a.to_string())),
        ];
        for base in self.merge_bases(&ca, &cb)? {
            items.push(RevItem::new(base, true, None));
        }
        Ok(Some(items))
    }

    /// `<rev>^@` (all parents), `<rev>^!` (the commit without its parents)
    /// and `<rev>^-<n>` (`<rev>^<n>..<rev>`).
    ///
    /// `Ok(None)` when `arg` has none of these suffixes or does not resolve.
    pub fn try_parent_shorthand(
        &self,
        arg: &str,
        flags: Flags,
    ) -> Result<Option<Vec<RevItem>>, Error> {
        enum Kind {
            Parents,
            Only,
            Minus(usize),
        }
        let (base, kind) = if let Some(b) = arg.strip_suffix("^@") {
            (b, Kind::Parents)
        } else if let Some(b) = arg.strip_suffix("^!") {
            (b, Kind::Only)
        } else if let Some(pos) = arg.rfind("^-") {
            let digits = &arg[pos + 2..];
            let n = if digits.is_empty() {
                1
            } else if digits.bytes().all(|b| b.is_ascii_digit()) {
                match digits.parse::<usize>() {
                    Ok(n) if n > 0 => n,
                    _ => return Ok(None),
                }
            } else {
                return Ok(None);
            };
            (&arg[..pos], Kind::Minus(n))
        } else {
            return Ok(None);
        };
        let f = Flags {
            hint: Hint::Committish,
            ..flags
        };
        let oid = match self.get_oid(base, f) {
            Ok(o) => o,
            Err(Error::NotFound) => return Ok(None),
            Err(e) => return Err(e),
        };
        let commit = match self.peel_to_commit(&oid) {
            Ok(c) => c,
            Err(Error::NotFound) => return Ok(None),
            Err(e) => return Err(e),
        };
        let parents = self.parents_of(&commit)?;
        let parent_name = |i: usize| Some(format!("{base}^{i}"));
        Ok(Some(match kind {
            Kind::Parents => parents
                .iter()
                .enumerate()
                .map(|(i, p)| RevItem::new(*p, false, parent_name(i + 1)))
                .collect(),
            Kind::Only => {
                let mut v = vec![RevItem::new(oid, false, Some(base.to_string()))];
                v.extend(
                    parents
                        .iter()
                        .enumerate()
                        .map(|(i, p)| RevItem::new(*p, true, parent_name(i + 1))),
                );
                v
            }
            Kind::Minus(n) => {
                let Some(p) = parents.get(n - 1) else {
                    return Ok(None);
                };
                vec![
                    RevItem::new(oid, false, Some(base.to_string())),
                    RevItem::new(*p, true, parent_name(n)),
                ]
            }
        }))
    }
}
