// SPDX-License-Identifier: MIT
//! A small line-based unified diff, used to render mismatches readably.
//!
//! The algorithm is a plain longest-common-subsequence table after trimming
//! the common prefix and suffix. Test outputs are small, so quadratic cost is
//! fine; above [`MAX_CELLS`] the differing middle is shown as one replaced block.

/// Lines of context around each change.
const CONTEXT: usize = 3;

/// Upper bound for the LCS table (rows * columns).
const MAX_CELLS: usize = 4_000_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    Equal,
    Delete,
    Insert,
}

/// Splits `text` into lines, each keeping its trailing `\n` (the last one may lack it).
fn lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

fn ops(a: &[&str], b: &[&str]) -> Vec<(Op, usize, usize)> {
    // (op, index in a, index in b); for Insert the a index is unused and vice versa.
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let max_suffix = a.len().min(b.len()) - prefix;
    let suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(max_suffix)
        .take_while(|(x, y)| x == y)
        .count();

    let mut out = Vec::with_capacity(a.len() + b.len());
    for i in 0..prefix {
        out.push((Op::Equal, i, i));
    }

    let am = &a[prefix..a.len() - suffix];
    let bm = &b[prefix..b.len() - suffix];
    let (n, m) = (am.len(), bm.len());
    if n.saturating_mul(m) > MAX_CELLS {
        out.extend((0..n).map(|i| (Op::Delete, prefix + i, 0)));
        out.extend((0..m).map(|j| (Op::Insert, 0, prefix + j)));
    } else {
        // table[i][j] = LCS length of am[i..] and bm[j..]
        let w = m + 1;
        let mut table = vec![0u32; (n + 1) * w];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                table[i * w + j] = if am[i] == bm[j] {
                    table[(i + 1) * w + j + 1] + 1
                } else {
                    table[(i + 1) * w + j].max(table[i * w + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n || j < m {
            if i < n && j < m && am[i] == bm[j] {
                out.push((Op::Equal, prefix + i, prefix + j));
                i += 1;
                j += 1;
            } else if i < n && (j == m || table[(i + 1) * w + j] >= table[i * w + j + 1]) {
                out.push((Op::Delete, prefix + i, 0));
                i += 1;
            } else {
                out.push((Op::Insert, 0, prefix + j));
                j += 1;
            }
        }
    }

    let (sa, sb) = (a.len() - suffix, b.len() - suffix);
    for k in 0..suffix {
        out.push((Op::Equal, sa + k, sb + k));
    }
    out
}

fn push_line(out: &mut String, sign: char, line: &str) {
    out.push(sign);
    out.push_str(line);
    if !line.ends_with('\n') {
        out.push_str("\n\\ No newline at end of file\n");
    }
}

/// Renders a unified diff from `old` to `new`. Returns an empty string when
/// both are equal.
pub fn unified_diff(old: &str, new: &str, old_name: &str, new_name: &str) -> String {
    if old == new {
        return String::new();
    }
    let a = lines(old);
    let b = lines(new);
    let ops = ops(&a, &b);

    // Position before each op, in a and b.
    let mut pos = Vec::with_capacity(ops.len());
    let (mut pa, mut pb) = (0usize, 0usize);
    for &(op, _, _) in &ops {
        pos.push((pa, pb));
        match op {
            Op::Equal => {
                pa += 1;
                pb += 1;
            }
            Op::Delete => pa += 1,
            Op::Insert => pb += 1,
        }
    }

    // Group changed ops into hunks with context.
    let changes: Vec<usize> = (0..ops.len()).filter(|&k| ops[k].0 != Op::Equal).collect();
    let mut hunks: Vec<(usize, usize)> = Vec::new();
    for &k in &changes {
        let start = k.saturating_sub(CONTEXT);
        let end = (k + CONTEXT + 1).min(ops.len());
        match hunks.last_mut() {
            Some(last) if start <= last.1 => last.1 = end,
            _ => hunks.push((start, end)),
        }
    }

    let mut out = format!("--- {old_name}\n+++ {new_name}\n");
    for (start, end) in hunks {
        let old_len = ops[start..end].iter().filter(|o| o.0 != Op::Insert).count();
        let new_len = ops[start..end].iter().filter(|o| o.0 != Op::Delete).count();
        let (oa, ob) = pos[start];
        let old_start = if old_len == 0 { oa } else { oa + 1 };
        let new_start = if new_len == 0 { ob } else { ob + 1 };
        out.push_str(&format!(
            "@@ -{old_start},{old_len} +{new_start},{new_len} @@\n"
        ));
        for &(op, ia, ib) in &ops[start..end] {
            match op {
                Op::Equal => push_line(&mut out, ' ', a[ia]),
                Op::Delete => push_line(&mut out, '-', a[ia]),
                Op::Insert => push_line(&mut out, '+', b[ib]),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::unified_diff;

    #[test]
    fn equal_is_empty() {
        assert_eq!(unified_diff("a\nb\n", "a\nb\n", "x", "y"), "");
    }

    #[test]
    fn single_change() {
        let d = unified_diff("a\nb\nc\n", "a\nB\nc\n", "git", "rgit");
        assert_eq!(d, "--- git\n+++ rgit\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n");
    }

    #[test]
    fn insertion_into_empty() {
        let d = unified_diff("", "x\n", "git", "rgit");
        assert_eq!(d, "--- git\n+++ rgit\n@@ -0,0 +1,1 @@\n+x\n");
    }

    #[test]
    fn missing_newline_is_marked() {
        let d = unified_diff("a\n", "a", "git", "rgit");
        assert_eq!(
            d,
            "--- git\n+++ rgit\n@@ -1,1 +1,1 @@\n-a\n+a\n\\ No newline at end of file\n"
        );
    }

    #[test]
    fn distant_changes_make_two_hunks() {
        let old: String = (1..=20).map(|i| format!("{i}\n")).collect();
        let new: String = (1..=20)
            .map(|i| match i {
                2 => "two\n".to_string(),
                19 => "nineteen\n".to_string(),
                _ => format!("{i}\n"),
            })
            .collect();
        let d = unified_diff(&old, &new, "git", "rgit");
        assert_eq!(d.matches("@@ ").count(), 2, "{d}");
        assert!(d.contains("@@ -1,5 +1,5 @@\n 1\n-2\n+two\n 3\n"), "{d}");
        assert!(d.contains("-19\n+nineteen\n 20\n"), "{d}");
    }
}
