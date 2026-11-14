// SPDX-License-Identifier: MIT
//! Trailers: `<key>: <value>` lines at the end of a commit message.
//!
//! Written from Documentation/git-interpret-trailers.adoc and
//! Documentation/config/trailer.adoc; details the documentation leaves open
//! were checked against the `git` binary (see CLEANROOM.md).
//!
//! - [`Config`]: the `trailer.*` configuration.
//! - [`parse()`]: find the trailer block of a message and its lines
//!   ([`Info`]); used by `interpret-trailers`, `commit --trailer`/`-s`,
//!   `format-patch` and the `%(trailers)` placeholder.
//! - [`process()`]: what `git interpret-trailers` does to one input.
//! - [`append_signoff()`]: `commit -s`.
//! - [`command`]: running `trailer.<alias>.cmd` / `.command`.

pub mod command;
mod config;
pub mod messages;
mod parse;
mod process;
mod signoff;

pub use config::{Config, IfExists, IfMissing, Item, Where};
pub use parse::{
    Info, Line, ParseOptions, Trailer, divider_start, find_separator, message_end, parse,
};
pub use process::{
    NewTrailer, Options, add_trailers, format_trailer, parse_argument, process, unfold_value,
};
pub use signoff::append_signoff;

#[cfg(test)]
mod tests;
