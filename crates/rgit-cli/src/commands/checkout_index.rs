// SPDX-License-Identifier: MIT
//! `git checkout-index` (Documentation/git-checkout-index.adoc): copy files
//! from the index to the work tree. `-a`, `-f`, `-q`, `-n`, `-u`,
//! `--prefix`, `--stdin`, `-z`, `--ignore-skip-worktree-bits`, `--temp`
//! (`.merge_file_XXXXXX` at the top of the work tree, names on stdout) and
//! `--stage=<n>|all`.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use rgit_fs::{LockFile, LockOptions};
use rgit_index::{Entry, Index, Stage, WriteOptions};
use rgit_unpack::messages as umsg;
use rgit_unpack::CheckoutError;
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
    conv: crate::convert::Converter,
    force: bool,
    quiet: bool,
    no_create: bool,
    update: bool,
    prefix: Vec<u8>,
    /// `--temp`: write temporary files and print their names.
    temp: bool,
    /// `--stage`.
    stage: Which,
    /// The directory the command runs in, relative to the top (empty or
    /// ending in `/`), for the paths printed with `--temp`.
    cwd_prefix: Vec<u8>,
    out: Vec<u8>,
    errors: bool,
    wrote_files: bool,
    /// Whether `-u` recorded new stat data in the in-memory index.
    refreshed: bool,
}

impl State<'_> {
    /// Writes the index entry at `pos` unless an up-to-date file is there.
    ///
    /// With `-u` the entry takes the new file's stat data at once, so a
    /// later request for the same path (`checkout-index -u b b`) finds the
    /// file up to date, as git does.
    fn checkout(&mut self, index: &mut Index, pos: usize) -> Result<(), Die> {
        if self.temp {
            let entry = index.entries()[pos].clone();
            if let Some(name) = self.write_temp(&entry)? {
                self.print_temp(&[Some(name)], entry.path());
            }
            return Ok(());
        }
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
                self.conv.to_worktree(entry.path(), &entry.id, blob)
            } else {
                blob
            }
        };
        let symlinks = self.checker.options().symlinks;
        match rgit_unpack::write_entry(&self.root, &target, entry, &data, symlinks) {
            Ok(Some(meta)) => {
                if self.update && self.prefix.is_empty() {
                    if let Some(e) = index.entry_mut(pos) {
                        e.stat = stat_from_metadata(&meta);
                        self.refreshed = true;
                    }
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
    fn checkout_path(&mut self, index: &mut Index, path: &[u8]) -> Result<(), Die> {
        let range = index.path_range(path);
        if range.is_empty() {
            if !self.quiet {
                eprintln!("{}", umsg::not_in_cache(&lossy(path)));
            }
            self.errors = true;
            return Ok(());
        }
        let stage = match self.stage {
            Which::All => {
                let entries: Vec<Entry> = index.entries()[range].to_vec();
                return self.checkout_stages(&entries);
            }
            Which::Merged => Stage::MERGED,
            Which::One(s) => s,
        };
        match index.position(path, stage) {
            Ok(pos) => self.checkout(index, pos),
            Err(_) => {
                if !self.quiet {
                    if stage == Stage::MERGED {
                        eprintln!("{}", umsg::unmerged(&lossy(path)));
                    } else {
                        eprintln!("{}", msg::not_at_stage(&lossy(path), stage.get()));
                    }
                }
                self.errors = true;
                Ok(())
            }
        }
    }

    /// `--stage=all`: the unmerged stages of one path (all entries of the
    /// path, in stage order) to temporary files, one output line naming
    /// them (`.` for a missing stage). A merged path prints nothing.
    fn checkout_stages(&mut self, entries: &[Entry]) -> Result<(), Die> {
        let mut names: [Option<String>; 3] = [None, None, None];
        let mut any = false;
        for e in entries {
            let n = e.stage().get();
            if n == 0 {
                continue;
            }
            any = true;
            names[usize::from(n) - 1] = self.write_temp(e)?;
        }
        if any {
            self.print_temp(&names, entries[0].path());
        }
        Ok(())
    }

    /// Writes `entry` (converted like a checkout) to a new temporary file
    /// at the top of the work tree and returns its name.
    fn write_temp(&mut self, entry: &Entry) -> Result<Option<String>, Die> {
        let blob = rgit_unpack::read_blob(self.repo.odb(), &entry.id)
            .map_err(|e| Die::Error(e.to_string(), 128))?;
        // Symbolic links become regular files holding the target.
        let prefix = if entry.mode.is_symlink() {
            ".merge_link_"
        } else {
            ".merge_file_"
        };
        let data = if entry.mode.is_file() {
            self.wrote_files = true;
            self.conv.to_worktree(entry.path(), &entry.id, blob)
        } else {
            blob
        };
        let fail = |e: std::io::Error| {
            Die::Error(
                umsg::unable_to_create_file(".merge_file_XXXXXX", &crate::global::strerror(&e)),
                128,
            )
        };
        let mut tmp = rgit_fs::TempFile::new_in(&self.root, prefix).map_err(fail)?;
        tmp.file_mut().write_all(&data).map_err(fail)?;
        let (file, path) = tmp.keep();
        drop(file);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(Some(name))
    }

    /// `<name>[ <name> <name>]<TAB><path>`, the path relative to the
    /// current directory and quoted like other path listings.
    fn print_temp(&mut self, names: &[Option<String>], path: &[u8]) {
        for (i, n) in names.iter().enumerate() {
            if i > 0 {
                self.out.push(b' ');
            }
            self.out
                .extend_from_slice(n.as_deref().unwrap_or(".").as_bytes());
        }
        self.out.push(b'\t');
        let rel = crate::quote::relative_to(path, &self.cwd_prefix);
        crate::quote::write_quoted(&mut self.out, &rel, true);
        self.out.push(b'\n');
    }
}

/// Which stage `checkout-index` copies out (`--stage`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Which {
    Merged,
    One(Stage),
    All,
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
    // -1: not given, 0: --no-temp, 1: --temp.
    let temp = Cell::new(-1);
    let prefix = RefCell::new(None::<String>);
    let stage = Cell::new(Which::Merged);
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
            Opt::bool(None, "temp", &temp, msg::HELP_TEMP),
            Opt::string(None, "prefix", &prefix, "string", msg::HELP_PREFIX),
            Opt::callback(None, "stage", Some("(1|2|3|all)"), msg::HELP_STAGE, |v, _| {
                stage.set(match v.unwrap_or("") {
                    "all" => Which::All,
                    "1" => Which::One(Stage::BASE),
                    "2" => Which::One(Stage::OURS),
                    "3" => Which::One(Stage::THEIRS),
                    _ => return Err(CallbackError::Fatal(msg::BAD_STAGE.into())),
                });
                Ok(())
            })
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
    let stage = stage.get();
    if stage == Which::All && temp.get() == 0 {
        return Err(Die::Fatal(msg::ALL_AND_NO_TEMP.into()));
    }
    let temp = temp.get() == 1 || stage == Which::All;

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
        conv: crate::convert::Converter::paths(&repo, &index)?,
        root: root.clone(),
        force: force.get() > 0,
        quiet: quiet.get() > 0,
        no_create: no_create.get(),
        update: update.get(),
        prefix: prefix.borrow().clone().unwrap_or_default().into_bytes(),
        temp,
        stage,
        cwd_prefix: repo.prefix(),
        out: Vec::new(),
        errors: false,
        wrote_files: false,
        refreshed: false,
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
        state.checkout_path(&mut index, &path)?;
    }
    if all.get() {
        // Only the entries below the current directory.
        let under = repo.prefix();
        let mut pos = 0;
        while pos < index.len() {
            let e: &Entry = &index.entries()[pos];
            if !e.path().starts_with(&under) || (e.flags.skip_worktree && !ignore_skip.get()) {
                pos += 1;
                continue;
            }
            match stage {
                Which::All => {
                    let range = index.path_range(e.path());
                    let next = range.end;
                    let entries: Vec<Entry> = index.entries()[range].to_vec();
                    state.checkout_stages(&entries)?;
                    pos = next;
                    continue;
                }
                Which::Merged if e.stage() == Stage::MERGED => state.checkout(&mut index, pos)?,
                Which::One(s) if e.stage() == s => state.checkout(&mut index, pos)?,
                _ => {}
            }
            pos += 1;
        }
    }
    if !state.out.is_empty() {
        let mut o = std::io::stdout().lock();
        let _ = o.write_all(&state.out);
        let _ = o.flush();
    }
    if state.wrote_files && state.conv.skipped_filter() {
        eprintln!("warning: {}", crate::messages::read_tree::ATTRIBUTES_NOT_APPLIED);
    }
    if let Some(mut lock) = lock.take() {
        if state.refreshed {
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
