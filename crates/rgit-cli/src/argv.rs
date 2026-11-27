// SPDX-License-Identifier: MIT
//! Command-line arguments that are not UTF-8.
//!
//! Commands receive their arguments as `String`s. Bytes of an argument that
//! are not valid UTF-8 (a commit message or an author name in ISO-8859-1,
//! EUC-JP or Shift_JIS given with `-m` / `--author`) are kept as private
//! use characters U+10FF00 + byte ([`rgit_bstr`]), so that [`bytes`] gives
//! the original argument back where it is stored verbatim.

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

/// `arg` as a `String`, invalid bytes escaped ([`rgit_bstr::from_os_string`]).
pub fn to_string(arg: OsString) -> String {
    rgit_bstr::from_os_string(arg)
}

/// The bytes of an argument converted with [`to_string`].
pub fn bytes(arg: &str) -> Vec<u8> {
    rgit_bstr::to_bytes(arg).into_owned()
}

/// Locked stdout that prints escaped names as their raw bytes, for output
/// made of names and fixed text ([`rgit_bstr::Unescape`]).
pub fn stdout() -> rgit_bstr::Unescape<std::io::StdoutLock<'static>> {
    rgit_bstr::Unescape::new(std::io::stdout().lock())
}

/// `println!` for a line that may hold escaped names: they are printed
/// as their raw bytes.
pub fn outln(line: &str) {
    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(&rgit_bstr::to_bytes(line));
    let _ = out.write_all(b"\n");
}

/// `eprintln!` for a message that may hold escaped names. Like the
/// crate's `eprintln!`, the line goes out in a single write: a parent that
/// relays our stderr packet by packet (`upload-archive`) must not see it
/// in pieces.
pub fn errln(line: &str) {
    use std::io::Write as _;
    let mut bytes = rgit_bstr::to_bytes(line).into_owned();
    bytes.push(b'\n');
    let _ = std::io::stderr().lock().write_all(&bytes);
}

/// An argument for a child process. Strings may hold escaped bytes and
/// are passed as their raw bytes ([`rgit_bstr::to_os`]), so that a child
/// rgit reads back the same argument; paths and `OsStr`s go as they are.
pub trait ChildArg {
    fn child_arg(&self) -> Cow<'_, OsStr>;
}

impl ChildArg for str {
    fn child_arg(&self) -> Cow<'_, OsStr> {
        rgit_bstr::to_os(self)
    }
}

impl ChildArg for String {
    fn child_arg(&self) -> Cow<'_, OsStr> {
        rgit_bstr::to_os(self)
    }
}

impl ChildArg for Cow<'_, str> {
    fn child_arg(&self) -> Cow<'_, OsStr> {
        rgit_bstr::to_os(self)
    }
}

impl ChildArg for OsStr {
    fn child_arg(&self) -> Cow<'_, OsStr> {
        Cow::Borrowed(self)
    }
}

impl ChildArg for OsString {
    fn child_arg(&self) -> Cow<'_, OsStr> {
        Cow::Borrowed(self)
    }
}

impl ChildArg for Path {
    fn child_arg(&self) -> Cow<'_, OsStr> {
        Cow::Borrowed(self.as_os_str())
    }
}

impl ChildArg for PathBuf {
    fn child_arg(&self) -> Cow<'_, OsStr> {
        Cow::Borrowed(self.as_os_str())
    }
}

impl<T: ChildArg + ?Sized> ChildArg for &T {
    fn child_arg(&self) -> Cow<'_, OsStr> {
        (**self).child_arg()
    }
}

/// `Command::arg` / `Command::args` taking [`ChildArg`]s: what spawns a
/// child rgit (or any program) with arguments that may hold escapes.
pub trait CommandArgs {
    fn raw_arg<A: ChildArg>(&mut self, arg: A) -> &mut Self;
    fn raw_args<I>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator,
        I::Item: ChildArg;
}

impl CommandArgs for Command {
    fn raw_arg<A: ChildArg>(&mut self, arg: A) -> &mut Self {
        self.arg(arg.child_arg())
    }

    fn raw_args<I>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator,
        I::Item: ChildArg,
    {
        for a in args {
            self.arg(a.child_arg());
        }
        self
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::*;

    #[cfg(unix)]
    #[test]
    fn round_trip() {
        use std::os::unix::ffi::OsStringExt;
        let raw = b"caf\xe9 \xc3\xa9 \xa4\xa2".to_vec();
        let s = to_string(OsString::from_vec(raw.clone()));
        assert!(s.contains('\u{e9}'));
        assert_eq!(bytes(&s), raw);
        assert_eq!(bytes("plain"), b"plain");
    }

    #[cfg(unix)]
    #[test]
    fn child_args_are_raw() {
        use std::os::unix::ffi::OsStrExt;
        let s = to_string(std::ffi::OsStr::from_bytes(b"m\xe9").to_owned());
        let mut cmd = Command::new("true");
        cmd.raw_arg(&s)
            .raw_args([s.as_str()])
            .raw_arg(Path::new("p"));
        let got: Vec<&[u8]> = cmd.get_args().map(OsStr::as_bytes).collect();
        assert_eq!(got, [&b"m\xe9"[..], b"m\xe9", b"p"]);
    }
}
