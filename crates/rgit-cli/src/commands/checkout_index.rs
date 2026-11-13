// SPDX-License-Identifier: MIT
//! `git checkout-index` (Documentation/git-checkout-index.adoc): copy files
//! from the index to the work tree. `-a`, `-f`, `-q`, `-n`, `-u`,
//! `--prefix`, `--stdin`, `-z`, `--ignore-skip-worktree-bits`; `--temp`
//! and `--stage` are not implemented yet.

use std::io::Read;
use std::path::{Path, PathBuf};

use rgit_fs::{LockFile, LockOptions};
use rgit_index::{Entry, Index, Stage, WriteOptions};
use rgit_unpack::messages as umsg;
use rgit_unpack::{CheckoutError, Filter, Identity};
use rgit_worktree::stat::{Checker, Status, compare, stat_from_metadata};

use crate::messages::checkout_index as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;

fn lossy(p: &[u8]) -> String {
    String::from_utf8_lossy(p).into_owned()
}

struct State<'a> {
    repo: &'a Repo,
    root: PathBuf,
    checker: Checker,
    force: bool,
    quiet: bool,
    no_create: bool,
    update: bool,
    prefix: Vec<u8>,
    errors: bool,
    wrote_files: bool,
    /// Index positions whose stat data changed.
    refreshed: Vec<(usize, rgit_index::Stat)>,
}

impl State<'_> {
    /// Writes the index entry at `pos` unless an up-to-date file is there.
    fn checkout(&mut self, index: &Index, pos: usize) -> Result<(), Die> {
        let entry = &index.entries()[pos];
        let mut target = self.prefix.clone();
        target.extend_from_slice(entry.path());
        let full = self.root.join(rgit_unpack_path(&target));
        if let Ok(meta) = std::fs::symlink_metadata(&full) {
            let changed = if self.prefix.is_empty() {
                !matches!(self.checker.check_with(entry, &meta), Ok(Status::Unchanged))
            } else {
                !compare(entry, &meta, self.checker.options()).is_empty()
            };
            if !changed {
                return Ok(());
            }
            if !self.force {
                if !self.quiet {
                    eprintln!("{}", umsg::already_exists(&lossy(&target)));
                }
                self.errors = true;
                return Ok(());
            }
        } else if self.no_create {
            return Ok(());
        }
        if let Err(e) = rgit_unpack::create_leading_dirs(&self.root, &target, self.force) {
            return Err(Die::Fatal(e.to_string()));
        }
        let data = if entry.mode.is_gitlink() {
            Vec::new()
        } else {
            let blob = rgit_unpack::read_blob(self.repo.odb(), &entry.id)
                .map_err(|e| Die::Error(e.to_string(), 128))?;
            if entry.mode.is_file() {
                self.wrote_files = true;
                Identity.to_worktree(entry.path(), blob)
            } else {
                blob
            }
        };
        let symlinks = self.checker.options().symlinks;
        match rgit_unpack::write_entry(&self.root, &target, entry, &data, symlinks) {
            Ok(Some(meta)) => {
                if self.update && self.prefix.is_empty() {
                    self.refreshed.push((pos, stat_from_metadata(&meta)));
                }
            }
            Ok(None) => {}
            Err(e @ CheckoutError::LeadingDirectory { .. }) => return Err(Die::Fatal(e.to_string())),
            Err(e) => {
                eprintln!("error: {e}");
                self.errors = true;
            }
        }
        Ok(())
    }

    /// `checkout-index <path>`: the stage-0 entry of `path`.
    fn checkout_path(&mut self, index: &Index, path: &[u8]) -> Result<(), Die> {
        let range = index.path_range(path);
        if range.is_empty() {
            if !self.quiet {
                eprintln!("{}", umsg::not_in_cache(&lossy(path)));
            }
            self.errors = true;
            return Ok(());
        }
        match index.position(path, Stage::MERGED) {
            Ok(pos) => self.checkout(index, pos),
            Err(_) => {
                if !self.quiet {
                    eprintln!("{}", umsg::unmerged(&lossy(path)));
                }
                self.errors = true;
                Ok(())
            }
        }
    }
}

/// `rgit_unpack` takes `/`-separated byte paths; this is the same
/// conversion for `Path` joins here.
fn rgit_unpack_path(rel: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(rel))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(rel).into_owned())
    }
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let all = Cell::new(false);
    let ignore_skip = Cell::new(false);
    let force = Cell::new(0);
    let quiet = Cell::new(0);
    let no_create = Cell::new(false);
    let update = Cell::new(false);
    let nul = Cell::new(false);
    let stdin = Cell::new(false);
    let temp = Cell::new(false);
    let prefix = RefCell::new(None::<String>);
    let stage = RefCell::new(None::<String>);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag('a', "all", &all, msg::HELP_ALL),
            Opt::flag(None, "ignore-skip-worktree-bits", &ignore_skip, msg::HELP_IGNORE_SKIP),
            Opt::force(&force, msg::HELP_FORCE),
            Opt::quiet(&quiet, msg::HELP_QUIET),
            Opt::callback('n', "no-create", None, msg::HELP_NO_CREATE, |_, _| {
                no_create.set(true);
                Ok(())
            })
            .noarg()
            .noneg(),
            Opt::callback(None, "create", None, msg::HELP_CREATE, |_, _| {
                no_create.set(false);
                Ok(())
            })
            .noarg()
            .noneg(),
            Opt::flag('u', "index", &update, msg::HELP_INDEX),
            Opt::flag('z', None, &nul, msg::HELP_Z),
            Opt::flag(None, "stdin", &stdin, msg::HELP_STDIN),
            Opt::flag(None, "temp", &temp, msg::HELP_TEMP),
            Opt::string(None, "prefix", &prefix, "string", msg::HELP_PREFIX),
            Opt::string(None, "stage", &stage, "(1|2|3|all)", msg::HELP_STAGE)
                .literal_argh()
                .noneg(),
        ],
    );
    let args = parser.parse(argv)?.args;
    drop(parser);
    if all.get() && !args.is_empty() {
        return Err(Die::Fatal(msg::MIX_ALL.into()));
    }
    if stdin.get() && !args.is_empty() {
        return Err(Die::Fatal(msg::MIX_STDIN.into()));
    }
    if temp.get() || stage.borrow().is_some() {
        return Err(Die::Fatal(msg::TEMP_UNSUPPORTED.into()));
    }

    let repo = Repo::open(ctx, Mode::Required)?;
    let root = repo.work_tree()?;
    let index_path = repo.setup.cwd.join(repo.index_path());
    let mut lock = if update.get() {
        Some(
            LockFile::acquire(&index_path, &LockOptions::default())
                .map_err(super::read_tree::lock_failed)?,
        )
    } else {
        None
    };
    let (mut index, mtime) = repo.read_index()?;
    let mut state = State {
        repo: &repo,
        checker: repo.checker(mtime)?,
        root: root.clone(),
        force: force.get() > 0,
        quiet: quiet.get() > 0,
        no_create: no_create.get(),
        update: update.get(),
        prefix: prefix.borrow().clone().unwrap_or_default().into_bytes(),
        errors: false,
        wrote_files: false,
        refreshed: Vec::new(),
    };

    let mut paths: Vec<Vec<u8>> = args.iter().map(|a| a.as_bytes().to_vec()).collect();
    if stdin.get() {
        let mut data = Vec::new();
        let _ = std::io::stdin().lock().read_to_end(&mut data);
        let sep = if nul.get() { 0 } else { b'\n' };
        for line in data.split(|&b| b == sep) {
            if line.is_empty() {
                continue;
            }
            let line = if !nul.get() && line.starts_with(b"\"") {
                rgit_pathspec::unquote_c_style(line).unwrap_or_else(|| line.to_vec())
            } else {
                line.to_vec()
            };
            paths.push(line);
        }
    }
    for arg in &paths {
        let path = repo.prefix_path(arg).map_err(|()| {
            Die::Fatal(crate::messages::update_index::outside_repository(
                &lossy(arg),
                &root_display(&repo),
            ))
        })?;
        state.checkout_path(&index, &path)?;
    }
    if all.get() {
        for pos in 0..index.len() {
            let e: &Entry = &index.entries()[pos];
            if e.stage() != Stage::MERGED || (e.flags.skip_worktree && !ignore_skip.get()) {
                continue;
            }
            state.checkout(&index, pos)?;
        }
    }
    if state.wrote_files && super::add::attributes_may_apply(&repo, &root) {
        eprintln!("warning: {}", crate::messages::read_tree::ATTRIBUTES_NOT_APPLIED);
    }
    if let Some(mut lock) = lock.take() {
        if !state.refreshed.is_empty() {
            for (pos, stat) in std::mem::take(&mut state.refreshed) {
                if let Some(e) = index.entry_mut(pos) {
                    e.stat = stat;
                }
            }
            let skip_hash = repo.config.bool("index.skiphash").ok().flatten().unwrap_or(false);
            index
                .write_locked(&mut lock, &WriteOptions { skip_hash })
                .and_then(|()| lock.commit())
                .map_err(|_| Die::Fatal(crate::messages::read_tree::UNABLE_TO_WRITE_INDEX.into()))?;
        }
    }
    Ok(i32::from(state.errors))
}

fn root_display(repo: &Repo) -> String {
    repo.setup
        .work_tree
        .as_deref()
        .map(Path::display)
        .map(|d| d.to_string())
        .unwrap_or_default()
}
