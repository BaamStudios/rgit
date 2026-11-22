// SPDX-License-Identifier: MIT
//! Messages of commit signing (CLEANROOM.md: user-visible strings are
//! reproduced verbatim and collected here).

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

pub fn not_supported_format(format: &str) -> String {
    format!("rgit: signing with gpg.format={format} is not supported yet (#51)")
}

/// The commit object could not be written (signing failed).
pub const FAILED_TO_WRITE_COMMIT: &str = "failed to write commit object";
