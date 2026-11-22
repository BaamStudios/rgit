// SPDX-License-Identifier: MIT

//! Function-name lines for hunk headers (`diff.<driver>.xfuncname`,
//! `.funcname`, the built-in drivers of gitattributes(5)).
//!
//! Behaviour taken from the `git` binary: each line is matched without
//! its line terminator (`\n` or `\r\n`); the patterns are tried in order
//! and the first one that matches decides (a pattern starting with `!`
//! rejects the line). The text shown is the first capture group when it
//! took part in the match, else the whole match; an empty text shows no
//! function name but still ends the search.

use std::sync::Arc;

use regex::bytes::Regex;

/// One pattern: `negate` for a `!` line.
#[derive(Debug, Clone)]
pub struct FuncPattern {
    pub negate: bool,
    pub regex: Regex,
}

/// Compiled function-name patterns of a diff driver.
#[derive(Debug, Clone)]
pub struct FuncName(Arc<Vec<FuncPattern>>);

impl PartialEq for FuncName {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
            || (self.0.len() == other.0.len()
                && self
                    .0
                    .iter()
                    .zip(other.0.iter())
                    .all(|(a, b)| a.negate == b.negate && a.regex.as_str() == b.regex.as_str()))
    }
}

impl Eq for FuncName {}

impl FuncName {
    /// Patterns tried in order.
    pub fn new(patterns: Vec<FuncPattern>) -> FuncName {
        FuncName(Arc::new(patterns))
    }

    /// The patterns.
    pub fn patterns(&self) -> &[FuncPattern] {
        &self.0
    }

    /// The function-name text of `line` (with or without its line
    /// terminator), or `None` when the line is not a function line.
    pub fn find<'l>(&self, line: &'l [u8]) -> Option<&'l [u8]> {
        let body = line.strip_suffix(b"\n").unwrap_or(line);
        let body = body.strip_suffix(b"\r").unwrap_or(body);
        for p in self.0.iter() {
            let Some(caps) = p.regex.captures(body) else {
                continue;
            };
            if p.negate {
                return None;
            }
            let m = caps.get(1).or_else(|| caps.get(0)).expect("match");
            return Some(m.as_bytes());
        }
        None
    }

    /// Whether `line` is a function line.
    pub fn is_func(&self, line: &[u8]) -> bool {
        self.find(line).is_some()
    }
}

/// Function lines with an optional driver: the default rule (a line
/// starting with a letter, `_` or `$`) without one.
pub fn find_func<'l>(funcname: Option<&FuncName>, line: &'l [u8]) -> Option<&'l [u8]> {
    match funcname {
        Some(f) => f.find(line),
        None => crate::unified::default_funcname(line).then_some(line),
    }
}
