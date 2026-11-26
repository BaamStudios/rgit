// SPDX-License-Identifier: MIT
//! Usage and option help of `git rerere` (CLEANROOM.md: user-visible
//! strings are reproduced verbatim and collected here). The messages of
//! the rerere passes are in `rgit_rerere::messages`.

pub const USAGE: &[&str] =
    &["git rerere [clear | forget <pathspec>... | diff | status | remaining | gc]"];

pub const HELP_AUTOUPDATE: &str = "register clean resolutions in index";

pub const FORGET_WITHOUT_PATHS: &str = "'git rerere forget' without paths is deprecated";

pub const CORRUPT_MERGE_RR: &str = "corrupt MERGE_RR";
