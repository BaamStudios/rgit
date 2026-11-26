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

pub const MERGE_PSEUDOREFS: &str = "--merge requires one of the pseudorefs MERGE_HEAD, CHERRY_PICK_HEAD, REVERT_HEAD or REBASE_HEAD";

pub const FOLLOW_ONE_PATHSPEC: &str = "--follow requires exactly one pathspec";

pub fn follow_magic(words: &[&str]) -> String {
    let quoted: Vec<String> = words.iter().map(|w| format!("'{w}'")).collect();
    format!(
        "pathspec magic not supported by --follow: {}",
        quoted.join(", ")
    )
}

pub fn revision_and_filename(arg: &str) -> String {
    format!(
        "ambiguous argument '{arg}': both revision and filename\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'"
    )
}

pub fn no_such_path(arg: &str) -> String {
    format!(
        "{arg}: no such path in the working tree.\n\
         Use 'git <command> -- <path>...' to specify paths that do not exist locally."
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

pub fn could_not_read(id: &str) -> String {
    format!("Could not read {id}")
}

pub fn failed_to_traverse_parents(id: &str) -> String {
    format!("Failed to traverse parents of commit {id}")
}

pub fn cannot_simplify(commit: &str, parent: &str) -> String {
    format!("cannot simplify commit {commit} (because of {parent})")
}

pub fn walk_setup_failed() -> &'static str {
    "revision walk setup failed"
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

pub fn pretty_alias_loop(name: &str) -> String {
    format!("invalid --pretty format: '{name}' references an alias which points to itself")
}

pub fn invalid_decorate(value: &str) -> String {
    format!("invalid --decorate option: {value}")
}

pub const UNABLE_TO_PARSE_PRETTY: &str = "unable to parse --pretty format";

pub fn invalid_graph_color(color: &str) -> String {
    format!("ignoring invalid color '{color}' in log.graphColors")
}

pub fn unknown_diff_merges(value: &str) -> String {
    format!("invalid value for '--diff-merges': '{value}'")
}

pub fn not_a_blob(spec: &str) -> String {
    format!("mailmap is not a blob: {spec}")
}

pub fn unsupported_hidden_section(section: &str) -> String {
    format!("unsupported section for hidden refs: {section}")
}

pub const EXCLUDE_HIDDEN_TWICE: &str = "--exclude-hidden= passed more than once";

pub fn invalid_disk_usage(value: &str) -> String {
    format!(
        "invalid value for '--disk-usage=<format>': '{value}', the only allowed format is 'human'"
    )
}

/// `log -L` with a malformed argument.
pub fn line_log_bad_arg(arg: &str) -> String {
    format!("-L argument not 'start,end:file' or ':funcname:file': {arg}")
}

/// `log -L` with a path the starting commit does not have.
pub fn line_log_no_path(path: &str) -> String {
    format!("There is no path {path} in the commit")
}

/// `log -L` with two starting commits.
pub fn line_log_two_tips(a: &str, b: &str) -> String {
    format!("More than one commit to dig from: {a} and {b}?")
}

pub const LINE_LOG_NO_COMMIT: &str = "No commit specified?";
pub const LINE_LOG_PATHSPEC: &str = "-L<range>:<file> cannot be used with pathspec";
pub const LINE_LOG_FORMAT: &str = "-L does not yet support the requested diff format";

pub const Z_UNSUPPORTED: &str = "-z option used with unsupported option";
/// `log -g` with `--graph` (and other history limiting).
pub const REFLOG_HISTORY_LIMITING: &str =
    "cannot combine --walk-reflogs with history-limiting options";

/// `log -g` with a range or an exclusion.
pub fn cannot_walk_reflogs(name: &str) -> String {
    format!("cannot walk reflogs for {name}")
}

/// `--output=<file>` cannot be created.
pub fn could_not_open_for_writing(path: &str, err: &str) -> String {
    format!("could not open '{path}' for writing: {err}")
}
