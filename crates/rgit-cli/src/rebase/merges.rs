// SPDX-License-Identifier: MIT
//! The todo list of `--rebase-merges` (Documentation/git-rebase.adoc,
//! "REBASING MERGES", `rebase.rebaseMerges`, `rebase.maxLabelLength`).
//!
//! The commits are split into first-parent chains, oldest commit first:
//! a commit continues the chain of its first parent unless an older
//! commit already did (then the parent gets a `branch-point` label and
//! the commit starts a chain of its own). Each chain is written as
//! `reset <base>`, its `pick`s and `merge`s and a closing `label`; the
//! chain ending at `HEAD` comes last. The second and further parents of
//! a merge are named by labels derived from a local branch pointing at
//! them or from the merge's subject. Commits whose base lies outside the
//! rebased range keep it unless `rebase-cousins` is asked for.
//!
//! The order of the chains, the label names and the comments were taken
//! from the behaviour of the `git` binary on many random branch shapes.

use std::collections::{HashMap, HashSet};

use rgit_hash::ObjectId;
use rgit_object::Object;

use super::todo::{Cmd, Item, Line, Todo};
use super::{Repo, lossy, merge_bases, walk};
use crate::prelude::*;

/// The default of `rebase.maxLabelLength` (observed).
const MAX_LABEL_LENGTH: usize = 234;

/// What the list is made of.
pub(crate) struct Params {
    /// The commits not to rebase start here (`^restrict`).
    pub restrict: Option<ObjectId>,
    /// Commits already upstream are dropped.
    pub upstream: Option<ObjectId>,
    pub head: ObjectId,
    pub reapply: bool,
    pub keep_empty: bool,
    pub quiet: bool,
    /// `--rebase-merges=rebase-cousins`.
    pub cousins: bool,
}

/// A commit of the range.
struct Node {
    parents: Vec<ObjectId>,
    subject: String,
}

/// Label names in use.
struct Labels {
    used: HashSet<String>,
    max: usize,
    hex_len: usize,
}

impl Labels {
    /// A label for `wanted` (sanitized, truncated and made unique).
    fn make(&mut self, wanted: &str, fallback: impl FnOnce() -> String) -> String {
        let mut name = truncate(&sanitize(wanted), self.max);
        if name.is_empty() {
            name = fallback();
        }
        let taken = |used: &HashSet<String>, n: &str| {
            used.contains(&n.to_ascii_lowercase())
                || (n.len() == self.hex_len && n.bytes().all(|b| b.is_ascii_hexdigit()))
        };
        let mut unique = name.clone();
        let mut k = 2;
        while taken(&self.used, &unique) {
            unique = format!("{name}-{k}");
            k += 1;
        }
        self.used.insert(unique.to_ascii_lowercase());
        unique
    }
}

/// Characters other than ASCII letters and digits become `-` (one for a
/// run of them, none at the start); other bytes are kept.
fn sanitize(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() || !ch.is_ascii() {
            out.push(ch);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out
}

/// Cuts `name` to `max` (`rebase.maxLabelLength`): ASCII characters
/// must end before the last byte, others may use it (observed).
fn truncate(name: &str, max: usize) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        let n = ch.len_utf8();
        let fits = if ch.is_ascii() {
            out.len() + n < max
        } else {
            out.len() + n <= max
        };
        if !fits {
            break;
        }
        out.push(ch);
    }
    out
}

/// The name a merge's subject gives to the branch it merged: the first
/// quoted name after `Merge `, the source of a pull request, or the
/// whole subject.
fn name_from_subject(subject: &str) -> &str {
    if let Some(rest) = subject.strip_prefix("Merge ") {
        if let Some(q) = rest.find('\'')
            && let Some(len) = rest[q + 1..].find('\'')
        {
            return &rest[q + 1..q + 1 + len];
        }
        if rest.starts_with("pull request ")
            && let Some(at) = rest.find(" from ")
        {
            return &rest[at + " from ".len()..];
        }
    }
    subject
}

/// The todo list of `--rebase-merges`.
pub(crate) fn todo_list(ctx: &Context, repo: &Repo, p: &Params) -> Result<Todo, Die> {
    let mut args = vec!["--topo-order".to_owned()];
    if let Some(r) = p.restrict {
        args.push(format!("^{}", r.to_hex()));
    }
    args.push(p.head.to_hex());
    let commits = walk(ctx, &args)?;
    let skipped = if p.reapply {
        HashSet::new()
    } else {
        super::upstream_equivalents(ctx, repo, &commits, p.upstream, p.head)?
    };
    let mut nodes: HashMap<ObjectId, Node> = HashMap::new();
    let mut dropped: HashSet<ObjectId> = HashSet::new();
    let mut warned = false;
    for c in &commits {
        let Some(Object::Commit(commit)) = repo.read_object(c) else {
            return Err(Die::Fatal(crate::messages::sequencer::could_not_parse(
                &c.to_hex(),
            )));
        };
        let parents = commit.parents().unwrap_or_default();
        if skipped.contains(c) {
            super::warn_skipped(repo, c, p.quiet);
            warned = true;
            dropped.insert(*c);
        } else if parents.len() <= 1 && !p.keep_empty && super::is_empty_commit(repo, &commit) {
            dropped.insert(*c);
        }
        let subject = lossy(&rgit_commit::message::subject(commit.message()));
        nodes.insert(*c, Node { parents, subject });
    }
    if warned {
        super::advise_skipped(ctx, repo)?;
    }
    // `rebase.instructionFormat` describes the commits (and names labels).
    let mut ids: Vec<ObjectId> = commits.clone();
    for c in &commits {
        for parent in &nodes[c].parents {
            if !nodes.contains_key(parent) && !ids.contains(parent) {
                ids.push(*parent);
            }
        }
    }
    let formatted = super::formatted_onelines(ctx, repo, &ids)?.unwrap_or_default();
    for (id, node) in nodes.iter_mut() {
        if let Some(line) = formatted.get(id) {
            node.subject = line.clone();
        }
    }
    let bases: HashSet<ObjectId> = match p.restrict {
        Some(r) => merge_bases(repo, &r, &p.head)?.into_iter().collect(),
        None => HashSet::new(),
    };

    // Local branches name the commits they point to.
    let mut branches: HashMap<ObjectId, String> = HashMap::new();
    let mut heads = repo.refs.list("refs/heads/");
    heads.sort();
    for (name, id) in heads {
        let short = name.strip_prefix("refs/heads/").unwrap_or(&name).to_owned();
        branches.insert(id, short);
    }

    let max = repo
        .config
        .int("rebase.maxlabellength")
        .ok()
        .flatten()
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(MAX_LABEL_LENGTH);
    let mut labels = Labels {
        // git does not keep `onto` free with `--root` (observed).
        used: p.restrict.map(|_| "onto".to_owned()).into_iter().collect(),
        max,
        hex_len: repo.hash.hex_len(),
    };
    let abbrev_len = repo.default_abbrev();

    // The tips: the merged branches, oldest merge first, and `HEAD`; each
    // merged branch is labelled by the first merge.
    let mut label_of: HashMap<ObjectId, String> = HashMap::new();
    let mut tips: Vec<Vec<ObjectId>> = Vec::new();
    for c in &commits {
        let node = &nodes[c];
        let mut group = Vec::new();
        for mp in node.parents.iter().skip(1) {
            if nodes.contains_key(mp) && !label_of.contains_key(mp) {
                let wanted = match branches.get(mp) {
                    Some(b) => b.clone(),
                    None => name_from_subject(&node.subject).to_owned(),
                };
                let fallback = || format!("rev-{}", repo.abbrev(mp, abbrev_len));
                let l = labels.make(&wanted, fallback);
                label_of.insert(*mp, l);
                group.push(*mp);
            }
        }
        if !group.is_empty() {
            tips.push(group);
        }
    }
    if nodes.contains_key(&p.head) {
        tips.push(vec![p.head]);
    }
    // Then the branch points: a second commit on top of the same parent.
    let mut has_child: HashSet<ObjectId> = HashSet::new();
    for c in &commits {
        if let Some(fp) = nodes[c].parents.first()
            && nodes.contains_key(fp)
            && !has_child.insert(*fp)
            && !label_of.contains_key(fp)
        {
            let l = labels.make("branch-point", String::new);
            label_of.insert(*fp, l);
        }
    }

    // The chains: from each tip along the first parents, the oldest
    // merged branch first.
    let mut assigned: HashSet<ObjectId> = HashSet::new();
    let mut chains: Vec<Vec<ObjectId>> = Vec::new();
    for tip in tips.iter().flatten() {
        if assigned.contains(tip) {
            continue;
        }
        let mut ch = Vec::new();
        let mut x = Some(*tip);
        while let Some(c) = x {
            if !nodes.contains_key(&c) || assigned.contains(&c) {
                break;
            }
            assigned.insert(c);
            ch.push(c);
            x = nodes[&c].parents.first().copied();
        }
        if let Some(fp) = x
            && nodes.contains_key(&fp)
            && !label_of.contains_key(&fp)
        {
            let l = labels.make("branch-point", String::new);
            label_of.insert(fp, l);
        }
        ch.reverse();
        chains.push(ch);
    }

    let mut lines = vec![Line::Cmd(Item::new(Cmd::Label, "onto"))];
    if !chains.is_empty() {
        lines.push(Line::Comment(String::new()));
    }
    let describe = |id: &ObjectId| format!("# {}", nodes[id].subject);
    for ch in &chains {
        let tip = *ch.last().expect("not empty");
        let is_head = tip == p.head;
        if !is_head {
            let l = label_of.get(&tip).cloned().unwrap_or_default();
            lines.push(Line::Comment(format!("# Branch {l}")));
        }
        // Where the chain starts.
        let root = ch[0];
        let base = match nodes[&root].parents.first() {
            // A label named `onto` (`--root`) gets no description (observed).
            Some(fp) if nodes.contains_key(fp) && label_of[fp] == "onto" => "onto".to_owned(),
            Some(fp) if nodes.contains_key(fp) => {
                format!("{} {}", label_of[fp], describe(fp))
            }
            Some(fp) if p.cousins || bases.contains(fp) => "onto".to_owned(),
            Some(fp) => {
                let subject = match repo.read_object(fp) {
                    _ if formatted.contains_key(fp) => formatted[fp].clone(),
                    Some(Object::Commit(c)) => lossy(&rgit_commit::message::subject(c.message())),
                    _ => String::new(),
                };
                format!("{} # {subject}", repo.abbrev(fp, abbrev_len))
            }
            None if p.cousins => "onto".to_owned(),
            None => "[new root]".to_owned(),
        };
        lines.push(Line::Cmd(Item::new(Cmd::Reset, base)));
        for c in ch {
            let node = &nodes[c];
            if node.parents.len() > 1 {
                let names: Vec<String> = node.parents[1..]
                    .iter()
                    .map(|mp| match label_of.get(mp) {
                        Some(l) => l.clone(),
                        None if p.cousins || bases.contains(mp) => "onto".to_owned(),
                        None => repo.abbrev(mp, abbrev_len),
                    })
                    .collect();
                lines.push(Line::Cmd(Item {
                    cmd: Cmd::Merge,
                    flag: Some('C'),
                    commit: Some(*c),
                    arg: format!("{} {}", names.join(" "), describe(c)),
                }));
            } else if !dropped.contains(c) {
                let Some(Object::Commit(commit)) = repo.read_object(c) else {
                    continue;
                };
                lines.push(Line::Cmd(Item::pick(
                    *c,
                    super::pick_description(repo, &commit)?,
                )));
            }
            if *c != tip
                && let Some(l) = label_of.get(c)
            {
                lines.push(Line::Cmd(Item::new(Cmd::Label, l.clone())));
            }
        }
        if !is_head {
            let l = label_of.get(&tip).cloned().unwrap_or_default();
            lines.push(Line::Cmd(Item::new(Cmd::Label, l)));
            lines.push(Line::Comment(String::new()));
        }
    }
    Ok(Todo { lines })
}
