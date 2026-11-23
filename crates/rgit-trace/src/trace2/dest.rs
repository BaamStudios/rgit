// SPDX-License-Identifier: MIT
//! trace2 destinations (Documentation/trace2-target-values.adoc).
//!
//! * `0`, `false` (any case), empty: off
//! * `1`, `true` (any case): standard error
//! * a single digit `2`..=`9`: that already open file descriptor
//! * an absolute path: append to that file, or, when it is an existing
//!   directory, to a new file in it named after the last component of the
//!   session id (`trace2.maxFiles` limits the number of files there)
//! * `af_unix:[stream:|dgram:]<absolute path>`: a Unix domain socket
//!
//! Anything else is warned about and the target stays off. Failures to open
//! a destination are reported only with `GIT_TRACE2_DST_DEBUG`.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::messages as m;

/// An open trace2 destination.
pub(crate) enum Dest {
    Stderr,
    Fd(i32),
    File(File),
    #[cfg(unix)]
    Stream(std::os::unix::net::UnixStream),
    #[cfg(unix)]
    Dgram(std::os::unix::net::UnixDatagram),
}

impl Dest {
    /// Write one complete event (a single write where possible).
    pub(crate) fn write_event(&mut self, buf: &[u8]) -> io::Result<()> {
        match self {
            Dest::Stderr => io::stderr().lock().write_all(buf),
            Dest::Fd(fd) => crate::key::write_fd(*fd, buf),
            Dest::File(f) => f.write_all(buf),
            #[cfg(unix)]
            Dest::Stream(s) => s.write_all(buf),
            #[cfg(unix)]
            Dest::Dgram(s) => s.send(buf).map(drop),
        }
    }
}

/// Where the value of a target setting points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Spec {
    Off,
    Stderr,
    Fd(i32),
    Path(PathBuf),
    Socket {
        kind: Option<SocketKind>,
        path: PathBuf,
    },
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SocketKind {
    Stream,
    Dgram,
}

impl Spec {
    pub(crate) fn parse(value: &str) -> Spec {
        if value.is_empty() || value == "0" || value.eq_ignore_ascii_case("false") {
            return Spec::Off;
        }
        if value == "1" || value.eq_ignore_ascii_case("true") {
            return Spec::Stderr;
        }
        if let [d @ b'2'..=b'9'] = value.as_bytes() {
            return Spec::Fd(i32::from(d - b'0'));
        }
        if value.starts_with('/') {
            return Spec::Path(PathBuf::from(value));
        }
        if let Some(rest) = value.strip_prefix("af_unix:") {
            let (kind, path) = if let Some(p) = rest.strip_prefix("stream:") {
                (Some(SocketKind::Stream), p)
            } else if let Some(p) = rest.strip_prefix("dgram:") {
                (Some(SocketKind::Dgram), p)
            } else {
                (None, rest)
            };
            if path.starts_with('/') {
                return Spec::Socket {
                    kind,
                    path: PathBuf::from(path),
                };
            }
        }
        Spec::Unknown
    }
}

/// What opening a directory target produced.
pub(crate) enum Opened {
    /// The destination to write events to.
    Ready(Dest),
    /// The directory holds too many files: a sentinel was created and gets
    /// the `version` and `too_many_files` events, nothing else.
    Discard(Dest),
    /// Nothing is written.
    None,
}

/// The name of the sentinel file in a full trace directory.
pub(crate) const DISCARD_FILE: &str = "git-trace2-discard";

/// Open the destination `spec` (from the variable `name`) for a process
/// whose session id ends in `sid_tail`.
pub(crate) fn open(
    name: &str,
    value: &str,
    spec: &Spec,
    sid_tail: &str,
    max_files: usize,
    debug: bool,
) -> Opened {
    match spec {
        Spec::Off => Opened::None,
        Spec::Unknown => {
            eprint!("{}", m::trace2_unknown_value(name, value));
            Opened::None
        }
        Spec::Stderr => Opened::Ready(Dest::Stderr),
        Spec::Fd(2) => Opened::Ready(Dest::Stderr),
        Spec::Fd(fd) => Opened::Ready(Dest::Fd(*fd)),
        Spec::Path(p) if p.is_dir() => open_in_dir(name, p, sid_tail, max_files, debug),
        Spec::Path(p) => match append(p) {
            Ok(f) => Opened::Ready(Dest::File(f)),
            Err(e) => {
                if debug {
                    eprint!("{}", m::trace2_cannot_open(&p.to_string_lossy(), name, &e));
                }
                Opened::None
            }
        },
        Spec::Socket { kind, path } => match connect(*kind, path) {
            Ok(d) => Opened::Ready(d),
            Err(e) => {
                if debug {
                    eprint!(
                        "{}",
                        m::trace2_cannot_connect(&path.to_string_lossy(), name, &e)
                    );
                }
                Opened::None
            }
        },
    }
}

fn append(p: &Path) -> io::Result<File> {
    OpenOptions::new().append(true).create(true).open(p)
}

fn open_in_dir(name: &str, dir: &Path, sid_tail: &str, max_files: usize, debug: bool) -> Opened {
    if max_files > 0 {
        if dir.join(DISCARD_FILE).exists() {
            return Opened::None;
        }
        let count = std::fs::read_dir(dir).map_or(0, |d| d.count());
        if count >= max_files {
            return match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dir.join(DISCARD_FILE))
            {
                Ok(f) => Opened::Discard(Dest::File(f)),
                Err(_) => Opened::None,
            };
        }
    }
    let mut path = dir.join(sid_tail);
    let mut n = 0;
    while path.exists() {
        n += 1;
        path = dir.join(format!("{sid_tail}.{n}"));
    }
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(f) => Opened::Ready(Dest::File(f)),
        Err(e) => {
            if debug {
                eprint!(
                    "{}",
                    m::trace2_cannot_open(&path.to_string_lossy(), name, &e)
                );
            }
            Opened::None
        }
    }
}

#[cfg(unix)]
fn connect(kind: Option<SocketKind>, path: &Path) -> io::Result<Dest> {
    use std::os::unix::net::{UnixDatagram, UnixStream};
    let stream = || UnixStream::connect(path).map(Dest::Stream);
    let dgram = || {
        let s = UnixDatagram::unbound()?;
        s.connect(path)?;
        Ok(Dest::Dgram(s))
    };
    match kind {
        Some(SocketKind::Stream) => stream(),
        Some(SocketKind::Dgram) => dgram(),
        None => stream().or_else(|_| dgram()),
    }
}

#[cfg(not(unix))]
fn connect(_kind: Option<SocketKind>, _path: &Path) -> io::Result<Dest> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Unix domain sockets are not supported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values() {
        for off in ["", "0", "false", "FALSE"] {
            assert_eq!(Spec::parse(off), Spec::Off);
        }
        for on in ["1", "true", "True"] {
            assert_eq!(Spec::parse(on), Spec::Stderr);
        }
        assert_eq!(Spec::parse("2"), Spec::Fd(2));
        assert_eq!(Spec::parse("9"), Spec::Fd(9));
        assert_eq!(Spec::parse("10"), Spec::Unknown);
        assert_eq!(Spec::parse("rel"), Spec::Unknown);
        assert_eq!(Spec::parse("/x"), Spec::Path("/x".into()));
        assert_eq!(
            Spec::parse("af_unix:dgram:/s"),
            Spec::Socket {
                kind: Some(SocketKind::Dgram),
                path: "/s".into()
            }
        );
        assert_eq!(
            Spec::parse("af_unix:/s"),
            Spec::Socket {
                kind: None,
                path: "/s".into()
            }
        );
        assert_eq!(Spec::parse("af_unix:s"), Spec::Unknown);
    }
}
