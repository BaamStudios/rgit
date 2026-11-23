// SPDX-License-Identifier: MIT
//! Submodules as the core commands see them: which `ignore` setting applies
//! to a gitlink, and the state of a submodule's work tree (new commits,
//! modified content, untracked content). The parsing and settings live in
//! the `rgit-submodule` crate.
//!
//! The work tree state is found as git does it: by running `status
//! --porcelain=2` inside the submodule (this binary, with the repository
//! environment of the superproject removed).

use std::path::Path;
use std::process::{Command, Stdio};

use rgit_hash::ObjectId;
use rgit_index::{Index, Stage};
use rgit_object::Object;
use rgit_submodule::{Diagnostic, Gitmodules, Ignore, Mode, Origin, parse_ignore};

use crate::Die;
use crate::messages::submodule as msg;
use crate::repository::Repo;

/// Parse an `--ignore-submodules` / `ignore` value; an invalid one is fatal.
pub fn parse_ignore_arg(value: &str) -> Result<Ignore, Die> {
    parse_ignore(value.as_bytes()).ok_or_else(|| Die::Fatal(msg::bad_ignore_argument(value)))
}

/// Load the superproject's `.gitmodules`: from the work tree, else the
/// index, else `HEAD`. Warnings are printed; a broken file is fatal. While
/// `.gitmodules` has a merge conflict it is not read at all.
pub fn load_gitmodules(repo: &Repo, index: Option<&Index>) -> Result<Gitmodules, Die> {
    let read;
    let index = match index {
        Some(i) => Some(i),
        None => {
            read = repo.read_index().ok().map(|(i, _)| i);
            read.as_ref()
        }
    };
    if index.is_some_and(|i| {
        i.entries_for_path(b".gitmodules")
            .iter()
            .any(|e| e.stage().is_conflict())
    }) {
        return Ok(Gitmodules::default());
    }
    let file = repo
        .setup
        .work_tree
        .as_ref()
        .map(|w| w.join(rgit_submodule::GITMODULES));
    let (data, origin) = if let Some(data) = file
        .as_ref()
        .filter(|f| f.symlink_metadata().is_ok_and(|m| m.is_file()))
        .and_then(|f| std::fs::read(f).ok())
    {
        let f = file.expect("file");
        (data, Origin::File(f.to_string_lossy().into_owned()))
    } else if let Some(e) = index.and_then(|i| i.get(b".gitmodules", Stage::MERGED)) {
        match repo.read_object(&e.id) {
            Some(Object::Blob(b)) => (
                b.data,
                Origin::Blob(format!(":{}", rgit_submodule::GITMODULES)),
            ),
            _ => return Ok(Gitmodules::default()),
        }
    } else {
        match head_gitmodules(repo) {
            Some((data, id)) => (data, Origin::Blob(format!("{}:.gitmodules", id.to_hex()))),
            None => return Ok(Gitmodules::default()),
        }
    };
    match Gitmodules::parse(&data, &origin, Mode::Current) {
        Ok((gm, diags)) => {
            print_diagnostics(&diags);
            Ok(gm)
        }
        Err(e) => {
            print_diagnostics(&e.diagnostics);
            Err(Die::Fatal(e.fatal))
        }
    }
}

fn head_gitmodules(repo: &Repo) -> Option<(Vec<u8>, ObjectId)> {
    use rgit_refs::RefStore;
    let head = rgit_refs::files::FilesStore::new(repo.setup.git_dir.clone(), repo.hash)
        .resolve_oid("HEAD")
        .ok()
        .flatten()?;
    let commit = match repo.read_object(&head)? {
        Object::Commit(c) => c,
        _ => return None,
    };
    let tree = match repo.read_object(&commit.tree().ok()?)? {
        Object::Tree(t) => t,
        _ => return None,
    };
    let e = tree.entries.iter().find(|e| e.name == b".gitmodules")?;
    match repo.read_object(&e.oid)? {
        Object::Blob(b) => Some((b.data, e.oid)),
        _ => None,
    }
}

fn print_diagnostics(diags: &[Diagnostic]) {
    for d in diags {
        match d {
            Diagnostic::Warning(m) => eprintln!("warning: {m}"),
            Diagnostic::Error(m) => crate::messages::print_error(m),
        }
    }
}

/// Which `ignore` setting applies to each submodule.
pub struct IgnoreRules {
    /// Path -> `submodule.<name>.ignore` from the configuration, else from
    /// `.gitmodules`.
    by_path: Vec<(Vec<u8>, Ignore)>,
    /// `--ignore-submodules` on the command line: overrides everything.
    command_line: Option<Ignore>,
    /// `diff.ignoreSubmodules` (when used), else the command's default.
    default: Ignore,
}

impl IgnoreRules {
    /// The command line wins, then `submodule.<name>.ignore` from the
    /// configuration and from `.gitmodules`, then `diff.ignoreSubmodules`
    /// when `use_diff_config` (status and the diff porcelain), then
    /// `fallback`.
    pub fn load(
        repo: &Repo,
        index: Option<&Index>,
        command_line: Option<Ignore>,
        use_diff_config: bool,
        fallback: Ignore,
    ) -> Result<IgnoreRules, Die> {
        let config_value = |key: &[u8]| -> Result<Option<Ignore>, Die> {
            match repo.config.get(key) {
                Some(e) => {
                    let v = e.value.as_ref().map(|v| v.to_string()).unwrap_or_default();
                    parse_ignore_arg(&v).map(Some)
                }
                None => Ok(None),
            }
        };
        let default = if use_diff_config {
            config_value(b"diff.ignoresubmodules")?.unwrap_or(fallback)
        } else {
            fallback
        };
        let gitmodules = load_gitmodules(repo, index)?;
        let mut by_path = Vec::new();
        for sub in gitmodules.iter() {
            let Some(path) = &sub.path else { continue };
            let mut key = b"submodule.".to_vec();
            key.extend_from_slice(&sub.name);
            key.extend_from_slice(b".ignore");
            if let Some(i) = config_value(&key)?.or(sub.ignore) {
                if gitmodules.by_path(path).is_some_and(|s| s.name == sub.name) {
                    by_path.push((path.to_vec(), i));
                }
            }
        }
        Ok(IgnoreRules {
            by_path,
            command_line,
            default,
        })
    }

    /// The rules for the diff commands when the index has gitlinks: the
    /// fallback is `untracked`; `porcelain` (`git diff`) also reads
    /// `diff.ignoreSubmodules`.
    pub fn for_index(
        repo: &Repo,
        index: &Index,
        command_line: Option<Ignore>,
        porcelain: bool,
    ) -> Result<Option<IgnoreRules>, Die> {
        if !index.entries().iter().any(|e| e.mode.is_gitlink()) {
            return Ok(None);
        }
        IgnoreRules::load(
            repo,
            Some(index),
            command_line,
            porcelain,
            Ignore::Untracked,
        )
        .map(Some)
    }

    /// Drop the pairs of submodules whose setting is `all` (also changes
    /// between recorded commits). The rules are only loaded when there is a
    /// gitlink among the pairs.
    pub fn drop_ignored(
        repo: &Repo,
        pairs: &mut Vec<crate::diff::collect::Pair>,
        command_line: Option<Ignore>,
        porcelain: bool,
    ) -> Result<(), Die> {
        use crate::diff::collect::{GITLINK_MODE, type_bits};
        let link = |p: &crate::diff::collect::Pair| {
            p.old
                .iter()
                .chain(p.new.iter())
                .any(|s| type_bits(s.mode) == GITLINK_MODE)
        };
        if !pairs.iter().any(link) {
            return Ok(());
        }
        let rules = IgnoreRules::load(repo, None, command_line, porcelain, Ignore::Untracked)?;
        pairs.retain(|p| !(link(p) && rules.for_path(&p.path) == Ignore::All));
        Ok(())
    }

    /// The setting for the gitlink at `path`.
    pub fn for_path(&self, path: &[u8]) -> Ignore {
        if let Some(i) = self.command_line {
            return i;
        }
        self.by_path
            .iter()
            .find(|(p, _)| p == path)
            .map_or(self.default, |(_, i)| *i)
    }
}

/// The state of a submodule's work tree against the commit recorded in
/// the superproject.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WtState {
    /// The commit checked out in the submodule, if it is populated.
    pub head: Option<ObjectId>,
    /// `HEAD` differs from the recorded commit.
    pub new_commits: bool,
    /// Tracked files are modified or staged (or a nested submodule is).
    pub modified: bool,
    /// There are untracked files.
    pub untracked: bool,
}

impl WtState {
    /// Modified or untracked content.
    pub fn dirty(&self) -> bool {
        self.modified || self.untracked
    }

    /// Anything to report.
    pub fn changed(&self) -> bool {
        self.new_commits || self.dirty()
    }
}

/// The state of the submodule work tree at `dir` whose recorded commit is
/// `recorded`, as far as `ignore` lets it count. `untracked_files` is false
/// for `status -uno`.
pub fn worktree_state(
    dir: &Path,
    recorded: &ObjectId,
    hash: rgit_hash::HashKind,
    ignore: Ignore,
    untracked_files: bool,
) -> WtState {
    let mut st = WtState::default();
    if ignore == Ignore::All {
        return st;
    }
    // A `.git` file that leads nowhere is fatal (as git).
    if std::fs::metadata(dir.join(".git")).is_ok_and(|m| m.is_file())
        && rgit_submodule::gitdir::git_dir_of(dir).is_some_and(|g| !g.join("HEAD").exists())
    {
        let dot_git = dir.join(".git");
        let cwd = std::env::current_dir().unwrap_or_default();
        let shown = dot_git.strip_prefix(&cwd).unwrap_or(&dot_git);
        crate::convert::die_now(&msg::gitfile_not_repo(&shown.display().to_string()));
    }
    let Some(head) = crate::repository::submodule_head(dir, hash) else {
        return st;
    };
    st.head = Some(head);
    st.new_commits = head != *recorded;
    if ignore == Ignore::Dirty {
        return st;
    }
    let untracked = untracked_files && ignore != Ignore::Untracked;
    let (modified, has_untracked) = run_status(dir, untracked);
    st.modified = modified;
    st.untracked = has_untracked && untracked;
    st
}

/// Run `status --porcelain=2` in the submodule and classify its lines.
fn run_status(dir: &Path, untracked: bool) -> (bool, bool) {
    let Ok(exe) = std::env::current_exe() else {
        return (false, false);
    };
    let mut cmd = Command::new(exe);
    cmd.current_dir(dir)
        .arg("status")
        .arg("--porcelain=2")
        .arg(if untracked { "-unormal" } else { "-uno" })
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    prepare_env(&mut cmd);
    let Ok(out) = cmd.output() else {
        return (false, false);
    };
    let (mut modified, mut has_untracked) = (false, false);
    for line in out.stdout.split(|&b| b == b'\n') {
        match line.first() {
            Some(b'?') => has_untracked = true,
            Some(b'1' | b'2' | b'u') => {
                // A nested submodule that only has untracked content
                // counts as untracked content here.
                let sub = line.split(|&b| b == b' ').nth(2).unwrap_or_default();
                if sub == b"S..U" {
                    has_untracked = true;
                } else {
                    modified = true;
                }
            }
            _ => {}
        }
        if modified && has_untracked {
            break;
        }
    }
    (modified, has_untracked)
}

/// The environment for a command run inside a submodule: none of the
/// superproject's repository variables (configuration given with `-c` is
/// kept), and `GIT_DIR=.git`.
pub fn prepare_env(cmd: &mut Command) {
    for v in rgit_transport::command::LOCAL_ENV_VARS {
        if !matches!(*v, "GIT_CONFIG_PARAMETERS" | "GIT_CONFIG_COUNT") {
            cmd.env_remove(v);
        }
    }
    cmd.env("GIT_DIR", ".git");
}

/// The git directory of the submodule at `path` (relative to the top of
/// the superproject's work tree): its `.git` in the work tree, else the
/// absorbed `$GIT_COMMON_DIR/modules/<name>`.
pub fn sub_git_dir(repo: &Repo, path: &[u8]) -> Option<std::path::PathBuf> {
    if let Some(wt) = &repo.setup.work_tree {
        let dir = crate::diff::collect::os_path(wt, path);
        if let Some(g) = rgit_submodule::gitdir::git_dir_of(&dir) {
            if g.join("HEAD").exists() {
                return Some(g);
            }
        }
    }
    let gm = load_gitmodules(repo, None).ok()?;
    let name = gm
        .by_path(path)
        .map_or_else(|| path.to_vec(), |s| s.name.to_vec());
    let dir = rgit_submodule::gitdir::modules_dir(&repo.setup.absolute_common_dir(), &name);
    dir.join("HEAD").exists().then_some(dir)
}

/// Run this binary on the repository `git_dir` and return its standard
/// output when it succeeds. Only commands that do not use a work tree are
/// run this way: the submodule's `core.worktree` may name a directory that
/// is gone, so the work tree is pinned to the git directory.
fn run_in(git_dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let exe = std::env::current_exe().ok()?;
    let mut cmd = Command::new(exe);
    prepare_env(&mut cmd);
    cmd.env("GIT_DIR", git_dir)
        .env("GIT_WORK_TREE", git_dir)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    let out = cmd.output().ok()?;
    out.status.success().then_some(out.stdout)
}

/// The `--submodule=log` lines for the submodule at `path` changing from
/// `old` to `new` (`None`: added or deleted), with `dirty`
/// ([`crate::diff::collect::DIRTY_MODIFIED`] / `DIRTY_UNTRACKED`).
pub fn summary_log(
    out: &mut Vec<u8>,
    repo: &Repo,
    path: &[u8],
    old: Option<ObjectId>,
    new: Option<ObjectId>,
    dirty: u8,
) {
    summary(out, repo, path, old, new, dirty, None, (b"", b""));
}

/// The `--submodule=diff` output: the `--submodule=log` header lines, then
/// the diff of the submodule's contents (against its work tree when
/// `worktree` is set), with paths below `path`.
#[allow(clippy::too_many_arguments)]
pub fn summary_diff(
    out: &mut Vec<u8>,
    repo: &Repo,
    path: &[u8],
    old: Option<ObjectId>,
    new: Option<ObjectId>,
    dirty: u8,
    worktree: bool,
    prefixes: (&[u8], &[u8]),
) {
    summary(out, repo, path, old, new, dirty, Some(worktree), prefixes);
}

/// `diff`: `None` for the log format, else whether the new side is the
/// submodule's work tree.
#[allow(clippy::too_many_arguments)]
fn summary(
    out: &mut Vec<u8>,
    repo: &Repo,
    path: &[u8],
    old: Option<ObjectId>,
    new: Option<ObjectId>,
    dirty: u8,
    diff: Option<bool>,
    pre: (&[u8], &[u8]),
) {
    use crate::diff::collect::{DIRTY_MODIFIED, DIRTY_UNTRACKED};
    let line = |out: &mut Vec<u8>, text: &str| {
        out.extend_from_slice(b"Submodule ");
        out.extend_from_slice(path);
        out.push(b' ');
        out.extend_from_slice(text.as_bytes());
        out.push(b'\n');
    };
    if dirty & DIRTY_UNTRACKED != 0 {
        line(out, msg::CONTAINS_UNTRACKED);
    }
    if dirty & DIRTY_MODIFIED != 0 {
        line(out, msg::CONTAINS_MODIFIED);
    }
    let git_dir = sub_git_dir(repo, path);
    if old == new {
        if let (Some(true), Some(g), Some(o)) = (diff, &git_dir, old) {
            run_diff(out, repo, g, path, pre, Some(o), None);
        }
        return;
    }
    let short = |id: &ObjectId| -> String {
        git_dir
            .as_ref()
            .and_then(|g| run_in(g, &["rev-parse", "--short", &id.to_hex()]))
            .and_then(|o| String::from_utf8(o).ok())
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| id.to_hex_with_len(7))
    };
    let zeros = "0".repeat(7);
    let (a, b) = match (old, new) {
        (None, Some(b)) => {
            line(out, &msg::new_submodule(&zeros, &short(&b)));
            if let (Some(wt), Some(g)) = (diff, &git_dir) {
                run_diff(out, repo, g, path, pre, None, (!wt).then_some(b));
            }
            return;
        }
        (Some(a), None) => {
            line(out, &msg::submodule_deleted(&short(&a), &zeros));
            if let (Some(_), Some(g)) = (diff, &git_dir) {
                run_diff_trees(out, g, path, pre, Some(a), None);
            }
            return;
        }
        (Some(a), Some(b)) => (a, b),
        (None, None) => return,
    };
    let (sa, sb) = (short(&a), short(&b));
    let present = |g: &Path, id: &ObjectId| {
        run_in(
            g,
            &["cat-file", "-e", &format!("{}^{{commit}}", id.to_hex())],
        )
        .is_some()
    };
    let Some(g) = git_dir.filter(|g| present(g, &a) && present(g, &b)) else {
        line(out, &msg::commits_not_present(&sa, &sb));
        return;
    };
    let ancestor = |x: &ObjectId, y: &ObjectId| {
        run_in(
            &g,
            &["merge-base", "--is-ancestor", &x.to_hex(), &y.to_hex()],
        )
        .is_some()
    };
    let header = if ancestor(&a, &b) {
        format!("{sa}..{sb}:")
    } else if ancestor(&b, &a) {
        format!("{sa}..{sb} (rewind):")
    } else {
        format!("{sa}...{sb}:")
    };
    line(out, &header);
    if let Some(wt) = diff {
        if wt {
            run_diff(out, repo, &g, path, pre, Some(a), None);
        } else {
            run_diff_trees(out, &g, path, pre, Some(a), Some(b));
        }
        return;
    }
    let range = format!("{}...{}", a.to_hex(), b.to_hex());
    if let Some(log) = run_in(
        &g,
        &[
            "log",
            "--left-right",
            "--first-parent",
            "--format=  %m %s",
            &range,
            "--",
        ],
    ) {
        out.extend_from_slice(&log);
    }
}

/// `status.submoduleSummary`: `None` when off, `Some(None)` without a
/// limit, else the number of commits to show.
pub fn summary_limit(config: &rgit_config::ConfigSet) -> Option<Option<usize>> {
    let e = config.get("status.submodulesummary")?;
    let n: i64 = match rgit_config::value::parse_bool_or_int(e.value.as_ref().map(|v| &v[..])) {
        Ok(rgit_config::BoolOrInt::Bool(true)) => -1,
        Ok(rgit_config::BoolOrInt::Bool(false)) | Err(_) => 0,
        Ok(rgit_config::BoolOrInt::Int(n)) => n,
    };
    match n {
        0 => None,
        n if n < 0 => Some(None),
        n => Some(Some(n as usize)),
    }
}

/// One block of the submodule summary of `status` and `commit`
/// (`* <path> <old>...<new> (<count>):` and the commits, like `git
/// submodule summary`) for the submodule at `path` going from `old` to
/// `new` (`None`: added or deleted), at most `limit` commits.
pub fn status_summary(
    out: &mut Vec<u8>,
    repo: &Repo,
    path: &[u8],
    old: Option<ObjectId>,
    new: Option<ObjectId>,
    limit: Option<usize>,
) {
    let git_dir = sub_git_dir(repo, path);
    let present = |id: &ObjectId| {
        git_dir.as_ref().is_some_and(|g| {
            run_in(
                g,
                &["cat-file", "-e", &format!("{}^{{commit}}", id.to_hex())],
            )
            .is_some()
        })
    };
    let short = |id: Option<&ObjectId>| -> String {
        let Some(id) = id else {
            return "0".repeat(7);
        };
        git_dir
            .as_ref()
            .filter(|_| present(id))
            .and_then(|g| run_in(g, &["rev-parse", "--short", &id.to_hex()]))
            .and_then(|o| String::from_utf8(o).ok())
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| id.to_hex_with_len(7))
    };
    out.extend_from_slice(b"* ");
    out.extend_from_slice(path);
    out.extend_from_slice(format!(" {}...{}", short(old.as_ref()), short(new.as_ref())).as_bytes());
    let missing = [old, new].into_iter().flatten().find(|id| !present(id));
    if let Some(id) = missing {
        out.extend_from_slice(b":\n  ");
        out.extend_from_slice(
            msg::summary_missing_commit(&String::from_utf8_lossy(path), &id.to_hex()).as_bytes(),
        );
        out.extend_from_slice(b"\n\n");
        return;
    }
    let (Some(g), Some(new)) = (git_dir, new) else {
        out.extend_from_slice(b":\n\n");
        return;
    };
    let range = match old {
        Some(old) => format!("{}...{}", old.to_hex(), new.to_hex()),
        None => new.to_hex(),
    };
    let count = run_in(&g, &["rev-list", "--first-parent", &range, "--"]).map_or(0, |o| {
        o.split(|&b| b == b'\n').filter(|l| !l.is_empty()).count()
    });
    out.extend_from_slice(format!(" ({count}):\n").as_bytes());
    let mut args = vec!["log".to_owned()];
    if let Some(n) = limit {
        args.push(format!("-{n}"));
    }
    args.extend(
        [
            "--left-right",
            "--first-parent",
            "--format=  %m %s",
            &range,
            "--",
        ]
        .map(str::to_owned),
    );
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    if let Some(log) = run_in(&g, &args) {
        out.extend_from_slice(&log);
        if !log.is_empty() && !log.ends_with(b"\n") {
            out.push(b'\n');
        }
    }
    out.push(b'\n');
}

/// `path` relative to the directory `from` (both absolute and normalized).
fn relative_path(from: &Path, to: &Path) -> std::path::PathBuf {
    let f: Vec<_> = from.components().collect();
    let t: Vec<_> = to.components().collect();
    let common = f.iter().zip(&t).take_while(|(a, b)| a == b).count();
    let mut out = std::path::PathBuf::new();
    for _ in common..f.len() {
        out.push("..");
    }
    for c in &t[common..] {
        out.push(c);
    }
    out
}

/// After `git mv` moved the submodule at `src` to `dst` (index already
/// updated): point its `.git` file and the `core.worktree` of its git
/// directory to the new place, and change and stage its `path` in
/// `.gitmodules`.
pub fn moved(repo: &Repo, index: &mut Index, src: &[u8], dst: &[u8]) -> Result<(), Die> {
    use rgit_config::write::set_in_file;
    use rgit_config::{SetRequest, ValuePattern};
    let Some(root) = repo.setup.work_tree.clone() else {
        return Ok(());
    };
    let root = rgit_repo::path::normalize(&root);
    let old_dir = crate::diff::collect::os_path(&root, src);
    let new_dir = crate::diff::collect::os_path(&root, dst);
    let dot_git = new_dir.join(".git");
    if std::fs::symlink_metadata(&dot_git).is_ok_and(|m| m.is_file()) {
        if let Ok(data) = std::fs::read(&dot_git) {
            if let Some(rest) = data.strip_prefix(b"gitdir:") {
                let target = String::from_utf8_lossy(rest.trim_ascii()).into_owned();
                let target = Path::new(&target);
                let git_dir = rgit_repo::path::normalize(&if target.is_absolute() {
                    target.to_path_buf()
                } else {
                    old_dir.join(target)
                });
                let rel = relative_path(&new_dir, &git_dir);
                let _ = std::fs::write(&dot_git, format!("gitdir: {}\n", rel.display()));
                let config = git_dir.join("config");
                if config.exists() {
                    let wt = relative_path(&git_dir, &new_dir);
                    let value = wt.to_string_lossy().into_owned();
                    let req = SetRequest {
                        key: b"core.worktree",
                        value: Some(value.as_bytes()),
                        pattern: &ValuePattern::Any,
                        all: false,
                        comment: None,
                    };
                    let _ = set_in_file(&config, &Default::default(), &req);
                }
            }
        }
    }
    // `.gitmodules`
    let file = root.join(rgit_submodule::GITMODULES);
    let Ok(data) = std::fs::read(&file) else {
        return Ok(());
    };
    let origin = Origin::File(file.to_string_lossy().into_owned());
    let Ok((gm, _)) = Gitmodules::parse(&data, &origin, Mode::Current) else {
        return Ok(());
    };
    let Some(sub) = gm.by_path(src) else {
        return Ok(());
    };
    let mut key = b"submodule.".to_vec();
    key.extend_from_slice(&sub.name);
    key.extend_from_slice(b".path");
    let req = SetRequest {
        key: &key,
        value: Some(dst),
        pattern: &ValuePattern::Any,
        all: false,
        comment: None,
    };
    set_in_file(&file, &Default::default(), &req).map_err(|e| Die::Fatal(e.to_string()))?;
    stage_gitmodules(repo, index)
}

/// Add the work tree's `.gitmodules` to the index.
pub fn stage_gitmodules(repo: &Repo, index: &mut Index) -> Result<(), Die> {
    let Some(root) = repo.setup.work_tree.as_ref() else {
        return Ok(());
    };
    let file = root.join(rgit_submodule::GITMODULES);
    let data = std::fs::read(&file).map_err(|e| Die::Fatal(e.to_string()))?;
    let id = repo
        .odb()
        .write(rgit_object::ObjectKind::Blob, &data)
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let mut e = rgit_index::Entry::new(
        b".gitmodules".to_vec(),
        Stage::MERGED,
        rgit_index::Mode::FILE,
        id,
    );
    if let Ok(m) = std::fs::symlink_metadata(&file) {
        e.stat = rgit_worktree::stat::stat_from_metadata(&m);
    }
    index.remove_path(b".gitmodules");
    index
        .add(e, rgit_index::AddOptions::default())
        .map_err(|e| Die::Fatal(e.to_string()))?;
    index.invalidate_path(b".gitmodules");
    Ok(())
}

/// Move an embedded `.git` directory of the submodule at `dir` to
/// `$GIT_COMMON_DIR/modules/<name>` and leave a `.git` file pointing there
/// (with `core.worktree` set), as `git submodule absorbgitdirs` does.
/// With `shown`, the move is reported under that path.
pub fn absorb_git_dir(
    repo: &Repo,
    dir: &Path,
    name: &[u8],
    shown: Option<&[u8]>,
) -> Result<(), Die> {
    let target = rgit_submodule::gitdir::modules_dir(&repo.setup.absolute_common_dir(), name);
    move_git_dir(dir, &target, shown)?;
    if let Some(p) = shown {
        if let Some(git_dir) = rgit_submodule::gitdir::git_dir_of(dir) {
            absorb_nested(dir, &git_dir, p)?;
        }
    }
    Ok(())
}

/// Absorb the embedded `.git` directories of the submodules nested in the
/// submodule at `dir` (git directory `git_dir`, shown as `shown`) into
/// `<git_dir>/modules/<name>`, recursively.
fn absorb_nested(dir: &Path, git_dir: &Path, shown: &[u8]) -> Result<(), Die> {
    let file = dir.join(rgit_submodule::GITMODULES);
    let Ok(data) = std::fs::read(&file) else {
        return Ok(());
    };
    let origin = Origin::File(file.to_string_lossy().into_owned());
    let Ok((gm, _)) = Gitmodules::parse(&data, &origin, Mode::Current) else {
        return Ok(());
    };
    for sub in gm.iter() {
        let Some(path) = sub.path.as_ref() else {
            continue;
        };
        let sub_dir = crate::diff::collect::os_path(dir, path);
        let mut sub_shown = shown.to_vec();
        sub_shown.push(b'/');
        sub_shown.extend_from_slice(path);
        let target = rgit_submodule::gitdir::modules_dir(git_dir, &sub.name);
        move_git_dir(&sub_dir, &target, Some(&sub_shown))?;
        if let Some(g) = rgit_submodule::gitdir::git_dir_of(&sub_dir) {
            absorb_nested(&sub_dir, &g, &sub_shown)?;
        }
    }
    Ok(())
}

/// Move `<dir>/.git` (when it is a directory) to `target`, leaving a `.git`
/// file and `core.worktree` behind.
fn move_git_dir(dir: &Path, target: &Path, shown: Option<&[u8]>) -> Result<(), Die> {
    use rgit_config::write::set_in_file;
    use rgit_config::{SetRequest, ValuePattern};
    let dot_git = dir.join(".git");
    if !std::fs::symlink_metadata(&dot_git).is_ok_and(|m| m.is_dir()) {
        return Ok(());
    }
    let from =
        rgit_repo::path::normalize(&std::path::absolute(&dot_git).unwrap_or(dot_git.clone()));
    let to = rgit_repo::path::normalize(target);
    if let Some(p) = shown {
        eprintln!(
            "{}",
            msg::migrating(
                &String::from_utf8_lossy(p),
                &from.to_string_lossy(),
                &to.to_string_lossy()
            )
        );
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Die::Fatal(e.to_string()))?;
    }
    std::fs::rename(&dot_git, target).map_err(|e| {
        Die::Fatal(msg::could_not_migrate(
            &from.to_string_lossy(),
            &to.to_string_lossy(),
            &crate::global::strerror(&e),
        ))
    })?;
    let dir = rgit_repo::path::normalize(&std::path::absolute(dir).unwrap_or(dir.to_path_buf()));
    let target = rgit_repo::path::normalize(target);
    let rel = relative_path(&dir, &target);
    std::fs::write(&dot_git, format!("gitdir: {}\n", rel.display()))
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let wt = relative_path(&target, &dir).to_string_lossy().into_owned();
    let req = SetRequest {
        key: b"core.worktree",
        value: Some(wt.as_bytes()),
        pattern: &ValuePattern::Any,
        all: false,
        comment: None,
    };
    set_in_file(&target.join("config"), &Default::default(), &req)
        .map_err(|e| Die::Fatal(e.to_string()))
}

/// Remove `[submodule "<name>"]` from the work tree's `.gitmodules`;
/// whether there was one.
pub fn remove_section(repo: &Repo, name: &[u8]) -> Result<bool, Die> {
    let Some(root) = repo.setup.work_tree.as_ref() else {
        return Ok(false);
    };
    let file = root.join(rgit_submodule::GITMODULES);
    if !file.exists() {
        return Ok(false);
    }
    let mut section = b"submodule.".to_vec();
    section.extend_from_slice(name);
    match rgit_config::write::rename_section_in_file(&file, &Default::default(), &section, None) {
        Ok(()) => Ok(true),
        Err(_) => Ok(false),
    }
}

/// The prefix options for a diff inside the submodule at `path`: the
/// superproject's prefixes followed by the path.
fn prefixes(path: &[u8], (src, dst): (&[u8], &[u8])) -> (String, String) {
    let p = String::from_utf8_lossy(path);
    let (s, d) = (String::from_utf8_lossy(src), String::from_utf8_lossy(dst));
    (
        format!("--src-prefix={s}{p}/"),
        format!("--dst-prefix={d}{p}/"),
    )
}

/// Diff the submodule's work tree against `old` (`None`: the empty tree).
fn run_diff(
    out: &mut Vec<u8>,
    repo: &Repo,
    git_dir: &Path,
    path: &[u8],
    pre: (&[u8], &[u8]),
    old: Option<ObjectId>,
    new: Option<ObjectId>,
) {
    if new.is_some() {
        run_diff_trees(out, git_dir, path, pre, old, new);
        return;
    }
    let Some(wt) = repo.setup.work_tree.as_ref() else {
        return;
    };
    let dir = crate::diff::collect::os_path(wt, path);
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let (src, dst) = prefixes(path, pre);
    let base = old.map_or_else(|| empty_tree(git_dir), |o| o.to_hex());
    let mut cmd = Command::new(exe);
    prepare_env(&mut cmd);
    cmd.current_dir(&dir)
        .args(["diff", "--submodule=diff", &src, &dst, &base, "--"])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    if let Ok(o) = cmd.output() {
        out.extend_from_slice(&o.stdout);
    }
}

/// Diff two commits of the submodule (`None`: the empty tree).
fn run_diff_trees(
    out: &mut Vec<u8>,
    git_dir: &Path,
    path: &[u8],
    pre: (&[u8], &[u8]),
    old: Option<ObjectId>,
    new: Option<ObjectId>,
) {
    let (src, dst) = prefixes(path, pre);
    let a = old.map_or_else(|| empty_tree(git_dir), |o| o.to_hex());
    let b = new.map_or_else(|| empty_tree(git_dir), |o| o.to_hex());
    if let Some(o) = run_in(
        git_dir,
        &["diff", "--submodule=diff", &src, &dst, &a, &b, "--"],
    ) {
        out.extend_from_slice(&o);
    }
}

fn empty_tree(git_dir: &Path) -> String {
    let kind = if std::fs::read_to_string(git_dir.join("config"))
        .is_ok_and(|c| c.to_ascii_lowercase().contains("objectformat = sha256"))
    {
        rgit_hash::HashKind::Sha256
    } else {
        rgit_hash::HashKind::Sha1
    };
    ObjectId::empty_tree(kind).to_hex()
}
