// SPDX-License-Identifier: MIT
//! Built-in commands.
//!
//! To add a command, create `src/commands/<name>.rs` (dashes in the command
//! name become underscores in the file name) with
//!
//! ```ignore
//! use crate::prelude::*;
//!
//! pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
//!     // argv[0] is the command name
//!     Ok(0)
//! }
//! ```
//!
//! `build.rs` picks the file up and registers it; no other file changes.

use crate::{CmdResult, Context};

/// A built-in command.
pub struct Command {
    /// Name as typed after `git`.
    pub name: &'static str,
    /// Entry point; `argv[0]` is the command name.
    pub run: fn(&mut Context, &[String]) -> CmdResult,
}

include!(concat!(env!("OUT_DIR"), "/commands.rs"));

/// Look up a built-in command by name.
pub fn find(name: &str) -> Option<&'static Command> {
    COMMANDS
        .binary_search_by(|c| c.name.cmp(name))
        .ok()
        .map(|i| &COMMANDS[i])
}
