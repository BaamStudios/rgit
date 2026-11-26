// SPDX-License-Identifier: MIT
//! `git difftool` (Documentation/git-difftool.adoc, config/difftool.adoc):
//! shows a diff with an external tool.
//!
//! Per file (the default) it runs `git diff` with
//! [`difftool--helper`](super::difftool__helper) as the external diff
//! program, the options passed on through `GIT_DIFFTOOL_*`,
//! `GIT_DIFF_TOOL` and `GIT_MERGETOOL_GUI`. With `-d` the changed files of
//! both sides are written into `left/` and `right/` of a temporary
//! directory and the tool runs once on the two directories; right-hand
//! files from the work tree are symbolic links to it (`--symlinks`, the
//! default) or copies that are written back when the tool changed them.
//! Messages and exit codes were taken from the behaviour of the `git`
//! binary.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::commands::mergetool::tools::{self, Files, Mode, Outcome};
use crate::messages::mergetool as msg;
use crate::prelude::*;
use crate::repository::Repo;

/// The environment of the helper (see [`super::difftool__helper`]).
pub(crate) const ENV_TOOL: &str = "GIT_DIFF_TOOL";
pub(crate) const ENV_EXTCMD: &str = "GIT_DIFFTOOL_EXTCMD";
pub(crate) const ENV_NO_PROMPT: &str = "GIT_DIFFTOOL_NO_PROMPT";
pub(crate) const ENV_PROMPT: &str = "GIT_DIFFTOOL_PROMPT";
pub(crate) const ENV_GUI: &str = "GIT_MERGETOOL_GUI";
pub(crate) const ENV_TRUST: &str = "GIT_DIFFTOOL_TRUST_EXIT_CODE";

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let gui = Cell::new(-1);
    let dir_diff = Cell::new(0);
    let prompt = Cell::new(-1);
    let symlinks = Cell::new(-1);
    let tool = RefCell::new(None::<String>);
    let tool_help = Cell::new(0);
    let trust = Cell::new(-1);
    let extcmd = RefCell::new(None::<String>);
    let no_index = Cell::new(0);
    let mut parser = Parser::new(
        msg::DIFFTOOL_USAGE,
        vec![
            Opt::bool('g', "gui", &gui, msg::HELP_GUI),
            Opt::bool('d', "dir-diff", &dir_diff, msg::HELP_DIR_DIFF),
            Opt::set_int('y', "no-prompt", &prompt, msg::HELP_NO_PROMPT, 0).noneg(),
            Opt::set_int(None, "prompt", &prompt, "", 1).noneg().hidden(),
            Opt::bool(None, "symlinks", &symlinks, msg::HELP_SYMLINKS),
            Opt::string('t', "tool", &tool, "tool", msg::HELP_TOOL),
            Opt::bool(None, "tool-help", &tool_help, msg::HELP_TOOL_HELP),
            Opt::bool(None, "trust-exit-code", &trust, msg::HELP_TRUST_EXIT_CODE),
            Opt::string('x', "extcmd", &extcmd, "command", msg::HELP_EXTCMD),
            Opt::set_int(None, "no-index", &no_index, msg::HELP_NO_INDEX, 1).noneg(),
            Opt::set_int(None, "index", &no_index, msg::HELP_INDEX, 0).noneg(),
        ],
    )
    .flags(ParseFlags {
        keep_dashdash: true,
        keep_unknown_opt: true,
        ..ParseFlags::default()
    });
    let mut args = parser.parse(argv)?.args;
    drop(parser);
    if tool_help.get() == 1 {
        let config = match Repo::open_gently(ctx)? {
            Some(r) => r.config.clone(),
            None => crate::repo_objects::load_config(ctx, None)?,
        };
        tools::print_help(Some(&config), Mode::Diff);
        return Ok(0);
    }
    let tool = tool.into_inner();
    let extcmd = extcmd.into_inner();
    let gui_given = gui.get() == 1;
    match (gui_given, tool.is_some(), extcmd.is_some()) {
        (true, true, true) => {
            return Err(Die::Fatal(msg::incompatible3("--gui", "--tool", "--extcmd")));
        }
        (true, true, false) => return Err(Die::Fatal(msg::incompatible2("--gui", "--tool"))),
        (true, false, true) => return Err(Die::Fatal(msg::incompatible2("--gui", "--extcmd"))),
        (false, true, true) => return Err(Die::Fatal(msg::incompatible2("--tool", "--extcmd"))),
        _ => {}
    }
    let repo = if no_index.get() == 1 {
        None
    } else {
        Repo::open_gently(ctx)?
    };
    let config = match &repo {
        Some(r) => r.config.clone(),
        None => crate::repo_objects::load_config(ctx, None)?,
    };
    let gui = match gui.get() {
        -1 => crate::commands::mergetool::gui_default(&config, "difftool.guidefault"),
        v => v == 1,
    };
    let trust = match trust.get() {
        -1 => None,
        v => Some(v == 1),
    };
    if dir_diff.get() == 1 {
        let repo = match repo {
            Some(r) => r,
            None => Repo::open(ctx, crate::repo::Mode::Required)?,
        };
        let symlinks = match symlinks.get() {
            -1 => repo.config.bool("core.symlinks").ok().flatten().unwrap_or(true),
            v => v == 1,
        };
        let d = DirDiff {
            ctx,
            repo: &repo,
            symlinks,
            gui,
            tool: tool.as_deref(),
            extcmd: extcmd.as_deref(),
            trust: trust.unwrap_or_else(|| {
                repo.config.bool("difftool.trustexitcode").ok().flatten().unwrap_or(false)
            }),
        };
        return d.run(&args);
    }
    // One file at a time, through `git diff` and the helper.
    if no_index.get() == 1 {
        args.insert(0, "--no-index".to_owned());
    }
    let exe = std::env::current_exe().map_err(|e| Die::Fatal(e.to_string()))?;
    let helper = format!(
        "{} difftool--helper",
        crate::context::sq_quote(&exe.to_string_lossy())
    );
    let mut cmd = Command::new(&exe);
    cmd.arg("diff").args(&args).envs(ctx.child_env());
    cmd.env("GIT_EXTERNAL_DIFF", helper);
    cmd.env_remove("GIT_EXTERNAL_DIFF_TRUST_EXIT_CODE");
    match prompt.get() {
        0 => {
            cmd.env(ENV_NO_PROMPT, "true");
        }
        1 => {
            cmd.env(ENV_PROMPT, "true");
        }
        _ => {}
    }
    if gui {
        cmd.env(ENV_GUI, "true");
    }
    if let Some(t) = &tool {
        cmd.env(ENV_TOOL, t);
    }
    if let Some(x) = &extcmd {
        cmd.env(ENV_EXTCMD, x);
    }
    if let Some(t) = trust {
        cmd.env(ENV_TRUST, if t { "true" } else { "false" });
    }
    let status = cmd.status().map_err(|e| Die::Fatal(e.to_string()))?;
    Ok(status.code().unwrap_or(128))
}

/// The exit code of a tool for difftool: the tool's own when it is trusted
/// or could not be run at all (126, 127), else 0.
pub(crate) fn tool_result(code: i32, trust: bool) -> i32 {
    if trust || code == 126 || code == 127 {
        code
    } else {
        0
    }
}

/// `difftool --dir-diff`.
struct DirDiff<'a> {
    ctx: &'a Context,
    repo: &'a Repo,
    symlinks: bool,
    gui: bool,
    tool: Option<&'a str>,
    extcmd: Option<&'a str>,
    trust: bool,
}

/// One changed path of `diff --raw`.
struct Change {
    old: Option<(u32, String)>,
    new: Option<(u32, String)>,
    old_path: String,
    new_path: String,
    unmerged: bool,
}

const NULL_HEX: &str = "0000000000000000000000000000000000000000";

impl DirDiff<'_> {
    fn git(&self) -> Command {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("git"));
        let mut c = Command::new(exe);
        c.envs(self.ctx.child_env())
            .env_remove("GIT_EXTERNAL_DIFF")
            .current_dir(&self.repo.setup.original_cwd);
        c
    }

    fn changes(&self, args: &[String]) -> Result<Vec<Change>, Die> {
        let out = self
            .git()
            .args(["diff", "--raw", "--no-abbrev", "-z"])
            .args(args)
            .stderr(Stdio::inherit())
            .output()
            .map_err(|e| Die::Fatal(e.to_string()))?;
        if !out.status.success() && out.status.code() != Some(1) {
            return Err(Die::Exit(out.status.code().unwrap_or(128)));
        }
        let mut fields = out.stdout.split(|&b| b == 0).map(|f| String::from_utf8_lossy(f).into_owned());
        let mut changes = Vec::new();
        while let Some(head) = fields.next() {
            let Some(head) = head.strip_prefix(':') else {
                continue;
            };
            let parts: Vec<&str> = head.split(' ').collect();
            if parts.len() < 5 {
                continue;
            }
            let side = |mode: &str, id: &str| -> Option<(u32, String)> {
                let m = u32::from_str_radix(mode, 8).unwrap_or(0);
                (m != 0).then(|| (m, id.to_owned()))
            };
            let status = parts[4];
            let p1 = fields.next().unwrap_or_default();
            let p2 = if status.starts_with(['R', 'C']) {
                fields.next().unwrap_or_default()
            } else {
                p1.clone()
            };
            changes.push(Change {
                old: side(parts[0], parts[2]),
                new: side(parts[1], parts[3]),
                old_path: p1,
                new_path: p2,
                unmerged: status.starts_with('U'),
            });
        }
        Ok(changes)
    }

    fn blob(&self, id: &str) -> Vec<u8> {
        self.git()
            .args(["cat-file", "-p", id])
            .stderr(Stdio::null())
            .output()
            .map(|o| o.stdout)
            .unwrap_or_default()
    }

    /// Writes one side of a path from the object database.
    fn write_object(&self, dir: &Path, path: &str, side: &(u32, String)) -> std::io::Result<()> {
        let target = dir.join(path);
        if let Some(p) = target.parent() {
            std::fs::create_dir_all(p)?;
        }
        let (mode, id) = side;
        let data = if *mode == 0o160000 {
            format!("Subproject commit {id}\n").into_bytes()
        } else if *mode == 0o120000 {
            // A symbolic link is written as its target, one line.
            let mut t = self.blob(id);
            t.push(b'\n');
            t
        } else {
            self.blob(id)
        };
        std::fs::write(&target, data)?;
        #[cfg(unix)]
        if *mode == 0o100755 {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
        }
        Ok(())
    }

    /// Whether the work tree file at `path` has the content of blob `id`.
    fn same_as_worktree(&self, root: &Path, path: &str, id: &str) -> bool {
        let wt = root.join(path);
        match std::fs::symlink_metadata(&wt) {
            Ok(m) if m.is_file() => std::fs::read(&wt).is_ok_and(|d| d == self.blob(id)),
            _ => false,
        }
    }

    fn run(&self, args: &[String]) -> CmdResult {
        let root = self.repo.work_tree()?;
        let changes = self.changes(args)?;
        if changes.is_empty() {
            return Ok(0);
        }
        let io = |e: std::io::Error| Die::Fatal(e.to_string());
        let tmp = tools::make_temp_dir("git-difftool.").map_err(io)?;
        let left = tmp.join("left");
        let right = tmp.join("right");
        std::fs::create_dir_all(&left).map_err(io)?;
        std::fs::create_dir_all(&right).map_err(io)?;
        // Work tree files copied to the right: their content when copied.
        let mut copied: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for c in &changes {
            if let Some(o) = c.old.as_ref().filter(|(_, id)| id != NULL_HEX) {
                if !c.unmerged {
                    self.write_object(&left, &c.old_path, o).map_err(io)?;
                }
            }
            let target = right.join(&c.new_path);
            if std::fs::symlink_metadata(&target).is_ok() {
                // An unmerged path comes more than once.
                continue;
            }
            let from_worktree = c.unmerged
                || c.new.as_ref().is_some_and(|(_, id)| id == NULL_HEX)
                || c.new.as_ref().is_some_and(|(mode, id)| {
                    *mode != 0o120000 && *mode != 0o160000 && self.same_as_worktree(&root, &c.new_path, id)
                });
            if from_worktree {
                let wt = root.join(&c.new_path);
                let Ok(meta) = std::fs::symlink_metadata(&wt) else {
                    continue;
                };
                if let Some(p) = target.parent() {
                    std::fs::create_dir_all(p).map_err(io)?;
                }
                if meta.file_type().is_symlink() {
                    let link = std::fs::read_link(&wt).map_err(io)?;
                    let mut t = link.as_os_str().as_encoded_bytes().to_vec();
                    t.push(b'\n');
                    std::fs::write(&target, t).map_err(io)?;
                } else if meta.is_file() {
                    if self.symlinks {
                        #[cfg(unix)]
                        std::os::unix::fs::symlink(&wt, &target).map_err(io)?;
                        #[cfg(not(unix))]
                        std::fs::copy(&wt, &target).map_err(io)?;
                    } else {
                        let data = std::fs::read(&wt).map_err(io)?;
                        std::fs::write(&target, &data).map_err(io)?;
                        #[cfg(unix)]
                        {
                            let _ = std::fs::set_permissions(&target, meta.permissions());
                        }
                        copied.insert(c.new_path.clone(), data);
                    }
                }
            } else if let Some(n) = &c.new {
                self.write_object(&right, &c.new_path, n).map_err(io)?;
            }
        }
        let l = format!("{}/", left.display());
        let r = format!("{}/", right.display());
        let code = self.launch(&l, &r, &root)?;
        // Changes the tool made to copies go back to the work tree.
        let mut conflict = false;
        for (path, original) in &copied {
            let tmp_file = right.join(path);
            let wt = root.join(path);
            let Ok(now) = std::fs::read(&tmp_file) else {
                continue;
            };
            if &now == original {
                continue;
            }
            let wt_now = std::fs::read(&wt).ok();
            if wt_now.as_ref() != Some(original) {
                eprintln!(
                    "warning: {}",
                    msg::both_modified(&wt.to_string_lossy(), &tmp_file.to_string_lossy())
                );
                eprintln!("warning: {}", msg::WORKTREE_LEFT);
                conflict = true;
                continue;
            }
            let _ = std::fs::write(&wt, &now);
        }
        if conflict {
            eprintln!("warning: {}", msg::temporary_files_exist(&tmp.to_string_lossy()));
            eprintln!("warning: {}", msg::MAY_CLEANUP);
            return Ok(1);
        }
        let _ = std::fs::remove_dir_all(&tmp);
        Ok(code)
    }

    /// Runs the external command or the tool on the two directories.
    fn launch(&self, left: &str, right: &str, root: &Path) -> CmdResult {
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        if let Some(x) = self.extcmd {
            return match Command::new(x).args([left, right]).current_dir(root).status() {
                Ok(s) => Ok(self.finish(s.code().unwrap_or(128))),
                Err(e) => {
                    eprintln!("error: {}", msg::cannot_run(x, &rgit_messages::strerror(&e)));
                    eprintln!("warning: {}", msg::tool_failed(-1));
                    Ok(1)
                }
            };
        }
        let explicit = self.tool.map(str::to_owned).or_else(|| std::env::var(ENV_TOOL).ok());
        let (tool, _) = tools::select(&self.repo.config, Mode::Diff, self.gui, explicit.as_deref());
        let files = Files {
            base: String::new(),
            local: left.to_owned(),
            remote: right.to_owned(),
            merged: String::new(),
            base_present: false,
        };
        match tools::run(&self.repo.config, Mode::Diff, &tool, &files, root) {
            Outcome::Ran(code) => Ok(self.finish(code)),
            Outcome::NotSet => Ok(1),
        }
    }

    /// The exit code of the run; a failed tool is reported.
    fn finish(&self, code: i32) -> i32 {
        let rc = tool_result(code, self.trust);
        if rc != 0 {
            eprintln!("warning: {}", msg::tool_failed(rc));
        }
        rc
    }
}
