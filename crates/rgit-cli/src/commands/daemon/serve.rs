// SPDX-License-Identifier: MIT
//! One connection of `rgit daemon`: read the request, find the
//! repository, check that it may be served, ask the access hook, run the
//! service.
//!
//! The checks and their order follow git 2.56 (observed): a service that
//! is off for good, the path (aliases, `~user`, `--interpolated-path`,
//! `--base-path`, the repository itself, the directory list), the
//! `git-daemon-export-ok` file, the repository's `daemon.*` setting, the
//! access hook. A client is told `access denied or repository not
//! exported` unless `--informative-errors` is on; the log has the
//! reason.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::ToSocketAddrs;
use std::process::{Command, Stdio};

use rgit_server::daemon::{self, Host, Interpolation, Request, RequestError, Service, messages as msg};

use super::imp::Options;
use super::log::Logger;
use super::sys;
use crate::commands::upload_pack::{FoundRepo, find_repo};
use crate::prelude::*;
use crate::repo::{self, Mode};

/// The exit code of a refused request (git ends with -1).
const REFUSED: i32 = 255;

/// The address and port of the client when standard input is a socket
/// (`--inetd`), as `REMOTE_ADDR` and `REMOTE_PORT` carry them.
pub fn peer_of_stdin() -> Option<(String, String)> {
    let stdin = std::io::stdin();
    let peer = socket2::SockRef::from(&stdin).peer_addr().ok()?;
    let peer = peer.as_socket()?;
    Some((remote_addr(&peer.ip()), peer.port().to_string()))
}

/// An address the way `REMOTE_ADDR` has it: IPv6 in brackets.
pub fn remote_addr(ip: &std::net::IpAddr) -> String {
    match ip {
        std::net::IpAddr::V4(a) => a.to_string(),
        std::net::IpAddr::V6(a) => format!("[{a}]"),
    }
}

/// The payload of the first packet, read without taking more from the
/// connection than that packet.
fn first_packet() -> Result<Vec<u8>, String> {
    let mut stdin = sys::raw_stdin();
    let mut header = [0u8; 4];
    stdin
        .read_exact(&mut header)
        .map_err(|_| msg::HUNG_UP.to_owned())?;
    let length = rgit_protocol::pktline::parse_length(header).map_err(|e| e.to_string())?;
    // A flush packet (or another special one) carries nothing.
    let mut payload = vec![0u8; length.saturating_sub(4)];
    stdin
        .read_exact(&mut payload)
        .map_err(|_| msg::HUNG_UP.to_owned())?;
    Ok(payload)
}

/// The names a host lookup gives: the canonical name and the address.
fn lookup(host: &str) -> (String, String) {
    if host.is_empty() {
        return (String::new(), String::new());
    }
    let ip = (host, 0u16)
        .to_socket_addrs()
        .ok()
        .and_then(|mut addrs| addrs.next())
        .map(|a| a.ip().to_string());
    match ip {
        Some(ip) => (sys::canonical_name(host).unwrap_or_default(), ip),
        None => (String::new(), String::new()),
    }
}

/// One connection, with what is known about its client.
struct Connection<'a> {
    opts: &'a Options,
    log: &'a Logger,
    /// `REMOTE_ADDR` and `REMOTE_PORT`.
    remote: Option<(String, String)>,
    /// The environment for the global options (`-c` and the like).
    env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
}

pub fn serve(
    ctx: &mut Context,
    opts: &Options,
    log: &Logger,
    remote: Option<(String, String)>,
) -> CmdResult {
    if let Some((addr, port)) = &remote {
        log.info(&msg::connection_from(addr, port));
    }
    // The request should come at once.
    let wait = if opts.init_timeout > 0 {
        opts.init_timeout
    } else {
        opts.timeout
    };
    sys::alarm(wait);
    let packet = first_packet().map_err(|m| log.fatal(m))?;
    sys::alarm(0);

    let attributes = |host: &Option<String>, extra: &[String]| {
        if let Some(host) = host {
            log.info(&msg::extended_attribute("host", host));
        }
        if let Some(protocol) = daemon::join_extra(extra) {
            log.info(&msg::extended_attribute("protocol", &protocol));
        }
    };
    let request = match Request::parse(&packet) {
        Ok(request) => request,
        Err(RequestError::Invalid { host }) => {
            attributes(&host, &[]);
            return Err(log.fatal(msg::INVALID_REQUEST));
        }
        Err(RequestError::Protocol { line, host, extra }) => {
            attributes(&host, &extra);
            log.error(&msg::protocol_error(&line));
            return Err(Die::Exit(REFUSED));
        }
    };
    attributes(&request.host, &request.extra);
    log.info(&msg::request(request.service.name(), &request.dir));

    let connection = Connection {
        opts,
        log,
        remote,
        env: ctx.child_env(),
    };
    connection.run(ctx, &request)
}

impl Connection<'_> {
    /// Tell the client why it is not served; returns how to end.
    fn refuse(&self, reason: &str, dir: &str) -> Die {
        let reason = if self.opts.informative_errors {
            reason
        } else {
            msg::ACCESS_DENIED
        };
        let text = format!("ERR {}", msg::client_error(reason, dir));
        let mut out = std::io::stdout().lock();
        if let Ok(packet) = rgit_protocol::pktline::encode(text.as_bytes()) {
            let _ = out.write_all(&packet);
            let _ = out.flush();
        }
        Die::Exit(REFUSED)
    }

    /// The repository `dir` asks for, if it may be served.
    fn path_ok(&self, dir: &str, host: Option<&Host>) -> Option<FoundRepo> {
        let (opts, log) = (self.opts, self.log);
        if daemon::is_aliased(dir) {
            log.error(&msg::aliased(dir));
            return None;
        }
        let mut path = dir.to_owned();
        if dir.starts_with('~') {
            match opts.user_path.as_deref() {
                None => {
                    log.error(&msg::user_path_not_allowed(dir));
                    return None;
                }
                Some("") => {}
                Some(user_path) => {
                    log.info(&msg::user_path_rewrite(user_path, dir));
                    path = daemon::user_path(dir, user_path);
                }
            }
        } else if let (Some(template), Some(host)) = (&opts.interpolated_path, host) {
            let (canonical, ip) = if daemon::needs_lookup(template) {
                lookup(&host.name)
            } else {
                (String::new(), String::new())
            };
            path = daemon::interpolate(
                template,
                &Interpolation {
                    host: &host.name,
                    canonical_host: &canonical,
                    ip: &ip,
                    port: &host.port,
                    dir,
                },
            );
            log.info(&msg::interpolated_dir(&path));
        } else if let Some(base) = &opts.base_path {
            path = format!("{base}{dir}");
        }

        let mut found = find_repo(&path, opts.strict_paths);
        if found.is_none() && opts.base_path.is_some() && opts.base_path_relaxed {
            // Without the base path, as before it was introduced.
            path = dir.to_owned();
            found = find_repo(&path, opts.strict_paths);
        }
        let Some(found) = found else {
            log.error(&msg::not_a_repository(&path));
            return None;
        };
        // The path as it was found counts, like in git: links are not
        // followed for this check.
        let found_at = found.path.to_string_lossy();
        if !daemon::in_directory_list(&found_at, &opts.directories, opts.strict_paths) {
            log.error(&msg::not_in_directory_list(&found_at));
            return None;
        }
        Some(found)
    }

    fn run(&self, ctx: &mut Context, request: &Request) -> CmdResult {
        let (opts, log) = (self.opts, self.log);
        let service = request.service;
        let dir = request.dir.as_str();
        if !opts.services.enabled(service) && !opts.services.overridable(service) {
            log.error(&msg::service_not_enabled(service.name()));
            return Err(self.refuse(msg::SERVICE_NOT_ENABLED, dir));
        }
        let host = request.host.as_deref().map(Host::parse);
        let Some(found) = self.path_ok(dir, host.as_ref()) else {
            return Err(self.refuse(msg::NO_SUCH_REPOSITORY, dir));
        };
        let path = found.path.to_string_lossy().into_owned();
        if !opts.export_all && !found.git_dir.join("git-daemon-export-ok").exists() {
            log.error(&msg::repository_not_exported(&path));
            return Err(self.refuse(msg::NOT_EXPORTED, dir));
        }
        // Everything from here on happens inside the repository.
        if std::env::set_current_dir(&found.git_dir).is_err() {
            log.error(&msg::not_a_repository(&path));
            return Err(self.refuse(msg::NO_SUCH_REPOSITORY, dir));
        }

        let mut enabled = opts.services.enabled(service);
        if opts.services.overridable(service) {
            ctx.git_dir = Some(".".into());
            let setup = repo::setup(ctx, Mode::RequiredQuiet)?;
            let config = repo::load_config(ctx, &setup)?;
            match config.bool(service.config_key()) {
                Ok(Some(value)) => enabled = value,
                Ok(None) => {}
                Err(e) => return Err(log.fatal(e.to_string())),
            }
        }
        if !enabled {
            log.error(&msg::service_not_enabled_for(service.name(), &path));
            return Err(self.refuse(msg::SERVICE_NOT_ENABLED, dir));
        }

        if let Some(hook) = &opts.access_hook {
            if let Err(reason) = self.access_hook(hook, service, &path, host.as_ref()) {
                return Err(self.refuse(&reason, dir));
            }
        }
        self.service(service, request)
    }

    /// Run `--access-hook`. On refusal the error is what the client is
    /// told: the first line the hook printed, if any.
    fn access_hook(
        &self,
        hook: &str,
        service: Service,
        path: &str,
        host: Option<&Host>,
    ) -> Result<(), String> {
        let unknown = Host::default();
        let host = host.unwrap_or(&unknown);
        let (canonical, ip) = lookup(&host.name);
        let mut command = Command::new(hook);
        command
            .args([service.name(), path, &host.name, &canonical, &ip, &host.port])
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
        self.environment(&mut command);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                let text = rgit_messages::strerror(&e);
                eprintln!("fatal: cannot exec '{hook}': {text}");
                self.log.error(&msg::access_hook_failed(hook));
                return Err(msg::SERVICE_REJECTED.to_owned());
            }
        };
        let mut output = Vec::new();
        if let Some(mut stdout) = child.stdout.take() {
            let _ = stdout.read_to_end(&mut output);
        }
        let allowed = child.wait().is_ok_and(|status| status.success());
        if allowed {
            return Ok(());
        }
        let text = String::from_utf8_lossy(&output);
        let line = text.lines().next().unwrap_or("").trim_end();
        Err(if line.is_empty() {
            msg::SERVICE_REJECTED.to_owned()
        } else {
            line.to_owned()
        })
    }

    /// What the hook and the service learn about the client.
    fn environment(&self, command: &mut Command) {
        if let Some((addr, port)) = &self.remote {
            command.env("REMOTE_ADDR", addr).env("REMOTE_PORT", port);
        }
    }

    /// Run the service in the repository (the current directory) on our
    /// standard input and output. What it writes to standard error goes
    /// to the log.
    fn service(&self, service: Service, request: &Request) -> CmdResult {
        let log = self.log;
        let exe = std::env::current_exe().map_err(|e| log.fatal(e.to_string()))?;
        let mut command = Command::new(exe);
        command.arg(service.name());
        if service == Service::UploadPack {
            command
                .arg("--strict")
                .arg(format!("--timeout={}", self.opts.timeout));
        }
        command
            .arg(".")
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stderr(Stdio::piped());
        self.environment(&mut command);
        match request.protocol() {
            Some(protocol) => command.env("GIT_PROTOCOL", protocol),
            None => command.env_remove("GIT_PROTOCOL"),
        };
        let mut child = command
            .spawn()
            .map_err(|e| log.fatal(format!("cannot run {}: {e}", service.name())))?;
        if let Some(stderr) = child.stderr.take() {
            for line in BufReader::new(stderr).split(b'\n').map_while(Result::ok) {
                log.error(&String::from_utf8_lossy(&line));
            }
        }
        let status = child.wait().map_err(|e| log.fatal(e.to_string()))?;
        if let Some(code) = status.code() {
            return Ok(code);
        }
        let signal = std::os::unix::process::ExitStatusExt::signal(&status).unwrap_or(0);
        eprintln!("error: {}", msg::died_of_signal(service.name(), signal));
        Ok(128 + signal)
    }
}
