// SPDX-License-Identifier: MIT
//! Messages of `git mergetool`, `git difftool` and the tool table they
//! share (CLEANROOM.md: user-visible strings are reproduced verbatim,
//! taken from the behaviour of the `git` binary, and collected here).

pub const MERGETOOL_USAGE: &str = "usage: git mergetool [--tool=tool] [--tool-help] [-y|--no-prompt|--prompt] [-g|--gui|--no-gui] [-O<orderfile>] [file to merge] ...";

pub const DIFFTOOL_USAGE: &[&str] =
    &["git difftool [<options>] [<commit> [<commit>]] [--] [<path>...]"];
pub const HELP_GUI: &str = "use `diff.guitool` instead of `diff.tool`";
pub const HELP_DIR_DIFF: &str = "perform a full-directory diff";
pub const HELP_NO_PROMPT: &str = "do not prompt before launching a diff tool";
pub const HELP_SYMLINKS: &str = "use symlinks in dir-diff mode";
pub const HELP_TOOL: &str = "use the specified diff tool";
pub const HELP_TOOL_HELP: &str = "print a list of diff tools that may be used with `--tool`";
pub const HELP_TRUST_EXIT_CODE: &str =
    "make 'git-difftool' exit when an invoked diff tool returns a non-zero exit code";
pub const HELP_EXTCMD: &str = "specify a custom command for viewing diffs";
pub const HELP_NO_INDEX: &str = "passed to `diff`";
pub const HELP_INDEX: &str = "opposite of --no-index";

pub fn no_files_need_merging() -> &'static str {
    "No files need merging"
}
pub const MERGING: &str = "Merging:";
pub fn tool_not_set(mode: &str, tool: &str) -> String {
    format!("{mode}tool.{tool}.cmd not set for tool '{tool}'")
}
pub fn unknown_configured_tool(key: &str, tool: &str) -> String {
    format!("git config option {key} set to unknown tool: {tool}\nResetting to default...")
}
pub fn guess_intro(key: &str, cmd: &str, candidates: &str) -> String {
    format!(
        "\nThis message is displayed because '{key}' is not configured.\n\
         See 'git {cmd} --tool-help' or 'git help config' for more details.\n\
         'git {cmd}' will now attempt to use one of the following tools:\n{candidates}"
    )
}
pub fn no_known_tool(kind: &str) -> String {
    format!("No known {kind} tool is available.")
}
pub fn tool_help_header(cmd: &str) -> String {
    format!("'git {cmd} --tool=<tool>' may be set to one of the following:")
}
pub const TOOL_HELP_USER_DEFINED: &str = "\tuser-defined:";
pub const TOOL_HELP_UNAVAILABLE: &str =
    "The following tools are valid, but not currently available:";
pub const TOOL_HELP_FOOTER: &str = "Some of the tools listed above only work in a windowed\n\
environment. If run in a terminal-only session, they will fail.";

pub fn normal_conflict(path: &str) -> String {
    format!("Normal merge conflict for '{path}':")
}
pub fn deleted_conflict(path: &str) -> String {
    format!("Deleted merge conflict for '{path}':")
}
pub fn symlink_conflict(path: &str) -> String {
    format!("Symbolic link merge conflict for '{path}':")
}
pub fn submodule_conflict(path: &str) -> String {
    format!("Submodule merge conflict for '{path}':")
}
pub fn side_local(what: &str) -> String {
    format!("  {{local}}: {what}")
}
pub fn side_remote(what: &str) -> String {
    format!("  {{remote}}: {what}")
}
pub const DESC_DELETED: &str = "deleted";
pub const DESC_MODIFIED: &str = "modified file";
pub const DESC_CREATED: &str = "created file";
pub fn desc_symlink(target: &str) -> String {
    format!("a symbolic link -> '{target}'")
}
pub fn desc_submodule(id: &str) -> String {
    format!("submodule commit {id}")
}
pub const PROMPT_DELETED: &str = "Use (m)odified or (d)eleted file, or (a)bort? ";
pub const PROMPT_LOCAL_REMOTE: &str = "Use (l)ocal or (r)emote, or (a)bort? ";
pub fn prompt_start_tool(tool: &str) -> String {
    format!("Hit return to start merge resolution tool ({tool}): ")
}
pub fn seems_unchanged(path: &str) -> String {
    format!("{path} seems unchanged.")
}
pub const PROMPT_SUCCESS: &str = "Was the merge successful [y/n]? ";
pub fn merge_failed(path: &str) -> String {
    format!("merge of {path} failed")
}
pub const PROMPT_CONTINUE: &str = "Continue merging other unresolved paths [y/n]? ";
pub fn not_need_merging(path: &str) -> String {
    format!("{path}: file does not need merging")
}
pub fn file_not_found(path: &str) -> String {
    format!("{path}: file not found")
}

pub fn viewing(counter: &str, total: &str, path: &str) -> String {
    format!("\nViewing ({counter}/{total}): '{path}'")
}
pub fn launch(tool: &str) -> String {
    format!("Launch '{tool}' [Y/n]? ")
}
pub fn incompatible2(a: &str, b: &str) -> String {
    format!("options '{a}' and '{b}' cannot be used together")
}
pub fn incompatible3(a: &str, b: &str, c: &str) -> String {
    format!("options '{a}', '{b}', and '{c}' cannot be used together")
}
pub fn cannot_run(cmd: &str, reason: &str) -> String {
    format!("cannot run {cmd}: {reason}")
}
pub fn both_modified(wt: &str, tmp: &str) -> String {
    format!("both files modified: '{wt}' and '{tmp}'.")
}
pub const WORKTREE_LEFT: &str = "working tree file has been left.";
pub fn temporary_files_exist(dir: &str) -> String {
    format!("temporary files exist in '{dir}'.")
}
pub const MAY_CLEANUP: &str = "you may want to cleanup or recover these.";
pub fn tool_failed(code: i32) -> String {
    format!("failed: {code}")
}
