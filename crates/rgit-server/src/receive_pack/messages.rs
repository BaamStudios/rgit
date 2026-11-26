// SPDX-License-Identifier: MIT
//! User-visible messages of `receive-pack`, reproduced verbatim from what
//! the reference `git` binary prints (see CLEANROOM.md, "User-visible
//! messages"). Texts are given without the `error: ` / `warning: ` /
//! `fatal: ` prefix unless noted. The `REASON_*` strings are the reasons
//! of `ng <ref> <reason>` lines of the status report.

// Reasons of rejected commands.
pub const REASON_UNPACKER: &str = "unpacker error";
pub const REASON_MISSING_OBJECTS: &str = "missing necessary objects";
pub const REASON_HIDDEN: &str = "deny updating a hidden ref";
pub const REASON_PRE_RECEIVE: &str = "pre-receive hook declined";
pub const REASON_FUNNY: &str = "funny refname";
pub const REASON_CHECKED_OUT: &str = "branch is currently checked out";
pub const REASON_BAD_PACK: &str = "bad pack";
pub const REASON_DELETION: &str = "deletion prohibited";
pub const REASON_DELETE_CURRENT: &str = "deletion of the current branch prohibited";
pub const REASON_NON_FF: &str = "non-fast-forward";
pub const REASON_HOOK: &str = "hook declined";
pub const REASON_ATOMIC: &str = "atomic push failure";
pub const REASON_TRANSACTION: &str = "atomic transaction failed";
pub const REASON_FAILED_REFS: &str = "failed to update refs";
pub const REASON_FAILED_REF: &str = "failed to update ref";
pub const REASON_SHALLOW: &str = "shallow update not allowed";
pub const REASON_PROC_RECEIVE_MISSING: &str = "fail to run proc-receive hook";
pub const REASON_PROC_RECEIVE_NO_REPORT: &str = "proc-receive failed to report status";
pub const REASON_PROC_RECEIVE_FAILED: &str = "proc-receive failed";
pub const REASON_ALIASED: &str = "inconsistent aliased update";
pub const REASON_INCONSISTENT_OPTIONS: &str = "inconsistent push options";
pub const REASON_PUSH_TO_CHECKOUT: &str = "hook declined";

// Rejections of ref transactions (per reference).
pub const REASON_REF_NOT_EXIST: &str = "reference does not exist";
pub const REASON_REF_EXISTS: &str = "reference already exists";
pub const REASON_REF_OLD_VALUE: &str = "incorrect old value provided";
pub const REASON_REF_CONFLICT: &str = "refname conflict";
pub const REASON_REF_EXPECTED_SYMREF: &str = "expected symref but found regular ref";
pub const REASON_REF_NEW_VALUE: &str = "invalid new value provided";

// updateInstead.
pub const UP_TO_DATE_CHECK_FAILED: &str = "Up-to-date check failed";
pub const UNSTAGED_CHANGES: &str = "Working directory has unstaged changes";
pub const STAGED_CHANGES: &str = "Working directory has staged changes";
pub const COULD_NOT_UPDATE_WORKTREE: &str = "Could not update working tree to new HEAD";
pub const NEEDS_WORKTREE: &str = "denyCurrentBranch = updateInstead needs a worktree";

/// `receive.denyCurrentBranch` left unset: the explanation that follows
/// the refusal.
pub const REFUSE_CURRENT_BRANCH: &str = "By default, updating the current branch in a non-bare repository\n\
is denied, because it will make the index and work tree inconsistent\n\
with what you pushed, and will require 'git reset --hard' to match\n\
the work tree to HEAD.\n\
\n\
You can set the 'receive.denyCurrentBranch' configuration variable\n\
to 'ignore' or 'warn' in the remote repository to allow pushing into\n\
its current branch; however, this is not recommended unless you\n\
arranged to update its work tree to match what you pushed in some\n\
other way.\n\
\n\
To squelch this message and still keep the default behaviour, set\n\
'receive.denyCurrentBranch' configuration variable to 'refuse'.";

/// `receive.denyDeleteCurrent` left unset: the explanation before the
/// refusal.
pub const REFUSE_DELETE_CURRENT: &str = "By default, deleting the current branch is denied, because the next\n\
'git clone' won't result in any file checked out, causing confusion.\n\
\n\
You can set 'receive.denyDeleteCurrent' configuration variable to\n\
'warn' or 'ignore' in the remote repository to allow deleting the\n\
current branch, with or without a warning message.\n\
\n\
To squelch this message, you can set it to 'refuse'.";

pub const UPDATING_CURRENT: &str = "updating the current branch";
pub const DELETING_CURRENT: &str = "deleting the current branch";
pub const DELETING_NONEXISTENT: &str = "deleting a non-existent ref";
pub const DELETING_CORRUPT: &str = "allowing deletion of corrupt ref";

pub fn checked_out(name: &str) -> String {
    format!("refusing to update checked out branch: {name}")
}

pub fn delete_current(name: &str) -> String {
    format!("refusing to delete the current branch: {name}")
}

pub fn funny_ref(name: &str) -> String {
    format!("refusing to update funny ref '{name}' remotely")
}

pub fn inconsistent_aliased(
    symref: &str,
    sold: &str,
    snew: &str,
    target: &str,
    told: &str,
    tnew: &str,
) -> String {
    format!(
        "refusing inconsistent update between symref '{symref}' ({sold}..{snew}) and its target '{target}' ({told}..{tnew})"
    )
}

pub fn deny_deletion(name: &str) -> String {
    format!("denying ref deletion for {name}")
}

pub fn deny_non_ff(name: &str) -> String {
    format!("denying non-fast-forward {name} (you should pull first)")
}

pub fn hook_declined(name: &str) -> String {
    format!("hook declined to update {name}")
}

pub fn should_have_generated(oid: &str) -> String {
    format!("unpack should have generated {oid}, but I can't find it!")
}

pub fn multiple_updates(name: &str) -> String {
    format!("multiple updates for ref '{name}' not allowed")
}

pub fn unsupported_object_format(name: &str) -> String {
    format!("error: unsupported object format '{name}'")
}

pub fn expected_old_new_ref(line: &str) -> String {
    format!("protocol error: expected old/new/ref, got '{line}'")
}

pub fn expected_shallow(line: &str) -> String {
    format!("protocol error: expected shallow sha, got '{line}'")
}

pub fn not_a_repository(dir: &str) -> String {
    format!("'{dir}' does not appear to be a git repository")
}

pub const MUST_SPECIFY_DIRECTORY: &str = "you must specify a directory";
pub const USAGE: &str = "git receive-pack <git-dir>";
pub const HELP_QUIET: &str = "quiet";
pub const HELP_ADVERTISE_REFS: &str = "alias of --http-backend-info-refs";

// Pack header.
pub const EOF_BEFORE_HEADER: &str = "eof before pack header was fully read";
pub const SIGNATURE_MISMATCH: &str = "protocol error (pack signature mismatch detected)";

pub fn version_unsupported(v: u32) -> String {
    format!("protocol error (pack version {v} unsupported)")
}

pub fn abnormal_exit(program: &str) -> String {
    format!("{program} abnormal exit")
}

// proc-receive.
pub const PROC_RECEIVE_NOT_FOUND: &str = "cannot find hook \"proc-receive\"";
pub const PROC_RECEIVE_NO_VERSION: &str = "fail to negotiate version with proc-receive hook";
pub const PROC_RECEIVE_OPTION_WITHOUT_OK: &str =
    "proc-receive reported \"option\" without a matching \"ok/ng\" directive";

pub fn proc_receive_bad_version(v: &str) -> String {
    format!("proc-receive version \"{v}\" is not supported")
}

pub fn proc_receive_unknown_ref(name: &str) -> String {
    format!("proc-receive reported status on unknown ref: {name}")
}

pub fn proc_receive_unexpected_ref(name: &str) -> String {
    format!("proc-receive reported status on unexpected ref: {name}")
}

pub fn proc_receive_incomplete(line: &str) -> String {
    format!("proc-receive reported incomplete status line: \"{line}\"")
}

pub fn proc_receive_bad_status(status: &str, name: &str) -> String {
    format!("proc-receive reported bad status \"{status}\" on ref \"{name}\"")
}

/// The capabilities every advertisement starts with (wire protocol, not a
/// message; kept here with the other fixed strings).
pub const BASE_CAPABILITIES: &str =
    "report-status report-status-v2 delete-refs side-band-64k quiet";
pub const CANNOT_CREATE_QUARANTINE: &str = "unable to create temporary object directory";
pub const CANNOT_MIGRATE: &str = "unable to migrate objects to permanent storage";
