// SPDX-License-Identifier: MIT
//! `git push` (Documentation/git-push.adoc, config/push.adoc): update
//! remote refs over the `receive-pack` protocol v0 and send the objects
//! they need.
//!
//! Implemented: `[<repository> [<refspec>...]]` with remote names and URLs
//! (local paths, `file://`, SSH, smart HTTP), `remote.<name>.push`,
//! `remote.<name>.pushurl`, `push.default` (all modes) and
//! `push.autoSetupRemote`, `-u`, `-f`/`+<refspec>`, `-d`/`:<dst>`,
//! `tag <tag>`, `--all`/`--branches`, `--tags`, `-n`, `-q`, `-v`,
//! `--porcelain`, `--atomic`, `-o`, `--receive-pack`; the status table,
//! hints, exit codes and the update of remote-tracking refs.
//!
//! Not yet: `--mirror`, `--prune`, `--force-with-lease`,
//! `--force-if-includes`, `--follow-tags`, `--signed`,
//! `--recurse-submodules`, thin packs, remote groups and local progress
//! output. The `pre-push` hook runs unless `--no-verify` is given (#76).

use std::io::IsTerminal;

use rgit_config::{ConfigSet, SetRequest, SystemEnv, ValuePattern};
use rgit_hash::ObjectId;
use rgit_odb::ObjectDatabase;
use rgit_protocol::push::{Command, Request};
use rgit_remote::{BranchConfig, Direction, PushDefault, Refspec, RefspecSet, Remote, Remotes};
use rgit_transport::{ConnectOptions, IpVersion, Service, Settings};

use crate::messages::push as msg;
use crate::prelude::*;
use crate::push::matching::{self, Local};
use crate::push::output::{self, Style, prettify};
use crate::push::send::{self, Outgoing};
use crate::push::{Objects, PushRef, Status, check_refs};
use crate::refs_support;
use crate::repo::{self, Mode};

fn fatal(e: impl std::fmt::Display) -> Die {
    Die::Fatal(e.to_string())
}

/// Options that matter after parsing.
struct Flags {
    verbose: bool,
    quiet: bool,
    porcelain: bool,
    dry_run: bool,
    force: bool,
    set_upstream: bool,
    atomic: bool,
    progress: bool,
    push_options: Vec<String>,
    receive_pack: Option<String>,
    ip: IpVersion,
    verify: bool,
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let verbose = Cell::new(0);
    let quiet = Cell::new(0);
    let repo_opt: RefCell<Option<String>> = RefCell::new(None);
    let all = Cell::new(false);
    let mirror = Cell::new(false);
    let delete = Cell::new(false);
    let tags = Cell::new(false);
    let dry_run = Cell::new(0);
    let porcelain = Cell::new(false);
    let force = Cell::new(0);
    let thin = Cell::new(true);
    let receive_pack: RefCell<Option<String>> = RefCell::new(None);
    let set_upstream = Cell::new(false);
    let progress = Cell::new(-1);
    let prune = Cell::new(false);
    let no_verify = Cell::new(false);
    let follow_tags = Cell::new(false);
    let atomic = Cell::new(false);
    let push_options: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let ipv4 = Cell::new(false);
    let ipv6 = Cell::new(false);
    let unsupported: RefCell<Option<String>> = RefCell::new(None);
    let reject = |name: &'static str| {
        let unsupported = &unsupported;
        move |_: Option<&str>, unset: bool| -> Result<(), CallbackError> {
            if !unset {
                unsupported
                    .borrow_mut()
                    .get_or_insert_with(|| format!("--{name}"));
            }
            Ok(())
        }
    };
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::verbose(&verbose, msg::H_VERBOSE),
            Opt::quiet(&quiet, msg::H_QUIET),
            Opt::string(None, "repo", &repo_opt, "repository", msg::H_REPO),
            Opt::flag(None, "all", &all, msg::H_ALL),
            Opt::alias(None, "branches", "all"),
            Opt::flag(None, "mirror", &mirror, msg::H_MIRROR),
            Opt::flag('d', "delete", &delete, msg::H_DELETE),
            Opt::flag(None, "tags", &tags, msg::H_TAGS),
            Opt::dry_run(&dry_run, msg::H_DRY_RUN),
            Opt::flag(None, "porcelain", &porcelain, msg::H_PORCELAIN),
            Opt::force(&force, msg::H_FORCE),
            Opt::callback(
                None,
                "force-with-lease",
                Some("<refname>:<expect>"),
                msg::H_FORCE_WITH_LEASE,
                reject("force-with-lease"),
            )
            .literal_argh()
            .optional(None),
            Opt::callback(
                None,
                "force-if-includes",
                None,
                msg::H_FORCE_IF_INCLUDES,
                reject("force-if-includes"),
            )
            .noarg(),
            Opt::callback(
                None,
                "recurse-submodules",
                Some("(check|on-demand|no)"),
                msg::H_RECURSE_SUBMODULES,
                reject("recurse-submodules"),
            )
            .literal_argh(),
            Opt::flag(None, "thin", &thin, msg::H_THIN),
            Opt::string(None, "receive-pack", &receive_pack, "receive-pack", msg::H_RECEIVE_PACK),
            Opt::string(None, "exec", &receive_pack, "receive-pack", msg::H_RECEIVE_PACK),
            Opt::flag('u', "set-upstream", &set_upstream, msg::H_SET_UPSTREAM),
            Opt::bool(None, "progress", &progress, msg::H_PROGRESS),
            Opt::flag(None, "prune", &prune, msg::H_PRUNE),
            Opt::flag(None, "no-verify", &no_verify, msg::H_NO_VERIFY).noneg(),
            Opt::callback(None, "verify", None, msg::H_VERIFY, |_, _| {
                no_verify.set(false);
                Ok(())
            })
            .noarg()
            .noneg(),
            Opt::flag(None, "follow-tags", &follow_tags, msg::H_FOLLOW_TAGS),
            Opt::callback(
                None,
                "signed",
                Some("(yes|no|if-asked)"),
                msg::H_SIGNED,
                reject("signed"),
            )
            .literal_argh()
            .optional(None),
            Opt::flag(None, "atomic", &atomic, msg::H_ATOMIC),
            Opt::string_list(
                'o',
                "push-option",
                &push_options,
                "server-specific",
                msg::H_PUSH_OPTION,
            ),
            Opt::flag('4', "ipv4", &ipv4, msg::H_IPV4).noneg(),
            Opt::flag('6', "ipv6", &ipv6, msg::H_IPV6).noneg(),
        ],
    );
    let args = parser.parse(argv)?.args;
    if let Some(opt) = unsupported.borrow().clone() {
        return Err(Die::Fatal(msg::not_supported(&opt)));
    }
    for (on, name) in [
        (mirror.get(), "--mirror"),
        (prune.get(), "--prune"),
        (follow_tags.get(), "--follow-tags"),
    ] {
        if on {
            return Err(Die::Fatal(msg::not_supported(name)));
        }
    }
    if tags.get() && all.get() {
        return Err(Die::Fatal(msg::TAGS_WITH_ALL.into()));
    }

    let setup = repo::setup(ctx, Mode::Required)?;
    let config = repo::config(ctx, &setup);
    let advisor = ctx.advisor(&config)?.without_color_ui();
    advisor.check_env().map_err(Die::Fatal)?;
    let hash = setup.format.object_format;
    let odb = repo::odb_with_config(&setup, &config)?;
    let refs = crate::refs::Refs::new(&setup);
    let head = refs.head_target();
    let branch = head
        .as_deref()
        .and_then(|h| h.strip_prefix("refs/heads/"))
        .map(|b| BranchConfig::from_config(&config, b.as_bytes()))
        .transpose()
        .map_err(fatal)?;
    let remotes = Remotes::from_config(&config).map_err(fatal)?;

    let (repo_arg, cli_specs): (Option<String>, &[String]) = match args.split_first() {
        Some((r, rest)) => (Some(r.clone()), rest),
        None => (repo_opt.borrow().clone(), &[]),
    };
    let remote_name = match &repo_arg {
        Some(r) => r.clone(),
        None => remotes
            .push_remote_name(
                branch.as_ref().and_then(|b| b.push_remote.as_ref()).map(|r| r.as_ref()),
                branch.as_ref().and_then(|b| b.remote.as_ref()).map(|r| r.as_ref()),
            )
            .to_string(),
    };
    let remote = remotes
        .get(remote_name.as_bytes(), Some(&setup.git_dir))
        .map_err(fatal)?;
    for w in &remote.warnings {
        eprintln!("warning: {w}");
    }
    if repo_arg.is_none() && !remote.is_configured() {
        return Err(Die::Fatal(msg::NO_DESTINATION.into()));
    }

    // The refspecs, by precedence: command line (and --all/--tags),
    // remote.<name>.push, push.default.
    let mut set_up = set_upstream.get();
    let mut specs: Vec<String> = Vec::new();
    let mut i = 0;
    while i < cli_specs.len() {
        let a = &cli_specs[i];
        if a == "tag" {
            let Some(t) = cli_specs.get(i + 1) else {
                return Err(Die::Fatal("tag shorthand without <tag>".into()));
            };
            specs.push(format!("refs/tags/{t}:refs/tags/{t}"));
            i += 2;
            continue;
        }
        if delete.get() {
            specs.push(format!(":{a}"));
        } else {
            specs.push(a.clone());
        }
        i += 1;
    }
    if delete.get() && specs.is_empty() {
        return Err(Die::Fatal(msg::DELETE_WITHOUT_REFS.into()));
    }
    if all.get() {
        if !specs.is_empty() {
            return Err(Die::Fatal(msg::ALL_WITH_REFSPECS.into()));
        }
        specs.push("refs/heads/*".into());
    }
    if tags.get() {
        specs.push("refs/tags/*".into());
    }
    let from_cli = !specs.is_empty();
    let refspecs = if from_cli {
        let mut set = RefspecSet::default();
        for s in &specs {
            let r = Refspec::parse_with_hash(s, Direction::Push, hash)
                .map_err(|_| Die::Fatal(msg::invalid_refspec(s)))?;
            set.items.push(r);
        }
        set
    } else if !remote.push.is_empty() {
        remote.push.clone()
    } else {
        let auto = config
            .bool("push.autosetupremote")
            .ok()
            .flatten()
            .unwrap_or(false);
        let mode = PushDefault::from_config(&config).map_err(fatal)?;
        let spec = default_refspec(
            mode,
            &remotes,
            &remote_name,
            branch.as_ref(),
            auto,
            &mut set_up,
        )?;
        RefspecSet::parse([spec], Direction::Push).map_err(fatal)?
    };

    let flags = Flags {
        verbose: verbose.get() > 0,
        quiet: quiet.get() > 0,
        porcelain: porcelain.get(),
        dry_run: dry_run.get() > 0,
        force: force.get() > 0,
        set_upstream: set_up,
        atomic: atomic.get(),
        progress: match progress.get() {
            -1 => quiet.get() == 0 && std::io::stderr().is_terminal(),
            p => p > 0,
        },
        push_options: push_options.borrow().clone(),
        receive_pack: receive_pack
            .borrow()
            .clone()
            .or_else(|| remote.receive_pack.as_ref().map(|r| r.to_string())),
        ip: if ipv4.get() {
            IpVersion::V4
        } else if ipv6.get() {
            IpVersion::V6
        } else {
            IpVersion::Any
        },
        verify: !no_verify.get(),
    };
    let push = Push {
        ctx,
        setup: &setup,
        config: &config,
        advisor: &advisor,
        odb: &odb,
        refs: &refs,
        head: head.as_deref(),
        remote: &remote,
        refspecs: &refspecs,
        remote_push: (from_cli && !remote.push.is_empty()).then_some(&remote.push),
        flags: &flags,
    };
    let mut rc = 0;
    for url in &remote.push_url {
        rc = rc.max(push.to_url(&url.to_string())?);
    }
    Ok(rc)
}

/// The refspec `push.default` stands for when nothing else says what to
/// push.
fn default_refspec(
    mode: PushDefault,
    remotes: &Remotes,
    remote: &str,
    branch: Option<&BranchConfig>,
    auto_setup: bool,
    set_upstream: &mut bool,
) -> Result<String, Die> {
    if mode == PushDefault::Nothing {
        return Err(Die::Fatal(msg::PUSH_DEFAULT_NOTHING.into()));
    }
    if mode == PushDefault::Matching {
        return Ok(":".into());
    }
    let Some(b) = branch else {
        return Err(Die::Fatal(msg::detached(remote)));
    };
    let name = b.name.to_string();
    let current = format!("refs/heads/{name}:refs/heads/{name}");
    if mode == PushDefault::Current {
        return Ok(current);
    }
    let fetch_remote = remotes.default_remote_name(b.remote.as_ref().map(|r| r.as_ref()));
    let triangular = fetch_remote != remote;
    if triangular {
        return if mode == PushDefault::Upstream {
            Err(Die::Fatal(msg::not_upstream_remote(remote, &name)))
        } else {
            Ok(current)
        };
    }
    if b.merge.is_empty() || b.remote.is_none() {
        if auto_setup {
            *set_upstream = true;
            return Ok(current);
        }
        return Err(Die::Fatal(msg::no_upstream(&name, remote)));
    }
    if b.merge.len() > 1 {
        return Err(Die::Fatal(msg::multiple_upstreams(&name)));
    }
    let merge = b.merge[0].to_string();
    if mode == PushDefault::Simple && merge != format!("refs/heads/{name}") {
        let short = merge.strip_prefix("refs/heads/").unwrap_or(&merge);
        return Err(Die::Fatal(msg::upstream_mismatch(remote, short)));
    }
    Ok(format!("refs/heads/{name}:{merge}"))
}

/// `scheme://user:pass@host/...` → `scheme://host/...` for messages.
fn anonymize(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let end = rest.find('/').unwrap_or(rest.len());
    match rest[..end].rfind('@') {
        Some(at) => format!("{scheme}://{}", &rest[at + 1..]),
        None => url.to_owned(),
    }
}

struct Push<'a> {
    ctx: &'a Context,
    setup: &'a rgit_repo::Setup,
    config: &'a ConfigSet,
    advisor: &'a rgit_messages::Advisor,
    odb: &'a ObjectDatabase,
    refs: &'a crate::refs::Refs,
    head: Option<&'a str>,
    remote: &'a Remote,
    refspecs: &'a RefspecSet,
    remote_push: Option<&'a RefspecSet>,
    flags: &'a Flags,
}

impl Push<'_> {
    /// Push advice is shown without the "Disable this message" line.
    fn hint(&self, advice: Advice, text: &str) -> Result<(), Die> {
        self.advisor
            .advise_without_footer(advice, text)
            .map_err(Die::Fatal)
    }

    fn to_url(&self, url: &str) -> Result<i32, Die> {
        let f = self.flags;
        let hash = self.setup.format.object_format;
        let display = anonymize(url);
        if f.verbose {
            eprintln!("{}", msg::pushing_to(&display));
        }
        let env = SystemEnv::default();
        let settings = Settings {
            env: &env,
            config: self.config,
            ip: f.ip,
            base_env: self.ctx.child_env(),
        };
        let Some(mut transport) = rgit_http::transport_for(url, &settings).map_err(fatal)? else {
            return Err(Die::Fatal(msg::transport_not_supported(url)));
        };
        let mut opts = ConnectOptions::new(Service::ReceivePack);
        opts.program = f.receive_pack.clone();
        let conn = transport.connect(&opts).map_err(fatal)?;
        let mut session =
            send::start(conn, hash).map_err(|e| Die::Fatal(send::advertisement_error(&e)))?;
        if f.atomic && !session.adv.caps.contains("atomic") {
            return Err(Die::Fatal(msg::NO_ATOMIC.into()));
        }
        if !f.push_options.is_empty() && !session.adv.caps.contains("push-options") {
            return Err(Die::Fatal(msg::NO_PUSH_OPTIONS.into()));
        }

        // Match.
        let objects = Objects {
            odb: self.odb,
            hash,
        };
        let mut local_refs = Vec::new();
        if let Some(h) = self.refs.resolve("HEAD") {
            local_refs.push(("HEAD".to_string(), h));
        }
        local_refs.extend(self.refs.list("refs/"));
        let revs = refs_support::Revs::new(self.ctx, self.setup)?;
        let resolve = |s: &str| revs.resolve(s);
        let kind = |id: &ObjectId| objects.kind(id);
        let local = Local {
            refs: &local_refs,
            head: self.head,
            resolve: &resolve,
            kind: &kind,
        };
        let (mut list, errors) = matching::match_refs(
            &local,
            &session.adv.refs,
            self.refspecs,
            self.remote_push,
            f.force,
        );
        if !errors.is_empty() {
            for e in &errors {
                eprintln!("error: {}", e.error);
                if let Some(h) = &e.hint {
                    self.hint(Advice::PushUnqualifiedRefname, h)?;
                }
            }
            eprintln!("error: {}", msg::failed_to_push(&display));
            send::finish_empty(session);
            return Ok(1);
        }

        check_refs(&mut list, &objects);
        if f.atomic {
            if let Some(bad) = list.iter().find(|r| r.status.is_error()) {
                eprintln!(
                    "error: {}",
                    msg::atomic_failed(&bad.dst, bad.status.code())
                );
                for r in list.iter_mut().filter(|r| r.status == Status::None) {
                    r.status = Status::AtomicFailed;
                }
            }
        }

        let null = ObjectId::null(hash);
        if f.verify && !self.pre_push(url, &list, null)? {
            eprintln!("error: {}", msg::failed_to_push(&display));
            send::finish_empty(session);
            return Ok(1);
        }

        // Send.
        let commands: Vec<Command> = list
            .iter()
            .filter(|r| r.status == Status::None)
            .map(|r| Command {
                old: r.old.unwrap_or(null),
                new: r.new.unwrap_or(null),
                name: r.dst.clone(),
            })
            .collect();
        if f.dry_run || commands.is_empty() {
            for r in list.iter_mut().filter(|r| r.status == Status::None) {
                r.status = Status::Ok;
            }
            send::finish_empty(session);
        } else {
            let request = Request {
                quiet: !f.progress,
                atomic: f.atomic,
                push_options: f.push_options.clone(),
                push_options_requested: false,
            };
            let mut out = Outgoing {
                commands,
                request,
                odb: self.odb,
                hash,
                objects: Vec::new(),
            };
            out.objects = match send::pack_objects(&out, &session.adv) {
                Ok(o) => o,
                Err(e) => {
                    send::finish_empty(session);
                    return Err(Die::Fatal(e));
                }
            };
            let result = send::exchange(&mut session, &out);
            let _ = session.conn.close();
            match result {
                Ok(Some(report)) => {
                    if let Some(e) = &report.unpack_error {
                        eprintln!("error: remote unpack failed: {e}");
                    }
                    for r in list.iter_mut().filter(|r| r.status == Status::None) {
                        match report.refs.iter().find(|x| x.name == r.dst) {
                            Some(x) if x.error.is_none() => r.status = Status::Ok,
                            Some(x) => {
                                r.status = Status::RemoteReject;
                                r.remote_msg = x.error.clone().filter(|m| !m.is_empty());
                            }
                            None => r.status = Status::RemoteFailure,
                        }
                    }
                }
                Ok(None) => {
                    for r in list.iter_mut().filter(|r| r.status == Status::None) {
                        r.status = Status::Ok;
                    }
                }
                Err(e) => {
                    eprintln!("fatal: {e}");
                    for r in list.iter_mut().filter(|r| r.status == Status::None) {
                        r.status = Status::RemoteFailure;
                    }
                }
            }
        }

        // Report.
        let abbrev = |id: &ObjectId| objects.abbrev(id);
        let style = Style {
            url: &display,
            verbose: f.verbose,
            quiet: f.quiet,
            porcelain: f.porcelain,
            abbrev: &abbrev,
        };
        let failed = output::print(&list, &style);
        if !f.dry_run {
            self.update_tracking(&list)?;
        }
        if !failed
            && !f.porcelain
            && !f.quiet
            && list.iter().all(|r| r.status == Status::UpToDate)
        {
            eprintln!("{}", msg::EVERYTHING_UP_TO_DATE);
        }
        if f.set_upstream {
            self.set_upstreams(&list)?;
        }
        if failed {
            eprintln!("error: {}", msg::failed_to_push(&display));
            self.rejection_hints(&list)?;
            return Ok(1);
        }
        Ok(0)
    }

    /// The `pre-push` hook (githooks(5)): the remote's name (the URL for
    /// a remote given as a URL) and URL as arguments, one line per ref to
    /// be updated on its input. False when it refuses the push.
    fn pre_push(&self, url: &str, list: &[PushRef], null: ObjectId) -> Result<bool, Die> {
        let hooks = crate::hooks::for_repo(self.ctx, self.setup, self.config)?;
        let mut input = String::new();
        for r in list.iter().filter(|r| r.status == Status::None) {
            let src = match (&r.new, &r.src) {
                (None, _) => "(delete)",
                (Some(_), Some(s)) => s.as_str(),
                (Some(_), None) => r.dst.as_str(),
            };
            input.push_str(&format!(
                "{src} {} {} {}\n",
                r.new.unwrap_or(null).to_hex(),
                r.dst,
                r.old.unwrap_or(null).to_hex()
            ));
        }
        let name = if self.remote.is_configured() {
            self.remote.name.to_string()
        } else {
            url.to_owned()
        };
        let mut opts = rgit_hook::RunOptions::with_args([name, url.to_owned()])
            .stdin(rgit_hook::Input::Data(input.as_bytes()));
        opts.separate_stdout = true;
        Ok(crate::hooks::run(&hooks, "pre-push", &opts)?.ok())
    }

    fn rejection_hints(&self, list: &[PushRef]) -> Result<(), Die> {
        let has = |s: Status| list.iter().any(|r| r.status == s);
        let nonff_current = list
            .iter()
            .any(|r| r.status == Status::RejectNonFf && Some(r.dst.as_str()) == self.head);
        if nonff_current {
            self.hint(Advice::PushNonFfCurrent, msg::HINT_NON_FF_CURRENT)
        } else if has(Status::RejectNonFf) {
            self.hint(Advice::PushNonFfMatching, msg::HINT_NON_FF_MATCHING)
        } else if has(Status::RejectAlreadyExists) {
            self.hint(Advice::PushAlreadyExists, msg::HINT_ALREADY_EXISTS)
        } else if has(Status::RejectFetchFirst) {
            self.hint(Advice::PushFetchFirst, msg::HINT_FETCH_FIRST)
        } else if has(Status::RejectNeedsForce) {
            self.hint(Advice::PushNeedsForce, msg::HINT_NEEDS_FORCE)
        } else {
            Ok(())
        }
    }

    /// Remote-tracking refs of a configured remote follow what the server
    /// now has.
    fn update_tracking(&self, list: &[PushRef]) -> Result<(), Die> {
        if !self.remote.is_configured() {
            return Ok(());
        }
        let store = refs_support::ref_store(self.setup, self.config);
        let objects = refs_support::Objects::open(self.setup);
        let mut hooks = refs_support::UpdateHooks::new(self.setup, self.config, &objects);
        let hash = self.setup.format.object_format;
        for r in list
            .iter()
            .filter(|r| matches!(r.status, Status::Ok | Status::UpToDate))
        {
            let Some(tracking) = self.remote.fetch.map_src(r.dst.as_bytes()) else {
                continue;
            };
            let tracking = tracking.to_string();
            if self.flags.verbose {
                eprintln!("{}", msg::updating_tracking(&tracking));
            }
            let update = match r.new {
                Some(new) => rgit_refs::RefUpdate::update(&tracking, new, None),
                None => {
                    if self.refs.resolve(&tracking).is_none() {
                        continue;
                    }
                    rgit_refs::RefUpdate::delete(&tracking, hash, None)
                }
            }
            .message(msg::REFLOG_MESSAGE);
            let mut tx = rgit_refs::Transaction::new(&store);
            if let Err(e) = tx.push(update).and_then(|()| tx.commit(&mut hooks)) {
                eprintln!("error: {e}");
            }
        }
        Ok(())
    }

    /// `-u`: make every pushed branch track its destination.
    fn set_upstreams(&self, list: &[PushRef]) -> Result<(), Die> {
        let remote = self.remote.name.to_string();
        let path = self.setup.common_dir.join("config");
        for r in list
            .iter()
            .filter(|r| matches!(r.status, Status::Ok | Status::UpToDate) && r.new.is_some())
        {
            let src = match r.src.as_deref() {
                Some("HEAD") => self.head,
                other => other,
            };
            let (Some(branch), Some(dst)) = (
                src.and_then(|s| s.strip_prefix("refs/heads/")),
                r.dst.strip_prefix("refs/heads/"),
            ) else {
                continue;
            };
            if self.flags.dry_run {
                println!("{}", msg::would_set_upstream(branch, dst, &remote));
                continue;
            }
            for (key, value) in [
                (format!("branch.{branch}.remote"), remote.as_str()),
                (format!("branch.{branch}.merge"), r.dst.as_str()),
            ] {
                let req = SetRequest {
                    key: key.as_bytes(),
                    value: Some(value.as_bytes()),
                    pattern: &ValuePattern::Any,
                    all: false,
                    comment: None,
                };
                rgit_config::write::set_in_file(&path, &rgit_fs::LockOptions::default(), &req)
                    .map_err(fatal)?;
            }
            println!("{}", msg::upstream_set(branch, &remote, prettify(&r.dst)));
        }
        Ok(())
    }
}
