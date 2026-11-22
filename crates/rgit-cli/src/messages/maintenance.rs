// SPDX-License-Identifier: MIT
//! Messages of automatic maintenance (CLEANROOM.md: user-visible strings
//! are reproduced verbatim and collected here).

pub fn unknown_strategy(name: &str) -> String {
    format!("unknown maintenance strategy: '{name}'")
}
