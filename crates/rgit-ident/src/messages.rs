// SPDX-License-Identifier: MIT
//! User-visible messages of this crate, reproduced verbatim from what the
//! reference `git` binary prints (see CLEANROOM.md, "User-visible
//! messages"). Texts are given without the `fatal: ` / `error: ` prefix.

/// First line of the identity advice for the author.
pub const AUTHOR_UNKNOWN: &str = "Author identity unknown";
/// First line of the identity advice for the committer.
pub const COMMITTER_UNKNOWN: &str = "Committer identity unknown";

/// The advice printed (to stderr, without prefix) after
/// [`AUTHOR_UNKNOWN`] / [`COMMITTER_UNKNOWN`] and before the `fatal:` line.
pub const PLEASE_TELL_ME: &str = "
*** Please tell me who you are.

Run

  git config --global user.email \"you@example.com\"
  git config --global user.name \"Your Name\"

to set your account's default identity.
Omit --global to set the identity only in this repository.
";

pub const NO_EMAIL_GIVEN: &str = "no email was given and auto-detection is disabled";
pub const NO_NAME_GIVEN: &str = "no name was given and auto-detection is disabled";

pub fn unable_to_auto_detect_email(guess: &str) -> String {
    format!("unable to auto-detect email address (got '{guess}')")
}

pub fn empty_ident_name(email: &str) -> String {
    format!("empty ident name (for <{email}>) not allowed")
}

pub fn only_disallowed(name: &str) -> String {
    format!("name consists only of disallowed characters: {name}")
}

pub fn invalid_date(date: &str) -> String {
    format!("invalid date format: {date}")
}

pub fn passwd_lookup(reason: &str) -> String {
    format!("unable to look up current user in the passwd file: {reason}")
}

pub const NO_SUCH_USER: &str = "no such user";

// Configuration problems.

pub fn missing_value(name: &str) -> String {
    format!("missing value for '{name}'")
}

pub fn unable_to_parse_cmdline(name: &str) -> String {
    format!("unable to parse '{name}' from command-line config")
}

pub const UNABLE_TO_PARSE_CMDLINE: &str = "unable to parse command-line config";

pub fn bad_config_variable(name: &str, source: &str, line: usize) -> String {
    format!("bad config variable '{name}' in {source} at line {line}")
}

pub fn bad_config_line(line: usize, source: &str) -> String {
    format!("bad config line {line} in {source}")
}

pub fn bad_env_bool(value: &str, name: &str) -> String {
    format!("bad boolean environment value '{value}' for '{name}'")
}

// Editor.

pub const TERMINAL_IS_DUMB: &str = "Terminal is dumb, but EDITOR unset";

pub const WAITING_FOR_EDITOR: &str = "Waiting for your editor to close the file...";

pub fn editor_problem(editor: &str) -> String {
    format!("there was a problem with the editor '{editor}'")
}

pub fn unable_to_start_editor(editor: &str) -> String {
    format!("unable to start editor '{editor}'")
}

pub fn cannot_run(program: &str, reason: &str) -> String {
    format!("cannot run {program}: {reason}")
}

pub fn cannot_exec(program: &str, reason: &str) -> String {
    format!("cannot exec '{program}': {reason}")
}

pub fn died_of_signal(program: &str, signal: i32) -> String {
    format!("{program} died of signal {signal}")
}
