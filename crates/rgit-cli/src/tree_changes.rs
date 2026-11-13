// SPDX-License-Identifier: MIT
//! Changes between two flat path lists (a tree, the index): the pairs
//! `commit` reports in its summary and in the "Changes to be committed"
//! section, with exact rename detection (identical blobs; `diff.renames`,
//! default on). Similarity-based renames are not detected yet (#145).

use std::collections::BTreeMap;

use rgit_hash::ObjectId;
use rgit_object::Object;

use crate::Die;
use crate::repository::Repo;

/// Mode and object of one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Side {
    pub mode: u32,
    pub oid: ObjectId,
}

/// Path → side, sorted by path.
pub type Flat = BTreeMap<Vec<u8>, Side>;

/// One changed path. A rename has `old_path`.
#[derive(Debug, Clone)]
pub struct Pair {
    pub path: Vec<u8>,
    pub old_path: Option<Vec<u8>>,
    pub old: Option<Side>,
    pub new: Option<Side>,
}

impl Pair {
    pub fn is_rename(&self) -> bool {
        self.old_path.is_some()
    }

    pub fn type_changed(&self) -> bool {
        matches!((self.old, self.new), (Some(o), Some(n)) if o.mode & 0o170000 != n.mode & 0o170000)
    }
}

/// The blobs of `tree` (recursively) into `out`.
pub fn flatten_tree(repo: &Repo, tree: &ObjectId, out: &mut Flat) -> Result<(), Die> {
    fn walk(repo: &Repo, tree: &ObjectId, base: &[u8], out: &mut Flat) -> Result<(), Die> {
        let Some(Object::Tree(t)) = repo.read_object(tree) else {
            return Err(Die::Fatal(crate::messages::ls_tree::could_not_read(
                &tree.to_hex(),
            )));
        };
        for e in t.entries {
            let mut path = base.to_vec();
            path.extend_from_slice(&e.name);
            let mode = e.mode.canonical();
            if mode.is_tree() {
                path.push(b'/');
                walk(repo, &e.oid, &path, out)?;
            } else {
                out.insert(
                    path,
                    Side {
                        mode: mode.bits(),
                        oid: e.oid,
                    },
                );
            }
        }
        Ok(())
    }
    walk(repo, tree, b"", out)
}

fn basename(p: &[u8]) -> &[u8] {
    match p.iter().rposition(|&b| b == b'/') {
        Some(i) => &p[i + 1..],
        None => p,
    }
}

/// The changes from `old` to `new`, sorted by (new) path. With `renames`,
/// a deleted path whose blob reappears unchanged (same mode) under an
/// added path becomes a rename at the added path.
pub fn diff(old: &Flat, new: &Flat, renames: bool) -> Vec<Pair> {
    let mut pairs: Vec<Pair> = Vec::new();
    let mut oi = old.iter().peekable();
    let mut ni = new.iter().peekable();
    loop {
        match (oi.peek(), ni.peek()) {
            (None, None) => break,
            (Some((op, os)), Some((np, ns))) if op == np => {
                if os != ns {
                    pairs.push(Pair {
                        path: op.to_vec(),
                        old_path: None,
                        old: Some(**os),
                        new: Some(**ns),
                    });
                }
                oi.next();
                ni.next();
            }
            (Some((op, os)), n) if n.is_none_or(|(np, _)| op < np) => {
                pairs.push(Pair {
                    path: op.to_vec(),
                    old_path: None,
                    old: Some(**os),
                    new: None,
                });
                oi.next();
            }
            (_, Some((np, ns))) => {
                pairs.push(Pair {
                    path: np.to_vec(),
                    old_path: None,
                    old: None,
                    new: Some(**ns),
                });
                ni.next();
            }
            (Some(_), None) => unreachable!(),
        }
    }
    if !renames {
        return pairs;
    }
    let is_blob = |s: &Side| s.mode & 0o170000 != 0o160000;
    let deleted: Vec<usize> = (0..pairs.len())
        .filter(|&i| pairs[i].new.is_none() && pairs[i].old.is_some_and(|s| is_blob(&s)))
        .collect();
    let mut used = vec![false; pairs.len()];
    let mut renames_found: Vec<(usize, usize)> = Vec::new();
    for i in 0..pairs.len() {
        let Some(ns) = pairs[i].new else { continue };
        if pairs[i].old.is_some() || !is_blob(&ns) {
            continue;
        }
        let candidates: Vec<usize> = deleted
            .iter()
            .copied()
            .filter(|&d| !used[d] && pairs[d].old == Some(ns))
            .collect();
        let pick = candidates
            .iter()
            .copied()
            .find(|&d| basename(&pairs[d].path) == basename(&pairs[i].path))
            .or_else(|| candidates.first().copied());
        if let Some(d) = pick {
            used[d] = true;
            renames_found.push((d, i));
        }
    }
    for (d, i) in renames_found {
        pairs[i].old_path = Some(pairs[d].path.clone());
        pairs[i].old = pairs[d].old;
    }
    pairs
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !used[*i])
        .map(|(_, p)| p)
        .collect()
}

/// `a => b` with the common leading directories and trailing path
/// components put outside braces: `dir/{a => b}`.
pub fn rename_text(a: &[u8], b: &[u8]) -> Vec<u8> {
    // Common prefix up to and including the last shared '/'.
    let mut pfx = 0;
    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        if x != y {
            break;
        }
        if *x == b'/' {
            pfx = i + 1;
        }
    }
    // Common suffix starting at a '/', not overlapping the prefix.
    let mut sfx = 0;
    let max = a.len().min(b.len()) - pfx;
    for i in 1..=max {
        let (x, y) = (a[a.len() - i], b[b.len() - i]);
        if x != y {
            break;
        }
        if x == b'/' {
            sfx = i;
        }
    }
    let mut out = Vec::new();
    if pfx == 0 && sfx == 0 {
        out.extend_from_slice(a);
        out.extend_from_slice(b" => ");
        out.extend_from_slice(b);
        return out;
    }
    out.extend_from_slice(&a[..pfx]);
    out.push(b'{');
    out.extend_from_slice(&a[pfx..a.len() - sfx]);
    out.extend_from_slice(b" => ");
    out.extend_from_slice(&b[pfx..b.len() - sfx]);
    out.push(b'}');
    out.extend_from_slice(&a[a.len() - sfx..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_texts() {
        assert_eq!(rename_text(b"f", b"g"), b"f => g");
        assert_eq!(rename_text(b"d/a", b"d/b"), b"d/{a => b}");
        assert_eq!(rename_text(b"a/x/f", b"b/x/f"), b"{a => b}/x/f");
        assert_eq!(rename_text(b"d/a/f", b"d/b/f"), b"d/{a => b}/f");
        assert_eq!(rename_text(b"d/f", b"f"), b"d/f => f");
    }
}
