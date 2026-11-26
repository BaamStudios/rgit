// SPDX-License-Identifier: MIT
//! Where the command line meets the library (`rgit`, ADR 0005).
//!
//! The library returns typed errors and never prints. This module is the
//! one place that turns such an error into what a command does with it:
//! the lines git prints before the `fatal:` line go to standard error
//! here, the message and the exit code travel on as a [`Die`].
//!
//! Commands that are moved onto the library use `?` on its results.

use std::io::Write;

use crate::Die;

/// End the command for the library error `e`: its diagnostics are printed
/// now, [`Die::emit`] prints `fatal: <message>` and gives exit code 128.
pub fn die(e: &rgit::Error) -> Die {
    let mut err = std::io::stderr().lock();
    for diagnostic in e.diagnostics() {
        let _ = writeln!(err, "{diagnostic}");
    }
    match e.exit_code() {
        rgit_messages::EXIT_FATAL => Die::Fatal(e.to_string()),
        // Not a `fatal:` of git (a cancelled operation): only the code.
        code => Die::Exit(code),
    }
}

impl From<rgit::Error> for Die {
    fn from(e: rgit::Error) -> Die {
        die(&e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_errors_end_a_command_like_git() {
        match Die::from(rgit::Error::BareRepository) {
            Die::Fatal(message) => {
                assert_eq!(message, "this operation must be run in a work tree");
            }
            other => panic!("{other:?}"),
        }
        match Die::from(rgit::Error::RevisionNotFound {
            spec: "topic".into(),
        }) {
            Die::Fatal(message) => assert_eq!(message, "bad revision 'topic'"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(Die::from(rgit::Error::Cancelled), Die::Exit(130)));
    }
}
