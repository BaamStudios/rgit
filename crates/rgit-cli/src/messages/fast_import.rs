// SPDX-License-Identifier: MIT
//! Messages, usage lines and option help of `git fast-import` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &["git fast-import [<options>]"];

pub const GROUP_COMMON: &str = "Common";
pub const GROUP_MARKS: &str = "Marks";
pub const GROUP_SUBMODULE: &str = "Submodule rewrite";
pub const GROUP_SIGNING: &str = "Signing";

pub const HELP_DATE_FORMAT: &str = "format of the commit/tag dates";
pub const HELP_STATS: &str = "display some basic statistics (objects, packfiles and memory)";
pub const HELP_QUIET: &str = "disable the output shown by --stats";
pub const HELP_FORCE: &str = "force updating modified existing branches";
pub const HELP_DONE: &str = "require a terminating 'done' command";
pub const HELP_MAX_PACK_SIZE: &str = "maximum size of each output pack file";
pub const HELP_BIG_FILE: &str = "maximum size of a blob that will be deltified";
pub const HELP_DEPTH: &str = "maximum delta depth";
pub const HELP_ACTIVE_BRANCHES: &str = "maximum number of branches to maintain active";
pub const HELP_IMPORT_MARKS: &str = "import marks from <file>";
pub const HELP_IMPORT_MARKS_IF_EXISTS: &str = "import marks from <file> if it exists";
pub const HELP_EXPORT_MARKS: &str = "dump marks to <file>";
pub const HELP_RELATIVE_MARKS: &str =
    "are --(import|export)-marks= paths relative to '.git/info/fast-import'?";
pub const HELP_REWRITE_FROM: &str = "rewrite object IDs for submodule <name> from <filename>";
pub const HELP_REWRITE_TO: &str = "rewrite object IDs for submodule <name> to <filename>";
pub const HELP_SIGNED_COMMITS: &str = "how to handle signed commits";
pub const HELP_SIGNED_TAGS: &str = "how to handle signed tags";
pub const HELP_UNSAFE: &str = "allow unsafe features in the stream";
pub const HELP_CAT_BLOB_FD: &str = "write cat-blob, ls and get-mark responses to <fd>";
pub const HELP_PACK_EDGES: &str = "list the last commit of each branch per pack in <file>";

pub const STREAM_ENDS_EARLY: &str = "stream ends early";
pub const EXPECTED_COMMITTER: &str = "expected committer but didn't get one";
pub const TAG_EMPTY_BRANCH: &str = "can't tag an empty branch.";
pub const NOTE_EMPTY_BRANCH: &str = "can't add a note on empty branch.";
pub const ROOT_NOT_DIR: &str = "root cannot be a non-directory";
pub const ONE_IMPORT_MARKS: &str = "only one import-marks command allowed per stream";
pub const REWRITE_FORMAT: &str = "expected format name:filename for submodule rewrite option";
pub const MULTIPLE_SHA1_SIGS: &str =
    "multiple SHA-1 signatures found, ignoring additional signature";
pub const MULTIPLE_SHA256_SIGS: &str =
    "multiple SHA-256 signatures found, ignoring additional signature";

pub fn crash_report(path: &str) -> String {
    format!("fast-import: dumping crash report to {path}")
}
pub fn unsupported_command(line: &str) -> String {
    format!("unsupported command: {line}")
}
pub fn expected_data(line: &str) -> String {
    format!("expected 'data n' command, found: {line}")
}
pub fn expected_from(line: &str) -> String {
    format!("expected 'from' command, got '{line}'")
}
pub fn expected_mark(line: &str) -> String {
    format!("expected 'mark' command, got {line}")
}
pub fn expected_to(line: &str) -> String {
    format!("expected 'to' command, got {line}")
}
pub fn eof_in_data_count(left: u64) -> String {
    format!("EOF in data ({left} bytes remaining)")
}
pub fn eof_in_data_term(term: &str) -> String {
    format!("EOF in data (terminator '{term}' not found)")
}
pub fn missing_lt(s: &str) -> String {
    format!("missing < in ident string: {s}")
}
pub fn missing_space_before_lt(s: &str) -> String {
    format!("missing space before < in ident string: {s}")
}
pub fn missing_gt(s: &str) -> String {
    format!("missing > in ident string: {s}")
}
pub fn missing_space_after_gt(s: &str) -> String {
    format!("missing space after > in ident string: {s}")
}
pub fn invalid_raw_date(date: &str, s: &str) -> String {
    format!("invalid raw date \"{date}\" in ident: {s}")
}
pub fn invalid_rfc2822_date(date: &str, s: &str) -> String {
    format!("invalid rfc2822 date \"{date}\" in ident: {s}")
}
pub fn date_must_be_now(s: &str) -> String {
    format!("date in ident must be 'now': {s}")
}
pub fn bad_branch_name(name: &str) -> String {
    format!("branch name doesn't conform to Git standards: {name}")
}
pub fn corrupt_mode(line: &str) -> String {
    format!("corrupt mode: {line}")
}
pub fn invalid_path_quoted(path: &str) -> String {
    format!("invalid path '{path}'")
}
pub fn invalid_field(field: &str, line: &str) -> String {
    format!("invalid {field}: {line}")
}
pub fn nul_in_field(field: &str, line: &str) -> String {
    format!("NUL in {field}: {line}")
}
pub fn garbage_after_field(field: &str, line: &str) -> String {
    format!("garbage after {field}: {line}")
}
pub fn missing_space_after_field(field: &str, line: &str) -> String {
    format!("missing space after {field}: {line}")
}
pub fn blob_not_found(line: &str) -> String {
    format!("blob not found: {line}")
}
pub fn mark_not_declared(mark: u64) -> String {
    format!("mark :{mark} not declared")
}
pub fn missing_space_after_mark(line: &str) -> String {
    format!("missing space after mark: {line}")
}
pub fn garbage_after_mark(line: &str) -> String {
    format!("garbage after mark: {line}")
}
pub fn invalid_dataref(line: &str) -> String {
    format!("invalid dataref: {line}")
}
pub fn missing_space_after_sha1(line: &str) -> String {
    format!("missing space after SHA1: {line}")
}
pub fn missing_space_after_tree_ish(line: &str) -> String {
    format!("missing space after tree-ish: {line}")
}
pub fn invalid_ref_or_sha1(name: &str) -> String {
    format!("invalid ref name or SHA1 expression: {name}")
}
pub fn branch_from_itself(name: &str) -> String {
    format!("can't create a branch from itself: {name}")
}
pub fn not_a_valid_commit(oid: &str) -> String {
    format!("not a valid commit: {oid}")
}
pub fn mark_not_a_commit(mark: u64) -> String {
    format!("mark :{mark} not a commit")
}
pub fn path_not_in_branch(path: &str) -> String {
    format!("path {path} not in branch")
}
pub fn unsupported_feature(f: &str) -> String {
    format!("this version of fast-import does not support feature {f}.")
}
pub fn feature_after_data(f: &str) -> String {
    format!("got feature command '{f}' after data command")
}
pub fn unsupported_option(o: &str) -> String {
    format!("this version of fast-import does not support option: {o}")
}
pub fn option_after_data(o: &str) -> String {
    format!("got option command '{o}' after data command")
}
pub fn not_non_negative(opt: &str) -> String {
    format!("--{opt}: argument must be a non-negative integer")
}
pub fn option_forbidden(o: &str) -> String {
    format!("option '{o}' forbidden in input without --allow-unsafe-features")
}
pub fn feature_forbidden(f: &str) -> String {
    format!("feature '{f}' forbidden in input without --allow-unsafe-features")
}
pub fn not_in_commit(line: &str) -> String {
    format!("not in a commit: {line}")
}
pub fn object_not_found(oid: &str) -> String {
    format!("object not found: {oid}")
}
pub fn not_a_mark(s: &str) -> String {
    format!("not a mark: {s}")
}
pub fn gitlink_inline(line: &str) -> String {
    format!("Git links cannot be specified 'inline': {line}")
}
pub fn dir_inline(line: &str) -> String {
    format!("directories cannot be specified 'inline': {line}")
}
pub fn not_a_kind(want: &str, actual: &str, line: &str) -> String {
    format!("not a {want} (actually a {actual}): {line}")
}
pub fn cat_blob_not_blob(oid: &str, kind: &str) -> String {
    format!("object {oid} is a {kind} but a blob was expected.")
}
pub fn gpgsig_format(line: &str) -> String {
    format!("expected gpgsig format: 'gpgsig <hash-algo> <signature-format>', got '{line}'")
}
pub fn gpgsig_unknown_algo(algo: &str) -> String {
    format!("unknown git hash algorithm in gpgsig: '{algo}'")
}
pub fn unknown_date_format(f: &str) -> String {
    format!("unknown --date-format argument {f}")
}
pub fn cannot_read(path: &str, err: &str) -> String {
    format!("cannot read '{path}': {err}")
}
pub fn corrupt_mark_line(line: &str) -> String {
    format!("corrupt mark line: {line}")
}
pub fn not_updating(name: &str, new: &str, old: &str) -> String {
    format!("not updating {name} (new tip {new} does not contain {old})")
}
pub fn dropping_replace(name: &str) -> String {
    format!(
        "dropping {name} since it would point to itself (i.e. to {})",
        name.trim_start_matches("refs/replace/")
    )
}
pub fn max_pack_size_mb(n: u64) -> String {
    format!("max-pack-size is now in bytes, assuming --max-pack-size={n}m")
}
pub fn unknown_signed_mode(opt: &str, mode: &str) -> String {
    format!("unknown --{opt} mode '{mode}'")
}
pub fn unable_to_write(path: &str, err: &str) -> String {
    format!("unable to write file {path}: {err}")
}
pub fn unable_to_create(path: &str, err: &str) -> String {
    format!("unable to create '{path}': {err}")
}
pub fn signed_commit_abort(oid: &str) -> String {
    format!("encountered signed commit {oid}; use --signed-commits=<mode> to handle it")
}
pub fn signed_tag_abort(name: &str) -> String {
    format!("encountered signed tag; use --signed-tags=<mode> to handle it: {name}")
}
pub fn importing_signed_commit(oid: &str) -> String {
    format!("importing a commit signature verbatim for commit {oid}")
}
pub fn stripping_signed_commit(oid: &str) -> String {
    format!("stripping a commit signature for commit {oid}")
}
pub fn importing_signed_tag(name: &str) -> String {
    format!("importing a tag signature verbatim for tag '{name}'")
}
pub fn stripping_signed_tag(name: &str) -> String {
    format!("stripping a tag signature for tag '{name}'")
}
pub const ABORT_INVALID_SIGNATURE: &str = "aborting due to invalid signature";
pub const SIGNING_FAILED: &str = "failed to sign";
pub fn stripping_invalid_commit(signer: &str) -> String {
    format!("stripping invalid signature for commit\n  allegedly by {signer}")
}
pub fn replacing_invalid_commit(signer: &str) -> String {
    format!("replacing invalid signature for commit\n  allegedly by {signer}")
}
