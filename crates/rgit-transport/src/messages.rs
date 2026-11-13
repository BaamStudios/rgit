// SPDX-License-Identifier: MIT
//! User-visible messages of the transport layer, reproduced verbatim from
//! git's output (observed with git 2.56) because scripts and tests compare
//! them. Messages are without the `fatal: ` / `error: ` / `warning: `
//! prefix unless noted.

/// `fatal:` for a host name that would be taken as an ssh option.
pub fn strange_hostname(host: &str) -> String {
    format!("strange hostname '{host}' blocked")
}

/// `fatal:` for a scp-style path that would be taken as an option.
pub fn strange_pathname(path: &str) -> String {
    format!("strange pathname '{path}' blocked")
}

/// `fatal:` for an `ssh://` URL without path.
pub const NO_PATH: &str = "no path specified; see 'git help pull' for valid url syntax";

/// `fatal:` when the ssh variant cannot express an option.
pub fn variant_unsupported(variant: &str, what: &str) -> String {
    format!("ssh variant '{variant}' does not support {what}")
}

/// What [`variant_unsupported`] names for a port.
pub const SETTING_PORT: &str = "setting port";

/// `fatal:` (printed by the child in git) when a program cannot be started.
pub fn cannot_exec(program: &str, reason: &str) -> String {
    format!("cannot exec '{program}': {reason}")
}

/// `error:` when a program cannot be started.
pub fn cannot_run(program: &str, reason: &str) -> String {
    format!("cannot run {program}: {reason}")
}

/// `fatal:` after a failed start of the ssh or service program.
pub const UNABLE_TO_FORK: &str = "unable to fork";

// ---- credentials ----------------------------------------------------------

/// `usage:` line of `git credential`.
pub const CREDENTIAL_USAGE: &str = "git credential (fill|approve|reject)";

/// `warning:` for a line without `=`.
pub fn invalid_credential_line(line: &str) -> String {
    format!("invalid credential line: {line}")
}

/// `fatal:` when standard input of `git credential` cannot be parsed.
pub const UNABLE_TO_READ_CREDENTIAL: &str = "unable to read credential from stdin";

/// `warning:` for a credential URL without `<scheme>://`.
pub fn url_has_no_scheme(url: &str) -> String {
    format!("url has no scheme: {url}")
}

/// `warning:` for a decoded URL component containing a newline.
pub fn url_newline(component: &str, url: &str) -> String {
    format!("url contains a newline in its {component} component: {url}")
}

/// `fatal:` for a `url=` value that cannot be parsed.
pub fn url_cannot_be_parsed(url: &str) -> String {
    format!("credential url cannot be parsed: {url}")
}

/// `warning:` for a `credential.<url>.*` key whose URL cannot be parsed.
pub fn skipping_credential_lookup(key: &str) -> String {
    format!("skipping credential lookup for key: {key}")
}

/// `fatal:` when a helper answered `quit=true`.
pub fn helper_quit(helper: &str) -> String {
    format!("credential helper '{helper}' told us to quit")
}

/// `fatal:` for a credential without `host`.
pub const MISSING_HOST: &str = "refusing to work with credential missing host field";

/// `fatal:` for a credential without `protocol`.
pub const MISSING_PROTOCOL: &str = "refusing to work with credential missing protocol field";

/// `fatal:` for a value with a newline.
pub fn value_contains_newline(key: &str) -> String {
    format!("credential value for {key} contains newline")
}

/// `fatal:` (two lines) for a value with a carriage return.
pub fn value_contains_cr(key: &str) -> String {
    format!(
        "credential value for {key} contains carriage return\n\
         If this is intended, set `credential.protectProtocol=false`"
    )
}

/// `fatal:` when `credential.interactive` forbids asking.
pub const UNABLE_TO_GET_PASSWORD: &str = "unable to get password from user";

/// `error:` when the askpass program fails.
pub fn askpass_failed(program: &str) -> String {
    format!("unable to read askpass response from '{program}'")
}

/// `fatal:` when prompting fails; `what` is e.g. `Username for 'https://host'`.
pub fn could_not_read(what: &str, reason: &str) -> String {
    format!("could not read {what}: {reason}")
}

/// Reason of [`could_not_read`] with `GIT_TERMINAL_PROMPT=0`.
pub const TERMINAL_PROMPTS_DISABLED: &str = "terminal prompts disabled";

/// `fatal:` for an unparsable boolean environment variable.
pub fn bad_bool_env(value: &str, name: &str) -> String {
    format!("bad boolean environment value '{value}' for '{name}'")
}

/// `fatal:` for an unusable configuration entry from a file.
pub fn bad_config_variable(name: &str, file: &str, line: usize) -> String {
    format!("bad config variable '{name}' in file '{file}' at line {line}")
}

/// `fatal:` for an unusable configuration entry from the command line.
pub fn unable_to_parse_cmdline(name: &str) -> String {
    format!("unable to parse '{name}' from command-line config")
}

/// The prompt label for a user name: `Username for '<url>'`.
pub fn username_for(url: &str) -> String {
    format!("Username for '{url}'")
}

/// The prompt label for a password: `Password for '<url>'`.
pub fn password_for(url: &str) -> String {
    format!("Password for '{url}'")
}

// ---- git:// ---------------------------------------------------------------

/// `fatal:` when the host name of a `git://` URL cannot be resolved.
pub fn unable_to_look_up(host: &str, port: u16) -> String {
    format!("unable to look up {host} (port {port}) (Name or service not known)")
}

/// One failed connection attempt in [`unable_to_connect`]:
/// `<host>[<n>: <address>]: errno=<reason>` and a newline.
pub fn connect_attempt_failed(host: &str, n: usize, addr: &str, reason: &str) -> String {
    format!("{host}[{n}: {addr}]: errno={reason}\n")
}

/// `fatal:` when no address of a `git://` host accepts the connection;
/// `attempts` are the lines of [`connect_attempt_failed`].
pub fn unable_to_connect(host: &str, attempts: &str) -> String {
    format!("unable to connect to {host}:\n{attempts}")
}
