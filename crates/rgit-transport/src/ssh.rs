// SPDX-License-Identifier: MIT
//! The SSH transport: runs the remote service through the system's `ssh`
//! (or the program configured instead), exactly as git does. rgit has no
//! SSH implementation of its own.
//!
//! Spec: `Documentation/config/ssh.adoc`, `git.adoc` (`GIT_SSH`,
//! `GIT_SSH_COMMAND`, `GIT_SSH_VARIANT`), `config/core.adoc`
//! (`core.sshCommand`), `gitprotocol-pack.adoc` ("SSH Transport"). Argument
//! order and details were observed with git 2.56 and a recording `GIT_SSH`
//! script:
//!
//! ```text
//! ssh:           [-G] [-o SendEnv=GIT_PROTOCOL] [-4|-6] [-p port] host [command]
//! plink, putty:       [-4|-6] [-P port] host command
//! tortoiseplink:      [-4|-6] -batch [-P port] host command
//! simple:             host command        (-4, -6 or a port are errors)
//! ```
//!
//! `-o SendEnv=GIT_PROTOCOL` and the `GIT_PROTOCOL` variable are only used
//! with the OpenSSH variant and a protocol version other than 0. With
//! `ssh.variant=auto` and an unknown program name, git first runs the
//! program with `-G` (OpenSSH's "print configuration") and the OpenSSH
//! options; success selects `ssh`, failure `simple`.

use std::ffi::OsString;
use std::io::Write;
use std::process::Stdio;

use bstr::{BString, ByteSlice};
use rgit_config::{ConfigSet, Environment};

use crate::child::ChildConnection;
use crate::command::{Command, sq_quote};
use crate::os::os_string;
use crate::url::SshTarget;
use crate::{ConnectOptions, Connection, Error, Result, Transport, messages};

/// The command-line dialect of the ssh program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// Decide from the program name, probing with `-G` if it is unknown.
    Auto,
    /// OpenSSH.
    Ssh,
    /// Only `host command`.
    Simple,
    /// PuTTY's plink.
    Plink,
    /// PuTTY.
    Putty,
    /// TortoiseGit's plink.
    TortoisePlink,
}

impl Variant {
    /// The name used in messages and in `ssh.variant`.
    pub fn name(self) -> &'static str {
        match self {
            Variant::Auto => "auto",
            Variant::Ssh => "ssh",
            Variant::Simple => "simple",
            Variant::Plink => "plink",
            Variant::Putty => "putty",
            Variant::TortoisePlink => "tortoiseplink",
        }
    }

    /// Parse `ssh.variant` / `GIT_SSH_VARIANT`. Unknown values mean `ssh`.
    pub fn from_setting(s: &str) -> Variant {
        match s {
            "auto" => Variant::Auto,
            "plink" => Variant::Plink,
            "putty" => Variant::Putty,
            "tortoiseplink" => Variant::TortoisePlink,
            "simple" => Variant::Simple,
            _ => Variant::Ssh,
        }
    }

    /// Detect the variant from the program: the basename of `GIT_SSH`, or
    /// of the first word of `GIT_SSH_COMMAND` / `core.sshCommand` (split
    /// like the shell does; a command that cannot be split is unknown).
    /// Unknown names give [`Variant::Auto`].
    pub fn detect(command: &[u8], is_cmdline: bool) -> Variant {
        let first: Vec<u8> = if is_cmdline {
            match split_words(command) {
                Some(words) if !words.is_empty() => words[0].clone(),
                _ => return Variant::Auto,
            }
        } else {
            command.to_vec()
        };
        let base = match first.rfind_byteset(b"/\\") {
            Some(i) => &first[i + 1..],
            None => &first[..],
        };
        let lower = base.to_ascii_lowercase();
        let name = lower.strip_suffix(b".exe").unwrap_or(&lower);
        match name {
            b"ssh" => Variant::Ssh,
            b"plink" => Variant::Plink,
            b"tortoiseplink" => Variant::TortoisePlink,
            _ => Variant::Auto,
        }
    }
}

/// Split a command line into words with the shell's quoting rules
/// (single quotes, double quotes with `\` escapes, backslash outside
/// quotes). `None` for an unterminated quote.
fn split_words(s: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut words = Vec::new();
    let mut cur: Option<Vec<u8>> = None;
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        match c {
            b' ' | b'\t' | b'\n' => {
                if let Some(w) = cur.take() {
                    words.push(w);
                }
                i += 1;
            }
            b'\'' => {
                let end = i + 1 + s[i + 1..].find_byte(b'\'')?;
                cur.get_or_insert_with(Vec::new)
                    .extend_from_slice(&s[i + 1..end]);
                i = end + 1;
            }
            b'"' => {
                let w = cur.get_or_insert_with(Vec::new);
                i += 1;
                loop {
                    let c = *s.get(i)?;
                    match c {
                        b'"' => {
                            i += 1;
                            break;
                        }
                        b'\\' if matches!(s.get(i + 1), Some(b'"' | b'\\' | b'$' | b'`')) => {
                            w.push(s[i + 1]);
                            i += 2;
                        }
                        _ => {
                            w.push(c);
                            i += 1;
                        }
                    }
                }
            }
            b'\\' => {
                let next = *s.get(i + 1)?;
                cur.get_or_insert_with(Vec::new).push(next);
                i += 2;
            }
            _ => {
                cur.get_or_insert_with(Vec::new).push(c);
                i += 1;
            }
        }
    }
    if let Some(w) = cur {
        words.push(w);
    }
    Some(words)
}

/// `-4` / `-6` from `git clone -4` etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IpVersion {
    /// No preference.
    #[default]
    Any,
    /// IPv4 only.
    V4,
    /// IPv6 only.
    V6,
}

/// The ssh program and how to interpret it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshCommand {
    /// The program (`GIT_SSH`) or shell command (`GIT_SSH_COMMAND`,
    /// `core.sshCommand`).
    pub command: BString,
    /// `command` is a shell command line, not a program path.
    pub is_cmdline: bool,
    /// `GIT_SSH_VARIANT`, else `ssh.variant`.
    pub variant: Option<String>,
}

impl SshCommand {
    /// Select the program: `GIT_SSH_COMMAND`, `core.sshCommand`,
    /// `GIT_SSH`, then `ssh`.
    pub fn from_config(env: &dyn Environment, config: &ConfigSet) -> SshCommand {
        let (command, is_cmdline) = if let Some(c) = env.var_bytes("GIT_SSH_COMMAND") {
            (c, true)
        } else if let Some(c) = config.get("core.sshcommand").and_then(|e| e.value.clone()) {
            (c, true)
        } else if let Some(c) = env.var_bytes("GIT_SSH").filter(|c| !c.is_empty()) {
            (c, false)
        } else {
            ("ssh".into(), false)
        };
        let variant = env
            .var("GIT_SSH_VARIANT")
            .map(|v| v.to_string_lossy().into_owned())
            .or_else(|| {
                config
                    .get("ssh.variant")
                    .and_then(|e| e.value.as_ref())
                    .map(|v| v.to_str_lossy().into_owned())
            });
        SshCommand {
            command,
            is_cmdline,
            variant,
        }
    }

    /// The variant from the setting or the program name; `Auto` means a
    /// `-G` probe is needed.
    pub fn configured_variant(&self) -> Variant {
        match self.variant.as_deref().map(Variant::from_setting) {
            Some(Variant::Auto) | None => Variant::detect(&self.command, self.is_cmdline),
            Some(v) => v,
        }
    }

    fn command(&self) -> Command {
        let c = os_string(&self.command);
        if self.is_cmdline {
            Command::shell(c)
        } else {
            Command::new(c)
        }
    }
}

/// Build the arguments for `ssh` (after the program itself).
///
/// `git_protocol` is the `GIT_PROTOCOL` value to send, `remote` the remote
/// command (`None` for the `-G` probe).
pub fn ssh_args(
    variant: Variant,
    ip: IpVersion,
    target_host: &[u8],
    port: Option<&[u8]>,
    git_protocol: Option<&str>,
    probe: bool,
    remote: Option<&[u8]>,
) -> Result<Vec<OsString>> {
    let mut a: Vec<OsString> = Vec::new();
    if probe {
        a.push("-G".into());
    }
    if variant == Variant::Ssh && git_protocol.is_some() {
        a.push("-o".into());
        a.push("SendEnv=GIT_PROTOCOL".into());
    }
    let ipflag = match ip {
        IpVersion::Any => None,
        IpVersion::V4 => Some("-4"),
        IpVersion::V6 => Some("-6"),
    };
    if let Some(f) = ipflag {
        if variant == Variant::Simple {
            return Err(Error::Refused(messages::variant_unsupported("simple", f)));
        }
        a.push(f.into());
    }
    if variant == Variant::TortoisePlink {
        a.push("-batch".into());
    }
    if let Some(port) = port {
        let flag = match variant {
            Variant::Ssh | Variant::Auto => "-p",
            Variant::Plink | Variant::Putty | Variant::TortoisePlink => "-P",
            Variant::Simple => {
                return Err(Error::Refused(messages::variant_unsupported(
                    "simple",
                    messages::SETTING_PORT,
                )));
            }
        };
        a.push(flag.into());
        a.push(os_string(port));
    }
    a.push(os_string(target_host));
    if let Some(r) = remote {
        a.push(os_string(r));
    }
    Ok(a)
}

/// The remote command: `<program> '<path>'`.
pub fn remote_command(program: &str, path: &[u8]) -> BString {
    let mut c: Vec<u8> = program.as_bytes().to_vec();
    c.push(b' ');
    c.extend_from_slice(&sq_quote(path));
    c.into()
}

/// A transport that reaches the repository through `ssh`.
#[derive(Debug, Clone)]
pub struct SshTransport {
    /// Where to connect.
    pub target: SshTarget,
    /// The ssh program.
    pub ssh: SshCommand,
    /// `-4` / `-6`.
    pub ip: IpVersion,
    /// Extra environment for children (see [`Command::base_env`]).
    pub base_env: Vec<(OsString, OsString)>,
}

impl SshTransport {
    /// Determine the variant, running the `-G` probe if needed.
    pub fn resolve_variant(&self, git_protocol: Option<&str>) -> Result<Variant> {
        let v = self.ssh.configured_variant();
        if v != Variant::Auto {
            return Ok(v);
        }
        let args = ssh_args(
            Variant::Ssh,
            self.ip,
            &self.target.host,
            self.target.port.as_deref().map(|p| p.as_bytes()),
            git_protocol,
            true,
            None,
        )?;
        let mut cmd = self.ssh.command();
        for a in args {
            cmd.arg(a);
        }
        cmd.base_env(&self.base_env);
        if let Some(gp) = git_protocol {
            cmd.env("GIT_PROTOCOL", gp);
        }
        cmd.trace();
        let status = cmd
            .to_std()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        Ok(match status {
            Ok(s) if s.success() => Variant::Ssh,
            Ok(_) => Variant::Simple,
            Err(e) => {
                let _ = writeln!(
                    std::io::stderr(),
                    "error: {}",
                    messages::cannot_run(
                        &cmd.program_display(),
                        &rgit_trace::messages::strerror(&e)
                    )
                );
                Variant::Simple
            }
        })
    }

    /// The full command that [`Transport::connect`] runs, for tests and
    /// for callers that need to show it.
    pub fn command_for(&self, options: &ConnectOptions) -> Result<Command> {
        let gp = options.git_protocol();
        let variant = self.resolve_variant(gp.as_deref())?;
        let remote = remote_command(options.program(), &self.target.path);
        let send_protocol = if variant == Variant::Ssh {
            gp.as_deref()
        } else {
            None
        };
        let args = ssh_args(
            variant,
            self.ip,
            &self.target.host,
            self.target.port.as_deref().map(|p| p.as_bytes()),
            send_protocol,
            false,
            Some(&remote),
        )?;
        let mut cmd = self.ssh.command();
        for a in args {
            cmd.arg(a);
        }
        cmd.base_env(&self.base_env);
        cmd.unset_local_env();
        if let Some(gp) = send_protocol {
            cmd.env("GIT_PROTOCOL", gp);
        }
        Ok(cmd)
    }
}

impl Transport for SshTransport {
    fn connect(&mut self, options: &ConnectOptions) -> Result<Box<dyn Connection>> {
        let cmd = self.command_for(options)?;
        Ok(Box::new(ChildConnection::spawn(&cmd)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(
        v: Variant,
        ip: IpVersion,
        port: Option<&str>,
        gp: Option<&str>,
    ) -> std::result::Result<Vec<String>, String> {
        ssh_args(
            v,
            ip,
            b"myhost",
            port.map(str::as_bytes),
            gp,
            false,
            Some(b"git-upload-pack 'src'"),
        )
        .map(|a| a.iter().map(|s| s.to_string_lossy().into_owned()).collect())
        .map_err(|e| e.to_string())
    }

    #[test]
    fn variants() {
        let cmd = "git-upload-pack 'src'";
        assert_eq!(
            args(Variant::Ssh, IpVersion::Any, None, Some("version=2")).unwrap(),
            ["-o", "SendEnv=GIT_PROTOCOL", "myhost", cmd]
        );
        assert_eq!(
            args(Variant::Ssh, IpVersion::V4, Some("123"), None).unwrap(),
            ["-4", "-p", "123", "myhost", cmd]
        );
        assert_eq!(
            args(Variant::Plink, IpVersion::V6, Some("22"), Some("version=2")).unwrap(),
            ["-6", "-P", "22", "myhost", cmd]
        );
        assert_eq!(
            args(Variant::TortoisePlink, IpVersion::V4, Some("22"), None).unwrap(),
            ["-4", "-batch", "-P", "22", "myhost", cmd]
        );
        assert_eq!(
            args(Variant::Simple, IpVersion::Any, None, Some("version=2")).unwrap(),
            ["myhost", cmd]
        );
        assert_eq!(
            args(Variant::Simple, IpVersion::V4, None, None).unwrap_err(),
            "ssh variant 'simple' does not support -4"
        );
        assert_eq!(
            args(Variant::Simple, IpVersion::Any, Some("1"), None).unwrap_err(),
            "ssh variant 'simple' does not support setting port"
        );
    }

    #[test]
    fn detection() {
        assert_eq!(Variant::detect(b"/x/ssh", false), Variant::Ssh);
        assert_eq!(Variant::detect(b"/x/SSH.EXE", false), Variant::Ssh);
        assert_eq!(Variant::detect(b"/x/Plink", false), Variant::Plink);
        assert_eq!(Variant::detect(b"/x/plink.EXE", false), Variant::Plink);
        assert_eq!(
            Variant::detect(b"/x/tortoiseplink.exe", false),
            Variant::TortoisePlink
        );
        assert_eq!(Variant::detect(b"/x/putty", false), Variant::Auto);
        assert_eq!(Variant::detect(b"/x/simple", false), Variant::Auto);
        assert_eq!(
            Variant::detect(b"\"/x/plink.exe\" -v", true),
            Variant::Plink
        );
        assert_eq!(Variant::detect(b"'/x/plink.exe' -v", true), Variant::Plink);
        assert_eq!(Variant::detect(b"/x/pl\\ink -v", true), Variant::Plink);
        assert_eq!(Variant::detect(b"\"/x/pl\"ink -v", true), Variant::Plink);
        assert_eq!(Variant::detect(b"'plink.exe -v", true), Variant::Auto);
        assert_eq!(Variant::detect(b"FOO=1 plink -v", true), Variant::Auto);
        assert_eq!(Variant::detect(b"/x/ssh -v", true), Variant::Ssh);
        assert_eq!(Variant::detect(b"/x/plink -v", false), Variant::Auto);
    }

    #[test]
    fn settings() {
        assert_eq!(Variant::from_setting("Plink"), Variant::Ssh);
        assert_eq!(Variant::from_setting("foo"), Variant::Ssh);
        assert_eq!(Variant::from_setting("putty"), Variant::Putty);
        assert_eq!(Variant::from_setting("auto"), Variant::Auto);
    }

    #[test]
    fn remote_cmd() {
        assert_eq!(
            remote_command("git-upload-pack", b"it's"),
            "git-upload-pack 'it'\\''s'"
        );
        assert_eq!(remote_command("foo bar", b"x"), "foo bar 'x'");
        assert_eq!(
            remote_command("git-upload-pack", b"a!b"),
            "git-upload-pack 'a'\\!'b'"
        );
    }
}
