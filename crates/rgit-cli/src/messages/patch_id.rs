// SPDX-License-Identifier: MIT
//! Usage and option help of `git patch-id` (CLEANROOM.md: user-visible
//! strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &["git patch-id [--stable | --unstable | --verbatim]"];
pub const HELP_UNSTABLE: &str = "use the unstable patch ID algorithm";
pub const HELP_STABLE: &str = "use the stable patch ID algorithm";
pub const HELP_VERBATIM: &str = "don't strip whitespace from the patch";
