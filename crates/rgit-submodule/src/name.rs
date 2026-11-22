// SPDX-License-Identifier: MIT
//! Submodule names.

/// Whether `name` may be used as a submodule name.
///
/// The name becomes a directory below `$GIT_DIR/modules/`, so it must not be
/// empty and no component may be `..` (components are separated by `/` and,
/// as on Windows, by `\`). Everything else is allowed, including `.`
/// components, absolute-looking names and spaces.
pub fn check_name(name: &[u8]) -> bool {
    !name.is_empty() && !name.split(|&b| b == b'/' || b == b'\\').any(|c| c == b"..")
}
