// SPDX-License-Identifier: MIT
//! `git restore` (Documentation/git-restore.adoc), and the path checkout
//! it shares with `git checkout [<tree-ish>] [--] <pathspec>...`
//! (git-checkout.adoc).
//!
//! Implemented: `-s`/`--source` (also `<a>...<b>`), `-S`/`--staged`,
//! `-W`/`--worktree`, `--overlay`/`--no-overlay`, `--ours`/`--theirs`,
//! `-m`/`--merge` and `--conflict=<style>` (the conflicted file is merged
//! again from the stages, labelled `ours`, `base` and `theirs`, observed
//! with the `git` binary), `--ignore-unmerged`,
//! `--ignore-skip-worktree-bits`, `-q`, `-p`,
//! `--pathspec-from-file`/`--pathspec-file-nul`.
//!
//! With `-m` the conflicts of paths resolved since are re-created from
//! the resolve-undo record. Not yet: `--recurse-submodules`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rgit_fs::{LockFile, LockOptions};
use rgit_hash::ObjectId;
use rgit_index::{AddOptions, Entry, Index, Stage};
use rgit_object::ObjectKind;
use rgit_pathspec::{GlobalFlags, ParseOptions, Pathspec};
use rgit_worktree::stat::{Status, smudge_racy_entries};

use crate::commands::read_tree::{lock_failed, unpack_failed};
use crate::messages::reset as rmsg;
use crate::messages::restore as msg;
use crate::messages::switch as smsg;
use crate::prelude::*;
use crate::refs_support::{Objects, Revs};
use crate::repo::Mode;
use crate::repository::{self, Repo};
use crate::switch_branch as sb;

/// What a path checkout reads and what it updates.
pub(crate) struct PathCheckout {
    /// The tree to take the content from; `None`: the index.
    pub source: Option<ObjectId>,
    /// Update the files in the work tree.
    pub worktree: bool,
    /// Update the index entries from `source`.
    pub index: bool,
    /// Overlay mode: never remove files (no-overlay removes tracked paths
    /// the source does not have).
    pub overlay: bool,
    /// `--ours`/`--theirs`: the stage to write for unmerged paths.
    pub stage: Option<Stage>,
    /// Leave unmerged paths alone with a warning (`-f`,
    /// `--ignore-unmerged`) instead of failing.
    pub ignore_unmerged: bool,
    /// Also restore entries outside the sparse checkout.
    pub ignore_skip_worktree: bool,
    /// Print "Updated N paths from ..." (on standard error).
    pub report: bool,
    /// No warnings about unmerged paths left alone.
    pub quiet: bool,
    /// `-m`/`--conflict`: re-create the conflicts of unmerged paths in
    /// this style.
    pub merge: Option<rgit_diff::merge::ConflictStyle>,
}

/// What happens to one path.
enum Action {
    /// Write this entry's content to the work tree.
    Write(Entry),
    /// Remove the path (`gitlink`: a submodule directory).
    Remove { gitlink: bool },
    /// Remove the path from the index only (an unmerged path left alone).
    Unindex,
    /// Merge the stages (base, ours, theirs) of an unmerged path again
    /// into the work tree.
    Merge(Box<(Option<Entry>, Entry, Entry)>),
}

/// The conflict style of `-m` (`merge.conflictStyle`) or `--conflict`.
pub(crate) fn conflict_style(
    config: &rgit_config::ConfigSet,
    given: Option<&str>,
) -> rgit_diff::merge::ConflictStyle {
    let name = given.map(str::to_owned).or_else(|| {
        config
            .string("merge.conflictstyle")
            .ok()
            .flatten()
            .map(|v| v.to_string())
    });
    name.as_deref()
        .and_then(rgit_diff::merge::ConflictStyle::from_name)
        .unwrap_or_default()
}


/// `--conflict=<style>`: "error: unknown conflict style" for anything
/// but the three styles git knows.
pub(crate) fn check_conflict_style(style: Option<&str>) -> Result<(), Die> {
    match style {
        None | Some("merge" | "diff3" | "zdiff3") => Ok(()),
        Some(other) => Err(Die::Error(msg::unknown_conflict_style(other), 129)),
    }
}

/// Reads the `--pathspec-from-file` input (`name`, relative to the
/// directory the command was started in; `-` is standard input).
pub(crate) fn read_pathspec_file(repo: &Repo, name: &str, nul: bool) -> Result<Vec<Vec<u8>>, Die> {
    use std::io::Read;
    let mut data = Vec::new();
    let read = if name == "-" {
        std::io::stdin().read_to_end(&mut data).map(|_| ())
    } else {
        let full = match &repo.setup.prefix {
            Some(p) if !Path::new(name).is_absolute() => Path::new(p).join(name),
            _ => PathBuf::from(name),
        };
        std::fs::File::open(full).and_then(|mut f| f.read_to_end(&mut data).map(|_| ()))
    };
    read.map_err(|e| Die::Fatal(rmsg::could_not_open(name, &crate::global::strerror(&e))))?;
    rgit_pathspec::parse_pathspec_file(&data, nul).map_err(|e| Die::Fatal(e.to_string()))
}

/// The tree of the tree-ish `id`.
pub(crate) fn tree_of(repo: &Repo, objects: &Objects, id: &ObjectId) -> Option<ObjectId> {
    let p = objects.peel(id)?;
    match objects.kind(&p)? {
        ObjectKind::Tree => Some(p),
        ObjectKind::Commit => sb::commit_tree(repo, &p),
        _ => None,
    }
}

/// Restores the paths matching `specs` as `how` says. Unmatched
/// pathspecs and unmerged paths that cannot be restored are errors
/// (exit 1) before anything changes.
#[allow(clippy::too_many_lines)]
pub(crate) fn checkout_paths<S: AsRef<[u8]>>(
    globals: GlobalFlags,
    repo: &Repo,
    objects: &Objects,
    specs: &[S],
    how: &PathCheckout,
) -> CmdResult {
    let work_tree = repo.work_tree()?;
    let opts = ParseOptions {
        prefix: repo.prefix(),
        root: Some(work_tree.as_os_str().as_encoded_bytes().to_vec()),
        globals,
        ..Default::default()
    };
    let ps = Pathspec::parse(specs, &opts).map_err(|e| Die::Fatal(e.to_string()))?;
    let index_path = repo.setup.cwd.join(repo.index_path());
    let lock = LockFile::acquire(&index_path, &LockOptions::default()).map_err(lock_failed)?;
    let (mut index, mtime) = repo.read_index()?;
    // No refresh: like git, files whose stat data changed are rewritten
    // (and counted) even when their content is the same.
    let checker = repo.checker(mtime)?;
    let mut seen = vec![false; ps.len()];
    let mut actions: Vec<(Vec<u8>, Action)> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut index_changed = false;
    // `--recurse-submodules`: active submodules are restored too.
    let updater = (how.worktree && crate::recurse_submodules::enabled(repo))
        .then(|| crate::recurse_submodules::Updater::new(repo, None))
        .flatten();
    let restore_submodule = |e: &Entry| {
        use rgit_unpack::Submodules as _;
        e.mode.is_gitlink() && updater.as_ref().is_some_and(|u| u.active(e.path()))
    };
    // An unmerged path left alone or failing.
    let unmerged = |path: &[u8], errors: &mut Vec<String>| {
        let p = String::from_utf8_lossy(path);
        if how.ignore_unmerged {
            if !how.quiet {
                eprintln!("warning: {}", smsg::path_unmerged(&p));
            }
        } else {
            errors.push(smsg::path_unmerged(&p));
        }
    };

    // `--merge` re-creates the conflicts of paths resolved since
    // (the resolve undo record).
    if how.source.is_none() && how.merge.is_some() {
        for r in index.resolve_undo_records() {
            if ps.match_path(&r.path).is_none() {
                continue;
            }
            index.remove_path(&r.path);
            for (k, s) in r.stages.iter().enumerate() {
                if let Some((mode, id)) = s {
                    let stage = Stage::new(k as u8 + 1).expect("stage");
                    let e = Entry::new(r.path.clone(), stage, rgit_index::Mode(*mode), *id);
                    index
                        .add(
                            e,
                            AddOptions {
                                ok_to_replace: true,
                                ..Default::default()
                            },
                        )
                        .map_err(|err| Die::Fatal(err.to_string()))?;
                }
            }
            index.remove_resolve_undo(&r.path);
            index_changed = true;
        }
    }
    match how.source {
        Some(t) => {
            let original = index.entries().to_vec();
            let entries = rgit_unpack::tree_entries(repo.odb(), &t).map_err(unpack_failed)?;
            let mut in_tree: HashSet<Vec<u8>> = HashSet::new();
            for e in entries {
                if ps.match_path_seen(e.path(), &mut seen).is_none() {
                    continue;
                }
                in_tree.insert(e.path().to_vec());
                let cur = index.entries_for_path(e.path());
                let same = cur.len() == 1
                    && cur[0].stage() == Stage::MERGED
                    && cur[0].mode == e.mode
                    && cur[0].id == e.id
                    && !cur[0].flags.intent_to_add;
                let clean = same && matches!(checker.check(&cur[0]), Ok(Status::Unchanged));
                if how.index && !same {
                    index.remove_path(e.path());
                    index
                        .add(
                            e.clone(),
                            AddOptions {
                                ok_to_replace: true,
                                ..Default::default()
                            },
                        )
                        .map_err(|err| Die::Fatal(err.to_string()))?;
                    index_changed = true;
                }
                if !clean || restore_submodule(&e) {
                    actions.push((e.path().to_vec(), Action::Write(e)));
                }
            }
            if !how.overlay {
                // Tracked paths the source does not have.
                let mut last: Option<&[u8]> = None;
                for e in &original {
                    if ps.match_path_seen(e.path(), &mut seen).is_none()
                        || in_tree.contains(e.path())
                        || last == Some(e.path())
                    {
                        continue;
                    }
                    last = Some(e.path());
                    if e.stage().is_conflict() {
                        // Left alone in the work tree, but gone from the
                        // index like the paths that are not unmerged.
                        unmerged(e.path(), &mut errors);
                        actions.push((e.path().to_vec(), Action::Unindex));
                        continue;
                    }
                    actions.push((
                        e.path().to_vec(),
                        Action::Remove {
                            gitlink: e.mode.is_gitlink(),
                        },
                    ));
                }
            }
        }
        None => {
            let mut last: Option<&[u8]> = None;
            for e in index.entries() {
                // Skip-worktree paths are outside the checkout: they do
                // not even match the pathspec unless the bits are ignored.
                if e.flags.skip_worktree && !how.ignore_skip_worktree {
                    continue;
                }
                if ps.match_path_seen(e.path(), &mut seen).is_none() {
                    continue;
                }
                if e.stage().is_conflict() {
                    // The first stage stands for the path.
                    if last == Some(e.path()) {
                        continue;
                    }
                    last = Some(e.path());
                    let all = index.entries_for_path(e.path());
                    if how.merge.is_some() {
                        let find = |s: Stage| all.iter().find(|x| x.stage() == s).cloned();
                        match (find(Stage::OURS), find(Stage::THEIRS)) {
                            (Some(ours), Some(theirs)) => actions.push((
                                e.path().to_vec(),
                                Action::Merge(Box::new((find(Stage::BASE), ours, theirs)))
                            )),
                            _ => errors.push(msg::not_all_versions(&String::from_utf8_lossy(e.path()))),
                        }
                        continue;
                    }
                    let Some(stage) = how.stage else {
                        unmerged(e.path(), &mut errors);
                        continue;
                    };
                    match all.iter().find(|x| x.stage() == stage) {
                        Some(chosen) => actions.push((e.path().to_vec(), Action::Write(chosen.clone()))),
                        None if !how.overlay => actions.push((
                            e.path().to_vec(),
                            Action::Remove {
                                gitlink: e.mode.is_gitlink(),
                            },
                        )),
                        None => {
                            let side = if stage == Stage::OURS { "our" } else { "their" };
                            errors.push(msg::no_version(&String::from_utf8_lossy(e.path()), side));
                        }
                    }
                    continue;
                }
                if e.flags.intent_to_add || (e.flags.skip_worktree && !how.ignore_skip_worktree) {
                    continue;
                }
                // A skip-worktree entry counts as unchanged; with
                // --ignore-skip-worktree-bits its file is checked for real.
                let changed = if e.flags.skip_worktree {
                    let mut plain = e.clone();
                    plain.flags.skip_worktree = false;
                    !matches!(checker.check(&plain), Ok(Status::Unchanged))
                } else {
                    !matches!(checker.check(e), Ok(Status::Unchanged))
                };
                if changed || restore_submodule(e) {
                    actions.push((e.path().to_vec(), Action::Write(e.clone())));
                }
            }
        }
    }
    if report_unmatched(&ps, &seen) {
        return Err(Die::Exit(1));
    }
    if !errors.is_empty() {
        for e in &errors {
            eprintln!("error: {e}");
        }
        return Err(Die::Exit(1));
    }

    let symlinks = checker.options().symlinks;
    let mut failed = false;
    let mut count = 0usize;
    // Removals first: a removed file may stand where a directory goes.
    for (path, action) in &actions {
        let gitlink = match action {
            Action::Remove { gitlink } => gitlink,
            Action::Unindex => {
                if how.index {
                    index.remove_path(path);
                    index_changed = true;
                }
                continue;
            }
            Action::Write(_) | Action::Merge(_) => continue,
        };
        if how.worktree {
            if let Err(e) = rgit_unpack::remove_entry(&work_tree, path, *gitlink) {
                eprintln!(
                    "error: {}",
                    rgit_unpack::messages::unable_to_unlink(&String::from_utf8_lossy(path), &crate::global::strerror(&e))
                );
                failed = true;
            }
        }
        if how.index && how.source.is_some() {
            index.remove_path(path);
            index_changed = true;
        }
    }
    let conv = crate::convert::Converter::paths(repo, &index)?;
    // Writes `data` for `entry` and records the new stat data.
    // Records the stat data of the file just written for `entry`.
    let record = |index: &mut Index, path: &[u8], entry: &Entry, meta: &std::fs::Metadata| {
        // The index entry (for `--ours`/`--theirs` the stage written) now
        // describes the new file, unless the index keeps something else
        // (worktree-only restore from a tree).
        let stage = if how.source.is_some() { Stage::MERGED } else { entry.stage() };
        if how.source.is_none() || how.index {
            if let Ok(pos) = index.position(path, stage) {
                if let Some(e) = index
                    .entry_mut(pos)
                    .filter(|e| e.id == entry.id && e.mode == entry.mode)
                {
                    e.stat = rgit_worktree::stat::stat_from_metadata(meta);
                }
            }
        }
    };
    let write = |index: &mut Index, path: &[u8], entry: &Entry, data: &[u8]| -> bool {
        match rgit_unpack::write_entry(&work_tree, path, entry, data, symlinks) {
            Ok(Some(meta)) => {
                record(index, path, entry, &meta);
                true
            }
            Ok(None) => true,
            Err(e) => {
                eprintln!("error: {e}");
                false
            }
        }
    };
    let mut parallel = rgit_unpack::ParallelCheckout::new(
        rgit_unpack::Filter::parallel_config(&conv),
        &work_tree,
        repo.odb(),
    );
    let mut delayed: Vec<(&[u8], &Entry)> = Vec::new();
    let mut recreated = 0usize;
    for (i, (path, action)) in actions.iter().enumerate() {
        if let Action::Merge(stages) = action {
            let (base, ours, theirs) = &**stages;
            recreated += 1;
            if let Err(e) = remerge(repo, &work_tree, &conv, path, base.as_ref(), ours, theirs, how, symlinks) {
                eprintln!("error: {e}");
                failed = true;
            }
            continue;
        }
        let Action::Write(entry) = action else {
            continue;
        };
        count += 1;
        if !how.worktree {
            continue;
        }
        if let Some(res) = rgit_unpack::stream_entry(repo.odb(), &conv, &work_tree, path, entry) {
            match res {
                Ok(meta) => record(&mut index, path, entry, &meta),
                Err(e) => {
                    eprintln!("error: {e}");
                    failed = true;
                    count -= 1;
                }
            }
            continue;
        }
        if parallel.enabled() {
            match parallel.enqueue(i, path, entry, &conv) {
                Ok(true) => continue,
                Ok(false) => {}
                Err(e) => {
                    eprintln!("error: {e}");
                    failed = true;
                    count -= 1;
                    continue;
                }
            }
        }
        let data = if entry.mode.is_gitlink() {
            Vec::new()
        } else {
            match rgit_unpack::read_blob(repo.odb(), &entry.id) {
                Ok(d) if entry.mode.is_file() => {
                    match conv.to_worktree_delayable(path, &entry.id, d, true) {
                        Some(d) => d,
                        None => {
                            delayed.push((path, entry));
                            continue;
                        }
                    }
                }
                Ok(d) => d,
                Err(_) => {
                    eprintln!(
                        "error: {}",
                        rgit_unpack::messages::unable_to_read_sha1_of(
                            &String::from_utf8_lossy(path),
                            &entry.id.to_hex()
                        )
                    );
                    failed = true;
                    count -= 1;
                    continue;
                }
            }
        };
        if !write(&mut index, path, entry, &data) {
            failed = true;
            count -= 1;
        }
    }
    for o in parallel.run() {
        let (path, action) = &actions[o.key];
        let Action::Write(entry) = action else {
            continue;
        };
        match o.result {
            Ok(meta) => record(&mut index, path, entry, &meta),
            Err(e) => {
                eprintln!("error: {e}");
                failed = true;
                count -= 1;
            }
        }
    }
    // `--recurse-submodules`: restored submodules are checked out at the
    // recorded commit (local changes in them are lost).
    if let Some(u) = &updater {
        use rgit_unpack::Submodules as _;
        for (path, action) in &actions {
            if let Action::Write(entry) = action {
                if restore_submodule(entry) {
                    if let Err(e) = u.update(path, None, Some(&entry.id), true) {
                        eprintln!("error: {e}");
                        failed = true;
                    }
                }
            }
        }
    }
    if !delayed.is_empty() {
        let mut filtering = crate::switch_branch::progress().then(|| {
            rgit_progress::Progress::start_delayed(
                rgit_unpack::messages::PROGRESS_FILTERING,
                delayed.len() as u64,
                true,
            )
        });
        let mut filtered = 0u64;
        let mut filtered_bytes = 0u64;
        if let Some(m) = &mut filtering {
            m.throughput(0);
        }
        let missing = conv.finish_delayed(&mut |path, data| {
            if let Some(m) = &mut filtering {
                filtered += 1;
                filtered_bytes += data.len() as u64;
                m.throughput(filtered_bytes);
                m.set(filtered);
            }
            if let Some((p, entry)) = delayed.iter().find(|(p, _)| *p == path) {
                if !write(&mut index, p, entry, &data) {
                    failed = true;
                    count -= 1;
                }
            }
        });
        if let Some(m) = filtering {
            m.stop();
        }
        for m in &missing {
            eprintln!("error: {m}");
        }
        failed |= !missing.is_empty();
        count = count.saturating_sub(missing.len());
    }
    if index_changed {
        index.set_cache_tree(None);
        let ct = rgit_unpack::repair_cache_tree(&index, repo.odb());
        index.set_cache_tree(Some(ct));
    }
    if how.source.is_none() || how.index {
        if let Some(reference) = mtime {
            let _ = smudge_racy_entries(&mut index, reference, &checker);
        }
        sb::write_index(repo, &index, lock)?;
    } else {
        drop(lock);
    }
    if how.report {
        if recreated > 0 {
            eprintln!("{}", msg::recreated_conflicts(recreated));
        }
        match how.source {
            Some(t) => eprintln!("{}", smsg::updated_paths_from(count, &objects.abbrev(&t, 7))),
            None if recreated == 0 || count > 0 => {
                eprintln!("{}", smsg::updated_paths_from_index(count));
            }
            None => {}
        }
    }

    Ok(if failed { 1 } else { 0 })
}

/// Writes the three-way merge of the stages of the unmerged `path` to
/// the work tree (the index keeps the stages).
#[allow(clippy::too_many_arguments)]
fn remerge(
    repo: &Repo,
    work_tree: &Path,
    conv: &crate::convert::Converter,
    path: &[u8],
    base: Option<&Entry>,
    ours: &Entry,
    theirs: &Entry,
    how: &PathCheckout,
    symlinks: bool,
) -> Result<(), String> {
    let read = |e: &Entry| rgit_unpack::read_blob(repo.odb(), &e.id).map_err(|e| e.to_string());
    let base_data = match base {
        Some(b) => read(b)?,
        None => Vec::new(),
    };
    let (ours_data, theirs_data) = (read(ours)?, read(theirs)?);
    // The `merge` and `conflict-marker-size` attributes of the path.
    let path_merge = crate::commands::merge::attrs::RepoPathMerge::open(
        &crate::Context::default(),
        repo,
        false,
        None,
        None,
    )
    .ok();
    let (driver, marker_size) = path_merge
        .as_ref()
        .map_or((rgit_merge::Driver::Text, None), |pm| pm.driver(path, false));
    let mut opts = rgit_diff::merge::MergeOptions {
        style: how.merge.unwrap_or_default(),
        marker_size: marker_size.unwrap_or(0),
        ours_label: b"ours",
        base_label: b"base",
        theirs_label: b"theirs",
        diff: rgit_diff::merge::merge_diff_options(),
        ..Default::default()
    };
    let merged = match (&driver, &path_merge) {
        (rgit_merge::Driver::Custom(name), Some(pm)) => {
            let input = rgit_merge::DriverInput {
                path,
                base: &base_data,
                ours: &ours_data,
                theirs: &theirs_data,
                marker_size: marker_size.unwrap_or(rgit_diff::merge::DEFAULT_MARKER_SIZE),
                base_label: b"base",
                ours_label: b"ours",
                theirs_label: b"theirs",
            };
            pm.run(name, &input)?.0
        }
        // Binary content cannot be merged: ours stays (as in a merge).
        (rgit_merge::Driver::Binary, _) => ours_data,
        _ => {
            if driver == rgit_merge::Driver::Union {
                opts.favor = rgit_diff::merge::Favor::Union;
            }
            rgit_diff::merge::merge(&base_data, &ours_data, &theirs_data, &opts)
                .map_or(ours_data, |r| r.content)
        }
    };
    // Like git, the merged content is stored as a blob.
    let id = repo
        .odb()
        .write(ObjectKind::Blob, &merged)
        .map_err(|e| e.to_string())?;
    if !how.worktree {
        return Ok(());
    }
    let data = if ours.mode.is_file() {
        conv.to_worktree(path, &id, merged)
    } else {
        merged
    };
    rgit_unpack::write_entry(work_tree, path, ours, &data, symlinks)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Prints "pathspec ... did not match" for every unmatched item; true if
/// there was one.
fn report_unmatched(ps: &Pathspec, seen: &[bool]) -> bool {
    let mut any = false;
    for item in ps.unmatched(seen) {
        eprintln!(
            "error: {}",
            smsg::pathspec_no_match(&String::from_utf8_lossy(item.original()))
        );
        any = true;
    }
    any
}

/// The patch mode of `restore -p` and `checkout -p`: `tree` is the
/// source (`None`: the index), `index`/`worktree` what is restored.
pub(crate) fn patch_options(
    repo: &Repo,
    revs: &Revs,
    tree: Option<ObjectId>,
    index: bool,
    worktree: bool,
    paths: Vec<String>,
) -> Result<crate::interactive::Options, Die> {
    use crate::interactive::{Mode as PatchMode, Options};
    let objects = Objects::open(&repo.setup);
    let head_tree = revs
        .resolve("HEAD")
        .and_then(|h| tree_of(repo, &objects, &h));
    let mode = match tree {
        None => PatchMode::CheckoutIndex,
        Some(t) => {
            let head = head_tree == Some(t);
            match (index, worktree, head) {
                (true, true, true) => PatchMode::CheckoutHead,
                (true, true, false) => PatchMode::CheckoutRev,
                (true, false, true) => PatchMode::ResetHead,
                (true, false, false) => PatchMode::ResetRev,
                (false, _, true) => PatchMode::WorktreeHead,
                (false, _, false) => PatchMode::WorktreeRev,
            }
        }
    };
    let mut opts = Options::new(mode);
    if let Some(t) = tree {
        if !matches!(mode, PatchMode::CheckoutHead | PatchMode::ResetHead | PatchMode::WorktreeHead) {
            opts.rev = t.to_hex();
        }
    }
    opts.pathspec = paths;
    Ok(opts)
}

/// Resolves the `--source` tree-ish: "could not resolve" when it names
/// nothing, "unable to read tree" when it is no tree-ish.
pub(crate) fn resolve_source(repo: &Repo, revs: &Revs, objects: &Objects, name: &str) -> Result<ObjectId, Die> {
    let id = match revs.resolve(name) {
        Some(id) => id,
        None => super::switch::resolve_commit(revs, objects, name)
            .ok_or_else(|| Die::Fatal(msg::could_not_resolve(name)))?,
    };
    if id == ObjectId::empty_tree(repo.hash) {
        // The empty tree exists without being stored.
        return Ok(id);
    }
    tree_of(repo, objects, &id).ok_or_else(|| {
        let id = objects.peel(&id).unwrap_or(id);
        Die::Fatal(smsg::unable_to_read_tree(&id.to_hex()))
    })
}

#[allow(clippy::too_many_lines)]
pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let source = RefCell::new(None);
    let staged = Cell::new(false);
    let worktree = Cell::new(false);
    let ignore_unmerged = Cell::new(false);
    let overlay = Cell::new(false);
    let quiet = Cell::new(0);
    let recurse = Cell::new(None::<bool>);
    let progress = Cell::new(-1);
    let merge = Cell::new(false);
    let conflict = RefCell::new(None);
    let stage = Cell::new(0);
    let patch = Cell::new(false);
    let unified = Cell::new(crate::interactive::patch::UNSET);
    let inter_hunk = Cell::new(crate::interactive::patch::UNSET);
    let skip_worktree_bits = Cell::new(false);
    let from_file = RefCell::new(None);
    let file_nul = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::string('s', "source", &source, "tree-ish", msg::HELP_SOURCE),
            Opt::flag('S', "staged", &staged, msg::HELP_STAGED),
            Opt::flag('W', "worktree", &worktree, msg::HELP_WORKTREE),
            Opt::flag(None, "ignore-unmerged", &ignore_unmerged, msg::HELP_IGNORE_UNMERGED),
            Opt::flag(None, "overlay", &overlay, msg::HELP_OVERLAY),
            Opt::quiet(&quiet, smsg::HELP_QUIET),
            crate::recurse_submodules::option(&recurse, smsg::HELP_RECURSE),
            Opt::bool(None, "progress", &progress, smsg::HELP_PROGRESS),
            // `--conflict` implies `--merge`; `--no-merge` undoes both,
            // `--no-conflict` only the implied merge (t7201 41-44).
            Opt::callback('m', "merge", None, smsg::HELP_MERGE, |_, unset| {
                merge.set(!unset);
                if unset {
                    *conflict.borrow_mut() = None;
                }
                Ok(())
            })
            .noarg(),
            Opt::callback(None, "conflict", Some("style"), smsg::HELP_CONFLICT, |v, unset| {
                *conflict.borrow_mut() = (!unset).then(|| v.unwrap_or_default().to_owned());
                Ok(())
            }),
            Opt::set_int('2', "ours", &stage, smsg::HELP_OURS, 2).noneg(),
            Opt::set_int('3', "theirs", &stage, smsg::HELP_THEIRS, 3).noneg(),
            Opt::flag('p', "patch", &patch, rmsg::HELP_PATCH),
            Opt::integer('U', "unified", &unified, rmsg::HELP_UNIFIED).noneg(),
            Opt::integer(None, "inter-hunk-context", &inter_hunk, rmsg::HELP_INTER_HUNK).noneg(),
            Opt::flag(
                None,
                "ignore-skip-worktree-bits",
                &skip_worktree_bits,
                smsg::HELP_SKIP_WORKTREE_BITS,
            ),
            Opt::string(None, "pathspec-from-file", &from_file, "file", rmsg::HELP_PATHSPEC_FROM_FILE),
            Opt::flag(None, "pathspec-file-nul", &file_nul, rmsg::HELP_PATHSPEC_FILE_NUL),
        ],
    );
    let args = parser.parse(argv)?.args;
    drop(parser);
    check_conflict_style(conflict.borrow().as_deref())?;
    let from_file = from_file.borrow().clone();
    let merge = merge.get() || conflict.borrow().is_some();
    let stage = Stage::new(stage.get() as u8).filter(|s| s.is_conflict());

    if from_file.is_some() && !args.is_empty() {
        return Err(Die::Fatal(rmsg::FROM_FILE_WITH_ARGS.to_owned()));
    }
    if patch.get() {
        if from_file.is_some() {
            return Err(Die::Fatal(rmsg::FROM_FILE_WITH_PATCH.to_owned()));
        }
        if overlay.get() {
            return Err(Die::Fatal(msg::PATCH_WITH_OVERLAY.to_owned()));
        }
    }
    if file_nul.get() && from_file.is_none() {
        return Err(Die::Fatal(rmsg::FILE_NUL_WITHOUT_FROM_FILE.to_owned()));
    }
    crate::interactive::patch::check_options(
        patch.get(),
        unified.get(),
        inter_hunk.get(),
        false,
        "--patch",
    )?;
    crate::recurse_submodules::declare(recurse.get());

    let repo = Repo::open(ctx, Mode::Required)?;
    let objects = Objects::open(&repo.setup);
    let revs = Revs::new(ctx, &repo.setup)?;
    let staged = staged.get();
    // Without -S/-W, the work tree; -S alone restores only the index.
    let worktree = worktree.get() || !staged;
    let source_name = source.borrow().clone().or_else(|| staged.then(|| "HEAD".to_owned()));
    let tree = match &source_name {
        Some(name) => Some(resolve_source(&repo, &revs, &objects, name)?),
        None => None,
    };
    if patch.get() {
        let paths = match &from_file {
            Some(name) => read_pathspec_file(&repo, name, file_nul.get())?
                .into_iter()
                .map(|p| String::from_utf8_lossy(&p).into_owned())
                .collect(),
            None => args,
        };
        let opts = patch_options(&repo, &revs, tree, staged, worktree, paths)?;
        let mut opts = opts;
        opts.set_context(unified.get(), inter_hunk.get());
        crate::interactive::patch::run(ctx, &repo, &opts)?;
        return Ok(0);
    }
    let specs: Vec<Vec<u8>> = match &from_file {
        Some(name) => read_pathspec_file(&repo, name, file_nul.get())?,
        None => args.into_iter().map(String::into_bytes).collect(),
    };
    if specs.is_empty() {
        return Err(Die::Fatal(msg::NO_PATHS.to_owned()));
    }
    if stage.is_some() && merge {
        return Err(Die::Fatal(msg::INDEX_MODES.to_owned()));
    }
    if staged && stage.is_some() {
        return Err(Die::Fatal(msg::OURS_WITH_STAGED.to_owned()));
    }
    if staged && merge {
        return Err(Die::Fatal(msg::MERGE_WITH_STAGED.to_owned()));
    }
    if tree.is_some() && (stage.is_some() || merge) {
        return Err(Die::Fatal(msg::FROM_TREE.to_owned()));
    }
    checkout_paths(
        repository::pathspec_globals(ctx),
        &repo,
        &objects,
        &specs,
        &PathCheckout {
            source: tree,
            worktree,
            index: staged,
            overlay: overlay.get(),
            stage,
            ignore_unmerged: ignore_unmerged.get(),
            ignore_skip_worktree: skip_worktree_bits.get(),
            report: false,
            quiet: quiet.get() > 0,
            merge: merge.then(|| conflict_style(&repo.config, conflict.borrow().as_deref())),
        },
    )
}
