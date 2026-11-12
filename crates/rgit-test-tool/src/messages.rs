// SPDX-License-Identifier: MIT

//! User-visible message texts, reproduced from the reference `test-tool` where
//! the test scripts depend on them (see `CLEANROOM.md`). Collected here so they
//! are reviewable in one place.

use std::fmt::Display;

use bstr::BStr;
use rgit_config::Origin;

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

// ----- config -----------------------------------------------------------

/// `config` with an unknown function or the wrong number of arguments.
pub const CONFIG_SYNTAX: &str = "config: Please check the syntax and the function name";
/// `config` without arguments.
pub const CONFIG_NO_COMMAND: &str = "Please, provide a command name on the command-line";
/// No repository found.
pub const NOT_A_REPOSITORY: &str = "not a git repository (or any of the parent directories): .git";
/// No repository found before a file system boundary.
pub fn not_a_repository_fs(mount_point: &str) -> String {
    format!(
        "not a git repository (or any parent up to mount point {mount_point})\nStopping at filesystem boundary (GIT_DISCOVERY_ACROSS_FILESYSTEM not set)."
    )
}

/// How C's printf shows a null string.
pub const C_NULL: &[u8] = b"(null)";

/// A lookup found nothing.
pub fn value_not_found(key: &str) -> String {
    format!("Value not found for \"{key}\"")
}

/// `config get` with a key without section or variable name.
pub fn key_no_section(key: &str) -> String {
    format!("Key \"{key}\" has no section")
}

/// `config get` with an invalid key.
pub fn key_invalid(key: &str) -> String {
    format!("Key \"{key}\" is invalid")
}

/// An integer value that does not parse.
pub fn bad_numeric(value: &BStr, name: &BStr, source: Option<String>, why: &str) -> String {
    match source {
        Some(s) => format!("bad numeric config value '{value}' for '{name}' in {s}: {why}"),
        None => format!("bad numeric config value '{value}' for '{name}': {why}"),
    }
}

/// A boolean value that does not parse.
pub fn bad_boolean(value: &BStr, name: &BStr) -> String {
    format!("bad boolean config value '{value}' for '{name}'")
}

/// A value-less key where a string is needed.
pub fn missing_value(name: &BStr) -> String {
    format!("missing value for '{name}'")
}

/// The fatal line after [`missing_value`] for a file or blob.
pub fn bad_config_variable(name: &BStr, origin: &Origin, line: usize) -> String {
    format!(
        "bad config variable '{name}' in {} '{}' at line {line}",
        origin.kind(),
        origin.name()
    )
}

/// The fatal line after [`missing_value`] for the command line.
pub fn unable_to_parse_cmdline(name: &BStr) -> String {
    format!("unable to parse '{name}' from command-line config")
}

/// `configset_*` could not read a file.
pub fn configset_read_error(file: &str) -> String {
    format!("Error (-1) reading configuration file {file}.")
}

/// Early configuration skips a repository it cannot use.
pub fn ignoring_git_dir(git_dir: &str, reason: &str) -> String {
    format!("ignoring git dir '{git_dir}': {reason}")
}

/// Repository format version too new.
pub fn repo_version(max: i64, found: i64) -> String {
    format!("Expected git repo version <= {max}, found {found}")
}

/// Unknown repository extensions.
pub fn unknown_extensions(names: &[String]) -> String {
    let mut s = String::from(if names.len() == 1 {
        "unknown repository extension found:"
    } else {
        "unknown repository extensions found:"
    });
    for n in names {
        s.push_str("\n\t");
        s.push_str(n);
    }
    s
}

/// A syntactically fine entry with an unusable value, after the `error:`.
pub fn bad_config_line(line: usize, file: &std::path::Path) -> String {
    format!("bad config line {line} in file {}", file.display())
}
