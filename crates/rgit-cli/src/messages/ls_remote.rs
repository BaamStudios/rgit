// SPDX-License-Identifier: MIT
//! Messages of `git ls-remote`, as printed by the reference git.

pub const USAGE: &[&str] = &[
    "git ls-remote [--branches] [--tags] [--refs] [--upload-pack=<exec>]
                     [-q | --quiet] [--exit-code] [--get-url] [--sort=<key>]
                     [--symref] [<repository> [<patterns>...]]",
];

pub const HELP_QUIET: &str = "do not print remote URL";
pub const HELP_UPLOAD_PACK: &str = "path of git-upload-pack on the remote host";
pub const HELP_TAGS: &str = "limit to tags";
pub const HELP_BRANCHES: &str = "limit to branches";
pub const HELP_REFS: &str = "do not show peeled tags";
pub const HELP_GET_URL: &str = "take url.<base>.insteadOf into account";
pub const HELP_EXIT_CODE: &str = "exit with exit code 2 if no matching refs are found";
pub const HELP_SYMREF: &str = "show underlying ref in addition to the object pointed by it";
pub const HELP_SERVER_OPTION: &str = "option to transmit";
pub const NO_REMOTE: &str = "No remote configured to list refs from.";
pub const HELP_SORT: &str = "field name to sort on";

/// rgit-specific: `--sort` is not implemented yet.
pub fn unsupported_sort(key: &str) -> String {
    format!("rgit: ls-remote --sort={key} is not supported yet")
}

/// No transport handles the URL's scheme.
pub fn no_remote_helper(scheme: &str) -> String {
    format!("unable to find remote helper for '{scheme}'")
}
