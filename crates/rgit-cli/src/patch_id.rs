// SPDX-License-Identifier: MIT
//! Patch ids (Documentation/git-patch-id.adoc): a hash of a patch with
//! line numbers and (unless verbatim) whitespace ignored. `git patch-id`
//! reads patches from text; `cherry`, `rev-list --cherry-*` and `rebase`
//! compute the id of a commit's diff against its first parent.
//!
//! The reading of the text was determined from the behaviour of
//! `git patch-id` on crafted input:
//!
//! - A line `commit <id>`, `From <id>` or a bare `<id>` starts the patch of
//!   that commit. Lines before the first `diff ` line of a patch are
//!   ignored.
//! - In a file header, `index` lines only record the object names, a line
//!   that does not start with a letter ends the patch, and `--- ` starts
//!   counting lines like a one-line hunk (so that `+++ ` follows). Every
//!   other line is hashed.
//! - Hunk headers are not hashed; their line counts tell where a hunk ends.
//!   After a hunk only another hunk or a `diff ` line continues the patch.
//!   `\ No newline` lines are hashed only with `--verbatim`.
//! - A binary file is hashed by the two object names of its `index` line;
//!   the rest is skipped up to and including the next `diff ` line.
//! - The unstable id is one hash over the whole patch. The stable id is the
//!   sum of the hashes of the parts between a hunk and the next `diff `
//!   line (and after a binary file), bytes added with carry, first byte
//!   lowest; so the order of the files does not matter.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use rgit_diff::diffcore::Detect;
use rgit_hash::{HashKind, Hasher, ObjectId};
use rgit_object::Object;
use rgit_pathspec::Pathspec;

use crate::diff::collect::{Pair, TreeDiff};
use crate::diff::options::{DiffOpts, Formats};
use crate::diff::output::Output;
use crate::repository::Repo;

/// How ids are computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// One hash over the patch, whitespace ignored (git 1.9 and older).
    Unstable,
    /// The sum of per-file hashes, whitespace ignored.
    Stable,
    /// Like `Stable`, whitespace kept.
    Verbatim,
}

impl Mode {
    fn stable(self) -> bool {
        self != Mode::Unstable
    }
}

/// Old and new lines still expected in a hunk; old `-1` in a file
/// header, `(0, 0)` after a hunk.
type Counts = (i64, i64);

const HEADER: Counts = (-1, -1);

/// Computes patch ids from patch text, one per patch.
pub struct Parser {
    mode: Mode,
    kind: HashKind,
    /// The commit of the current patch (null when the input named none).
    commit: ObjectId,
    /// The commit the next patch belongs to.
    next_commit: ObjectId,
    counts: Counts,
    /// Skipping the rest of a binary file.
    binary: bool,
    /// Bytes hashed for this patch (0: not started).
    hashed: usize,
    ctx: Hasher,
    /// The stable sum of the parts flushed so far.
    sum: Vec<u8>,
    /// The object names of the last `index` line.
    old_id: Vec<u8>,
    new_id: Vec<u8>,
}

/// Add `digest` to `sum` (equal length), byte by byte with carry.
fn add_into(sum: &mut [u8], digest: &[u8]) {
    let mut carry = 0u16;
    for (s, d) in sum.iter_mut().zip(digest) {
        let v = u16::from(*s) + u16::from(*d) + carry;
        *s = (v & 0xff) as u8;
        carry = v >> 8;
    }
}

/// The commit a header line names: `commit <id>`, `From <id>` or `<id>`
/// at the start of the line (more may follow).
fn commit_line(line: &[u8], kind: HashKind) -> Option<ObjectId> {
    let rest = line
        .strip_prefix(b"commit ")
        .or_else(|| line.strip_prefix(b"From "))
        .unwrap_or(line);
    let hex = rest.get(..kind.hex_len())?;
    ObjectId::from_hex(kind, hex).ok()
}

/// The digits at the start of `s`, their value (0 without any) and the
/// rest.
fn digits(s: &[u8]) -> (usize, i64, &[u8]) {
    let end = s
        .iter()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(s.len());
    let value = s[..end].iter().fold(0i64, |v, &d| {
        v.saturating_mul(10).saturating_add(i64::from(d - b'0'))
    });
    (end, value, &s[end..])
}

/// `<start>[,<count>]`: the count (1 without a comma) and, when the last
/// number has digits, the rest.
fn range(s: &[u8]) -> (i64, Option<&[u8]>) {
    let (n, _, rest) = digits(s);
    let (n, count, rest) = match rest.strip_prefix(b",") {
        Some(r) => digits(r),
        None => (n, 1, rest),
    };
    (count, (n > 0).then_some(rest))
}

/// Read the line counts of a hunk header `@@ -a[,b] +c[,d] @@` (the line
/// starts with `@@ -`) into `counts`. A malformed header sets what it
/// could read before the error and leaves the rest.
fn hunk_counts(line: &[u8], counts: &mut (i64, i64)) {
    let (old, rest) = range(&line[4..]);
    counts.0 = old;
    let Some(rest) = rest.and_then(|r| r.strip_prefix(b" +")) else {
        return;
    };
    counts.1 = range(rest).0;
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

impl Parser {
    pub fn new(mode: Mode, kind: HashKind) -> Parser {
        Parser {
            mode,
            kind,
            commit: ObjectId::null(kind),
            next_commit: ObjectId::null(kind),
            counts: HEADER,
            binary: false,
            hashed: 0,
            ctx: Hasher::new(kind),
            sum: vec![0; kind.byte_len()],
            old_id: Vec::new(),
            new_id: Vec::new(),
        }
    }

    /// Hash a line: as it is with `--verbatim`, else without whitespace.
    fn hash(&mut self, line: &[u8]) {
        if self.mode == Mode::Verbatim {
            self.ctx.update(line);
            self.hashed += line.len();
        } else {
            let kept: Vec<u8> = line.iter().copied().filter(|&b| !is_space(b)).collect();
            self.ctx.update(&kept);
            self.hashed += kept.len();
        }
    }

    /// Stable ids: add the hash of the current part to the sum.
    fn flush(&mut self) {
        let ctx = std::mem::replace(&mut self.ctx, Hasher::new(self.kind));
        let digest = ctx.finalize().unwrap_or_else(|e| e.id);
        add_into(&mut self.sum, digest.as_bytes());
    }

    /// End the current patch: its id and commit, if it hashed anything.
    /// The next patch belongs to `self.next_commit`.
    fn finish(&mut self) -> Option<(ObjectId, ObjectId)> {
        let hashed = std::mem::take(&mut self.hashed);
        let commit = std::mem::replace(&mut self.commit, self.next_commit);
        self.next_commit = ObjectId::null(self.kind);
        self.counts = HEADER;
        self.binary = false;
        let result = if self.mode.stable() {
            self.flush();
            let sum = std::mem::replace(&mut self.sum, vec![0; self.kind.byte_len()]);
            ObjectId::from_bytes(self.kind, &sum).ok()
        } else {
            let ctx = std::mem::replace(&mut self.ctx, Hasher::new(self.kind));
            Some(ctx.finalize().unwrap_or_else(|e| e.id))
        };
        if hashed == 0 {
            return None;
        }
        result.map(|id| (id, commit))
    }

    /// Feed one line (with its newline, if any). Returns the id of the
    /// patch it ended: `(patch id, commit id)`; the commit is null when
    /// the input did not name one.
    pub fn line(&mut self, line: &[u8]) -> Option<(ObjectId, ObjectId)> {
        if let Some(id) = commit_line(line, self.kind) {
            self.next_commit = id;
            return self.finish();
        }
        if self.hashed == 0 && !line.starts_with(b"diff ") {
            return None;
        }
        if self.binary {
            if line.starts_with(b"diff ") {
                self.binary = false;
            }
            return None;
        }
        if self.counts.0 != -1 && line.starts_with(b"\\") {
            if self.mode == Mode::Verbatim {
                self.hash(line);
            }
            return None;
        }
        match self.counts {
            (-1, _) => {
                if line.starts_with(b"GIT binary patch") || line.starts_with(b"Binary files") {
                    let (old, new) = (self.old_id.clone(), self.new_id.clone());
                    self.ctx.update(&old);
                    self.ctx.update(&new);
                    self.hashed += old.len() + new.len();
                    if self.mode.stable() {
                        self.flush();
                    }
                    self.binary = true;
                    return None;
                }
                if let Some(rest) = line.strip_prefix(b"index ") {
                    let end = rest.iter().position(|&b| is_space(b)).unwrap_or(rest.len());
                    let ids = &rest[..end];
                    if let Some(i) = ids.windows(2).position(|w| w == b"..") {
                        self.old_id = ids[..i].to_vec();
                        self.new_id = ids[i + 2..].to_vec();
                    }
                    return None;
                }
                if line.starts_with(b"--- ") {
                    self.counts = (1, 1);
                } else if !line.first().is_some_and(u8::is_ascii_alphabetic) {
                    return self.finish();
                }
            }
            (0, 0) => {
                if line.starts_with(b"@@ -") {
                    hunk_counts(line, &mut self.counts);
                    return None;
                }
                if !line.starts_with(b"diff ") {
                    return self.finish();
                }
                if self.mode.stable() {
                    self.flush();
                }
                self.counts = HEADER;
            }
            _ => {}
        }
        let (old, new) = &mut self.counts;
        match line.first() {
            Some(b'-') => *old -= 1,
            Some(b'+') => *new -= 1,
            Some(b' ') => {
                *old -= 1;
                *new -= 1;
            }
            _ => {}
        }
        self.hash(line);
        None
    }

    /// The end of the input: the id of the last patch, if any.
    pub fn end(&mut self) -> Option<(ObjectId, ObjectId)> {
        self.finish()
    }
}

/// The ids of every patch in `input`.
pub fn parse(input: &[u8], mode: Mode, kind: HashKind) -> Vec<(ObjectId, ObjectId)> {
    let mut p = Parser::new(mode, kind);
    let mut out = Vec::new();
    for line in input.split_inclusive(|&b| b == b'\n') {
        out.extend(p.line(line));
    }
    out.extend(p.end());
    out
}

/// The stable id of a patch text produced for one commit.
pub fn of_text(text: &[u8], kind: HashKind) -> Option<ObjectId> {
    parse(text, Mode::Stable, kind).first().map(|(id, _)| *id)
}

/// Patch ids of commits: the stable id of the diff against the first
/// parent (the empty tree for a root commit), without rename detection
/// and with full object names. Merges have none.
pub struct CommitIds<'r> {
    repo: &'r Repo,
    pathspec: Pathspec,
    /// Commits whose first parent is not used (shallow boundary).
    shallow: HashSet<ObjectId>,
    full: RefCell<HashMap<ObjectId, Option<ObjectId>>>,
}

impl<'r> CommitIds<'r> {
    pub fn new(repo: &'r Repo) -> CommitIds<'r> {
        let shallow = std::fs::read(repo.setup.common_dir.join("shallow"))
            .unwrap_or_default()
            .split(|&b| b == b'\n')
            .filter_map(|l| ObjectId::from_hex(repo.hash, l).ok())
            .collect();
        CommitIds {
            repo,
            pathspec: Pathspec::default(),
            shallow,
            full: RefCell::new(HashMap::new()),
        }
    }

    /// The changed paths of `id` against its first parent; `None` for a
    /// merge or an unreadable commit.
    fn pairs(&self, id: &ObjectId) -> Option<Vec<Pair>> {
        let Some(Object::Commit(c)) = self.repo.read_object(id) else {
            return None;
        };
        let parents = if self.shallow.contains(id) {
            Vec::new()
        } else {
            c.parents().ok()?
        };
        if parents.len() > 1 {
            return None;
        }
        let tree = c.tree().ok()?;
        let parent_tree = match parents.first() {
            Some(p) => match self.repo.read_object(p) {
                Some(Object::Commit(pc)) => Some(pc.tree().ok()?),
                _ => return None,
            },
            None => None,
        };
        let mut w = TreeDiff {
            repo: self.repo,
            pathspec: &self.pathspec,
            recursive: true,
            show_trees: false,
            pairs: Vec::new(),
        };
        w.walk(parent_tree.as_ref(), Some(&tree), b"").ok()?;
        Some(w.pairs)
    }

    /// A key that equal patch ids imply: the changed paths, each with
    /// whether it was added, deleted or modified. Needs no file contents.
    pub fn header_key(&self, id: &ObjectId) -> Option<Vec<u8>> {
        let pairs = self.pairs(id)?;
        let mut key = Vec::new();
        for p in &pairs {
            key.push(match (&p.old, &p.new) {
                (None, _) => b'A',
                (_, None) => b'D',
                _ => b'M',
            });
            key.extend_from_slice(&p.path);
            key.push(0);
        }
        Some(key)
    }

    /// The patch id of commit `id`.
    pub fn patch_id(&self, id: &ObjectId) -> Option<ObjectId> {
        if let Some(found) = self.full.borrow().get(id) {
            return *found;
        }
        let found = self.compute(id);
        self.full.borrow_mut().insert(*id, found);
        found
    }

    fn compute(&self, id: &ObjectId) -> Option<ObjectId> {
        let pairs = self.pairs(id)?;
        let opts = DiffOpts {
            full_index: true,
            detect: Some(Detect::Off),
            ..DiffOpts::default()
        };
        let formats = Formats {
            patch: true,
            ..Formats::default()
        };
        let out = Output::new(Some(self.repo), self.repo.hash, &opts, formats, false).ok()?;
        let text = out.render(pairs).ok()?.0;
        if text.is_empty() {
            // No change at all still has an id (of nothing).
            return Hasher::new(self.repo.hash).finalize().ok();
        }
        of_text(&text, self.repo.hash)
    }

    /// The commits of `left` and `right` with an equal patch id on the
    /// other side. Full ids are computed only for commits whose
    /// [`header key`](Self::header_key) occurs on the other side.
    pub fn equivalent(&self, left: &[ObjectId], right: &[ObjectId]) -> HashSet<ObjectId> {
        let keys = |ids: &[ObjectId]| -> Vec<(ObjectId, Vec<u8>)> {
            ids.iter()
                .filter_map(|id| Some((*id, self.header_key(id)?)))
                .collect()
        };
        let lk = keys(left);
        let rk = keys(right);
        let lset: HashSet<&[u8]> = lk.iter().map(|(_, k)| k.as_slice()).collect();
        let rset: HashSet<&[u8]> = rk.iter().map(|(_, k)| k.as_slice()).collect();
        let full = |list: &[(ObjectId, Vec<u8>)], other: &HashSet<&[u8]>| {
            list.iter()
                .filter(|(_, k)| other.contains(k.as_slice()))
                .filter_map(|(id, _)| Some((*id, self.patch_id(id)?)))
                .collect::<Vec<_>>()
        };
        let lf = full(&lk, &rset);
        let rf = full(&rk, &lset);
        let lids: HashSet<ObjectId> = lf.iter().map(|(_, p)| *p).collect();
        let rids: HashSet<ObjectId> = rf.iter().map(|(_, p)| *p).collect();
        let mut out = HashSet::new();
        out.extend(
            lf.iter()
                .filter(|(_, p)| rids.contains(p))
                .map(|(id, _)| *id),
        );
        out.extend(
            rf.iter()
                .filter(|(_, p)| lids.contains(p))
                .map(|(id, _)| *id),
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(input: &str, mode: Mode) -> String {
        let ids = parse(input.as_bytes(), mode, HashKind::Sha1);
        assert_eq!(ids.len(), 1, "{input}");
        ids[0].0.to_hex()
    }

    const ONE: &str = "diff --git a/bar b/bar\nindex bdaf90f..31051f6 100644\n\
                       --- a/bar\n+++ b/bar\n@@ -2 +2,2 @@\n b\n+c\n";

    #[test]
    fn ignores_line_numbers_whitespace_and_index_lines() {
        let want = id(ONE, Mode::Unstable);
        let other = "diff --git  a/bar b/bar\nindex 1111111..2222222\n\
                     --- a/bar\n+++ b/bar\n@@ -20 +20,2 @@ fn\n  b\n+ c\r\n";
        assert_eq!(id(other, Mode::Unstable), want);
        assert_ne!(id(other, Mode::Verbatim), id(ONE, Mode::Verbatim));
    }

    #[test]
    fn stable_ignores_file_order() {
        let two = ONE.replace("bar", "foo");
        let a = format!("{ONE}{two}");
        let b = format!("{two}{ONE}");
        assert_eq!(id(&a, Mode::Stable), id(&b, Mode::Stable));
        assert_ne!(id(&a, Mode::Unstable), id(&b, Mode::Unstable));
        // One file: the stable id is the plain hash.
        assert_eq!(id(ONE, Mode::Stable), id(ONE, Mode::Unstable));
    }

    #[test]
    fn commits_and_patch_ends() {
        let input = format!(
            "commit 1234567890123456789012345678901234567890\nAuthor: x\n\n    msg\n\n{ONE}\
             From abcdef7890123456789012345678901234567890 Mon Sep 17 00:00:00 2001\n\
             Subject: s\n---\n bar | 1 +\n\n{ONE}-- \n2.0\n\n{ONE}"
        );
        let ids = parse(input.as_bytes(), Mode::Unstable, HashKind::Sha1);
        let hexes: Vec<String> = ids.iter().map(|(_, c)| c.to_hex()).collect();
        assert_eq!(
            hexes,
            [
                "1234567890123456789012345678901234567890",
                "abcdef7890123456789012345678901234567890",
                "0000000000000000000000000000000000000000",
            ]
        );
        assert!(ids.iter().all(|(p, _)| *p == ids[0].0));
    }

    #[test]
    fn hunk_counts_parse() {
        let counts = |line: &[u8]| {
            let mut c = (0, 0);
            hunk_counts(line, &mut c);
            c
        };
        assert_eq!(counts(b"@@ -2 +2,2 @@"), (1, 2));
        assert_eq!(counts(b"@@ -0,0 +1 @@ x"), (0, 1));
        assert_eq!(counts(b"@@ -x +1 @@"), (1, 0));
        assert_eq!(counts(b"@@ -1 +x @@"), (1, 1));
        assert_eq!(counts(b"@@ -2,x +1 @@"), (0, 0));
    }
}
