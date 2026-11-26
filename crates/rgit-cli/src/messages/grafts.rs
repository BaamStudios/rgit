// SPDX-License-Identifier: MIT
//! Messages about the legacy graft file (CLEANROOM.md: user-visible strings
//! are reproduced verbatim and collected here).

/// The deprecation hint shown when `info/grafts` is read
/// (`advice.graftFileDeprecated`).
pub const HINT_DEPRECATED: &str = "Support for <GIT_DIR>/info/grafts is deprecated\n\
and will be removed in a future Git version.\n\
\n\
Please use \"git replace --convert-graft-file\"\n\
to convert the grafts into replace refs.\n\
\n\
Turn this message off by running\n\
\"git config set advice.graftFileDeprecated false\"";

/// `bad graft data: <line>`.
pub fn bad_graft_data(line: &str) -> String {
    format!("bad graft data: {line}")
}
