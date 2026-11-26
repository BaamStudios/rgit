// SPDX-License-Identifier: MIT
//! Messages of `git imap-send`.

pub const USAGE: &[&str] = &[
    "git imap-send [-v] [-q] [--[no-]curl] [--[no-]draft] [(--folder|-f) <folder>] < <mbox>",
    "git imap-send --list",
];

pub const HELP_VERBOSE: &str = "be more verbose";
pub const HELP_QUIET: &str = "be more quiet";
pub const HELP_CURL: &str = "use libcurl to communicate with the IMAP server";
pub const HELP_DRAFT: &str = "mark uploaded messages with the IMAP \\Draft flag";
pub const HELP_FOLDER: &str = "specify the IMAP folder";
pub const HELP_LIST: &str = "list all folders on the IMAP server";

pub const NO_HOST: &str = "error: no IMAP host specified\n\
hint: set the IMAP host with 'git config imap.host <host>'.\n\
hint: (e.g., 'git config imap.host imaps://imap.example.com')";

pub const NO_FOLDER: &str = "error: no IMAP folder specified\n\
hint: set the target folder with 'git config imap.folder <folder>'.\n\
hint: (e.g., 'git config imap.folder Drafts')";

pub const NOTHING_TO_SEND: &str = "nothing to send";

pub fn starting_tunnel(cmd: &str) -> String {
    format!("Starting tunnel '{cmd}'... ")
}

pub const LOGGING_IN: &str = "Logging in...";
pub const CLEARTEXT: &str = "*** IMAP Warning *** Password is being sent in the clear";
pub const FETCHING_LIST: &str = "Fetching the list of available folders...";

pub fn sending(n: usize, folder: &str) -> String {
    let s = if n == 1 { "" } else { "s" };
    format!("Sending {n} message{s} to {folder} folder...")
}

pub fn connect_failed(host: &str, err: &str) -> String {
    format!("error: unable to connect to {host}: {err}")
}

pub fn tunnel_failed(cmd: &str, err: &str) -> String {
    format!("error: cannot start proxy {cmd}: {err}")
}

pub fn imap_error(err: &str) -> String {
    format!("error: IMAP error: {err}")
}

pub const LOGIN_DISABLED: &str = "error: IMAP server does not allow LOGIN (LOGINDISABLED)";

pub fn unsupported_auth(method: &str) -> String {
    format!("error: authentication method {method} is not supported")
}

pub fn login_failed(user: &str, host: &str) -> String {
    format!("error: IMAP login for {user}@{host} failed")
}

pub fn bad_host(host: &str) -> String {
    format!("error: invalid IMAP host: {host}")
}
