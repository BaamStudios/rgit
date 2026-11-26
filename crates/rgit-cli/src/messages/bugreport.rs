// SPDX-License-Identifier: MIT
//! Usage and messages of `git bugreport` and `git diagnose` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const BUGREPORT_USAGE: &str = "git bugreport [(-o | --output-directory) <path>]\n[(-s | --suffix) <format> | --no-suffix]\n[--diagnose[=<mode>]]";
pub const DIAGNOSE_USAGE: &str =
    "git diagnose [(-o | --output-directory) <path>] [(-s | --suffix) <format>]\n[--mode=<mode>]";

pub const HELP_DIAGNOSE: &str =
    "create an additional zip archive of detailed diagnostics (default 'stats')";
pub const HELP_OUTPUT_DIRECTORY: &str = "specify a destination for the bugreport file(s)";
pub const HELP_SUFFIX: &str = "specify a strftime format suffix for the filename(s)";
pub const HELP_DIAGNOSE_OUTPUT_DIRECTORY: &str =
    "specify a destination for the diagnostics archive";
pub const HELP_DIAGNOSE_SUFFIX: &str = "specify a strftime format suffix for the filename";
pub const HELP_MODE: &str = "specify the content of the diagnostic archive";

/// The questions at the top of a report (checked verbatim by t0091).
pub const TEMPLATE: &str = "Thank you for filling out a Git bug report!
Please answer the following questions to help us understand your issue.

What did you do before the bug happened? (Steps to reproduce your issue)

What did you expect to happen? (Expected behavior)

What happened instead? (Actual behavior)

What's different between what you expected and what actually happened?

Anything else you want to add:

Please review the rest of the bug report below.
You can delete any lines you don't wish to share.
";

/// rgit's own section: where reports about rgit go (never to the git
/// project).
pub const WHERE_TO_REPORT: &str = "[Where to Report]
This report is about rgit, an independent reimplementation of Git.
Please file it at https://github.com/BaamStudios/rgit/issues
and not with the Git project.
";

pub const NO_HOOKS_OUTSIDE_REPO: &str = "not run from a git repository - no hooks to show";
pub const COLLECTING: &str = "Collecting diagnostic info";
pub const DIAGNOSTICS_COMPLETE: &str = "Diagnostics complete.";

pub fn unknown_argument(arg: &str) -> String {
    format!("unknown argument `{arg}'")
}

pub fn invalid_diagnose_value(v: &str) -> String {
    format!("invalid --diagnose value '{v}'")
}

pub fn invalid_mode_value(v: &str) -> String {
    format!("invalid --mode value '{v}'")
}

pub fn created_report(path: &str) -> String {
    format!("Created new report at '{path}'.")
}

pub fn unable_to_create(path: &str, err: &str) -> String {
    format!("unable to create '{path}': {err}")
}

pub fn could_not_create_leading_directories(path: &str) -> String {
    format!("could not create leading directories for '{path}'")
}

pub fn could_not_open_for_writing(path: &str, err: &str) -> String {
    format!("could not open '{path}' for writing: {err}")
}

pub fn captured_in(path: &str) -> String {
    format!("All of the gathered info is captured in '{path}'")
}

pub fn repository_root(root: &str) -> String {
    format!("Repository root: {root}")
}

pub fn available_space(path: &str, space: &str, flags: u64) -> String {
    format!("Available space on '{path}': {space} (mount flags 0x{flags:x})")
}

pub fn contents_of(dir: &str) -> String {
    format!("Contents of {dir}:")
}

pub fn object_dir_stats(dir: &str) -> String {
    format!("Object directory stats for {dir}:")
}

pub fn total_loose(n: usize) -> String {
    format!("Total: {n} loose objects")
}
