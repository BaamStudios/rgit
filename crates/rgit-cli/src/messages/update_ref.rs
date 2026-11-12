// SPDX-License-Identifier: MIT
//! Messages of `git update-ref`, as printed by the reference git.

pub const USAGE: &[&str] = &[
    "git update-ref [<options>] -d <refname> [<old-oid>]",
    "git update-ref [<options>]    <refname> <new-oid> [<old-oid>]",
    "git update-ref [<options>] --stdin [-z] [--batch-updates]",
];

pub const HELP_MESSAGE: &str = "reason of the update";
pub const HELP_DELETE: &str = "delete the reference";
pub const HELP_NO_DEREF: &str = "update <refname> not the one it points to";
pub const HELP_DEREF: &str = "opposite of --no-deref";
pub const HELP_Z: &str = "stdin has NUL-terminated arguments";
pub const HELP_STDIN: &str = "read updates from stdin";
pub const HELP_CREATE_REFLOG: &str = "create a reflog";
pub const HELP_BATCH: &str = "batch reference updates";

pub const EMPTY_MESSAGE: &str = "Refusing to perform update with empty message.";
pub const EMPTY_COMMAND: &str = "empty command in input";
pub const PREPARED_ONLY_CLOSE: &str = "prepared transactions can only be closed";
pub const CANNOT_RESTART: &str = "cannot restart ongoing transaction";
pub const TRANSACTION_CLOSED: &str = "transaction is closed";

/// rgit wording: `--batch-updates` is not implemented yet.
pub const BATCH_NOT_SUPPORTED: &str = "rgit update-ref: --batch-updates is not supported yet";

pub fn not_valid_sha1(arg: &str) -> String {
    format!("{arg}: not a valid SHA1")
}

pub fn not_valid_old_sha1(arg: &str) -> String {
    format!("{arg}: not a valid old SHA1")
}

pub fn update_ref_failed(name: &str, err: &str) -> String {
    format!("update_ref failed for ref '{name}': {err}")
}

pub fn whitespace_before_command(line: &str) -> String {
    format!("whitespace before command: {line}")
}

pub fn unknown_command(line: &str) -> String {
    format!("unknown command: {line}")
}

pub fn badly_quoted(arg: &str) -> String {
    format!("badly quoted argument: {arg}")
}

pub fn junk_after_quoted(arg: &str) -> String {
    format!("unexpected character after quoted argument: {arg}")
}

pub fn missing_ref(cmd: &str) -> String {
    format!("{cmd}: missing <ref>")
}

pub fn invalid_ref_format(cmd: &str, name: &str) -> String {
    format!("{cmd}: invalid ref format: {name}")
}

pub fn missing_field(cmd: &str, name: &str, field: &str) -> String {
    format!("{cmd} {name}: missing {field}")
}

pub fn missing_treat_zero(cmd: &str, name: &str, field: &str) -> String {
    format!("{cmd} {name}: missing {field}, treating as zero")
}

pub fn unexpected_eof(cmd: &str, name: &str, field: &str) -> String {
    format!("{cmd} {name}: unexpected end of input when reading {field}")
}

pub fn invalid_field(cmd: &str, name: &str, field: &str, value: &str) -> String {
    format!("{cmd} {name}: invalid {field}: {value}")
}

pub fn zero_field(cmd: &str, name: &str, field: &str) -> String {
    format!("{cmd} {name}: zero {field}")
}

pub fn extra_input(cmd: &str, name: &str, rest: &str) -> String {
    format!("{cmd} {name}: extra input: {rest}")
}

pub fn invalid_old_arg(cmd: &str, name: &str, arg: &str) -> String {
    format!("{cmd} {name}: invalid arg '{arg}' for old value")
}

pub fn invalid_oid(cmd: &str, name: &str, arg: &str) -> String {
    format!("{cmd} {name}: invalid oid: {arg}")
}

pub fn deref_mode(cmd: &str) -> String {
    format!("{cmd}: cannot operate with deref mode")
}

pub fn option_unknown(rest: &str) -> String {
    format!("option unknown: {rest}")
}

pub fn ok(cmd: &str) -> String {
    format!("{cmd}: ok")
}
