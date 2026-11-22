// SPDX-License-Identifier: MIT
//! Signing and verifying commits and tags (#51) by driving the external
//! programs: `gpg` (OpenPGP), `gpgsm` (X.509) and `ssh-keygen` (SSH).
//!
//! Sources: Documentation/config/gpg.adoc (`gpg.format`, `gpg.program`,
//! `gpg.<format>.program`, `gpg.minTrustLevel`, `gpg.ssh.*`),
//! Documentation/config/user.adoc (`user.signingKey`, else the committer
//! identity), Documentation/gitformat-signature.adoc (the `gpgsig` header,
//! `gpgsig-sha256` in a SHA-256 repository, signatures appended to tag
//! messages) and the behaviour of the `git` binary with stub programs:
//!
//! - OpenPGP and X.509: the payload is fed to `<program> --status-fd=2
//!   -bsau <key>`; a signature counts only when the status lines report
//!   `SIG_CREATED`.
//! - SSH: the payload is written to a temporary file and signed with
//!   `ssh-keygen -Y sign -n git -f <key> <file>`, which writes `<file>.sig`.
//!   A literal public key (`key::<key>` or `ssh-<type> ...`) is written to
//!   a temporary file and used with `-U` (the private key is in the agent).
//!
//! Verification lives in [`verify`].

pub(crate) mod verify;

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use rgit_config::{ConfigSet, SystemEnv};
use rgit_hash::HashKind;
use rgit_object::{Commit, Header, Signature};

use crate::Die;
use crate::messages::signing as msg;

/// The signature formats (`gpg.format`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    OpenPgp,
    X509,
    Ssh,
}

impl Format {
    fn default_program(self) -> &'static str {
        match self {
            Format::OpenPgp => "gpg",
            Format::X509 => "gpgsm",
            Format::Ssh => "ssh-keygen",
        }
    }

    /// The format of an armoured signature, from its first line.
    pub(crate) fn of_signature(sig: &[u8]) -> Option<Format> {
        const MARKERS: &[(&[u8], Format)] = &[
            (b"-----BEGIN PGP SIGNATURE-----", Format::OpenPgp),
            (b"-----BEGIN PGP MESSAGE-----", Format::OpenPgp),
            (b"-----BEGIN SIGNED MESSAGE-----", Format::X509),
            (b"-----BEGIN SSH SIGNATURE-----", Format::Ssh),
        ];
        MARKERS
            .iter()
            .find(|(m, _)| sig.starts_with(m))
            .map(|&(_, f)| f)
    }
}

/// Trust levels, in increasing order (`gpg.minTrustLevel`, `%GT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) enum Trust {
    #[default]
    Undefined,
    Never,
    Marginal,
    Fully,
    Ultimate,
}

impl Trust {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Trust::Undefined => "undefined",
            Trust::Never => "never",
            Trust::Marginal => "marginal",
            Trust::Fully => "fully",
            Trust::Ultimate => "ultimate",
        }
    }

    fn parse(s: &str) -> Option<Trust> {
        [
            Trust::Undefined,
            Trust::Never,
            Trust::Marginal,
            Trust::Fully,
            Trust::Ultimate,
        ]
        .into_iter()
        .find(|t| t.name().eq_ignore_ascii_case(s))
    }
}

/// The `gpg.*` settings.
#[derive(Debug, Clone)]
pub(crate) struct Settings {
    pub(crate) format: Format,
    /// `gpg.<format>.program` (and `gpg.program` for OpenPGP), by format.
    programs: [Option<String>; 3],
    signing_key: Option<String>,
    /// `gpg.minTrustLevel`, when set.
    pub(crate) min_trust: Option<Trust>,
    default_key_command: Option<String>,
    pub(crate) allowed_signers: Option<PathBuf>,
    pub(crate) revocation_file: Option<PathBuf>,
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Program output inside an `error:` message: control characters other
/// than tab and newline become `?`, like git's message printing does.
fn reported(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_control() && c != '\t' && c != '\n' {
                '?'
            } else {
                c
            }
        })
        .collect()
}

/// Drops carriage returns before newlines.
pub(crate) fn strip_cr(data: &[u8]) -> Vec<u8> {
    normalize_newlines(data.to_vec())
}

/// `~` and `~/...` relative to `$HOME`.
fn expand_home(s: &str) -> String {
    if s == "~" || s.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return format!("{}{}", home.to_string_lossy(), &s[1..]);
        }
    }
    s.to_owned()
}

impl Settings {
    /// Reads the settings; an invalid `gpg.format` or `gpg.minTrustLevel`
    /// is fatal, like in git.
    pub(crate) fn from_config(config: &ConfigSet) -> Result<Settings, Die> {
        let mut s = Settings {
            format: Format::OpenPgp,
            programs: [None, None, None],
            signing_key: None,
            min_trust: None,
            default_key_command: None,
            allowed_signers: None,
            revocation_file: None,
        };
        let env = SystemEnv::default();
        for e in config.entries() {
            let name: &[u8] = e.name.as_ref();
            let value = e.value.as_ref().map(|v| lossy(v.as_ref()));
            match name {
                b"gpg.format" => {
                    let v = value.unwrap_or_default();
                    s.format = match v.as_str() {
                        "openpgp" => Format::OpenPgp,
                        "x509" => Format::X509,
                        "ssh" => Format::Ssh,
                        _ => return Err(crate::comment::bad_entry(e, &msg::bad_format(&v))),
                    };
                }
                b"gpg.mintrustlevel" => {
                    let v = value.unwrap_or_default();
                    match Trust::parse(&v) {
                        Some(t) => s.min_trust = Some(t),
                        None => {
                            return Err(crate::comment::bad_entry(e, &msg::bad_min_trust(&v)));
                        }
                    }
                }
                b"gpg.program" | b"gpg.openpgp.program" => {
                    s.programs[0] = value.map(|v| expand_home(&v));
                }
                b"gpg.x509.program" => s.programs[1] = value.map(|v| expand_home(&v)),
                b"gpg.ssh.program" => s.programs[2] = value.map(|v| expand_home(&v)),
                b"user.signingkey" => s.signing_key = value,
                b"gpg.ssh.defaultkeycommand" => s.default_key_command = value,
                b"gpg.ssh.allowedsignersfile" => {
                    s.allowed_signers = e.path(&env).ok().flatten();
                }
                b"gpg.ssh.revocationfile" => s.revocation_file = e.path(&env).ok().flatten(),
                _ => {}
            }
        }
        Ok(s)
    }

    /// The program of `format`.
    pub(crate) fn program(&self, format: Format) -> String {
        let i = match format {
            Format::OpenPgp => 0,
            Format::X509 => 1,
            Format::Ssh => 2,
        };
        self.programs[i]
            .clone()
            .unwrap_or_else(|| format.default_program().to_owned())
    }

    /// The minimum trust level, `default` when `gpg.minTrustLevel` is
    /// unset (`undefined` for most commands, `marginal` for merges).
    pub(crate) fn min_trust_or(&self, default: Trust) -> Trust {
        self.min_trust.unwrap_or(default)
    }
}

/// Which key signs: `-S` (the default key) or `-S<keyid>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Sign {
    /// `user.signingKey`, else the committer identity.
    Default,
    Key(String),
}

impl Sign {
    /// From the value of `-S[<keyid>]`.
    pub(crate) fn from_arg(value: Option<&str>) -> Sign {
        match value {
            Some(k) if !k.is_empty() => Sign::Key(k.to_owned()),
            _ => Sign::Default,
        }
    }

    /// The option as git writes it into a rebase state (`gpg_sign_opt`).
    pub(crate) fn to_opt(&self) -> String {
        match self {
            Sign::Default => "-S".to_owned(),
            Sign::Key(k) => format!("-S{k}"),
        }
    }

    /// The inverse of [`Sign::to_opt`].
    pub(crate) fn from_opt(opt: &str) -> Option<Sign> {
        opt.strip_prefix("-S").map(|k| Sign::from_arg(Some(k)))
    }
}

/// The signing request of a command: `cli` is `Some(Some(_))` for
/// `-S[<keyid>]`, `Some(None)` for `--no-gpg-sign` and `None` when neither
/// was given; then `commit.gpgSign` decides.
pub(crate) fn resolve(config: &ConfigSet, cli: Option<Option<Sign>>) -> Option<Sign> {
    match cli {
        Some(s) => s,
        None => {
            (config.bool("commit.gpgsign").ok().flatten() == Some(true)).then_some(Sign::Default)
        }
    }
}

/// Why signing failed.
#[derive(Debug)]
pub(crate) enum SignError {
    /// The signing program failed; the message (with its standard error)
    /// has been printed.
    Failed,
    /// A configuration error.
    Die(Die),
}

/// Signs `commit` (built without signature headers) and adds the
/// signature header. `committer` provides the default key.
pub(crate) fn sign_commit(
    config: &ConfigSet,
    hash: HashKind,
    commit: &mut Commit,
    sign: &Sign,
    committer: &Signature,
) -> Result<(), SignError> {
    let settings = Settings::from_config(config).map_err(SignError::Die)?;
    let ident = format!(
        "{} <{}>",
        String::from_utf8_lossy(&committer.name),
        String::from_utf8_lossy(&committer.email)
    );
    let key = signing_key(&settings, sign, &ident)?;
    let mut value = sign_buffer(&settings, &commit.serialize(), &key)?;
    while value.last() == Some(&b'\n') {
        value.pop();
    }
    let name: &[u8] = match hash {
        HashKind::Sha256 => b"gpgsig-sha256",
        _ => b"gpgsig",
    };
    commit.block_mut().headers.push(Header::new(name, value));
    Ok(())
}

/// Writes the commit `new`, signed when `sign` asks for it. A failed
/// signature is git's "failed to write commit object".
pub(crate) fn create_commit(
    repo: &crate::repo_objects::Repo,
    new: &rgit_commit::NewCommit<'_>,
    sign: Option<&Sign>,
) -> Result<rgit_hash::ObjectId, Die> {
    if !new.conforms() {
        eprintln!("{}", rgit_commit::messages::NOT_UTF8_WARNING);
    }
    let mut commit = new.to_commit();
    if let Some(sign) = sign {
        match sign_commit(&repo.config, repo.hash, &mut commit, sign, new.committer) {
            Ok(()) => {}
            Err(SignError::Die(d)) => return Err(d),
            Err(SignError::Failed) => {
                return Err(Die::Fatal(msg::FAILED_TO_WRITE_COMMIT.to_owned()));
            }
        }
    }
    repo.odb
        .write(rgit_object::ObjectKind::Commit, &commit.serialize())
        .map_err(|e| Die::Fatal(e.to_string()))
}

/// The key to sign with: `-S<key>`, `user.signingKey`, else for SSH the
/// output of `gpg.ssh.defaultKeyCommand` and for the other formats the
/// committer identity `ident`.
pub(crate) fn signing_key(
    settings: &Settings,
    sign: &Sign,
    ident: &str,
) -> Result<String, SignError> {
    if let Sign::Key(k) = sign {
        return Ok(k.clone());
    }
    match &settings.signing_key {
        Some(k) if !k.is_empty() || settings.format != Format::Ssh => return Ok(k.clone()),
        Some(_) => {
            // Set but empty: the default key command is not asked.
            eprintln!("error: {}", msg::SSH_SIGNING_KEY_UNSET);
            return Err(SignError::Failed);
        }
        None => {}
    }
    if settings.format != Format::Ssh {
        return Ok(ident.to_owned());
    }
    let Some(command) = &settings.default_key_command else {
        return Err(SignError::Die(Die::Fatal(msg::SSH_KEY_NEEDED.to_owned())));
    };
    let out = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .output();
    let (ok, stdout, stderr) = match out {
        Ok(o) => (o.status.success(), o.stdout, o.stderr),
        Err(_) => (false, Vec::new(), Vec::new()),
    };
    let (stdout, stderr) = (lossy(&stdout), lossy(&stderr));
    if !ok {
        eprintln!(
            "warning: {}",
            msg::default_key_command_failed(&stderr, &stdout)
        );
    } else if let Some(key) = stdout.lines().find(|l| l.starts_with("key::")) {
        return Ok(key.to_owned());
    } else {
        eprintln!(
            "warning: {}",
            msg::default_key_command_no_keys(&stderr, &stdout)
        );
    }
    eprintln!("error: {}", msg::SSH_SIGNING_KEY_UNSET);
    Err(SignError::Failed)
}

/// Signs `payload` with `key` in the configured format and returns the
/// armoured signature (ending with a newline).
pub(crate) fn sign_buffer(
    settings: &Settings,
    payload: &[u8],
    key: &str,
) -> Result<Vec<u8>, SignError> {
    let program = settings.program(settings.format);
    if settings.format == Format::Ssh {
        return sign_ssh(&program, payload, key);
    }
    let (ok, signature, status) = run(&program, &["--status-fd=2", "-bsau", key], Some(payload));
    let status = if status.is_empty() {
        msg::NO_GPG_OUTPUT.as_bytes().to_vec()
    } else {
        status
    };
    let created = status.starts_with(b"[GNUPG:] SIG_CREATED ")
        || status.windows(22).any(|w| w == b"\n[GNUPG:] SIG_CREATED ");
    if !ok || !created || signature.is_empty() {
        eprintln!("error: {}", reported(&msg::sign_failed(&lossy(&status))));
        return Err(SignError::Failed);
    }
    Ok(normalize_newlines(signature))
}

/// Strips carriage returns before newlines (programs on some systems
/// write CRLF).
fn normalize_newlines(sig: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(sig.len());
    for (i, &b) in sig.iter().enumerate() {
        if b == b'\r' && sig.get(i + 1) == Some(&b'\n') {
            continue;
        }
        out.push(b);
    }
    out
}

/// A temporary file in the system's temporary directory.
pub(crate) fn temp_file(prefix: &str, data: &[u8]) -> Result<rgit_fs::TempFile, String> {
    let mut f =
        rgit_fs::TempFile::new_in(std::env::temp_dir(), prefix).map_err(|e| e.to_string())?;
    f.write_all(data).map_err(|e| e.to_string())?;
    f.flush().map_err(|e| e.to_string())?;
    Ok(f)
}

fn sign_ssh(program: &str, payload: &[u8], key: &str) -> Result<Vec<u8>, SignError> {
    let literal = key
        .strip_prefix("key::")
        .or_else(|| key.starts_with("ssh-").then_some(key));
    let key_file = match literal {
        Some(k) => match temp_file(".git_signing_key_tmp", k.as_bytes()) {
            Ok(f) => Some(f),
            Err(e) => {
                eprintln!("error: {}", msg::cannot_create_temp(&e));
                return Err(SignError::Failed);
            }
        },
        None => None,
    };
    let buffer = match temp_file(".git_signing_buffer_tmp", payload) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: {}", msg::cannot_create_temp(&e));
            return Err(SignError::Failed);
        }
    };
    let key_path = key_file
        .as_ref()
        .map_or_else(|| expand_home(key), |f| f.path().display().to_string());
    let buffer_path = buffer.path().display().to_string();
    let mut args = vec!["-Y", "sign", "-n", "git", "-f", key_path.as_str()];
    if key_file.is_some() {
        args.push("-U");
    }
    args.push(&buffer_path);
    let (ok, _, stderr) = run(program, &args, None);
    let sig_path = format!("{buffer_path}.sig");
    if !ok {
        let _ = std::fs::remove_file(&sig_path);
        let stderr = lossy(&stderr);
        if stderr.contains("usage:") {
            eprintln!("error: {}", msg::SSH_SIGN_NEEDED);
        } else {
            eprintln!("error: {}", reported(&stderr));
        }
        return Err(SignError::Failed);
    }
    match std::fs::read(&sig_path) {
        Ok(sig) => {
            let _ = std::fs::remove_file(&sig_path);
            Ok(normalize_newlines(sig))
        }
        Err(e) => {
            eprintln!(
                "error: {}",
                msg::failed_reading_ssh_signature(&sig_path, &rgit_messages::strerror(&e))
            );
            Err(SignError::Failed)
        }
    }
}

/// Runs `program args...`, feeding `input` (when given) to its standard
/// input: (success, standard output, standard error). A program that
/// cannot be started prints git's `cannot exec` message.
pub(crate) fn run(program: &str, args: &[&str], input: Option<&[u8]>) -> (bool, Vec<u8>, Vec<u8>) {
    let child = Command::new(program)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "{}",
                msg::cannot_exec(program, &rgit_messages::strerror(&e))
            );
            return (false, Vec::new(), Vec::new());
        }
    };
    // A program that does not read its input is judged by its exit.
    let writer = match (child.stdin.take(), input) {
        (Some(mut stdin), Some(data)) => {
            let data = data.to_vec();
            Some(std::thread::spawn(move || {
                let _ = stdin.write_all(&data);
            }))
        }
        _ => None,
    };
    let out = child.wait_with_output();
    if let Some(w) = writer {
        let _ = w.join();
    }
    match out {
        Ok(o) => (o.status.success(), o.stdout, o.stderr),
        Err(_) => (false, Vec::new(), Vec::new()),
    }
}
