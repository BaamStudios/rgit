// SPDX-License-Identifier: MIT
//! `git clone` (Documentation/git-clone.adoc, config/clone.adoc,
//! urls.adoc): create a repository from a remote one.
//!
//! The steps: work out the source and the target directory, create and
//! `init` the target, record the remote in the configuration, then talk
//! to the remote through `rgit-fetch` (any transport: a local path,
//! `file://`, ssh, `git://`, `http(s)://`): `ls-refs`, one `fetch` without
//! negotiation, the connectivity check. The references are written as one
//! initial `packed-refs` file, `HEAD` points at the remote's default
//! branch (or `--branch`), and the initial checkout is a one-way merge into
//! the empty index through `rgit-unpack`. Everything created is removed
//! again when a step before the checkout fails; a failed checkout leaves
//! the repository (git's "Clone succeeded, but checkout failed.").
//!
//! A local path (without `--no-local`) gets a copy of the source's object
//! store, hard-linked where possible ([`LocalCopy`]); the references still
//! come through the transport. `--revision` clones one revision without
//! refs or refspec; a detached remote `HEAD` is attached to the branch at
//! the same commit when there is one.

use std::collections::HashSet;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use bstr::ByteSlice;
use rgit_config::{ConfigSet, SystemEnv};
use rgit_fetch::{ConnectSettings, LocalRepo, PackOptions, ReceivedPack, Session, graph, refmap};
use rgit_hash::ObjectId;
use rgit_object::ObjectKind;
use rgit_protocol::v2::{LsRefsArgs, RemoteRef};
use rgit_refs::packed::{PackedRef, PackedRefs};
use rgit_refs::{RefUpdate, Transaction};
use rgit_remote::refspec::{CloneOptions, Direction, RefspecSet, clone_refspec};
use rgit_repo::init::{self as repo_init, InitOptions, Notice};
use rgit_repo::{Setup, SetupOptions, StandardProtectedConfig};
use rgit_transport::IpVersion;

use crate::housekeeping::store;
use crate::messages::clone as msg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, UpdateHooks};
use crate::repo::command_line;

/// Parsed command line.
#[derive(Default)]
struct Options {
    /// `--sparse`: a cone-mode sparse checkout of the top level files.
    sparse: bool,
    /// `--bundle-uri`.
    bundle_uri: Option<String>,
    quiet: bool,
    progress: Option<bool>,
    no_checkout: bool,
    bare: bool,
    mirror: bool,
    /// `-l`/`--local` (`Some(true)`), `--no-local` (`Some(false)`).
    local: Option<bool>,
    origin: Option<String>,
    branch: Option<String>,
    upload_pack: Option<String>,
    depth: Option<String>,
    /// `--shallow-since`.
    shallow_since: Option<String>,
    /// `--shallow-exclude`.
    shallow_exclude: Vec<String>,
    /// `--[no-]reject-shallow`.
    reject_shallow: Option<bool>,
    /// `--no-hardlinks`.
    no_hardlinks: bool,
    /// `--ref-format`.
    ref_format: Option<rgit_repo::format::RefStorage>,
    /// `--revision`.
    revision: Option<String>,
    /// `--separate-git-dir`.
    separate_git_dir: Option<String>,
    /// `-s` / `--shared`.
    shared: bool,
    /// `--reference`.
    reference: Vec<String>,
    /// `--reference-if-able`.
    reference_if_able: Vec<String>,
    /// `--dissociate`.
    dissociate: bool,
    /// `--single-branch` / `--no-single-branch`.
    single_branch: Option<bool>,
    tags: bool,
    template: Option<String>,
    config: Vec<String>,
    server_options: Vec<String>,
    ip: IpVersion,
    /// `-v` (connection steps of `git://`).
    verbose: bool,
    /// `--recurse-submodules` pathspecs (`.` for all).
    recurse: Vec<String>,
    shallow_submodules: bool,
    remote_submodules: bool,
    jobs: i32,
    /// `--filter`: the spec sent to the server and the one stored in
    /// `remote.<name>.partialCloneFilter`.
    filter: Option<(String, String)>,
    /// `--also-filter-submodules`.
    filter_submodules: bool,
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let verbose = Cell::new(0);
    let quiet = Cell::new(0);
    let progress = Cell::new(-1);
    let reject_shallow = Cell::new(-1);
    let no_checkout = Cell::new(false);
    let bare = Cell::new(false);
    let mirror = Cell::new(false);
    let local = Cell::new(-1);
    let no_hardlinks = Cell::new(false);
    let shared = Cell::new(false);
    let recurse = RefCell::new(Vec::new());
    let jobs = Cell::new(-1);
    let template = RefCell::new(None);
    let reference = RefCell::new(Vec::new());
    let reference_if_able = RefCell::new(Vec::new());
    let dissociate = Cell::new(false);
    let origin = RefCell::new(None);
    let branch = RefCell::new(None);
    let revision = RefCell::new(None);
    let upload_pack = RefCell::new(None);
    let depth = RefCell::new(None);
    let shallow_since = RefCell::new(None);
    let shallow_exclude = RefCell::new(Vec::new());
    let single_branch = Cell::new(-1);
    let tags = Cell::new(1);
    let shallow_submodules = Cell::new(false);
    let separate_git_dir = RefCell::new(None);
    let ref_format = RefCell::new(None);
    let config = RefCell::new(Vec::new());
    let server_options = RefCell::new(Vec::new());
    let ip = Cell::new(0);
    let filter: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let also_filter_submodules = Cell::new(false);
    let remote_submodules = Cell::new(false);
    let sparse = Cell::new(false);
    let bundle_uri = RefCell::new(None);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::verbose(&verbose, msg::HELP_VERBOSE),
            Opt::quiet(&quiet, msg::HELP_QUIET),
            Opt::bool(None, "progress", &progress, msg::HELP_PROGRESS),
            Opt::bool(None, "reject-shallow", &reject_shallow, msg::HELP_REJECT_SHALLOW),
            Opt::flag('n', "no-checkout", &no_checkout, msg::HELP_NO_CHECKOUT),
            Opt::flag(None, "bare", &bare, msg::HELP_BARE),
            Opt::flag(None, "mirror", &mirror, msg::HELP_MIRROR),
            Opt::bool('l', "local", &local, msg::HELP_LOCAL),
            Opt::flag(None, "no-hardlinks", &no_hardlinks, msg::HELP_NO_HARDLINKS),
            Opt::flag('s', "shared", &shared, msg::HELP_SHARED),
            Opt::callback(
                None,
                "recurse-submodules",
                Some("pathspec"),
                msg::HELP_RECURSE_SUBMODULES,
                |v, unset| {
                    if unset {
                        recurse.borrow_mut().clear();
                    } else {
                        recurse.borrow_mut().push(v.unwrap_or(".").to_owned());
                    }
                    Ok(())
                },
            )
            .optional(None),
            Opt::alias(None, "recursive", "recurse-submodules"),
            Opt::integer('j', "jobs", &jobs, msg::HELP_JOBS),
            Opt::string(None, "template", &template, "template-directory", msg::HELP_TEMPLATE),
            Opt::string_list(None, "reference", &reference, "repo", msg::HELP_REFERENCE),
            Opt::string_list(
                None,
                "reference-if-able",
                &reference_if_able,
                "repo",
                msg::HELP_REFERENCE,
            ),
            Opt::flag(None, "dissociate", &dissociate, msg::HELP_DISSOCIATE),
            Opt::string('o', "origin", &origin, "name", msg::HELP_ORIGIN),
            Opt::string('b', "branch", &branch, "branch", msg::HELP_BRANCH),
            Opt::string(None, "revision", &revision, "rev", msg::HELP_REVISION),
            Opt::string('u', "upload-pack", &upload_pack, "path", msg::HELP_UPLOAD_PACK),
            Opt::string(None, "depth", &depth, "depth", msg::HELP_DEPTH),
            Opt::string(None, "shallow-since", &shallow_since, "time", msg::HELP_SHALLOW_SINCE),
            Opt::string_list(
                None,
                "shallow-exclude",
                &shallow_exclude,
                "ref",
                msg::HELP_SHALLOW_EXCLUDE,
            ),
            Opt::bool(None, "single-branch", &single_branch, msg::HELP_SINGLE_BRANCH),
            Opt::bool(None, "tags", &tags, msg::HELP_TAGS),
            Opt::flag(
                None,
                "shallow-submodules",
                &shallow_submodules,
                msg::HELP_SHALLOW_SUBMODULES,
            ),
            Opt::string(
                None,
                "separate-git-dir",
                &separate_git_dir,
                "gitdir",
                msg::HELP_SEPARATE_GIT_DIR,
            ),
            Opt::string(None, "ref-format", &ref_format, "format", msg::HELP_REF_FORMAT),
            Opt::string_list('c', "config", &config, "key=value", msg::HELP_CONFIG),
            Opt::string_list(
                None,
                "server-option",
                &server_options,
                "server-specific",
                msg::HELP_SERVER_OPTION,
            ),
            Opt::set_int('4', "ipv4", &ip, msg::HELP_IPV4, 4).noneg(),
            Opt::set_int('6', "ipv6", &ip, msg::HELP_IPV6, 6).noneg(),
            Opt::callback(None, "filter", Some("args"), msg::HELP_FILTER, |v, unset| {
                match v {
                    Some(spec) if !unset => filter.borrow_mut().push(spec.to_owned()),
                    _ => filter.borrow_mut().clear(),
                }
                Ok(())
            }),
            Opt::flag(
                None,
                "also-filter-submodules",
                &also_filter_submodules,
                msg::HELP_ALSO_FILTER_SUBMODULES,
            ),
            Opt::flag(
                None,
                "remote-submodules",
                &remote_submodules,
                msg::HELP_REMOTE_SUBMODULES,
            ),
            Opt::flag(None, "sparse", &sparse, msg::HELP_SPARSE),
            Opt::string(None, "bundle-uri", &bundle_uri, "uri", msg::HELP_BUNDLE_URI),
        ],
    );
    let args = parser.parse(argv)?.args;
    if args.is_empty() {
        return Err(parser.usage_msg(msg::NO_REPOSITORY).into());
    }
    if args.len() > 2 {
        return Err(parser.usage_msg(msg::TOO_MANY_ARGUMENTS).into());
    }
    drop(parser);

    if also_filter_submodules.get() {
        if filter.borrow().is_empty() {
            return Err(Die::Fatal(msg::option_requires(
                "--also-filter-submodules",
                "--filter",
            )));
        }
        if recurse.borrow().is_empty() {
            return Err(Die::Fatal(msg::option_requires(
                "--also-filter-submodules",
                "--recurse-submodules",
            )));
        }
    }
    let filter = if crate::promisor::is_auto(&filter.borrow()) {
        Some((crate::promisor::AUTO.to_owned(), crate::promisor::AUTO.to_owned()))
    } else {
        crate::promisor::parse_filters(&filter.borrow())?.map(|(f, text)| (f.to_spec(), text))
    };
    let ref_format = match ref_format.into_inner() {
        Some(f) => Some(repo_init::parse_ref_format(&f).map_err(|e| init_failed(&e))?),
        None => None,
    };
    if bundle_uri.borrow().is_some()
        && (depth.borrow().is_some()
            || shallow_since.borrow().is_some()
            || !shallow_exclude.borrow().is_empty())
    {
        return Err(Die::Fatal(msg::incompatible(
            "--bundle-uri",
            "--depth/--shallow-since/--shallow-exclude",
        )));
    }
    let separate_git_dir = separate_git_dir.into_inner();
    if separate_git_dir.is_some() && (bare.get() || mirror.get()) {
        let first = if bare.get() { "--bare" } else { "--mirror" };
        return Err(Die::Fatal(msg::incompatible(first, "--separate-git-dir")));
    }

    let mut opts = Options {
        sparse: sparse.get(),
        revision: revision.into_inner(),
        ref_format,
        separate_git_dir,
        quiet: quiet.get() > 0,
        progress: (progress.get() >= 0).then(|| progress.get() > 0),
        no_checkout: no_checkout.get(),
        bare: bare.get() || mirror.get(),
        mirror: mirror.get(),
        local: (local.get() >= 0).then(|| local.get() > 0),
        origin: origin.into_inner(),
        branch: branch.into_inner(),
        upload_pack: upload_pack.into_inner(),
        depth: depth.into_inner(),
        shallow_since: shallow_since.into_inner(),
        shallow_exclude: shallow_exclude.into_inner(),
        reject_shallow: (reject_shallow.get() >= 0).then(|| reject_shallow.get() > 0),
        no_hardlinks: no_hardlinks.get(),
        shared: shared.get(),
        reference: reference.into_inner(),
        reference_if_able: reference_if_able.into_inner(),
        dissociate: dissociate.get(),
        single_branch: (single_branch.get() >= 0).then(|| single_branch.get() > 0),
        tags: tags.get() != 0,
        template: template.into_inner(),
        config: config.into_inner(),
        server_options: server_options.into_inner(),
        ip: match ip.get() {
            4 => IpVersion::V4,
            6 => IpVersion::V6,
            _ => IpVersion::Any,
        },
        verbose: verbose.get() > 0 && quiet.get() == 0,
        recurse: recurse.into_inner(),
        shallow_submodules: shallow_submodules.get(),
        remote_submodules: remote_submodules.get(),
        jobs: jobs.get(),
        filter,
        filter_submodules: also_filter_submodules.get(),
        bundle_uri: bundle_uri.into_inner(),
    };
    if opts.bare {
        opts.no_checkout = true;
    }
    clone(ctx, &opts, &args[0], args.get(1).map(String::as_str))
}

/// Where the objects come from.
struct Source {
    /// The URL recorded in `remote.<name>.url` and the reflog.
    url: String,
    /// The repository is a local directory.
    local_dir: bool,
    /// Where the transport connects: the URL, or the git directory a
    /// gitfile at the source points to.
    connect_url: String,
    /// The source is a bundle file.
    bundle: bool,
}

/// git's lookup of a local repository: `<repo>/.git`, `<repo>`,
/// `<repo>.git/.git`, `<repo>.git`; the first directory that is a git
/// repository wins. The recorded path is absolute, without a trailing
/// `/.git` that was added here.
fn local_repository(repo: &str, cwd: &Path) -> Option<(PathBuf, Option<PathBuf>)> {
    for (suffix, keep) in [("/.git", ""), ("", ""), (".git/.git", ".git"), (".git", ".git")] {
        let candidate = cwd.join(format!("{repo}{suffix}"));
        if is_git_directory(&candidate) || gitfile_repository(&candidate) {
            // A gitfile (`gitdir: <path>`), e.g. a submodule's work tree:
            // the transport connects to the git directory it names.
            let via_gitfile = candidate
                .is_file()
                .then(|| repository_git_dir(&candidate))
                .and_then(|g| std::fs::canonicalize(g).ok());
            // As given, made absolute (`.`, `..` and symbolic links stay).
            return Some((cwd.join(format!("{repo}{keep}")), via_gitfile));
        }
    }
    None
}

/// A directory that has `HEAD`, `objects/` and `refs/`; for a linked work
/// tree's git directory `objects/` and `refs/` are in its common
/// directory.
fn is_git_directory(dir: &Path) -> bool {
    let common = common_dir_of(dir);
    dir.is_dir()
        && dir.join("HEAD").is_file()
        && common.join("objects").is_dir()
        && common.join("refs").is_dir()
}

/// A gitfile (`gitdir: <path>`) pointing to a git directory.
fn gitfile_repository(file: &Path) -> bool {
    file.is_file() && is_git_directory(&repository_git_dir(file))
}

/// git's `transport_anonymize_url`: the password (and user) of a URL are
/// not recorded in messages.
fn anonymize(url: &str) -> String {
    let Some(i) = url.find("://") else {
        return url.to_owned();
    };
    let rest = &url[i + 3..];
    let host_end = rest.find('/').unwrap_or(rest.len());
    match rest[..host_end].rfind('@') {
        Some(at) => format!("{}{}", &url[..i + 3], &rest[at + 1..]),
        None => url.to_owned(),
    }
}

fn resolve_source(repo: &str, cwd: &Path, local: Option<bool>) -> Result<Source, Die> {
    if let Some((path, git_dir)) = local_repository(repo, cwd) {
        let url = path.to_string_lossy().into_owned();
        return Ok(Source {
            connect_url: git_dir.map_or_else(|| url.clone(), |g| g.to_string_lossy().into_owned()),
            url,
            local_dir: local != Some(false),
            bundle: false,
        });
    }
    // A bundle file: `<repo>`, else `<repo>.bundle`.
    for suffix in ["", ".bundle"] {
        let candidate = cwd.join(format!("{repo}{suffix}"));
        if rgit_fetch::bundle::is_bundle_file(&candidate) {
            let url = candidate.to_string_lossy().into_owned();
            return Ok(Source {
                connect_url: url.clone(),
                url,
                local_dir: false,
                bundle: true,
            });
        }
    }
    let bytes = repo.as_bytes();
    if rgit_remote::url::is_url(bytes) || !rgit_remote::url::is_local_path(bytes) {
        return Ok(Source {
            url: repo.to_owned(),
            connect_url: repo.to_owned(),
            local_dir: false,
            bundle: false,
        });
    }
    Err(Die::Fatal(msg::repository_does_not_exist(repo)))
}

/// What is removed when the clone fails.
struct Junk {
    work_tree: Option<PathBuf>,
    /// The work tree existed before (empty): remove its contents only.
    work_tree_existed: bool,
    git_dir: Option<PathBuf>,
    git_dir_existed: bool,
    armed: bool,
    /// The same directories, removed when a signal kills the clone.
    on_signal: Vec<rgit_fs::DirGuard>,
}

impl Junk {
    fn remove(path: &Path, existed: bool) {
        if existed {
            if let Ok(rd) = std::fs::read_dir(path) {
                for e in rd.flatten() {
                    let p = e.path();
                    if e.file_type().is_ok_and(|t| t.is_dir()) {
                        let _ = std::fs::remove_dir_all(&p);
                    } else {
                        let _ = std::fs::remove_file(&p);
                    }
                }
            }
        } else {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

impl Drop for Junk {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(g) = &self.git_dir {
            Junk::remove(g, self.git_dir_existed);
        }
        if let Some(w) = &self.work_tree {
            Junk::remove(w, self.work_tree_existed);
        }
    }
}

fn is_empty_dir(path: &Path) -> bool {
    std::fs::read_dir(path).is_ok_and(|mut rd| rd.next().is_none())
}

/// `mkdir -p` for the target directory; git's messages on failure.
fn create_dir(dir: &Path, shown: &str) -> Result<(), Die> {
    if let Some(parent) = dir.parent() {
        if !parent.as_os_str().is_empty() && std::fs::create_dir_all(parent).is_err() {
            return Err(Die::Fatal(msg::cannot_create_leading(shown)));
        }
    }
    match std::fs::create_dir(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
        Err(_) => Err(Die::Fatal(msg::cannot_create_work_tree(shown))),
    }
}

fn clone(ctx: &mut Context, opts: &Options, repo: &str, dir_arg: Option<&str>) -> CmdResult {
    let env = SystemEnv::default();
    let cl = command_line(ctx);
    let cwd = std::env::current_dir()
        .map_err(|e| Die::Fatal(crate::messages::cannot_get_cwd(&crate::global::strerror(&e))))?;
    let early = repo_init::load_config(&env, &cl, None).map_err(|e| Die::Fatal(e.to_string()))?;

    let origin = match &opts.origin {
        Some(o) => o.clone(),
        None => early
            .string("clone.defaultremotename")
            .ok()
            .flatten()
            .map_or_else(|| "origin".to_owned(), |v| v.to_string()),
    };

    let source = resolve_source(repo, &cwd, opts.local)?;
    let dir = match dir_arg {
        Some(d) => d.to_owned(),
        None => rgit_remote::url::humanish_dir(repo.as_bytes(), opts.bare, source.bundle)
            .map_err(|e| Die::Fatal(e.to_string()))?
            .to_string(),
    };
    if let Some(d) = &opts.depth {
        match d.parse::<i64>() {
            Ok(n) if n > 0 => {}
            _ => return Err(Die::Fatal(msg::depth_not_positive(d))),
        }
    }

    let target = cwd.join(&dir);
    let existed = std::fs::symlink_metadata(&target).is_ok();
    if existed && !(target.is_dir() && is_empty_dir(&target)) {
        return Err(Die::Fatal(msg::destination_exists(&dir)));
    }
    let local_source = source.local_dir;
    // A shallow repository is cloned like a remote one (its shallow
    // boundary comes with the fetch).
    let source_shallow =
        local_source && repository_git_dir(Path::new(&source.url)).join("shallow").is_file();
    let local_mode = local_source && !source_shallow;
    // Shallow options; a local path ignores them (it copies everything).
    let mut ignored_local: Vec<&str> = Vec::new();
    let mut deepen = PackOptions::default();
    if let Some(d) = &opts.depth {
        ignored_local.push("--depth");
        deepen.deepen = d.parse::<u32>().ok();
    }
    if let Some(t) = &opts.shallow_since {
        ignored_local.push("--shallow-since");
        let dctx = rgit_date::DateContext::from_env();
        deepen.deepen_since = Some(rgit_date::approxidate(t, &dctx) as i64);
    }
    if !opts.shallow_exclude.is_empty() {
        ignored_local.push("--shallow-exclude");
        deepen.deepen_not = opts.shallow_exclude.clone();
    }
    // Implies --single-branch, also when ignored.
    let shallow_clone = !ignored_local.is_empty();
    if opts.filter.is_some() {
        ignored_local.push("--filter");
    }
    if local_source {
        deepen = PackOptions::default();
    }

    // `--separate-git-dir`: the repository lives there, the work tree
    // gets a `.git` file.
    let separate = opts
        .separate_git_dir
        .as_ref()
        .map(|s| (s, rgit_repo::path::real_path_lossy(Path::new(s), &cwd)));
    if let Some((shown, path)) = &separate {
        if std::fs::symlink_metadata(path).is_ok() && !(path.is_dir() && is_empty_dir(path)) {
            return Err(Die::Fatal(msg::repository_path_exists(shown)));
        }
    }
    // `GIT_WORK_TREE`: `<dir>` becomes the git directory of a non-bare
    // repository whose work tree is elsewhere (`core.worktree`).
    let env_work_tree = std::env::var_os("GIT_WORK_TREE")
        .filter(|w| !w.is_empty() && !opts.bare && separate.is_none())
        .map(|w| cwd.join(w));

    let mut junk = Junk {
        work_tree: None,
        work_tree_existed: existed,
        git_dir: None,
        git_dir_existed: existed,
        armed: true,
        on_signal: Vec::new(),
    };
    let mut init_git_dir = None;
    let mut init_work_tree = None;
    let (git_dir, work_tree) = if opts.bare {
        junk.git_dir = Some(target.clone());
        if !existed {
            create_dir(&target, &dir)?;
        }
        (target.clone(), None)
    } else if let Some(wt) = &env_work_tree {
        junk.git_dir = Some(target.clone());
        if !existed {
            create_dir(&target, &dir)?;
        }
        let wt_shown = wt.to_string_lossy().into_owned();
        junk.work_tree_existed = wt.exists();
        junk.work_tree = Some(wt.clone());
        create_dir(wt, &wt_shown)?;
        let wt = std::fs::canonicalize(wt).unwrap_or_else(|_| wt.clone());
        init_work_tree = Some(wt.clone());
        (target.clone(), Some(wt))
    } else {
        junk.work_tree = Some(target.clone());
        if !existed {
            create_dir(&target, &dir)?;
        }
        match &separate {
            Some((_, path)) => {
                junk.git_dir_existed = path.exists();
                junk.git_dir = Some(path.clone());
                init_git_dir = Some(target.join(".git"));
                (path.clone(), Some(target.clone()))
            }
            None => {
                junk.git_dir_existed = false;
                (target.join(".git"), Some(target.clone()))
            }
        }
    };

    // A clone interrupted by a signal leaves nothing behind either.
    for (dir, existed) in [
        (junk.work_tree.clone(), junk.work_tree_existed),
        (junk.git_dir.clone(), junk.git_dir_existed),
    ] {
        if let Some(dir) = dir {
            let abs = std::path::absolute(&dir).unwrap_or(dir);
            junk.on_signal.push(rgit_fs::remove_dir_on_signal(&abs, existed));
        }
    }

    // Create the repository.
    let init_opts = InitOptions {
        git_dir: init_git_dir.unwrap_or_else(|| git_dir.clone()),
        separate_git_dir: separate.as_ref().map(|(_, p)| p.clone()),
        bare: opts.bare,
        work_tree: init_work_tree,
        object_dir: None,
        template: opts.template.as_ref().map(|t| {
            if t.is_empty() {
                repo_init::Template::Empty
            } else {
                repo_init::Template::Dir(cwd.join(t))
            }
        }),
        shared: None,
        // A clone starts out as SHA-1 whatever `GIT_DEFAULT_HASH` says and
        // takes over the remote's object format once that is known.
        object_format: Some(rgit_hash::HashKind::Sha1),
        ref_format: opts.ref_format,
        initial_branch: None,
    };
    repo_init::init(&init_opts, &env, &cl, early, &mut |n| {
        if let Notice::Warning(w) = n {
            eprintln!("warning: {w}");
        }
    })
    .map_err(|e| init_failed(&e))?;
    // The separate git directory with symbolic links resolved.
    let git_dir = if separate.is_some() {
        std::fs::canonicalize(&git_dir).unwrap_or(git_dir)
    } else {
        git_dir
    };

    if !opts.quiet {
        if opts.bare {
            eprintln!("{}", msg::cloning_into_bare(&dir));
        } else {
            eprintln!("{}", msg::cloning_into(&dir));
        }
    }
    if local_source {
        for o in &ignored_local {
            eprintln!("warning: {}", msg::ignored_in_local_clones(o));
        }
    }
    if let Some(rev) = &opts.revision {
        if opts.branch.is_some() {
            return Err(Die::Fatal(msg::incompatible("--revision", "--branch")));
        }
        if opts.mirror {
            return Err(Die::Fatal(msg::incompatible("--revision", "--mirror")));
        }
        if RefspecSet::parse([rev.as_str()], Direction::Fetch).is_err() {
            return Err(Die::Fatal(msg::invalid_refspec(rev)));
        }
    }

    let refname = format!("refs/remotes/{origin}/test");
    if origin.is_empty()
        || !rgit_refs::name::is_valid_refname(
            refname.as_bytes(),
            rgit_refs::name::RefNameFlags::STRICT,
        )
    {
        return Err(Die::Fatal(msg::invalid_remote_name(&origin)));
    }

    let config_path = git_dir.join("config");
    // `-c <key>[=<value>]`: added (without a value: `true`) with section
    // and variable name in lower case, like git writes them.
    for kv in &opts.config {
        let (k, v) = kv.split_once('=').unwrap_or((kv, "true"));
        let k = canonical_key(k);
        if let Err(e) = config_add(&config_path, &k, v) {
            if let Die::Fatal(m) = e {
                eprintln!("error: {m}");
            }
            return Err(Die::Fatal(msg::CONFIG_WRITE_FAILED.into()));
        }
    }

    // `--bundle-uri`: the bundles come first (before a local clone copies
    // the objects). Their refs (`refs/bundles/*`) become `have`s below.
    let mut bundles_used = false;
    if let Some(uri) = &opts.bundle_uri {
        let setup = open_new_repository(ctx, &git_dir, work_tree.as_deref())?;
        let config = refs_support::load_config(ctx, &setup)?;
        let repo = LocalRepo::open(&setup, config).map_err(|e| Die::Fatal(e.to_string()))?;
        let bundle_settings = ConnectSettings {
            env: &env,
            config: &repo.config,
            base_env: ctx.child_env(),
            upload_pack: None,
            ip: opts.ip,
            verbose: opts.verbose,
        };
        let o = rgit_fetch::bundle_uri::fetch_bundle_uri(&repo, &bundle_settings, uri, None);
        bundles_used = o.unbundled;
        if let (true, Some(token)) = (o.heuristic, o.token) {
            config_set(&config_path, "fetch.bundleURI", uri)?;
            config_set(&config_path, "fetch.bundleCreationToken", &token.to_string())?;
        }
    }

    // Borrow objects: --reference repositories, then the source itself
    // for a local --shared clone.
    let mut alternates: Vec<PathBuf> = Vec::new();
    let references = opts
        .reference
        .iter()
        .map(|r| (r, true))
        .chain(opts.reference_if_able.iter().map(|r| (r, false)));
    for (r, required) in references {
        match reference_objects(r, &cwd) {
            Ok(dir) => {
                if !alternates.contains(&dir) {
                    alternates.push(dir);
                }
            }
            Err(reason) if required => return Err(Die::Fatal(reason)),
            Err(reason) => eprintln!("info: {}", msg::could_not_add_alternate(r, &reason)),
        }
    }
    if opts.shared && local_mode {
        let dir = common_dir_of(&repository_git_dir(Path::new(&source.url))).join("objects");
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        if !alternates.contains(&dir) {
            alternates.push(dir);
        }
    } else if local_mode {
        // A local clone copies the source's object store (hard links
        // where possible) and borrows what the source borrows.
        let src_git_dir = repository_git_dir(Path::new(&source.url));
        check_source_ownership(ctx, &src_git_dir, &cwd)?;
        let src = common_dir_of(&src_git_dir).join("objects");
        let mut copy = LocalCopy {
            hardlink: !opts.no_hardlinks,
            alternates: &mut alternates,
        };
        copy.objects(&src, &git_dir.join("objects"))?;
    }
    let alternates_file = git_dir.join("objects").join("info").join("alternates");
    if !alternates.is_empty() {
        let mut text = String::new();
        for a in &alternates {
            text.push_str(&a.to_string_lossy());
            text.push('\n');
        }
        let shown = alternates_file.to_string_lossy().into_owned();
        std::fs::create_dir_all(git_dir.join("objects").join("info"))
            .and_then(|()| std::fs::write(&alternates_file, text))
            .map_err(|e| {
                Die::Fatal(msg::unable_to_write(&shown, &crate::global::strerror(&e)))
            })?;
    }

    // Open the new repository like any command would.
    let setup = open_new_repository(ctx, &git_dir, work_tree.as_deref())?;
    let config = refs_support::load_config(ctx, &setup)?;
    rgit_protocol::sideband::set_remote_colors(crate::color::remote_colors(&config));
    crate::color::set_sideband_controls(&config, Some(&source.url));
    let fatal = |e: &dyn std::fmt::Display| Die::Fatal(e.to_string());
    let repo = LocalRepo::open(&setup, config).map_err(|e| fatal(&e))?;
    for w in &repo.warnings {
        eprintln!("warning: {w}");
    }
    // --reject-shallow (clone.rejectShallow): a local source is checked
    // here, a remote one when its shallow-info arrives.
    let reject_shallow = opts.reject_shallow.unwrap_or_else(|| {
        repo.config
            .bool("clone.rejectshallow")
            .ok()
            .flatten()
            .unwrap_or(false)
    });
    if reject_shallow && source_shallow {
        return Err(Die::Fatal(msg::SOURCE_SHALLOW.into()));
    }
    if opts.local == Some(true) && !local_mode {
        if source_shallow {
            eprintln!("warning: {}", msg::SHALLOW_IGNORING_LOCAL);
        }
        eprintln!("warning: {}", msg::LOCAL_IGNORED);
    }

    // A partial clone: the server filters, the pack is a promisor pack.
    // Nothing is fetched lazily before the checkout.
    let partial = opts.filter.is_some() && !local_mode;
    let lazy_before = rgit_odb::lazy::set_enabled(false);

    // Talk to the remote.
    let connect = ConnectSettings {
        env: &env,
        config: &repo.config,
        base_env: ctx.child_env(),
        upload_pack: opts.upload_pack.clone(),
        ip: opts.ip,
        verbose: opts.verbose,
    };
    // `--server-option` replaces `remote.<name>.serverOption` (given with
    // `-c`); a bad `remote.*` entry is fatal here like in git.
    let configured = rgit_remote::Remotes::from_config(&repo.config)
        .map_err(super::ls_remote::remote_config_die)?
        .get(origin.as_bytes(), None)
        .map_err(super::ls_remote::remote_config_die)?
        .server_options;
    let server_options: Vec<String> = if opts.server_options.is_empty() {
        configured
            .iter()
            .map(|o| o.to_str_lossy().into_owned())
            .collect()
    } else {
        opts.server_options.clone()
    };
    let helper_ctx = rgit_transport::helper::HelperContext {
        remote: Some(origin.clone()),
        vcs: repo
            .config
            .string(format!("remote.{origin}.vcs"))
            .ok()
            .flatten()
            .map(|v| v.to_str_lossy().into_owned()),
        git_dir: Some(repo.git_dir.clone()),
    };
    let mut session = Session::connect_with(&source.connect_url, &connect, "clone", &helper_ctx)
        .map_err(|e| fatal(&e))?;
    if session.bundle_header().is_some_and(|h| h.filter.is_some()) {
        return Err(Die::Fatal(msg::CLONE_FILTERED_BUNDLE.into()));
    }
    session
        .check_server_options(&opts.server_options)
        .map_err(|e| fatal(&e))?;
    // Promisor remotes the server offers; `--filter=auto` takes their
    // filters.
    // (Their configuration follows that of the new remote.)
    let accepted = session.accept_promisor_remotes(&repo.config);
    let send_filter = match opts.filter.as_ref().map(|f| f.0.as_str()) {
        Some(crate::promisor::AUTO) => accepted.as_ref().and_then(|a| a.auto_filter()),
        f => f.map(str::to_owned),
    };

    // Fetch refspecs given with `-c remote.<name>.fetch=...` are used in
    // addition to the default one.
    let fetch_key = format!("remote.{origin}.fetch");
    let extra_fetch: Vec<String> = repo
        .config
        .entries()
        .iter()
        .filter(|e| e.name == fetch_key.as_bytes())
        .filter_map(|e| e.value.as_ref().map(|v| v.to_str_lossy().into_owned()))
        .collect();
    let mut ls = LsRefsArgs::standard();
    ls.server_options = server_options.clone();
    if !opts.mirror && opts.revision.is_none() {
        ls.ref_prefixes.push("refs/heads/".into());
        if opts.tags || opts.branch.is_some() {
            ls.ref_prefixes.push("refs/tags/".into());
        }
        // The names `--branch` could stand for.
        if let Some(b) = &opts.branch {
            for p in ["", "refs/", "refs/tags/", "refs/heads/", "refs/remotes/"] {
                ls.ref_prefixes.push(format!("{p}{b}"));
            }
            ls.ref_prefixes.push(format!("refs/remotes/{b}/HEAD"));
        }
        for spec in &extra_fetch {
            let src = spec.trim_start_matches('+');
            let src = src.split(':').next().unwrap_or(src);
            let prefix = src.split('*').next().unwrap_or(src);
            if !prefix.is_empty() && !src.starts_with('^') {
                ls.ref_prefixes.push(prefix.to_owned());
            }
        }
        // A single branch named by --branch does not need the remote HEAD
        // (and gets no `refs/remotes/<origin>/HEAD` over protocol v2).
        if opts.branch.is_none() || !opts.single_branch.unwrap_or(shallow_clone) {
            ls.ref_prefixes.push("HEAD".into());
        }
    }
    let mut refs: Vec<RemoteRef> = session.list_refs(&ls).map_err(|e| fatal(&e))?;
    // Invalid names from the server are dropped with a warning (t5702).
    let valid = |name: &[u8]| {
        rgit_refs::name::is_valid_refname(name, rgit_refs::name::RefNameFlags::ONELEVEL)
    };
    refs.retain(|r| {
        let ok = r.name == b"HEAD" || valid(&r.name);
        if !ok {
            eprintln!("warning: {}", msg::invalid_advertised_ref(&r.name.to_str_lossy()));
        }
        ok
    });
    for r in &mut refs {
        if let Some(t) = r.symref_target.as_ref().filter(|t| !valid(t)) {
            eprintln!("warning: {}", msg::invalid_symref_target(&t.to_str_lossy()));
            r.symref_target = None;
        }
    }
    // A v0/v1 server advertises every ref (HEAD included): keep what v2
    // would list.
    if !ls.ref_prefixes.is_empty() {
        refs.retain(|r| {
            r.name == b"HEAD"
                || ls
                    .ref_prefixes
                    .iter()
                    .any(|p| r.name.starts_with(p.as_bytes()))
        });
    }
    // A remote helper that imports lists no values: import everything
    // (`HEAD` first) and take the values from its private refs.
    if session.needs_import() {
        session.helper_option("cloning", "true").map_err(|e| fatal(&e))?;
        let mut names: Vec<Vec<u8>> = refs
            .iter()
            .filter(|r| r.name == b"HEAD")
            .map(|r| r.name.clone())
            .collect();
        names.extend(refs.iter().filter(|r| r.name != b"HEAD").map(|r| r.name.clone()));
        session
            .import_refs(&repo, &mut refs, &names)
            .map_err(|e| fatal(&e))?;
        let null = ObjectId::null(session.object_format());
        refs.retain(|r| r.oid != Some(null));
    }

    let remote_head = refs.iter().find(|r| r.name == b"HEAD");
    let mut head_target: Option<String> = remote_head
        .and_then(|h| h.symref_target.as_ref())
        .map(|t| rgit_bstr::from_bytes(t).into_owned());
    // The local default branch name.
    let default_branch = || {
        std::env::var("GIT_TEST_DEFAULT_INITIAL_BRANCH_NAME")
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| {
                repo.config
                    .string("init.defaultbranch")
                    .ok()
                    .flatten()
                    .map(|v| v.to_string())
            })
            .unwrap_or_else(|| "master".to_owned())
    };
    // A detached remote HEAD: the branch at the same commit, preferring
    // the default branch name, else the first one.
    if head_target.is_none() {
        if let Some(oid) = remote_head.and_then(|h| h.oid) {
            let preferred = format!("refs/heads/{}", default_branch());
            let branches = || {
                refs.iter()
                    .filter(|r| r.oid == Some(oid) && r.name.starts_with(b"refs/heads/"))
            };
            head_target = branches()
                .find(|r| r.name == preferred.as_bytes())
                .or_else(|| branches().next())
                .map(|r| rgit_bstr::from_bytes(&r.name).into_owned());
        }
    }
    let empty = refs.iter().all(|r| r.oid.is_none());
    // The new repository uses the remote's object format (an empty
    // repository passes it on too); it is switched below.
    let hash = session.object_format();

    // The branch to check out: --branch, else the remote's HEAD.
    let mut our: Option<Our> = None;
    if let Some(rev) = &opts.revision {
        let found = resolve_revision(rev, &refs, hash)
            .ok_or_else(|| Die::Fatal(msg::remote_revision_not_found(rev, &origin)))?;
        our = Some(Our::Tag(found.0, found.1));
    } else if let Some(b) = &opts.branch {
        let head = format!("refs/heads/{b}");
        let tag = format!("refs/tags/{b}");
        let found = |name: &str| {
            refs.iter()
                .find(|r| r.name == name.as_bytes())
                .and_then(|r| r.oid)
        };
        our = found(&head)
            .map(|oid| Our::Branch(head.clone(), oid))
            .or_else(|| found(&tag).map(|oid| Our::Tag(tag.clone(), oid)));
        if our.is_none() {
            return Err(Die::Fatal(msg::remote_branch_not_found(b, &origin)));
        }
    } else if let Some(oid) = remote_head.and_then(|h| h.oid) {
        our = Some(match &head_target {
            Some(t) if t.starts_with("refs/heads/") => Our::Branch(t.clone(), oid),
            _ => Our::Detached(oid),
        });
    } else if remote_head.is_none() {
        // No HEAD advertised (an unborn one the server does not show):
        // the remote branch named like the local default branch.
        let name = format!("refs/heads/{}", default_branch());
        if let Some(oid) = refs
            .iter()
            .find(|r| r.name == name.as_bytes())
            .and_then(|r| r.oid)
        {
            our = Some(Our::Branch(name, oid));
        }
    }

    // The refspec, now that the branch for --single-branch is known.
    // A shallow clone is a single-branch clone unless told otherwise.
    let single_ref = match (&our, opts.single_branch.unwrap_or(shallow_clone)) {
        (Some(Our::Branch(r, _) | Our::Tag(r, _)), true) => Some(r.clone()),
        _ => None,
    };
    // `--single-branch` without a branch to follow (a detached HEAD):
    // nothing but the commit HEAD is at.
    // `--revision`: the same, and no refspec at all.
    let detached_single = opts.revision.is_some()
        || (opts.single_branch == Some(true)
            && !opts.mirror
            && matches!(our, Some(Our::Detached(_))));
    let spec = clone_refspec(
        &origin,
        &CloneOptions {
            bare: opts.bare,
            mirror: opts.mirror,
            single: single_ref.as_deref(),
        },
    );
    let mut cfg = vec![(format!("remote.{origin}.url"), source.url.clone())];
    if opts.mirror || !opts.tags {
        cfg.push((format!("remote.{origin}.tagOpt"), "--no-tags".to_owned()));
    }
    if let (Some(f), false) = (&spec.configured, detached_single) {
        cfg.push((format!("remote.{origin}.fetch"), f.clone()));
    }
    if opts.mirror {
        cfg.push((format!("remote.{origin}.mirror"), "true".to_owned()));
    }
    if let Some((_, text)) = &opts.filter {
        cfg.push(("core.repositoryformatversion".to_owned(), "1".to_owned()));
        cfg.push((format!("remote.{origin}.promisor"), "true".to_owned()));
        cfg.push((format!("remote.{origin}.partialclonefilter"), text.clone()));
    }
    for (k, v) in &cfg {
        if *k == fetch_key {
            config_add(&config_path, k, v)?;
        } else {
            config_set(&config_path, k, v)?;
        }
    }
    if let Some(a) = &accepted {
        a.write(&config_path);
    }
    // Take over the remote's object format: record it, and open the
    // repository again so that objects and references use it.
    let (setup, repo) = if hash == setup.format.object_format {
        (setup, repo)
    } else {
        switch_object_format(&setup, &repo.config, &config_path, hash)?;
        drop(repo);
        let setup = open_new_repository(ctx, &git_dir, work_tree.as_deref())?;
        let config = refs_support::load_config(ctx, &setup)?;
        let repo = LocalRepo::open(&setup, config).map_err(|e| fatal(&e))?;
        (setup, repo)
    };

    // A list the server advertises (`transfer.bundleURI`), unless
    // `--bundle-uri` was given. Bundle refs become `have`s below.
    if opts.bundle_uri.is_none()
        && repo
            .config
            .bool("transfer.bundleuri")
            .ok()
            .flatten()
            .unwrap_or(false)
    {
        if let Some(lines) = session.bundle_uri_lines().map_err(|e| fatal(&e))? {
            let mut list = rgit_bundle::BundleList::new(&source.url);
            for line in &lines {
                let _ = list.parse_line(line, &mut |m| eprintln!("warning: {m}"));
            }
            let missing = list.missing_uris();
            if !missing.is_empty() {
                for id in missing {
                    eprintln!("error: {}", rgit_bundle::messages::bundle_has_no_uri(id));
                }
            } else if !list.entries.is_empty() {
                let bundle_settings = ConnectSettings {
                    env: &env,
                    config: &repo.config,
                    base_env: ctx.child_env(),
                    upload_pack: None,
                    ip: opts.ip,
                    verbose: opts.verbose,
                };
                let o =
                    rgit_fetch::bundle_uri::fetch_bundle_list(&repo, &bundle_settings, &list);
                bundles_used |= o.unbundled;
            }
        }
    }
    let reflog_msg = msg::reflog_clone(&anonymize(&source.url));
    let mut packs: Vec<ReceivedPack> = Vec::new();
    let mut remote_head_ref: Option<String> = None;
    // Every object the new refs and HEAD point to.
    let mut all_tips: Vec<ObjectId> = Vec::new();
    if empty {
        session.close().map_err(|e| fatal(&e))?;
        eprintln!("warning: {}", msg::EMPTY_REPOSITORY);
        if local_mode && !opts.quiet {
            eprintln!("{}", msg::DONE);
        }
    } else {
        let default_spec = (!detached_single).then_some(spec.fetch.as_str());
        let all_specs = extra_fetch.iter().map(String::as_str).chain(default_spec);
        let specs = RefspecSet::parse_with_hash(all_specs, Direction::Fetch, hash)
            .map_err(|e| fatal(&e))?;
        let follow_tags = opts.tags && !opts.mirror && opts.revision.is_none();
        let mut errors = Vec::new();
        let mut mapped: Vec<(String, ObjectId)> = refmap::map_refspecs(&specs, &refs, hash, &mut errors)
            .map_err(|e| fatal(&e))?
            .into_iter()
            .filter_map(|e| Some((rgit_bstr::from_bytes(&e.local?).into_owned(), e.oid)))
            .collect();
        for e in errors {
            eprintln!("error: {e}");
        }
        // The initial refs are one transaction: a ref named twice fails it.
        let duplicate = {
            let mut names = HashSet::new();
            mapped
                .iter()
                .find(|(n, _)| !names.insert(n.as_str()))
                .map(|(n, _)| n.clone())
        };
        // Without --single-branch every tag is fetched; with it, the tags
        // pointing into the fetched history follow (below).
        let all_tags = follow_tags && single_ref.is_none() && !detached_single;
        let is_tag = |r: &RemoteRef| r.name.starts_with(b"refs/tags/") && r.oid.is_some();
        if all_tags {
            for r in refs.iter().filter(|r| is_tag(r)) {
                let name = rgit_bstr::from_bytes(&r.name).into_owned();
                if !mapped.iter().any(|(n, _)| *n == name) {
                    mapped.push((name, r.oid.unwrap_or_else(|| hash.null())));
                }
            }
        }
        // Objects borrowed from alternates are not asked for.
        let mut wants: Vec<ObjectId> = Vec::new();
        let mut seen = HashSet::new();
        // A local source's broken refs are advertised with the null id;
        // they fail when the refs are written (below), as in git, which
        // reads them directly.
        if local_mode {
            seen.insert(hash.null());
        }
        for (_, oid) in &mapped {
            if seen.insert(*oid) && !repo.has_object(oid) {
                wants.push(*oid);
            }
        }
        if let Some(Our::Tag(_, oid) | Our::Detached(oid)) = &our {
            if seen.insert(*oid) && !repo.has_object(oid) {
                wants.push(*oid);
            }
        }
        // With alternates, their refs and the advertised objects found
        // there are announced as `have`s.
        let advertised: Vec<ObjectId> = refs.iter().filter_map(|r| r.oid).collect();
        let haves = (!alternates.is_empty() || bundles_used).then_some(advertised.as_slice());

        let show_progress = !opts.quiet
            && opts
                .progress
                .unwrap_or_else(|| std::io::stderr().is_terminal());
        crate::switch_branch::set_progress(i32::from(show_progress), false);
        let pack_opts = PackOptions {
            no_progress: !show_progress,
            // git asks for tags with --single-branch also under --no-tags
            // (the tag objects arrive, no tag refs are written).
            include_tag: single_ref.is_some() && !opts.mirror,
            server_options: server_options.clone(),
            // A clone always keeps the pack.
            unpack_limit: None,
            fsck: rgit_fetch::FsckCheck::from_config(
                &repo.config,
                repo.hash,
                rgit_fetch::messages::INVALID_INDEX_PACK_OUTPUT,
            ),
            filter: partial.then(|| send_filter.clone()).flatten(),
            promisor: partial.then(|| promisor_refs(&refs)),
            ..deepen.clone()
        };
        packs.extend(
            session
                .fetch_pack(&repo, &wants, haves, &pack_opts)
                .map_err(|e| fatal(&e))?,
        );

        let tag_opts = PackOptions {
            deepen: None,
            deepen_relative: false,
            deepen_since: None,
            deepen_not: Vec::new(),
            ..pack_opts.clone()
        };
        // --single-branch: the tags pointing into the fetched history; tag
        // objects the server did not include come in a second round.
        if follow_tags && !all_tags {
            let mut more: Vec<ObjectId> = Vec::new();
            for r in refs.iter().filter(|r| is_tag(r)) {
                let name = rgit_bstr::from_bytes(&r.name).into_owned();
                let oid = r.oid.unwrap_or_else(|| hash.null());
                if mapped.iter().any(|(n, _)| *n == name) {
                    continue;
                }
                if !repo.has_object(&r.peeled.unwrap_or(oid)) {
                    continue;
                }
                if !repo.has_object(&oid) && !more.contains(&oid) {
                    more.push(oid);
                }
                mapped.push((name, oid));
            }
            packs.extend(
                session
                    .fetch_pack(&repo, &more, Some(&advertised), &tag_opts)
                    .map_err(|e| fatal(&e))?,
            );
        }
        // The new shallow boundary: what the server cut (also when the
        // source itself is shallow).
        let info = session.take_shallow_info();
        session.close().map_err(|e| fatal(&e))?;
        let shallow: HashSet<ObjectId> = info
            .map(|i| rgit_fetch::shallow::apply(&[], &i).into_iter().collect())
            .unwrap_or_default();
        if reject_shallow && !shallow_clone && !shallow.is_empty() {
            return Err(Die::Fatal(msg::SOURCE_SHALLOW.into()));
        }

        if local_mode && !opts.quiet {
            eprintln!("{}", msg::DONE);
        }
        if let Some(dup) = duplicate {
            return Err(Die::Fatal(rgit_refs::messages::duplicate(&dup)));
        }
        if let Some((name, _)) = mapped.iter().find(|(_, o)| local_mode && o.is_null()) {
            return Err(Die::Fatal(rgit_refs::messages::no_oid_nor_target(name)));
        }

        let received: HashSet<ObjectId> = packs
            .iter()
            .flat_map(|p| p.objects.iter().copied())
            .collect();
        let tips: Vec<ObjectId> = mapped.iter().map(|(_, o)| *o).collect();
        all_tips.extend(&tips);
        if let Some(Our::Tag(_, oid) | Our::Branch(_, oid) | Our::Detached(oid)) = &our {
            all_tips.push(*oid);
        }
        if graph::check_connected(&repo.odb, &tips, &received, &shallow, partial).is_err() {
            return Err(Die::Fatal(msg::REMOTE_DID_NOT_SEND_ALL.into()));
        }
        if !shallow.is_empty() {
            let set: std::collections::BTreeSet<ObjectId> = shallow.iter().copied().collect();
            rgit_fetch::shallow::write(&repo.shallow_file(), &set).map_err(|e| fatal(&e))?;
        }

        // The initial reference transaction: one new `packed-refs` file,
        // without reflog entries (as git writes it).
        let packed: Vec<PackedRef> = mapped
            .into_iter()
            .map(|(name, oid)| {
                let peeled = (repo.kind(&oid) == Some(ObjectKind::Tag))
                    .then(|| graph::peel(&repo.odb, &oid))
                    .flatten();
                PackedRef { name, oid, peeled }
            })
            .collect();
        write_packed_refs(&setup, &repo.config, &packed)?;

        if !opts.bare {
            if let Some(t) = &head_target {
                if let Some(dst) = specs.map_src(&rgit_bstr::to_bytes(t)) {
                    let dst = rgit_bstr::from_bytes(&dst).into_owned();
                    if packed.iter().any(|p| p.name == dst) {
                        remote_head_ref = Some(dst);
                    }
                }
            }
        }
    }

    // --dissociate: copy what is borrowed, then stop borrowing.
    if opts.dissociate && !alternates.is_empty() {
        dissociate(&repo, &all_tips, &alternates_file)?;
    }

    // HEAD, the local branch and its upstream.
    let config = &repo.config;
    let objects = Objects::open(&setup);
    let store = refs_support::ref_store(&setup, config);
    let mut hooks = UpdateHooks::new(&setup, config, &objects);
    let commit = |u: RefUpdate, hooks: &mut UpdateHooks<'_>| -> Result<(), Die> {
        let mut tx = Transaction::new(&store);
        tx.push(u)
            .and_then(|()| tx.commit(hooks))
            .map_err(|e| Die::Fatal(e.to_string()))
    };
    if let Some(r) = &remote_head_ref {
        let name = format!("refs/remotes/{origin}/HEAD");
        hooks.mark_symbolic(&name);
        commit(
            RefUpdate::symref_update(name.as_str(), r.as_str(), None).message(reflog_msg.as_str()),
            &mut hooks,
        )?;
    }
    let mut checkout_oid = None;
    let mut detached_name = None;
    let mut not_a_commit = None;
    match &our {
        _ if empty => {
            let branch = match (&opts.branch, &head_target) {
                (Some(b), _) => Some(format!("refs/heads/{b}")),
                (None, Some(t)) => Some(t.clone()),
                (None, None) => None,
            };
            if let Some(b) = &branch {
                set_head_symref(&git_dir, b)?;
            }
            // Without the remote's HEAD (protocol v0/v1), the branch the
            // new repository was created with.
            let branch = branch.or_else(|| {
                let head = rgit_refs::read_head_file(&git_dir).ok()?;
                let target = head.strip_prefix(b"ref: ")?.trim_ascii_end();
                Some(rgit_bstr::from_bytes(target).into_owned())
            });
            if let (Some(b), false) = (branch, opts.bare) {
                write_branch_config(&config_path, config, &origin, &b)?;
            }
        }
        Some(Our::Branch(name, oid)) => {
            set_head_symref(&git_dir, name)?;
            if !opts.bare {
                commit(
                    RefUpdate::update("HEAD", *oid, None).message(reflog_msg.as_str()),
                    &mut hooks,
                )?;
                write_branch_config(&config_path, config, &origin, name)?;
            }
            checkout_oid = Some(*oid);
            crate::convert::set_checkout_meta(Some(name.clone()), Some(oid));
        }
        Some(Our::Tag(_, oid) | Our::Detached(oid)) => {
            let commit_id = objects.peel(oid).unwrap_or(*oid);
            let kind = objects.kind(&commit_id);
            if opts.revision.is_some() && kind != Some(ObjectKind::Commit) {
                let hex = commit_id.to_hex();
                let kind = kind.map_or("object", ObjectKind::name);
                eprintln!("error: {}", msg::object_not_a_commit(&hex, kind));
                return Err(Die::Fatal(msg::could_not_parse(&hex)));
            }
            if let Some(Our::Tag(name, _)) = &our {
                if objects.kind(oid) != Some(ObjectKind::Commit) {
                    not_a_commit = Some(msg::not_a_commit(name, &oid.to_hex()));
                }
            }
            commit(
                RefUpdate::update("HEAD", commit_id, None)
                    .no_deref(true)
                    .message(reflog_msg.as_str()),
                &mut hooks,
            )?;
            checkout_oid = Some(commit_id);
            crate::convert::set_checkout_meta(None, Some(&commit_id));
            detached_name = Some(commit_id.to_hex());
        }
        None => {
            // The remote's HEAD names an unborn branch: so does ours.
            // Without one (v0/v1 do not advertise an unborn HEAD) the
            // initial branch stays.
            if let Some(t) = &head_target {
                set_head_symref(&git_dir, t)?;
            }
            if !opts.bare {
                eprintln!("warning: {}", msg::REMOTE_HEAD_NONEXISTENT);
            }
        }
    }
    // The refs are written: the packs need no `.keep` any more.
    for p in &packs {
        p.release();
    }

    if let Some(w) = not_a_commit {
        eprintln!("warning: {w}");
    }

    // From here on the repository stays, even when the checkout fails.
    junk.armed = false;
    rgit_odb::lazy::set_enabled(lazy_before);
    // `--recurse-submodules`: the submodules to initialise are the active
    // ones.
    if work_tree.is_some() && !opts.no_checkout {
        for spec in &opts.recurse {
            config_add(&git_dir.join("config"), "submodule.active", spec)?;
        }
        // Submodules borrow from the matching repositories below the
        // references' `modules/` (config/submodule.adoc).
        let any_reference = !opts.reference.is_empty() || !opts.reference_if_able.is_empty();
        if !opts.recurse.is_empty() && any_reference {
            let strategy = if opts.reference.is_empty() { "info" } else { "die" };
            config_add(&git_dir.join("config"), "submodule.alternateLocation", "superproject")?;
            config_add(&git_dir.join("config"), "submodule.alternateErrorStrategy", strategy)?;
        }
    }
    junk.on_signal.clear();
    if let (Some(oid), false, Some(_)) = (checkout_oid, opts.no_checkout, &work_tree) {
        // An invalid GIT_ADVICE fails the checkout step (observed).
        let advice = match ctx
            .advisor_with_env(config, &env)
            .and_then(|a| a.enabled(Advice::DetachedHead).map_err(Die::Fatal))
        {
            Ok(a) => a,
            Err(e) => {
                let _ = e.emit();
                eprint!("warning: {}", msg::CHECKOUT_FAILED);
                return Err(Die::Exit(128));
            }
        };
        drop(hooks);
        drop(repo);
        // A filter that fails during the checkout ends the process.
        crate::convert::set_die_epilogue(Some(format!("warning: {}", msg::CHECKOUT_FAILED)));
        let checked_out = initial_checkout(ctx, setup, oid, opts.sparse);
        crate::convert::set_die_epilogue(None);
        let checked_out = match checked_out {
            Ok(r) => r,
            Err(e) => {
                let _ = e.emit();
                eprintln!("fatal: {}", msg::UNABLE_TO_CHECKOUT);
                eprint!("warning: {}", msg::CHECKOUT_FAILED);
                return Err(Die::Exit(128));
            }
        };
        if let Some(name) = detached_name {
            // Shown with --quiet as well (observed from git 2.56).
            if advice {
                eprint!("{}", msg::detached_head_advice(&name));
            }
        }
        // `post-checkout` from the null id; clone exports the new git
        // directory, and its exit code becomes clone's.
        let hooks = crate::hooks::of(&checked_out)?;
        let null = ObjectId::null(checked_out.hash);
        let hook_opts =
            rgit_hook::RunOptions::with_args([null.to_hex(), oid.to_hex(), "1".into()])
                .env("GIT_DIR", checked_out.setup.absolute_git_dir());
        let code = crate::hooks::run(&hooks, "post-checkout", &hook_opts)?.code;
        if code != 0 || opts.recurse.is_empty() {
            return Ok(code);
        }
        let root = checked_out.setup.cwd.join(checked_out.work_tree()?);
        return update_submodules(ctx, opts, &root);
    }
    Ok(0)
}

/// `--recurse-submodules`: `git submodule update --init --recursive` in
/// the new work tree.
fn update_submodules(ctx: &Context, opts: &Options, root: &Path) -> CmdResult {
    let exe = std::env::current_exe().map_err(|e| Die::Fatal(e.to_string()))?;
    let mut args: Vec<String> = ["submodule", "update", "--require-init", "--recursive"]
        .map(String::from)
        .to_vec();
    if opts.quiet {
        args.push("--quiet".into());
    }
    match opts.progress {
        Some(true) => args.push("--progress".into()),
        Some(false) => args.push("--no-progress".into()),
        None => {}
    }
    if let (true, Some((_, text))) = (opts.filter_submodules, &opts.filter) {
        args.push(format!("--filter={text}"));
    }
    if opts.shallow_submodules {
        args.push("--depth=1".into());
    }
    if opts.remote_submodules {
        args.push("--remote".into());
    }
    if opts.jobs >= 0 {
        args.push(format!("--jobs={}", opts.jobs));
    }
    let mut cmd = std::process::Command::new(exe);
    let env = ctx.child_env();
    cmd.envs(
        env.iter()
            .filter(|(k, _)| k != "GIT_DIR" && k != "GIT_WORK_TREE")
            .map(|(k, v)| (k, v)),
    );
    cmd.env_remove("GIT_DIR").env_remove("GIT_WORK_TREE");
    let _ = std::io::stdout().flush();
    let status = cmd
        .args(&args)
        .current_dir(root)
        .status()
        .map_err(|e| Die::Fatal(e.to_string()))?;
    Ok(status.code().unwrap_or(128))
}

/// The initial checkout: a one-way merge of `commit`'s tree into the
/// empty index with the work tree updated (rgit-unpack), then the index
/// is written.
fn initial_checkout(
    ctx: &Context,
    setup: Setup,
    commit: ObjectId,
    sparse: bool,
) -> Result<crate::repository::Repo, Die> {
    use crate::commands::read_tree::{path_options, unpack_failed};
    use rgit_unpack::{Merge, Options as UnpackOptions, Worktree};

    let mut repo = crate::repository::Repo::from_setup(ctx, setup)?;
    let tree = repo
        .peel_to(commit, ObjectKind::Tree)
        .ok_or_else(|| Die::Fatal(msg::unable_to_read_tree(&commit.to_hex())))?;
    let index_path = repo.setup.cwd.join(repo.index_path());
    let mut lock = rgit_fs::LockFile::acquire(&index_path, &rgit_fs::LockOptions::default())
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let (old, old_mtime) = repo.read_index()?;
    let root = repo.setup.cwd.join(repo.work_tree()?);
    let checker = repo.checker(old_mtime)?;
    let wt = Worktree {
        root: &root,
        checker: &checker,
    };
    let sparse = if sparse {
        Some(crate::commands::sparse_checkout::init_for_clone(&repo)?)
    } else {
        crate::sparse::load(&repo)
    };
    let mut opts = UnpackOptions {
        sparse: sparse.as_ref(),
        reset: true,
        update: true,
        initial_checkout: true,
        paths: path_options(&repo),
        ..UnpackOptions::default()
    };
    let plan = rgit_unpack::plan(repo.odb(), &old, &Merge::OneWay(tree), Some(&wt), &mut opts)
        .map_err(unpack_failed)?;
    let conv = crate::convert::Converter::checkout(&repo, plan.index())?;
    let applied = plan.detect_collisions().apply_with_progress(
        repo.odb(),
        &wt,
        &conv,
        crate::switch_branch::progress(),
    );
    for e in &applied.errors {
        eprintln!("error: {e}");
    }
    let ignore_case = repo
        .config
        .bool("core.ignorecase")
        .ok()
        .flatten()
        .unwrap_or(false);
    report_collisions(&root, &applied.index, &applied.collided, ignore_case);
    let opts = crate::index_config::write_options(&repo.config, &applied.index)?;
    let unable = || Die::Fatal(crate::messages::read_tree::UNABLE_TO_WRITE_INDEX.into());
    applied
        .index
        .write_locked(&mut lock, &opts)
        .map_err(|_| unable())?;
    lock.commit().map_err(|_| unable())?;
    if applied.errors.is_empty() {
        Ok(repo)
    } else {
        Err(Die::Exit(128))
    }
}

/// Warns about paths of the initial checkout that were written to the
/// same file (a case-insensitive file system): every collided path and
/// every other entry that now names the same file (or, with
/// `core.ignoreCase`, the same path in another case), in index order.
fn report_collisions(
    root: &std::path::Path,
    index: &rgit_index::Index,
    collided: &[Vec<u8>],
    ignore_case: bool,
) {
    if collided.is_empty() {
        return;
    }
    let file_id = |path: &[u8]| -> Option<(u64, u64)> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            use std::os::unix::fs::MetadataExt;
            let m = std::fs::symlink_metadata(root.join(std::ffi::OsStr::from_bytes(path))).ok()?;
            Some((m.dev(), m.ino()))
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            None
        }
    };
    let ids: HashSet<(u64, u64)> = collided.iter().filter_map(|p| file_id(p)).collect();
    let folded: HashSet<Vec<u8>> = collided.iter().map(|p| p.to_ascii_lowercase()).collect();
    eprint!("warning: {}", msg::PATHS_COLLIDED);
    for e in index.entries() {
        let p = e.path();
        let peer = collided.iter().any(|c| c.as_slice() == p)
            || (ignore_case && folded.contains(&p.to_ascii_lowercase()))
            || (!e.mode.is_gitlink() && file_id(p).is_some_and(|id| ids.contains(&id)));
        if peer {
            eprintln!("  '{}'", String::from_utf8_lossy(p));
        }
    }
}

/// `--revision`: a full object name, or a ref the remote advertises
/// (full name, or the usual `refs/`, `refs/tags/`, `refs/heads/`,
/// `refs/remotes/` abbreviations). Returns the name and the object.
fn resolve_revision(
    rev: &str,
    refs: &[RemoteRef],
    hash: rgit_hash::HashKind,
) -> Option<(String, ObjectId)> {
    if rev.len() == hash.hex_len() {
        if let Ok(oid) = ObjectId::from_hex(hash, rev.as_bytes()) {
            return Some((rev.to_owned(), oid));
        }
    }
    let candidates = [
        rev.to_owned(),
        format!("refs/{rev}"),
        format!("refs/tags/{rev}"),
        format!("refs/heads/{rev}"),
        format!("refs/remotes/{rev}"),
        format!("refs/remotes/{rev}/HEAD"),
    ];
    candidates.iter().find_map(|name| {
        refs.iter()
            .find(|r| r.name == name.as_bytes())
            .and_then(|r| r.oid)
            .map(|oid| (name.clone(), oid))
    })
}

/// Reports a failed `init` step (git's messages) and the exit code.
fn init_failed(e: &repo_init::InitError) -> Die {
    let mut err = std::io::stderr().lock();
    for (level, line) in e.report() {
        let _ = match level {
            Some(l) => writeln!(err, "{} {line}", l.prefix()),
            None => writeln!(err, "{line}"),
        };
    }
    Die::Exit(e.exit_code())
}

/// What `HEAD` of the new repository becomes.
enum Our {
    /// A branch (full name) and its tip.
    Branch(String, ObjectId),
    /// `--branch <tag>`: detached at the tag.
    Tag(String, ObjectId),
    /// The remote's HEAD is detached.
    Detached(ObjectId),
}

/// Turn the freshly initialised (SHA-1, still empty) repository into one
/// with the object format `hash` of the remote.
fn switch_object_format(
    setup: &Setup,
    config: &rgit_config::ConfigSet,
    config_path: &Path,
    hash: rgit_hash::HashKind,
) -> Result<(), Die> {
    config_set(config_path, "core.repositoryformatversion", "1")?;
    config_set(config_path, "extensions.objectformat", hash.name())?;
    if setup.format.ref_storage == rgit_repo::RefStorage::Reftable {
        // The tables name their hash function: start the stack again.
        let store = refs_support::ref_store(setup, config);
        let head = match rgit_refs::RefStore::read_raw(&store, "HEAD") {
            Ok(Some(rgit_refs::Target::Symbolic(t))) => t.to_string(),
            _ => "refs/heads/master".to_owned(),
        };
        drop(store);
        repo_init::restart_reftable(
            &setup.common_dir,
            hash,
            refs_support::shared_repository(config),
            &head,
        )
        .map_err(|e| Die::Fatal(e.to_string()))?;
    }
    Ok(())
}

fn open_new_repository(
    ctx: &Context,
    git_dir: &Path,
    work_tree: Option<&Path>,
) -> Result<Setup, Die> {
    let env = SystemEnv::default();
    let mut so = SetupOptions::from_process().map_err(crate::repo::die)?;
    so.git_dir = Some(git_dir.to_path_buf());
    so.work_tree = work_tree.map(Path::to_path_buf);
    so.common_dir = None;
    so.object_dir = None;
    so.implicit_work_tree = work_tree.is_some();
    let mut protected = StandardProtectedConfig::new(&env, command_line(ctx));
    rgit_repo::setup(&so, &mut protected).map_err(crate::repo::die)
}

fn config_write(path: &Path, key: &str, value: &str, add: bool) -> Result<(), Die> {
    use rgit_config::edit::{SetRequest, ValuePattern};
    let pattern = if add {
        ValuePattern::Never
    } else {
        ValuePattern::Any
    };
    let req = SetRequest {
        key: key.as_bytes(),
        value: Some(value.as_bytes()),
        pattern: &pattern,
        all: false,
        comment: None,
    };
    rgit_config::write::set_in_file(path, &rgit_fs::LockOptions::default(), &req)
        .map(|_| ())
        .map_err(|e| Die::Fatal(e.to_string()))
}

/// `key` with its section and variable name in lower case (the
/// subsection keeps its case).
fn canonical_key(key: &str) -> String {
    match (key.find('.'), key.rfind('.')) {
        (Some(first), Some(last)) if first < last => format!(
            "{}{}{}",
            key[..first].to_ascii_lowercase(),
            &key[first..last],
            key[last..].to_ascii_lowercase()
        ),
        _ => key.to_ascii_lowercase(),
    }
}

fn config_set(path: &Path, key: &str, value: &str) -> Result<(), Die> {
    config_write(path, key, value, false)
}

fn config_add(path: &Path, key: &str, value: &str) -> Result<(), Die> {
    config_write(path, key, value, true)
}

/// The upstream of the new branch; `branch.autoSetupRebase` `always` or
/// `remote` adds `branch.<name>.rebase` (config/branch.adoc).
fn write_branch_config(
    path: &Path,
    config: &ConfigSet,
    origin: &str,
    full: &str,
) -> Result<(), Die> {
    let short = full.strip_prefix("refs/heads/").unwrap_or(full);
    config_set(path, &format!("branch.{short}.remote"), origin)?;
    config_set(path, &format!("branch.{short}.merge"), full)?;
    let rebase = config
        .string("branch.autosetuprebase")
        .ok()
        .flatten()
        .is_some_and(|v| {
            let v = v.to_string();
            v == "always" || v == "remote"
        });
    if rebase {
        config_set(path, &format!("branch.{short}.rebase"), "true")?;
    }
    Ok(())
}

fn set_head_symref(git_dir: &Path, target: &str) -> Result<(), Die> {
    let mut lock = rgit_fs::LockFile::acquire(git_dir.join("HEAD"), &rgit_fs::LockOptions::default())
        .map_err(|e| Die::Fatal(e.to_string()))?;
    lock.write_all(format!("ref: {target}\n").as_bytes())
        .and_then(|()| lock.commit())
        .map_err(|e| Die::Fatal(e.to_string()))
}

/// The initial reference transaction: every fetched reference goes into a
/// new `packed-refs` file, without reflog entries.
fn write_packed_refs(setup: &Setup, config: &ConfigSet, refs: &[PackedRef]) -> Result<(), Die> {
    if refs.is_empty() {
        return Ok(());
    }
    let store = refs_support::ref_store(setup, config);
    if rgit_refs::reftable::backend::uses_reftable(store.git_dir()) {
        // A reftable repository takes them as one transaction.
        let mut tx = rgit_refs::Transaction::new(&store);
        for r in refs {
            tx.push(rgit_refs::RefUpdate::create(r.name.clone(), r.oid))
                .map_err(|e| Die::Fatal(e.to_string()))?;
        }
        return tx
            .commit(&mut rgit_refs::NoHooks)
            .map_err(|e| Die::Fatal(e.to_string()));
    }
    let packed = PackedRefs::from_refs(refs.to_vec());
    let mut lock = rgit_fs::LockFile::acquire(store.packed_refs_path(), &rgit_fs::LockOptions::default())
        .map_err(|e| Die::Fatal(e.to_string()))?;
    lock.write_all(&packed.serialize())
        .and_then(|()| lock.commit())
        .map_err(|e| Die::Fatal(e.to_string()))
}


/// The common directory of the git directory `git_dir` (its `commondir`
/// file for a linked work tree), else `git_dir`.
fn common_dir_of(git_dir: &Path) -> PathBuf {
    match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(text) => git_dir.join(text.trim_end()),
        Err(_) => git_dir.to_path_buf(),
    }
}

/// A local clone reads the source directly: like opening a repository, a
/// source owned by someone else must be listed in `safe.directory`.
fn check_source_ownership(ctx: &Context, git_dir: &Path, cwd: &Path) -> Result<(), Die> {
    use rgit_repo::ProtectedConfig;
    let env = SystemEnv::default();
    if rgit_repo::safe::Owner::current(&env).owns(git_dir) {
        return Ok(());
    }
    let real = rgit_repo::path::real_path_lossy(git_dir, cwd);
    let mut protected = StandardProtectedConfig::new(&env, command_line(ctx));
    let set = protected.load().map_err(crate::repo::die)?;
    let (safe, warnings) = rgit_repo::safe::is_safe_directory(&set, &env, &real, cwd);
    if safe {
        return Ok(());
    }
    Err(crate::repo::die(rgit_repo::Error::DubiousOwnership {
        path: real,
        warnings,
    }))
}

/// The object store copy of a local clone (git-clone.adoc, `--local`):
/// every file under `objects/` is hard-linked (or copied), symbolic links
/// are refused, and `info/alternates` is not copied but its entries are
/// borrowed with absolute paths.
struct LocalCopy<'a> {
    hardlink: bool,
    alternates: &'a mut Vec<PathBuf>,
}

impl LocalCopy<'_> {
    fn objects(&mut self, src: &Path, dst: &Path) -> Result<(), Die> {
        if std::fs::symlink_metadata(src).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(Die::Fatal(msg::objects_is_symlink(&src.to_string_lossy())));
        }
        self.dir(src, dst, Path::new(""))
    }

    fn dir(&mut self, src_root: &Path, dst_root: &Path, rel: &Path) -> Result<(), Die> {
        let src = src_root.join(rel);
        let dst = dst_root.join(rel);
        if !dst.is_dir() && std::fs::create_dir_all(&dst).is_err() {
            return Err(Die::Fatal(msg::failed_to_create_directory(
                &dst.to_string_lossy(),
            )));
        }
        let mut entries: Vec<_> = std::fs::read_dir(&src)
            .map_err(|e| Die::Fatal(e.to_string()))?
            .flatten()
            .collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for e in entries {
            let rel = rel.join(e.file_name());
            let ty = e.file_type().map_err(|e| Die::Fatal(e.to_string()))?;
            if ty.is_symlink() {
                return Err(Die::Fatal(msg::symlink_in_objects(&rel.to_string_lossy())));
            }
            if ty.is_dir() {
                self.dir(src_root, dst_root, &rel)?;
            } else if rel == Path::new("info/alternates") {
                self.borrow_alternates(src_root, &e.path());
            } else {
                self.file(&e.path(), &dst_root.join(&rel))?;
            }
        }
        Ok(())
    }

    fn borrow_alternates(&mut self, objects: &Path, file: &Path) {
        let Ok(text) = std::fs::read_to_string(file) else {
            return;
        };
        for line in text.lines() {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let path = objects.join(line);
            let path = std::fs::canonicalize(&path).unwrap_or(path);
            if !self.alternates.contains(&path) {
                self.alternates.push(path);
            }
        }
    }

    fn file(&self, src: &Path, dst: &Path) -> Result<(), Die> {
        let _ = std::fs::remove_file(dst);
        if self.hardlink && std::fs::hard_link(src, dst).is_ok() {
            return Ok(());
        }
        // A copy gets the default permissions, like a new file, but keeps
        // the modification time, so that the age of loose objects (prune
        // expiry, `gc --prune`) carries over.
        let copied = std::fs::File::open(src).and_then(|mut from| {
            let mut to = std::fs::File::create(dst)?;
            std::io::copy(&mut from, &mut to)?;
            if let Ok(time) = from.metadata().and_then(|m| m.modified()) {
                let _ = to.set_modified(time);
            }
            Ok(())
        });
        copied.map_err(|e| {
            Die::Fatal(msg::failed_to_copy(
                &dst.to_string_lossy(),
                &crate::global::strerror(&e),
            ))
        })
    }
}

/// The git directory of the local repository at `path`: `<path>/.git`
/// (a directory or a gitfile), else `path` itself.
fn repository_git_dir(path: &Path) -> PathBuf {
    let gitfile = |file: &Path| -> Option<PathBuf> {
        let text = std::fs::read_to_string(file).ok()?;
        let target = text.trim_end().strip_prefix("gitdir: ")?;
        Some(file.parent().unwrap_or(Path::new(".")).join(target))
    };
    if path.is_file() {
        if let Some(dir) = gitfile(path) {
            return dir;
        }
    }
    let dot = path.join(".git");
    if dot.is_dir() {
        return dot;
    }
    gitfile(&dot).unwrap_or_else(|| path.to_path_buf())
}

/// The object directory of the `--reference` repository `reference`
/// (absolute), or why it cannot be used.
fn reference_objects(reference: &str, cwd: &Path) -> Result<PathBuf, String> {
    let base = cwd.join(reference);
    let path = std::fs::canonicalize(&base).unwrap_or(base);
    let git_dir = repository_git_dir(&path);
    if git_dir.join("commondir").is_file() {
        return Err(msg::reference_linked_checkout(reference));
    }
    if !is_git_directory(&git_dir) {
        return Err(msg::reference_not_local(reference));
    }
    if git_dir.join("shallow").is_file() {
        return Err(msg::reference_shallow(reference));
    }
    if git_dir.join("info").join("grafts").is_file() {
        return Err(msg::reference_grafted(reference));
    }
    let objects = git_dir.join("objects");
    Ok(std::fs::canonicalize(&objects).unwrap_or(objects))
}

/// `--dissociate`: write every object reachable from `tips` into a pack
/// of the new repository, then remove `objects/info/alternates`.
fn dissociate(repo: &LocalRepo, tips: &[ObjectId], alternates_file: &Path) -> Result<(), Die> {
    use rgit_pack::write::{self as pw, Tip, WriteOptions};
    let fatal = |e: &dyn std::fmt::Display| Die::Fatal(e.to_string());
    let hash = repo.hash;
    let tips: Vec<Tip> = tips
        .iter()
        .map(|id| Tip {
            id: *id,
            exclude: false,
        })
        .collect();
    let listed = pw::enumerate(&repo.odb, hash, &tips).map_err(|e| fatal(&e))?;
    let ordered = pw::pack_order(&listed);
    // Existing packs (also those of the alternates) are copied from.
    let mut packs = repo
        .odb
        .packs()
        .map(|p| p.set().packs())
        .unwrap_or_default();
    for dir in rgit_odb::resolve_alternates(&repo.objects_dir()) {
        packs.extend(rgit_pack::PackSet::open(dir.join("pack"), hash).packs());
    }
    let opts = WriteOptions {
        ofs_delta: true,
        ..WriteOptions::default()
    };
    let (_, data) = pw::write_pack(Vec::new(), hash, &ordered, &repo.odb, &packs, &opts)
        .map_err(|e| fatal(&e))?;
    // The packs the local copy brought along are redundant now, like after
    // `git repack -a -d` (packs with a `.keep` file stay).
    let old: Vec<PathBuf> = store::local_packs(&repo.odb, &[])
        .iter()
        .filter(|p| !p.keep)
        .map(store::LocalPack::base)
        .collect();
    let pack = rgit_fetch::store_pack_stream(repo, &mut data.as_slice())
        .map_err(|e| fatal(&e))?;
    pack.release();
    let objects = repo.objects_dir();
    let pack_dir = objects.join("pack");
    if !old.is_empty() {
        store::remove_multi_pack_index(&pack_dir);
    }
    let new_base = pack.pack.as_ref().map(|p| p.with_extension(""));
    for base in old.iter().filter(|b| Some(*b) != new_base.as_ref()) {
        store::delete_pack(base);
    }
    store::remove_pack_garbage(&pack_dir);
    let packs_now = store::local_packs(&repo.odb, &[]);
    store::prune_packed(&objects, &objects, hash, &packs_now, false)?;
    match std::fs::remove_file(alternates_file) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Die::Fatal(e.to_string())),
    }
}

/// The text of the `.promisor` file of a partial clone's pack: the
/// remote's refs it was fetched for, `<id> <name>` per line.
fn promisor_refs(refs: &[RemoteRef]) -> String {
    let mut out = String::new();
    for r in refs {
        if let Some(oid) = r.oid {
            out.push_str(&format!("{} {}\n", oid.to_hex(), rgit_bstr::from_bytes(&r.name)));
        }
    }
    out
}
