// SPDX-License-Identifier: MIT
//! `git mergetool` (Documentation/git-mergetool.adoc,
//! config/mergetool.adoc): runs a merge tool on each unmerged path.
//!
//! For every path the conflicted work tree file is kept as a backup
//! (`<name>_BACKUP_<pid><ext>`, `.orig` afterwards with
//! `mergetool.keepBackup`), the three stages are written to
//! `<name>_BASE_…`, `_LOCAL_…`, `_REMOTE_…` (in a temporary directory with
//! `mergetool.writeToTemp`) and the tool runs with `BASE`, `LOCAL`,
//! `REMOTE` and `MERGED` set. Deleted, symbolic link and submodule
//! conflicts are resolved by a choice instead. The prompts, messages and
//! file names were taken black-box from the `git` binary; tools come from
//! [`tools`].

use crate::argv::CommandArgs as _;
#[path = "mergetool/tools.rs"]
pub(crate) mod tools;

use std::path::{Path, PathBuf};
use std::process::Command;

use rgit_config::ConfigSet;

use crate::messages::mergetool as msg;
use crate::prelude::*;
use crate::repo::Mode as RepoMode;
use crate::repository::Repo;
use tools::{Files, Mode, Outcome};

const MODE_LINK: u32 = 0o120000;
const MODE_GITLINK: u32 = 0o160000;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mut tool: Option<String> = None;
    let mut prompt: Option<bool> = None;
    let mut gui: Option<bool> = None;
    let mut order: Option<String> = None;
    let mut tool_help = false;
    let mut paths: Vec<String> = Vec::new();
    let mut i = 1;
    let usage = || -> CmdResult {
        eprintln!("{}", msg::MERGETOOL_USAGE);
        Ok(1)
    };
    while i < argv.len() {
        let a = argv[i].as_str();
        i += 1;
        match a {
            "-h" | "--help-all" if argv.len() == 2 => {
                println!("{}", msg::MERGETOOL_USAGE);
                return Ok(0);
            }
            "--tool-help" => tool_help = true,
            "-t" | "--tool" => match argv.get(i) {
                Some(t) => {
                    tool = Some(t.clone());
                    i += 1;
                }
                None => return usage(),
            },
            "-y" | "--no-prompt" => prompt = Some(false),
            "--prompt" => prompt = Some(true),
            "-g" | "--gui" => gui = Some(true),
            "--no-gui" => gui = Some(false),
            "--" => {
                paths.extend(argv[i..].iter().cloned());
                break;
            }
            _ => {
                if let Some(t) = a.strip_prefix("--tool=") {
                    tool = Some(t.to_owned());
                } else if let Some(t) = a.strip_prefix("-t").filter(|t| !t.is_empty()) {
                    tool = Some(t.to_owned());
                } else if let Some(o) = a.strip_prefix("-O") {
                    order = Some(o.to_owned());
                } else if a.starts_with('-') {
                    return usage();
                } else {
                    paths.push(a.to_owned());
                }
            }
        }
    }
    if tool_help {
        let repo = Repo::open_gently(ctx)?;
        let config = match &repo {
            Some(r) => r.config.clone(),
            None => crate::repo_objects::load_config(ctx, None)?,
        };
        tools::print_help(Some(&config), Mode::Merge);
        return Ok(0);
    }
    let repo = Repo::open(ctx, RepoMode::Required)?;
    let root = repo.work_tree()?;
    let config = repo.config.clone();
    let gui = gui.unwrap_or_else(|| gui_default(&config, "mergetool.guidefault"));
    let (tool, guessed) = tools::select(&config, Mode::Merge, gui, tool.as_deref());
    let prompt = if guessed {
        true
    } else {
        prompt.unwrap_or_else(|| config.bool("mergetool.prompt").ok().flatten().unwrap_or(false))
    };
    let mt = Mergetool {
        ctx,
        repo: &repo,
        root: root.clone(),
        config: &config,
        tool,
        prompt,
        keep_backup: bool_config(&config, "mergetool.keepbackup", true),
        keep_temporaries: bool_config(&config, "mergetool.keeptemporaries", false),
        write_to_temp: bool_config(&config, "mergetool.writetotemp", false),
    };
    let files = mt.unmerged_files(&paths, order.as_deref())?;
    if files.is_empty() {
        println!("{}", msg::no_files_need_merging());
        return Ok(0);
    }
    println!("{}", msg::MERGING);
    for f in &files {
        println!("{f}");
    }
    let mut rc = 0;
    for (n, f) in files.iter().enumerate() {
        println!();
        match mt.merge_file(f)? {
            Step::Done => {}
            Step::Failed => {
                rc = 1;
                if n + 1 < files.len() && !ask_continue() {
                    return Ok(1);
                }
            }
            Step::Exit => return Ok(1),
        }
    }
    Ok(rc)
}

fn bool_config(config: &ConfigSet, key: &str, default: bool) -> bool {
    config.bool(key).ok().flatten().unwrap_or(default)
}

/// `mergetool.guiDefault` / `difftool.guiDefault`: `true`, or `auto` when
/// `DISPLAY` is set.
pub(crate) fn gui_default(config: &ConfigSet, key: &str) -> bool {
    match config.string(key).ok().flatten() {
        Some(v) if v.eq_ignore_ascii_case(b"auto") => {
            std::env::var_os("DISPLAY").is_some_and(|d| !d.is_empty())
        }
        Some(_) => config.bool(key).ok().flatten().unwrap_or(false),
        None => false,
    }
}

/// "Continue merging other unresolved paths [y/n]?" until y or n.
fn ask_continue() -> bool {
    loop {
        print!("{}", msg::PROMPT_CONTINUE);
        match tools::read_answer() {
            None => return false,
            Some(a) if a.starts_with(['y', 'Y']) => return true,
            Some(a) if a.starts_with(['n', 'N']) => return false,
            Some(_) => {}
        }
    }
}

enum Step {
    Done,
    Failed,
    /// Input ended at a prompt: stop at once.
    Exit,
}

struct Mergetool<'a> {
    ctx: &'a Context,
    repo: &'a Repo,
    root: PathBuf,
    config: &'a ConfigSet,
    tool: String,
    prompt: bool,
    keep_backup: bool,
    keep_temporaries: bool,
    write_to_temp: bool,
}

/// The stages of one unmerged path: `(mode, id)` of base, local, remote.
type Stage = Option<(u32, String)>;

impl Mergetool<'_> {
    /// rgit itself, in the top of the work tree.
    fn git(&self) -> Command {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("git"));
        let mut c = Command::new(exe);
        let mut env = self.ctx.child_env();
        env.retain(|(k, _)| k != "GIT_DIR" && k != "GIT_WORK_TREE");
        c.envs(env)
            .env("GIT_DIR", self.repo.setup.absolute_git_dir())
            .env("GIT_WORK_TREE", &self.root)
            .current_dir(&self.root);
        c
    }

    fn git_ok(&self, args: &[&str]) -> bool {
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        self.git().raw_args(args).status().is_ok_and(|s| s.success())
    }

    /// The unmerged paths to work on, in diff order.
    fn unmerged_files(&self, paths: &[String], order: Option<&str>) -> Result<Vec<String>, Die> {
        // With rerere the paths it has not resolved, in diff order.
        let mut remaining: Option<Vec<String>> = None;
        if paths.is_empty() && self.rerere_enabled() {
            let out = self
                .git()
                .raw_args(["rerere", "remaining"])
                .stderr(std::process::Stdio::null())
                .output();
            if let Some(o) = out.ok().filter(|o| o.status.success()) {
                let text = String::from_utf8_lossy(&o.stdout).into_owned();
                let list: Vec<String> =
                    text.lines().filter(|l| !l.is_empty()).map(str::to_owned).collect();
                if list.is_empty() {
                    return Ok(list);
                }
                remaining = Some(list);
            }
        }
        let mut cmd = self.git();
        {
            cmd.raw_args(["diff", "--name-only", "--diff-filter=U"]);
            if let Some(o) = order {
                let p = if o.is_empty() || Path::new(o).is_absolute() {
                    o.to_owned()
                } else {
                    self.repo
                        .setup
                        .original_cwd
                        .join(o)
                        .to_string_lossy()
                        .into_owned()
                };
                cmd.raw_arg(format!("-O{p}"));
            }
            if let Some(list) = &remaining {
                cmd.raw_arg("--");
                cmd.raw_args(list.iter().map(|p| format!(":(top,literal){p}")));
            } else if !paths.is_empty() {
                cmd.raw_arg("--");
                for p in paths {
                    match self.repo.prefix_path(p.as_bytes()) {
                        Ok(full) if full.is_empty() => cmd.raw_arg(":/"),
                        Ok(full) => cmd.raw_arg(format!(":(top){}", String::from_utf8_lossy(&full))),
                        Err(()) => cmd.raw_arg(p),
                    };
                }
            }
        }
        let out = cmd
            .stderr(std::process::Stdio::inherit())
            .output()
            .map_err(|e| Die::Fatal(e.to_string()))?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        Ok(text.lines().filter(|l| !l.is_empty()).map(str::to_owned).collect())
    }

    fn rerere_enabled(&self) -> bool {
        match self.config.bool("rerere.enabled").ok().flatten() {
            Some(b) => b,
            None => self.repo.setup.absolute_git_dir().join("rr-cache").is_dir(),
        }
    }

    fn stages(&self, path: &str) -> Result<[Stage; 3], Die> {
        let (index, _) = self.repo.read_index()?;
        let mut out: [Stage; 3] = [None, None, None];
        for e in index.entries_for_path(path.as_bytes()) {
            let s = e.stage().get();
            if (1..=3).contains(&s) {
                out[usize::from(s) - 1] = Some((e.mode.0, e.id.to_hex()));
            }
        }
        Ok(out)
    }

    fn describe(&self, side: &Stage, base: bool) -> String {
        match side {
            None => msg::DESC_DELETED.to_owned(),
            Some((MODE_LINK, id)) => {
                let target = self
                    .git()
                    .raw_args(["cat-file", "blob", id])
                    .output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                    .unwrap_or_default();
                msg::desc_symlink(&target)
            }
            Some((MODE_GITLINK, id)) => msg::desc_submodule(id),
            Some(_) if base => msg::DESC_MODIFIED.to_owned(),
            Some(_) => msg::DESC_CREATED.to_owned(),
        }
    }

    fn merge_file(&self, path: &str) -> Result<Step, Die> {
        let [base, local, remote] = self.stages(path)?;
        if base.is_none() && local.is_none() && remote.is_none() {
            if self.root.join(path).exists() {
                println!("{}", msg::not_need_merging(path));
            } else {
                println!("{}", msg::file_not_found(path));
            }
            return Ok(Step::Failed);
        }
        if !tools::is_valid(self.config, Mode::Merge, &self.tool) {
            eprintln!("error: {}", msg::tool_not_set("merge", &self.tool));
            return Ok(Step::Failed);
        }
        let names = TempNames::new(path, self.write_to_temp)?;
        let is = |s: &Stage, m: u32| s.as_ref().is_some_and(|(mode, _)| *mode == m);
        let submodule = is(&local, MODE_GITLINK) || is(&remote, MODE_GITLINK);
        let symlink = is(&local, MODE_LINK) || is(&remote, MODE_LINK);
        let merged = self.root.join(path);
        let backup_made = std::fs::symlink_metadata(&merged).is_ok_and(|m| m.is_file());
        if backup_made {
            let _ = std::fs::rename(&merged, self.root.join(&names.backup));
            let _ = std::fs::copy(self.root.join(&names.backup), &merged);
        }
        if !submodule {
            self.write_stage(path, 1, &base, &names.base);
            self.write_stage(path, 2, &local, &names.local);
            self.write_stage(path, 3, &remote, &names.remote);
        }
        let deleted = local.is_none() || remote.is_none();
        let header = match (submodule, symlink, deleted) {
            (true, _, _) => msg::submodule_conflict(path),
            (false, true, _) => msg::symlink_conflict(path),
            (false, false, true) => msg::deleted_conflict(path),
            (false, false, false) => msg::normal_conflict(path),
        };
        println!("{header}");
        println!("{}", msg::side_local(&self.describe(&local, base.is_some())));
        println!("{}", msg::side_remote(&self.describe(&remote, base.is_some())));
        if submodule || symlink {
            return self.choose_side(path, &names, &local, &remote, backup_made, submodule);
        }
        if deleted {
            return self.choose_deleted(path, &names, &local, &remote, backup_made);
        }
        if self.hide_resolved() {
            self.hide(&names);
        }
        if self.prompt {
            print!("{}", msg::prompt_start_tool(&self.tool));
            if tools::read_answer().is_none() {
                return Ok(Step::Exit);
            }
        }
        let before = std::fs::metadata(&merged).and_then(|m| m.modified()).ok();
        let files = Files {
            base: names.base.clone(),
            local: names.local.clone(),
            remote: names.remote.clone(),
            merged: path.to_owned(),
            base_present: base.is_some(),
        };
        let outcome = tools::run(self.config, Mode::Merge, &self.tool, &files, &self.root);
        let ok = match outcome {
            Outcome::NotSet => {
                self.restore(&names, backup_made);
                self.cleanup(&names, false);
                return Ok(Step::Failed);
            }
            Outcome::Ran(code) => {
                if tools::trust_exit_code(self.config, Mode::Merge, &self.tool) {
                    code == 0
                } else {
                    let after = std::fs::metadata(&merged).and_then(|m| m.modified()).ok();
                    if after.is_some() && after != before {
                        true
                    } else {
                        println!("{}", msg::seems_unchanged(path));
                        ask_success()
                    }
                }
            }
        };
        if !ok {
            eprintln!("{}", msg::merge_failed(path));
            self.restore(&names, backup_made);
            self.cleanup(&names, true);
            return Ok(Step::Failed);
        }
        self.git_ok(&["add", "--", path]);
        self.finish_backup(path, &names, backup_made);
        self.cleanup(&names, false);
        Ok(Step::Done)
    }

    fn hide_resolved(&self) -> bool {
        let key = format!("mergetool.{}.hideresolved", self.tool);
        self.config
            .bool(&key)
            .ok()
            .flatten()
            .unwrap_or_else(|| bool_config(self.config, "mergetool.hideresolved", false))
    }

    /// `mergetool.hideResolved`: LOCAL and REMOTE get everything that
    /// merges cleanly, so only the conflicts differ.
    fn hide(&self, n: &TempNames) {
        for (favor, target) in [("--ours", &n.local), ("--theirs", &n.remote)] {
            if let Ok(out) = self
                .git()
                .raw_args(["merge-file", favor, "-p", &n.local, &n.base, &n.remote])
                .output()
            {
                let _ = std::fs::write(self.root.join(target), out.stdout);
            }
        }
    }

    /// Writes stage `stage` (or an empty file) to `name`.
    fn write_stage(&self, path: &str, stage: u8, side: &Stage, name: &str) {
        let target = self.root.join(name);
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if side.is_some() {
            let out = self
                .git()
                .raw_args(["checkout-index", "--temp", &format!("--stage={stage}"), "--", path])
                .output();
            if let Ok(o) = out {
                let text = String::from_utf8_lossy(&o.stdout);
                if let Some(tmp) = text.split('\t').next().filter(|t| !t.is_empty()) {
                    let tmp = self.root.join(tmp.trim());
                    // A temporary directory may be on another file system.
                    if std::fs::rename(&tmp, &target).is_ok()
                        || std::fs::copy(&tmp, &target).is_ok_and(|_| std::fs::remove_file(&tmp).is_ok())
                    {
                        return;
                    }
                }
            }
        }
        let _ = std::fs::write(&target, b"");
    }

    /// The backup back in place of the work tree file.
    fn restore(&self, n: &TempNames, backup_made: bool) {
        if backup_made {
            let _ = std::fs::rename(self.root.join(&n.backup), self.root.join(&n.merged));
        }
    }

    /// After a resolution: the backup becomes `<path>.orig`
    /// (`mergetool.keepBackup`) or goes.
    fn finish_backup(&self, path: &str, n: &TempNames, backup_made: bool) {
        if !backup_made {
            return;
        }
        let backup = self.root.join(&n.backup);
        if self.keep_backup {
            let _ = std::fs::rename(&backup, self.root.join(format!("{path}.orig")));
        } else {
            let _ = std::fs::remove_file(&backup);
        }
    }

    /// Removes BASE, LOCAL and REMOTE (kept after a failure with
    /// `mergetool.keepTemporaries`).
    fn cleanup(&self, n: &TempNames, failed: bool) {
        if failed && self.keep_temporaries {
            return;
        }
        for f in [&n.base, &n.local, &n.remote, &n.backup] {
            let _ = std::fs::remove_file(self.root.join(f));
        }
        // Directories made for the temporary files go when empty.
        let mut dir = Path::new(&n.merged).parent();
        while let Some(d) = dir {
            if d.as_os_str().is_empty() || std::fs::remove_dir(self.root.join(d)).is_err() {
                break;
            }
            dir = d.parent();
        }
        if let Some(d) = &n.dir {
            let _ = std::fs::remove_dir(d);
        }
    }

    /// Deleted on one side: keep the modified file or delete it.
    fn choose_deleted(
        &self,
        path: &str,
        n: &TempNames,
        local: &Stage,
        remote: &Stage,
        backup_made: bool,
    ) -> Result<Step, Die> {
        loop {
            print!("{}", msg::PROMPT_DELETED);
            let Some(ans) = tools::read_answer() else {
                // The temporary files stay at the end of the input.
                return Ok(Step::Failed);
            };
            match ans.chars().next() {
                Some('m' | 'M' | 'c' | 'C') if local.is_some() || remote.is_some() => {
                    if local.is_none() {
                        self.git_ok(&["checkout-index", "-f", "--stage=3", "--", path]);
                    }
                    self.git_ok(&["add", "--", path]);
                    self.finish_backup(path, n, backup_made);
                    self.cleanup(n, false);
                    return Ok(Step::Done);
                }
                Some('m' | 'M' | 'c' | 'C' | 'd' | 'D') => {
                    self.remove_path(path);
                    if backup_made {
                        let _ = std::fs::remove_file(self.root.join(&n.backup));
                    }
                    self.cleanup(n, false);
                    return Ok(Step::Done);
                }
                Some('a' | 'A') => {
                    // The work tree file is still the backup's copy.
                    self.cleanup(n, true);
                    return Ok(Step::Failed);
                }
                _ => {}
            }
        }
    }

    /// `git rm` of the path, quietly, and the directories it leaves empty.
    fn remove_path(&self, path: &str) {
        let _ = self
            .git()
            .raw_args(["rm", "-q", "--", path])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let _ = std::fs::remove_file(self.root.join(path));
        let mut dir = Path::new(path).parent();
        while let Some(d) = dir {
            if d.as_os_str().is_empty() || std::fs::remove_dir(self.root.join(d)).is_err() {
                break;
            }
            dir = d.parent();
        }
    }

    /// Symbolic link and submodule conflicts: take one side.
    fn choose_side(
        &self,
        path: &str,
        n: &TempNames,
        local: &Stage,
        remote: &Stage,
        backup_made: bool,
        submodule: bool,
    ) -> Result<Step, Die> {
        loop {
            print!("{}", msg::PROMPT_LOCAL_REMOTE);
            let Some(ans) = tools::read_answer() else {
                // The temporary files stay at the end of the input.
                return Ok(Step::Failed);
            };
            let (stage, side) = match ans.chars().next() {
                Some('l' | 'L') => (2, local),
                Some('r' | 'R') => (3, remote),
                Some('a' | 'A') => {
                    // The work tree file is still the backup's copy.
                    self.cleanup(n, true);
                    return Ok(Step::Failed);
                }
                _ => continue,
            };
            match side {
                None => {
                    let full = self.root.join(path);
                    if submodule && full.is_dir() {
                        // The submodule's directory is kept aside.
                        let _ = std::fs::rename(&full, self.root.join(format!("{path}.orig")));
                    }
                    self.remove_path(path);
                }
                Some((MODE_GITLINK, id)) => {
                    self.git_ok(&["update-index", "--force-remove", "--", path]);
                    let info = format!("160000,{id},{path}");
                    self.git_ok(&["update-index", "--add", "--cacheinfo", &info]);
                }
                Some(_) => {
                    let full = self.root.join(path);
                    if full.is_dir() && !full.is_symlink() {
                        let _ = std::fs::rename(&full, self.root.join(format!("{path}.orig")));
                    }
                    let st = format!("--stage={stage}");
                    self.git_ok(&["checkout-index", "-f", &st, "--", path]);
                    self.git_ok(&["add", "--", path]);
                }
            }
            if backup_made {
                let _ = std::fs::remove_file(self.root.join(&n.backup));
            }
            self.cleanup(n, false);
            return Ok(Step::Done);
        }
    }
}

/// "Was the merge successful [y/n]?" until y or n; end of input is no.
fn ask_success() -> bool {
    loop {
        print!("{}", msg::PROMPT_SUCCESS);
        match tools::read_answer() {
            None => return false,
            Some(a) if a.starts_with(['y', 'Y']) => return true,
            Some(a) if a.starts_with(['n', 'N']) => return false,
            Some(_) => {}
        }
    }
}

/// The temporary file names of one path, relative to the top of the work
/// tree (or absolute, in a temporary directory).
struct TempNames {
    merged: String,
    backup: String,
    base: String,
    local: String,
    remote: String,
    dir: Option<PathBuf>,
}

impl TempNames {
    fn new(path: &str, to_temp: bool) -> Result<TempNames, Die> {
        let pid = std::process::id();
        let (dir, file) = match path.rfind('/') {
            Some(i) => (&path[..=i], &path[i + 1..]),
            None => ("", path),
        };
        let (stem, ext) = match file.rfind('.') {
            Some(i) if i > 0 => (&file[..i], &file[i..]),
            _ => (file, ""),
        };
        let name = |kind: &str| format!("{stem}_{kind}_{pid}{ext}");
        let backup = format!("./{dir}{}", name("BACKUP"));
        if to_temp {
            let d = tools::make_temp_dir("git-mergetool-").map_err(|e| Die::Fatal(e.to_string()))?;
            let at = |kind: &str| d.join(name(kind)).to_string_lossy().into_owned();
            return Ok(TempNames {
                merged: path.to_owned(),
                backup,
                base: at("BASE"),
                local: at("LOCAL"),
                remote: at("REMOTE"),
                dir: Some(d),
            });
        }
        let at = |kind: &str| format!("./{dir}{}", name(kind));
        Ok(TempNames {
            merged: path.to_owned(),
            backup,
            base: at("BASE"),
            local: at("LOCAL"),
            remote: at("REMOTE"),
            dir: None,
        })
    }
}
