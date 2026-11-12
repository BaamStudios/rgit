// SPDX-License-Identifier: MIT
//! Abbreviated object ids: resolving them (with type hints and git's
//! candidate list on ambiguity) and producing unique abbreviations.

use rgit_date::{DateMode, DateStyle, show_date};
use rgit_hash::{ObjectId, Prefix};
use rgit_object::{Commit, ObjectKind, Tag};

use super::{Error, Flags, Hint, Level, Resolver, messages};

/// The shortest abbreviation git accepts.
pub const MINIMUM_ABBREV: usize = 4;

/// The default abbreviation for small repositories.
const DEFAULT_ABBREV: usize = 7;

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Number of leading hex digits `a` and `b` share.
fn common_hex_len(a: &ObjectId, b: &ObjectId) -> usize {
    let mut n = 0;
    for (x, y) in a.as_bytes().iter().zip(b.as_bytes()) {
        if x == y {
            n += 2;
            continue;
        }
        if x >> 4 == y >> 4 {
            n += 1;
        }
        break;
    }
    n
}

/// Sort rank for the candidate list: tags, commits, trees, blobs, bad.
fn kind_rank(k: Option<ObjectKind>) -> u8 {
    match k {
        Some(ObjectKind::Tag) => 0,
        Some(ObjectKind::Commit) => 1,
        Some(ObjectKind::Tree) => 2,
        Some(ObjectKind::Blob) => 3,
        None => 4,
    }
}

/// `hhmm` time zone from minutes.
fn tz_hhmm(minutes: i32) -> i32 {
    let sign = if minutes < 0 { -1 } else { 1 };
    let m = minutes.abs();
    sign * ((m / 60) * 100 + m % 60)
}

/// First paragraph of a message, lines joined with spaces (`%s`).
fn subject(message: &[u8]) -> String {
    let text = String::from_utf8_lossy(message);
    let mut out = String::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if out.is_empty() {
                continue;
            }
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(line.trim_end());
    }
    out
}

impl Resolver<'_> {
    /// The abbreviation length for `core.abbrev=auto` or the configured one.
    pub fn default_abbrev(&self) -> usize {
        let hexsz = self.repo.objects.hash_kind().hex_len();
        match self.opts.abbrev {
            Some(n) => n.clamp(MINIMUM_ABBREV, hexsz),
            None => {
                let count = self.repo.objects.approximate_count();
                let bits = (64 - count.leading_zeros()) as usize;
                bits.div_ceil(2).clamp(DEFAULT_ABBREV, hexsz)
            }
        }
    }

    /// The shortest prefix of `oid` with at least `len` digits that no other
    /// object shares (git's `find_unique_abbrev`).
    pub fn unique_abbrev(&self, oid: &ObjectId, len: usize) -> Result<String, Error> {
        let hexsz = oid.kind().hex_len();
        let len = len.clamp(MINIMUM_ABBREV, hexsz);
        if len == hexsz {
            return Ok(oid.to_hex());
        }
        let prefix = Prefix::new(*oid, len).map_err(|e| Error::Fatal(e.to_string()))?;
        let mut need = len;
        for other in self.repo.objects.prefix_matches(&prefix)? {
            if &other != oid {
                need = need.max(common_hex_len(oid, &other) + 1);
            }
        }
        Ok(oid.to_hex_with_len(need.min(hexsz)))
    }

    /// Whether `oid` satisfies `hint`.
    fn matches_hint(&self, oid: &ObjectId, hint: Hint) -> Result<bool, Error> {
        Ok(match hint {
            Hint::None => true,
            Hint::Commit => self.kind_of(oid)? == Some(ObjectKind::Commit),
            Hint::Tree => self.kind_of(oid)? == Some(ObjectKind::Tree),
            Hint::Blob => self.kind_of(oid)? == Some(ObjectKind::Blob),
            Hint::Committish => matches!(self.peel_tags(oid)?, Some((_, ObjectKind::Commit))),
            Hint::Treeish => matches!(
                self.peel_tags(oid)?,
                Some((_, ObjectKind::Commit | ObjectKind::Tree))
            ),
        })
    }

    /// All objects starting with the hex digits `hex`, sorted, without
    /// duplicates. `None` if `hex` is not an abbreviated id.
    pub fn prefix_candidates(&self, hex: &str) -> Result<Option<Vec<ObjectId>>, Error> {
        let kind = self.repo.objects.hash_kind();
        if hex.len() < MINIMUM_ABBREV || hex.len() > kind.hex_len() || !is_hex(hex) {
            return Ok(None);
        }
        let prefix = Prefix::from_hex(kind, hex.as_bytes()).map_err(|_| Error::NotFound)?;
        let mut c = self.repo.objects.prefix_matches(&prefix)?;
        c.sort();
        c.dedup();
        Ok(Some(c))
    }

    /// Resolve an abbreviated id (git's `get_short_oid`).
    pub(crate) fn short_oid(&self, hex: &str, flags: Flags) -> Result<ObjectId, Error> {
        let Some(all) = self.prefix_candidates(hex)? else {
            return Err(Error::NotFound);
        };
        match all.len() {
            0 => return Err(Error::NotFound),
            1 => return Ok(all[0]),
            _ => {}
        }
        let hint = if flags.hint == Hint::None {
            self.opts.disambiguate
        } else {
            flags.hint
        };
        let mut filtered = Vec::new();
        if hint != Hint::None {
            for c in &all {
                if self.matches_hint(c, hint)? {
                    filtered.push(*c);
                }
            }
            if filtered.len() == 1 {
                return Ok(filtered[0]);
            }
        }
        if !flags.quiet && !flags.diagnose {
            let shown = if filtered.is_empty() { &all } else { &filtered };
            self.report_ambiguous(hex, shown)?;
        }
        Err(Error::NotFound)
    }

    fn report_ambiguous(&self, hex: &str, candidates: &[ObjectId]) -> Result<(), Error> {
        self.push_diag(Level::Error, messages::short_id_ambiguous(hex));
        let mut rows = Vec::new();
        for c in candidates {
            let read = self.repo.objects.read(c).ok().flatten();
            rows.push((kind_rank(read.as_ref().map(|r| r.0)), *c, read));
        }
        rows.sort_by_key(|a| (a.0, a.1));
        let abbrev = self.default_abbrev();
        let mut lines = vec![messages::CANDIDATES.to_string()];
        for (_, oid, read) in rows {
            let short = self.unique_abbrev(&oid, abbrev)?;
            lines.push(format!("  {short} {}", self.describe_candidate(read)));
        }
        self.push_diag(Level::Hint, lines.join("\n"));
        Ok(())
    }

    fn short_date(&self, seconds: i64, offset_minutes: i32) -> String {
        let mode = DateMode::new(DateStyle::Short);
        show_date(
            seconds.max(0) as u64,
            tz_hhmm(offset_minutes),
            &mode,
            &self.opts.date,
        )
        .unwrap_or_default()
    }

    fn describe_candidate(&self, read: Option<(ObjectKind, Vec<u8>)>) -> String {
        let hash = self.repo.objects.hash_kind();
        match read {
            None => "[bad object]".to_string(),
            Some((ObjectKind::Commit, data)) => match Commit::parse(hash, &data) {
                Ok(c) => {
                    let date = c
                        .committer()
                        .and_then(|s| s.time())
                        .map(|t| self.short_date(t.seconds, t.offset_minutes))
                        .unwrap_or_default();
                    format!("commit {date} - {}", subject(c.message()))
                }
                Err(_) => "[bad object]".to_string(),
            },
            Some((ObjectKind::Tag, data)) => match Tag::parse(hash, &data) {
                Ok(t) => {
                    // The tag's own time zone is not used for tags.
                    let date = t
                        .tagger()
                        .and_then(|s| s.time())
                        .map(|t| self.short_date(t.seconds, 0))
                        .unwrap_or_default();
                    let name = t
                        .name()
                        .map(|n| String::from_utf8_lossy(n).into_owned())
                        .unwrap_or_default();
                    format!("tag {date} - {name}")
                }
                Err(_) => "[bad object]".to_string(),
            },
            Some((k, _)) => k.name().to_string(),
        }
    }
}

pub(crate) fn looks_hex(s: &str) -> bool {
    is_hex(s)
}
