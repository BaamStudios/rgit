// SPDX-License-Identifier: MIT

//! `core.precomposeUnicode` (git-config(1)): macOS stores or reports file
//! names in decomposed form (NFD, `a` + combining diaeresis) where every
//! other system uses the precomposed form (NFC, `ä`). With the setting,
//! file names read from a directory and names given on the command line
//! are converted to the precomposed form, so a repository holds the same
//! names on every system.
//!
//! git only implements the setting on macOS; elsewhere it is ignored, and
//! so it is here. `RGIT_TEST_PRECOMPOSE_UNICODE=1` makes the other
//! platforms honour it too, to test this code on a file system that does
//! not tell the two forms apart (docs/platforms.md).

use std::borrow::Cow;
use std::ffi::OsStr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use unicode_normalization::{IsNormalized, UnicodeNormalization, is_nfc_quick};

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Whether this platform implements `core.precomposeUnicode`.
pub fn supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        cfg!(target_os = "macos")
            || std::env::var_os("RGIT_TEST_PRECOMPOSE_UNICODE").is_some_and(|v| v == "1")
    })
}

/// Applies `core.precomposeUnicode` to the whole process (ignored where
/// the setting is not [`supported`]).
pub fn set_enabled(on: bool) {
    ENABLED.store(on && supported(), Ordering::Relaxed);
}

/// Whether names are converted.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// `name` in precomposed form (NFC). Names that are ASCII, already
/// precomposed or not UTF-8 are returned as they are.
pub fn to_nfc(name: &[u8]) -> Cow<'_, [u8]> {
    if name.is_ascii() {
        return Cow::Borrowed(name);
    }
    let Ok(text) = std::str::from_utf8(name) else {
        return Cow::Borrowed(name);
    };
    if is_nfc_quick(text.chars()) == IsNormalized::Yes {
        return Cow::Borrowed(name);
    }
    let composed: String = text.nfc().collect();
    if composed == text {
        Cow::Borrowed(name)
    } else {
        Cow::Owned(composed.into_bytes())
    }
}

/// The bytes of a name read from a directory, precomposed when the setting
/// is [`enabled`].
pub fn file_name(name: &OsStr) -> Vec<u8> {
    let bytes = name.as_encoded_bytes();
    if enabled() {
        to_nfc(bytes).into_owned()
    } else {
        bytes.to_vec()
    }
}

/// A command-line argument, precomposed (the caller checks the setting).
pub fn argument(arg: &mut String) {
    if arg.is_ascii() || is_nfc_quick(arg.chars()) == IsNormalized::Yes {
        return;
    }
    let composed: String = arg.nfc().collect();
    *arg = composed;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composes_decomposed_names() {
        assert_eq!(&*to_nfc("A\u{308}".as_bytes()), "\u{c4}".as_bytes());
        assert_eq!(
            &*to_nfc("d.A\u{308}/f.O\u{308}".as_bytes()),
            "d.\u{c4}/f.\u{d6}".as_bytes()
        );
        // Hangul: conjoining jamo compose to a syllable.
        assert_eq!(
            &*to_nfc("\u{1100}\u{1161}".as_bytes()),
            "\u{ac00}".as_bytes()
        );
    }

    #[test]
    fn leaves_other_names_alone() {
        for name in ["plain.txt", "\u{c4}", "\u{c6}\u{c6}", "\u{4e2d}\u{6587}"] {
            assert!(
                matches!(to_nfc(name.as_bytes()), Cow::Borrowed(_)),
                "{name}"
            );
        }
        // Not UTF-8: kept byte for byte.
        let invalid = b"\xc3\xff";
        assert_eq!(&*to_nfc(invalid), invalid);
    }

    #[test]
    fn arguments() {
        let mut arg = String::from("f.A\u{308}");
        argument(&mut arg);
        assert_eq!(arg, "f.\u{c4}");
        let mut plain = String::from("--option=x");
        argument(&mut plain);
        assert_eq!(plain, "--option=x");
    }
}
