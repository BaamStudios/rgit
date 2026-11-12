// SPDX-License-Identifier: MIT
//! The local transport (local paths and `file://` URLs): runs the service
//! program on this machine.
//!
//! Like git, the command `<program> '<path>'` is run through the shell (so
//! `--upload-pack="git upload-pack"` works), without the repository-local
//! environment variables, and with `GIT_PROTOCOL` when a protocol version
//! other than 0 is requested.

use std::ffi::OsString;

use bstr::BString;

use crate::child::ChildConnection;
use crate::command::Command;
use crate::os::os_string;
use crate::ssh::remote_command;
use crate::{ConnectOptions, Connection, Result, Transport};

/// A transport to a repository on the local file system.
#[derive(Debug, Clone)]
pub struct LocalTransport {
    /// The path as written (relative paths are relative to the current
    /// directory of the child).
    pub path: BString,
    /// Extra environment for children (see [`Command::base_env`]).
    pub base_env: Vec<(OsString, OsString)>,
}

impl LocalTransport {
    /// The command [`Transport::connect`] runs.
    pub fn command_for(&self, options: &ConnectOptions) -> Command {
        let mut cmd = Command::shell(os_string(&remote_command(options.program(), &self.path)));
        cmd.base_env(&self.base_env);
        cmd.unset_local_env();
        if let Some(gp) = options.git_protocol() {
            cmd.env("GIT_PROTOCOL", gp);
        }
        cmd
    }
}

impl Transport for LocalTransport {
    fn connect(&mut self, options: &ConnectOptions) -> Result<Box<dyn Connection>> {
        let cmd = self.command_for(options);
        Ok(Box::new(ChildConnection::spawn(&cmd)?))
    }
}
