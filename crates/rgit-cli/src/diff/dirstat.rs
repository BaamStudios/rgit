// SPDX-License-Identifier: MIT
//! `--dirstat[=<param>,...]`, `-X`, `--cumulative`, `--dirstat-by-file`
//! and `diff.dirstat` (Documentation/diff-options.adoc).
//!
//! Each changed file contributes its "damage" to its directory:
//! `changes` counts the bytes removed from the old and added in the new
//! side (the material shared by both, as in rename detection, does not
//! count), `lines` the added and removed lines (for binary files the
//! 64-byte chunks of both sides together, rounded up), `files` one per
//! file (a file whose old and new contents only differ in order counts
//! 1). Directories are listed children first, in path order, as
//! `%4d.%d%% <dir>/` per mille of the total; a directory below the
//! cut-off passes its amount to its parent (unless `cumulative`, where
//! parents count their children anyway). Files at the top are never
//! listed. Observed with the `git` binary where the documentation is
//! silent.

use std::io::Write;

use crate::messages::diff as msg;

/// How the amounts are counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Measure {
    #[default]
    Changes,
    Lines,
    Files,
}

/// The parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    pub measure: Measure,
    pub cumulative: bool,
    /// Cut-off in per mille.
    pub permille: u64,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            measure: Measure::Changes,
            cumulative: false,
            permille: 30,
        }
    }
}

impl Params {
    /// Applies a comma separated list; returns the error lines for the
    /// parameters that were not understood (the others still apply).
    pub fn apply(&mut self, value: &str) -> Vec<String> {
        let mut errors = Vec::new();
        for p in value.split(',') {
            match p {
                "changes" => self.measure = Measure::Changes,
                "lines" => self.measure = Measure::Lines,
                "files" => self.measure = Measure::Files,
                "cumulative" => self.cumulative = true,
                "noncumulative" => self.cumulative = false,
                _ if p.starts_with(|c: char| c.is_ascii_digit()) => match parse_permille(p) {
                    Some(n) => self.permille = n,
                    None => errors.push(msg::dirstat_bad_percentage(p)),
                },
                _ => errors.push(msg::dirstat_unknown(p)),
            }
        }
        errors
    }
}

/// `<int>[.<digit>...]` percent as per mille (further digits are ignored).
fn parse_permille(p: &str) -> Option<u64> {
    let (int, frac) = match p.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (p, None),
    };
    let mut n: u64 = int.parse().ok()?;
    n = n.checked_mul(10)?;
    if let Some(f) = frac {
        if f.is_empty() || !f.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        n += u64::from(f.as_bytes()[0] - b'0');
    }
    Some(n)
}

/// The damage of one file pair.
pub fn damage(measure: Measure, old: &[u8], new: &[u8], lines: Option<(usize, usize)>) -> u64 {
    match measure {
        Measure::Files => 1,
        Measure::Lines => match lines {
            Some((a, d)) => (a + d) as u64,
            None => (old.len() as u64 + new.len() as u64).div_ceil(64),
        },
        Measure::Changes => {
            use rgit_diff::diffcore::similarity::Pieces;
            let a = Pieces::new(old);
            let b = Pieces::new(new);
            let copied = a.copied(&b);
            let added = b.total().saturating_sub(copied);
            // Contents that differ count at least 1 (observed: two swapped
            // lines).
            ((old.len() as u64).saturating_sub(copied) + added).max(1)
        }
    }
}

/// Writes the directory list for `files` (path, damage), sorted here.
pub fn write(out: &mut Vec<u8>, files: &mut Vec<(Vec<u8>, u64)>, params: &Params) {
    files.retain(|(_, d)| *d > 0);
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let total: u64 = files.iter().map(|(_, d)| d).sum();
    if total == 0 {
        return;
    }
    let mut pos = 0;
    while pos < files.len() {
        gather(out, files, &mut pos, b"", total, params);
    }
}

fn gather(
    out: &mut Vec<u8>,
    files: &[(Vec<u8>, u64)],
    pos: &mut usize,
    base: &[u8],
    total: u64,
    params: &Params,
) -> u64 {
    let mut sum = 0;
    // Files directly in `base` and sub-directories that pass something up.
    let (mut own, mut subdirs) = (0, 0);
    while *pos < files.len() {
        let (name, dmg) = &files[*pos];
        if !name.starts_with(base) {
            break;
        }
        let rest = &name[base.len()..];
        match rest.iter().position(|&c| c == b'/') {
            Some(slash) => {
                let sub = &name[..base.len() + slash + 1];
                let s = gather(out, files, pos, sub, total, params);
                if s > 0 {
                    subdirs += 1;
                }
                sum += s;
            }
            None => {
                sum += dmg;
                own += 1;
                *pos += 1;
            }
        }
    }
    // A directory that only passes on one sub-directory is not listed.
    if !base.is_empty() && !(own == 0 && subdirs == 1) {
        let permille = sum * 1000 / total;
        if sum > 0 && permille >= params.permille {
            write!(out, "{:4}.{}% ", permille / 10, permille % 10).unwrap();
            out.extend_from_slice(base);
            out.push(b'\n');
            if !params.cumulative {
                return 0;
            }
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params() {
        let mut p = Params::default();
        assert!(p.apply("files,10,cumulative").is_empty());
        assert_eq!(
            p,
            Params {
                measure: Measure::Files,
                cumulative: true,
                permille: 100
            }
        );
        assert!(p.apply("1.5").is_empty());
        assert_eq!(p.permille, 15);
        assert_eq!(p.apply("bogus").len(), 1);
    }

    #[test]
    fn nested() {
        let mut files = vec![
            (b"a/b/c/f".to_vec(), 31),
            (b"a/b/g".to_vec(), 15),
            (b"a/h".to_vec(), 21),
            (b"top".to_vec(), 25),
            (b"x/y".to_vec(), 51),
        ];
        let mut out = Vec::new();
        write(&mut out, &mut files, &Params::default());
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "  21.6% a/b/c/\n  10.4% a/b/\n  14.6% a/\n  35.6% x/\n"
        );
    }
}
