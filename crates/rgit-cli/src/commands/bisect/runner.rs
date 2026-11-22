// SPDX-License-Identifier: MIT
//! `git bisect run [--reset-when-found[=<where>]] <cmd> [<arg>...]`: mark
//! each commit by the exit code of a command (0 old, 125 skip, 1..127 new,
//! anything else stops), until the first new commit is found.

use std::io::Write;

use super::{Bisect, Outcome};
use crate::context::sq_quote;
use crate::messages::bisect as msg;
use crate::prelude::*;

/// Run `command` (already quoted) through the shell; the exit code, or -1
/// for a signal.
fn run_shell(b: &Bisect<'_>, command: &str) -> Result<i32, Die> {
    let _ = std::io::stdout().flush();
    let status = std::process::Command::new(if cfg!(unix) { "/bin/sh" } else { "sh" })
        .arg("-c")
        .arg(command)
        .arg(command)
        .envs(b.ctx.child_env())
        .current_dir(b.work_dir())
        .status()
        .map_err(|e| Die::Fatal(e.to_string()))?;
    Ok(status.code().unwrap_or(-1))
}

pub fn run(b: &mut Bisect<'_>, args: &[String]) -> CmdResult {
    let mut reset_when_found: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--reset-when-found" {
            reset_when_found = Some("original".into());
        } else if let Some(v) = a.strip_prefix("--reset-when-found=") {
            reset_when_found = Some(v.into());
        } else {
            break;
        }
        i += 1;
    }
    if let Some(w) = &reset_when_found {
        if w != "original" && w != "found" {
            return Err(Die::Error(msg::invalid_reset_when_found(w), 1));
        }
    }
    let cmd = &args[i..];
    if cmd.is_empty() {
        return Err(Die::Error(msg::RUN_NO_COMMAND.into(), 1));
    }
    if !b.is_bisecting() {
        return Err(Die::Exit(1));
    }
    if let Some(w) = &reset_when_found {
        if b.no_checkout() {
            return Err(Die::Error(msg::RESET_WHEN_FOUND_NO_CHECKOUT.into(), 1));
        }
        b.write("BISECT_RESET_WHEN_FOUND", &format!("{w}\n"))?;
    }
    let command = cmd.iter().map(|a| sq_quote(a)).collect::<Vec<_>>().join(" ");
    let mut verified = false;
    loop {
        println!("{}", msg::running(&command));
        let code = run_shell(b, &command)?;
        if !(0..128).contains(&code) {
            eprintln!("error: {}", msg::run_bad_exit(code, &command));
            return Err(Die::Exit((-code) & 0xff));
        }
        let terms = b.terms();
        if (code == 126 || code == 127) && !verified {
            verified = true;
            if let Some(c) = verify_on_good(b, &command, &terms.old)? {
                return Err(Die::Exit(c));
            }
        }
        let state = match code {
            125 => "skip".to_string(),
            0 => terms.old.clone(),
            _ => terms.new.clone(),
        };
        if !b.is_bisecting() {
            let _ = std::fs::write(b.path("BISECT_RUN"), "");
            let _ = writeln!(std::io::stderr(), "{}", msg::NEED_START);
            eprintln!("error: {}", msg::state_failed(&state, -1));
            return Err(Die::Exit(1));
        }
        let outcome = match b.mark(&terms, &state, &[]) {
            Ok(o) => o,
            Err(die) => {
                b.flush();
                let c = die.emit();
                eprintln!("error: {}", msg::state_failed(&state, c));
                return Err(Die::Exit(c));
            }
        };
        if b.is_bisecting() {
            let _ = std::fs::write(b.path("BISECT_RUN"), &b.out);
        }
        b.flush();
        match outcome {
            Outcome::Continue => {}
            Outcome::Found => {
                println!("{}", msg::found_first(&terms.new));
                return Ok(0);
            }
            Outcome::OnlySkipped => {
                eprintln!("error: {}", msg::CANNOT_CONTINUE);
                return Err(Die::Exit(2));
            }
            Outcome::Failed(c) => {
                eprintln!("error: {}", msg::state_failed(&state, c));
                return Err(Die::Exit(c));
            }
        }
    }
}

/// Exit codes 126 and 127 may mean the command cannot run at all: run it
/// on an old commit too, which must pass. `Some(exit code)` when it does
/// not.
fn verify_on_good(b: &mut Bisect<'_>, command: &str, old: &str) -> Result<Option<i32>, Die> {
    let terms = b.terms();
    let (_, goods, _) = b.marks(&terms);
    let Some(good) = goods.first().copied() else {
        return Ok(None);
    };
    let current = if b.no_checkout() {
        b.commit("BISECT_HEAD")
    } else {
        b.commit("HEAD")
    };
    b.check_out(&good)?;
    b.flush();
    println!("{}", msg::running(command));
    let code = run_shell(b, command)?;
    let failed = if code == 0 {
        None
    } else if !(0..128).contains(&code) {
        eprintln!("error: {}", msg::unable_to_verify(command, old));
        Some(1)
    } else {
        eprintln!("error: {}", msg::bogus_exit_code(code, old));
        Some(1)
    };
    if let Some(cur) = current {
        b.check_out(&cur)?;
        b.flush();
    }
    Ok(failed)
}
