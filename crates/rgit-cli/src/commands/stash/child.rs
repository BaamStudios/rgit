// SPDX-License-Identifier: MIT
//! Running rgit's own commands as child processes, the way the stash
//! steps are described in git-stash(1) (`reset --hard`, `clean`, `status`,
//! a temporary index for the untracked files, ...).
//!
//! Children run at the top of the work tree with the repository named
//! explicitly (`GIT_DIR`, `GIT_WORK_TREE`), so the caller's current
//! directory and discovery do not matter.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::prelude::*;
use crate::repository::Repo;

/// The environment for child commands of one stash operation.
pub(crate) struct Git {
    exe: PathBuf,
    env: Vec<(OsString, OsString)>,
    dir: PathBuf,
}

/// Where a child's standard output goes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Out {
    Inherit,
    Capture,
}

/// One child command, built with [`Git::cmd`].
pub(crate) struct Call<'a> {
    git: &'a Git,
    args: Vec<OsString>,
    index: Option<PathBuf>,
    dir: Option<PathBuf>,
    env: Vec<(OsString, OsString)>,
    stdin: Option<Vec<u8>>,
    stdout: Out,
    capture_stderr: bool,
}

impl Git {
    pub(crate) fn new(ctx: &Context, repo: &Repo) -> Result<Git, Die> {
        let exe = std::env::current_exe().map_err(|e| Die::Fatal(e.to_string()))?;
        let mut env = ctx.child_env();
        // A git directory given on the command line may be relative to the
        // directory the command started in; children run at the top.
        if ctx.git_dir.is_some() || ctx.work_tree.is_some() {
            let git_dir = repo.setup.cwd.join(&repo.setup.git_dir);
            env.retain(|(k, _)| k != "GIT_DIR" && k != "GIT_WORK_TREE");
            env.push(("GIT_DIR".into(), git_dir.into_os_string()));
            if let Some(wt) = &repo.setup.work_tree {
                env.push(("GIT_WORK_TREE".into(), wt.clone().into_os_string()));
            }
        }
        let dir = repo
            .setup
            .work_tree
            .clone()
            .unwrap_or_else(|| repo.setup.cwd.clone());
        Ok(Git { exe, env, dir })
    }

    /// A child running `rgit <args>`.
    pub(crate) fn cmd<S: AsRef<std::ffi::OsStr>>(&self, args: &[S]) -> Call<'_> {
        Call {
            git: self,
            args: args.iter().map(|a| a.as_ref().to_owned()).collect(),
            index: None,
            dir: None,
            env: Vec::new(),
            stdin: None,
            stdout: Out::Inherit,
            capture_stderr: false,
        }
    }
}

impl Call<'_> {
    /// Use `path` as the index (`GIT_INDEX_FILE`).
    pub(crate) fn index(mut self, path: &Path) -> Self {
        self.index = Some(path.to_path_buf());
        self
    }

    /// Run in `dir` instead of the top of the work tree.
    pub(crate) fn in_dir(mut self, dir: &Path) -> Self {
        self.dir = Some(dir.to_path_buf());
        self
    }

    pub(crate) fn env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub(crate) fn stdin(mut self, data: Vec<u8>) -> Self {
        self.stdin = Some(data);
        self
    }

    fn command(&self) -> Command {
        let mut c = Command::new(&self.git.exe);
        c.args(&self.args)
            .current_dir(self.dir.as_ref().unwrap_or(&self.git.dir))
            .envs(self.git.env.iter().map(|(k, v)| (k, v)))
            .envs(self.env.iter().map(|(k, v)| (k, v)));
        if let Some(i) = &self.index {
            c.env("GIT_INDEX_FILE", i);
        }
        c.stdin(if self.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        c.stdout(match self.stdout {
            Out::Inherit => Stdio::inherit(),
            Out::Capture => Stdio::piped(),
        });
        if self.capture_stderr {
            c.stderr(Stdio::piped());
        }
        c
    }

    fn spawn_wait(self) -> Result<(i32, Vec<u8>), Die> {
        let (code, out, _) = self.spawn_wait_all()?;
        Ok((code, out))
    }

    fn spawn_wait_all(self) -> Result<(i32, Vec<u8>, Vec<u8>), Die> {
        let _ = std::io::stdout().flush();
        let mut child = self
            .command()
            .spawn()
            .map_err(|e| Die::Fatal(e.to_string()))?;
        if let Some(data) = &self.stdin {
            if let Some(mut input) = child.stdin.take() {
                let _ = input.write_all(data);
            }
        }
        let out = child
            .wait_with_output()
            .map_err(|e| Die::Fatal(e.to_string()))?;
        Ok((out.status.code().unwrap_or(128), out.stdout, out.stderr))
    }

    /// Runs the child with standard output passed through and returns
    /// its exit code and standard error.
    pub(crate) fn status_stderr(mut self) -> Result<(i32, Vec<u8>), Die> {
        self.capture_stderr = true;
        let (code, _, err) = self.spawn_wait_all()?;
        Ok((code, err))
    }

    /// Runs the child and returns its exit code.
    pub(crate) fn status(self) -> Result<i32, Die> {
        Ok(self.spawn_wait()?.0)
    }

    /// Runs the child and returns its exit code and standard output.
    pub(crate) fn output(mut self) -> Result<(i32, Vec<u8>), Die> {
        if self.stdout == Out::Inherit {
            self.stdout = Out::Capture;
        }
        self.spawn_wait()
    }
}

/// A temporary index file in the git directory, removed when dropped.
pub(crate) struct TempIndex {
    pub(crate) path: PathBuf,
}

impl TempIndex {
    pub(crate) fn new(repo: &Repo, name: &str) -> TempIndex {
        let dir = repo.setup.cwd.join(&repo.setup.git_dir);
        let path = dir.join(format!("{name}.{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        TempIndex { path }
    }
}

impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
