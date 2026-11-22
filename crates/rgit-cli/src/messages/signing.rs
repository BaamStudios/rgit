// SPDX-License-Identifier: MIT
//! Messages of signing and verification (CLEANROOM.md: user-visible
//! strings are reproduced verbatim and collected here).

/// Shown instead of the signing program's standard error when it printed
/// nothing.
pub const NO_GPG_OUTPUT: &str = "(no gpg output)";

/// The signing program failed; `stderr` is its standard error.
pub fn sign_failed(stderr: &str) -> String {
    format!("gpg failed to sign the data:\n{stderr}")
}

pub fn cannot_exec(program: &str, reason: &str) -> String {
    format!("fatal: cannot exec '{program}': {reason}")
}

pub fn bad_format(value: &str) -> String {
    format!("invalid value for 'gpg.format': '{value}'")
}

pub fn bad_min_trust(value: &str) -> String {
    format!("invalid value for 'gpg.mintrustlevel': '{value}'")
}

/// The commit object could not be written (signing failed).
pub const FAILED_TO_WRITE_COMMIT: &str = "failed to write commit object";

pub const SSH_KEY_NEEDED: &str =
    "either user.signingkey or gpg.ssh.defaultKeyCommand needs to be configured";

pub const SSH_SIGNING_KEY_UNSET: &str = "user.signingKey needs to be set for ssh signing";

pub fn default_key_command_failed(stderr: &str, stdout: &str) -> String {
    format!("gpg.ssh.defaultKeyCommand failed: {stderr} {stdout}")
}

pub fn default_key_command_no_keys(stderr: &str, stdout: &str) -> String {
    format!("gpg.ssh.defaultKeyCommand succeeded but returned no keys: {stderr} {stdout}")
}

pub const SSH_SIGN_NEEDED: &str =
    "ssh-keygen -Y sign is needed for ssh signing (available in openssh version 8.2p1+)";

pub fn cannot_create_temp(reason: &str) -> String {
    format!("could not create temporary file: {reason}")
}

pub fn failed_reading_ssh_signature(path: &str, reason: &str) -> String {
    format!("failed reading ssh signing data buffer from '{path}': {reason}")
}

pub const CANNOT_CREATE_TEMP_SIG: &str = "could not create temporary file";

pub const ALLOWED_SIGNERS_NEEDED: &str =
    "gpg.ssh.allowedSignersFile needs to be configured and exist for ssh signature verification";

pub fn revocation_file_missing(path: &str) -> String {
    format!("ssh signing revocation file configured but not found: {path}")
}

/// The first words of `ssh-keygen -Y verify` for a trusted signature.
pub const SSH_GOOD_PREFIX: &str = "Good \"git\" signature for ";
/// The first words of `ssh-keygen -Y check-novalidate`.
pub const SSH_GOOD_UNTRUSTED_PREFIX: &str = "Good \"git\" signature with ";

// verify-commit / verify-tag

pub const VERIFY_COMMIT_USAGE: &[&str] =
    &["git verify-commit [-v | --verbose] [--raw] <commit>..."];
pub const VERIFY_TAG_USAGE: &[&str] =
    &["git verify-tag [-v | --verbose] [--format=<format>] [--raw] <tag>..."];
pub const HELP_VERBOSE_COMMIT: &str = "print commit contents";
pub const HELP_VERBOSE_TAG: &str = "print tag contents";
pub const HELP_RAW: &str = "print raw gpg status output";
pub const HELP_FORMAT: &str = "format to use for the output";

pub fn commit_not_found(name: &str) -> String {
    format!("commit '{name}' not found.")
}

pub fn tag_not_found(name: &str) -> String {
    format!("tag '{name}' not found.")
}

pub fn non_commit(name: &str, kind: &str) -> String {
    format!("{name}: cannot verify a non-commit object of type {kind}.")
}

pub fn non_tag(name: &str, kind: &str) -> String {
    format!("{name}: cannot verify a non-tag object of type {kind}.")
}

pub const NO_SIGNATURE: &str = "no signature found";

// tag

pub const UNABLE_TO_SIGN_TAG: &str = "unable to sign the tag";

// --show-signature on merges

pub fn merged_tag(name: &str) -> String {
    format!("merged tag '{name}'")
}

// merge --verify-signatures

pub fn no_gpg_signature(commit: &str) -> String {
    format!("Commit {commit} does not have a GPG signature.")
}

pub fn bad_gpg_signature(commit: &str, signer: &str) -> String {
    format!("Commit {commit} has a bad GPG signature allegedly by {signer}.")
}

pub fn untrusted_gpg_signature(commit: &str, signer: &str) -> String {
    format!("Commit {commit} has an untrusted GPG signature, allegedly by {signer}.")
}

pub fn good_gpg_signature(commit: &str, signer: &str) -> String {
    format!("Commit {commit} has a good GPG signature by {signer}")
}
