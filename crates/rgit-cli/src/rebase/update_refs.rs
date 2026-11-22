// SPDX-License-Identifier: MIT
//! `--update-refs` (Documentation/git-rebase.adoc, `rebase.updateRefs`):
//! the `update-ref` commands added after the commits that branches point
//! to, and the state file `rebase-merge/update-refs`.
//!
//! The file holds three lines per ref, sorted by name: the ref, its value
//! when the rebase started (the null id when it did not exist) and the
//! commit its `update-ref` command recorded (the null id until then). It
//! follows the edits of the todo list: refs whose `update-ref` command
//! was removed are dropped (and no longer count as checked out), new
//! ones are added. The layout and the messages were taken from the
//! behaviour of the `git` binary.

use std::collections::{BTreeMap, HashMap};

use rgit_hash::ObjectId;
use rgit_refs::RefUpdate;

use super::todo::{self, Cmd, Item, Line};
use super::{Repo, msg, ref_tx, state_dir, write};
use crate::prelude::*;

const FILE: &str = "update-refs";

/// One ref of the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Record {
    /// The value when the rebase started (`None`: the ref did not exist).
    pub before: Option<ObjectId>,
    /// The commit recorded by `update-ref` (`None`: not reached yet).
    pub after: Option<ObjectId>,
}

/// Adds `update-ref` commands after each commit of `todo` that local
/// branches point to; a branch checked out in a worktree gets a comment
/// instead. The branch being rebased (`head_name`) is left out, unless
/// its commit also has a branch checked out in another worktree; then it
/// gets the comment too (observed with the git binary).
pub(crate) fn add_commands(repo: &Repo, todo: &mut todo::Todo, head_name: &str, comment: &str) {
    let worktrees = crate::worktrees::list(
        &repo.setup,
        crate::worktrees::main_is_bare(&repo.setup, &repo.config),
    );
    let mut at: HashMap<ObjectId, Vec<String>> = HashMap::new();
    for (name, id) in repo.refs.list("refs/heads/") {
        at.entry(id).or_default().push(name);
    }
    let lines = std::mem::take(&mut todo.lines);
    for line in lines {
        let commit = line
            .item()
            .filter(|i| i.cmd.picks() || i.cmd == Cmd::Drop)
            .and_then(|i| i.commit);
        todo.lines.push(line);
        let Some(names) = commit.and_then(|c| at.get(&c)) else {
            continue;
        };
        let mut names = names.clone();
        names.sort();
        let used: Vec<Option<String>> = names
            .iter()
            .map(|n| {
                crate::worktrees::checked_out(&worktrees, n, true)
                    .map(|w| w.path.display().to_string())
            })
            .collect();
        let elsewhere = names
            .iter()
            .zip(&used)
            .any(|(n, u)| n != head_name && u.is_some());
        // git lists the branches of a commit in reverse order.
        for (name, used) in names.iter().zip(&used).rev() {
            if name == head_name && !elsewhere {
                continue;
            }
            match used {
                Some(path) => todo.lines.push(Line::Comment(format!(
                    "{comment} {}",
                    msg::ref_checked_out_at(name, path)
                ))),
                None => todo
                    .lines
                    .push(Line::Cmd(Item::new(Cmd::UpdateRef, name.clone()))),
            }
            todo.lines.push(Line::Comment(String::new()));
        }
    }
}

/// The records of the file.
pub(crate) fn read(repo: &Repo) -> BTreeMap<String, Record> {
    let text = std::fs::read_to_string(state_dir(repo).join(FILE)).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let parse = |s: &str| {
        ObjectId::from_hex(repo.hash, s.trim().as_bytes())
            .ok()
            .filter(|id| !id.is_null())
    };
    let mut out = BTreeMap::new();
    for chunk in lines.chunks(3) {
        if let [name, before, after] = chunk {
            out.insert(
                (*name).to_owned(),
                Record {
                    before: parse(before),
                    after: parse(after),
                },
            );
        }
    }
    out
}

fn write_records(repo: &Repo, records: &BTreeMap<String, Record>) -> Result<(), Die> {
    let path = state_dir(repo).join(FILE);
    if records.is_empty() {
        let _ = std::fs::remove_file(path);
        return Ok(());
    }
    let null = ObjectId::null(repo.hash);
    let mut text = String::new();
    for (name, r) in records {
        text.push_str(&format!(
            "{name}\n{}\n{}\n",
            r.before.unwrap_or(null).to_hex(),
            r.after.unwrap_or(null).to_hex()
        ));
    }
    write(&path, text)
}

/// The refs of the `update-ref` commands in `text` (a todo or done list).
fn names_in(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let (word, rest) = l.split_once([' ', '\t'])?;
            matches!(word, "update-ref" | "u").then(|| rest.trim().to_owned())
        })
        .filter(|n| !n.is_empty())
        .collect()
}

/// Makes the file follow the `update-ref` commands of the done list and
/// `todo`: removed refs are dropped, new ones recorded with their value.
pub(crate) fn sync(repo: &Repo, todo: &todo::Todo) -> Result<(), Die> {
    let done = std::fs::read_to_string(state_dir(repo).join("done")).unwrap_or_default();
    let mut wanted = names_in(&done);
    wanted.extend(
        todo.items()
            .filter(|i| i.cmd == Cmd::UpdateRef)
            .map(|i| i.arg.clone()),
    );
    let old = read(repo);
    if wanted.is_empty() && old.is_empty() {
        return Ok(());
    }
    let mut records = BTreeMap::new();
    for name in wanted {
        if records.contains_key(&name) {
            continue;
        }
        let r = old.get(&name).cloned().unwrap_or_else(|| Record {
            before: repo.refs.resolve(&name),
            after: None,
        });
        records.insert(name, r);
    }
    write_records(repo, &records)
}

/// `update-ref <ref>`: records `head` for `name`.
pub(crate) fn record(repo: &Repo, name: &str, head: ObjectId) -> Result<(), Die> {
    let mut records = read(repo);
    records
        .entry(name.to_owned())
        .or_insert_with(|| Record {
            before: repo.refs.resolve(name),
            after: None,
        })
        .after = Some(head);
    write_records(repo, &records)
}

/// What [`apply`] did: the refs it moved, the ones it could not move, and
/// the error lines.
#[derive(Debug, Default)]
pub(crate) struct Applied {
    updated: Vec<String>,
    failed: Vec<String>,
    errors: Vec<String>,
}

impl Applied {
    pub(crate) fn ok(&self) -> bool {
        self.failed.is_empty()
    }

    /// Prints the errors and the lists of refs.
    pub(crate) fn report(&self) {
        for e in &self.errors {
            eprintln!("error: {e}");
        }
        if !self.updated.is_empty() {
            eprintln!("{}", msg::UPDATED_REFS);
            for n in &self.updated {
                eprintln!("\t{n}");
            }
        }
        if !self.failed.is_empty() {
            eprintln!("{}", msg::FAILED_REFS);
            for n in &self.failed {
                eprintln!("\t{n}");
            }
        }
    }
}

/// At the end of the rebase: moves the recorded refs (from the value they
/// had when the rebase started).
pub(crate) fn apply(ctx: &Context, repo: &Repo, records: &BTreeMap<String, Record>) -> Applied {
    let mut out = Applied::default();
    for (name, r) in records {
        let Some(new) = r.after else {
            continue;
        };
        let update = match r.before {
            Some(old) => RefUpdate::update(name.as_str(), new, Some(old)),
            None => RefUpdate::create(name.as_str(), new),
        };
        match ref_tx(
            ctx,
            repo,
            update.message(msg::REWRITTEN_DURING_REBASE),
            false,
        ) {
            Ok(()) => out.updated.push(name.clone()),
            Err(e) => {
                let why = match e {
                    Die::Fatal(m) | Die::Error(m, _) => m,
                    _ => String::new(),
                };
                out.errors.push(msg::update_ref_failed(name, &why));
                out.failed.push(name.clone());
            }
        }
    }
    out
}
