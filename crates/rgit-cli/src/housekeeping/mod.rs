// SPDX-License-Identifier: MIT
//! Shared parts of the housekeeping commands `repack`, `prune`,
//! `prune-packed` and `gc`: which objects are reachable
//! ([`reach`]), the loose objects and packs of the repository
//! ([`store`]), writing a pack into `objects/pack` ([`store::write_pack`])
//! and the catalog files of `update-server-info` ([`server_info`]).

use crate::argv::CommandArgs as _;
pub mod cruft;
pub mod reach;
pub mod server_info;
pub mod stdin_packs;
pub mod store;

use std::time::{SystemTime, UNIX_EPOCH};

/// The objects named by the `gc.recentObjectsHook` commands
/// (`config/gc.adoc`): each runs through the shell and prints one object
/// id per line; those that exist count as recent, whatever their age. A
/// failing hook or a line that is not an object id is fatal.
pub fn recent_objects_hook(
    config: &rgit_config::ConfigSet,
    odb: &rgit_odb::ObjectDatabase,
) -> Result<Vec<rgit_hash::ObjectId>, crate::Die> {
    let mut out = Vec::new();
    for entry in config.get_all("gc.recentobjectshook") {
        let Ok(cmd) = entry.string() else { continue };
        let cmd = cmd.to_string();
        let result = std::process::Command::new("sh")
            .raw_arg("-c")
            .raw_arg(&cmd)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .output()
            .map_err(|e| crate::Die::Fatal(e.to_string()))?;
        if !result.status.success() {
            return Err(crate::Die::Fatal(
                crate::messages::housekeeping::hook_failed(&cmd),
            ));
        }
        for line in String::from_utf8_lossy(&result.stdout).lines() {
            let id = rgit_hash::ObjectId::from_hex(odb.hash_kind(), line.trim().as_bytes())
                .map_err(|_| {
                    crate::Die::Fatal(crate::messages::housekeeping::hook_bad_line(line))
                })?;
            if odb.contains(&id) {
                out.push(id);
            }
        }
    }
    Ok(out)
}

/// Run `git pack-objects <args>` as a child process of the same binary
/// (traced like git's child processes), feeding it `input` and returning
/// the lines it printed (the names of the packs written). Its standard
/// error is passed through; when it fails, its exit code is returned as
/// the error, as it has reported the reason already.
pub fn run_pack_objects(
    ctx: &crate::Context,
    setup: &rgit_repo::Setup,
    args: &[String],
    input: &str,
) -> Result<Vec<String>, crate::Die> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe().map_err(|e| crate::Die::Fatal(e.to_string()))?;
    let mut argv: Vec<String> = vec!["pack-objects".into()];
    argv.extend(args.iter().cloned());
    let mut cmd = Command::new(exe);
    cmd.raw_args(&argv).current_dir(&setup.cwd);
    for (k, v) in ctx.child_env() {
        cmd.env(k, v);
    }
    cmd.env("GIT_DIR", setup.absolute_git_dir());
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let full_argv = rgit_trace::trace2::git_argv(&argv);
    let traced = rgit_trace::trace2::child_start(Default::default(), &full_argv);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            if let Some(t) = traced {
                t.exit(-1, -1);
            }
            return Err(crate::Die::Fatal(e.to_string()));
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    let pid = child.id();
    let out = child
        .wait_with_output()
        .map_err(|e| crate::Die::Fatal(e.to_string()))?;
    if let Some(t) = traced {
        t.exit_status(pid, out.status);
    }
    if !out.status.success() {
        return Err(crate::Die::Exit(out.status.code().unwrap_or(128)));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect())
}

/// Seconds since the epoch of a file time (0 when unknown).
pub fn secs(t: std::io::Result<SystemTime>) -> u64 {
    t.ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}
