// SPDX-License-Identifier: MIT
//! `git ls-remote` (Documentation/git-ls-remote.adoc): list the references
//! of a remote repository through protocol v2 `ls-refs`.
//!
//! The repository is a remote name or a URL (local, SSH, smart HTTP);
//! without one, the default remote is used and its URL printed as
//! `From <url>` on stderr. Protocol v2 servers are asked with `ls-refs`;
//! v0/v1 servers (or `protocol.version` 0/1) are read from their ref
//! advertisement. `--sort` is not wired yet.

use std::io::Write;

use std::path::Path;

use bstr::ByteSlice;
use rgit_config::{ConfigSet, SystemEnv};
use rgit_hash::ObjectId;
use rgit_pathspec::wildmatch::{WildmatchFlags, wildmatch};
use rgit_protocol::capabilities::{NO_REFS_NAME, split_ref_line};
use rgit_protocol::v2::{Client, LsRefsArgs, RemoteRef};
use rgit_protocol::{Capabilities, Packet, ProtocolVersion, Reader};
use rgit_remote::{BranchConfig, Remotes};
use rgit_transport::{ConnectOptions, Connection, IpVersion, Service, Settings};

use crate::messages::ls_remote as msg;
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
    let head = std::fs::read(git_dir.join("HEAD")).ok()?;
    head.strip_prefix(b"ref: refs/heads/")
        .map(|r| r.trim_ascii_end().to_vec())
}

/// `protocol.version` (default 2).
fn protocol_version(config: &ConfigSet) -> Result<ProtocolVersion, Die> {
    let Some(value) = config
        .get("protocol.version")
        .and_then(|e| e.value.as_deref())
    else {
        return Ok(ProtocolVersion::V2);
    };
    match value.trim_ascii() {
        b"0" => Ok(ProtocolVersion::V0),
        b"1" => Ok(ProtocolVersion::V1),
        b"2" => Ok(ProtocolVersion::V2),
        other => Err(Die::Fatal(msg::unknown_protocol_version(
            &other.to_str_lossy(),
        ))),
    }
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

/// The refs of a protocol v0/v1 advertisement (after the version line):
/// `<oid> <name>` lines up to a flush, the first one with capabilities
/// (`symref=HEAD:<target>`), a peeled tag as `<name>^{}` after its tag.
fn read_v0_advertisement(
    reader: &mut Reader<Box<dyn Connection>>,
) -> rgit_protocol::Result<Vec<RemoteRef>> {
    let mut refs: Vec<RemoteRef> = Vec::new();
    let mut symrefs: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let bad =
        |line: &[u8]| rgit_protocol::Error::InvalidLsRefs(String::from_utf8_lossy(line).into_owned());
    loop {
        let line = match reader.read_or_eof()? {
            None | Some(Packet::Flush) => break,
            Some(Packet::Data(d)) => d.strip_suffix(b"\n").unwrap_or(d).to_vec(),
            Some(other) => return Err(other.unexpected()),
        };
        let (refpart, caps) = split_ref_line(&line);
        if let Some(caps) = caps {
            for (from, to) in caps.symrefs() {
                symrefs.push((from.as_bytes().to_vec(), to.as_bytes().to_vec()));
            }
        }
        let (hex, name) = refpart.split_once_str(" ").ok_or_else(|| bad(&line))?;
        if name == NO_REFS_NAME {
            continue;
        }
        let oid = ObjectId::from_hex_any(hex).map_err(|_| bad(&line))?;
        if let Some(base) = name.strip_suffix(b"^{}") {
            if let Some(last) = refs.last_mut().filter(|r| r.name == base) {
                last.peeled = Some(oid);
                continue;
            }
        }
        refs.push(RemoteRef {
            name: name.to_vec(),
            oid: Some(oid),
            symref_target: None,
            peeled: None,
        });
    }
    for r in &mut refs {
        r.symref_target = symrefs
            .iter()
            .find(|(from, _)| *from == r.name)
            .map(|(_, to)| to.clone());
    }
    Ok(refs)
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
    let sort: RefCell<Option<String>> = RefCell::new(None);
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
            Opt::string(None, "sort", &sort, "key", msg::HELP_SORT),
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
    let remotes = Remotes::from_config(&config).map_err(|e| Die::Fatal(e.to_string()))?;
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

    let settings = Settings {
        env: &env,
        config: &config,
        ip: IpVersion::Any,
        base_env: ctx.child_env(),
    };
    let fatal = |e: &dyn std::fmt::Display| Die::Fatal(e.to_string());
    let Some(mut transport) = rgit_http::transport_for(&url, &settings).map_err(|e| fatal(&e))?
    else {
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
    options.protocol_version = protocol_version(&config)?;
    let conn = transport.connect(&options).map_err(|e| fatal(&e))?;
    let mut reader = Reader::new(conn, "ls-remote");
    skip_service_header(&mut reader).map_err(|e| fatal(&e))?;
    // A service that ends without saying anything (no repository, a
    // failing upload-pack) has printed its own error already.
    if reader.peek().map_err(|e| fatal(&e))?.is_none() {
        let _ = reader.into_inner().close();
        return Err(Die::Fatal(msg::READ_FAILED.into()));
    }
    let refs: Vec<RemoteRef> = match ProtocolVersion::read(&mut reader).map_err(|e| fatal(&e))? {
        ProtocolVersion::V2 => {
            let caps = Capabilities::read_v2(&mut reader).map_err(|e| fatal(&e))?;
            let mut client = Client::with_capabilities(reader, caps).map_err(|e| fatal(&e))?;
            let mut ls = LsRefsArgs::standard();
            ls.server_options = server_options.borrow().clone();
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
        ProtocolVersion::V0 | ProtocolVersion::V1 => {
            let refs = read_v0_advertisement(&mut reader).map_err(|e| fatal(&e))?;
            let mut conn = reader.into_inner();
            if !conn.is_stateless() {
                // End the session: a flush packet instead of wants.
                conn.write_all(b"0000").map_err(|e| fatal(&e))?;
                conn.end_request().map_err(|e| fatal(&e))?;
            }
            conn.close().map_err(|e| fatal(&e))?;
            refs
        }
    };
    if dest.is_none() && !quiet.get() {
        eprintln!("From {url}");
    }
    // Sorting (peeled entries sort as refs of their own) is not done yet.
    if let Some(k) = sort.borrow().as_deref() {
        return Err(Die::Fatal(msg::unsupported_sort(k)));
    }

    let mut out = std::io::stdout().lock();
    let mut found = false;
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
        found = true;
        let name = String::from_utf8_lossy(name);
        if symref.get() {
            if let Some(t) = &r.symref_target {
                let _ = writeln!(out, "ref: {}\t{name}", String::from_utf8_lossy(t));
            }
        }
        let _ = writeln!(out, "{}\t{name}", oid.to_hex());
        if let (Some(p), false) = (r.peeled, refs_only.get()) {
            let peeled_name = format!("{name}^{{}}");
            if tail_match(patterns, peeled_name.as_bytes()) {
                let _ = writeln!(out, "{}\t{peeled_name}", p.to_hex());
            }
        }
    }
    Ok(if exit_code.get() && !found { 2 } else { 0 })
}
