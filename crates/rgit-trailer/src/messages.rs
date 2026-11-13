// SPDX-License-Identifier: MIT
//! User-visible messages of the trailer code (CLEANROOM.md: reproduced
//! verbatim where scripts depend on them, collected here for review).

/// The scissors line without its comment prefix.
pub const SCISSORS: &str = " ------------------------ >8 ------------------------";

/// Prefixes of lines git writes itself; they make a trailer block count
/// as one even with other lines in it.
pub const GIT_GENERATED_PREFIXES: &[&str] = &["Signed-off-by: ", "(cherry picked from commit "];

pub fn unknown_value(value: &str, key: &str) -> String {
    format!("unknown value '{value}' for key '{key}'")
}

pub fn empty_token(arg: &str) -> String {
    format!("empty trailer token in trailer '{arg}'")
}

pub fn command_failed(cmd: &str) -> String {
    format!("running trailer command '{cmd}' failed")
}
