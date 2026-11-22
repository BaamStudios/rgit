// SPDX-License-Identifier: MIT
//! Signing commits with OpenPGP (`gpg`), the part of #51 that `commit`
//! and `rebase` need for `-S[<keyid>]`, `--no-gpg-sign` and
//! `commit.gpgSign`.
//!
//! The protocol comes from Documentation/config/gpg.adoc (`gpg.program`:
//! the payload is fed to `gpg -bsau <key>`, the armoured detached
//! signature is read from standard output), the key from
//! Documentation/config/user.adoc (`user.signingKey`, else the committer
//! identity) and the embedding from Documentation/gitformat-signature.adoc
//! (`gpgsig` header after the other headers; `gpgsig-sha256` in a SHA-256
//! repository, Documentation/technical/hash-function-transition.adoc).
//! Like git, the program gets `--status-fd=2` and a signature counts only
//! when the status lines report `SIG_CREATED` (observed with the `git`
//! binary and stub programs).
//!
//! Not yet: the `x509` and `ssh` formats and verification (#51).

use std::io::Write as _;
use std::process::{Command, Stdio};

use rgit_config::ConfigSet;
use rgit_hash::HashKind;
use rgit_object::{Commit, Header, Signature};

use crate::Die;
use crate::messages::signing as msg;

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

fn config_string(config: &ConfigSet, key: &str) -> Option<String> {
    config
        .string(key)
        .ok()
        .flatten()
        .map(|v| String::from_utf8_lossy(v).into_owned())
}

/// Why signing failed.
#[derive(Debug)]
pub(crate) enum SignError {
    /// The signing program failed; the message (with its standard error)
    /// has been printed.
    Failed,
    /// Something rgit cannot do yet or a configuration error.
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
    let format = config_string(config, "gpg.format");
    match format.as_deref() {
        None | Some("openpgp") => {}
        Some(f @ ("x509" | "ssh")) => {
            return Err(SignError::Die(Die::Fatal(msg::not_supported_format(f))));
        }
        Some(other) => {
            eprintln!("error: {}", msg::bad_format(other));
            return Err(SignError::Die(Die::Exit(128)));
        }
    }
    // `gpg.program` is a synonym of `gpg.openpgp.program`: the last one
    // set wins.
    let program = config
        .entries()
        .iter()
        .rev()
        .find(|e| {
            let n: &[u8] = e.name.as_ref();
            n == b"gpg.program" || n == b"gpg.openpgp.program"
        })
        .and_then(|e| e.value.as_ref())
        .map(|v| String::from_utf8_lossy(v.as_ref()).into_owned())
        .unwrap_or_else(|| "gpg".to_owned());
    let key = match sign {
        Sign::Key(k) => k.clone(),
        Sign::Default => config_string(config, "user.signingkey").unwrap_or_else(|| {
            format!(
                "{} <{}>",
                String::from_utf8_lossy(&committer.name),
                String::from_utf8_lossy(&committer.email)
            )
        }),
    };
    let payload = commit.serialize();
    let (ok, signature, status) = run_signer(&program, &key, &payload);
    let status = if status.is_empty() {
        msg::NO_GPG_OUTPUT.as_bytes().to_vec()
    } else {
        status
    };
    let created = status.starts_with(b"[GNUPG:] SIG_CREATED ")
        || status.windows(22).any(|w| w == b"\n[GNUPG:] SIG_CREATED ");
    if !ok || !created || signature.is_empty() {
        eprintln!(
            "error: {}",
            msg::sign_failed(&String::from_utf8_lossy(&status))
        );
        return Err(SignError::Failed);
    }
    let mut value = signature;
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

/// Runs `<program> --status-fd=2 -bsau <key>` on `payload`: (success,
/// standard output, standard error).
fn run_signer(program: &str, key: &str, payload: &[u8]) -> (bool, Vec<u8>, Vec<u8>) {
    let child = Command::new(program)
        .args(["--status-fd=2", "-bsau", key])
        .stdin(Stdio::piped())
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
    if let Some(mut stdin) = child.stdin.take() {
        // A program that does not read its input is judged by its exit.
        let data = payload.to_vec();
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&data);
        });
        let out = child.wait_with_output();
        let _ = writer.join();
        return match out {
            Ok(o) => (o.status.success(), o.stdout, o.stderr),
            Err(_) => (false, Vec::new(), Vec::new()),
        };
    }
    (false, Vec::new(), Vec::new())
}
