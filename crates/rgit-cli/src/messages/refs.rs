// SPDX-License-Identifier: MIT
//! Messages of `git refs`, as printed by the reference git.

pub const USAGE: &[&str] = &[
    "git refs migrate --ref-format=<format> [--no-reflog] [--dry-run]",
    "git refs verify [--strict] [--verbose]",
    "git refs list [--count=<count>] [--shell|--perl|--python|--tcl]
                                [(--sort=<key>)...] [--format=<format>]
                                [--include-root-refs] [--points-at=<object>]
                                [--merged[=<object>]] [--no-merged[=<object>]]
                                [--contains[=<object>]] [--no-contains[=<object>]]
                                [(--exclude=<pattern>)...] [--start-after=<marker>]
                                [ --stdin | (<pattern>...)]",
    USAGE_EXISTS,
    "git refs optimize [--all] [--no-prune] [--auto] [--include <pattern>] [--exclude <pattern>]",
    USAGE_CREATE,
    USAGE_DELETE,
    USAGE_UPDATE,
    USAGE_RENAME,
];

pub const USAGE_LIST: &[&str] = &[
    "git refs list [--count=<count>] [--shell|--perl|--python|--tcl]
                                [(--sort=<key>)...] [--format=<format>]
                                [--include-root-refs] [--points-at=<object>]
                                [--merged[=<object>]] [--no-merged[=<object>]]
                                [--contains[=<object>]] [--no-contains[=<object>]]
                                [(--exclude=<pattern>)...] [--start-after=<marker>]
                                [ --stdin | (<pattern>...)]",
];
pub const USAGE_OPTIMIZE: &[&str] = &[
    "git refs optimize [--all] [--no-prune] [--auto] [--include <pattern>] [--exclude <pattern>]",
];
pub const USAGE_EXISTS: &str = "git refs exists <ref>";
pub const USAGE_CREATE: &str =
    "git refs create [--message=<reason>] [--no-deref] [--create-reflog] <ref> <new-value>";
pub const USAGE_DELETE: &str =
    "git refs delete [--message=<reason>] [--no-deref] <ref> [<old-value>]";
pub const USAGE_UPDATE: &str = "git refs update [--message=<reason>] [--no-deref] [--create-reflog] <ref> <new-value> [<old-value>]";
pub const USAGE_RENAME: &str = "git refs rename [--message=<reason>] <old-ref> <new-ref>";

pub const HELP_MESSAGE: &str = "reason of the update";
pub const HELP_NO_DEREF: &str = "update <refname> not the one it points to";
pub const HELP_DEREF: &str = "opposite of --no-deref";
pub const HELP_CREATE_REFLOG: &str = "create a reflog";

pub const NEED_SUBCOMMAND: &str = "need a subcommand";
pub const EXISTS_REQUIRES_REF: &str = "'git refs exists' requires a reference";
pub const CREATE_ARGS: &str = "create requires reference name and an object ID";
pub const UPDATE_ARGS: &str = "update requires reference name, new value and an optional old value";
pub const DELETE_ARGS: &str = "delete requires reference name and an optional old object ID";
pub const RENAME_ARGS: &str = "rename requires old and new reference name";
pub const EMPTY_MESSAGE: &str = "refusing to perform update with empty message";
pub const CREATE_NULL: &str = "cannot create reference with null new object ID";
pub const DELETE_NULL: &str = "cannot delete reference with null old object ID";

pub fn unknown_subcommand(s: &str) -> String {
    format!("unknown subcommand: `{s}'")
}

pub fn invalid_object_id(v: &str) -> String {
    format!("invalid object ID: '{v}'")
}

pub fn invalid_new_object_id(v: &str) -> String {
    format!("invalid new object ID: '{v}'")
}

pub fn invalid_old_object_id(v: &str) -> String {
    format!("invalid old object ID: '{v}'")
}

pub fn invalid_ref_format(name: &str) -> String {
    format!("invalid ref format: '{name}'")
}

pub fn does_not_exist(name: &str) -> String {
    format!("reference does not exist: '{name}'")
}

pub fn already_exists(name: &str) -> String {
    format!("reference already exists: '{name}'")
}

/// rgit wording: a subcommand that is not implemented yet.
pub fn not_supported(sub: &str) -> String {
    format!("rgit refs: '{sub}' is not supported yet")
}

pub fn rename_symref(name: &str) -> String {
    format!("refname {name} is a symbolic ref, renaming it is not supported")
}
