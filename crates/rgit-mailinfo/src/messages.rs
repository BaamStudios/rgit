// SPDX-License-Identifier: MIT
//! User-visible messages of the mail code (CLEANROOM.md: reproduced
//! verbatim where scripts depend on them, collected here for review).

pub const QUOTED_CRLF: &str = "quoted CRLF detected";

pub fn cannot_convert(from: &str, to: &str) -> String {
    format!("cannot convert from {from} to {to}")
}

pub fn nul_in_header(name: &str) -> String {
    format!("a NUL byte in '{name}' is not allowed")
}
