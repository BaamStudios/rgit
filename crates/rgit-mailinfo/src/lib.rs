// SPDX-License-Identifier: MIT
//! E-mail handling for `git mailsplit`, `git mailinfo` and `git am`.
//!
//! Written from Documentation/git-mailsplit.adoc, git-mailinfo.adoc,
//! git-am.adoc and config/mailinfo.adoc; the many details the
//! documentation leaves open were determined by running the `git` binary
//! (see CLEANROOM.md and the notes in each module).
//!
//! - [`split`]: cutting an mbox into single messages.
//! - [`info()`]: taking one message apart into authorship, commit message
//!   and patch.
//! - [`charset`]: re-coding text between character sets.

pub mod charset;
mod from;
mod info;
pub mod messages;
mod rfc2047;
pub mod split;
mod subject;

pub use info::{Info, Options, QuotedCr, info};
