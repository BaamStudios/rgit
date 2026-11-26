// SPDX-License-Identifier: MIT
//! `git difftool--helper`: the external diff program `git difftool` runs
//! through `git diff` (`GIT_EXTERNAL_DIFF`). Called with the arguments of
//! an external diff (`<path> <old-file> <old-hex> <old-mode> <new-file>
//! <new-hex> <new-mode>`), it asks before each file (`Viewing (n/m)`)
//! unless told not to and runs `GIT_DIFFTOOL_EXTCMD` or the diff tool with
//! `LOCAL`, `REMOTE`, `MERGED` and `BASE`. A failed tool stops the diff
//! when its exit code is trusted (or it could not be run at all). Like
//! git's, it is not listed as a built-in command.

use crate::commands::difftool::{
    ENV_EXTCMD, ENV_GUI, ENV_NO_PROMPT, ENV_PROMPT, ENV_TOOL, ENV_TRUST, tool_result,
};
use crate::commands::mergetool::tools::{self, Files, Mode, Outcome};
use crate::messages::mergetool as msg;
use crate::prelude::*;
use crate::repository::Repo;

fn env_true(name: &str) -> Option<bool> {
    std::env::var_os(name)
        .and_then(|v| rgit_config::value::parse_bool(Some(v.as_encoded_bytes())))
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let args = &argv[1..];
    if args.len() < 7 {
        // An unmerged path: nothing to show.
        return Ok(0);
    }
    let config = match Repo::open_gently(ctx)? {
        Some(r) => r.config.clone(),
        None => crate::repo_objects::load_config(ctx, None)?,
    };
    let path = &args[0];
    let local = &args[1];
    let remote = &args[4];
    let merged = if args.len() >= 8 { &args[7] } else { path };
    let prompt = if env_true(ENV_NO_PROMPT) == Some(true) {
        false
    } else if env_true(ENV_PROMPT) == Some(true) {
        true
    } else {
        let b = |k: &str| config.bool(k).ok().flatten();
        b("difftool.prompt").or_else(|| b("mergetool.prompt")).unwrap_or(true)
    };
    let extcmd = std::env::var(ENV_EXTCMD).ok().filter(|x| !x.is_empty());
    let trust = env_true(ENV_TRUST).unwrap_or_else(|| {
        config.bool("difftool.trustexitcode").ok().flatten().unwrap_or(false)
    });
    let (tool, _) = match &extcmd {
        Some(x) => (x.clone(), false),
        None => {
            let explicit = std::env::var(ENV_TOOL).ok();
            let gui = env_true(ENV_GUI) == Some(true);
            tools::select(&config, Mode::Diff, gui, explicit.as_deref())
        }
    };
    if prompt {
        let counter = std::env::var("GIT_DIFF_PATH_COUNTER").unwrap_or_default();
        let total = std::env::var("GIT_DIFF_PATH_TOTAL").unwrap_or_default();
        println!("{}", msg::viewing(&counter, &total, merged));
        print!("{}", msg::launch(&tool));
        match tools::read_answer() {
            None => return Ok(0),
            Some(a) if a.starts_with(['n', 'N']) => return Ok(0),
            Some(_) => {}
        }
    }
    let cwd = std::env::current_dir().map_err(|e| Die::Fatal(e.to_string()))?;
    let code = match &extcmd {
        Some(x) => {
            use std::io::Write as _;
            let _ = std::io::stdout().flush();
            // Evaluated by the shell with the external diff's arguments
            // as positional parameters (observed: `$1` is the path).
            let status = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("{x} \"$LOCAL\" \"$REMOTE\""))
                .arg("sh")
                .args(args)
                .env("LOCAL", local)
                .env("REMOTE", remote)
                .status();
            match status {
                Ok(s) => s.code().unwrap_or(128),
                Err(_) => 127,
            }
        }
        None => {
            let files = Files {
                base: merged.clone(),
                local: local.clone(),
                remote: remote.clone(),
                merged: merged.clone(),
                base_present: false,
            };
            match tools::run(&config, Mode::Diff, &tool, &files, &cwd) {
                Outcome::Ran(c) => c,
                Outcome::NotSet => return Ok(1),
            }
        }
    };
    Ok(tool_result(code, trust))
}
