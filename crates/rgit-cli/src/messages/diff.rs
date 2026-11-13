// SPDX-License-Identifier: MIT
//! Messages of the diff commands (`diff`, `diff-files`, `diff-index`,
//! `diff-tree`) and their shared options (CLEANROOM.md: user-visible
//! strings are reproduced verbatim and collected here).

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

pub fn no_merge_base(a: &str, b: &str) -> String {
    format!("{a}...{b}: no merge base")
}

pub fn multiple_merge_bases(a: &str, b: &str, base: &str) -> String {
    format!("warning: {a}...{b}: multiple merge bases, using {base}")
}
