// SPDX-License-Identifier: MIT
//! The form of paths on the platforms rgit runs on.
//!
//! git keeps paths in one form on every platform: components are joined
//! with `/`. Git for Windows converts what the system reports (the current
//! directory, resolved paths) to that form and never shows the `\\?\`
//! prefix; strings that come from the user (environment variables,
//! configuration values, command-line arguments) stay as they were typed.
//! The helpers of this module are the single place for those rules; on
//! other platforms they do nothing beyond what `std` does.
//!
//! * [`is_absolute`], [`is_absolute_bytes`]: what git treats as an absolute
//!   path (on Windows also `/x` and `\x` without a drive, and `C:x`).
//! * [`current_dir`], [`canonicalize`]: the system's answer in git's form.
//! * [`simplified`], [`to_slash`], [`join`]: the conversions themselves.

use std::io;
use std::path::{Path, PathBuf};

/// Whether git treats `path` as absolute.
///
/// On Windows this differs from [`Path::is_absolute`]: a path that starts
/// with a directory separator (`/x`, `\x`) or with a drive (`C:/x`, also
/// the drive-relative `C:x`) is absolute for git. Elsewhere a path is
/// absolute when it starts with `/`.
#[must_use]
pub fn is_absolute(path: impl AsRef<Path>) -> bool {
    is_absolute_bytes(path.as_ref().as_os_str().as_encoded_bytes())
}

/// [`is_absolute`] for a path held as bytes (configuration values,
/// environment variables).
#[must_use]
pub fn is_absolute_bytes(path: &[u8]) -> bool {
    absolute_for(path, cfg!(windows))
}

/// The length of the drive prefix (`C:`) of `path`: 2 or 0. Always 0 on
/// platforms without drives.
#[must_use]
pub fn drive_prefix_len(path: &[u8]) -> usize {
    if cfg!(windows) { drive_len(path) } else { 0 }
}

/// Whether `b` separates directories in a path typed by the user (`/`, and
/// `\` on Windows).
#[must_use]
pub fn is_separator(b: u8) -> bool {
    b == b'/' || (cfg!(windows) && b == b'\\')
}

fn drive_len(path: &[u8]) -> usize {
    match path {
        [letter, b':', ..] if letter.is_ascii_alphabetic() => 2,
        _ => 0,
    }
}

fn absolute_for(path: &[u8], windows: bool) -> bool {
    match path.first() {
        Some(b'/') => true,
        Some(b'\\') => windows,
        _ => windows && drive_len(path) == 2,
    }
}

/// The current directory in git's form (see the module documentation).
///
/// # Errors
/// The error of [`std::env::current_dir`].
pub fn current_dir() -> io::Result<PathBuf> {
    std::env::current_dir().map(|dir| to_slash(simplified(dir)))
}

/// [`std::fs::canonicalize`] without the `\\?\` prefix Windows adds to the
/// result. The prefix switches off the interpretation of `/` and `..`, so
/// a path joined to such a result does not name what it names in any
/// other form, and git never prints it.
///
/// # Errors
/// The error of [`std::fs::canonicalize`].
pub fn canonicalize(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    std::fs::canonicalize(path).map(simplified)
}

/// `path` without a leading `\\?\` (`\\?\C:\x` becomes `C:\x`,
/// `\\?\UNC\server\share` becomes `\\server\share`). Other paths, and all
/// paths outside Windows, are returned unchanged.
#[must_use]
pub fn simplified(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        match strip_verbatim(&wide) {
            Some(plain) => PathBuf::from(std::ffi::OsString::from_wide(&plain)),
            None => path,
        }
    }
    #[cfg(not(windows))]
    {
        path
    }
}

/// `path` with `/` in place of every `\` on Windows; unchanged elsewhere
/// (where `\` is an ordinary character of a name).
#[must_use]
pub fn to_slash(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        if !wide.contains(&BACKSLASH) {
            return path;
        }
        slashes(&mut wide);
        PathBuf::from(std::ffi::OsString::from_wide(&wide))
    }
    #[cfg(not(windows))]
    {
        path
    }
}

/// `base` and `rel` joined with `/`, the separator git uses on every
/// platform ([`Path::join`] inserts `\` on Windows). An absolute `rel`
/// (in git's sense, [`is_absolute`]) replaces `base`, an empty `base`
/// yields `rel`.
#[must_use]
pub fn join(base: impl AsRef<Path>, rel: impl AsRef<Path>) -> PathBuf {
    let (base, rel) = (base.as_ref(), rel.as_ref());
    if is_absolute(rel) || base.as_os_str().is_empty() {
        return rel.to_path_buf();
    }
    let mut out = base.as_os_str().to_os_string();
    let ends_with_separator = out
        .as_encoded_bytes()
        .last()
        .is_some_and(|&b| is_separator(b));
    if !ends_with_separator {
        out.push("/");
    }
    out.push(rel.as_os_str());
    PathBuf::from(out)
}

#[cfg_attr(not(windows), allow(dead_code))]
const BACKSLASH: u16 = b'\\' as u16;

#[cfg_attr(not(windows), allow(dead_code))]
fn slashes(wide: &mut [u16]) {
    for unit in wide {
        if *unit == BACKSLASH {
            *unit = u16::from(b'/');
        }
    }
}

/// The plain form of a `\\?\` path (UTF-16 units), `None` when `wide` has
/// no such prefix or names something without a plain form (a device, a
/// volume GUID).
#[cfg_attr(not(windows), allow(dead_code))]
fn strip_verbatim(wide: &[u16]) -> Option<Vec<u16>> {
    const VERBATIM: [u16; 4] = [BACKSLASH, BACKSLASH, b'?' as u16, BACKSLASH];
    const UNC: [u16; 4] = [b'U' as u16, b'N' as u16, b'C' as u16, BACKSLASH];
    let rest = wide.strip_prefix(&VERBATIM)?;
    if let Some(share) = rest.strip_prefix(&UNC) {
        let mut out = vec![BACKSLASH, BACKSLASH];
        out.extend_from_slice(share);
        return Some(out);
    }
    let is_letter = |unit: u16| u8::try_from(unit).is_ok_and(|b| b.is_ascii_alphabetic());
    match rest {
        [letter, colon, ..] if is_letter(*letter) && *colon == u16::from(b':') => {
            Some(rest.to_vec())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn absolute_paths() {
        for windows in [false, true] {
            assert!(absolute_for(b"/a", windows));
            assert!(!absolute_for(b"a/b", windows));
            assert!(!absolute_for(b"", windows));
            assert!(!absolute_for(b"./a", windows));
        }
        assert!(absolute_for(b"\\a", true));
        assert!(absolute_for(b"C:/a", true));
        assert!(absolute_for(b"c:\\a", true));
        assert!(absolute_for(b"C:a", true));
        assert!(absolute_for(b"\\\\server\\share", true));
        assert!(!absolute_for(b"1:/a", true));
        assert!(!absolute_for(b"\\a", false));
        assert!(!absolute_for(b"C:/a", false));
    }

    #[test]
    fn verbatim_prefixes() {
        let plain = |s: &str| strip_verbatim(&wide(s)).map(|w| String::from_utf16(&w).unwrap());
        assert_eq!(plain(r"\\?\C:\a\b").as_deref(), Some(r"C:\a\b"));
        assert_eq!(plain(r"\\?\c:\").as_deref(), Some(r"c:\"));
        assert_eq!(
            plain(r"\\?\UNC\server\share\x").as_deref(),
            Some(r"\\server\share\x")
        );
        assert_eq!(plain(r"C:\a"), None);
        assert_eq!(plain(r"\\server\share"), None);
        assert_eq!(plain(r"\\?\Volume{0a1b}\x"), None);
        assert_eq!(plain(r"\\?\"), None);
    }

    #[test]
    fn slash_form() {
        let mut w = wide(r"C:\a\b/c");
        slashes(&mut w);
        assert_eq!(String::from_utf16(&w).unwrap(), "C:/a/b/c");
    }

    #[test]
    fn joins_with_a_slash() {
        assert_eq!(join(Path::new("a"), "b"), Path::new("a/b"));
        assert_eq!(join(Path::new("a/"), "b/c"), Path::new("a/b/c"));
        assert_eq!(join(Path::new("a"), "/b"), Path::new("/b"));
        assert_eq!(join(Path::new(""), "b"), Path::new("b"));
        assert_eq!(
            join(Path::new("sub"), "../x")
                .as_os_str()
                .as_encoded_bytes(),
            b"sub/../x"
        );
    }

    #[test]
    fn current_directory_is_plain() {
        let dir = current_dir().unwrap();
        assert!(is_absolute(&dir));
        let bytes = dir.as_os_str().as_encoded_bytes();
        assert!(!bytes.starts_with(br"\\?\"));
        if cfg!(windows) {
            assert!(!bytes.contains(&b'\\'));
        }
        let real = canonicalize(&dir).unwrap();
        assert!(!real.as_os_str().as_encoded_bytes().starts_with(br"\\?\"));
    }
}
