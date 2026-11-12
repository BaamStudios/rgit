// SPDX-License-Identifier: MIT
//! Reading and writing git configuration: the file syntax, typed values,
//! scopes, includes and in-place edits.
//!
//! - [`parse`]: lossless parser for the file format ([`File`], [`Item`]),
//!   suitable for a formatting-preserving writer.
//! - [`key`]: validation of variable names given by the user.
//! - [`value`]: `bool`, `int` (with `k`/`m`/`g`), `bool-or-int`, `path`.
//! - [`ConfigSet`], [`Loader`]: the merged view with the origin and scope
//!   of every value, `include.path` and `includeIf` (`gitdir`, `gitdir/i`,
//!   `worktree`, `onbranch`, `hasconfig:remote.*.url`). Repository facts come
//!   in through the [`RepoFacts`] trait, the environment through
//!   [`Environment`].
//! - [`cmdline`]: `-c`, `--config-env`, `GIT_CONFIG_PARAMETERS` and
//!   `GIT_CONFIG_COUNT`.
//! - [`sources`]: the standard stack (system, global, local, worktree,
//!   command line).
//! - [`edit`], [`write`]: set/add/replace-all/unset/unset-all and
//!   rename/remove-section, keeping everything else in the file as it was,
//!   written through `<file>.lock`.
//!
//! Behaviour follows git's documentation (`git-config(1)`, `git-config`
//! "CONFIGURATION FILE") and was checked against the `git` binary.

pub mod cmdline;
pub mod edit;
pub mod env;
mod error;
mod glob;
pub mod include;
pub mod key;
mod messages;
mod origin;
pub mod parse;
mod set;
pub mod sources;
pub mod value;
pub mod write;

pub use cmdline::CommandLine;
pub use edit::{EditError, SetRequest, ValuePattern};
pub use env::{Environment, MapEnv, SystemEnv};
pub use error::{Error, Level, ValueError, ValueErrorKind};
pub use include::{RepoFacts, StaticRepoFacts};
pub use key::{Key, KeyError};
pub use origin::{Origin, Scope, quote_c_style};
pub use parse::{File, Item, SyntaxError};
pub use set::{ConfigSet, Entry, LoadOptions, Loader, Source, SourceSpec, parse_bytes};
pub use value::{BoolOrInt, IntError, IntWidth};
pub use write::WriteError;
