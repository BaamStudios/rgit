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
            // `false` reads as `never` here (git v2.56.1); other boolean
            // spellings are invalid.
            let parsed = if value == "false" {
                Ok(0)
            } else {
                rgit_date::parse_expiry_date(&value, date)
            };
            let t = parsed.map_err(|_| {
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
    /// `--updateref`: when the newest entry goes, the reference is set to
    /// the new value of the entry that is now the newest (not for
    /// symbolic references, nor when no entry is left).
    pub updateref: bool,
}

/// Which entries go.
#[derive(Clone, Copy, Debug)]
pub enum Rule {
    /// By age and reachability (`reflog expire`).
    Age(Cutoffs),
    /// Exactly the entry `<ref>@{<n>}` (`reflog delete`), counted from the
    /// newest one.
    Nth(usize),
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
    prune(repo, common_dir, log, Rule::Age(cut), o, None)
}

/// `--stale-fix`: finds commits that are not reachable from any reference
/// and lead, through their parents and trees, to a missing object.
pub struct StaleFix {
    reachable: HashSet<ObjectId>,
    /// Verdicts so far: `true` for a broken commit or an incomplete tree.
    known: std::collections::HashMap<ObjectId, bool>,
}

impl StaleFix {
    /// Marks everything reachable from the references and `HEAD` of `repo`.
    pub fn new(repo: &Repo) -> StaleFix {
        let mut todo: Vec<ObjectId> = repo
            .refs
            .list("refs/")
            .into_iter()
            .map(|(_, id)| id)
            .collect();
        todo.extend(repo.refs.resolve("HEAD"));
        let mut reachable = HashSet::new();
        while let Some(id) = todo.pop() {
            if !reachable.insert(id) {
                continue;
            }
            match repo.read_object(&id) {
                Some(Object::Commit(c)) => todo.extend(c.parents().unwrap_or_default()),
                Some(Object::Tag(t)) => todo.extend(t.object().ok()),
                _ => {}
            }
        }
        StaleFix {
            reachable,
            known: Default::default(),
        }
    }

    /// Whether `id` is a broken commit: not reachable, and a commit, tree
    /// or blob it refers to is missing.
    pub fn broken(&mut self, repo: &Repo, id: &ObjectId) -> bool {
        if id.is_null() || self.reachable.contains(id) {
            return false;
        }
        if let Some(&b) = self.known.get(id) {
            return b;
        }
        // Depth-first over the unreachable commits; a commit is decided
        // once all its parents are.
        let mut stack = vec![(*id, false)];
        while let Some((c, expanded)) = stack.pop() {
            if self.known.contains_key(&c) {
                continue;
            }
            let Some(Object::Commit(commit)) = repo.read_object(&c) else {
                self.known.insert(c, true);
                continue;
            };
            let parents: Vec<ObjectId> = commit
                .parents()
                .unwrap_or_default()
                .into_iter()
                .filter(|p| !self.reachable.contains(p))
                .collect();
            if !expanded {
                stack.push((c, true));
                stack.extend(
                    parents
                        .iter()
                        .filter(|p| !self.known.contains_key(*p))
                        .map(|p| (*p, false)),
                );
                continue;
            }
            let bad = parents
                .iter()
                .any(|p| self.known.get(p).copied().unwrap_or(false))
                || commit.tree().map_or(true, |t| self.tree_broken(repo, &t));
            self.known.insert(c, bad);
        }
        self.known.get(id).copied().unwrap_or(true)
    }

    fn tree_broken(&mut self, repo: &Repo, id: &ObjectId) -> bool {
        if let Some(&b) = self.known.get(id) {
            return b;
        }
        let bad = match repo.read_object(id) {
            Some(Object::Tree(tree)) => tree.entries.iter().any(|e| {
                if e.mode.is_tree() {
                    self.tree_broken(repo, &e.oid)
                } else if e.mode == rgit_object::EntryMode::COMMIT {
                    false
                } else {
                    !repo.odb().contains(&e.oid)
                }
            }),
            _ => true,
        };
        self.known.insert(*id, bad);
        bad
    }
}

/// Prunes the entries of the reflog `log.name` (worktree `log.git_dir`)
/// that `rule` selects, and with `stale` those that point to broken
/// commits. Nothing happens if the reference has no log or the log is
/// locked.
pub fn prune(
    repo: &Repo,
    common_dir: &Path,
    log: &Log,
    rule: Rule,
    o: Options,
    mut stale: Option<&mut StaleFix>,
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
    let count = entries.len();
    for (i, e) in entries.iter().enumerate() {
        let mut e = e.clone();
        if o.rewrite && after_pruned {
            e.old = kept.last().map_or_else(|| kind.null(), |p| p.new);
        }
        let mut prune = match rule {
            Rule::Age(cut) => {
                e.time < cut.total
                    || (e.time < cut.unreachable && (reach.misses(&e.old) || reach.misses(&e.new)))
            }
            Rule::Nth(n) => count - 1 - i == n,
        };
        if !prune {
            if let Some(s) = stale.as_deref_mut() {
                prune = s.broken(repo, &e.old) || s.broken(repo, &e.new);
            }
        }
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
    let opts = rgit_fs::LockOptions::default()
        .shared(crate::refs_support::shared_repository(&repo.config));
    let Ok(mut lock) = rgit_fs::LockFile::acquire(&path, &opts) else {
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
    if o.updateref && after_pruned {
        if let Some(top) = kept.last() {
            update_ref(repo, log, top.new);
        }
    }
    Ok(())
}

/// `--updateref`: points the reference of `log` at `new` without a reflog
/// entry; symbolic references are left alone.
fn update_ref(repo: &Repo, log: &Log, new: ObjectId) {
    let store =
        FilesStore::new(repo.setup.absolute_common_dir(), repo.hash).with_worktree(&log.git_dir);
    if !matches!(store.read_raw(&log.name), Ok(Some(Target::Object(_)))) {
        return;
    }
    let objects = crate::refs_support::Objects::open(&repo.setup);
    let mut hooks = crate::refs_support::UpdateHooks::new(&repo.setup, &repo.config, &objects)
        .without_reflogs();
    let mut tx = rgit_refs::Transaction::new(&store);
    let update = rgit_refs::RefUpdate::update(log.name.as_str(), new, None).no_deref(true);
    if tx.push(update).is_ok() {
        let _ = tx.commit(&mut hooks);
    }
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
