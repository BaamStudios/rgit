// SPDX-License-Identifier: MIT
//! Submodules: the shared library behind `status`, `diff`, `add`, `rm`,
//! `mv`, `checkout` and `git submodule`.
//!
//! Written from Documentation/gitmodules.adoc, gitsubmodules.adoc and
//! config/submodule.adoc; details the documentation leaves open (messages,
//! which values are fatal, the exact URL checks and relative URL
//! resolution) were observed from the `git` binary and the reference
//! `test-tool submodule*` helpers (see CLEANROOM.md).
//!
//! - [`Gitmodules`]: the parsed `.gitmodules` file, looked up by name or
//!   path ([`Submodule`]).
//! - [`settings`]: the effective `submodule.<name>.*` values, with the
//!   repository configuration overriding `.gitmodules`.
//! - [`is_active`]: whether a submodule is of interest
//!   (`submodule.<name>.active`, `submodule.active`, `submodule.<name>.url`).
//! - [`check_name`], [`check_url`], [`looks_like_option`]: the security
//!   checks for names and URLs.
//! - [`resolve_relative_url`]: `./` and `../` URLs relative to the
//!   superproject's remote.
//! - [`gitdir`]: where a submodule's repository lives (`.git/modules/<name>`
//!   or the submodule's own `.git`).

mod active;
mod config;
pub mod fsck;
pub mod gitdir;
pub mod messages;
mod name;
pub mod settings;
mod url;

pub use active::is_active;
pub use config::{
    Diagnostic, FetchRecurse, Gitmodules, Ignore, Mode, Origin, ParseError, Submodule, Update,
    parse_fetch_recurse, parse_ignore, parse_update,
};
pub use name::check_name;
pub use url::{
    RelativeUrlError, check_url, is_relative_url, looks_like_option, resolve_relative_url,
};

/// Name of the file that describes the submodules of a work tree.
pub const GITMODULES: &str = ".gitmodules";

#[cfg(test)]
mod tests;
