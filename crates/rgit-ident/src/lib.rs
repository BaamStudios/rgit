// SPDX-License-Identifier: MIT
//! Who is acting and with which programs: author and committer identities,
//! and the user's editor and pager.
//!
//! - [`ident`]: name, address and time stamp of the author or committer,
//!   strict (new commits) or lenient (reflogs), with git's errors and the
//!   "Please tell me who you are" advice.
//! - [`editor`]: choosing the editor, sequence editor and pager, and
//!   running the editor on a file.
//! - [`system`]: the facts about the current user and host used to guess
//!   an identity.
//! - [`config`]: reading the configuration values involved, with git's
//!   errors for unusable values.

pub mod config;
pub mod editor;
pub mod ident;
pub mod messages;
pub mod system;

pub use config::Problem;
pub use ident::{Ident, IdentConfig, IdentError, Resolver, Role, Strictness, sanitize};
pub use system::UserInfo;
