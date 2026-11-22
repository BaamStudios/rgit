// SPDX-License-Identifier: MIT
//! `-O<orderfile>` / `diff.orderFile` and `--rotate-to` / `--skip-to`
//! (Documentation/diff-options.adoc).
//!
//! The order file holds one glob per line (blank lines and `#` comments
//! are skipped, `\#` escapes a leading hash). A path takes the rank of
//! the first pattern it matches, where `*` also matches `/` and a path
//! matches when one of its leading directory parts does; paths without a
//! match come last, and the sort keeps the normal order within a rank.

use rgit_pathspec::wildmatch::{WildmatchFlags, wildmatch};

/// The patterns of an order file.
pub fn parse(data: &[u8]) -> Vec<Vec<u8>> {
    let mut patterns = Vec::new();
    for line in data.split(|&c| c == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() || line.starts_with(b"#") {
            continue;
        }
        let line = line
            .strip_prefix(b"\\")
            .filter(|l| l.starts_with(b"#"))
            .unwrap_or(line);
        patterns.push(line.to_vec());
    }
    patterns
}

/// The rank of `path`: the index of the first matching pattern, or the
/// number of patterns.
pub fn rank(patterns: &[Vec<u8>], path: &[u8]) -> usize {
    for (i, p) in patterns.iter().enumerate() {
        let mut cand = path;
        loop {
            if wildmatch(p, cand, WildmatchFlags::NONE) {
                return i;
            }
            match cand.iter().rposition(|&c| c == b'/') {
                Some(slash) => cand = &cand[..slash],
                None => break,
            }
        }
    }
    patterns.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks() {
        let p = parse(b"# comment\n*Makefile\n\n*.txt\n\\#x\nfoo*bar\n");
        assert_eq!(p.len(), 4);
        assert_eq!(rank(&p, b"c/Makefile"), 0);
        assert_eq!(rank(&p, b"d.txt"), 1);
        assert_eq!(rank(&p, b"#x"), 2);
        assert_eq!(rank(&p, b"foo/bar/baz/asdf"), 3);
        assert_eq!(rank(&p, b"foobarx"), 4);
    }
}
