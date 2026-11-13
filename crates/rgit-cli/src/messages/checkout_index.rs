// SPDX-License-Identifier: MIT
//! Messages, usage and option help of `git checkout-index` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here). The
//! per-path messages live in `rgit_unpack::messages`.

pub const USAGE: &[&str] = &["git checkout-index [<options>] [--] [<file>...]"];

pub const HELP_ALL: &str = "check out all files in the index";
pub const HELP_IGNORE_SKIP: &str = "do not skip files with skip-worktree set";
pub const HELP_FORCE: &str = "force overwrite of existing files";
pub const HELP_QUIET: &str = "no warning for existing files and files not in index";
pub const HELP_NO_CREATE: &str = "don't checkout new files";
pub const HELP_CREATE: &str = "opposite of --no-create";
pub const HELP_INDEX: &str = "update stat information in the index file";
pub const HELP_Z: &str = "paths are separated with NUL character";
pub const HELP_STDIN: &str = "read list of paths from the standard input";
pub const HELP_TEMP: &str = "write the content to temporary files";
pub const HELP_PREFIX: &str = "when creating files, prepend <string>";
pub const HELP_STAGE: &str = "copy out the files from named stage";

pub const MIX_ALL: &str = "git checkout-index: don't mix '--all' and explicit filenames";
pub const MIX_STDIN: &str = "git checkout-index: don't mix '--stdin' and explicit filenames";

/// rgit's own: options that are not implemented yet.
pub const TEMP_UNSUPPORTED: &str =
    "rgit: checkout-index --temp and --stage are not implemented yet";
