// SPDX-License-Identifier: MIT
//! `git daemon` (Documentation/git-daemon.adoc): a server for the `git://`
//! transport.
//!
//! As ADR 0004 says, the daemon only accepts connections; every
//! connection is served by a child process (`rgit daemon --serve`, with
//! the connection as its standard input and output), which reads the
//! request, finds the repository, asks the access hook and then runs
//! `rgit upload-pack --strict --timeout=<n> .`, `rgit receive-pack .` or
//! `rgit upload-archive .` in the repository. With `--inetd` the process
//! itself is that child.
//!
//! - `daemon/listen.rs`: sockets, the accept loop, the children.
//! - `daemon/serve.rs`: one connection.
//! - `daemon/log.rs`: the log (`--log-destination`, `--verbose`).
//! - `daemon/sys.rs`: system calls outside the standard library.
//! - `rgit_server::daemon`: the rules for requests and paths, and the
//!   messages.

use crate::prelude::*;

#[cfg(unix)]
#[path = "daemon/listen.rs"]
mod listen;
#[cfg(unix)]
#[path = "daemon/log.rs"]
mod log;
#[cfg(unix)]
#[path = "daemon/serve.rs"]
mod serve;
#[cfg(unix)]
#[path = "daemon/sys.rs"]
mod sys;

#[cfg(unix)]
pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    imp::run(ctx, argv)
}

#[cfg(not(unix))]
pub fn run(_ctx: &mut Context, _argv: &[String]) -> CmdResult {
    Err(Die::Fatal(
        "rgit daemon is not supported on this platform".to_owned(),
    ))
}

#[cfg(unix)]
mod imp {
    use std::process::{Command, Stdio};

    use rgit_server::daemon::{Service, Services, messages as msg};

    use super::log::{Destination, Logger};
    use super::{listen, serve, sys};
    use crate::prelude::*;

    /// The port of the `git://` transport.
    const DEFAULT_PORT: u16 = 9418;

    /// Set for the process that `--detach` starts in the background.
    const DETACHED: &str = "RGIT_DAEMON_DETACHED";

    /// The command line.
    #[derive(Debug, Default)]
    pub struct Options {
        pub verbose: bool,
        pub log_destination: Option<Destination>,
        pub export_all: bool,
        pub strict_paths: bool,
        pub base_path: Option<String>,
        pub base_path_relaxed: bool,
        /// `--user-path[=<path>]`; empty for the plain option.
        pub user_path: Option<String>,
        pub interpolated_path: Option<String>,
        pub reuseaddr: bool,
        pub detach: bool,
        pub pid_file: Option<String>,
        pub services: Services,
        pub access_hook: Option<String>,
        pub informative_errors: bool,
        pub inetd: bool,
        /// `--serve`: this process serves the connection on its standard
        /// input and output (set by the listening daemon).
        pub serve: bool,
        pub listen: Vec<String>,
        pub port: Option<u16>,
        pub user: Option<String>,
        pub group: Option<String>,
        pub timeout: u32,
        pub init_timeout: u32,
        /// 0: no limit.
        pub max_connections: u32,
        pub directories: Vec<String>,
    }

    fn usage() -> Die {
        Die::Output(rgit_options::Exit {
            code: rgit_options::EXIT_USAGE,
            stdout: String::new(),
            stderr: format!("usage: {}\n", msg::USAGE),
        })
    }

    fn timeout(option: &str, value: &str) -> Result<u32, Die> {
        value
            .parse::<u32>()
            .map_err(|_| Die::Fatal(msg::invalid_timeout(option, value)))
    }

    fn service(name: &str) -> Result<Service, Die> {
        Service::from_name(name).ok_or_else(|| Die::Fatal(msg::no_such_service(name)))
    }

    fn parse(args: &[String]) -> Result<Options, Die> {
        let mut o = Options {
            max_connections: 32,
            ..Options::default()
        };
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            let (name, value) = match arg.split_once('=') {
                Some((n, v)) if n.starts_with("--") => (n, Some(v)),
                _ => (arg.as_str(), None),
            };
            match (name, value) {
                ("--listen", Some(v)) => o.listen.push(v.to_ascii_lowercase()),
                ("--port", Some(v)) => match v.parse::<u16>() {
                    Ok(0) => o.port = None,
                    Ok(n) => o.port = Some(n),
                    Err(_) => return Err(usage()),
                },
                ("--serve", None) => o.serve = true,
                ("--inetd", None) => o.inetd = true,
                ("--verbose", None) => o.verbose = true,
                ("--syslog", None) => o.log_destination = Some(Destination::Syslog),
                ("--log-destination", Some(v)) => {
                    o.log_destination = Some(match v {
                        "stderr" => Destination::Stderr,
                        "syslog" => Destination::Syslog,
                        "none" => Destination::None,
                        _ => return Err(Die::Fatal(msg::unknown_log_destination(v))),
                    });
                }
                ("--export-all", None) => o.export_all = true,
                ("--timeout", Some(v)) => o.timeout = timeout("timeout", v)?,
                ("--init-timeout", Some(v)) => o.init_timeout = timeout("init-timeout", v)?,
                ("--max-connections", Some(v)) => {
                    let n = v
                        .parse::<i64>()
                        .map_err(|_| Die::Fatal(msg::invalid_max_connections(v)))?;
                    // Nothing or less means no limit.
                    o.max_connections = u32::try_from(n.max(0)).unwrap_or(u32::MAX);
                }
                ("--strict-paths", None) => o.strict_paths = true,
                ("--base-path", Some(v)) => o.base_path = Some(v.to_owned()),
                ("--base-path-relaxed", None) => o.base_path_relaxed = true,
                ("--interpolated-path", Some(v)) => o.interpolated_path = Some(v.to_owned()),
                ("--reuseaddr", None) => o.reuseaddr = true,
                ("--user-path", v) => o.user_path = Some(v.unwrap_or("").to_owned()),
                ("--pid-file", Some(v)) => o.pid_file = Some(v.to_owned()),
                ("--detach", None) => o.detach = true,
                ("--user", Some(v)) => o.user = Some(v.to_owned()),
                ("--group", Some(v)) => o.group = Some(v.to_owned()),
                ("--enable", Some(v)) => o.services.set_enabled(service(v)?, true),
                ("--disable", Some(v)) => o.services.set_enabled(service(v)?, false),
                ("--allow-override", Some(v)) => o.services.set_overridable(service(v)?, true),
                ("--forbid-override", Some(v)) => o.services.set_overridable(service(v)?, false),
                ("--informative-errors", None) => o.informative_errors = true,
                ("--no-informative-errors", None) => o.informative_errors = false,
                ("--access-hook", Some(v)) => o.access_hook = Some(v.to_owned()),
                ("--", None) => {
                    o.directories = it.cloned().collect();
                    break;
                }
                _ if !arg.starts_with('-') => {
                    // The first directory ends the options.
                    o.directories = std::iter::once(arg.clone()).chain(it.cloned()).collect();
                    break;
                }
                _ => return Err(usage()),
            }
        }
        Ok(o)
    }

    pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
        let args = &argv[1..];
        let opts = parse(args)?;
        let background = opts.inetd || opts.detach;
        let log = Logger {
            destination: opts.log_destination.unwrap_or(if background {
                Destination::Syslog
            } else {
                Destination::Stderr
            }),
            verbose: opts.verbose,
        };

        if opts.inetd && (!opts.listen.is_empty() || opts.port.is_some()) {
            return Err(log.fatal(msg::INETD_LISTEN));
        }
        if opts.inetd && (opts.detach || opts.user.is_some() || opts.group.is_some()) {
            return Err(log.fatal(msg::INETD_DETACH));
        }
        if opts.group.is_some() && opts.user.is_none() {
            return Err(log.fatal(msg::GROUP_WITHOUT_USER));
        }
        let credentials = match &opts.user {
            Some(user) => match sys::credentials(user, opts.group.as_deref()) {
                Ok(c) => Some(c),
                Err(sys::LookupError::User) => return Err(log.fatal(msg::user_not_found(user))),
                Err(sys::LookupError::Group) => {
                    let group = opts.group.as_deref().unwrap_or_default();
                    return Err(log.fatal(msg::group_not_found(group)));
                }
            },
            None => None,
        };
        if opts.strict_paths && opts.directories.is_empty() {
            return Err(log.fatal(msg::STRICT_PATHS_NEEDS_DIRECTORIES));
        }
        if let Some(base) = &opts.base_path {
            if !std::path::Path::new(base).is_dir() {
                return Err(log.fatal(msg::base_path_missing(base)));
            }
        }

        if opts.serve {
            let remote = std::env::var("REMOTE_ADDR")
                .ok()
                .zip(std::env::var("REMOTE_PORT").ok());
            return serve::serve(ctx, &opts, &log, remote);
        }
        if opts.inetd {
            return serve::serve(ctx, &opts, &log, serve::peer_of_stdin());
        }

        if opts.detach && std::env::var_os(DETACHED).is_none() {
            return detach(args, &log);
        }
        let exe = std::env::current_exe().map_err(|e| log.fatal(e.to_string()))?;
        let port = opts.port.unwrap_or(DEFAULT_PORT);
        let listeners = listen::bind(&opts, &log, port)?;
        if let Some(file) = &opts.pid_file {
            std::fs::write(file, format!("{}\n", std::process::id())).map_err(|e| {
                log.fatal(msg::cannot_write_pid_file(file, &rgit_messages::strerror(&e)))
            })?;
        }
        if let Some(credentials) = &credentials {
            if !sys::drop_privileges(credentials) {
                return Err(log.fatal(msg::CANNOT_DROP_PRIVILEGES));
            }
        }
        log.info(msg::READY);
        let env = ctx.child_env();
        listen::accept(listeners, &opts, log, exe, args.to_vec(), env)
    }

    /// `--detach`: start the daemon again in the background, in a session
    /// of its own and away from the terminal, and leave.
    fn detach(args: &[String], log: &Logger) -> CmdResult {
        let exe = std::env::current_exe().map_err(|e| log.fatal(e.to_string()))?;
        let mut command = Command::new(exe);
        command
            .arg("daemon")
            .args(args)
            .env(DETACHED, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        sys::new_session(&mut command);
        command
            .spawn()
            .map_err(|e| log.fatal(msg::cannot_detach(&rgit_messages::strerror(&e))))?;
        Ok(0)
    }
}
