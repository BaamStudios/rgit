// SPDX-License-Identifier: MIT
//! `git ls-remote` (Documentation/git-ls-remote.adoc): list the references
//! of a remote repository through protocol v2 `ls-refs`.
//!
//! First slice: the repository is a URL or path reachable by the local or
//! SSH transport; remote names, smart HTTP and `--sort` are not wired yet.

use std::io::Write;

use rgit_config::{ConfigSet, SystemEnv};
use rgit_pathspec::wildmatch::{WildmatchFlags, wildmatch};
use rgit_protocol::ProtocolVersion;
use rgit_protocol::v2::{Client, LsRefsArgs, RemoteRef};
use rgit_transport::{ConnectOptions, IpVersion, Service, Settings};

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
    let upload_pack: RefCell<Option<String>> = RefCell::new(None);
    let server_options: RefCell<Vec<String>> = RefCell::new(Vec::new());
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
            Opt::flag('h', "heads", &branches, msg::HELP_BRANCHES).hidden(),
            Opt::flag(None, "refs", &refs_only, msg::HELP_REFS),
            Opt::flag(None, "get-url", &get_url, msg::HELP_GET_URL),
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
    let Some((url, patterns)) = args.split_first() else {
        return Err(Die::Fatal(msg::NO_REMOTE.into()));
    };
    if get_url.get() {
        println!("{url}");
        return Ok(0);
    }

    let env = SystemEnv::default();
    let config = match crate::repo::setup_gently(ctx)? {
        Some(setup) => crate::repo::config(ctx, &setup),
        None => ConfigSet::new(),
    };
    let settings = Settings {
        env: &env,
        config: &config,
        ip: IpVersion::Any,
        base_env: ctx.child_env(),
    };
    let fatal = |e: &dyn std::fmt::Display| Die::Fatal(e.to_string());
    let Some(mut transport) =
        rgit_transport::transport_for(url, &settings).map_err(|e| fatal(&e))?
    else {
        return Err(Die::Fatal(format!(
            "unable to find remote helper for '{}'",
            url.split("://").next().unwrap_or(url)
        )));
    };
    let mut options = ConnectOptions::new(Service::UploadPack);
    options.program = upload_pack.borrow().clone();
    options.protocol_version = ProtocolVersion::V2;
    let conn = transport.connect(&options).map_err(|e| fatal(&e))?;
    let mut client = Client::connect(conn, "ls-remote").map_err(|e| fatal(&e))?;

    let mut ls = LsRefsArgs::standard();
    ls.server_options = server_options.borrow().clone();
    if branches.get() {
        ls.ref_prefixes.push("refs/heads/".into());
    }
    if tags.get() {
        ls.ref_prefixes.push("refs/tags/".into());
    }
    let refs: Vec<RemoteRef> = client.ls_refs(&ls).map_err(|e| fatal(&e))?;
    client.close().map_err(|e| fatal(&e))?;
    client.into_inner().close().map_err(|e| fatal(&e))?;

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
