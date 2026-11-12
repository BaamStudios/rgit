// SPDX-License-Identifier: MIT
//! Dashed external commands: `git foo` runs `git-foo` from the exec path,
//! then from `$PATH`, with the exec path prepended to the child's `PATH`
//! and the global options passed on through the environment.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{CmdResult, Context, Die};

/// Is `p` a regular file we may execute?
fn is_executable(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Find `git-<name>`: the exec path first, then each `$PATH` entry.
pub fn find(ctx: &Context, name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains('/') {
        return None;
    }
    let file = format!("git-{name}");
    let mut dirs = vec![ctx.exec_path.clone()];
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path).map(|d| {
            if d.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                d
            }
        }));
    }
    dirs.into_iter()
        .map(|d| d.join(&file))
        .find(|p| is_executable(p))
}

/// Run the external command `argv[0]`; `None` if there is no such program.
pub fn run(ctx: &Context, argv: &[String]) -> Option<CmdResult> {
    let prog = find(ctx, &argv[0])?;
    let mut cmd = Command::new(&prog);
    cmd.args(&argv[1..]);
    // The child sees `argv[0]` as `git-<name>`, like an execvp of it.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.arg0(format!("git-{}", argv[0]));
    }
    for (k, v) in ctx.child_env() {
        cmd.env(k, v);
    }
    let status = match cmd.status() {
        Ok(s) => s,
        Err(e) => {
            return Some(Err(Die::Fatal(format!("cannot run git-{}: {e}", argv[0]))));
        }
    };
    if let Some(code) = status.code() {
        return Some(Ok(code));
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            // git stays quiet about SIGINT and SIGPIPE.
            if sig != libc::SIGINT && sig != libc::SIGPIPE {
                let msg = crate::messages::died_of_signal(&format!("git-{}", argv[0]), sig);
                return Some(Err(Die::Error(msg, 128 + sig)));
            }
            return Some(Ok(128 + sig));
        }
    }
    Some(Ok(128))
}
