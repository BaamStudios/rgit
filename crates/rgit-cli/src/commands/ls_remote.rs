// SPDX-License-Identifier: MIT
//! `git ls-remote` (Documentation/git-ls-remote.adoc): list the references
//! of a remote repository through protocol v2 `ls-refs`.
//!
//! The repository is a remote name or a URL (local, SSH, smart HTTP);
//! without one, the default remote is used and its URL printed as
//! `From <url>` on stderr. Protocol v2 servers are asked with `ls-refs`;
//! v0/v1 servers (or `protocol.version` 0/1) are read from their ref
//! advertisement. `--sort` orders the listed entries (a peeled `^{}`
//! entry sorts as a reference of its own); keys other than `refname` need
//! a repository and evaluate on the local objects.

use std::io::Write;

use std::path::Path;

use bstr::ByteSlice;
use rgit_config::{ConfigSet, SystemEnv};
use rgit_pathspec::wildmatch::{WildmatchFlags, wildmatch};
use rgit_protocol::v2::{Client, LsRefsArgs, RemoteRef};
use rgit_protocol::{Capabilities, Packet, ProtocolVersion, Reader};
use rgit_remote::{BranchConfig, Remotes};
use rgit_transport::helper::{HelperContext, Target};
use rgit_transport::{ConnectOptions, Connection, IpVersion, Service, Settings};

use rgit_hash::ObjectId;
use rgit_repo::Setup;

use crate::messages::ls_remote as msg;
use crate::ref_filter::{Kind, RefValues, SortKey, Value};
use crate::refs_support::Objects;
use crate::prelude::*;

/// Tail match of git's ls-remote patterns: `<pattern>` matches a ref whose
/// name is the pattern or ends in `/<pattern>` (globs allowed).
fn tail_match(patterns: &[String], name: &[u8]) -> bool {
    if patterns.is_empty() {
        return true;
    }
    patterns.iter().any(|p| {
        let glob = format!("*/{p}");
        wildmatch(p.as_bytes(), name, WildmatchFlags::NONE)
            || wildmatch(glob.as_bytes(), name, WildmatchFlags::NONE)
    })
}

/// The configuration outside a repository: system, global and `-c`.
/// A bad `remote.*` configuration, reported like git (`error: missing
/// value for ...` before the fatal line).
pub(crate) fn remote_config_die(e: rgit_remote::RemoteError) -> Die {
    let rgit_remote::RemoteError::Config(c) = &e else {
        return Die::Fatal(e.to_string());
    };
    let mut lines = c.report();
    let Some((_, last)) = lines.pop() else {
        return Die::Fatal(e.to_string());
    };
    for (level, line) in lines {
        eprintln!("{} {line}", level.prefix());
    }
    Die::Fatal(last)
}

fn config_outside_repository(ctx: &Context) -> ConfigSet {
    let env = SystemEnv::default();
    let opts = rgit_config::LoadOptions {
        env: &env,
        repo: None,
        includes: true,
    };
    let mut set = ConfigSet::new();
    let _ = rgit_config::sources::load_standard(
        opts,
        None,
        &crate::repo::command_line(ctx),
        rgit_config::sources::ScopeFilter::default(),
        &mut set,
    );
    set
}

/// The short name of the checked-out branch, if `HEAD` is a symref.
fn current_branch(git_dir: &Path) -> Option<Vec<u8>> {
    let head = rgit_refs::read_head_file(git_dir).ok()?;
    head.strip_prefix(b"ref: refs/heads/")
        .map(|r| r.trim_ascii_end().to_vec())
}

/// Skip the `# service=git-upload-pack` header and its flush in front of
/// a smart HTTP advertisement.
fn skip_service_header(reader: &mut Reader<Box<dyn Connection>>) -> rgit_protocol::Result<()> {
    if !reader.get_ref().is_stateless() {
        return Ok(());
    }
    if matches!(reader.peek()?, Some(Packet::Data(d)) if d.starts_with(b"# service=")) {
        reader.read()?;
        match reader.read()? {
            Packet::Flush => {}
            other => return Err(other.unexpected()),
        }
    }
    Ok(())
}


pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    // `-q` only silences the "From <url>" line, which is printed when the
    // repository comes from the configuration (not supported yet).
    let quiet = Cell::new(false);
    let tags = Cell::new(false);
    let branches = Cell::new(false);
    let refs_only = Cell::new(false);
    let get_url = Cell::new(false);
    let exit_code = Cell::new(false);
    let symref = Cell::new(false);
    let sort: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let upload_pack: RefCell<Option<String>> = RefCell::new(None);
    let server_options: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let help_only = argv.last().is_some_and(|a| a == "-h") && argv.len() <= 2;
    let heads_short = if help_only { None } else { Some('h') };
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag('q', "quiet", &quiet, msg::HELP_QUIET),
            Opt::string(
                None,
                "upload-pack",
                &upload_pack,
                "exec",
                msg::HELP_UPLOAD_PACK,
            ),
            Opt::string(None, "exec", &upload_pack, "exec", msg::HELP_UPLOAD_PACK).hidden(),
            Opt::flag('t', "tags", &tags, msg::HELP_TAGS),
            Opt::flag('b', "branches", &branches, msg::HELP_BRANCHES),
            // `-h` means `--heads` unless it is the only argument (usage).
            Opt::flag(heads_short, "heads", &branches, msg::HELP_BRANCHES).hidden(),
            Opt::flag(None, "refs", &refs_only, msg::HELP_REFS),
            Opt::flag(None, "get-url", &get_url, msg::HELP_GET_URL),
            Opt::string_list(None, "sort", &sort, "key", msg::HELP_SORT),
            Opt::flag(None, "exit-code", &exit_code, msg::HELP_EXIT_CODE),
            Opt::flag(None, "symref", &symref, msg::HELP_SYMREF),
            Opt::string_list(
                'o',
                "server-option",
                &server_options,
                "server-specific",
                msg::HELP_SERVER_OPTION,
            ),
        ],
    );
    let args = parser.parse(argv)?.args;
    let sort_specs = sort.borrow().clone();
    let sort_keys = crate::ref_filter::parse_sort_keys(&sort_specs)?;
    let (dest, patterns) = match args.split_first() {
        Some((d, p)) => (Some(d.as_str()), p),
        None => (None, &[][..]),
    };

    let env = SystemEnv::default();
    let setup = crate::repo::setup_gently(ctx)?;
    let config = match &setup {
        Some(setup) => crate::repo::config(ctx, setup),
        None => config_outside_repository(ctx),
    };
    let remotes = Remotes::from_config(&config).map_err(remote_config_die)?;
    for w in remotes.warnings() {
        eprintln!("warning: {w}");
    }
    let git_dir = setup.as_ref().map(|s| s.absolute_git_dir());
    // Without `<repository>`: `branch.<current>.remote`, else the only
    // remote, else `origin`; only the last fallback must be configured.
    let branch_remote = match (dest, &git_dir) {
        (None, Some(dir)) => current_branch(dir)
            .map(|b| BranchConfig::from_config(&config, &b))
            .transpose()
            .map_err(|e| Die::Fatal(e.to_string()))?
            .and_then(|b| b.remote),
        _ => None,
    };
    let name: Vec<u8> = match dest {
        Some(d) => d.as_bytes().to_vec(),
        None => remotes
            .default_remote_name(branch_remote.as_ref().map(|r| r.as_bstr()))
            .into(),
    };
    let remote = remotes
        .get(&name, git_dir.as_deref())
        .map_err(|e| Die::Fatal(e.to_string()))?;
    if setup.is_none() {
        // Keys are in reverse command-line order; git names the first
        // offending one as given.
        let object_key = sort_specs
            .iter()
            .zip(sort_keys.iter().rev())
            .find(|(_, k)| !matches!(k.atom.kind, Kind::Refname(_)));
        if let Some((spec, _)) = object_key {
            let field = spec.strip_prefix('-').unwrap_or(spec);
            let field = field
                .strip_prefix("version:")
                .or_else(|| field.strip_prefix("v:"))
                .unwrap_or(field);
            return Err(Die::Fatal(msg::sort_needs_repository(field)));
        }
    }
    if dest.is_none() && branch_remote.is_none() && !remote.is_configured() {
        return Err(Die::Fatal(msg::NO_REMOTE.into()));
    }
    for w in &remote.warnings {
        eprintln!("warning: {w}");
    }
    let url = remote
        .fetch_url()
        .map(|u| u.to_str_lossy().into_owned())
        .unwrap_or_default();
    if get_url.get() {
        println!("{url}");
        return Ok(0);
    }

    let vcs = remote.vcs.as_ref().map(|v| v.to_str_lossy().into_owned());
    let protocol = vcs.clone().unwrap_or_else(|| rgit_fetch::protocol_name(&url));
    rgit_remote::check_allowed(protocol.as_bytes(), &config, &env)
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let settings = Settings {
        env: &env,
        config: &config,
        ip: IpVersion::Any,
        base_env: ctx.child_env(),
    };
    let fatal = |e: &dyn std::fmt::Display| Die::Fatal(e.to_string());
    let helper_ctx = HelperContext {
        remote: remote
            .is_configured()
            .then(|| remote.name.to_str_lossy().into_owned()),
        vcs,
        git_dir: setup.as_ref().map(|s| s.git_dir.clone()),
    };
    let target = rgit_http::target_for(&url, &settings, &helper_ctx).map_err(|e| fatal(&e))?;
    let (transport, helper) = match target {
        Some(Target::Transport(t)) => (Some(t), None),
        Some(Target::Helper(h)) => (None, Some(h)),
        None => (None, None),
    };
    let refs: Vec<RemoteRef> = if let Some(mut helper) = helper {
        helper.standard_options(false, 1).map_err(|e| fatal(&e))?;
        let list = helper.list(false).map_err(|e| fatal(&e))?;
        helper.disconnect().map_err(|e| fatal(&e))?;
        rgit_fetch::helper_refs(&list)
    } else {
    let Some(mut transport) = transport else {
        return Err(Die::Fatal(msg::no_remote_helper(
            url.split("://").next().unwrap_or(&url),
        )));
    };
    let mut options = ConnectOptions::new(Service::UploadPack);
    options.program = upload_pack.borrow().clone().or_else(|| {
        remote
            .upload_pack
            .as_ref()
            .map(|p| p.to_str_lossy().into_owned())
    });
    options.protocol_version =
        rgit_fetch::protocol_version(&config, &env).map_err(|e| fatal(&e))?;
    options.trace_name = Some("ls-remote".into());
    let conn = transport.connect(&options).map_err(|e| fatal(&e))?;
    let mut reader = Reader::new(conn, "ls-remote");
    skip_service_header(&mut reader).map_err(|e| fatal(&e))?;
    // A service that ends without saying anything (no repository, a
    // failing upload-pack) has printed its own error already.
    if reader.peek().map_err(|e| fatal(&e))?.is_none() {
        let _ = reader.into_inner().close();
        return Err(Die::Fatal(msg::READ_FAILED.into()));
    }
    match ProtocolVersion::read(&mut reader).map_err(|e| fatal(&e))? {
        ProtocolVersion::V2 => {
            let caps = Capabilities::read_v2(&mut reader).map_err(|e| fatal(&e))?;
            let mut client = Client::with_capabilities(reader, caps).map_err(|e| fatal(&e))?;
            let mut ls = LsRefsArgs::standard();
            // `-o` replaces `remote.<name>.serverOption`.
            ls.server_options = if server_options.borrow().is_empty() {
                remote
                    .server_options
                    .iter()
                    .map(|o| o.to_str_lossy().into_owned())
                    .collect()
            } else {
                server_options.borrow().clone()
            };
            if branches.get() {
                ls.ref_prefixes.push("refs/heads/".into());
            }
            if tags.get() {
                ls.ref_prefixes.push("refs/tags/".into());
            }
            let refs = client.ls_refs(&ls).map_err(|e| fatal(&e))?;
            client.close().map_err(|e| fatal(&e))?;
            client.into_inner().close().map_err(|e| fatal(&e))?;
            refs
        }
        v => {
            if !server_options.borrow().is_empty() {
                eprintln!("hint: {}", rgit_fetch::messages::SERVER_OPTIONS_HINT);
                return Err(Die::Fatal(rgit_fetch::messages::SERVER_OPTIONS_NEED_V2.into()));
            }
            let refs = rgit_protocol::v0::read_advertisement(&mut reader, v)
                .map_err(|e| fatal(&e))?
                .refs;
            let mut conn = reader.into_inner();
            if !conn.is_stateless() {
                // End the session: a flush packet instead of wants. A
                // server that is gone already does not need it.
                let _ = conn.write_all(b"0000");
                let _ = conn.end_request();
            }
            // How the server ends after the flush does not matter (git
            // does not look at its exit status either).
            let _ = conn.close();
            refs
        }
    }
    };
    if dest.is_none() && !quiet.get() {
        eprintln!("From {url}");
    }
    let mut entries: Vec<Entry> = Vec::new();
    for r in &refs {
        let Some(oid) = r.oid else { continue };
        let name = r.name.as_slice();
        if refs_only.get() && !name.starts_with(b"refs/") {
            continue;
        }
        if (branches.get() || tags.get())
            && !((branches.get() && name.starts_with(b"refs/heads/"))
                || (tags.get() && name.starts_with(b"refs/tags/")))
        {
            continue;
        }
        if !tail_match(patterns, name) {
            continue;
        }
        let name = String::from_utf8_lossy(name).into_owned();
        let target = r
            .symref_target
            .as_ref()
            .filter(|_| symref.get())
            .map(|t| String::from_utf8_lossy(t).into_owned());
        let peeled = r.peeled.filter(|_| !refs_only.get());
        entries.push(Entry {
            name: name.clone(),
            oid,
            symref: target,
        });
        if let Some(p) = peeled {
            let peeled_name = format!("{name}^{{}}");
            if tail_match(patterns, peeled_name.as_bytes()) {
                entries.push(Entry {
                    name: peeled_name,
                    oid: p,
                    symref: None,
                });
            }
        }
    }
    let found = !entries.is_empty();
    if !sort_keys.is_empty() {
        entries = sort_entries(entries, &sort_keys, setup.as_ref(), &config)?;
    }

    let mut out = std::io::stdout().lock();
    for e in &entries {
        if let Some(t) = &e.symref {
            let _ = writeln!(out, "ref: {t}\t{}", e.name);
        }
        let _ = writeln!(out, "{}\t{}", e.oid.to_hex(), e.name);
    }
    Ok(if exit_code.get() && !found { 2 } else { 0 })
}

/// One line of the listing (with its `ref:` line for `--symref`).
struct Entry {
    name: String,
    oid: ObjectId,
    symref: Option<String>,
}

/// `entries` in `--sort` order. Outside a repository only `refname` keys
/// get here; inside, the atoms are evaluated on the local objects.
fn sort_entries(
    entries: Vec<Entry>,
    keys: &[SortKey],
    setup: Option<&Setup>,
    config: &ConfigSet,
) -> Result<Vec<Entry>, Die> {
    let suffixes = if keys.iter().any(|k| k.version) {
        crate::ref_filter::version_suffixes(config)
    } else {
        Vec::new()
    };
    let keyed: Vec<(Vec<Value>, Entry)> = match setup {
        None => entries
            .into_iter()
            .map(|e| {
                let vals = keys.iter().map(|_| Value::text(e.name.clone())).collect();
                (vals, e)
            })
            .collect(),
        Some(setup) => {
            let objects = Objects::open(setup);
            let store = crate::refs_support::ref_store(setup, config);
            let mut values = RefValues::new(&objects, &store);
            values.prepare(keys.iter().map(|k| &k.atom))?;
            let mut keyed = Vec::with_capacity(entries.len());
            for e in entries {
                let mut vals = Vec::with_capacity(keys.len());
                for k in keys {
                    vals.push(values.atom_value(&k.atom, &e.name, e.oid)?);
                }
                keyed.push((vals, e));
            }
            keyed
        }
    };
    Ok(crate::ref_filter::sort_items(
        keyed,
        keys,
        false,
        &suffixes,
        |e: &Entry| e.name.as_str(),
    ))
}
