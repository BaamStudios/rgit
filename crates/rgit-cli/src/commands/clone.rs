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
//! A local path is cloned through the normal transport, like
//! `--no-local`; git's messages for a local clone (`done.`) are kept.

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

use crate::messages::clone as msg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, UpdateHooks};
use crate::repo::command_line;

/// Parsed command line.
#[derive(Default)]
struct Options {
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
    /// `--single-branch` / `--no-single-branch`.
    single_branch: Option<bool>,
    tags: bool,
    template: Option<String>,
    config: Vec<String>,
    server_options: Vec<String>,
    ip: IpVersion,
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
    let filter = RefCell::new(None);
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
            Opt::string(None, "filter", &filter, "args", msg::HELP_FILTER),
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

    // Options rgit does not implement yet.
    let unsupported: [(&str, bool); 17] = [
        ("--reject-shallow", reject_shallow.get() > 0),
        ("--no-hardlinks", no_hardlinks.get()),
        ("--shared", shared.get()),
        ("--recurse-submodules", !recurse.borrow().is_empty()),
        ("--reference", !reference.borrow().is_empty()),
        ("--reference-if-able", !reference_if_able.borrow().is_empty()),
        ("--dissociate", dissociate.get()),
        ("--revision", revision.borrow().is_some()),
        ("--shallow-since", shallow_since.borrow().is_some()),
        ("--shallow-exclude", !shallow_exclude.borrow().is_empty()),
        ("--shallow-submodules", shallow_submodules.get()),
        ("--separate-git-dir", separate_git_dir.borrow().is_some()),
        ("--ref-format", ref_format.borrow().is_some()),
        ("--filter", filter.borrow().is_some()),
        ("--also-filter-submodules", also_filter_submodules.get()),
        ("--sparse", sparse.get()),
        ("--bundle-uri", bundle_uri.borrow().is_some()),
    ];
    if let Some((name, _)) = unsupported.iter().find(|(_, used)| *used) {
        return Err(Die::Fatal(msg::unsupported(name)));
    }
    // -v, -j and --remote-submodules change nothing without submodules
    // and progress meters.
    let _ = (verbose.get(), jobs.get(), remote_submodules.get());

    let mut opts = Options {
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
}

/// git's lookup of a local repository: `<repo>/.git`, `<repo>`,
/// `<repo>.git/.git`, `<repo>.git`; the first directory that is a git
/// repository wins. The recorded path is absolute, without a trailing
/// `/.git` that was added here.
fn local_repository(repo: &str, cwd: &Path) -> Option<PathBuf> {
    for (suffix, keep) in [("/.git", ""), ("", ""), (".git/.git", ".git"), (".git", ".git")] {
        let candidate = cwd.join(format!("{repo}{suffix}"));
        if candidate.is_dir() && is_git_directory(&candidate) {
            let base = cwd.join(format!("{repo}{keep}"));
            let real = std::fs::canonicalize(&base).unwrap_or(base);
            return Some(real);
        }
    }
    None
}

/// A directory that has `HEAD`, `objects/` and `refs/` (or a gitfile).
fn is_git_directory(dir: &Path) -> bool {
    dir.join("HEAD").is_file() && dir.join("objects").is_dir() && dir.join("refs").is_dir()
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
    if let Some(path) = local_repository(repo, cwd) {
        return Ok(Source {
            url: path.to_string_lossy().into_owned(),
            local_dir: local != Some(false),
        });
    }
    let bytes = repo.as_bytes();
    if rgit_remote::url::is_url(bytes) || !rgit_remote::url::is_local_path(bytes) {
        return Ok(Source {
            url: repo.to_owned(),
            local_dir: false,
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
        None => rgit_remote::url::humanish_dir(repo.as_bytes(), opts.bare, false)
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
    let local_mode = source.local_dir;
    if opts.depth.is_some() {
        if local_mode {
            eprintln!("warning: {}", msg::DEPTH_IGNORED_LOCAL);
        } else {
            return Err(Die::Fatal(msg::unsupported("--depth")));
        }
    }

    let mut junk = Junk {
        work_tree: None,
        work_tree_existed: existed,
        git_dir: None,
        git_dir_existed: existed,
        armed: true,
    };
    let (git_dir, work_tree) = if opts.bare {
        junk.git_dir = Some(target.clone());
        if !existed {
            create_dir(&target, &dir)?;
        }
        (target.clone(), None)
    } else {
        junk.work_tree = Some(target.clone());
        if !existed {
            create_dir(&target, &dir)?;
        }
        junk.git_dir_existed = false;
        (target.join(".git"), Some(target.clone()))
    };

    // Create the repository.
    let init_opts = InitOptions {
        git_dir: git_dir.clone(),
        separate_git_dir: None,
        bare: opts.bare,
        work_tree: None,
        object_dir: None,
        template: opts.template.as_ref().map(|t| {
            if t.is_empty() {
                repo_init::Template::Empty
            } else {
                repo_init::Template::Dir(cwd.join(t))
            }
        }),
        shared: None,
        object_format: None,
        ref_format: None,
        initial_branch: None,
    };
    repo_init::init(&init_opts, &env, &cl, early, &mut |n| {
        if let Notice::Warning(w) = n {
            eprintln!("warning: {w}");
        }
    })
    .map_err(|e| {
        let mut err = std::io::stderr().lock();
        for (level, line) in e.report() {
            let _ = match level {
                Some(l) => writeln!(err, "{} {line}", l.prefix()),
                None => writeln!(err, "{line}"),
            };
        }
        Die::Exit(e.exit_code())
    })?;

    if !opts.quiet {
        if opts.bare {
            eprintln!("{}", msg::cloning_into_bare(&dir));
        } else {
            eprintln!("{}", msg::cloning_into(&dir));
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
    for kv in &opts.config {
        let Some((k, v)) = kv.split_once('=') else {
            return Err(Die::Fatal(msg::invalid_config_pair(kv)));
        };
        config_add(&config_path, k, v)?;
    }

    // Open the new repository like any command would.
    let setup = open_new_repository(ctx, &git_dir, work_tree.as_deref())?;
    let config = refs_support::load_config(ctx, &setup)?;
    let hash = setup.format.object_format;
    let fatal = |e: &dyn std::fmt::Display| Die::Fatal(e.to_string());
    let repo = LocalRepo::open(&setup, config).map_err(|e| fatal(&e))?;
    for w in &repo.warnings {
        eprintln!("warning: {w}");
    }

    // Talk to the remote.
    let connect = ConnectSettings {
        env: &env,
        config: &repo.config,
        base_env: ctx.child_env(),
        upload_pack: opts.upload_pack.clone(),
        ip: opts.ip,
    };
    let mut session = Session::connect(&source.url, &connect, "clone").map_err(|e| fatal(&e))?;

    let mut ls = LsRefsArgs::standard();
    ls.server_options = opts.server_options.clone();
    if !opts.mirror {
        ls.ref_prefixes.push("HEAD".into());
        ls.ref_prefixes.push("refs/heads/".into());
        if opts.tags {
            ls.ref_prefixes.push("refs/tags/".into());
        }
    }
    let refs: Vec<RemoteRef> = session.client().ls_refs(&ls).map_err(|e| fatal(&e))?;

    let remote_head = refs.iter().find(|r| r.name == b"HEAD");
    let head_target: Option<String> = remote_head
        .and_then(|h| h.symref_target.as_ref())
        .map(|t| String::from_utf8_lossy(t).into_owned());
    let empty = refs.iter().all(|r| r.oid.is_none());

    // The branch to check out: --branch, else the remote's HEAD.
    let mut our: Option<Our> = None;
    if let Some(b) = &opts.branch {
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
        if our.is_none() && !empty {
            return Err(Die::Fatal(msg::remote_branch_not_found(b, &origin)));
        }
    } else if let Some(oid) = remote_head.and_then(|h| h.oid) {
        our = Some(match &head_target {
            Some(t) if t.starts_with("refs/heads/") => Our::Branch(t.clone(), oid),
            _ => Our::Detached(oid),
        });
    }

    // The refspec, now that the branch for --single-branch is known.
    let single_ref = match (&our, opts.single_branch.unwrap_or(false)) {
        (Some(Our::Branch(r, _) | Our::Tag(r, _)), true) => Some(r.clone()),
        _ => None,
    };
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
    if let Some(f) = &spec.configured {
        cfg.push((format!("remote.{origin}.fetch"), f.clone()));
    }
    if opts.mirror {
        cfg.push((format!("remote.{origin}.mirror"), "true".to_owned()));
    }
    for (k, v) in &cfg {
        config_set(&config_path, k, v)?;
    }

    let reflog_msg = msg::reflog_clone(&anonymize(&source.url));
    let mut packs: Vec<ReceivedPack> = Vec::new();
    let mut remote_head_ref: Option<String> = None;
    if empty {
        session.close().map_err(|e| fatal(&e))?;
        eprintln!("warning: {}", msg::EMPTY_REPOSITORY);
    } else {
        let specs = RefspecSet::parse([spec.fetch.as_str()], Direction::Fetch).map_err(|e| fatal(&e))?;
        let follow_tags = opts.tags && !opts.mirror;
        let mut errors = Vec::new();
        let mut mapped: Vec<(String, ObjectId)> = refmap::map_refspecs(&specs, &refs, hash, &mut errors)
            .map_err(|e| fatal(&e))?
            .into_iter()
            .filter_map(|e| Some((e.local?.to_str_lossy().into_owned(), e.oid)))
            .collect();
        for e in errors {
            eprintln!("error: {e}");
        }
        // Without --single-branch every tag is fetched; with it, the tags
        // pointing into the fetched history follow (below).
        let all_tags = follow_tags && single_ref.is_none();
        let is_tag = |r: &RemoteRef| r.name.starts_with(b"refs/tags/") && r.oid.is_some();
        if all_tags {
            for r in refs.iter().filter(|r| is_tag(r)) {
                let name = String::from_utf8_lossy(&r.name).into_owned();
                if !mapped.iter().any(|(n, _)| *n == name) {
                    mapped.push((name, r.oid.unwrap_or_else(|| hash.null())));
                }
            }
        }
        let mut wants: Vec<ObjectId> = Vec::new();
        let mut seen = HashSet::new();
        for (_, oid) in &mapped {
            if seen.insert(*oid) {
                wants.push(*oid);
            }
        }
        if let Some(Our::Tag(_, oid)) = &our {
            if seen.insert(*oid) {
                wants.push(*oid);
            }
        }

        let show_progress = !opts.quiet
            && opts
                .progress
                .unwrap_or_else(|| std::io::stderr().is_terminal());
        let pack_opts = PackOptions {
            no_progress: !show_progress,
            // git asks for tags with --single-branch also under --no-tags
            // (the tag objects arrive, no tag refs are written).
            include_tag: single_ref.is_some() && !opts.mirror,
            server_options: opts.server_options.clone(),
        };
        packs.extend(
            session
                .fetch_pack(&repo, &wants, None, &pack_opts)
                .map_err(|e| fatal(&e))?,
        );

        // --single-branch: the tags pointing into the fetched history; tag
        // objects the server did not include come in a second round.
        if follow_tags && !all_tags {
            let mut more: Vec<ObjectId> = Vec::new();
            for r in refs.iter().filter(|r| is_tag(r)) {
                let name = String::from_utf8_lossy(&r.name).into_owned();
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
            let advertised: Vec<ObjectId> = refs.iter().filter_map(|r| r.oid).collect();
            packs.extend(
                session
                    .fetch_pack(&repo, &more, Some(&advertised), &pack_opts)
                    .map_err(|e| fatal(&e))?,
            );
        }
        session.close().map_err(|e| fatal(&e))?;

        let received: HashSet<ObjectId> = packs
            .iter()
            .flat_map(|p| p.objects.iter().copied())
            .collect();
        let tips: Vec<ObjectId> = mapped.iter().map(|(_, o)| *o).collect();
        if graph::check_connected(&repo.odb, &tips, &received).is_err() {
            return Err(Die::Fatal(msg::REMOTE_DID_NOT_SEND_ALL.into()));
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
                if let Some(dst) = specs.map_src(t.as_bytes()) {
                    let dst = dst.to_str_lossy().into_owned();
                    if packed.iter().any(|p| p.name == dst) {
                        remote_head_ref = Some(dst);
                    }
                }
            }
        }
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
            if let Some(b) = branch {
                set_head_symref(&git_dir, &b)?;
                if !opts.bare {
                    write_branch_config(&config_path, &origin, &b)?;
                }
            }
        }
        Some(Our::Branch(name, oid)) => {
            set_head_symref(&git_dir, name)?;
            if !opts.bare {
                commit(
                    RefUpdate::update("HEAD", *oid, None).message(reflog_msg.as_str()),
                    &mut hooks,
                )?;
                write_branch_config(&config_path, &origin, name)?;
            }
            checkout_oid = Some(*oid);
        }
        Some(Our::Tag(_, oid) | Our::Detached(oid)) => {
            let commit_id = objects.peel(oid).unwrap_or(*oid);
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
            detached_name = Some(commit_id.to_hex());
        }
        None => {
            if head_target.is_some() {
                eprintln!("warning: {}", msg::REMOTE_HEAD_NONEXISTENT);
            }
        }
    }
    // The refs are written: the packs need no `.keep` any more.
    for p in &packs {
        p.release();
    }

    if local_mode && !opts.quiet {
        eprintln!("{}", msg::DONE);
    }
    if let Some(w) = not_a_commit {
        eprintln!("warning: {w}");
    }

    // From here on the repository stays, even when the checkout fails.
    junk.armed = false;
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
        let checked_out = match initial_checkout(ctx, setup, oid) {
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
        let opts = rgit_hook::RunOptions::with_args([null.to_hex(), oid.to_hex(), "1".into()])
            .env("GIT_DIR", checked_out.setup.absolute_git_dir());
        return Ok(crate::hooks::run(&hooks, "post-checkout", &opts)?.code);
    }
    Ok(0)
}

/// The initial checkout: a one-way merge of `commit`'s tree into the
/// empty index with the work tree updated (rgit-unpack), then the index
/// is written.
fn initial_checkout(
    ctx: &Context,
    setup: Setup,
    commit: ObjectId,
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
    let mut opts = UnpackOptions {
        reset: true,
        update: true,
        initial_checkout: true,
        paths: path_options(&repo),
        ..UnpackOptions::default()
    };
    let plan = rgit_unpack::plan(repo.odb(), &old, &Merge::OneWay(tree), Some(&wt), &mut opts)
        .map_err(unpack_failed)?;
    let conv = crate::convert::Converter::checkout(&repo, plan.index())?;
    let applied = plan.apply(repo.odb(), &wt, &conv);
    if conv.skipped_filter() {
        eprintln!("warning: {}", crate::messages::read_tree::ATTRIBUTES_NOT_APPLIED);
    }
    for e in &applied.errors {
        eprintln!("error: {e}");
    }
    let skip_hash = repo
        .config
        .bool("index.skiphash")
        .ok()
        .flatten()
        .unwrap_or(false);
    let unable = || Die::Fatal(crate::messages::read_tree::UNABLE_TO_WRITE_INDEX.into());
    applied
        .index
        .write_locked(&mut lock, &rgit_index::WriteOptions { skip_hash })
        .map_err(|_| unable())?;
    lock.commit().map_err(|_| unable())?;
    if applied.errors.is_empty() {
        Ok(repo)
    } else {
        Err(Die::Exit(128))
    }
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
        ValuePattern::regex(b"^$").map_err(|e| Die::Fatal(e.to_string()))?
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

fn config_set(path: &Path, key: &str, value: &str) -> Result<(), Die> {
    config_write(path, key, value, false)
}

fn config_add(path: &Path, key: &str, value: &str) -> Result<(), Die> {
    config_write(path, key, value, true)
}

fn write_branch_config(path: &Path, origin: &str, full: &str) -> Result<(), Die> {
    let short = full.strip_prefix("refs/heads/").unwrap_or(full);
    config_set(path, &format!("branch.{short}.remote"), origin)?;
    config_set(path, &format!("branch.{short}.merge"), full)
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
    let packed = PackedRefs::from_refs(refs.to_vec());
    let mut lock = rgit_fs::LockFile::acquire(store.packed_refs_path(), &rgit_fs::LockOptions::default())
        .map_err(|e| Die::Fatal(e.to_string()))?;
    lock.write_all(&packed.serialize())
        .and_then(|()| lock.commit())
        .map_err(|e| Die::Fatal(e.to_string()))
}

