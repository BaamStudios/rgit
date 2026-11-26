// SPDX-License-Identifier: MIT
//! Where `rgit daemon` writes its log (`--log-destination`): standard
//! error (`[<pid>] <message>`), syslog or nowhere. Details are only
//! logged with `--verbose`, errors always.

use std::io::Write;

use super::sys::{self, Priority};
use crate::Die;

/// `--log-destination=<destination>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    Stderr,
    Syslog,
    None,
}

/// The log of one daemon process.
#[derive(Debug, Clone, Copy)]
pub struct Logger {
    pub destination: Destination,
    pub verbose: bool,
}

impl Logger {
    /// A detail (`--verbose`).
    pub fn info(&self, message: &str) {
        if self.verbose {
            self.write(Priority::Info, message);
        }
    }

    /// An error.
    pub fn error(&self, message: &str) {
        self.write(Priority::Error, message);
    }

    /// A fatal error: on standard error it is printed as `fatal:
    /// <message>` with exit code 128; with another destination it goes
    /// there and the process ends with 1, as git does.
    pub fn fatal(&self, message: impl Into<String>) -> Die {
        let message = message.into();
        match self.destination {
            Destination::Stderr => Die::Fatal(message),
            _ => {
                self.write(Priority::Error, &message);
                Die::Exit(1)
            }
        }
    }

    fn write(&self, priority: Priority, message: &str) {
        match self.destination {
            Destination::Stderr => {
                // One write, so lines of several processes do not mix.
                let line = format!("[{}] {message}\n", std::process::id());
                let _ = std::io::stderr().write_all(line.as_bytes());
            }
            Destination::Syslog => sys::syslog(priority, message),
            Destination::None => {}
        }
    }
}
