// SPDX-License-Identifier: MIT
//! Messages of the diff commands (`diff`, `diff-files`, `diff-index`,
//! `diff-tree`) and their shared options (CLEANROOM.md: user-visible
//! strings are reproduced verbatim and collected here).

/// Reflog message of the textconv cache (`refs/notes/textconv/<driver>`).
pub const NOTES_CACHE_REFLOG: &str = "update notes cache";

pub const DIFF_FILES_USAGE: &str = "usage: git diff-files [-q] [-0 | -1 | -2 | -3 | -c | --cc] [<common-diff-options>] [<path>...]\n";

pub const DIFF_INDEX_USAGE: &str = "usage: git diff-index [-m] [--cached] [--merge-base] [<common-diff-options>] <tree-ish> [<path>...]\n";

pub const DIFF_USAGE: &str = "usage: git diff [<options>] [<commit>] [--] [<path>...]
   or: git diff [<options>] --cached [--merge-base] [<commit>] [--] [<path>...]
   or: git diff [<options>] [--merge-base] <commit> [<commit>...] <commit> [--] [<path>...]
   or: git diff [<options>] <commit>...<commit> [--] [<path>...]
   or: git diff [<options>] <blob> <blob>
   or: git diff [<options>] --no-index [--] <path> <path> [<pathspec>...]
";

/// The option summary all diff commands append to their usage line.
pub const COMMON_OPTIONS: &str = "
common diff options:
  -z            output diff-raw with lines terminated with NUL.
  -p            output patch format.
  -u            synonym for -p.
  --patch-with-raw
                output both a patch and the diff-raw format.
  --stat        show diffstat instead of patch.
  --numstat     show numeric diffstat instead of patch.
  --patch-with-stat
                output a patch and prepend its diffstat.
  --name-only   show only names of changed files.
  --name-status show names and status of changed files.
  --full-index  show full object name on index lines.
  --abbrev=<n>  abbreviate object names in diff-tree header and diff-raw.
  -R            swap input file pairs.
  -B            detect complete rewrites.
  -M            detect renames.
  -C            detect copies.
  --find-copies-harder
                try unchanged files as candidate for copy detection.
  -l<n>         limit rename attempts up to <n> paths.
  -O<file>      reorder diffs according to the <file>.
  -S<string>    find filepair whose only one side contains the string.
  --pickaxe-all
                show all files diff when -S is used and hit is found.
  -a  --text    treat all files as text.

";

pub const EXCLUSIVE_FORMATS: &str =
    "options '--name-only', '--name-status', '--check', and '-s' cannot be used together";

pub fn invalid_option(arg: &str) -> String {
    format!("error: invalid option: {arg}\n")
}

pub fn expects_numerical(option: &str) -> String {
    format!("error: {option} expects a numerical value\n")
}

pub fn invalid_stat_value(value: &str) -> String {
    format!("error: invalid --stat value: {value}\n")
}

pub fn not_supported(what: &str) -> String {
    format!("rgit: {what} is not supported yet")
}

pub fn bad_tree_object(name: &str) -> String {
    format!("bad tree object {name}")
}

pub fn could_not_access(path: &str) -> String {
    format!("Could not access '{path}'")
}

pub fn could_not_read(id: &str) -> String {
    format!("unable to read tree ({id})")
}

pub fn unable_to_read_blob(hex: &str) -> String {
    format!("unable to read {hex}")
}

pub fn read_failed(path: &str) -> String {
    format!("cannot read '{path}'")
}

pub const UNMERGED_PATH: &str = "* Unmerged path ";

/// The last line of `--stat` and `--shortstat`.
pub fn stat_summary(files: usize, insertions: usize, deletions: usize) -> String {
    let mut s = format!(" {files} file{} changed", if files == 1 { "" } else { "s" });
    if files == 0 {
        return s;
    }
    if insertions > 0 || deletions == 0 {
        s.push_str(&format!(
            ", {insertions} insertion{}(+)",
            if insertions == 1 { "" } else { "s" }
        ));
    }
    if deletions > 0 || insertions == 0 {
        s.push_str(&format!(
            ", {deletions} deletion{}(-)",
            if deletions == 1 { "" } else { "s" }
        ));
    }
    s
}

pub fn binary_stat(old: usize, new: usize) -> String {
    format!("Bin {old} -> {new} bytes")
}

pub const UNMERGED_STAT: &str = "Unmerged";

pub const NO_INDEX_USAGE: &str =
    "usage: git diff --no-index [<options>] <path> <path> [<pathspec>...]\n";
/// The hidden option `diff --help-all` lists outside a repository.
pub const NO_INDEX_HIDDEN: &str = "    --no-index\n\n";
/// `diff --help-all` inside a repository is an unknown option.
pub const INVALID_HELP_ALL: &str = "error: invalid option: --help-all\n";

pub fn no_merge_base(a: &str, b: &str) -> String {
    format!("{a}...{b}: no merge base")
}

pub const NO_MERGE_BASE_FOUND: &str = "no merge base found";
pub const MULTIPLE_MERGE_BASES_FOUND: &str = "multiple merge bases found";
pub const MERGE_BASE_WITH_RANGES: &str = "--merge-base does not work with ranges";
pub const MERGE_BASE_TWO_COMMITS: &str = "--merge-base only works with two commits";
pub const STDIN_WITH_MERGE_BASE: &str =
    "options '--stdin' and '--merge-base' cannot be used together";

pub fn not_a_commit(hex: &str, kind: &str) -> String {
    format!("error: object {hex} is a {kind}, not a commit")
}

pub fn more_than_two_blobs(name: &str) -> String {
    format!("more than two blobs given: '{name}'")
}

pub fn multiple_merge_bases(a: &str, b: &str, base: &str) -> String {
    format!("warning: {a}...{b}: multiple merge bases, using {base}")
}

/// `--diff-algorithm=<unknown>` (followed by exit code 129).
pub const BAD_DIFF_ALGORITHM: &str =
    "error: option diff-algorithm accepts \"myers\", \"minimal\", \"patience\" and \"histogram\"\n";

/// A one-letter option that needs a value was the last argument.
pub fn switch_requires_value(switch: &str) -> String {
    format!("error: switch `{switch}' requires a value\n")
}

/// An option that needs a value was the last argument.
pub fn requires_value(option: &str) -> String {
    format!("error: option `{option}' requires a value\n")
}

/// `diff.algorithm` has a value git does not know.
pub fn unknown_config_value(key: &str, value: &str) -> String {
    format!("unknown value for config '{key}': {value}")
}

/// A bad score after `-M` (`find-renames`) or `-C` (`find-copies`).
pub fn invalid_score(option: &str) -> String {
    format!("error: invalid argument to {option}\n")
}

pub const BREAK_FORM: &str = "error: break-rewrites expects <n>/<m> form\n";

pub fn unknown_change_class(c: char, value: &str) -> String {
    format!("error: unknown change class '{c}' in --diff-filter={value}\n")
}

pub fn switch_expects_integer(switch: char) -> String {
    format!("error: switch `{switch}' expects an integer value with an optional k/m/g suffix\n")
}

pub fn unable_to_resolve(name: &str) -> String {
    format!("error: unable to resolve '{name}'\n")
}

pub fn invalid_regex(_pattern: &str, reason: &str) -> String {
    format!("invalid regex: {reason}")
}

pub const CORRUPTED_CACHE_TREE: &str = "corrupted cache-tree has entries not present in index";

pub fn pickaxe_needs_argument(option: &str) -> String {
    format!("error: {option} requires a non-empty argument\n")
}

pub const PICKAXE_EXCLUSIVE: &str =
    "options '-G', '-S', and '--find-object' cannot be used together";

pub const PICKAXE_ALL_WITH_FIND_OBJECT: &str = "options '--pickaxe-all' and '--find-object' cannot be used together, use '--pickaxe-all' with '-G' and '-S'";

pub const G_WITH_PICKAXE_REGEX: &str =
    "options '-G' and '--pickaxe-regex' cannot be used together, use '--pickaxe-regex' with '-S'";

pub const RENAME_LIMIT_SKIPPED: &str =
    "warning: exhaustive rename detection was skipped due to too many files.\n";

pub const RENAME_LIMIT_DEGRADED: &str =
    "warning: only found copies from modified paths due to too many files.\n";

pub fn rename_limit_hint(n: usize) -> String {
    format!(
        "warning: you may want to set your diff.renameLimit variable to at least {n} and retry the command.\n"
    )
}

pub fn invalid_ignore_regex(pattern: &str) -> String {
    format!("error: invalid regex given to -I: '{pattern}'\n")
}

pub const BAD_WS_ERROR_HIGHLIGHT: &str = "error: unknown value after ws-error-highlight=\n";

pub const WS_RULE_CONFLICT: &str = "cannot enforce both tab-in-indent and indent-with-non-tab";

pub fn ws_check_line(path: &str, line: usize, what: &str) -> String {
    format!("{path}:{line}: {what}.\n")
}

pub fn leftover_conflict_marker(path: &str, line: usize) -> String {
    format!("{path}:{line}: leftover conflict marker\n")
}

pub const WS_TRAILING: &str = "trailing whitespace";
pub const WS_SPACE_BEFORE_TAB: &str = "space before tab in indent";
pub const WS_INDENT_WITH_SPACES: &str = "indent with spaces";
pub const WS_TAB_IN_INDENT: &str = "tab in indent";
pub const WS_BLANK_AT_EOF: &str = "new blank line at EOF";
pub const WS_INCOMPLETE: &str = "no newline at the end of file";

pub fn tab_width_out_of_range(value: &str) -> String {
    format!("tabwidth {value} out of range")
}

pub fn bad_word_diff(value: &str) -> String {
    format!("error: bad --word-diff argument: {value}\n")
}

pub fn invalid_word_regex(pattern: &str) -> String {
    format!("invalid regular expression: {pattern}")
}

pub const COLOR_MOVED_MUST_BE: &str = "color moved setting must be one of 'no', 'default', 'blocks', 'zebra', 'dimmed-zebra', 'plain'";

pub fn bad_color_moved(value: &str) -> String {
    format!("error: {COLOR_MOVED_MUST_BE}\nerror: bad --color-moved argument: {value}\n")
}

pub fn unknown_color_moved_ws(mode: &str) -> String {
    format!(
        "unknown color-moved-ws mode '{mode}', possible values are 'ignore-space-change', 'ignore-space-at-eol', 'ignore-all-space', 'allow-indentation-change'"
    )
}

pub const COLOR_MOVED_WS_INDENT_COMBINED: &str =
    "color-moved-ws: allow-indentation-change cannot be combined with other whitespace modes";

pub fn bad_color_moved_ws(reason: &str, value: &str) -> String {
    format!("error: {reason}\nerror: invalid mode '{value}' in --color-moved-ws\n")
}

pub fn dirstat_unknown(param: &str) -> String {
    format!("  Unknown dirstat parameter '{param}'\n")
}

pub fn dirstat_bad_percentage(param: &str) -> String {
    format!("  Failed to parse dirstat cut-off percentage '{param}'\n")
}

pub fn dirstat_option_errors(errors: &str) -> String {
    format!("Failed to parse --dirstat/-X option parameter:\n{errors}")
}

pub fn dirstat_config_errors(errors: &str) -> String {
    format!("warning: Found errors in 'diff.dirstat' config variable:\n{errors}\n")
}

pub fn expects_non_negative(option: &str) -> String {
    format!("error: {option} expects a non-negative integer\n")
}

pub fn expects_magnitude(option: &str) -> String {
    format!(
        "error: option `{option}' expects a non-negative integer value with an optional k/m/g suffix\n"
    )
}

pub const FIND_OBJECT_NEEDS_REPO: &str = "error: --find-object requires a git repository\n";

pub const NO_INDEX_STDIN_DIR: &str = "cannot compare stdin to a directory";

pub const NO_INDEX_PIPE_DIR: &str = "cannot compare a named pipe to a directory";

pub fn orderfile_failed(path: &str) -> String {
    format!("failed to read orderfile '{path}'")
}

pub fn no_such_path_in_diff(path: &str) -> String {
    format!("No such path '{path}' in the diff")
}

pub fn could_not_open_output(path: &str) -> String {
    format!("could not open '{path}'")
}

pub const MAX_DEPTH_WILDCARD: &str = "--max-depth cannot be used with wildcard pathspecs";

pub const NO_INDEX_PATHSPEC_NEEDS_DIRS: &str = "warning: Limiting comparison with pathspecs is only supported if both paths are directories.\n";

/// The diff options as the parse-options based commands list them in
/// `-h` (`range-diff`, `diff-pairs`, `diff --no-index` outside a
/// repository).
pub const OPTIONS_GROUP: &str = "\
Diff output format options
    -p, --patch           generate patch
    -s, --no-patch        suppress diff output
    -u                    generate patch
    -U, --unified[=<n>]   generate diffs with <n> lines context
    -W, --[no-]function-context
                          generate diffs with <n> lines context
    --raw                 generate the diff in raw format
    --patch-with-raw      synonym for '-p --raw'
    --patch-with-stat     synonym for '-p --stat'
    --numstat             machine friendly --stat
    --shortstat           output only the last line of --stat
    -X, --dirstat[=<param1>,<param2>...]
                          output the distribution of relative amount of changes for each sub-directory
    --cumulative          synonym for --dirstat=cumulative
    --dirstat-by-file[=<param1>,<param2>...]
                          synonym for --dirstat=files,<param1>,<param2>...
    --check               warn if changes introduce conflict markers or whitespace errors
    --summary             condensed summary such as creations, renames and mode changes
    --name-only           show only names of changed files
    --name-status         show only names and status of changed files
    --stat[=<width>[,<name-width>[,<count>]]]
                          generate diffstat
    --stat-width <width>  generate diffstat with a given width
    --stat-name-width <width>
                          generate diffstat with a given name width
    --stat-graph-width <width>
                          generate diffstat with a given graph width
    --stat-count <count>  generate diffstat with limited lines
    --[no-]compact-summary
                          generate compact summary in diffstat
    --binary              output a binary diff that can be applied
    --[no-]full-index     show full pre- and post-image object names on the \"index\" lines
    --[no-]color[=<when>] show colored diff
    --ws-error-highlight <kind>
                          highlight whitespace errors in the 'context', 'old' or 'new' lines in the diff
    -z                    do not munge pathnames and use NULs as output field terminators in --raw or --numstat
    --[no-]abbrev[=<n>]   use <n> digits to display object names
    --src-prefix <prefix> show the given source prefix instead of \"a/\"
    --dst-prefix <prefix> show the given destination prefix instead of \"b/\"
    --line-prefix <prefix>
                          prepend an additional prefix to every line of output
    --no-prefix           do not show any source or destination prefix
    --default-prefix      use default prefixes a/ and b/
    --inter-hunk-context <n>
                          show context between diff hunks up to the specified number of lines
    --output-indicator-new <char>
                          specify the character to indicate a new line instead of '+'
    --output-indicator-old <char>
                          specify the character to indicate an old line instead of '-'
    --output-indicator-context <char>
                          specify the character to indicate a context instead of ' '

Diff rename options
    -B, --break-rewrites[=<n>[/<m>]]
                          break complete rewrite changes into pairs of delete and create
    -M, --find-renames[=<n>]
                          detect renames
    -D, --irreversible-delete
                          omit the preimage for deletes
    -C, --find-copies[=<n>]
                          detect copies
    --[no-]find-copies-harder
                          use unmodified files as source to find copies
    --no-renames          disable rename detection
    --[no-]rename-empty   use empty blobs as rename source
    --[no-]follow         continue listing the history of a file beyond renames
    -l <n>                limit to cheap rename/copy detection if the number of rename/copy targets exceeds this value

Diff algorithm options
    --minimal             produce the smallest possible diff
    -w, --ignore-all-space
                          ignore whitespace when comparing lines
    -b, --ignore-space-change
                          ignore changes in amount of whitespace
    --ignore-space-at-eol ignore changes in whitespace at EOL
    --ignore-cr-at-eol    ignore carrier-return at the end of line
    --ignore-blank-lines  ignore changes whose lines are all blank
    -I, --[no-]ignore-matching-lines <regex>
                          ignore changes whose all lines match <regex>
    --[no-]indent-heuristic
                          heuristic to shift diff hunk boundaries for easy reading
    --patience            generate diff using the \"patience diff\" algorithm
    --histogram           generate diff using the \"histogram diff\" algorithm
    --diff-algorithm <algorithm>
                          choose a diff algorithm
    --anchored <text>     generate diff using the \"anchored diff\" algorithm
    --word-diff[=<mode>]  show word diff, using <mode> to delimit changed words
    --word-diff-regex <regex>
                          use <regex> to decide what a word is
    --color-words[=<regex>]
                          equivalent to --word-diff=color --word-diff-regex=<regex>
    --[no-]color-moved[=<mode>]
                          moved lines of code are colored differently
    --[no-]color-moved-ws <mode>
                          how white spaces are ignored in --color-moved

Other diff options
    --[no-]relative[=<prefix>]
                          when run from subdir, exclude changes outside and show relative paths
    -a, --[no-]text       treat all files as text
    -R                    swap two inputs, reverse the diff
    --[no-]exit-code      exit with 1 if there were differences, 0 otherwise
    --[no-]quiet          disable all output of the program
    --[no-]ext-diff       allow an external diff helper to be executed
    --[no-]textconv       run external text conversion filters when comparing binary files
    --ignore-submodules[=<when>]
                          ignore changes to submodules in the diff generation
    --submodule[=<format>]
                          specify how differences in submodules are shown
    --ita-invisible-in-index
                          hide 'git add -N' entries from the index
    --ita-visible-in-index
                          treat 'git add -N' entries as real in the index
    -S <string>           look for differences that change the number of occurrences of the specified string
    -G <regex>            look for differences that change the number of occurrences of the specified regex
    --pickaxe-all         show all changes in the changeset with -S or -G
    --pickaxe-regex       treat <string> in -S as extended POSIX regular expression
    -O <file>             control the order in which files appear in the output
    --rotate-to <path>    show the change in the specified path first
    --skip-to <path>      skip the output to the specified path
    --find-object <object-id>
                          look for differences that change the number of occurrences of the specified object
    --diff-filter [(A|C|D|M|R|T|U|X|B)...[*]]
                          select files by diff type
    --max-depth <depth>   maximum tree depth to recurse
    --output <file>       output to a specific file

";
