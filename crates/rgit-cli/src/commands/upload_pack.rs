// SPDX-License-Identifier: MIT
//! `git upload-pack` (Documentation/git-upload-pack.adoc): the server
//! side of fetch and clone, started by a transport (`file://`, SSH, `git
//! daemon`, `git http-backend`) as `git-upload-pack <directory>`.
//!
//! This file finds the repository and connects standard input and output
//! to the service in `rgit_server::upload_pack`, which speaks protocol
//! v0, v1 and v2 (`GIT_PROTOCOL` says which one the client asks for).
//! The pack is made by `rgit pack-objects`, started as a child process
//! (or by `uploadpack.packObjectsHook`).
//!
//! `<directory>` may be the repository's work tree or git directory; like
//! git, `<directory>/.git`, `<directory>`, `<directory>.git/.git` and
//! `<directory>.git` are tried in this order, and `~` and `~user` expand
//! to home directories. With `--strict` only `<directory>` itself counts.
//! A candidate may also be a `.git` file naming the git directory (a
//! linked work tree, a submodule).

use std::io::{BufWriter, Write};
use std::path::PathBuf;

use rgit_config::SystemEnv;
use rgit_protocol::ProtocolVersion;
use rgit_refs::files::FilesStore;
use rgit_repo::layout::{LayoutOverrides, is_git_directory, read_gitfile};
use rgit_server::upload_pack as service;

use crate::messages::upload_pack as msg;
use crate::prelude::*;
use crate::repo::{self, Mode};

/// The git directory `<directory>` names, if any.
pub(crate) fn enter_repo(dir: &str, strict: bool) -> Option<PathBuf> {
    find_repo(dir, strict).map(|found| found.git_dir)
}

/// A repository a server was asked for.
pub(crate) struct FoundRepo {
    /// The path at which it was found: the request, with `/.git` or
    /// `.git` added where that was needed. For a `.git` file this is the
    /// file, not where it points (`git daemon` checks this path against
    /// its directory list, as git does).
    #[cfg_attr(not(unix), allow(dead_code))]
    pub path: PathBuf,
    /// The git directory.
    pub git_dir: PathBuf,
}

/// Find the repository `<directory>` names (see the module
/// documentation).
pub(crate) fn find_repo(dir: &str, strict: bool) -> Option<FoundRepo> {
    let cwd = std::env::current_dir().ok()?;
    let overrides = LayoutOverrides::default();
    // The git directory at `p`: `p` itself, or what the `.git` file `p`
    // points to.
    let repository = |p: PathBuf| -> Option<FoundRepo> {
        let git_dir = if p.is_file() {
            read_gitfile(&p, &cwd, &overrides).ok()?
        } else if is_git_directory(&p, &cwd, &overrides) {
            p.clone()
        } else {
            return None;
        };
        Some(FoundRepo { path: p, git_dir })
    };
    if strict {
        return repository(PathBuf::from(dir));
    }
    let base = if dir.starts_with('~') {
        let env = SystemEnv::default();
        rgit_config::value::expand_user_path(dir.as_bytes(), &env)
            .ok()?
            .to_string_lossy()
            .into_owned()
    } else {
        dir.to_owned()
    };
    // A trailing slash would hide `<directory>.git`.
    let trimmed = match base.trim_end_matches('/') {
        "" => "/",
        t => t,
    };
    ["/.git", "", ".git/.git", ".git"]
        .iter()
        .map(|suffix| PathBuf::from(format!("{trimmed}{suffix}")))
        .find_map(repository)
}

/// The protocol version a client asks for in `GIT_PROTOCOL`
/// (`version=<n>` among colon-separated entries; the highest one known).
fn requested_protocol() -> ProtocolVersion {
    let value = std::env::var("GIT_PROTOCOL").unwrap_or_default();
    let mut version = ProtocolVersion::V0;
    for entry in value.split(':') {
        match entry.strip_prefix("version=") {
            Some("2") => version = ProtocolVersion::V2,
            Some("1") if version == ProtocolVersion::V0 => version = ProtocolVersion::V1,
            _ => {}
        }
    }
    version
}

/// `--timeout=<n>`: end the process after `seconds` without activity.
#[cfg(unix)]
fn restart_timer(seconds: u32) {
    // SAFETY: alarm(2) only replaces the process's pending alarm; the
    // default action of SIGALRM ends the process, as git's timeout does.
    unsafe {
        libc::alarm(seconds);
    }
}

#[cfg(not(unix))]
fn restart_timer(_seconds: u32) {}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let stateless_rpc = Cell::new(false);
    let advertise_refs = Cell::new(false);
    let strict = Cell::new(false);
    let timeout = Cell::new(0);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag(None, "stateless-rpc", &stateless_rpc, msg::H_STATELESS_RPC),
            Opt::flag(
                None,
                "http-backend-info-refs",
                &advertise_refs,
                msg::H_HTTP_BACKEND_INFO_REFS,
            )
            .hidden(),
            Opt::alias(None, "advertise-refs", "http-backend-info-refs"),
            Opt::flag(None, "strict", &strict, msg::H_STRICT),
            Opt::integer(None, "timeout", &timeout, msg::H_TIMEOUT),
        ],
    );
    let args = parser.parse(argv)?.args;
    if args.len() != 1 {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let protocol = requested_protocol();
    let dir = &args[0];
    let Some(git_dir) = enter_repo(dir, strict.get()) else {
        return Err(Die::Fatal(msg::not_a_repository(dir)));
    };
    // Like git: work inside the git directory, with `GIT_DIR=.`.
    std::env::set_current_dir(&git_dir)
        .map_err(|_| Die::Fatal(msg::not_a_repository(dir)))?;
    ctx.git_dir = Some(".".into());

    let setup = repo::setup(ctx, Mode::RequiredQuiet)?;
    let hash = setup.format.object_format;
    let config = repo::load_config(ctx, &setup)?;
    let odb = repo::odb_with_config(&setup, &config)?;
    let refs = FilesStore::new(&setup.common_dir, hash).with_worktree(&setup.git_dir);
    let shallow = rgit_fetch::shallow::read(
        &rgit_fetch::shallow::file_for(&setup.common_dir),
        hash,
    );
    let repository = service::Repository {
        hash,
        odb: &odb,
        refs: &refs,
        config: &config,
        shallow,
    };

    let exe = std::env::current_exe().map_err(|e| Die::Fatal(e.to_string()))?;
    let mut opts = service::Options::new(vec![exe.into_os_string()]);
    opts.protocol = protocol;
    opts.stateless_rpc = stateless_rpc.get();
    opts.advertise_refs = advertise_refs.get();
    opts.pack_objects.env = ctx.child_env();
    // A child must not fetch missing objects from a promisor remote of
    // the served repository (git-upload-pack(1), GIT_NO_LAZY_FETCH).
    if std::env::var_os("GIT_NO_LAZY_FETCH").is_none() {
        opts.pack_objects
            .env
            .push(("GIT_NO_LAZY_FETCH".into(), "1".into()));
    }
    opts.namespace = ctx
        .namespace
        .clone()
        .or_else(|| std::env::var("GIT_NAMESPACE").ok())
        .filter(|n| !n.is_empty());
    opts.sideband_all = std::env::var("GIT_TEST_SIDEBAND_ALL")
        .ok()
        .and_then(|v| rgit_config::value::parse_bool(Some(v.as_bytes())));
    let seconds = timeout.get().max(0) as u32;
    if seconds > 0 {
        restart_timer(seconds);
        opts.on_activity = Some(Box::new(move || restart_timer(seconds)));
    }

    let stdin = std::io::stdin().lock();
    let mut stdout = BufWriter::with_capacity(1 << 16, std::io::stdout().lock());
    let served = service::upload_pack(&repository, &opts, stdin, &mut stdout);
    let flushed = stdout.flush();
    match served {
        Ok(()) => {
            flushed.map_err(|e| Die::Fatal(e.to_string()))?;
            Ok(0)
        }
        Err(service::Error::Refused(m)) => Err(Die::Error(m, 128)),
        Err(e) => Err(Die::Fatal(e.to_string())),
    }
}
