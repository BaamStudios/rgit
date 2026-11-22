// SPDX-License-Identifier: MIT
//! Pruning reflogs: `git reflog expire` (Documentation/git-reflog.adoc,
//! `gc.reflogExpire*` in Documentation/config/gc.adoc), shared by the
//! command and by automatic maintenance ([`crate::maintenance`]).
//!
//! An entry is pruned when it is older than the `expire` limit, or older
//! than the `expire-unreachable` limit and not reachable from the current
//! value of its reference. Behaviour of git v2.56.1 the documentation does
//! not spell out (determined with the reference binary):
//!
//! - Without options or configuration, entries older than 30 days are
//!   pruned, reachable or not, and the unreachable limit is 90 days.
//! - An entry is unreachable when its old or its new commit is not
//!   reachable from the reference; for `HEAD` from any reference. The
//!   entries are judged in order, with `--rewrite` after the old value
//!   was replaced.
//! - The first `gc.<pattern>.*` (in configuration order) whose pattern
//!   matches decides both limits, an unset one being `never`; the plain
//!   `gc.reflogExpire*` and the defaults do not mix in.
//! - `refs/stash` is pruned only by `--expire`, `--expire-unreachable` and
//!   `gc.<pattern>.reflogExpire*` settings matching it, never by the plain
//!   `gc.reflogExpire*` or the defaults.
//! - `--dry-run --verbose` says "would prune" for the logs of `--all`,
//!   "prune" for the ones named on the command line.
//! - The log is rewritten only when an entry goes; a log whose entries all
//!   go stays as an empty file.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use bstr::ByteSlice;
use rgit_config::ConfigSet;
use rgit_date::DateContext;
use rgit_hash::{HashKind, ObjectId};
use rgit_object::Object;
use rgit_pathspec::wildmatch::{WildmatchFlags, wildmatch};
use rgit_refs::files::FilesStore;
use rgit_refs::name::is_per_worktree;
use rgit_refs::reflog::{Entry, FilesReflog};
use rgit_refs::{RefStore, Target};

use crate::messages::config as cmsg;
use crate::prelude::*;
use crate::repository::Repo;

const DAY: u64 = 24 * 60 * 60;
const STASH: &str = "refs/stash";

/// The limits for one reflog as timestamps: entries older than the value
/// qualify. `0` prunes nothing (`never`), `u64::MAX` everything (`now`,
/// `all`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cutoffs {
    /// Entries older than this go.
    pub total: u64,
    /// Unreachable entries older than this go.
    pub unreachable: u64,
}

/// The settings of one `gc.<pattern>.*`; an unset limit is `never`.
struct Pattern {
    pattern: Vec<u8>,
    total: Option<u64>,
    unreachable: Option<u64>,
}

/// Limits given on the command line (`--expire`, `--expire-unreachable`);
/// they win over the configuration for every reference.
#[derive(Clone, Copy, Debug, Default)]
pub struct Explicit {
    pub total: Option<u64>,
    pub unreachable: Option<u64>,
}

/// The `gc.reflogExpire*` settings, read once.
pub struct Config {
    now: u64,
    explicit: Explicit,
    /// `gc.reflogExpire`, `gc.reflogExpireUnreachable`.
    total: Option<u64>,
    unreachable: Option<u64>,
    /// `gc.<pattern>.reflogExpire` and `...Unreachable` per pattern, in
    /// the order the patterns first appear.
    patterns: Vec<Pattern>,
}

impl Config {
    /// Reads the settings; an unparsable value ends the command like git
    /// (`error: ... is not a valid timestamp` and the source).
    pub fn load(config: &ConfigSet, explicit: Explicit, date: &DateContext) -> Result<Config, Die> {
        let mut c = Config {
            now: u64::try_from(date.now).unwrap_or(0),
            explicit,
            total: None,
            unreachable: None,
            patterns: Vec::new(),
        };
        for e in config.entries() {
            let name = e.name.as_bytes();
            let Some(rest) = name.strip_prefix(b"gc.") else {
                continue;
            };
            let (sub, unreachable) = if let Some(s) = rest.strip_suffix(b"reflogexpireunreachable")
            {
                (s, true)
            } else if let Some(s) = rest.strip_suffix(b"reflogexpire") {
                (s, false)
            } else {
                continue;
            };
            if !sub.is_empty() && !sub.ends_with(b".") {
                continue;
            }
            let value = e
                .value
                .as_ref()
                .map(|v| v.to_str_lossy().into_owned())
                .unwrap_or_default();
            let t = rgit_date::parse_expiry_date(&value, date).map_err(|_| {
                crate::comment::bad_entry(
                    e,
                    &cmsg::invalid_timestamp(&value, &e.name.to_str_lossy()),
                )
            })?;
            let slot = if sub.is_empty() {
                if unreachable {
                    &mut c.unreachable
                } else {
                    &mut c.total
                }
            } else {
                let pattern = &sub[..sub.len() - 1];
                let i = match c.patterns.iter().position(|p| p.pattern == pattern) {
                    Some(i) => i,
                    None => {
                        c.patterns.push(Pattern {
                            pattern: pattern.to_vec(),
                            total: None,
                            unreachable: None,
                        });
                        c.patterns.len() - 1
                    }
                };
                let p = &mut c.patterns[i];
                if unreachable {
                    &mut p.unreachable
                } else {
                    &mut p.total
                }
            };
            *slot = Some(t);
        }
        Ok(c)
    }

    /// The limits for the reflog of `refname`: those of the first pattern
    /// that matches it, else the plain settings.
    pub fn cutoffs(&self, refname: &str) -> Cutoffs {
        let pattern = self
            .patterns
            .iter()
            .find(|p| wildmatch(&p.pattern, refname.as_bytes(), WildmatchFlags::NONE));
        let (total, unreachable) = match pattern {
            Some(p) => (p.total.unwrap_or(0), p.unreachable.unwrap_or(0)),
            None if refname == STASH => (0, 0),
            None => (
                self.total
                    .unwrap_or_else(|| self.now.saturating_sub(30 * DAY)),
                self.unreachable
                    .unwrap_or_else(|| self.now.saturating_sub(90 * DAY)),
            ),
        };
        Cutoffs {
            total: self.explicit.total.unwrap_or(total),
            unreachable: self.explicit.unreachable.unwrap_or(unreachable),
        }
    }
}

/// How to expire.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// `-n`/`--dry-run`: only report.
    pub dry_run: bool,
    /// `--verbose`: `keep`/`prune` per entry on standard output.
    pub verbose: bool,
    /// `--rewrite`: the old value of an entry after a pruned one becomes
    /// the new value of the entry now before it.
    pub rewrite: bool,
}

/// One reflog of some worktree.
pub struct Log {
    /// The git directory of the worktree the log belongs to.
    pub git_dir: PathBuf,
    /// The reference.
    pub name: String,
    /// Found by `--all` (not named on the command line).
    pub listed: bool,
}

/// The reflogs `--all` processes: those of the current worktree, then
/// (unless `single_worktree`) the per-worktree ones of the other worktrees.
pub fn all_logs(
    git_dir: &Path,
    common_dir: &Path,
    kind: HashKind,
    single_worktree: bool,
) -> Vec<Log> {
    let mut out = Vec::new();
    for name in FilesReflog::new(git_dir, common_dir, kind)
        .list()
        .unwrap_or_default()
    {
        out.push(Log {
            git_dir: git_dir.to_path_buf(),
            name,
            listed: true,
        });
    }
    if single_worktree {
        return out;
    }
    let mut others = vec![common_dir.to_path_buf()];
    others.extend(
        crate::worktrees::linked_ids(common_dir)
            .into_iter()
            .map(|id| common_dir.join("worktrees").join(id)),
    );
    for dir in others {
        if crate::worktrees::real(&dir) == crate::worktrees::real(git_dir) {
            continue;
        }
        for name in FilesReflog::new(&dir, common_dir, kind)
            .list()
            .unwrap_or_default()
        {
            if is_per_worktree(&name) {
                out.push(Log {
                    git_dir: dir.clone(),
                    name,
                    listed: true,
                });
            }
        }
    }
    out
}

/// The commit a reference points to in the worktree of `git_dir`.
pub fn tip(git_dir: &Path, common_dir: &Path, kind: HashKind, name: &str) -> Option<ObjectId> {
    let store = FilesStore::new(common_dir, kind).with_worktree(git_dir);
    let mut current = name.to_owned();
    for _ in 0..=rgit_refs::MAX_SYMREF_DEPTH {
        match store.read_raw(&current).ok().flatten()? {
            Target::Object(id) => return Some(id),
            Target::Symbolic(next) => current = next,
        }
    }
    None
}

/// The commits reachable from some tips, computed on first use.
struct Reachable<'a> {
    repo: &'a Repo,
    tips: Vec<ObjectId>,
    set: Option<HashSet<ObjectId>>,
}

impl Reachable<'_> {
    fn contains(&mut self, id: &ObjectId) -> bool {
        if self.set.is_none() {
            let mut set = HashSet::new();
            let mut todo = std::mem::take(&mut self.tips);
            while let Some(id) = todo.pop() {
                if !set.insert(id) {
                    continue;
                }
                match self.repo.read_object(&id) {
                    Some(Object::Commit(c)) => todo.extend(c.parents().unwrap_or_default()),
                    Some(Object::Tag(t)) => todo.extend(t.object().ok()),
                    _ => {}
                }
            }
            self.set = Some(set);
        }
        self.set.as_ref().is_some_and(|s| s.contains(id))
    }

    /// Whether `id` is set and not reachable.
    fn misses(&mut self, id: &ObjectId) -> bool {
        !id.is_null() && !self.contains(id)
    }
}

/// Prunes the reflog `log.name` of the worktree `log.git_dir`. Nothing
/// happens if the reference has no log or the log is locked.
pub fn expire(
    repo: &Repo,
    common_dir: &Path,
    log: &Log,
    cut: Cutoffs,
    o: Options,
) -> Result<(), Die> {
    let kind = repo.hash;
    let logs = FilesReflog::new(&log.git_dir, common_dir, kind);
    let Ok(Some(data)) = logs.read_raw(&log.name) else {
        return Ok(());
    };
    let entries = Entry::parse_all(&data, kind);
    let mut tips: Vec<ObjectId> = tip(&log.git_dir, common_dir, kind, &log.name)
        .into_iter()
        .collect();
    if log.name == "HEAD" {
        tips.extend(repo.refs.list("refs/").into_iter().map(|(_, id)| id));
    }
    let mut reach = Reachable {
        repo,
        tips,
        set: None,
    };
    let mut out = std::io::stdout().lock();
    let mut kept: Vec<Entry> = Vec::with_capacity(entries.len());
    let mut changed = false;
    let mut after_pruned = false;
    for e in &entries {
        let mut e = e.clone();
        if o.rewrite && after_pruned {
            e.old = kept.last().map_or_else(|| kind.null(), |p| p.new);
        }
        let prune = e.time < cut.total
            || (e.time < cut.unreachable && (reach.misses(&e.old) || reach.misses(&e.new)));
        if o.verbose {
            let what = match (prune, o.dry_run && log.listed) {
                (true, true) => crate::messages::reflog::WOULD_PRUNE,
                (true, false) => crate::messages::reflog::PRUNE,
                (false, _) => crate::messages::reflog::KEEP,
            };
            let mut line = format!("{what} ").into_bytes();
            line.extend_from_slice(&e.message);
            line.push(b'\n');
            let _ = out.write_all(&line);
        }
        if prune {
            changed = true;
            after_pruned = true;
            continue;
        }
        after_pruned = false;
        kept.push(e);
    }
    let _ = out.flush();
    if !changed || o.dry_run {
        return Ok(());
    }
    let path = logs.path(&log.name);
    let Ok(mut lock) = rgit_fs::LockFile::acquire(&path, &rgit_fs::LockOptions::default()) else {
        return Ok(());
    };
    let mut content = Vec::new();
    for e in &kept {
        content.extend_from_slice(&e.to_line());
    }
    if lock.file_mut().write_all(&content).is_err() {
        lock.rollback();
        return Ok(());
    }
    let _ = lock.commit();
    Ok(())
}

/// How many entries of the `HEAD` reflog of the worktree are old enough
/// to go, ignoring reachability (the trigger of the `reflog-expire`
/// maintenance task).
pub fn expired_head_entries(repo: &Repo, config: &Config) -> usize {
    let setup = &repo.setup;
    let logs = FilesReflog::new(
        setup.absolute_git_dir(),
        setup.absolute_common_dir(),
        repo.hash,
    );
    let cut = config.cutoffs("HEAD");
    let limit = cut.total.max(cut.unreachable);
    logs.read("HEAD")
        .ok()
        .flatten()
        .unwrap_or_default()
        .iter()
        .filter(|e| e.time < limit)
        .count()
}
