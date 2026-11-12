// SPDX-License-Identifier: MIT
//! Repository discovery and layout for rgit.
//!
//! - [`setup`]: find the git directory and work tree for a command from
//!   `GIT_DIR`, `GIT_WORK_TREE`, `GIT_COMMON_DIR`, `core.worktree`,
//!   `core.bare` and the upward search (`.git` directories and files, bare
//!   repositories, `GIT_CEILING_DIRECTORIES`,
//!   `GIT_DISCOVERY_ACROSS_FILESYSTEM`), and compute the prefix.
//! - [`safe`]: ownership checks with `safe.directory`, and
//!   `safe.bareRepository`.
//! - [`format`]: `core.repositoryFormatVersion` and `extensions.*`; unknown
//!   extensions are refused like git does, known ones rgit cannot handle yet
//!   are refused before anything is touched.
//! - [`layout`]: what a repository directory looks like, `.git` files.
//! - [`init`]: creating and reinitialising repositories (`git init`),
//!   with rgit's own default template.
//!
//! Behaviour follows git's documentation (`git(1)` ENVIRONMENT,
//! `gitrepository-layout(5)`, `git-config(1)` `safe.*`/`core.*`/
//! `extensions.*`, `technical/repository-version`) and was checked against
//! the git binary (`GIT_TRACE_SETUP`). The library never changes the process
//! working directory; [`Setup::cwd`] says where a command should run.

mod error;
pub mod format;
pub mod init;
pub mod layout;
mod messages;
pub mod path;
pub mod safe;
pub mod setup;

pub use error::{Error, GitfileError, Level, NotFound};
pub use format::{RefStorage, RepositoryFormat, Support};
pub use setup::{ProtectedConfig, Setup, SetupOptions, StandardProtectedConfig, discover, setup};
