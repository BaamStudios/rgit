// SPDX-License-Identifier: MIT
//! Asking the user: the askpass program, then the terminal.
//!
//! Order (`Documentation/gitcredentials.adoc`, "REQUESTING CREDENTIALS",
//! and git 2.56): `GIT_ASKPASS` if set (an empty value disables askpass,
//! `SSH_ASKPASS` is then not consulted), else `core.askPass`, else
//! `SSH_ASKPASS`. The program is executed directly (no shell) with the
//! prompt as its only argument; the first line of its output is the
//! answer. If it cannot be run or fails, git falls back to the terminal,
//! unless `GIT_TERMINAL_PROMPT` is false. Passwords are read without echo.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::process::Stdio;

use bstr::{BString, ByteSlice};
use rgit_config::{ConfigSet, Environment};

use super::{CredentialContext, fatal};
use crate::command::Command;
use crate::os::os_string;
use crate::{Result, messages};

/// How the user is asked; replaceable for tests.
pub trait Prompter {
    /// Run `program` with `prompt`; the answer, or `None` if it failed
    /// (after printing git's message).
    fn askpass(
        &mut self,
        program: &[u8],
        prompt: &str,
        base_env: &[(OsString, OsString)],
    ) -> Option<BString>;

    /// Ask on the terminal (`/dev/tty`), without echo unless `echo`.
    fn terminal(&mut self, prompt: &str, echo: bool) -> io::Result<BString>;
}

/// The askpass program to use, if any.
pub fn askpass_program(env: &dyn Environment, config: &ConfigSet) -> Option<BString> {
    let p = env
        .var_bytes("GIT_ASKPASS")
        .or_else(|| config.get("core.askpass").and_then(|e| e.value.clone()))
        .or_else(|| env.var_bytes("SSH_ASKPASS"))?;
    (!p.is_empty()).then_some(p)
}

/// Ask for `what` (e.g. `Username for 'https://host'`); the prompt is
/// `what` followed by `: `.
pub fn ask(ctx: &mut CredentialContext<'_>, what: &str, echo: bool) -> Result<BString> {
    let prompt = format!("{what}: ");
    if let Some(program) = askpass_program(ctx.env, ctx.config) {
        if let Some(answer) = ctx.prompter.askpass(&program, &prompt, &ctx.base_env) {
            return Ok(answer);
        }
    }
    if let Some(v) = ctx.env.var("GIT_TERMINAL_PROMPT") {
        let bytes = v.as_encoded_bytes();
        match rgit_config::value::parse_bool(Some(bytes)) {
            None => {
                return Err(fatal(messages::bad_bool_env(
                    &bytes.to_str_lossy(),
                    "GIT_TERMINAL_PROMPT",
                )));
            }
            Some(false) => {
                return Err(fatal(messages::could_not_read(
                    what,
                    messages::TERMINAL_PROMPTS_DISABLED,
                )));
            }
            Some(true) => {}
        }
    }
    ctx.prompter.terminal(&prompt, echo).map_err(|e| {
        fatal(messages::could_not_read(
            what,
            &rgit_trace::messages::strerror(&e),
        ))
    })
}

/// The first line of `out`, without the line end.
fn first_line(out: &[u8]) -> BString {
    let line = match out.find_byte(b'\n') {
        Some(i) => &out[..i],
        None => out,
    };
    line.strip_suffix(b"\r").unwrap_or(line).into()
}

/// The real prompter: runs askpass programs and uses `/dev/tty`.
#[derive(Debug, Default)]
pub struct SystemPrompter;

impl Prompter for SystemPrompter {
    fn askpass(
        &mut self,
        program: &[u8],
        prompt: &str,
        base_env: &[(OsString, OsString)],
    ) -> Option<BString> {
        let mut cmd = Command::new(os_string(program));
        cmd.arg(prompt).base_env(base_env);
        cmd.trace();
        let display = program.to_str_lossy();
        let mut child = match cmd.to_std().stdout(Stdio::piped()).spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = writeln!(
                    io::stderr(),
                    "fatal: {}",
                    messages::cannot_exec(&display, &rgit_trace::messages::strerror(&e))
                );
                return None;
            }
        };
        let mut out = Vec::new();
        let read = child
            .stdout
            .take()
            .map(|mut s| s.read_to_end(&mut out))
            .transpose();
        let status = child.wait();
        match (read, status) {
            (Ok(_), Ok(s)) if s.success() => Some(first_line(&out)),
            _ => {
                let _ = writeln!(
                    io::stderr(),
                    "error: {}",
                    messages::askpass_failed(&display)
                );
                None
            }
        }
    }

    fn terminal(&mut self, prompt: &str, echo: bool) -> io::Result<BString> {
        terminal_prompt(prompt, echo)
    }
}

#[cfg(unix)]
fn terminal_prompt(prompt: &str, echo: bool) -> io::Result<BString> {
    use rustix::termios::{LocalModes, OptionalActions, tcgetattr, tcsetattr};

    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")?;
    tty.write_all(prompt.as_bytes())?;
    tty.flush()?;
    let saved = if echo {
        None
    } else {
        let saved = tcgetattr(&tty)?;
        let mut t = saved.clone();
        t.local_modes.remove(LocalModes::ECHO);
        tcsetattr(&tty, OptionalActions::Flush, &t)?;
        Some(saved)
    };
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    let result = loop {
        match tty.read(&mut byte) {
            Ok(0) => break Ok(()),
            Ok(_) if byte[0] == b'\n' => break Ok(()),
            Ok(_) => line.push(byte[0]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => break Err(e),
        }
    };
    if let Some(saved) = saved {
        let _ = tcsetattr(&tty, OptionalActions::Flush, &saved);
        let _ = tty.write_all(b"\n");
    }
    result?;
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Ok(line.into())
}

#[cfg(not(unix))]
fn terminal_prompt(_prompt: &str, _echo: bool) -> io::Result<BString> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_line_strips_line_end() {
        assert_eq!(first_line(b"line1\r\nline2\n"), "line1");
        assert_eq!(first_line(b"noeol"), "noeol");
        assert_eq!(first_line(b""), "");
    }
}
