// SPDX-License-Identifier: MIT
//! Messages, usage lines and option help of `git fast-export` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &["git fast-export [<options>] [<revision-range>] [[--] <path>...]"];

pub const HELP_PROGRESS: &str = "show progress after <n> objects";
pub const HELP_SIGNED_TAGS: &str = "select handling of signed tags";
pub const HELP_SIGNED_COMMITS: &str = "select handling of signed commits";
pub const HELP_TAG_OF_FILTERED: &str = "select handling of tags that tag filtered objects";
pub const HELP_REENCODE: &str = "select handling of commit messages in an alternate encoding";
pub const HELP_EXPORT_MARKS: &str = "dump marks to this file";
pub const HELP_IMPORT_MARKS: &str = "import marks from this file";
pub const HELP_IMPORT_MARKS_IF_EXISTS: &str = "import marks from this file if it exists";
pub const HELP_FAKE_TAGGER: &str = "fake a tagger when tags lack one";
pub const HELP_FULL_TREE: &str = "output full tree for each commit";
pub const HELP_DONE: &str = "use the done feature to terminate the stream";
pub const HELP_NO_DATA: &str = "skip output of blob data";
pub const HELP_DATA: &str = "opposite of --no-data";
pub const HELP_REFSPEC: &str = "apply refspec to exported refs";
pub const HELP_ANONYMIZE: &str = "anonymize output";
pub const HELP_ANONYMIZE_MAP: &str = "convert <from> to <to> in anonymized output";
pub const HELP_REFERENCE_EXCLUDED: &str =
    "reference parents which are not in fast-export stream by object id";
pub const HELP_ORIGINAL_IDS: &str = "show original object ids of blobs/commits";
pub const HELP_MARK_TAGS: &str = "label tags with mark ids";

pub const EMPTY_MAP_TOKEN: &str = "--anonymize-map token cannot be empty";
pub const MAP_NEEDS_ANONYMIZE: &str = "the option '--anonymize-map' requires '--anonymize'";
pub const FAKE_TAGGER: &str = "Unspecified Tagger <unspecified-tagger> 0 +0000";

pub fn unknown_signed_tags_mode(m: &str) -> String {
    format!("unknown signed-tags mode: {m}")
}
pub fn unknown_signed_commits_mode(m: &str) -> String {
    format!("unknown signed-commits mode: {m}")
}
pub fn unknown_reencode_mode(m: &str) -> String {
    format!("unknown reencoding mode: {m}")
}
pub fn unknown_filtered_mode(m: &str) -> String {
    format!("unknown tag-of-filtered mode: {m}")
}
pub fn signed_tag_abort(oid: &str) -> String {
    format!("encountered signed tag {oid}; use --signed-tags=<mode> to handle it")
}
pub fn exporting_signed_tag(oid: &str) -> String {
    format!("exporting signed tag {oid}")
}
pub fn stripping_tag_signature(oid: &str) -> String {
    format!("stripping signature from tag {oid}")
}
pub fn signed_commit_abort(oid: &str) -> String {
    format!("encountered signed commit {oid}; use --signed-commits=<mode> to handle it")
}
pub fn exporting_signed_commit(oid: &str) -> String {
    format!("exporting {} signature(s) for commit {oid}", 1)
}
pub fn stripping_commit_signature(oid: &str) -> String {
    format!("stripping signature(s) from commit {oid}")
}
pub fn encoding_abort(enc: &str, oid: &str) -> String {
    format!(
        "encountered commit-specific encoding {enc} in commit {oid}; use --reencode=[yes|no] to handle it"
    )
}
pub fn tags_unexported(oid: &str) -> String {
    format!("tag {oid} tags unexported object; use --tag-of-filtered-object=<mode> to handle it")
}
pub fn no_author(oid: &str) -> String {
    format!("could not find author in commit {oid}")
}
pub fn no_committer(oid: &str) -> String {
    format!("could not find committer in commit {oid}")
}
pub fn unexpected_type(name: &str, kind: &str) -> String {
    format!("{name}: unexpected object of type {kind}, skipping.")
}
pub fn tag_unexpected_type(kind: &str) -> String {
    format!("tag points to object of unexpected type {kind}, skipping.")
}
pub fn omitting_tree_tag(oid: &str) -> String {
    format!(
        "omitting tag {oid},\nsince tags of trees (or tags of tags of trees, etc.) are not supported."
    )
}
pub fn could_not_read(oid: &str) -> String {
    format!("could not read blob {oid}")
}
pub fn not_a_mark(line: &str) -> String {
    format!("corrupt mark line: {line}")
}
pub fn cannot_read(path: &str, err: &str) -> String {
    format!("cannot read '{path}': {err}")
}
pub fn unable_to_write(path: &str, err: &str) -> String {
    format!("unable to write marks file {path}: {err}")
}
pub fn invalid_refspec(spec: &str) -> String {
    format!("invalid refspec '{spec}'")
}
