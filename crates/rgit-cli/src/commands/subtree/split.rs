// SPDX-License-Identifier: MIT
//! `git subtree split`: a synthetic history of the subtree.
//!
//! Every commit of the main project that has the subtree gets a
//! counterpart whose root tree is the subtree. A counterpart is a new
//! commit (same author, committer and message, optionally annotated)
//! unless the commit left the subtree as one of its rewritten parents had
//! it; then that parent stands in for it. Commits without the subtree map
//! to themselves when they descend from subproject history (they are
//! subproject commits merged in by `add` or `merge`), else to nothing.
//!
//! Splitting the same history twice gives the same commits. Earlier
//! `add`, `--rejoin` and squash commits (found by their trailers) seed the
//! mapping and cut the walk short.

use std::collections::{HashMap, HashSet};

use bstr::ByteSlice;
use rgit_hash::ObjectId;
use rgit_object::{Commit, ObjectKind, Signature, SignatureRef};

use super::{Subtree, die};
use crate::messages::subtree as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;

/// The walk's state.
struct Walk<'a> {
    st: &'a Subtree,
    repo: Repo,
    /// Original commit to its counterpart (itself for subproject commits).
    cache: HashMap<ObjectId, ObjectId>,
    /// Commits seen without the subtree.
    notree: HashSet<ObjectId>,
    revcount: usize,
    revmax: usize,
    createcount: usize,
    extracount: usize,
    latest_new: Option<ObjectId>,
    latest_old: Option<ObjectId>,
}

/// A pending commit of the walk; `next` is the parent to look at next.
struct Frame {
    rev: ObjectId,
    parents: Vec<ObjectId>,
    next: usize,
}

/// Split the subtree out of the history of `rev`. `Ok(Err(code))` when a
/// child command (the `--rejoin` merge, the branch update) failed.
pub(super) fn split(
    ctx: &Context,
    st: &Subtree,
    rev: &ObjectId,
    repository: Option<&str>,
    for_push: bool,
) -> Result<Result<ObjectId, i32>, Die> {
    if st.opts.rejoin {
        st.ensure_clean()?;
    }
    st.debug(&format!("Splitting {}...", st.dir));
    let mut cache: HashMap<ObjectId, ObjectId> = HashMap::new();
    if let Some(onto) = &st.opts.onto {
        st.debug(&format!("Reading history for --onto={onto}..."));
        let (_, out) = st.capture(&["rev-list", onto], None, false)?;
        for line in out.lines() {
            if let Ok(id) = ObjectId::from_hex_any(line.trim()) {
                st.debug(&format!("cache: {id}"));
                cache_set(&mut cache, id, id)?;
            }
        }
    }
    let unrevs = find_existing_splits(st, rev, repository, &mut cache)?;

    let mut args: Vec<String> = vec![
        "rev-list".into(),
        "--topo-order".into(),
        "--reverse".into(),
        "--parents".into(),
        rev.to_hex(),
    ];
    args.extend(unrevs.iter().map(|u| format!("^{u}")));
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let (_, out) = st.capture(&argv, None, false)?;
    let mut lines: Vec<Vec<ObjectId>> = Vec::new();
    for line in out.lines() {
        let ids: Vec<ObjectId> = line
            .split_str(" ")
            .filter_map(|w| ObjectId::from_hex_any(w.trim()).ok())
            .collect();
        if !ids.is_empty() {
            lines.push(ids);
        }
    }

    let repo = Repo::open(ctx, Mode::Required)?;
    let mut walk = Walk {
        st,
        repo,
        cache,
        notree: HashSet::new(),
        revcount: 0,
        revmax: lines.len(),
        createcount: 0,
        extracount: 0,
        latest_new: None,
        latest_old: None,
    };
    for ids in lines {
        walk.process(ids[0], ids[1..].to_vec())?;
    }
    let Some(latest_new) = walk.latest_new else {
        return Err(die(msg::NO_NEW_REVISIONS));
    };
    let latest_old = walk.latest_old.unwrap_or(*rev);
    drop(walk);

    if st.opts.rejoin {
        st.debug("Merging split branch into HEAD...");
        let message = st.rejoin_msg(&latest_old, &latest_new);
        // The first join of the subtree is an `add`, later ones merges.
        let code = if st.find_latest_squash(repository)?.is_some() {
            match st.cmd_merge(&latest_new.to_hex(), repository, Some(&message), true) {
                // "Subtree is already at commit ...": nothing to rejoin. git
                // ends there, and `push` then deletes the remote branch (it
                // pushes an empty source); rgit pushes the split instead.
                Err(Die::Exit(0)) if for_push => 0,
                other => other?,
            }
        } else {
            st.ensure_clean()?;
            st.add_commit(&latest_new, Some(&message))?
        };
        if code != 0 {
            return Ok(Err(code));
        }
    }
    if let Some(branch) = &st.opts.branch {
        let full = format!("refs/heads/{branch}");
        let existing = st.verify(&full)?;
        if let Some(old) = existing {
            let code =
                st.run_silent(&["merge-base", "--is-ancestor", &old.to_hex(), &latest_new.to_hex()])?;
            if code != 0 {
                return Err(die(&msg::not_ancestor(branch, &latest_new.to_hex())));
            }
        }
        let code = st.run(&["update-ref", "-m", "subtree split", &full, &latest_new.to_hex()])?;
        if code != 0 {
            return Ok(Err(code));
        }
        st.say(&msg::branch_done(existing.is_some(), branch));
    }
    Ok(Ok(latest_new))
}

/// Seed `cache` from the trailers of earlier `add`, `--rejoin` and squash
/// commits; the commits the walk can leave out (the first parents of both
/// sides of each earlier join).
fn find_existing_splits(
    st: &Subtree,
    rev: &ObjectId,
    repository: Option<&str>,
    cache: &mut HashMap<ObjectId, ObjectId>,
) -> Result<Vec<ObjectId>, Die> {
    st.debug("Looking for prior splits...");
    let log = st.trailer_log(&rev.to_hex(), st.opts.ignore_joins)?;
    let mut unrevs: Vec<ObjectId> = Vec::new();
    let mut commit = String::new();
    let mut main: Option<ObjectId> = None;
    let mut sub: Option<ObjectId> = None;
    let mut exclude = |st: &Subtree, id: &ObjectId| -> Result<(), Die> {
        if let Some(p) = st.verify(&format!("{id}^"))? {
            unrevs.push(p);
        }
        Ok(())
    };
    for line in log.split(|&b| b == b'\n') {
        let line = String::from_utf8_lossy(line);
        let mut words = line.split_ascii_whitespace();
        let a = words.next().unwrap_or("");
        let b = words.next().unwrap_or("");
        match a {
            "START" => {
                commit = b.to_string();
                main = None;
                sub = None;
            }
            _ if a == msg::TRAILER_MAINLINE => main = ObjectId::from_hex_any(b.as_bytes()).ok(),
            _ if a == msg::TRAILER_SPLIT => {
                sub = Some(st.split_commit(b, &commit, repository)?);
            }
            "END" => {
                st.debug(&format!(
                    "  Main is: '{}'",
                    main.map(|m| m.to_hex()).unwrap_or_default()
                ));
                match (main, sub) {
                    (Some(m), Some(s)) => {
                        st.debug(&format!("    Prior: {m} -> {s}"));
                        cache_set(cache, m, s)?;
                        cache_set(cache, s, s)?;
                        exclude(st, &m)?;
                        exclude(st, &s)?;
                    }
                    (None, Some(s)) => {
                        if let Ok(c) = ObjectId::from_hex_any(commit.as_bytes()) {
                            st.debug(&format!("    Squash: {c} from {s}"));
                            cache_set(cache, c, s)?;
                        }
                    }
                    _ => {}
                }
                main = None;
                sub = None;
            }
            _ => {}
        }
    }
    Ok(unrevs)
}

impl Walk<'_> {
    fn debug(&self, depth: usize, m: &str) {
        if self.st.opts.debug {
            eprintln!("{}{m}", "  ".repeat(depth));
        }
    }

    /// Map `rev` (and, first, any of its parents the walk has not seen).
    fn process(&mut self, rev: ObjectId, parents: Vec<ObjectId>) -> Result<(), Die> {
        self.revcount += 1;
        let mut stack: Vec<Frame> = Vec::new();
        if self.enter(&rev, &parents, 0) {
            stack.push(Frame {
                rev,
                parents,
                next: 0,
            });
        }
        while let Some(top) = stack.last_mut() {
            if top.next < top.parents.len() {
                let p = top.parents[top.next];
                top.next += 1;
                if self.cache.contains_key(&p) || self.notree.contains(&p) {
                    continue;
                }
                self.extracount += 1;
                let pp = self.parents_of(&p)?;
                let depth = stack.len();
                if self.enter(&p, &pp, depth) {
                    stack.push(Frame {
                        rev: p,
                        parents: pp,
                        next: 0,
                    });
                }
                continue;
            }
            let frame = stack.pop().expect("not empty");
            self.finish(&frame.rev, &frame.parents, stack.len())?;
        }
        Ok(())
    }

    /// Start on a commit: false when it is mapped already.
    fn enter(&mut self, rev: &ObjectId, parents: &[ObjectId], depth: usize) -> bool {
        // git-subtree counts the revisions with `wc -l`, and the `wc` of
        // macOS pads the number to eight columns.
        let revmax = if cfg!(target_os = "macos") {
            format!("{:>8}", self.revmax)
        } else {
            self.revmax.to_string()
        };
        self.st.progress(&format!(
            "{}/{revmax} ({}) [{}]",
            self.revcount, self.createcount, self.extracount
        ));
        self.debug(depth, &format!("Processing commit: {rev}"));
        if let Some(prior) = self.cache.get(rev) {
            self.debug(depth, &format!("  prior: {prior}"));
            return false;
        }
        self.createcount += 1;
        self.debug(depth, &format!("  parents: {}", join(parents)));
        true
    }

    /// Map a commit whose parents are all mapped (or without subtree).
    fn finish(&mut self, rev: &ObjectId, parents: &[ObjectId], depth: usize) -> Result<(), Die> {
        let newparents: Vec<ObjectId> = parents
            .iter()
            .filter_map(|p| self.cache.get(p).copied())
            .collect();
        let shown: Vec<String> = newparents.iter().map(|p| p.to_hex()).collect();
        self.debug(depth, &format!("  newparents: {}", shown.join("\n")));
        let tree = self.subtree_of(rev)?;
        self.debug(
            depth,
            &format!(
                "  tree is: {}",
                tree.map(|t| t.to_hex()).unwrap_or_default()
            ),
        );
        let Some(tree) = tree else {
            self.notree.insert(*rev);
            if !newparents.is_empty() {
                self.cache.insert(*rev, *rev);
            }
            return Ok(());
        };
        let newrev = self.copy_or_skip(rev, &tree, &newparents, depth)?;
        self.debug(depth, &format!("  newrev is: {newrev}"));
        self.cache.insert(*rev, newrev);
        self.latest_new = Some(newrev);
        self.latest_old = Some(*rev);
        Ok(())
    }

    fn read_commit(&self, id: &ObjectId) -> Result<Commit, Die> {
        let obj = self
            .repo
            .odb()
            .read(id)
            .ok()
            .flatten()
            .ok_or_else(|| Die::Fatal(format!("bad object {id}")))?;
        Commit::parse(self.repo.hash, &obj.data)
            .map_err(|_| Die::Fatal(format!("bad object {id}")))
    }

    fn parents_of(&self, id: &ObjectId) -> Result<Vec<ObjectId>, Die> {
        self.read_commit(id)?
            .parents()
            .map_err(|_| Die::Fatal(format!("bad object {id}")))
    }

    fn tree_of(&self, id: &ObjectId) -> Option<ObjectId> {
        self.read_commit(id).ok().and_then(|c| c.tree().ok())
    }

    /// The tree at the prefix in commit `rev`; `None` without one (or with
    /// a submodule there).
    fn subtree_of(&self, rev: &ObjectId) -> Result<Option<ObjectId>, Die> {
        let commit = self.read_commit(rev)?;
        let Ok(mut tree) = commit.tree() else {
            return Ok(None);
        };
        let parts: Vec<&str> = self.st.dir.split('/').filter(|p| !p.is_empty()).collect();
        for (i, part) in parts.iter().enumerate() {
            let Some(obj) = self.repo.odb().read(&tree).ok().flatten() else {
                return Ok(None);
            };
            let mut found = None;
            for entry in rgit_object::TreeIter::new(self.repo.hash, &obj.data) {
                let Ok(entry) = entry else { break };
                if entry.name == part.as_bytes() {
                    found = Some((entry.mode_bits & 0o170000, entry.oid));
                    break;
                }
            }
            let Some((kind, oid)) = found else {
                return Ok(None);
            };
            let last = i + 1 == parts.len();
            match kind {
                0o040000 => tree = oid,
                0o160000 if last => return Ok(None),
                _ if last => {
                    return Err(die("tree entry is of type blob, expected tree or commit"));
                }
                _ => return Ok(None),
            }
        }
        Ok(Some(tree))
    }

    /// The counterpart of `rev`: a rewritten parent with the same tree
    /// when that loses no history, else a new commit.
    fn copy_or_skip(
        &mut self,
        rev: &ObjectId,
        tree: &ObjectId,
        newparents: &[ObjectId],
        depth: usize,
    ) -> Result<ObjectId, Die> {
        let mut identical: Option<ObjectId> = None;
        let mut nonidentical: Option<ObjectId> = None;
        let mut keep = false;
        let mut parents: Vec<ObjectId> = Vec::new();
        for parent in newparents {
            let Some(ptree) = self.tree_of(parent) else {
                continue;
            };
            if ptree == *tree {
                match identical {
                    Some(prev) => {
                        // Two parents with our tree: use the newer one, keep the
                        // merge when they have unrelated histories.
                        let base = self.merge_base(&prev, parent)?;
                        if base == Some(prev) {
                            identical = Some(*parent);
                        } else if base != Some(*parent) {
                            keep = true;
                        }
                    }
                    None => identical = Some(*parent),
                }
            } else {
                nonidentical = Some(*parent);
            }
            if !parents.contains(parent) {
                parents.push(*parent);
            }
        }
        if let (Some(same), Some(other)) = (identical, nonidentical)
            && self.count_between(&same, &other)? != 0
        {
            // The other side has history the identical one lacks.
            keep = true;
        }
        match identical {
            Some(same) if !keep => Ok(same),
            _ => self.copy_commit(rev, tree, &parents, depth),
        }
    }

    fn merge_base(&self, a: &ObjectId, b: &ObjectId) -> Result<Option<ObjectId>, Die> {
        let (code, out) = self
            .st
            .capture(&["merge-base", &a.to_hex(), &b.to_hex()], None, true)?;
        if code != 0 {
            return Ok(None);
        }
        Ok(out
            .lines()
            .next()
            .and_then(|l| ObjectId::from_hex_any(l.trim()).ok()))
    }

    /// `rev-list --count <a>..<b>`.
    fn count_between(&self, a: &ObjectId, b: &ObjectId) -> Result<u64, Die> {
        let range = format!("{a}..{b}");
        let (_, out) = self
            .st
            .capture(&["rev-list", "--count", &range], None, false)?;
        Ok(String::from_utf8_lossy(&out).trim().parse().unwrap_or(0))
    }

    /// A commit like `rev` with `tree` and `parents`.
    fn copy_commit(
        &self,
        rev: &ObjectId,
        tree: &ObjectId,
        parents: &[ObjectId],
        depth: usize,
    ) -> Result<ObjectId, Die> {
        let parent_args: String = parents.iter().map(|p| format!(" -p {p}")).collect();
        self.debug(
            depth,
            &format!("  copy_commit {{{rev}}} {{{tree}}} {{{parent_args}}}"),
        );
        let orig = self.read_commit(rev)?;
        let author = identity(orig.author_raw())?;
        let committer = identity(orig.committer_raw())?;
        let mut message: Vec<u8> = Vec::new();
        if let Some(a) = &self.st.opts.annotate {
            message.extend_from_slice(a.as_bytes());
        }
        let body = orig.message();
        match crate::history::recode::reencode(body, orig.encoding(), b"UTF-8") {
            Some(utf8) => message.extend_from_slice(&utf8),
            None => message.extend_from_slice(body),
        }
        let new = rgit_commit::NewCommit {
            tree: *tree,
            parents,
            author: &author,
            committer: &committer,
            encoding: None,
            message: &message,
        };
        let mut commit = new.to_commit();
        if let Some(Some(key)) = &self.st.opts.gpg_sign {
            let sign = crate::signing::Sign::from_arg(Some(key));
            match crate::signing::sign_commit(
                &self.repo.config,
                self.repo.hash,
                &mut commit,
                &sign,
                &committer,
            ) {
                Ok(()) => {}
                Err(crate::signing::SignError::Die(d)) => return Err(d),
                Err(crate::signing::SignError::Failed) => return Err(Die::Exit(1)),
            }
        }
        self.repo
            .odb()
            .write(ObjectKind::Commit, &commit.serialize())
            .map_err(|e| Die::Fatal(e.to_string()))
    }
}

/// Seed the mapping; like git, a commit seeded twice (two joins naming
/// the same commit) is an error.
fn cache_set(
    cache: &mut HashMap<ObjectId, ObjectId>,
    from: ObjectId,
    to: ObjectId,
) -> Result<(), Die> {
    if cache.contains_key(&from) {
        return Err(die(&msg::cache_exists(&from.to_hex())));
    }
    cache.insert(from, to);
    Ok(())
}

/// An identity copied from a commit header, cleaned up the way it would
/// be when handed to `commit-tree` through the environment.
fn identity(raw: Option<&[u8]>) -> Result<Signature, Die> {
    let bad = || Die::Fatal("malformed identity in commit".into());
    let raw = raw.ok_or_else(bad)?;
    let sig = SignatureRef::parse(raw).ok_or_else(bad)?;
    let time = sig.time().ok_or_else(bad)?;
    Ok(Signature {
        name: rgit_ident::ident::sanitize(sig.name).to_vec(),
        email: rgit_ident::ident::sanitize(sig.email).to_vec(),
        time,
    })
}

fn join(ids: &[ObjectId]) -> String {
    ids.iter()
        .map(|i| i.to_hex())
        .collect::<Vec<_>>()
        .join(" ")
}
