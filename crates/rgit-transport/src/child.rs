// SPDX-License-Identifier: MIT
//! A [`Connection`] over the standard input and output of a child process
//! (local service programs and `ssh`).

use std::io::{self, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};

use crate::command::Command;
use crate::sigpipe::IgnoreSigpipe;
use crate::{Connection, Error, Result, WriteHalf, messages};

/// A running child process speaking the protocol on stdin/stdout. Its
/// stderr is inherited, so ssh password prompts and remote messages reach
/// the user directly.
#[derive(Debug)]
pub struct ChildConnection {
    child: Child,
    stdin: Stdin,
    stdout: ChildStdout,
    /// A service that exits early (a fake `--upload-pack`, a failing
    /// server) must not kill rgit with SIGPIPE: writes fail with EPIPE.
    _sigpipe: IgnoreSigpipe,
}

impl ChildConnection {
    /// Trace and start `cmd` with piped stdin/stdout.
    pub fn spawn(cmd: &Command) -> Result<ChildConnection> {
        cmd.trace();
        let mut c = cmd.to_std();
        c.stdin(Stdio::piped()).stdout(Stdio::piped());
        let mut child = c.spawn().map_err(|e| spawn_error(cmd, e))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(Error::Other(messages::UNABLE_TO_FORK.into()));
        };
        Ok(ChildConnection {
            child,
            stdin: Stdin(Some(stdin)),
            stdout,
            _sigpipe: IgnoreSigpipe::new(),
        })
    }
}

/// The child's standard input, closed early by [`WriteHalf::close_write`].
#[derive(Debug)]
struct Stdin(Option<ChildStdin>);

impl Write for Stdin {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match &mut self.0 {
            Some(s) => s.write(buf),
            None => Err(io::ErrorKind::BrokenPipe.into()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.0 {
            Some(s) => s.flush(),
            None => Ok(()),
        }
    }
}

impl WriteHalf for Stdin {
    fn close_write(&mut self) {
        self.0 = None;
    }
}

/// Report a failed start like git (`fatal: cannot exec '<prog>': <reason>`
/// on stderr) and return the error for the caller's final `fatal:` line.
pub(crate) fn spawn_error(cmd: &Command, e: io::Error) -> Error {
    let reason = rgit_trace::messages::strerror(&e);
    let _ = writeln!(
        io::stderr(),
        "fatal: {}",
        messages::cannot_exec(&cmd.program_display(), &reason)
    );
    Error::Spawn {
        message: messages::UNABLE_TO_FORK.into(),
        source: e,
    }
}

impl Read for ChildConnection {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stdout.read(buf)
    }
}

impl Write for ChildConnection {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stdin.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stdin.flush()
    }
}

impl Connection for ChildConnection {
    fn is_stateless(&self) -> bool {
        false
    }

    fn end_request(&mut self) -> Result<()> {
        self.stdin.flush()?;
        Ok(())
    }

    fn split(&mut self) -> (&mut (dyn Read + Send), &mut dyn WriteHalf) {
        (&mut self.stdout, &mut self.stdin)
    }

    fn close(self: Box<Self>) -> Result<()> {
        let ChildConnection {
            mut child,
            stdin,
            stdout,
            _sigpipe,
        } = *self;
        drop(stdin);
        drop(stdout);
        let status = child.wait()?;
        if status.success() {
            Ok(())
        } else {
            Err(Error::ServiceFailed(status.code()))
        }
    }
}
