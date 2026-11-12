// SPDX-License-Identifier: MIT
//! User-visible messages of this crate, kept verbatim where scripts and the
//! conformance tests depend on them (see CLEANROOM.md). Every string that git
//! users may see is built here so the set can be reviewed in one place.

use std::fmt::Display;

use bstr::BStr;

pub(crate) fn key_no_section(key: &BStr) -> String {
    format!("key does not contain a section: {key}")
}

pub(crate) fn key_no_variable(key: &BStr) -> String {
    format!("key does not contain variable name: {key}")
}

pub(crate) fn invalid_key(key: &BStr) -> String {
    format!("invalid key: {key}")
}

pub(crate) fn invalid_key_newline(key: &BStr) -> String {
    format!("invalid key (newline): {key}")
}

/// `bad config line N in <source>`
pub(crate) fn bad_config_line(line: usize, source: impl Display) -> String {
    format!("bad config line {line} in {source}")
}

pub(crate) fn bad_numeric(value: &BStr, name: &BStr, source: Option<String>, why: &str) -> String {
    match source {
        Some(s) => format!("bad numeric config value '{value}' for '{name}' in {s}: {why}"),
        None => format!("bad numeric config value '{value}' for '{name}': {why}"),
    }
}

pub(crate) fn bad_boolean(value: &BStr, name: &BStr) -> String {
    format!("bad boolean config value '{value}' for '{name}'")
}

pub(crate) const INVALID_UNIT: &str = "invalid unit";
pub(crate) const OUT_OF_RANGE: &str = "out of range";

pub(crate) fn missing_value(name: &BStr) -> String {
    format!("missing value for '{name}'")
}

pub(crate) fn expand_user_dir(path: &BStr) -> String {
    format!("failed to expand user dir in: '{path}'")
}

pub(crate) fn expand_include_path(path: &BStr) -> String {
    format!("could not expand include path '{path}'")
}

pub(crate) const RELATIVE_INCLUDE: &str = "relative config includes must come from files";
pub(crate) const RELATIVE_INCLUDE_CONDITION: &str =
    "relative config include conditionals must come from files";

pub(crate) fn include_depth(depth: usize, path: impl Display, from: impl Display) -> String {
    format!(
        "exceeded maximum include depth ({depth}) while including\n\t{path}\nfrom\n\t{from}\nThis might be due to circular includes."
    )
}

pub(crate) const REMOTE_URL_IN_HASCONFIG: &str = "remote URLs cannot be configured in file directly or indirectly included by includeIf.hasconfig:remote.*.url";

pub(crate) fn unable_to_access(path: impl Display, err: &std::io::Error) -> String {
    format!("unable to access '{path}': {}", io_error_text(err))
}

pub(crate) const UNABLE_TO_PARSE_COMMAND_LINE: &str = "unable to parse command-line config";
pub(crate) const BOGUS_COUNT: &str = "bogus count in GIT_CONFIG_COUNT";
pub(crate) const TOO_MANY_ENTRIES: &str = "too many entries in GIT_CONFIG_COUNT";
pub(crate) const BOGUS_PARAMETERS: &str = "bogus format in GIT_CONFIG_PARAMETERS";
pub(crate) const EMPTY_KEY: &str = "empty config key";

pub(crate) fn missing_env_key(n: usize) -> String {
    format!("missing config key GIT_CONFIG_KEY_{n}")
}

pub(crate) fn missing_env_value(n: usize) -> String {
    format!("missing config value GIT_CONFIG_VALUE_{n}")
}

pub(crate) fn invalid_config_format(arg: &str) -> String {
    format!("invalid config format: {arg}")
}

pub(crate) fn missing_env_name(key: &str) -> String {
    format!("missing environment variable name for configuration '{key}'")
}

pub(crate) fn missing_env_var(var: &str, key: &str) -> String {
    format!("missing environment variable '{var}' for configuration '{key}'")
}

pub(crate) fn no_config_env_key() -> &'static str {
    "no config key given for --config-env"
}

pub(crate) fn multiple_values(key: &BStr) -> String {
    format!("{key} has multiple values")
}

pub(crate) fn invalid_pattern(pattern: &BStr) -> String {
    format!("invalid pattern: {pattern}")
}

pub(crate) fn multiline_comment(comment: &BStr) -> String {
    format!("no multi-line comment allowed: '{comment}'")
}

pub(crate) fn invalid_section_name(name: &BStr) -> String {
    format!("invalid section name: {name}")
}

pub(crate) fn no_such_section(name: &BStr) -> String {
    format!("no such section: {name}")
}

pub(crate) fn overlong_line(file: impl Display, line: usize) -> String {
    format!("refusing to work with overly long line in '{file}' on line {line}")
}

pub(crate) fn could_not_lock(file: impl Display, err: &std::io::Error) -> String {
    format!("could not lock config file {file}: {}", io_error_text(err))
}

pub(crate) fn opening(file: impl Display, err: &std::io::Error) -> String {
    format!("opening {file}: {}", io_error_text(err))
}

pub(crate) fn could_not_write(file: impl Display, err: &std::io::Error) -> String {
    format!("could not write config file {file}: {}", io_error_text(err))
}

pub(crate) fn could_not_commit(file: impl Display, err: &std::io::Error) -> String {
    format!(
        "could not commit config file {file}: {}",
        io_error_text(err)
    )
}

/// The C library's text for an I/O error (git prints `strerror(errno)`).
pub(crate) fn io_error_text(err: &std::io::Error) -> String {
    use std::io::ErrorKind::*;
    match err.kind() {
        NotFound => "No such file or directory".into(),
        PermissionDenied => "Permission denied".into(),
        NotADirectory => "Not a directory".into(),
        IsADirectory => "Is a directory".into(),
        _ => {
            // std appends " (os error N)"; strip it to match strerror().
            let s = err.to_string();
            match s.find(" (os error") {
                Some(i) => s[..i].to_string(),
                None => s,
            }
        }
    }
}
