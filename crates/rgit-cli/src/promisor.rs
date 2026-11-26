// SPDX-License-Identifier: MIT
//! Promisor remotes and fetching missing objects on demand (partial
//! clone, `Documentation/technical/partial-clone.adoc`,
//! `Documentation/config/remote.adoc`, `Documentation/config/promisor.adoc`).
//!
//! A promisor remote is a remote with `remote.<name>.promisor` true or a
//! `remote.<name>.partialCloneFilter`, plus the remote named by
//! `extensions.partialClone`, which is tried last. When an object lookup
//! finds nothing, [`install`]'s fetcher runs
//! `git -c fetch.negotiationAlgorithm=noop fetch <remote> --no-tags
//! --no-write-fetch-head --recurse-submodules=no --filter=blob:none --stdin`
//! with the wanted ids on standard input, remote after remote until every
//! object arrived (the command line git uses, seen with `GIT_TRACE`).

use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;

use bstr::ByteSlice;
use rgit_config::ConfigSet;
use rgit_hash::ObjectId;

use crate::Context;

/// The promisor remotes in the order they are asked: those marked with
/// `remote.<name>.promisor` or `remote.<name>.partialCloneFilter` in
/// configuration order, then `extensions.partialClone`.
pub fn remotes(config: &ConfigSet) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in config.entries() {
        let name = e.name.to_str_lossy();
        let Some(rest) = name.strip_prefix("remote.") else {
            continue;
        };
        let Some((remote, key)) = rest.rsplit_once('.') else {
            continue;
        };
        let marked = match key {
            "promisor" => config.bool(e.name.as_slice()).ok().flatten() == Some(true),
            "partialclonefilter" => e.value.is_some(),
            _ => false,
        };
        if marked && !out.iter().any(|r| r == remote) {
            out.push(remote.to_owned());
        }
    }
    if let Some(name) = partial_clone_extension(config) {
        out.retain(|r| *r != name);
        out.push(name);
    }
    out
}

/// The value of `extensions.partialClone`, if set.
pub fn partial_clone_extension(config: &ConfigSet) -> Option<String> {
    config
        .get("extensions.partialclone")
        .and_then(|e| e.value.as_ref())
        .map(|v| v.to_str_lossy().into_owned())
}

/// Whether the repository has a promisor remote (it may miss objects).
pub fn has_remote(config: &ConfigSet) -> bool {
    !remotes(config).is_empty()
}

/// The filter of a promisor remote: `remote.<name>.partialCloneFilter`.
pub fn filter_of(config: &ConfigSet, remote: &str) -> Option<String> {
    config
        .get(format!("remote.{remote}.partialclonefilter"))
        .and_then(|e| e.value.as_ref())
        .map(|v| v.to_str_lossy().into_owned())
}

/// Commands that never fetch missing objects (git turns fetching off in
/// them): transfers, packing and checking commands.
const NO_FETCH: &[&str] = &[
    "fetch",
    "fetch-pack",
    "index-pack",
    "unpack-objects",
    "fsck",
    "fsck-objects",
    "prune",
    "prune-packed",
    "repack",
    "gc",
    "upload-pack",
    "receive-pack",
    "multi-pack-index",
    "count-objects",
    "maintenance",
    "pack-redundant",
];

/// Install the lazy fetcher for this process (`rgit_odb::lazy`), unless
/// `--no-lazy-fetch` or `GIT_NO_LAZY_FETCH` forbid it, and turn it off for
/// the commands that never fetch.
pub fn install(ctx: &Context, command: &str) {
    if ctx.no_lazy_fetch || env_true("GIT_NO_LAZY_FETCH") {
        if command == "pack-objects" {
            // pack-objects asks for missing promisor objects itself (as
            // under upload-pack, which forbids lazy fetches): it is told
            // no and fails on promisor objects.
            rgit_odb::lazy::set_fetcher(Arc::new(|objects: &Path, ids: &[ObjectId]| {
                refuse(objects, ids)
            }));
        } else {
            rgit_odb::lazy::clear_fetcher();
        }
        return;
    }
    let child = ctx.clone();
    rgit_odb::lazy::set_fetcher(Arc::new(move |objects: &Path, ids: &[ObjectId]| {
        fetch_missing(&child, objects, ids)
    }));
    rgit_odb::lazy::set_enabled(!NO_FETCH.contains(&command));
}

/// The fetcher when lazy fetching is forbidden.
fn refuse(objects: &Path, ids: &[ObjectId]) -> bool {
    use crate::messages::promisor as msg;
    let Some(hash) = ids.first().map(|i| i.kind()) else {
        return false;
    };
    eprintln!("warning: {}", msg::LAZY_FETCH_DISABLED);
    let odb =
        rgit_odb::ObjectDatabase::open_with_packs(objects, hash, rgit_odb::LooseOptions::default());
    if let Some(id) = ids.iter().find(|id| odb.is_promisor_object(id)) {
        crate::convert::die_now(&msg::could_not_fetch(&id.to_hex()));
    }
    false
}

fn env_true(name: &str) -> bool {
    std::env::var(name)
        .is_ok_and(|v| rgit_config::value::parse_bool(Some(v.as_bytes())).unwrap_or(false))
}

/// The git directory of an `objects` directory, with its configuration.
fn repository_of(ctx: &Context, objects: &Path) -> Option<(std::path::PathBuf, ConfigSet)> {
    let git_dir = std::path::absolute(objects.parent()?).ok()?;
    let env = rgit_config::SystemEnv::default();
    let opts = rgit_config::LoadOptions {
        env: &env,
        repo: None,
        includes: true,
    };
    let repo_cfg = rgit_config::sources::RepoConfig {
        common_dir: git_dir.clone(),
        git_dir: git_dir.clone(),
        worktree_config: None,
    };
    let mut set = ConfigSet::new();
    let _ = rgit_config::sources::load_standard(
        opts,
        Some(&repo_cfg),
        &crate::repo::command_line(ctx),
        rgit_config::sources::ScopeFilter::default(),
        &mut set,
    );
    Some((git_dir, set))
}

/// The fetcher: ask the promisor remotes of the repository owning
/// `objects` for `ids`. Dies like git when a promisor object cannot be
/// fetched from any of them.
fn fetch_missing(ctx: &Context, objects: &Path, ids: &[ObjectId]) -> bool {
    let Some((git_dir, config)) = repository_of(ctx, objects) else {
        return false;
    };
    let remotes = remotes(&config);
    if remotes.is_empty() {
        return false;
    }
    let Some(hash) = ids.first().map(|i| i.kind()) else {
        return false;
    };
    let odb =
        rgit_odb::ObjectDatabase::open_with_packs(objects, hash, rgit_odb::LooseOptions::default());
    let quiet = config.bool("promisor.quiet").ok().flatten() == Some(true);
    let mut remaining: Vec<ObjectId> = ids.to_vec();
    for remote in &remotes {
        run_fetch(ctx, &git_dir, remote, quiet, &remaining);
        remaining.retain(|id| !odb.contains(id));
        if remaining.is_empty() {
            return true;
        }
    }
    // Objects the promisor remotes promised and did not send are fatal;
    // others are just missing.
    if let Some(id) = remaining.iter().find(|id| odb.is_promisor_object(id)) {
        crate::convert::die_now(&crate::messages::promisor::could_not_fetch(&id.to_hex()));
    }
    true
}

/// Run one lazy fetch from `remote`.
fn run_fetch(ctx: &Context, git_dir: &Path, remote: &str, quiet: bool, ids: &[ObjectId]) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut args: Vec<String> = [
        "-c",
        "fetch.negotiationAlgorithm=noop",
        "fetch",
        remote,
        "--no-tags",
        "--no-write-fetch-head",
        "--recurse-submodules=no",
        "--filter=blob:none",
        "--stdin",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    if quiet {
        args.push("--quiet".to_owned());
    }
    let traced = rgit_trace::trace2::git_argv(&args);
    rgit_trace::trace_run_command::<String>(&[], &traced);
    let mut cmd = std::process::Command::new(exe);
    cmd.args(&args);
    for (k, v) in ctx.child_env() {
        cmd.env(k, v);
    }
    cmd.env_remove("GIT_WORK_TREE")
        .env("GIT_DIR", git_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null());
    let child_trace = rgit_trace::trace2::child_start(Default::default(), &traced);
    let Ok(mut child) = cmd.spawn() else {
        if let Some(t) = child_trace {
            t.exit(-1, -1);
        }
        return;
    };
    rgit_trace::trace2::data_repo_id("promisor", "fetch_count", &ids.len().to_string(), Some(0));
    if let Some(mut stdin) = child.stdin.take() {
        let mut text = String::new();
        for id in ids {
            text.push_str(&id.to_hex());
            text.push('\n');
        }
        let _ = stdin.write_all(text.as_bytes());
    }
    let pid = child.id();
    let status = child.wait();
    if let Some(t) = child_trace {
        match status {
            Ok(s) => t.exit_status(pid, s),
            Err(_) => t.exit(i64::from(pid), -1),
        }
    }
}

/// Characters written as `%xx` in a filter inside `combine:`
/// (`Documentation/rev-list-options.adoc`).
fn needs_escape(b: u8) -> bool {
    b <= 0x20 || b >= 0x7f || b"~!@#$^&*()[]{}\\;\",<>?'`%+".contains(&b)
}

/// `spec` as a part of `combine:`.
fn encode_sub_spec(spec: &str) -> String {
    let mut out = String::new();
    for b in spec.bytes() {
        if needs_escape(b) {
            out.push_str(&format!("%{b:02x}"));
        } else {
            out.push(b as char);
        }
    }
    out
}

/// Repeated `--filter` options: the parsed filter and its spec as stored
/// in `remote.<name>.partialCloneFilter` (one spec expanded, several as
/// `combine:` of the specs as given). `GIT_TRACE` shows each spec added to
/// a combination.
pub fn parse_filters(
    specs: &[String],
) -> Result<Option<(rgit_pack::write::ObjectFilter, String)>, crate::Die> {
    use rgit_pack::write::ObjectFilter;
    let parse = |s: &str| ObjectFilter::parse(s).map_err(|e| crate::Die::Fatal(e.to_string()));
    match specs {
        [] => Ok(None),
        [one] => {
            let f = parse(one)?;
            let text = f.to_spec();
            Ok(Some((f, text)))
        }
        many => {
            let mut parts = Vec::new();
            let mut text = String::from("combine:");
            for (i, s) in many.iter().enumerate() {
                let enc = encode_sub_spec(s);
                rgit_trace::trace!(rgit_trace::TRACE, "Add to combine filter-spec: {}", enc);
                parts.push(parse(s)?);
                if i > 0 {
                    text.push('+');
                }
                text.push_str(&enc);
            }
            Ok(Some((ObjectFilter::Combine(parts), text)))
        }
    }
}

/// Make the repository able to hold a partial clone: version 1 of the
/// repository format. Fails when an unknown extension stands in the way.
pub fn upgrade_format(setup: &rgit_repo::Setup) -> Result<(), crate::Die> {
    use crate::messages::promisor as msg;
    if setup.format.version >= 1 {
        return Ok(());
    }
    if let Some(ext) = setup.format.unknown.first() {
        eprintln!("error: {}", msg::cannot_upgrade_unknown_extension(ext));
        return Err(crate::Die::Fatal(msg::UNABLE_TO_UPGRADE.into()));
    }
    set_config(setup, "core.repositoryformatversion", "1")
}

fn set_config(setup: &rgit_repo::Setup, key: &str, value: &str) -> Result<(), crate::Die> {
    use rgit_config::edit::{SetRequest, ValuePattern};
    let req = SetRequest {
        key: key.as_bytes(),
        value: Some(value.as_bytes()),
        pattern: &ValuePattern::Any,
        all: false,
        comment: None,
    };
    rgit_config::write::set_in_file(
        &setup.common_dir.join("config"),
        &rgit_fs::LockOptions::default(),
        &req,
    )
    .map(|_| ())
    .map_err(|e| crate::Die::Fatal(e.to_string()))
}

/// Register `remote` as a promisor remote using `filter` (the spec as
/// stored): `remote.<name>.promisor` unless it is one already, and
/// `remote.<name>.partialCloneFilter` unless one is set.
pub fn register(
    setup: &rgit_repo::Setup,
    config: &ConfigSet,
    remote: &str,
    filter: &str,
) -> Result<(), crate::Die> {
    if !remotes(config).iter().any(|r| r == remote) {
        upgrade_format(setup)?;
        set_config(setup, &format!("remote.{remote}.promisor"), "true")?;
    }
    if filter_of(config, remote).is_none() {
        set_config(
            setup,
            &format!("remote.{remote}.partialclonefilter"),
            filter,
        )?;
    }
    Ok(())
}

/// The filter a fetch from `remote` sends and whether the remote is a
/// promisor remote: `--filter` (which registers the remote as promisor
/// remote), else the remote's `partialCloneFilter` unless `--no-filter`.
pub fn fetch_filter(
    setup: &rgit_repo::Setup,
    config: &ConfigSet,
    remote: &str,
    filters: &[String],
    no_filter: bool,
) -> Result<(Option<String>, bool), crate::Die> {
    if let Some((f, text)) = parse_filters(filters)? {
        register(setup, config, remote, &text)?;
        return Ok((Some(f.to_spec()), true));
    }
    if !remotes(config).iter().any(|r| r == remote) {
        return Ok((None, false));
    }
    // Observed from git 2.56: without a filter (`--no-filter`, none
    // configured) a promisor remote's pack is stored like any other one
    // (loose below the unpack limit, no `.promisor` file) and checked for
    // connectivity.
    if no_filter {
        return Ok((None, false));
    }
    let spec = match filter_of(config, remote) {
        Some(s) => Some(
            rgit_pack::write::ObjectFilter::parse(&s)
                .map_err(|e| crate::Die::Fatal(e.to_string()))?
                .to_spec(),
        ),
        None => None,
    };
    let promisor = spec.is_some();
    Ok((spec, promisor))
}
