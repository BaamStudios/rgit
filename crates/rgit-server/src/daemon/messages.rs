// SPDX-License-Identifier: MIT
//! Messages of `git daemon`, word for word as git 2.56 writes them
//! (observed from the binary): what a client is told in an `ERR` packet,
//! the log lines and the fatal errors.

// ------------------------------------------------------------- to the client

/// What every refusal says without `--informative-errors`.
pub const ACCESS_DENIED: &str = "access denied or repository not exported";
/// The requested path names no repository (or one outside the directory
/// list).
pub const NO_SUCH_REPOSITORY: &str = "no such repository";
/// The repository has no `git-daemon-export-ok` file.
pub const NOT_EXPORTED: &str = "repository not exported";
/// The service is switched off.
pub const SERVICE_NOT_ENABLED: &str = "service not enabled";
/// The access hook said no without giving a reason.
pub const SERVICE_REJECTED: &str = "service rejected";

/// The `ERR` packet (without `ERR `): the reason and the path the client
/// asked for.
pub fn client_error(reason: &str, dir: &str) -> String {
    format!("{reason}: {dir}")
}

// ------------------------------------------------------------------ log lines

/// The daemon listens.
pub const READY: &str = "Ready to rumble";

/// A client connected.
pub fn connection_from(addr: &str, port: &str) -> String {
    format!("Connection from {addr}:{port}")
}

/// A parameter of the request.
pub fn extended_attribute(name: &str, value: &str) -> String {
    format!("Extended attribute \"{name}\": {value}")
}

/// The request.
pub fn request(service: &str, dir: &str) -> String {
    format!("Request {service} for '{dir}'")
}

/// The first packet is no request.
pub fn protocol_error(line: &str) -> String {
    format!("Protocol error: '{line}'")
}

/// A path that could name a repository in more than one way.
pub fn aliased(dir: &str) -> String {
    format!("'{dir}': aliased")
}

/// `~user` without `--user-path`.
pub fn user_path_not_allowed(dir: &str) -> String {
    format!("'{dir}': User-path not allowed")
}

/// How `--user-path=<path>` rewrites a request.
pub fn user_path_rewrite(path: &str, dir: &str) -> String {
    let (name, rest) = match dir.find('/') {
        Some(at) => dir.split_at(at),
        None => (dir, ""),
    };
    format!(
        "userpath <{path}>, request <{dir}>, namlen {}, restlen {}, slash <{rest}>",
        name.len(),
        rest.len()
    )
}

/// What `--interpolated-path` made of a request.
pub fn interpolated_dir(path: &str) -> String {
    format!("Interpolated dir '{path}'")
}

/// No repository at `path`.
pub fn not_a_repository(path: &str) -> String {
    format!("'{path}' does not appear to be a git repository")
}

/// The repository is not below one of the directories given on the
/// command line.
pub fn not_in_directory_list(path: &str) -> String {
    format!("'{path}': not in directory list")
}

/// No `git-daemon-export-ok` file.
pub fn repository_not_exported(path: &str) -> String {
    format!("'{path}': repository not exported.")
}

/// A service that is off and that no repository may switch on.
pub fn service_not_enabled(service: &str) -> String {
    format!("'{service}': service not enabled.")
}

/// A service that is off for this repository.
pub fn service_not_enabled_for(service: &str, path: &str) -> String {
    format!("'{service}': service not enabled for '{path}'")
}

/// The access hook could not be run.
pub fn access_hook_failed(hook: &str) -> String {
    format!("daemon access hook '{hook}' failed to start")
}

/// A connection ended.
pub fn disconnected(child: u32, with_error: bool) -> String {
    if with_error {
        format!("[{child}] Disconnected (with error)")
    } else {
        format!("[{child}] Disconnected")
    }
}

/// `--max-connections` reached.
pub const TOO_MANY_CHILDREN: &str = "Too many children, dropping connection";

/// A listening address could not be resolved.
/// The reason the C library gives when the port it is asked for is not a
/// number it takes (`--port` beyond 2^31 - 1; `EAI_SERVICE`).
pub const SERVNAME_NOT_SUPPORTED: &str = "Servname not supported for ai_socktype";

/// The host name of the lookup without `--listen`.
pub const ANY_HOST: &str = "(null)";

pub fn getaddrinfo_failed(host: &str, reason: &str) -> String {
    format!("getaddrinfo() for {host} failed: {reason}")
}

/// A listening socket could not be bound.
pub fn could_not_bind(addr: &str, reason: &str) -> String {
    format!("Could not bind to {addr}: {reason}")
}

/// No socket for one `--listen` host.
pub fn no_sockets_for_host(host: &str, port: u32) -> String {
    format!("unable to allocate any listen sockets for host {host} on port {port}")
}

/// A service ended by a signal (printed as `error: ...`).
pub fn died_of_signal(program: &str, signal: i32) -> String {
    format!("{program} died of signal {signal}")
}

// --------------------------------------------------------------------- fatal

/// The usage text (after `usage: `).
pub const USAGE: &str = "git daemon [--verbose] [--syslog] [--export-all]
           [--timeout=<n>] [--init-timeout=<n>] [--max-connections=<n>]
           [--strict-paths] [--base-path=<path>] [--base-path-relaxed]
           [--user-path | --user-path=<path>]
           [--interpolated-path=<path>]
           [--reuseaddr] [--pid-file=<file>]
           [--(enable|disable|allow-override|forbid-override)=<service>]
           [--access-hook=<path>]
           [--inetd | [--listen=<host_or_ipaddr>] [--port=<n>]
                      [--detach] [--user=<user> [--group=<group>]]
           [--log-destination=(stderr|syslog|none)]
           [<directory>...]";

/// `--timeout`, `--init-timeout` with something that is no number ≥ 0.
pub fn invalid_timeout(option: &str, value: &str) -> String {
    format!("invalid {option} '{value}', expecting a non-negative integer")
}

/// `--max-connections` with something that is no number.
pub fn invalid_max_connections(value: &str) -> String {
    format!("invalid max-connections '{value}', expecting an integer")
}

/// `--log-destination` with an unknown value.
pub fn unknown_log_destination(value: &str) -> String {
    format!("unknown log destination '{value}'")
}

/// `--enable` and friends with an unknown service.
pub fn no_such_service(name: &str) -> String {
    format!("No such service {name}")
}

/// `--inetd` with `--listen` or `--port`.
pub const INETD_LISTEN: &str = "--listen= and --port= are incompatible with --inetd";
/// `--inetd` with `--detach`, `--user` or `--group`.
pub const INETD_DETACH: &str = "--detach, --user and --group are incompatible with --inetd";
/// `--group` alone.
pub const GROUP_WITHOUT_USER: &str = "--group supplied without --user";

/// `--user` names nobody.
pub fn user_not_found(name: &str) -> String {
    format!("user not found - {name}")
}

/// `--group` names nothing.
pub fn group_not_found(name: &str) -> String {
    format!("group not found - {name}")
}

/// Changing the user or group failed.
pub const CANNOT_DROP_PRIVILEGES: &str = "cannot drop privileges";

/// `--strict-paths` without directories.
pub const STRICT_PATHS_NEEDS_DIRECTORIES: &str =
    "option --strict-paths requires '<directory>' arguments";

/// `--base-path` is no directory.
pub fn base_path_missing(path: &str) -> String {
    format!("base-path '{path}' does not exist or is not a directory")
}

/// Nothing to listen on.
pub fn no_listen_sockets(port: u32) -> String {
    format!("unable to allocate any listen sockets on port {port}")
}

/// The parameters of the first packet are malformed.
pub const INVALID_REQUEST: &str = "Invalid request";

/// The connection ended before the first packet was complete.
pub const HUNG_UP: &str = "the remote end hung up unexpectedly";

/// A repository's `daemon.*` setting is no boolean.
pub fn bad_boolean(value: &str, key: &str) -> String {
    format!("bad boolean config value '{value}' for '{key}'")
}

/// rgit: the detached daemon could not be started.
pub fn cannot_detach(reason: &str) -> String {
    format!("cannot start the detached daemon: {reason}")
}

/// rgit: the pid file could not be written.
pub fn cannot_write_pid_file(path: &str, reason: &str) -> String {
    format!("cannot write pid file '{path}': {reason}")
}
