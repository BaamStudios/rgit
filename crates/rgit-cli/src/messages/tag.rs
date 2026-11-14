// SPDX-License-Identifier: MIT
//! Messages, usage lines and option help of `git tag`, as printed by the
//! reference git (CLEANROOM.md: user-visible strings are reproduced
//! verbatim and collected here).

pub const USAGE: &[&str] = &[
    "git tag [-a | -s | -u <key-id>] [-f] [-m <msg> | -F <file>] [-e]
               [(--trailer <token>[(=|:)<value>])...]
               <tagname> [<commit> | <object>]",
    "git tag -d <tagname>...",
    "git tag [-n[<num>]] -l [--contains <commit>] [--no-contains <commit>]
               [--points-at <object>] [--column[=<options>] | --no-column]
               [--create-reflog] [--sort=<key>] [--format=<format>]
               [--merged <commit>] [--no-merged <commit>] [<pattern>...]",
    "git tag -v [--format=<format>] <tagname>...",
];

pub const HELP_LIST: &str = "list tag names";
pub const HELP_LINES: &str = "print <n> lines of each tag message";
pub const HELP_DELETE: &str = "delete tags";
pub const HELP_VERIFY: &str = "verify tags";
pub const GROUP_CREATE: &str = "Tag creation options";
pub const HELP_ANNOTATE: &str = "annotated tag, needs a message";
pub const HELP_MESSAGE: &str = "tag message";
pub const HELP_FILE: &str = "read message from file";
pub const HELP_TRAILER: &str = "add custom trailer(s)";
pub const HELP_EDIT: &str = "force edit of tag message";
pub const HELP_SIGN: &str = "annotated and GPG-signed tag";
pub const HELP_CLEANUP: &str = "how to strip spaces and #comments from message";
pub const HELP_LOCAL_USER: &str = "use another key to sign the tag";
pub const HELP_FORCE: &str = "replace the tag if exists";
pub const HELP_CREATE_REFLOG: &str = "create a reflog";
pub const GROUP_LIST: &str = "Tag listing options";
pub const HELP_COLUMN: &str = "show tag list in columns";
pub const HELP_CONTAINS: &str = "print only tags that contain the commit";
pub const HELP_NO_CONTAINS: &str = "print only tags that don't contain the commit";
pub const HELP_MERGED: &str = "print only tags that are merged";
pub const HELP_NO_MERGED: &str = "print only tags that are not merged";
pub const HELP_OMIT_EMPTY: &str = "do not output a newline after empty formatted refs";
pub const HELP_SORT: &str = "field name to sort on";
pub const HELP_POINTS_AT: &str = "print only tags of the object";
pub const HELP_FORMAT: &str = "format to use for the output";
pub const HELP_COLOR: &str = "respect format colors";
pub const HELP_IGNORE_CASE: &str = "sorting and filtering are case insensitive";

pub const NO_TAG_MESSAGE: &str = "no tag message?";
pub const TOO_MANY_ARGUMENTS: &str = "too many arguments";
pub const NO_SIGNATURE: &str = "no signature found";
pub const SUPPLY_MESSAGE: &str = "Please supply the message using either -m or -F option.";

/// The commented instructions of the editor template (`strip` cleanup).
pub fn template_strip(name: &str, comment: &str) -> String {
    format!(
        "\nWrite a message for tag:\n  {name}\nLines starting with '{comment}' will be ignored.\n"
    )
}

/// The commented instructions of the editor template (other cleanups).
pub fn template_keep(name: &str, comment: &str) -> String {
    format!(
        "\nWrite a message for tag:\n  {name}\nLines starting with '{comment}' will be kept; you may remove them yourself if you want to.\n"
    )
}

/// Advice `nestedTag`.
pub fn hint_nested_tag(name: &str, target: &str) -> String {
    format!(
        "You have created a nested tag. The object referred to by your new tag is\n\
         already a tag. If you meant to tag the object that it points to, use:\n\
         \n\
         \tgit tag -f {name} {target}^{{}}"
    )
}

pub fn failed_to_resolve(name: &str) -> String {
    format!("Failed to resolve '{name}' as a valid ref.")
}

pub fn already_exists(name: &str) -> String {
    format!("tag '{name}' already exists")
}

pub fn invalid_name(name: &str) -> String {
    format!("'{name}' is not a valid tag name.")
}

pub fn not_found(name: &str) -> String {
    format!("tag '{name}' not found.")
}

pub fn deleted(name: &str, abbrev: &str) -> String {
    format!("Deleted tag '{name}' (was {abbrev})")
}

pub fn updated(name: &str, abbrev: &str) -> String {
    format!("Updated tag '{name}' (was {abbrev})")
}

pub fn only_in_list_mode(opt: &str) -> String {
    format!("the '{opt}' option is only allowed in list mode")
}

pub fn incompatible(a: &str, b: &str) -> String {
    format!("options '{a}' and '{b}' cannot be used together")
}

pub fn invalid_cleanup(mode: &str) -> String {
    format!("Invalid cleanup mode {mode}")
}

pub fn could_not_read_file(path: &str, err: &str) -> String {
    format!("could not open or read '{path}': {err}")
}

pub fn malformed_object_name(name: &str) -> String {
    format!("malformed object name {name}")
}

pub fn malformed_object_name_quoted(name: &str) -> String {
    format!("malformed object name '{name}'")
}

pub fn not_a_commit(oid: &str, kind: &str) -> String {
    format!("object {oid} is a {kind}, not a commit")
}

pub fn no_such_commit(name: &str) -> String {
    format!("no such commit {name}")
}

pub fn must_point_to_commit(opt: &str) -> String {
    format!("option `{opt}' must point to a commit")
}

pub fn cannot_verify_non_tag(name: &str, kind: &str) -> String {
    format!("{name}: cannot verify a non-tag object of type {kind}.")
}

pub fn rejected_atom(atom: &str) -> String {
    format!("this command reject atom %({atom})")
}

pub fn missing_value(key: &str) -> String {
    format!("missing value for '{key}'")
}

/// Reflog message of a tag creation or update.
pub fn reflog_tagging(abbrev: &str, what: &str) -> String {
    format!("tag: tagging {abbrev} ({what})")
}

pub fn reflog_commit(subject: &str, date: &str) -> String {
    format!("{subject}, {date}")
}

pub const REFLOG_TREE: &str = "tree object";
pub const REFLOG_BLOB: &str = "blob object";
pub const REFLOG_TAG: &str = "other tag object";

/// rgit wording: signing and verifying signatures come with rgit #51.
pub const SIGNING_NOT_SUPPORTED: &str = "rgit: signed tags are not supported yet (rgit #51)";

/// rgit wording: `--column` is not implemented yet.
pub const COLUMN_NOT_SUPPORTED: &str = "rgit: tag --column is not supported yet";
