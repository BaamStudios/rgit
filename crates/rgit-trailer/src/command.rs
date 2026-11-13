// SPDX-License-Identifier: MIT
//! Running `trailer.<alias>.cmd` and `trailer.<alias>.command`
//! (Documentation/config/trailer.adoc).
//!
//! `cmd` is run by the shell with the value as its only argument (`$1`);
//! `command` gets no arguments, the first `$ARG` in it is replaced by the
//! value instead. A command without shell metacharacters is executed
//! directly. Standard input is empty, standard error is passed through,
//! standard output (trimmed) is the new value.

use std::ffi::OsString;
use std::process::{Command, Stdio};

use bstr::{BString, ByteSlice};

use crate::config::Item;
use crate::parse::trim;

/// Characters that make git hand a command to the shell.
const SHELL_METACHARS: &[u8] = b"|&;<>()$`\\\"' \t\n*?[#~=%";

fn os(b: &[u8]) -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(b.to_vec())
    }
    #[cfg(not(unix))]
    {
        OsString::from(String::from_utf8_lossy(b).into_owned())
    }
}

fn shell_command(cmd: &[u8], args: &[&[u8]]) -> Command {
    if cmd.iter().any(|b| SHELL_METACHARS.contains(b)) {
        let mut script = cmd.to_vec();
        if !args.is_empty() {
            script.extend_from_slice(b" \"$@\"");
        }
        let mut c = Command::new("sh");
        c.arg("-c").arg(os(&script)).arg(os(cmd));
        c.args(args.iter().map(|a| os(a)));
        c
    } else {
        let mut c = Command::new(os(cmd));
        c.args(args.iter().map(|a| os(a)));
        c
    }
}

/// Run the command of `item` for the value `arg`. Returns the trimmed
/// output, or the command as shown in the error message when it fails.
pub fn run(item: &Item, arg: &[u8]) -> Result<BString, BString> {
    let (shown, mut command) = if let Some(cmd) = &item.cmd {
        (cmd.clone(), shell_command(cmd, &[arg]))
    } else if let Some(command) = &item.command {
        let cmd: BString = command.replacen(b"$ARG", arg, 1).into();
        let c = shell_command(&cmd, &[]);
        (cmd, c)
    } else {
        return Ok(BString::default());
    };
    command.stdin(Stdio::null()).stderr(Stdio::inherit());
    match command.output() {
        Ok(out) if out.status.success() => Ok(trim(&out.stdout).into()),
        _ => Err(shown),
    }
}
