// SPDX-License-Identifier: MIT
//! `--diff-filter=[(A|C|D|M|R|T|U|X|B)...[*]]` (diff-options(1)).

use super::{FilePair, Status};

/// A parsed `--diff-filter` value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffFilter {
    /// Upper-case letters: only these (none given: all).
    include: Vec<u8>,
    /// Lower-case letters, upper-cased: not these.
    exclude: Vec<u8>,
    /// `*`: all pairs when any matches, else none.
    all_or_none: bool,
}

const LETTERS: &[u8] = b"ACDMRTUXB";

impl DiffFilter {
    /// Parse a value; `Err(c)` names an unknown letter. Repeated options
    /// add to the earlier ones.
    pub fn parse_into(&mut self, value: &str) -> Result<(), char> {
        for c in value.chars() {
            if c == '*' {
                self.all_or_none = true;
                continue;
            }
            let Ok(b) = u8::try_from(c) else {
                return Err(c);
            };
            if LETTERS.contains(&b) {
                self.include.push(b);
            } else if LETTERS.contains(&b.to_ascii_uppercase()) {
                self.exclude.push(b.to_ascii_uppercase());
            } else {
                return Err(c);
            }
        }
        Ok(())
    }

    /// Whether anything is filtered.
    pub fn is_active(&self) -> bool {
        !self.include.is_empty() || !self.exclude.is_empty() || self.all_or_none
    }

    fn selects(&self, letter: u8, broken: bool) -> bool {
        let has = |set: &[u8]| set.contains(&letter) || (broken && set.contains(&b'B'));
        if has(&self.exclude) {
            return false;
        }
        self.include.is_empty() || has(&self.include)
    }

    /// Whether a pair with this status letter (and broken flag) passes.
    pub fn select_pair<T>(&self, p: &FilePair<T>) -> bool {
        let letter = match p.status {
            Status::Unmodified => b'X',
            s => s.letter(),
        };
        self.selects(letter, p.is_rewrite())
    }

    /// Apply to a list of pairs.
    pub fn apply<T>(&self, pairs: Vec<FilePair<T>>) -> Vec<FilePair<T>> {
        self.apply_by(pairs, |p| (p.status.letter(), p.is_rewrite()))
    }

    /// Apply to any list, given each item's status letter and broken flag.
    pub fn apply_by<P>(&self, items: Vec<P>, key: impl Fn(&P) -> (u8, bool)) -> Vec<P> {
        if !self.is_active() {
            return items;
        }
        if self.all_or_none {
            let any = items.iter().any(|p| {
                let (l, b) = key(p);
                self.selects_star(l, b)
            });
            return if any { items } else { Vec::new() };
        }
        items
            .into_iter()
            .filter(|p| {
                let (l, b) = key(p);
                self.selects(l, b)
            })
            .collect()
    }

    /// With `*`, only the upper-case letters count as matches (observed:
    /// `*` alone or with lower-case letters only selects nothing).
    fn selects_star(&self, letter: u8, broken: bool) -> bool {
        !self.include.is_empty() && self.selects(letter, broken)
    }
}
