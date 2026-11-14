// SPDX-License-Identifier: MIT
//! Messages and usage of `git rev-list` and `git log` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

/// `git rev-list` usage, printed for unknown options and missing revisions.
pub const REV_LIST_USAGE: &str = "\
usage: git rev-list [<options>] <commit>... [--] [<path>...]

  limiting output:
    --max-count=<n>
    --max-age=<epoch>
    --min-age=<epoch>
    --sparse
    --no-merges
    --min-parents=<n>
    --no-min-parents
    --max-parents=<n>
    --no-max-parents
    --remove-empty
    --all
    --branches
    --tags
    --remotes
    --stdin
    --exclude-hidden=[fetch|receive|uploadpack]
    --quiet
  ordering output:
    --topo-order
    --date-order
    --reverse
  formatting output:
    --parents
    --children
    --objects | --objects-edge
    --disk-usage[=human]
    --unpacked
    --header | --pretty
    --[no-]object-names
    --abbrev=<n> | --no-abbrev
    --abbrev-commit
    --left-right
    --count
    -z
  special purpose:
    --bisect
    --bisect-vars
    --bisect-all
";

/// `git log -h`.
pub const LOG_USAGE: &str = "\
usage: git log [<options>] [<revision-range>] [[--] <path>...]
   or: git show [<options>] <object>...

    -q, --[no-]quiet      suppress diff output
    --[no-]source         show source
    --[no-]use-mailmap    use mail map file
    --[no-]mailmap        alias of --use-mailmap
    --clear-decorations   clear all previously-defined decoration filters
    --[no-]decorate-refs <pattern>
                          only decorate refs that match <pattern>
    --[no-]decorate-refs-exclude <pattern>
                          do not decorate refs that match <pattern>
    --[no-]decorate[=...] decorate options
    -L <range:file>       trace the evolution of line range <start>,<end> or function :<funcname> in <file>

";

/// `git log --help-all` (also lists hidden options).
pub const LOG_USAGE_ALL: &str = "\
usage: git log [<options>] [<revision-range>] [[--] <path>...]
   or: git show [<options>] <object>...

    -q, --[no-]quiet      suppress diff output
    --[no-]source         show source
    --[no-]use-mailmap    use mail map file
    --[no-]i-still-use-this
                          <use this deprecated command>
    --[no-]mailmap        alias of --use-mailmap
    --clear-decorations   clear all previously-defined decoration filters
    --[no-]decorate-refs <pattern>
                          only decorate refs that match <pattern>
    --[no-]decorate-refs-exclude <pattern>
                          do not decorate refs that match <pattern>
    --[no-]decorate[=...] decorate options
    -L <range:file>       trace the evolution of line range <start>,<end> or function :<funcname> in <file>

";

pub fn ambiguous_argument(arg: &str) -> String {
    format!(
        "ambiguous argument '{arg}': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'"
    )
}

pub fn bad_revision(arg: &str) -> String {
    format!("bad revision '{arg}'")
}

pub fn not_an_integer(arg: &str) -> String {
    format!("'{arg}': not an integer")
}

pub fn unrecognized_argument(arg: &str) -> String {
    format!("unrecognized argument: {arg}")
}

pub fn unborn_branch(branch: &str) -> String {
    format!("your current branch '{branch}' does not have any commits yet")
}

pub fn bad_object(id: &str) -> String {
    format!("bad object {id}")
}

pub fn invalid_pretty(name: &str) -> String {
    format!("invalid --pretty format: {name}")
}

pub fn not_supported(what: &str) -> String {
    format!("rgit: {what} is not supported yet")
}

pub fn not_epoch_seconds(arg: &str) -> String {
    format!("'{arg}': not a number of seconds since epoch")
}

pub fn requires_value(option: &str) -> String {
    format!("Option '{option}' requires a value")
}

pub const INVALID_NO_WALK: &str = "invalid argument to --no-walk";

pub fn invalid_stdin_option(arg: &str) -> String {
    format!("invalid option '{arg}' in --stdin mode")
}

pub fn cannot_be_used_together(a: &str, b: &str) -> String {
    format!("options '{a}' and '{b}' cannot be used together")
}

pub const MARKED_COUNT_OBJECTS: &str = "marked counting and '--objects' cannot be used together";

pub const NO_BOTTOM_COMMITS: &str = "--ancestry-path given but there are no bottom commits";
