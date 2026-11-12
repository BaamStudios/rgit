// SPDX-License-Identifier: MIT

//! User-visible strings that must match git's output byte for byte.

/// Marker printed after a line that has no terminating newline.
pub const NO_NEWLINE_AT_EOF: &str = "\\ No newline at end of file";

/// Appends the `Binary files <a> and <b> differ` line (with newline).
pub fn binary_files_differ(out: &mut Vec<u8>, a: &[u8], b: &[u8]) {
    out.extend_from_slice(b"Binary files ");
    out.extend_from_slice(a);
    out.extend_from_slice(b" and ");
    out.extend_from_slice(b);
    out.extend_from_slice(b" differ\n");
}
