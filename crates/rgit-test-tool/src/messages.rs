// SPDX-License-Identifier: MIT

//! User-visible message texts, reproduced from the reference `test-tool` where
//! the test scripts depend on them (see `CLEANROOM.md`). Collected here so they
//! are reviewable in one place.

use std::fmt::Display;

/// First line of the subcommand list.
pub const USAGE: &str = "usage: test-tool <toolname> [args]";

/// Unknown subcommand.
pub fn no_such_tool(name: &str) -> String {
    format!("error: there is no tool named '{name}'")
}

/// `-C <dir>` failed.
pub fn cannot_chdir(dir: &str, _err: &dyn Display) -> String {
    format!("fatal: Could not cd to '{dir}'")
}

// ----- parse-options, parse-options-flags, parse-subcommand ------------

/// `--abbrev=<n>` with a non-numeric `<n>`.
pub const ABBREV_NOT_NUMERIC: &str = "option `abbrev' expects a numerical value";
/// `--no-expect`.
pub const EXPECT_MALFORMED: &str = "malformed --expect option";
/// `--expect` without a colon.
pub const EXPECT_NO_COLON: &str = "malformed --expect option, lacking a colon";
/// `--expect` twice for the same key (the trailing space is the reference's).
pub const EXPECT_DUPLICATE: &str = "malformed --expect option, duplicate ";
/// `parse-options-flags` / `parse-subcommand` without `cmd`.
pub const CMD_MANDATORY: &str = "'cmd' is mandatory";

/// A callback rejecting its argument.
pub fn invalid_value(option: &str, value: &str) -> String {
    format!("invalid value for '{option}': '{value}'")
}

/// Exit code of the reference binary after a `BUG:` message.
pub const BUG_EXIT_CODE: i32 = 99;
/// Flag combinations the reference rejects as programming errors; the
/// location is part of its observed output.
pub const BUG_SUBCOMMAND_OPTIONAL_WITHOUT_SUBCOMMANDS: &str =
    "BUG: parse-options.c:764: Using PARSE_OPT_SUBCOMMAND_OPTIONAL without subcommands";
pub const BUG_SUBCOMMANDS_STOP_AT_NON_OPTION: &str =
    "BUG: parse-options.c:767: subcommands are incompatible with PARSE_OPT_STOP_AT_NON_OPTION";
pub const BUG_SUBCOMMANDS_KEEP_UNKNOWN_OPT: &str = "BUG: parse-options.c:770: subcommands are incompatible with PARSE_OPT_KEEP_UNKNOWN_OPT unless in combination with PARSE_OPT_SUBCOMMAND_OPTIONAL";
pub const BUG_SUBCOMMANDS_KEEP_DASHDASH: &str = "BUG: parse-options.c:772: subcommands are incompatible with PARSE_OPT_KEEP_DASHDASH unless in combination with PARSE_OPT_SUBCOMMAND_OPTIONAL";
pub const BUG_STOP_AT_NON_OPTION_KEEP_UNKNOWN: &str =
    "BUG: parse-options.c:778: STOP_AT_NON_OPTION and KEEP_UNKNOWN don't go together";

/// Prefix of fatal errors (exit code 128).
pub fn fatal(msg: impl Display) -> String {
    format!("fatal: {msg}")
}
