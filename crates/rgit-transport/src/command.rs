// SPDX-License-Identifier: MIT
//! Starting child processes the way git does: optional `sh -c` wrapping,
//! extra and removed environment variables, and `GIT_TRACE` lines.
//!
//! Behaviour observed from git 2.56:
//!
//! - A command that is to be run "through the shell" is only wrapped in
//!   `/bin/sh -c` when its first word contains shell syntax; the wrapper is
//!   `sh -c '<cmd> "$@"' '<cmd>' <args>...` (without `"$@"` when there are
//!   no arguments).
//! - `trace: run_command:` lists `unset <names>;` (sorted, only variables
//!   that are present) and `NAME=value` for added variables, then the
//!   command as given; `trace: start_command:` shows what is executed.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process;

use rgit_trace::{Location, TRACE, quote_arg_into, quote_argv};

/// The repository-local environment variables a transport child must not
/// inherit (`git rev-parse --local-env-vars`).
pub const LOCAL_ENV_VARS: &[&str] = &[
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

/// Characters that make git hand a command to the shell.
const SHELL_META: &[u8] = b"|&;<>()$`\\\"' \t\n*?[#~=%";

/// The shell git uses.
pub const SHELL_PATH: &str = "/bin/sh";

/// Whether `cmd` needs `sh -c` to be run.
pub fn needs_shell(cmd: &[u8]) -> bool {
    cmd.iter().any(|b| SHELL_META.contains(b))
}

/// Quote `s` for the shell: `'...'`, with `'` and `!` written as `'\''`
/// and `'\!'`.
pub fn sq_quote(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() + 2);
    out.push(b'\'');
    for &b in s {
        if b == b'\'' || b == b'!' {
            out.extend_from_slice(b"'\\");
            out.push(b);
            out.push(b'\'');
        } else {
            out.push(b);
        }
    }
    out.push(b'\'');
    out
}

/// A child process description.
#[derive(Debug, Clone, Default)]
pub struct Command {
    argv: Vec<OsString>,
    use_shell: bool,
    set_env: Vec<(String, OsString)>,
    unset_env: Vec<String>,
    base_env: Vec<(OsString, OsString)>,
}

impl Command {
    /// Run `program` directly.
    pub fn new(program: impl Into<OsString>) -> Self {
        Command {
            argv: vec![program.into()],
            ..Default::default()
        }
    }

    /// Run `cmd` through the shell if it contains shell syntax.
    pub fn shell(cmd: impl Into<OsString>) -> Self {
        Command {
            argv: vec![cmd.into()],
            use_shell: true,
            ..Default::default()
        }
    }

    /// Append an argument.
    pub fn arg(&mut self, a: impl Into<OsString>) -> &mut Self {
        self.argv.push(a.into());
        self
    }

    /// Set an environment variable for the child (shown in the trace).
    pub fn env(&mut self, name: &str, value: impl Into<OsString>) -> &mut Self {
        self.set_env.push((name.to_string(), value.into()));
        self
    }

    /// Variables the child inherits on top of this process's environment
    /// (the global options' exports, `GIT_EXEC_PATH`, `PATH`). They are not
    /// traced, as git has them in its own environment.
    pub fn base_env(&mut self, env: &[(OsString, OsString)]) -> &mut Self {
        self.base_env.extend(env.iter().cloned());
        self
    }

    /// Remove the repository-local variables ([`LOCAL_ENV_VARS`]).
    /// `GIT_PREFIX` is always listed: git sets it in its own environment
    /// during setup.
    pub fn unset_local_env(&mut self) -> &mut Self {
        let mut names: Vec<String> = LOCAL_ENV_VARS
            .iter()
            .filter(|n| **n == "GIT_PREFIX" || self.is_present(n))
            .map(|n| n.to_string())
            .collect();
        names.sort();
        self.unset_env = names;
        self
    }

    fn is_present(&self, name: &str) -> bool {
        self.base_env.iter().any(|(k, _)| k == name) || std::env::var_os(name).is_some()
    }

    fn lookup_env(&self, name: &str) -> Option<OsString> {
        if let Some((_, v)) = self.set_env.iter().rev().find(|(k, _)| k == name) {
            return Some(v.clone());
        }
        if let Some((_, v)) = self.base_env.iter().rev().find(|(k, _)| k == name) {
            return Some(v.clone());
        }
        std::env::var_os(name)
    }

    /// The command as given (program and arguments).
    pub fn argv(&self) -> &[OsString] {
        &self.argv
    }

    /// The argument vector that is executed, after shell wrapping.
    pub fn resolved_argv(&self) -> Vec<OsString> {
        let first = self.argv[0].as_encoded_bytes();
        if self.use_shell && needs_shell(first) {
            let mut script = self.argv[0].clone();
            if self.argv.len() > 1 {
                script.push(" \"$@\"");
            }
            let mut v: Vec<OsString> = vec![SHELL_PATH.into(), "-c".into(), script];
            v.extend(self.argv.iter().cloned());
            v
        } else {
            self.argv.clone()
        }
    }

    /// Search `PATH` for a program name without a slash (for the trace).
    fn find_in_path(&self, name: &OsStr) -> Option<PathBuf> {
        if name.as_encoded_bytes().contains(&b'/') || name.is_empty() {
            return None;
        }
        let path = self.lookup_env("PATH")?;
        std::env::split_paths(&path)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    }

    /// Write the `trace: run_command:` and `trace: start_command:` lines.
    #[track_caller]
    pub fn trace(&self) {
        if !TRACE.enabled() {
            return;
        }
        let loc = Location::caller();
        let mut msg = b"trace: run_command: ".to_vec();
        if !self.unset_env.is_empty() {
            msg.extend_from_slice(b"unset ");
            msg.extend_from_slice(self.unset_env.join(" ").as_bytes());
            msg.extend_from_slice(b"; ");
        }
        for (k, v) in &self.set_env {
            msg.extend_from_slice(k.as_bytes());
            msg.push(b'=');
            quote_arg_into(&mut msg, v.as_encoded_bytes());
            msg.push(b' ');
        }
        msg.extend_from_slice(&quote_argv(&self.argv));
        TRACE.write_line(loc, &msg);

        let mut resolved = self.resolved_argv();
        if let Some(p) = self.find_in_path(&resolved[0]) {
            resolved[0] = p.into_os_string();
        }
        let mut msg = b"trace: start_command: ".to_vec();
        msg.extend_from_slice(&quote_argv(&resolved));
        TRACE.write_line(loc, &msg);
    }

    /// A `std::process::Command` for this description. Standard streams
    /// are inherited unless the caller changes them.
    pub fn to_std(&self) -> process::Command {
        let argv = self.resolved_argv();
        let mut c = process::Command::new(&argv[0]);
        c.args(&argv[1..]);
        for (k, v) in &self.base_env {
            c.env(k, v);
        }
        for k in &self.unset_env {
            c.env_remove(k);
        }
        for (k, v) in &self.set_env {
            c.env(k, v);
        }
        c
    }

    /// The program name for messages (`argv[0]`, lossily converted).
    pub fn program_display(&self) -> String {
        self.argv[0].to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_wrapping() {
        let mut c = Command::shell("ssh -v");
        c.arg("host").arg("cmd");
        let v: Vec<String> = c
            .resolved_argv()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            v,
            ["/bin/sh", "-c", "ssh -v \"$@\"", "ssh -v", "host", "cmd"]
        );

        let c = Command::shell("git-upload-pack 'x'");
        let v: Vec<String> = c
            .resolved_argv()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            v,
            [
                "/bin/sh",
                "-c",
                "git-upload-pack 'x'",
                "git-upload-pack 'x'"
            ]
        );

        let mut c = Command::shell("/usr/bin/ssh");
        c.arg("host");
        assert_eq!(c.resolved_argv().len(), 2);
    }

    #[test]
    fn quoting() {
        assert_eq!(sq_quote(b"src"), b"'src'");
        assert_eq!(sq_quote(b"it's"), b"'it'\\''s'");
        assert_eq!(sq_quote(b"a!b"), b"'a'\\!'b'");
    }
}
