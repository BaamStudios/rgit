// SPDX-License-Identifier: MIT
//! Messages of `git notes` and of notes shown and rewritten by other
//! commands.

pub const USAGE: &[&str] = &[
    "git notes [--ref <notes-ref>] [list [<object>]]",
    "git notes [--ref <notes-ref>] add [-f] [--allow-empty] [--[no-]separator|--separator=<paragraph-break>] [--[no-]stripspace] [-m <msg> | -F <file> | (-c | -C) <object>] [<object>] [-e]",
    "git notes [--ref <notes-ref>] copy [-f] <from-object> <to-object>",
    "git notes [--ref <notes-ref>] append [--allow-empty] [--[no-]separator|--separator=<paragraph-break>] [--[no-]stripspace] [-m <msg> | -F <file> | (-c | -C) <object>] [<object>] [-e]",
    "git notes [--ref <notes-ref>] edit [--allow-empty] [<object>]",
    "git notes [--ref <notes-ref>] show [<object>]",
    "git notes [--ref <notes-ref>] merge [-v | -q] [-s <strategy>] <notes-ref>",
    "git notes merge --commit [-v | -q]",
    "git notes merge --abort [-v | -q]",
    "git notes [--ref <notes-ref>] remove [<object>...]",
    "git notes [--ref <notes-ref>] prune [-n] [-v]",
    "git notes [--ref <notes-ref>] get-ref",
];
pub const USAGE_LIST: &[&str] = &["git notes [list [<object>]]"];
pub const USAGE_ADD: &[&str] = &["git notes add [<options>] [<object>]"];
pub const USAGE_COPY: &[&str] = &[
    "git notes copy [<options>] <from-object> <to-object>",
    "git notes copy --stdin [<from-object> <to-object>]...",
];
pub const USAGE_APPEND: &[&str] = &["git notes append [<options>] [<object>]"];
pub const USAGE_EDIT: &[&str] = &["git notes edit [<object>]"];
pub const USAGE_SHOW: &[&str] = &["git notes show [<object>]"];
pub const USAGE_MERGE: &[&str] = &[
    "git notes merge [<options>] <notes-ref>",
    "git notes merge --commit [<options>]",
    "git notes merge --abort [<options>]",
];
pub const USAGE_REMOVE: &[&str] = &["git notes remove [<object>]"];
pub const USAGE_PRUNE: &[&str] = &["git notes prune [<options>]"];
pub const USAGE_GET_REF: &[&str] = &["git notes get-ref"];

pub const HELP_REF: &str = "use notes from <notes-ref>";
pub const HELP_MESSAGE: &str = "note contents as a string";
pub const HELP_FILE: &str = "note contents in a file";
pub const HELP_REEDIT: &str = "reuse and edit specified note object";
pub const HELP_EDIT: &str = "edit note message in editor";
pub const HELP_REUSE: &str = "reuse specified note object";
pub const HELP_ALLOW_EMPTY: &str = "allow storing empty note";
pub const HELP_FORCE_ADD: &str = "replace existing notes";
pub const HELP_SEPARATOR: &str = "insert <paragraph-break> between paragraphs";
pub const HELP_STRIPSPACE: &str = "remove unnecessary whitespace";
pub const HELP_STDIN_COPY: &str = "read objects from stdin";
pub const HELP_FOR_REWRITE: &str = "load rewriting config for <command> (implies --stdin)";
pub const HELP_IGNORE_MISSING: &str = "attempt to remove non-existent note is not an error";
pub const HELP_STDIN_REMOVE: &str = "read object names from the standard input";
pub const HELP_DRY_RUN: &str = "do not remove, show only";
pub const HELP_PRUNE_VERBOSE: &str = "report pruned notes";
pub const GROUP_GENERAL: &str = "General options";
pub const GROUP_MERGE: &str = "Merge options";
pub const GROUP_COMMIT: &str = "Committing unmerged notes";
pub const GROUP_ABORT: &str = "Aborting notes merge resolution";
pub const HELP_MERGE_VERBOSE: &str = "be more verbose";
pub const HELP_MERGE_QUIET: &str = "be more quiet";
pub const HELP_STRATEGY: &str =
    "resolve notes conflicts using the given strategy (manual/ours/theirs/union/cat_sort_uniq)";
pub const HELP_COMMIT: &str = "finalize notes merge by committing unmerged notes";
pub const HELP_ABORT: &str = "abort notes merge";

pub const REV_LIST_NOTES: &str = "rev-list does not support display of notes";
pub const TOO_MANY_ARGUMENTS: &str = "too many arguments";
pub const TOO_FEW_ARGUMENTS: &str = "too few arguments";
pub const EDIT_DEPRECATED: &str = "The -m/-F/-c/-C options have been deprecated for the 'edit' subcommand.\n\
Please use 'git notes add -f -m/-F/-c/-C' instead.\n";
pub const SUPPLY_CONTENTS: &str = "please supply the note contents using either -m or -F option";
pub const EDITOR_INTRO: &str = "\nWrite/edit the notes for the following object:\n";
pub const NOTES_EDITMSG: &str = "NOTES_EDITMSG";
pub const MUST_SPECIFY_REF: &str = "must specify a notes ref to merge";
pub const CANNOT_MIX: &str = "cannot mix --commit, --abort or -s/--strategy";
pub const READ_PARTIAL_FAILED: &str = "failed to read ref NOTES_MERGE_PARTIAL";
pub const REMOVE_WORKTREE_FAILED: &str = "failed to remove 'git notes merge' worktree";
pub const AUTOMATIC_MERGE_FAILED: &str = "Automatic notes merge failed. Fix conflicts in %s and commit the result with 'git notes merge --commit', or abort the merge with 'git notes merge --abort'.";
pub const FAST_FORWARD: &str = "Fast-forward";
pub const UP_TO_DATE: &str = "Already up to date.";
pub const NO_MERGE_BASE: &str = "No merge base found; doing history-less merge";
pub const WORKTREE_EXISTS: &str = "You have not concluded your previous notes merge (%s exists).\n\
Please, use 'git notes merge --commit' or 'git notes merge --abort' to commit/abort the previous merge before you start a new notes merge.";

pub fn unknown_subcommand(s: &str) -> String {
    format!("unknown subcommand: `{s}'")
}

pub fn failed_to_resolve(name: &str) -> String {
    format!("failed to resolve '{name}' as a valid ref.")
}

pub fn failed_to_resolve_cap(name: &str) -> String {
    format!("Failed to resolve '{name}' as a valid ref.")
}

pub fn no_note_found(hex: &str) -> String {
    format!("no note found for object {hex}.")
}

pub fn refusing(verb: &str, refname: &str) -> String {
    format!("refusing to {verb} notes in {refname} (outside of refs/notes/)")
}

pub fn cannot_use_ref(refname: &str) -> String {
    format!("Cannot use notes ref {refname}")
}

pub fn cannot_add(hex: &str) -> String {
    format!(
        "Cannot add notes. Found existing notes for object {hex}. Use '-f' to overwrite existing notes"
    )
}

pub fn cannot_copy(hex: &str) -> String {
    format!(
        "Cannot copy notes. Found existing notes for object {hex}. Use '-f' to overwrite existing notes"
    )
}

pub fn overwriting(hex: &str) -> String {
    format!("Overwriting existing notes for object {hex}")
}

pub fn removing(name: &str) -> String {
    format!("Removing note for object {name}")
}

pub fn has_no_note(name: &str) -> String {
    format!("Object {name} has no note")
}

pub fn missing_source(hex: &str) -> String {
    format!("missing notes on source object {hex}. Cannot copy.")
}

pub fn failed_to_copy(from: &str, to: &str) -> String {
    format!("failed to copy notes from '{from}' to '{to}'")
}

pub fn malformed_line(line: &str) -> String {
    format!("malformed input line: '{line}'.")
}

pub fn non_blob(name: &str) -> String {
    format!("cannot read note data from non-blob object '{name}'.")
}

pub fn could_not_read(path: &str, err: &str) -> String {
    format!("could not open or read '{path}': {err}")
}

pub fn bad_value(what: &str, value: &str) -> String {
    format!("Bad {what} value: '{value}'")
}

pub fn refusing_rewrite(refname: &str) -> String {
    format!("Refusing to rewrite notes in {refname} (outside of refs/notes/)")
}

pub fn invalid_display_ref(refname: &str) -> String {
    format!("notes ref {refname} is invalid")
}

/// The commit message (and reflog message) of a notes change.
pub fn added_by(cmd: &str) -> String {
    format!("Notes added by 'git {cmd}'")
}

pub fn removed_by(cmd: &str) -> String {
    format!("Notes removed by 'git {cmd}'")
}

pub fn unknown_strategy_option(s: &str) -> String {
    format!("unknown -s/--strategy: {s}")
}

pub fn unknown_strategy(s: &str) -> String {
    format!("unknown notes merge strategy {s}")
}

pub fn refusing_merge(refname: &str) -> String {
    format!("refusing to merge notes in {refname} (outside of refs/notes/)")
}

pub fn failed_local_ref(refname: &str) -> String {
    format!("Failed to resolve local notes ref '{refname}'")
}

pub fn cannot_merge_empty(remote: &str, local: &str) -> String {
    format!("Cannot merge empty notes ref ({remote}) into empty notes ref ({local})")
}

pub fn merged_notes(remote: &str, local: &str) -> String {
    format!("Merged notes from {remote} into {local}")
}

pub fn one_merge_base(abbrev: &str) -> String {
    format!("One merge base found ({abbrev})")
}

pub fn multiple_merge_bases(abbrev: &str) -> String {
    format!("Multiple merge bases found. Using the first ({abbrev})")
}

pub fn merging_remote(remote: &str, local: &str, base: &str) -> String {
    format!("Merging remote commit {remote} into local commit {local} with merge-base {base}")
}

pub fn auto_merging(hex: &str) -> String {
    format!("Auto-merging notes for {hex}")
}

pub fn conflict_add_add(hex: &str) -> String {
    format!("CONFLICT (add/add): Merge conflict in notes for object {hex}")
}

pub fn conflict_content(hex: &str) -> String {
    format!("CONFLICT (content): Merge conflict in notes for object {hex}")
}

pub fn conflict_delete_modify(hex: &str, deleted: &str, modified: &str) -> String {
    format!(
        "CONFLICT (delete/modify): Notes for object {hex} deleted in {deleted} and modified in {modified}. Version from {modified} left in tree."
    )
}

pub fn using_local(hex: &str) -> String {
    format!("Using local notes for {hex}")
}

pub fn using_remote(hex: &str) -> String {
    format!("Using remote notes for {hex}")
}

pub fn concatenating(hex: &str) -> String {
    format!("Concatenating local and remote notes for {hex}")
}

pub fn concatenating_unique(hex: &str) -> String {
    format!("Concatenating unique lines in local and remote notes for {hex}")
}

pub fn merge_result(unmerged: usize, clean: bool) -> String {
    format!(
        "Merge result: {unmerged} unmerged notes and a {} notes tree",
        if clean { "clean" } else { "dirty" }
    )
}

pub fn automatic_merge_failed(worktree: &str) -> String {
    AUTOMATIC_MERGE_FAILED.replace("%s", worktree)
}

pub fn worktree_exists(what: &str) -> String {
    WORKTREE_EXISTS.replace("%s", what)
}

pub fn already_in_progress(refname: &str, path: &str) -> String {
    format!("a notes merge into {refname} is already in-progress at {path}")
}

pub fn committing_worktree(path: &str) -> String {
    format!("Committing notes in notes merge worktree at {path}")
}

pub fn removing_worktree(path: &str) -> String {
    format!("Removing notes merge worktree at {path}/*")
}
