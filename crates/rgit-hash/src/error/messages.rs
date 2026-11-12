// SPDX-License-Identifier: MIT

//! User-visible message texts reproduced from git for compatibility.
//!
//! Collected here so they are reviewable in one place (see `CLEANROOM.md`).

/// Text of a detected SHA-1 collision, followed by the full hex digest.
///
/// The CLI prints it as `fatal: <text><hex>` and exits with status 128.
pub const COLLISION: &str = "SHA-1 appears to be part of a collision attack: ";
