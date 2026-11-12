// SPDX-License-Identifier: MIT

//! Pathspecs and git's glob engine for rgit.
//!
//! - [`wildmatch`]: the glob matcher behind pathspecs, `.gitignore`,
//!   `.gitattributes` and `includeIf`. For a plain path glob use
//!   [`wildmatch::match_path_glob`].
//! - [`Pathspec`]: command line path limiting as described in
//!   gitglossary(7) under "pathspec": magic (`:(top)`, `:(literal)`,
//!   `:(glob)`, `:(icase)`, `:(exclude)`, short forms `:/`, `:!`, `:^`,
//!   `:(prefix:<n>)`, `:(attr:...)` parsed), paths relative to the current
//!   directory, the global switches ([`GlobalFlags`]) and matching with
//!   "seen" tracking and directory pruning.
//! - [`parse_pathspec_file`]: `--pathspec-from-file` input.
//!
//! All paths are byte strings relative to the top of the work tree, using
//! `/` as separator, without a trailing slash.
//!
//! ```
//! use rgit_pathspec::{GlobalFlags, ParseOptions, Pathspec};
//!
//! let opts = ParseOptions {
//!     prefix: b"src/".to_vec(),
//!     globals: GlobalFlags { icase: true, ..Default::default() },
//!     ..Default::default()
//! };
//! let ps = Pathspec::parse([":(glob)**/*.RS"], &opts).unwrap();
//! assert!(ps.matches(b"src/a/b.rs"));
//! assert!(!ps.matches(b"SRC/a/b.rs")); // the cwd part is case-sensitive
//! ```

mod attr;
mod error;
mod file;
mod global;
mod magic;
mod matching;
pub mod messages;
mod parse;
pub mod wildmatch;

pub use attr::{AttrRequirement, AttrSpec};
pub use error::PathspecError;
pub use file::{parse_pathspec_file, unquote_c_style};
pub use global::{ENV_GLOB, ENV_ICASE, ENV_LITERAL, ENV_NOGLOB, GlobalFlags};
pub use magic::{Magic, MagicWord};
pub use matching::{DirInterest, MatchKind, Pathspec};
pub use parse::{ImplicitBase, ParseOptions, PathspecItem};
