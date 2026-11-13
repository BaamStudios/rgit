// SPDX-License-Identifier: MIT
//! Messages of `git help`, alias expansion, external commands and
//! autocorrection (#179). Headings and option texts that scripts and tests
//! compare are reproduced; the command descriptions are rgit's own
//! (`crate::cmdlist`).

/// `git help -h`.
pub const USAGE: &[&str] = &[
    "git help [-a|--all] [--[no-]verbose] [--[no-]external-commands] [--[no-]aliases]",
    "git help [[-i|--info] [-m|--man] [-w|--web]] [<command>|<doc>]",
    "git help [-g|--guides]",
    "git help [-c|--config]",
    "git help [--user-interfaces]",
    "git help [--developer-interfaces]",
];
pub const HELP_ALL: &str = "print all available commands";
pub const HELP_EXTERNAL: &str = "show external commands in --all";
pub const HELP_ALIASES: &str = "show aliases in --all";
pub const HELP_MAN: &str = "show man page";
pub const HELP_WEB: &str = "show manual in web browser";
pub const HELP_INFO: &str = "show info page";
pub const HELP_VERBOSE: &str = "print command description";
pub const HELP_GUIDES: &str = "print list of useful guides";
pub const HELP_USER_INTERFACES: &str =
    "print list of user-facing repository, command and file interfaces";
pub const HELP_DEVELOPER_INTERFACES: &str =
    "print list of file formats, protocols and other developer interfaces";
pub const HELP_CONFIG: &str = "print all configuration variable names";

/// `git help -a` with another argument.
pub fn option_takes_no_arguments(opt: &str) -> String {
    format!("the '{opt}' option doesn't take any non-option arguments")
}

/// Two options that exclude each other.
pub fn options_incompatible(a: &str, b: &str) -> String {
    format!("options '{a}' and '{b}' cannot be used together")
}

/// `--[no-]external-commands` / `--[no-]aliases` without `--all`.
pub const ONLY_WITH_ALL: &str =
    "the '--no-[external-commands|aliases]' options can only be used with '--all'";

/// rgit has no list of configuration variables yet.
pub const CONFIG_LIST_UNSUPPORTED: &str =
    "rgit cannot list configuration variables yet; see https://git-scm.com/docs/git-config";

pub fn unrecognized_help_format(format: &str) -> String {
    format!("unrecognized help format '{format}'")
}

pub fn documentation_not_found(path: &str) -> String {
    format!("'{path}': documentation file not found.")
}

pub fn unknown_browser(tool: &str) -> String {
    format!(
        "git config option help.browser set to unknown browser: {tool}\nResetting to default..."
    )
}

pub fn path_for_unsupported_man_viewer(tool: &str) -> String {
    format!(
        "'{tool}.path': path for unsupported man viewer.\nPlease consider using 'man.<tool>.cmd' instead."
    )
}

pub fn unknown_man_viewer(tool: &str) -> String {
    format!("'{tool}': unknown man viewer.")
}

pub const NO_MAN_VIEWER: &str = "no man viewer handled the request";
pub const NO_INFO_VIEWER: &str = "no info viewer handled the request";

/// rgit ships no manual: where to read it when the local viewer fails.
pub fn online_manual(page: &str) -> String {
    format!("rgit: the manual is at https://git-scm.com/docs/{page}")
}

/// No browser is configured and none could be started.
pub fn open_in_browser(url: &str) -> String {
    format!("rgit: open {url} in a web browser")
}

pub fn cannot_run(prog: &str, err: &str) -> String {
    format!("cannot run {prog}: {err}")
}

// ---------------------------------------------------------------- listings

pub const COMMON_HEADING: &str = "These are common Git commands used in various situations:";
pub const GROUP_INIT: &str = "start a working area (see also: git help tutorial)";
pub const GROUP_WORKTREE: &str = "work on the current change (see also: git help everyday)";
pub const GROUP_INFO: &str = "examine the history and state (see also: git help revisions)";
pub const GROUP_HISTORY: &str = "grow, mark and tweak your common history";
pub const GROUP_REMOTE: &str = "collaborate (see also: git help workflows)";

/// Footer of `git help` and `git help -g`.
pub const HELP_FOOTER: &str = "\
'git help -a' and 'git help -g' list available subcommands and some
concept guides. See 'git help <command>' or 'git help <concept>'
to read about a specific subcommand or concept.
See 'git help git' for an overview of the system.";

pub const ALL_HEADING: &str = "See 'git help <command>' to read about a specific subcommand";
pub const CAT_MAIN: &str = "Main Porcelain Commands";
pub const CAT_ANCILLARY_MANIPULATORS: &str = "Ancillary Commands / Manipulators";
pub const CAT_ANCILLARY_INTERROGATORS: &str = "Ancillary Commands / Interrogators";
pub const CAT_FOREIGN: &str = "Interacting with Others";
pub const CAT_PLUMBING_MANIPULATORS: &str = "Low-level Commands / Manipulators";
pub const CAT_PLUMBING_INTERROGATORS: &str = "Low-level Commands / Interrogators";
pub const CAT_SYNCING: &str = "Low-level Commands / Syncing Repositories";
pub const CAT_HELPERS: &str = "Low-level Commands / Internal Helpers";
pub const CAT_USER_INTERFACES: &str = "User-facing repository, command and file interfaces";
pub const CAT_DEVELOPER_INTERFACES: &str =
    "Developer-facing file formats, protocols and other interfaces";
pub const EXTERNAL_COMMANDS: &str = "External commands";
pub const COMMAND_ALIASES: &str = "Command aliases";

pub const GUIDES_HEADING: &str = "The Git concept guides are:";
pub const USER_INTERFACES_HEADING: &str = "User-facing repository, command and file interfaces:";
pub const DEVELOPER_INTERFACES_HEADING: &str =
    "File formats, protocols and other developer interfaces:";

/// `git help -a --no-verbose`.
pub fn available_in(dir: &str) -> String {
    format!("available git commands in '{dir}'")
}
pub const AVAILABLE_ELSEWHERE: &str = "git commands available from elsewhere on your $PATH";

// ---------------------------------------------------------------- aliases

pub fn is_aliased_to(alias: &str, value: &str) -> String {
    format!("'{alias}' is aliased to '{value}'")
}

pub fn bad_alias_string(alias: &str, reason: &str) -> String {
    format!("bad alias.{alias} string: {reason}")
}
pub const UNCLOSED_QUOTE: &str = "unclosed quote";
pub const ENDS_WITH_BACKSLASH: &str = "cmdline ends with \\";

pub fn alias_changes_environment(alias: &str) -> String {
    format!(
        "alias '{alias}' changes environment variables.\nYou can use '!git' in the alias to do this"
    )
}

pub fn empty_alias(alias: &str) -> String {
    format!("empty alias for {alias}")
}

pub fn recursive_alias(alias: &str) -> String {
    format!("recursive alias: {alias}")
}

/// First line of the loop report; the chain follows, one name per line.
pub fn alias_loop(alias: &str) -> String {
    format!("alias loop detected: expansion of '{alias}' does not terminate:")
}

pub fn alias_expansion_failed(alias: &str, cmd: &str) -> String {
    format!("expansion of alias '{alias}' failed; '{cmd}' is not a git command")
}

pub fn while_expanding_alias(alias: &str, prog: &str, err: &str) -> String {
    format!("while expanding alias '{alias}': '{prog}': {err}")
}

/// `git-<cmd>` called by its dashed name, but `<cmd>` is not built in.
pub fn cannot_handle_as_builtin(cmd: &str) -> String {
    format!("cannot handle {cmd} as a builtin")
}

// ---------------------------------------------------------------- autocorrect

pub fn similar_commands(n: usize) -> &'static str {
    if n == 1 {
        "The most similar command is"
    } else {
        "The most similar commands are"
    }
}

pub fn autocorrect_warning(cmd: &str) -> String {
    format!("WARNING: You called a Git command named '{cmd}', which does not exist.")
}

pub fn autocorrect_immediately(assumed: &str) -> String {
    format!("Continuing under the assumption that you meant '{assumed}'.")
}

pub fn autocorrect_in(seconds: &str, assumed: &str) -> String {
    format!("Continuing in {seconds} seconds, assuming that you meant '{assumed}'.")
}

pub fn autocorrect_prompt(assumed: &str) -> String {
    format!("Run '{assumed}' instead [y/N]? ")
}
