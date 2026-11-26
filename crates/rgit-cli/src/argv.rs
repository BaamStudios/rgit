// SPDX-License-Identifier: MIT
//! Command-line arguments that are not UTF-8.
//!
//! Commands receive their arguments as `String`s. Bytes of an argument that
//! are not valid UTF-8 (a commit message or an author name in ISO-8859-1,
//! EUC-JP or Shift_JIS given with `-m` / `--author`) are kept as private
//! use characters U+10FF00 + byte ([`rgit_bstr`]), so that [`bytes`] gives
//! the original argument back where it is stored verbatim.

use std::ffi::OsString;

/// `arg` as a `String`, invalid bytes escaped ([`rgit_bstr::from_os_string`]).
pub fn to_string(arg: OsString) -> String {
    rgit_bstr::from_os_string(arg)
}

/// The bytes of an argument converted with [`to_string`].
pub fn bytes(arg: &str) -> Vec<u8> {
    rgit_bstr::to_bytes(arg).into_owned()
}

#[cfg(test)]
mod tests {
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
}
