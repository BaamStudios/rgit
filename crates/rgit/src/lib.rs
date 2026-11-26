// SPDX-License-Identifier: MIT
//! rgit as a library.
//!
//! This crate is the documented entry point for Rust programs that work
//! with git repositories through rgit: servers, editors, build tools. It
//! is a small facade over the `rgit-*` crates, which stay free to change;
//! the design and the plan are in `docs/adr/0005-library-api.md`.
//!
//! ```no_run
//! # fn main() -> rgit::Result<()> {
//! use rgit::{LogOptions, Repository, StatusOptions};
//!
//! let repo = Repository::open("/path/to/checkout")?;
//!
//! // Where are we?
//! let head = repo.head()?;
//! println!("on {:?}", head.shorthand());
//!
//! // What has changed?
//! for entry in repo.status(StatusOptions::new())?.entries {
//!     println!("{} {}", String::from_utf8_lossy(&entry.code()), entry.path);
//! }
//!
//! // What happened lately?
//! for commit in repo.log(LogOptions::new().max_count(5))? {
//!     let commit = commit?;
//!     println!("{:.7} {}", commit.id, commit.summary());
//! }
//!
//! // Any revision expression of gitrevisions(7).
//! let id = repo.rev_parse("HEAD~1^{tree}")?;
//! # let _ = id;
//! # Ok(())
//! # }
//! ```
//!
//! # What a library call does not do
//!
//! - **It does not print.** Messages the `git` command would write to
//!   standard error come back as [`Diagnostic`]s
//!   ([`Repository::take_diagnostics`], [`Error::diagnostics`]).
//! - **It does not exit or panic on bad input.** Every failure is an
//!   [`Error`]; a damaged repository is ordinary input.
//! - **It does not depend on the process.** The repository is the
//!   directory given to [`Repository::open`]; the current directory and
//!   `GIT_DIR` play no part. Environment variables are read through the
//!   [`Environment`] of [`OpenOptions`], which [`OpenOptions::isolated`]
//!   leaves empty.
//! - **It does not write unless asked to.** The operations of this
//!   version only read; [`Repository::status`] does not refresh the index
//!   file.
//!
//! # Names are byte strings
//!
//! git does not require reference names and file names to be UTF-8, so
//! this API hands them out as [`BString`] and takes `impl AsRef<[u8]>`
//! (a `&str` works). Paths in the file system are [`std::path::Path`].
//!
//! # Threads
//!
//! [`Repository`] is `Send + Sync`: open once, use from many threads.
//! Long operations take a [`Cancel`] token in their options.
//!
//! # Stability
//!
//! The crate is at step 1 of ADR 0005 and is not published yet. Until
//! version 1.0, minor versions (`0.x`) may change the API; enums and
//! structs that will grow are `#[non_exhaustive]` already.

mod cancel;
mod convert;
mod error;
mod log;
mod messages;
mod refs;
mod repository;
mod revision;
mod status;

pub use cancel::Cancel;
pub use error::{Diagnostic, Error, Level, Result, Source};
pub use log::{Commit, Log, LogOptions, Order, Signature, Time};
pub use refs::{Head, Reference, Target};
pub use repository::{OpenOptions, Repository};
pub use status::{Change, Conflict, State, Status, StatusEntry, StatusOptions, UntrackedFiles};

/// Byte strings: the type of reference names and repository paths.
pub use bstr::{BStr, BString};
/// The environment a repository is opened with, and two implementations:
/// the process ([`SystemEnv`]) and a fixed map ([`MapEnv`]).
pub use rgit_config::{Environment, MapEnv, SystemEnv};
/// Object names.
pub use rgit_hash::{HashKind, ObjectId};

// A repository handle is shared between threads (ADR 0005, "Threads").
const _: () = {
    const fn shared<T: Send + Sync>() {}
    shared::<Repository>();
    shared::<OpenOptions>();
    shared::<Error>();
    shared::<Cancel>();
};
