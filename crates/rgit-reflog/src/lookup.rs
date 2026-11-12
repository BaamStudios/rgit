// SPDX-License-Identifier: MIT
//! Resolving `<ref>@{<n>}`, `<ref>@{<date>}` and `@{-<n>}` against a reflog
//! (`gitrevisions(7)`). The revision parser (#100) splits the name, resolves
//! the reference and prints the warnings; this module answers which object
//! the selector names.

use std::fmt;

use rgit_date::{DateContext, DateMode, DateStyle, approxidate_careful, parse_date, show_date};
use rgit_hash::ObjectId;

use crate::entry::Entry;
use crate::messages;

/// What is inside `@{...}` after a reference name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selector {
    /// `@{<n>}`: the n-th prior value (`0` is the newest entry).
    Nth(usize),
    /// `@{<date>}`: the value at that time (seconds since the epoch).
    Date(u64),
}

impl Selector {
    /// Parses the text between the braces. A plain number is an entry
    /// count unless it reads as a timestamp; anything else is a date in
    /// any form `approxidate` understands. `None` if it is neither (the
    /// name is then not a valid revision).
    ///
    /// `@{-<n>}` and `@{upstream}`-style forms are the caller's business.
    pub fn parse(spec: &str, ctx: &DateContext) -> Option<Selector> {
        if spec.is_empty() {
            return None;
        }
        if spec.bytes().all(|b| b.is_ascii_digit()) {
            if let Some((t, _)) = parse_date(spec, ctx) {
                return Some(Selector::Date(t));
            }
            return spec.parse::<usize>().ok().map(Selector::Nth);
        }
        let (t, error) = approxidate_careful(spec, ctx);
        (!error).then_some(Selector::Date(t))
    }
}

/// A warning printed while resolving (with `warning: `), the lookup still
/// succeeds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Warning {
    /// The entry found does not continue into the next newer one.
    GapAfter { refname: String, time: u64, tz: i32 },
    /// The newest entry does not match the reference's current value; the
    /// current value is used.
    UnexpectedlyEnded { refname: String, time: u64, tz: i32 },
    /// The date is older than the oldest entry.
    OnlyGoesBack { display: String, time: u64, tz: i32 },
}

fn rfc2822(time: u64, tz: i32) -> String {
    show_date(time, tz, &DateMode::new(DateStyle::Rfc2822), &DateContext::utc(0))
        .unwrap_or_else(|_| rgit_date::format_raw(time, tz))
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Warning::GapAfter { refname, time, tz } => {
                f.write_str(&messages::gap_after(refname, &rfc2822(*time, *tz)))
            }
            Warning::UnexpectedlyEnded { refname, time, tz } => {
                f.write_str(&messages::unexpectedly_ended(refname, &rfc2822(*time, *tz)))
            }
            Warning::OnlyGoesBack { display, time, tz } => {
                f.write_str(&messages::only_goes_back(display, &rfc2822(*time, *tz)))
            }
        }
    }
}

/// A failed lookup (git dies with `fatal: `).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LookupError {
    /// The log has no entries (and the selector is not `@{0}`).
    #[error("{}", messages::log_is_empty(.refname))]
    Empty { refname: String },
    /// `@{n}` beyond the oldest entry.
    #[error("{}", messages::only_has_entries(.display, *.count))]
    TooShort { display: String, count: usize },
}

/// The answer of [`resolve`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    /// The object named.
    pub oid: ObjectId,
    /// Index (oldest first) of the entry that answered, `None` when the
    /// answer is the old value of the oldest entry or the current value of
    /// an empty log.
    pub index: Option<usize>,
    /// Warnings to print before using the result.
    pub warnings: Vec<Warning>,
}

/// The reference a selector applies to.
#[derive(Clone, Copy, Debug)]
pub struct Target<'a> {
    /// Full name of the reference (`refs/heads/main`), used in the
    /// `gap` / `unexpectedly ended` / `empty` messages.
    pub refname: &'a str,
    /// The name as the user wrote it before `@{` (`main`), used in the
    /// `only goes back` / `only has` messages.
    pub display: &'a str,
    /// The reference's current value, if it has one.
    pub current: Option<ObjectId>,
}

/// Looks up `selector` in `entries` (oldest first, as read from the file).
pub fn resolve(entries: &[Entry], selector: Selector, target: Target<'_>) -> Result<Resolved, LookupError> {
    let len = entries.len();
    if len == 0 {
        if let (Selector::Nth(0), Some(cur)) = (selector, target.current) {
            return Ok(Resolved {
                oid: cur,
                index: None,
                warnings: Vec::new(),
            });
        }
        return Err(LookupError::Empty {
            refname: target.refname.to_string(),
        });
    }
    let newest = &entries[len - 1];
    // `@{0}` and dates after the newest entry ask for the value after the
    // end of the log: the current value, which should match.
    let past_end = match selector {
        Selector::Nth(n) => n == 0,
        Selector::Date(t) => t > newest.time,
    };
    if past_end {
        let mut warnings = Vec::new();
        let mut oid = newest.new;
        if let Some(cur) = target.current.filter(|c| *c != newest.new) {
            warnings.push(Warning::UnexpectedlyEnded {
                refname: target.refname.to_string(),
                time: newest.time,
                tz: newest.tz,
            });
            oid = cur;
        }
        return Ok(Resolved {
            oid,
            index: Some(len - 1),
            warnings,
        });
    }
    let index = match selector {
        Selector::Nth(n) if n < len => len - 1 - n,
        Selector::Nth(n) => {
            let oldest = &entries[0];
            if n == len && !oldest.old.is_null() {
                return Ok(Resolved {
                    oid: oldest.old,
                    index: None,
                    warnings: Vec::new(),
                });
            }
            return Err(LookupError::TooShort {
                display: target.display.to_string(),
                count: len,
            });
        }
        Selector::Date(t) => match entries.iter().rposition(|e| e.time <= t) {
            Some(i) => i,
            None => {
                let oldest = &entries[0];
                let oid = if oldest.old.is_null() {
                    oldest.new
                } else {
                    oldest.old
                };
                return Ok(Resolved {
                    oid,
                    index: None,
                    warnings: vec![Warning::OnlyGoesBack {
                        display: target.display.to_string(),
                        time: oldest.time,
                        tz: oldest.tz,
                    }],
                });
            }
        },
    };
    // The entry's new value should be the next entry's old value.
    let e = &entries[index];
    let mut warnings = Vec::new();
    if entries.get(index + 1).is_some_and(|next| next.old != e.new) {
        warnings.push(Warning::GapAfter {
            refname: target.refname.to_string(),
            time: e.time,
            tz: e.tz,
        });
    }
    Ok(Resolved {
        oid: e.new,
        index: Some(index),
        warnings,
    })
}

/// `@{-<n>}`: the branch (or detached object name) the n-th last
/// `checkout: moving from <x> to <y>` entry of the `HEAD` log moved away
/// from. `n` starts at 1; `None` if there are not that many checkouts.
pub fn nth_prior_checkout(head_log: &[Entry], n: usize) -> Option<&[u8]> {
    if n == 0 {
        return None;
    }
    head_log
        .iter()
        .rev()
        .filter_map(Entry::checkout_from)
        .nth(n - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgit_hash::HashKind;

    fn id(c: u8) -> ObjectId {
        if c == 0 {
            return HashKind::Sha1.null();
        }
        ObjectId::from_bytes(HashKind::Sha1, &[c; 20]).unwrap()
    }

    fn e(old: u8, new: u8, time: u64) -> Entry {
        Entry {
            old: id(old),
            new: id(new),
            ident: b"N <e>".to_vec(),
            time,
            tz: 0,
            message: b"m".to_vec(),
        }
    }

    fn t(current: u8) -> Target<'static> {
        Target {
            refname: "refs/heads/x",
            display: "x",
            current: (current != 0).then(|| id(current)),
        }
    }

    #[test]
    fn selector_parsing() {
        let ctx = DateContext::utc(1_800_000_000);
        let p = |s| Selector::parse(s, &ctx);
        assert_eq!(p("0"), Some(Selector::Nth(0)));
        assert_eq!(p("01"), Some(Selector::Nth(1)));
        assert_eq!(p("99999999"), Some(Selector::Nth(99999999)));
        assert_eq!(p("12345678901"), Some(Selector::Nth(12345678901)));
        assert_eq!(p("100000000"), Some(Selector::Date(100000000)));
        assert_eq!(p("1000000000"), Some(Selector::Date(1000000000)));
        assert_eq!(p("now"), Some(Selector::Date(1_800_000_000)));
        assert_eq!(p("1112913993 -0700"), Some(Selector::Date(1112913993)));
        assert_eq!(p("garbage"), None);
        assert_eq!(p(""), None);
        assert!(matches!(p(" 1"), Some(Selector::Date(_))));
    }

    #[test]
    fn nth_lookup() {
        // A->B, C->D, E->F; ref H: every step has a gap.
        let log = [e(1, 2, 1000), e(3, 4, 2000), e(5, 6, 3000)];
        let r = resolve(&log, Selector::Nth(0), t(8)).unwrap();
        assert_eq!(r.oid, id(8));
        assert!(matches!(r.warnings[..], [Warning::UnexpectedlyEnded { time: 3000, .. }]));
        let r = resolve(&log, Selector::Nth(1), t(8)).unwrap();
        assert_eq!(r.oid, id(4));
        assert!(matches!(r.warnings[..], [Warning::GapAfter { time: 2000, .. }]));
        let r = resolve(&log, Selector::Nth(3), t(8)).unwrap();
        assert_eq!((r.oid, r.warnings.len()), (id(1), 0));
        assert_eq!(
            resolve(&log, Selector::Nth(4), t(8)).unwrap_err().to_string(),
            "log for 'x' only has 3 entries"
        );
        let r = resolve(&log, Selector::Nth(0), t(6)).unwrap();
        assert_eq!((r.oid, r.warnings.len()), (id(6), 0));
    }

    #[test]
    fn nth_past_creation() {
        let log = [e(0, 1, 1000), e(1, 2, 2000)];
        assert_eq!(resolve(&log, Selector::Nth(1), t(2)).unwrap().oid, id(1));
        assert!(resolve(&log, Selector::Nth(2), t(2)).is_err());
    }

    #[test]
    fn date_lookup() {
        let log = [e(0, 1, 1000), e(1, 2, 2000), e(2, 3, 3000)];
        let d = |s| resolve(&log, Selector::Date(s), t(3)).unwrap();
        // exactly at the newest entry: no comparison with the current value
        assert_eq!(resolve(&log, Selector::Date(3000), t(7)).unwrap().oid, id(3));
        assert_eq!(resolve(&log, Selector::Date(3001), t(7)).unwrap().oid, id(7));
        assert_eq!(d(2000).oid, id(2));
        assert_eq!(d(2999).oid, id(2));
        assert_eq!(d(9999).oid, id(3));
        let r = d(10);
        assert_eq!(r.oid, id(1));
        assert_eq!(
            r.warnings[0].to_string(),
            "log for 'x' only goes back to Thu, 1 Jan 1970 00:16:40 +0000"
        );
        let log = [e(5, 1, 1000)];
        assert_eq!(resolve(&log, Selector::Date(10), t(1)).unwrap().oid, id(5));
    }

    #[test]
    fn empty_log() {
        assert_eq!(resolve(&[], Selector::Nth(0), t(4)).unwrap().oid, id(4));
        assert_eq!(
            resolve(&[], Selector::Nth(1), t(4)).unwrap_err().to_string(),
            "log for refs/heads/x is empty"
        );
        assert!(resolve(&[], Selector::Date(5), t(4)).is_err());
    }

    #[test]
    fn warning_texts() {
        let w = Warning::GapAfter {
            refname: "refs/heads/x".into(),
            time: 1112913000,
            tz: 0,
        };
        assert_eq!(
            w.to_string(),
            "log for ref refs/heads/x has gap after Thu, 7 Apr 2005 22:30:00 +0000"
        );
        let w = Warning::UnexpectedlyEnded {
            refname: "refs/heads/x".into(),
            time: 1112912000,
            tz: 100,
        };
        assert_eq!(
            w.to_string(),
            "log for ref refs/heads/x unexpectedly ended on Thu, 7 Apr 2005 23:13:20 +0100"
        );
    }

    #[test]
    fn prior_checkouts() {
        let mut log = Vec::new();
        for m in [
            "commit (initial): one",
            "checkout: moving from main to f1",
            "commit: two",
            "checkout: moving from f1 to main",
        ] {
            let mut x = e(1, 1, 5);
            x.message = m.as_bytes().to_vec();
            log.push(x);
        }
        assert_eq!(nth_prior_checkout(&log, 1), Some(&b"f1"[..]));
        assert_eq!(nth_prior_checkout(&log, 2), Some(&b"main"[..]));
        assert_eq!(nth_prior_checkout(&log, 3), None);
        assert_eq!(nth_prior_checkout(&log, 0), None);
    }
}
