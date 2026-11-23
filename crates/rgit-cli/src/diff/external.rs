// SPDX-License-Identifier: MIT
//! External diff programs: `GIT_EXTERNAL_DIFF`, `diff.external` and
//! `diff.<driver>.command` (Documentation/git.adoc, gitattributes(5)).
//!
//! Interface as documented and checked against the `git` binary: the
//! program runs in the top directory of the work tree with the arguments
//! `<path> <old-file> <old-hex> <old-mode> <new-file> <new-hex>
//! <new-mode>`, plus `<new-path> <header>` for renames and copies. A
//! missing side is `/dev/null . .`; a side in the work tree is passed as
//! its path, other contents as a temporary file named like the path.
//! `GIT_DIFF_PATH_COUNTER` and `GIT_DIFF_PATH_TOTAL` count the pairs. An
//! unmerged path gets the path as its only argument. Without
//! `trustExitCode` any exit code but 0 is fatal; with it, 1 means "the
//! files differ" and 0 "they do not".

use std::process::Stdio;

use rgit_config::ConfigSet;
use rgit_hash::ObjectId;

use super::driver::TempFile;
use crate::Die;
use crate::messages::userdiff as msg;

/// A program and whether its exit code tells about differences.
#[derive(Debug, Clone)]
pub struct Program {
    pub cmd: String,
    pub trust_exit_code: bool,
}

fn env_bool(name: &str) -> Option<bool> {
    let v = std::env::var_os(name)?;
    rgit_config::value::parse_bool(Some(v.as_encoded_bytes()))
}

/// `GIT_EXTERNAL_DIFF` (with `GIT_EXTERNAL_DIFF_TRUST_EXIT_CODE`), else
/// `diff.external` (with `diff.trustExitCode`).
pub fn global(config: Option<&ConfigSet>) -> Option<Program> {
    if let Some(cmd) = std::env::var_os("GIT_EXTERNAL_DIFF").filter(|v| !v.is_empty()) {
        return Some(Program {
            cmd: cmd.to_string_lossy().into_owned(),
            trust_exit_code: env_bool("GIT_EXTERNAL_DIFF_TRUST_EXIT_CODE").unwrap_or(false),
        });
    }
    let config = config?;
    let cmd = config.string("diff.external").ok().flatten()?.to_string();
    Some(Program {
        cmd,
        trust_exit_code: config
            .bool("diff.trustexitcode")
            .ok()
            .flatten()
            .unwrap_or(false),
    })
}

/// One side of a pair for the program.
pub struct ExtSide<'a> {
    pub data: Vec<u8>,
    pub oid: ObjectId,
    pub mode: u32,
    /// The side is the work tree file `file`.
    pub in_worktree: bool,
    pub path: &'a [u8],
    /// The work tree file, relative to where the program runs.
    pub file: Vec<u8>,
}

/// A call of the program.
pub struct Call<'a> {
    pub path: &'a [u8],
    /// `None` for an unmerged path.
    pub sides: Option<(Option<ExtSide<'a>>, Option<ExtSide<'a>>)>,
    /// New path and extended header of a rename or copy.
    pub rename: Option<(&'a [u8], String)>,
    pub counter: usize,
    pub total: usize,
}

pub fn os(b: &[u8]) -> std::ffi::OsString {
    // Paths are bytes; on Unix they convert losslessly.
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::ffi::OsStr::from_bytes(b).to_os_string()
    }
    #[cfg(not(unix))]
    {
        std::ffi::OsString::from(String::from_utf8_lossy(b).into_owned())
    }
}

/// Run `prog` for one pair in `cwd`; returns whether the files differ.
/// With `quiet` the output is discarded; `stdout` is where it goes
/// instead of our standard output.
pub fn run(
    prog: &Program,
    call: &Call<'_>,
    cwd: Option<&std::path::Path>,
    quiet: bool,
    stdout: Option<std::fs::File>,
) -> Result<bool, Die> {
    let died = || Die::Fatal(msg::external_diff_died(&String::from_utf8_lossy(call.path)));
    let mut args: Vec<std::ffi::OsString> = vec![os(call.path)];
    let mut temps = Vec::new();
    if let Some((old, new)) = &call.sides {
        for side in [old, new] {
            match side {
                None => {
                    args.push("/dev/null".into());
                    args.push(".".into());
                    args.push(".".into());
                }
                Some(s) => {
                    if s.in_worktree {
                        args.push(os(&s.file));
                    } else {
                        let t = TempFile::new(&s.data, s.path).map_err(|_| died())?;
                        args.push(t.path.clone().into_os_string());
                        temps.push(t);
                    }
                    args.push(s.oid.to_hex().into());
                    args.push(format!("{:06o}", s.mode).into());
                }
            }
        }
        if let Some((new_path, header)) = &call.rename {
            args.push(os(new_path));
            if !header.is_empty() {
                args.push(header.into());
            }
        }
    }
    let arg_refs: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_os_str()).collect();
    let mut cmd = rgit_ident::editor::shell_command(&prog.cmd, &arg_refs);
    cmd.env("GIT_DIFF_PATH_COUNTER", call.counter.to_string())
        .env("GIT_DIFF_PATH_TOTAL", call.total.to_string())
        .stdin(Stdio::null());
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    if quiet {
        cmd.stdout(Stdio::null());
    } else if let Some(f) = stdout {
        cmd.stdout(f);
    }
    let status = cmd.status().map_err(|_| died())?;
    drop(temps);
    match (status.code(), prog.trust_exit_code) {
        (Some(0), true) => Ok(false),
        (Some(1), true) | (Some(0), false) => Ok(true),
        _ => Err(died()),
    }
}
