// SPDX-License-Identifier: MIT

//! User-visible messages of this crate, verbatim as git prints them (see
//! CLEANROOM.md, "User-visible messages").

/// The error for a commit message with a NUL byte, which git refuses to
/// store (without the `error: ` prefix).
pub const NUL_IN_MESSAGE: &str = "a NUL byte in commit log message not allowed.";

/// The warning commands print when a commit message is not UTF-8 and no
/// `i18n.commitEncoding` is set (without a prefix).
pub const NOT_UTF8_WARNING: &str = "Warning: commit message did not conform to UTF-8.\n\
You may want to amend it after fixing the message, or set the config\n\
variable i18n.commitEncoding to the encoding your project uses.";
