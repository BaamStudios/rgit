// SPDX-License-Identifier: MIT
//! `git merge-index [-o] [-q] <merge-program> (-a | [--] <file>...)`
//! (Documentation/git-merge-index.adoc): runs the merge program for each
//! unmerged path with the blob ids of stages 1 to 3, the path and the
//! three modes (empty for a missing stage).
//!
//! Details taken from the behaviour of the `git` binary: only `-o` and `-q`
//! are options before the program (anything else is the program); after
//! it, `-a` merges every unmerged path and may be mixed with paths, `--`
//! ends the options; paths are index paths, not relative to the current
//! directory; the program runs in the top of the work tree with
//! `GIT_PREFIX` set; `git-merge-one-file` is rgit's own
//! [`merge-one-file`](super::merge_one_file). A failed program stops the
//! run (`fatal: merge program failed`) unless `-o`; `-q` drops that
//! message and exits with the number of failures.

use crate::argv::CommandArgs as _;
use std::process::Command;

use rgit_index::Entry;

use crate::messages::merge as mmsg;
use crate::messages::merge_plumbing as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    if argv.len() == 2 && matches!(argv[1].as_str(), "-h" | "--help-all") {
        println!("{}", msg::MERGE_INDEX_USAGE);
        return Ok(0);
    }
    if argv.len() < 3 {
        eprintln!("{}", msg::MERGE_INDEX_USAGE);
        return Ok(129);
    }
    let repo = Repo::open(ctx, Mode::Required)?;
    let (index, _) = repo.read_index()?;
    let mut i = 1;
    let mut one_shot = false;
    let mut quiet = false;
    while i < argv.len() {
        match argv[i].as_str() {
            "-o" => one_shot = true,
            "-q" => quiet = true,
            _ => break,
        }
        i += 1;
    }
    let Some(program) = argv.get(i) else {
        eprintln!("{}", msg::MERGE_INDEX_USAGE);
        return Ok(129);
    };
    let runner = Runner::new(ctx, &repo, program);
    let mut state = State {
        one_shot,
        quiet,
        errors: 0,
    };
    let mut force_file = false;
    for arg in &argv[i + 1..] {
        if !force_file && arg.starts_with('-') {
            match arg.as_str() {
                "--" => force_file = true,
                "-a" => {
                    let entries = index.entries();
                    let mut k = 0;
                    while k < entries.len() {
                        let n = entries[k..]
                            .iter()
                            .take_while(|e| e.path() == entries[k].path())
                            .count();
                        let group = &entries[k..k + n];
                        k += n;
                        if group[0].stage().is_conflict() {
                            state.merge(&runner, group)?;
                        }
                    }
                }
                _ => return Err(Die::Fatal(msg::merge_index_unknown_option(arg))),
            }
            continue;
        }
        let group = index.entries_for_path(arg.as_bytes());
        if group.is_empty() {
            return Err(Die::Fatal(msg::merge_index_not_in_cache(arg)));
        }
        if group[0].stage().is_conflict() {
            state.merge(&runner, group)?;
        }
    }
    if state.errors > 0 && !state.quiet {
        return Err(Die::Fatal(mmsg::MERGE_PROGRAM_FAILED.to_owned()));
    }
    Ok(state.errors)
}

struct State {
    one_shot: bool,
    quiet: bool,
    errors: i32,
}

impl State {
    /// Runs the program for the stages of one path.
    fn merge(&mut self, runner: &Runner, group: &[Entry]) -> Result<(), Die> {
        let mut ids = [String::new(), String::new(), String::new()];
        let mut modes = [String::new(), String::new(), String::new()];
        for e in group {
            let s = e.stage().get();
            if (1..=3).contains(&s) {
                ids[usize::from(s) - 1] = e.id.to_hex();
                modes[usize::from(s) - 1] = format!("{:o}", e.mode.0);
            }
        }
        let path = String::from_utf8_lossy(group[0].path()).into_owned();
        let args = [
            ids[0].as_str(),
            &ids[1],
            &ids[2],
            &path,
            &modes[0],
            &modes[1],
            &modes[2],
        ];
        if runner.run(&args) {
            return Ok(());
        }
        if !self.one_shot {
            if !self.quiet {
                return Err(Die::Fatal(mmsg::MERGE_PROGRAM_FAILED.to_owned()));
            }
            return Err(Die::Exit(1));
        }
        self.errors += 1;
        Ok(())
    }
}

struct Runner {
    program: String,
    env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    dir: std::path::PathBuf,
}

impl Runner {
    fn new(ctx: &Context, repo: &Repo, program: &str) -> Runner {
        let mut env = ctx.child_env();
        let prefix = String::from_utf8_lossy(&repo.prefix()).into_owned();
        env.push(("GIT_PREFIX".into(), prefix.into()));
        let dir = repo
            .setup
            .work_tree
            .clone()
            .unwrap_or_else(|| repo.setup.cwd.clone());
        if ctx.git_dir.is_some() || std::env::var_os("GIT_DIR").is_some() {
            // The program runs elsewhere: the git directory must not be
            // relative.
            env.retain(|(k, _)| k != "GIT_DIR");
            env.push(("GIT_DIR".into(), repo.setup.absolute_git_dir().into_os_string()));
        }
        Runner {
            program: program.to_owned(),
            env,
            dir,
        }
    }

    /// Runs the program; whether it succeeded.
    fn run(&self, args: &[&str]) -> bool {
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        let mut cmd = if self.program == "git-merge-one-file" {
            let exe = std::env::current_exe().unwrap_or_else(|_| "git".into());
            let mut c = Command::new(exe);
            c.raw_arg("merge-one-file");
            c
        } else {
            Command::new(&self.program)
        };
        cmd.raw_args(args)
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&self.dir);
        match cmd.status() {
            Ok(s) => s.success(),
            Err(e) => {
                let reason = rgit_messages::strerror(&e);
                if self.program.contains('/') {
                    eprintln!("fatal: {}", msg::cannot_exec(&self.program, &reason));
                } else {
                    eprintln!("error: {}", msg::cannot_run(&self.program, &reason));
                }
                false
            }
        }
    }
}
