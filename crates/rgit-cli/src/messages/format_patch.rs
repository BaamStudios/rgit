// SPDX-License-Identifier: MIT
//! Messages and option help of `git format-patch` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &str = "\
usage: git format-patch [<options>] [<since> | <revision-range>]

    -n, --[no-]numbered   use [PATCH n/m] even with a single patch
    -N, --no-numbered     use [PATCH] even with multiple patches
    -s, --[no-]signoff    add a Signed-off-by trailer
    --[no-]stdout         print patches to standard out
    --[no-]cover-letter   generate a cover letter
    --[no-]commit-list-format <format-spec>
                          format spec used for the commit list in the cover letter
    --[no-]numbered-files use simple number sequence for output file names
    --[no-]suffix <sfx>   use <sfx> instead of '.patch'
    --[no-]start-number <n>
                          start numbering patches at <n> instead of 1
    -v, --[no-]reroll-count <reroll-count>
                          mark the series as Nth re-roll
    --[no-]filename-max-length <n>
                          max length of output filename
    --[no-]rfc[=<rfc>]    add <rfc> (default 'RFC') before 'PATCH'
    --[no-]cover-from-description <cover-from-description-mode>
                          generate parts of a cover letter based on a branch's description
    --[no-]description-file <file>
                          use branch description from file
    --subject-prefix <prefix>
                          use [<prefix>] instead of [PATCH]
    -o, --output-directory <dir>
                          store resulting files in <dir>
    -k, --keep-subject    don't strip/add [PATCH]
    --no-binary           don't output binary diffs
    --binary              opposite of --no-binary
    --[no-]zero-commit    output all-zero hash in From header
    --[no-]ignore-if-in-upstream
                          don't include a patch matching a commit upstream
    -p, --no-stat         show patch format instead of default (patch + stat)

Messaging
    --[no-]add-header <header>
                          add email header
    --[no-]to <email>     add To: header
    --[no-]cc <email>     add Cc: header
    --[no-]from[=<ident>] set From address to <ident> (or committer ident if absent)
    --[no-]in-reply-to <message-id>
                          make first mail a reply to <message-id>
    --[no-]attach[=<boundary>]
                          attach the patch
    --inline[=<boundary>] inline the patch
    --[no-]thread[=<style>]
                          enable message threading, styles: shallow, deep
    --[no-]signature <signature>
                          add a signature
    --[no-]base <base-commit>
                          add prerequisite tree info to the patch series
    --[no-]signature-file <file>
                          add a signature from a file
    -q, --[no-]quiet      don't print the patch filenames
    --[no-]progress       show progress while generating patches
    --[no-]interdiff <rev>
                          show changes against <rev> in cover letter or single patch
    --[no-]range-diff <refspec>
                          show changes against <refspec> in cover letter or single patch
    --[no-]creation-factor <n>
                          percentage by which creation is weighted
    --[no-]force-in-body-from
                          show in-body From: even if identical to the e-mail header

";

/// The magic date of the `From <commit>` line.
pub const MAGIC_DATE: &str = "Mon Sep 17 00:00:00 2001";

pub const SUBJECT_HERE: &str = "*** SUBJECT HERE ***";
pub const BLURB_HERE: &str = "*** BLURB HERE ***";

/// After a non-boolean `format.noprefix`.
pub const NOPREFIX_HINT: &[&str] = &[
    "'format.noprefix' used to accept any value and treat that as 'true'.",
    "Now it only accepts boolean values, like what 'diff.noprefix' does.",
];

pub fn does_not_make_sense(option: &str) -> String {
    format!("{option} does not make sense")
}

pub fn cannot_be_used_together(a: &str, b: &str) -> String {
    format!("options '{a}' and '{b}' cannot be used together")
}

pub fn invalid_ident(ident: &str) -> String {
    format!("invalid ident line: {ident}")
}

pub fn invalid_cover_mode(mode: &str) -> String {
    format!("{mode}: invalid cover from description mode")
}

pub fn expects_integer(option: &str) -> String {
    format!("error: option `{option}' expects an integer value with an optional k/m/g suffix\n")
}

pub fn requires_value(option: &str) -> String {
    format!("error: option `{option}' requires a value\n")
}

pub fn requires_value_short(option: char) -> String {
    format!("error: switch `{option}' requires a value\n")
}

pub fn cannot_open_file(path: &str, reason: &str) -> String {
    format!("could not open '{path}': {reason}")
}

pub fn cannot_create_directory(path: &str) -> String {
    format!("could not create directory '{path}'")
}

pub fn cannot_read_signature(path: &str) -> String {
    format!("unable to read signature file '{path}'")
}

pub fn cannot_read_description(path: &str) -> String {
    format!("unable to read branch description file '{path}'")
}

pub fn not_supported(what: &str) -> String {
    format!("format-patch {what} is not supported by rgit yet")
}

/// The MIME headers of a patch whose text is in `charset`.
pub fn mime_headers(charset: &str) -> String {
    format!(
        "MIME-Version: 1.0\nContent-Type: text/plain; charset={charset}\nContent-Transfer-Encoding: 8bit\n"
    )
}
