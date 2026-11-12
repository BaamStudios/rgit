// SPDX-License-Identifier: MIT
//! Byte strings to OS strings.

use std::ffi::OsString;

/// Convert bytes to an `OsString` (exact on Unix, lossy elsewhere).
#[cfg(unix)]
pub fn os_string(b: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(b.to_vec())
}

/// Convert bytes to an `OsString` (exact on Unix, lossy elsewhere).
#[cfg(not(unix))]
pub fn os_string(b: &[u8]) -> OsString {
    OsString::from(String::from_utf8_lossy(b).into_owned())
}
