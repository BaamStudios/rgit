// SPDX-License-Identifier: MIT
//! User-visible messages of the front end, in one place for review
//! (see CLEANROOM.md, "User-visible messages").

pub mod cat_file;
pub mod check_ignore;
pub mod config;
pub mod for_each_ref;
pub mod hash_object;
pub mod init;
pub mod ls_files;
pub mod ls_tree;
pub mod mktag;
pub mod mktree;
pub mod objects;
pub mod pack_objects;
pub mod pack_refs;
pub mod reflog;
pub mod rev_list;
pub mod show_ref;
pub mod switch;
pub mod update_index;
pub mod update_ref;
pub mod var;

/// The top-level usage (git's SYNOPSIS, reproduced because scripts and
/// tests compare it).
pub const GIT_USAGE: &str = "\
usage: git [-v | --version] [-h | --help] [-C <path>] [-c <name>=<value>]
           [--exec-path[=<path>]] [--html-path] [--man-path] [--info-path]
           [-p | --paginate | -P | --no-pager] [--no-replace-objects] [--no-lazy-fetch]
           [--no-optional-locks] [--no-advice] [--bare] [--git-dir=<path>]
           [--work-tree=<path>] [--namespace=<name>] [--config-env=<name>=<envvar>]
           <command> [<args>]
";

/// Our own text after the usage when no command (or `--help`) is given and
/// no `help` command is available.
pub fn help_fallback(commands: &[&str]) -> String {
    let mut s =
        String::from("\nrgit is an independent reimplementation of git, not the official git.\n");
    s.push_str("Commands implemented so far:\n\n");
    for c in commands {
        s.push_str("   ");
        s.push_str(c);
        s.push('\n');
    }
    s.push_str("\nGit's documentation applies: https://git-scm.com/docs\n");
    s
}

pub fn unknown_option(opt: &str) -> String {
    format!("unknown option: {opt}")
}

pub fn no_directory_given(opt: &str) -> String {
    format!("no directory given for '{opt}' option")
}

pub const C_EXPECTS_CONFIG: &str = "-c expects a configuration string";
pub const NO_NAMESPACE: &str = "no namespace given for --namespace";
pub const NO_CONFIG_KEY: &str = "no config key given for --config-env";
pub const NO_ATTR_SOURCE: &str = "no attribute source given for --attr-source";

pub fn cannot_change_to(dir: &str, err: &str) -> String {
    format!("cannot change to '{dir}': {err}")
}

pub fn invalid_config_format(spec: &str) -> String {
    format!("invalid config format: {spec}")
}

pub fn missing_env_name_for_config(key: &str) -> String {
    format!("missing environment variable name for configuration '{key}'")
}

pub fn missing_env_for_config(var: &str, key: &str) -> String {
    format!("missing environment variable '{var}' for configuration '{key}'")
}

pub fn unsupported_listing(group: &str) -> String {
    format!("unsupported command listing type '{group}'")
}

pub fn not_a_git_command(cmd: &str) -> String {
    format!("git: '{cmd}' is not a git command. See 'git --help'.")
}

pub fn cannot_get_cwd(err: &str) -> String {
    format!("unable to get current working directory: {err}")
}

/// `version --build-options` when the build has no commit id.
pub const NO_BUILD_COMMIT: &str = "no commit associated with this build";

/// Second line of `version` when not invoked as `git`.
pub const NOT_OFFICIAL_GIT: &str = "rgit is an independent reimplementation, not the official git.";

/// `git symbolic-ref` usage lines.
pub const SYMBOLIC_REF_USAGE: &[&str] = &[
    "git symbolic-ref [-m <reason>] <name> <ref>",
    "git symbolic-ref [-q] [--short] [--no-recurse] <name>",
    "git symbolic-ref --delete [-q] <name>",
];
pub const SYMBOLIC_REF_HELP_QUIET: &str = "suppress error message for non-symbolic (detached) refs";
pub const SYMBOLIC_REF_HELP_DELETE: &str = "delete symbolic ref";
pub const SYMBOLIC_REF_HELP_SHORT: &str = "shorten ref output";
pub const SYMBOLIC_REF_HELP_RECURSE: &str = "recursively dereference (default)";
pub const SYMBOLIC_REF_HELP_REASON: &str = "reason of the update";

pub fn not_a_symbolic_ref(name: &str) -> String {
    format!("ref {name} is not a symbolic ref")
}

/// rgit wording: a rev-parse argument that is not implemented yet.
pub fn rev_parse_not_supported(arg: &str) -> String {
    format!("rgit rev-parse: '{arg}' is not supported yet")
}

pub fn cannot_chdir(dir: &str, reason: &str) -> String {
    format!("cannot chdir to '{dir}': {reason}")
}

/// Print `error: <msg>` to standard error.
pub fn print_error(msg: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "error: {msg}");
}

/// A configuration value that cannot be used, given on the command line.
pub fn unable_to_parse_config(key: &str, source: &str) -> String {
    format!("unable to parse '{key}' from {source}")
}

/// A configuration value that cannot be used, in a file.
pub fn bad_config_variable(key: &str, file: &str, line: usize) -> String {
    format!("bad config variable '{key}' in file '{file}' at line {line}")
}

pub fn unknown_hint_type(value: &str) -> String {
    format!("unknown hint type for 'core.disambiguate': {value}")
}

pub fn abbrev_out_of_range(n: i64) -> String {
    format!("abbrev length out of range: {n}")
}

pub fn malformed_push_default(value: &str) -> String {
    format!("malformed value for push.default: {value}")
}

pub const PUSH_DEFAULT_VALUES: &str =
    "must be one of nothing, matching, simple, upstream or current";

/// `git rev-parse -h`.
pub const REV_PARSE_USAGE: &str = "\
usage: git rev-parse --parseopt [<options>] -- [<args>...]
   or: git rev-parse --sq-quote [<arg>...]
   or: git rev-parse [<options>] [<arg>...]

Run \"git rev-parse --parseopt -h\" for more information on the first usage.
";

/// `rev-parse --verify` without exactly one revision.
pub const NEEDED_SINGLE_REVISION: &str = "Needed a single revision";

/// An argument that is neither a revision nor a path.
pub fn ambiguous_argument(arg: &str) -> String {
    format!(
        "ambiguous argument '{arg}': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'"
    )
}

/// A path argument (after the first path) that does not exist.
pub fn no_such_path(arg: &str) -> String {
    format!(
        "{arg}: no such path in the working tree.\n\
         Use 'git <command> -- <path>...' to specify paths that do not exist locally."
    )
}

/// A non-revision before `--`.
pub fn bad_revision(arg: &str) -> String {
    format!("bad revision '{arg}'")
}

/// An option after a path.
pub fn option_after_path(arg: &str) -> String {
    format!("option '{arg}' must come before non-option arguments")
}

pub fn option_requires_argument(opt: &str) -> String {
    format!("{opt} requires an argument")
}

pub fn unknown_abbrev_ref_mode(mode: &str) -> String {
    format!("unknown mode for --abbrev-ref: {mode}")
}

pub fn bad_format_open(cmd: &str, element: &str) -> String {
    format!("bad {cmd} format: element '{element}' does not start with '('")
}

pub fn bad_format_close(cmd: &str, element: &str) -> String {
    format!("bad {cmd} format: element '{element}' does not end in ')'")
}

pub fn bad_format_field(cmd: &str, field: &str) -> String {
    format!("bad {cmd} format: %{field}")
}

pub fn abbrev_not_numeric(opt: &str) -> String {
    format!("option `{opt}' expects a numerical value")
}
