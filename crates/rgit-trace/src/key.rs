// SPDX-License-Identifier: MIT
//! A trace key: one `GIT_TRACE*` variable, resolved lazily on first use.

use std::ffi::OsStr;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::messages;
use crate::target::Target;

/// Source location shown in the second column of a trace line.
///
/// rgit prints the base name of the Rust source file and the line, so the
/// column never matches git's C file names; scripts must not rely on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Location {
    /// Source file, usually from `file!()`.
    pub file: &'static str,
    /// Line, usually from `line!()`.
    pub line: u32,
}

impl Location {
    /// The location of the caller of a `#[track_caller]` function.
    #[track_caller]
    pub fn caller() -> Location {
        let l = std::panic::Location::caller();
        Location {
            file: l.file(),
            line: l.line(),
        }
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let base = self.file.rsplit(['/', '\\']).next().unwrap_or(self.file);
        write!(f, "{base}:{}", self.line)
    }
}

enum Sink {
    Stderr,
    Fd(i32),
    File(File),
}

impl Sink {
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        match self {
            Sink::Stderr => io::stderr().write_all(buf),
            Sink::Fd(fd) => write_fd(*fd, buf),
            Sink::File(f) => f.write_all(buf),
        }
    }
}

/// Write to a raw, inherited descriptor without taking ownership of it.
#[cfg(unix)]
fn write_fd(fd: i32, mut buf: &[u8]) -> io::Result<()> {
    while !buf.is_empty() {
        // SAFETY: `write(2)` only reads `buf.len()` bytes from a valid slice.
        // The descriptor is an arbitrary integer chosen by the user; if it is
        // not open the call fails with EBADF, which is reported as an error.
        // No Rust object takes ownership of the descriptor, so it is never
        // closed by us.
        let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if n == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        buf = &buf[n as usize..];
    }
    Ok(())
}

#[cfg(not(unix))]
fn write_fd(_fd: i32, _buf: &[u8]) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "file descriptor targets are not supported on this platform",
    ))
}

struct State {
    enabled: AtomicBool,
    sink: Mutex<Option<Sink>>,
}

/// One `GIT_TRACE*` variable.
///
/// Declare keys as `static`; the environment is read the first time the key
/// is used. Checking [`TraceKey::enabled`] after that is a single atomic
/// load, so call sites stay cheap when tracing is off.
pub struct TraceKey {
    name: &'static str,
    state: OnceLock<State>,
}

impl TraceKey {
    /// A key for the environment variable `name`.
    pub const fn new(name: &'static str) -> TraceKey {
        TraceKey {
            name,
            state: OnceLock::new(),
        }
    }

    /// The environment variable name.
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Resolve the key from an explicit value instead of the environment.
    /// Has no effect if the key was already resolved. Meant for tests and
    /// for callers that compute the value themselves.
    pub fn init_from(&self, value: Option<&OsStr>) {
        self.state.get_or_init(|| self.resolve(value));
    }

    fn state(&self) -> &State {
        self.state
            .get_or_init(|| self.resolve(std::env::var_os(self.name).as_deref()))
    }

    fn resolve(&self, value: Option<&OsStr>) -> State {
        let sink = match Target::parse(value) {
            Target::Disabled => None,
            Target::Unknown(v) => {
                warn(&messages::unknown_value(self.name, &v));
                None
            }
            Target::Stderr => Some(Sink::Stderr),
            Target::Fd(fd) => Some(Sink::Fd(fd)),
            Target::File(path) => match OpenOptions::new().append(true).create(true).open(&path) {
                Ok(f) => Some(Sink::File(f)),
                Err(e) => {
                    warn(&messages::cannot_open(&path.to_string_lossy(), &e));
                    None
                }
            },
        };
        State {
            enabled: AtomicBool::new(sink.is_some()),
            sink: Mutex::new(sink),
        }
    }

    /// Whether this key currently produces output.
    #[inline]
    pub fn enabled(&self) -> bool {
        self.state().enabled.load(Ordering::Relaxed)
    }

    /// Turn the key off for the rest of the process (for example
    /// `GIT_TRACE_PACKET` once pack data starts).
    pub fn disable(&self) {
        let st = self.state();
        st.enabled.store(false, Ordering::Relaxed);
        if let Ok(mut s) = st.sink.lock() {
            *s = None;
        }
    }

    /// Write bytes exactly as given: no header, no newline. Used for
    /// verbatim keys such as `GIT_TRACE_PACKFILE`.
    pub fn write_raw(&self, buf: &[u8]) {
        if !self.enabled() {
            return;
        }
        let st = self.state();
        let mut guard = match st.sink.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let Some(sink) = guard.as_mut() else {
            return;
        };
        if let Err(e) = sink.write_all(buf) {
            *guard = None;
            st.enabled.store(false, Ordering::Relaxed);
            drop(guard);
            warn(&messages::cannot_write(self.name, &e));
        }
    }

    /// Write one trace line: header (unless `GIT_TRACE_BARE`), message and
    /// a newline if the message does not end in one.
    pub fn write_line(&self, loc: Location, msg: &[u8]) {
        if !self.enabled() {
            return;
        }
        let line = crate::format::line(crate::bare(), loc, msg);
        self.write_raw(&line);
    }

    /// Like [`TraceKey::write_line`] for formatted text; used by the
    /// [`trace!`](crate::trace) macro.
    pub fn write_fmt_line(&self, loc: Location, args: fmt::Arguments<'_>) {
        if !self.enabled() {
            return;
        }
        match args.as_str() {
            Some(s) => self.write_line(loc, s.as_bytes()),
            None => self.write_line(loc, args.to_string().as_bytes()),
        }
    }
}

impl fmt::Debug for TraceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TraceKey")
            .field("name", &self.name)
            .field(
                "enabled",
                &self.state.get().map(|s| s.enabled.load(Ordering::Relaxed)),
            )
            .finish()
    }
}

fn warn(text: &str) {
    let _ = io::stderr().write_all(text.as_bytes());
}
