// SPDX-License-Identifier: MIT
//! Usage, option help and messages of `git am` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &[
    "git am [<options>] [(<mbox> | <Maildir>)...]",
    "git am [<options>] (--continue | --skip | --abort)",
];

pub const HELP_INTERACTIVE: &str = "run interactively";
pub const HELP_NO_VERIFY: &str = "bypass pre-applypatch and applypatch-msg hooks";
pub const HELP_VERIFY: &str = "opposite of --no-verify";
pub const HELP_BINARY: &str = "historical option -- no-op";
pub const HELP_3WAY: &str = "allow fall back on 3way merging if needed";
pub const HELP_QUIET: &str = "be quiet";
pub const HELP_SIGNOFF: &str = "add a Signed-off-by trailer to the commit message";
pub const HELP_UTF8: &str = "recode into utf8 (default)";
pub const HELP_KEEP: &str = "pass -k flag to git-mailinfo";
pub const HELP_KEEP_NON_PATCH: &str = "pass -b flag to git-mailinfo";
pub const HELP_MESSAGE_ID: &str = "pass -m flag to git-mailinfo";
pub const HELP_KEEP_CR: &str = "pass --keep-cr flag to git-mailsplit for mbox format";
pub const HELP_SCISSORS: &str = "strip everything before a scissors line";
pub const HELP_QUOTED_CR: &str = "pass it through git-mailinfo";
pub const HELP_APPLY: &str = "pass it through git-apply";
pub const HELP_PATCH_FORMAT: &str = "format the patch(es) are in";
pub const HELP_RESOLVEMSG: &str = "override error message when patch failure occurs";
pub const HELP_CONTINUE: &str = "continue applying patches after resolving a conflict";
pub const HELP_RESOLVED: &str = "synonyms for --continue";
pub const HELP_SKIP: &str = "skip the current patch";
pub const HELP_ABORT: &str = "restore the original branch and abort the patching operation";
pub const HELP_QUIT: &str = "abort the patching operation but keep HEAD where it is";
pub const HELP_SHOW_CURRENT_PATCH: &str = "show the patch being applied";
pub const HELP_RETRY: &str = "try to apply current patch again";
pub const HELP_ALLOW_EMPTY: &str = "record the empty patch as an empty commit";
pub const HELP_COMMITTER_DATE: &str = "lie about committer date";
pub const HELP_IGNORE_DATE: &str = "use current timestamp for author date";
pub const HELP_RERERE: &str = "update the index with reused conflict resolution if possible";
pub const HELP_GPG_SIGN: &str = "GPG-sign commits";
pub const HELP_EMPTY: &str = "how to handle empty patches";
pub const HELP_REBASING: &str = "(internal use for git-rebase)";

pub const NOT_RESUMING: &str = "Resolve operation not in progress, we are not resuming.";
pub const FORMAT_DETECTION_FAILED: &str = "Patch format detection failed.";
pub const PATCH_IS_EMPTY: &str = "Patch is empty.";
pub const NO_CHANGES: &str = "No changes - did you forget to use 'git add'?\n\
If there is nothing left to stage, chances are that something else\n\
already introduced the same changes; you might want to skip this patch.";
pub const UNMERGED: &str = "You still have unmerged paths in your index.\n\
You should 'git add' each file with resolved conflicts to mark them as such.\n\
You might run `git rm` on a file to accept \"deleted by them\" for it.";
pub const RECORDED_EMPTY: &str = "No changes - recorded it as an empty commit.";
pub const USING_INDEX_INFO: &str = "Using index info to reconstruct a base tree...";
pub const FALLING_BACK: &str = "Falling back to patching base and 3-way merge...";
/// First line of an stgit series file (format detection).
pub const STGIT_SERIES_MARK: &str = "# This series applies on GIT commit";
/// First line of an hg export (format detection).
pub const HG_MARK: &str = "# HG changeset patch";
/// The subject of the message Pine keeps its folder data in (skipped).
pub const PINE_INTERNAL_DATA: &str = "DON'T DELETE THIS MESSAGE -- FOLDER INTERNAL DATA";
pub const NUL_IN_MESSAGE: &str = "a NUL byte in commit log message not allowed.";
pub const NO_FAKE_ANCESTOR: &str = "could not build fake ancestor";
pub const FAILED_TO_MERGE: &str = "Failed to merge in the changes.";
pub const MOVED_HEAD: &str =
    "You seem to have moved HEAD since the last 'am' failure.\nNot rewinding to ORIG_HEAD";
pub const FORMAT_FLOWED: &str =
    "Patch sent with format=flowed; space at the end of lines might be lost.";
pub const BINARY_NOOP: &str = "The -b/--binary option has been a no-op for long time, and\n\
it will be removed. Please do not use it anymore.";
pub const HINT_SHOW_PATCH: &str = "Use 'git am --show-current-patch=diff' to see the failed patch";
pub const HINT_CONTINUE: &str = "When you have resolved this problem, run \"git am --continue\".";
pub const HINT_SKIP: &str = "If you prefer to skip this patch, run \"git am --skip\" instead.";
pub const HINT_ALLOW_EMPTY: &str =
    "To record the empty patch as an empty commit, run \"git am --allow-empty\".";
pub const HINT_ABORT: &str =
    "To restore the original branch and stop patching, run \"git am --abort\".";
pub const INTERACTIVE_UNSUPPORTED: &str = "am --interactive is not supported by rgit yet";
pub const GPG_UNSUPPORTED: &str = "am -S/--gpg-sign is not supported by rgit yet";

pub fn previous_dir_exists(dir: &str) -> String {
    format!("previous rebase directory {dir} still exists but mbox given.")
}

pub fn dirty_index(paths: &str) -> String {
    format!("Dirty index: cannot apply patches (dirty: {paths})")
}

pub fn applying(subject: &str) -> String {
    format!("Applying: {subject}")
}

pub fn patch_failed(num: &str, subject: &str) -> String {
    format!("Patch failed at {num} {subject}")
}

pub fn skipping(subject: &str) -> String {
    format!("Skipping: {subject}")
}

pub fn creating_empty(subject: &str) -> String {
    format!("Creating an empty commit: {subject}")
}

pub fn invalid_value(option: &str, value: &str) -> String {
    format!("invalid value for '{option}': '{value}'")
}

pub fn cannot_be_used_together(a: &str, b: &str) -> String {
    format!("options '{a}' and '{b}' cannot be used together")
}

pub fn invalid_patch_format(value: &str) -> String {
    format!("Invalid value for --patch-format: {value}")
}

pub fn could_not_open(path: &str, reason: &str) -> String {
    format!("could not open '{path}' for reading: {reason}")
}

pub fn could_not_parse(path: &str) -> String {
    format!("could not parse {path}")
}
