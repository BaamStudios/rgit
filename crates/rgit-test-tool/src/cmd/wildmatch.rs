// SPDX-License-Identifier: MIT

//! `wildmatch` (t3070).
//!
//! Observed behaviour of the reference binary: `wildmatch <mode> <text>
//! <pattern>` prints nothing and exits 0 when `text` matches `pattern`,
//! 1 otherwise. The modes are `wildmatch` (`/` is a separator),
//! `iwildmatch` (the same, ignoring case), `pathmatch` (`*` crosses `/`)
//! and `ipathmatch`; an unknown mode exits 1. The reference crashes when
//! arguments are missing; this stand-in exits 1 instead.

use rgit_pathspec::wildmatch::{WildmatchFlags, wildmatch as matches};

pub fn wildmatch(args: &[String]) -> i32 {
    let [mode, text, pattern, ..] = args else {
        return 1;
    };
    let flags = match mode.as_str() {
        "wildmatch" => WildmatchFlags::PATHNAME,
        "iwildmatch" => WildmatchFlags::PATHNAME | WildmatchFlags::CASEFOLD,
        "pathmatch" => WildmatchFlags::NONE,
        "ipathmatch" => WildmatchFlags::CASEFOLD,
        _ => return 1,
    };
    i32::from(!matches(pattern.as_bytes(), text.as_bytes(), flags))
}
