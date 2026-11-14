// SPDX-License-Identifier: MIT
//! Changes between two flat path lists (a tree, the index): the pairs
//! `commit` reports in its summary and in the "Changes to be committed"
//! section, with rename detection (`diff.renames`, default on).

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
    /// Similarity of a rename (`rgit_diff::diffcore::MAX_SCORE` units).
    pub score: u32,
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

/// The changes from `old` to `new`, sorted by (new) path. With `renames`,
/// deleted and added paths are paired up by rename detection
/// (`rgit_diff::diffcore`, default similarity); a rename is reported at
/// the added path.
pub fn diff(repo: &Repo, old: &Flat, new: &Flat, renames: bool) -> Vec<Pair> {
    use rgit_diff::diffcore::{self, Detect, FilePair, Side as CoreSide};
    let side = |path: &[u8], s: &Side| CoreSide {
        path: path.to_vec(),
        mode: s.mode,
        oid: s.oid,
        extra: (),
    };
    let mut input: Vec<FilePair<()>> = Vec::new();
    let mut oi = old.iter().peekable();
    let mut ni = new.iter().peekable();
    loop {
        let (one, two) = match (oi.peek(), ni.peek()) {
            (None, None) => break,
            (Some((op, os)), Some((np, ns))) if op == np => {
                let same = os == ns;
                let r = (Some(side(op, os)), Some(side(np, ns)));
                oi.next();
                ni.next();
                if same {
                    continue;
                }
                r
            }
            (Some((op, os)), n) if n.is_none_or(|(np, _)| op < np) => {
                let r = (Some(side(op, os)), None);
                oi.next();
                r
            }
            (_, Some((np, ns))) => {
                let r = (None, Some(side(np, ns)));
                ni.next();
                r
            }
            (Some(_), None) => unreachable!(),
        };
        input.push(FilePair::new(one, two));
    }
    let opts = diffcore::Options {
        detect: if renames {
            Detect::Renames
        } else {
            Detect::Off
        },
        ..Default::default()
    };
    let contents = |s: &CoreSide<()>| match repo.read_object(&s.oid) {
        Some(Object::Blob(b)) => b.data,
        _ => Vec::new(),
    };
    let (out, _) = diffcore::run(input, &opts, &contents);
    out.into_iter()
        .map(|p| {
            let renamed = p.status == diffcore::Status::Renamed;
            let flat = |s: &Option<CoreSide<()>>| {
                s.as_ref().map(|s| Side {
                    mode: s.mode,
                    oid: s.oid,
                })
            };
            Pair {
                path: p
                    .two
                    .as_ref()
                    .or(p.one.as_ref())
                    .map(|s| s.path.clone())
                    .unwrap_or_default(),
                old_path: if renamed {
                    p.one.as_ref().map(|s| s.path.clone())
                } else {
                    None
                },
                old: flat(&p.one),
                new: flat(&p.two),
                score: if renamed { p.score } else { 0 },
            }
        })
        .collect()
}

/// `a => b` with the common leading directories and trailing path
/// components put outside braces: `dir/{a => b}`.
pub fn rename_text(a: &[u8], b: &[u8]) -> Vec<u8> {
    let (pfx, sfx) = rename_split(a, b);
    let mut out = Vec::new();
    if pfx == 0 && sfx == 0 {
        out.extend_from_slice(a);
        out.extend_from_slice(b" => ");
        out.extend_from_slice(b);
        return out;
    }
    let mid = |p: &[u8]| p[pfx.min(p.len() - sfx)..p.len() - sfx].to_vec();
    out.extend_from_slice(&a[..pfx]);
    out.push(b'{');
    out.extend_from_slice(&mid(a));
    out.extend_from_slice(b" => ");
    out.extend_from_slice(&mid(b));
    out.push(b'}');
    out.extend_from_slice(&a[a.len() - sfx..]);
    out
}

/// The common leading directories (`pfx` bytes, ending in `/`) and the
/// common trailing components (`sfx` bytes, starting with `/`) of a
/// rename. The suffix may share its `/` with the end of the prefix:
/// `d/e` and `d/f/e` give `d/{ => f}/e`.
pub fn rename_split(a: &[u8], b: &[u8]) -> (usize, usize) {
    let mut pfx = 0;
    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        if x != y {
            break;
        }
        if *x == b'/' {
            pfx = i + 1;
        }
    }
    let overlap = usize::from(pfx > 0);
    let max = (a.len() + overlap - pfx).min(b.len() + overlap - pfx);
    let mut sfx = 0;
    for i in 1..=max {
        let (x, y) = (a[a.len() - i], b[b.len() - i]);
        if x != y {
            break;
        }
        if x == b'/' {
            sfx = i;
        }
    }
    (pfx, sfx)
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
        assert_eq!(rename_text(b"d/e", b"d/f/e"), b"d/{ => f}/e");
        assert_eq!(rename_text(b"d/f/e", b"d/f/f/e"), b"d/f/{ => f}/e");
        assert_eq!(rename_text(b"c/d/e", b"d/e"), b"{c/d => d}/e");
    }
}
