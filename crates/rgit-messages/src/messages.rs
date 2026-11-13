// SPDX-License-Identifier: MIT
//! The user-visible strings of this crate (CLEANROOM.md: reproduced
//! verbatim and collected in one place).

/// The fatal message for an environment variable that must be a boolean.
#[must_use]
pub fn bad_bool_env(name: &str, value: &str) -> String {
    format!("bad boolean environment value '{value}' for '{name}'")
}

/// The last line of an advice whose `advice.<name>` is not configured.
#[must_use]
pub fn advice_footer(name: &str) -> String {
    format!("Disable this message with \"git config set advice.{name} false\"")
}
