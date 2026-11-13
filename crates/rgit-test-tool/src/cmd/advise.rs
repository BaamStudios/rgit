// SPDX-License-Identifier: MIT

//! `advise <advice>` (t0018): show `<advice>` as the `nestedTag` advice
//! through `rgit-messages`.
//!
//! Observed behaviour of the reference binary:
//!
//! - Anything but exactly one argument: `fatal: usage: advise <advice>`,
//!   exit 128 (checked before the repository is looked up).
//! - Outside a repository: the usual `fatal: not a git repository` error.
//! - Otherwise the argument is printed like any advice (`hint: ` lines, the
//!   "Disable this message" line while `advice.nestedTag` is unset, nothing
//!   with `advice.nestedTag=false` or `GIT_ADVICE=0`), exit 0.

use rgit_config::SystemEnv;
use rgit_messages::{Advice, Advisor};

use crate::cmd::config::{fatal, load, print_fatal, report};
use crate::messages;
use crate::repo::{self, SetupError};

pub fn advise(args: &[String]) -> i32 {
    let [text] = args else {
        return fatal(messages::ADVISE_USAGE);
    };
    let repo = match repo::discover(true) {
        Ok(r) => r,
        Err(SetupError::NotFound) => return fatal(messages::NOT_A_REPOSITORY),
        Err(SetupError::FilesystemBoundary(dir)) => {
            return fatal(messages::not_a_repository_fs(&dir.display().to_string()));
        }
        Err(SetupError::Format { reason, .. }) => return fatal(reason),
        Err(SetupError::Fatal(lines)) => return print_fatal(&lines),
    };
    let set = match load(Some(&repo), false) {
        Ok(s) => s,
        Err(e) => return report(&e),
    };
    let result = Advisor::new(&set, &SystemEnv::default(), false)
        .and_then(|a| a.advise(Advice::NestedTag, text));
    match result {
        Ok(()) => 0,
        Err(e) => fatal(e),
    }
}
