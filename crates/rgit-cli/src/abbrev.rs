// SPDX-License-Identifier: MIT
//! The `--[no-]abbrev[=<n>]` option shared by plumbing commands.

use std::cell::Cell;

use rgit_options::{CallbackError, Opt};

use crate::objects::MIN_ABBREV;
use crate::repository::Repo;

/// What `--abbrev` asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Abbrev {
    /// Full object names (no option, `--no-abbrev`, `--abbrev=0`).
    #[default]
    Full,
    /// `--abbrev` without a value: `core.abbrev` or the automatic length.
    Configured,
    /// `--abbrev=<n>`.
    Digits(usize),
}

impl Abbrev {
    /// The option, storing into `slot`.
    pub fn option<'a>(slot: &'a Cell<Abbrev>, help: &'a str) -> Opt<'a> {
        Opt::callback(None, "abbrev", Some("n"), help, move |arg, neg| {
            let choice = match (neg, arg) {
                (true, _) => Abbrev::Full,
                (false, None) => Abbrev::Configured,
                (false, Some(v)) => match v.parse::<i64>() {
                    Ok(n) if n <= 0 => Abbrev::Full,
                    Ok(n) => Abbrev::Digits(usize::try_from(n).unwrap_or(usize::MAX)),
                    Err(_) => {
                        return Err(CallbackError::Error(crate::messages::abbrev_not_numeric(
                            "abbrev",
                        )));
                    }
                },
            };
            slot.set(choice);
            Ok(())
        })
        .optional(None)
    }

    /// Minimum digits to show; 0 for full names.
    pub fn length(self, repo: &Repo) -> usize {
        match self {
            Abbrev::Full => 0,
            Abbrev::Configured => repo.default_abbrev(),
            Abbrev::Digits(n) => n.max(MIN_ABBREV),
        }
    }
}
