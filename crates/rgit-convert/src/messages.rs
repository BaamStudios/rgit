// SPDX-License-Identifier: MIT

//! User-visible messages of this crate, verbatim as git prints them (see
//! CLEANROOM.md, "User-visible messages"), without the `warning: `,
//! `fatal: `, `error: ` or `hint: ` prefix.

/// `core.safecrlf=warn`: CR LF in the file would become LF.
pub fn crlf_to_lf_warning(path: &str) -> String {
    format!(
        "in the working copy of '{path}', CRLF will be replaced by LF the next time Git touches it"
    )
}

/// `core.safecrlf=warn`: LF in the file would become CR LF.
pub fn lf_to_crlf_warning(path: &str) -> String {
    format!(
        "in the working copy of '{path}', LF will be replaced by CRLF the next time Git touches it"
    )
}

/// `core.safecrlf=true`: CR LF would become LF (fatal).
pub fn crlf_to_lf_fatal(path: &str) -> String {
    format!("CRLF would be replaced by LF in {path}")
}

/// `core.safecrlf=true`: LF would become CR LF (fatal).
pub fn lf_to_crlf_fatal(path: &str) -> String {
    format!("LF would be replaced by CRLF in {path}")
}

/// `working-tree-encoding` set without a value (fatal).
pub const ENCODING_NOT_BOOL: &str = "true/false are no valid working-tree-encodings";

/// Conversion between `from` and `to` failed.
pub fn failed_to_encode(path: &str, from: &str, to: &str) -> String {
    format!("failed to encode '{path}' from {from} to {to}")
}

/// A round trip through UTF-8 did not give the original content back.
pub fn roundtrip_failed(path: &str, enc: &str) -> String {
    format!("encoding '{path}' from {enc} to UTF-8 and back is not the same")
}

/// A BOM in a file whose encoding names the byte order.
pub fn bom_prohibited(path: &str, enc: &str) -> String {
    format!("BOM is prohibited in '{path}' if encoded as {enc}")
}

/// The hint before [`bom_prohibited`]; `bits` is `16` or `32`.
pub fn bom_prohibited_hint(path: &str, bits: &str) -> String {
    format!(
        "The file '{path}' contains a byte order mark (BOM). Please use UTF-{bits} as working-tree-encoding."
    )
}

/// No BOM in a file whose encoding does not name the byte order.
pub fn bom_required(path: &str, enc: &str) -> String {
    format!("BOM is required in '{path}' if encoded as {enc}")
}

/// The hint before [`bom_required`]; `bits` is `16` or `32`.
pub fn bom_required_hint(path: &str, bits: &str) -> String {
    format!(
        "The file '{path}' is missing a byte order mark (BOM). Please use UTF-{bits}BE or UTF-{bits}LE (depending on the byte order) as working-tree-encoding."
    )
}

/// `GIT_TRACE`: a round trip check is made.
pub fn checking_roundtrip(enc: &str) -> String {
    format!("Checking roundtrip encoding for {enc}...")
}

/// A filter program given with a path could not be started.
pub fn cannot_exec(program: &str, reason: &str) -> String {
    format!("cannot exec '{program}': {reason}")
}

/// A filter program looked up in `PATH` could not be started.
pub fn cannot_run(program: &str, reason: &str) -> String {
    format!("cannot run {program}: {reason}")
}

/// A single-file filter could not be started.
pub fn cannot_fork_filter(cmd: &str) -> String {
    format!("cannot fork to run external filter '{cmd}'")
}

/// A single-file or `process` filter failed.
pub fn filter_failed(cmd: &str) -> String {
    format!("external filter '{cmd}' failed")
}

/// A single-file filter exited with a non-zero status.
pub fn filter_failed_code(cmd: &str, code: i32) -> String {
    format!("external filter '{cmd}' failed {code}")
}

/// A `process` filter could not be started.
pub fn cannot_fork_subprocess(cmd: &str) -> String {
    format!("cannot fork to run subprocess '{cmd}'")
}

/// The handshake with a `process` filter failed.
pub fn subprocess_init_failed(cmd: &str) -> String {
    format!("initialization for subprocess '{cmd}' failed")
}

/// A `process` filter ended before its greeting.
pub fn could_not_read_greeting(cmd: &str) -> String {
    format!("could not read greeting from subprocess '{cmd}'")
}

/// A `process` filter sent something else in the handshake.
pub fn unexpected_line(got: &str, want: &str) -> String {
    format!("Unexpected line '{got}', expected {want}")
}

/// A filter process was ended by a signal.
pub fn died_of_signal(cmd: &str, sig: i32) -> String {
    format!("{cmd} died of signal {sig}")
}

/// A required clean filter failed (fatal).
pub fn clean_filter_failed(path: &str, driver: &str) -> String {
    format!("{path}: clean filter '{driver}' failed")
}

/// A required smudge filter failed (fatal).
pub fn smudge_filter_failed(path: &str, driver: &str) -> String {
    format!("{path}: smudge filter {driver} failed")
}

/// A delayed blob never came from its `process` filter.
pub fn not_filtered_properly(path: &str) -> String {
    format!("'{path}' was not filtered properly")
}

/// A `process` filter named a blob it had not delayed.
pub fn not_delayed_earlier(cmd: &str, path: &str) -> String {
    format!(
        "external filter '{cmd}' signaled that '{path}' is now available although it has not been delayed earlier"
    )
}
