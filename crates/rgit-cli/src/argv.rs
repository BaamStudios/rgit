// SPDX-License-Identifier: MIT
//! Command-line arguments that are not UTF-8.
//!
//! Commands receive their arguments as `String`s. Bytes of an argument that
//! are not valid UTF-8 (a commit message or an author name in ISO-8859-1,
//! EUC-JP or Shift_JIS given with `-m` / `--author`) are kept as private
//! use characters U+10FF00 + byte, so that [`bytes`] gives the original
//! argument back where it is stored verbatim.

use std::ffi::OsString;

const BASE: u32 = 0x10ff00;

/// `arg` as a `String`, invalid bytes escaped.
pub fn to_string(arg: OsString) -> String {
    match arg.into_string() {
        Ok(s) => s,
        Err(os) => {
            let raw = os.as_encoded_bytes();
            let mut out = String::with_capacity(raw.len() + 8);
            for chunk in raw.utf8_chunks() {
                out.push_str(chunk.valid());
                for &b in chunk.invalid() {
                    out.push(char::from_u32(BASE + u32::from(b)).unwrap_or('\u{fffd}'));
                }
            }
            out
        }
    }
}

/// The bytes of an argument converted with [`to_string`].
pub fn bytes(arg: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(arg.len());
    for c in arg.chars() {
        let v = u32::from(c);
        if (BASE..=BASE + 0xff).contains(&v) {
            out.push((v - BASE) as u8);
        } else {
            let mut buf = [0; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }
    out
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
}
