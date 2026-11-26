// SPDX-License-Identifier: MIT
//! User-visible messages of rerere (CLEANROOM.md: reproduced verbatim
//! where scripts depend on them, collected here for review). All of them
//! go to standard error.

fn shown(path: &[u8]) -> String {
    String::from_utf8_lossy(path).into_owned()
}

pub fn recorded_preimage(path: &[u8]) -> String {
    format!("Recorded preimage for '{}'", shown(path))
}

pub fn recorded_resolution(path: &[u8]) -> String {
    format!("Recorded resolution for '{}'.", shown(path))
}

pub fn resolved(path: &[u8]) -> String {
    format!("Resolved '{}' using previous resolution.", shown(path))
}

pub fn staged(path: &[u8]) -> String {
    format!("Staged '{}' using previous resolution.", shown(path))
}

pub fn updated_preimage(path: &[u8]) -> String {
    format!("Updated preimage for '{}'", shown(path))
}

pub fn forgot(path: &[u8]) -> String {
    format!("Forgot resolution for '{}'", shown(path))
}

pub fn no_remembered(path: &[u8]) -> String {
    format!("error: no remembered resolution for '{}'", shown(path))
}

pub fn could_not_parse(path: &[u8]) -> String {
    format!("error: could not parse conflict hunks in '{}'", shown(path))
}

pub fn could_not_open(path: &[u8], err: &str) -> String {
    format!("error: could not open '{}': {err}", shown(path))
}
