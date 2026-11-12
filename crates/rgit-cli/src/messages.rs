// SPDX-License-Identifier: MIT
//! User-visible messages of the front end, in one place for review
//! (see CLEANROOM.md, "User-visible messages").

pub mod cat_file;
pub mod check_ignore;
pub mod config;
pub mod hash_object;
pub mod mktag;
pub mod mktree;
pub mod objects;
pub mod pack_objects;
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
