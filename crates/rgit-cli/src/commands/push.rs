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
//! `--prune` deletes remote refs a pattern refspec covers whose local
//! source is gone; `--follow-tags` (`push.followTags`) adds the annotated
//! tags missing on the remote whose commit is in the pushed history.
//!
//! `--force-with-lease[=<refname>[:<expect>]]` forces refs whose remote
//! value is the expected one and rejects the others as `stale info`;
//! `--force-if-includes` (`push.useForceIfIncludes`) adds the reflog check.
//!
//! A remote group (`remotes.<group>`) pushes to each member in turn.
//!
//! Not yet: `--signed`, `--recurse-submodules`, thin packs and local
//! progress output. The `pre-push` hook runs unless `--no-verify` is given
//! (#76).

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
    /// `git send-pack`: no hints, no `failed to push` line.
    send_pack: bool,
    verbose: bool,
    /// The number of `-v` (smart HTTP reports POST sizes above 1).
    verbosity: i32,
    quiet: bool,
    porcelain: bool,
    dry_run: bool,
    force: bool,
    set_upstream: bool,
    atomic: bool,
    progress: bool,
    thin: bool,
    push_options: Vec<String>,
    receive_pack: Option<String>,
    ip: IpVersion,
    verify: bool,
    prune: bool,
    follow_tags: bool,
    /// `--force-with-lease=<refname>[:<expect>]`, in command-line order
    /// (the first entry naming a ref wins).
    leases: Vec<(String, Expect)>,
    /// `--force-with-lease` without a value: every ref not named above.
    lease_all: bool,
    /// `--force-if-includes` / `push.useForceIfIncludes`.
    if_includes: bool,
}

/// `--force-with-lease` as given.
#[derive(Default)]
struct LeaseArgs {
    all: bool,
    refs: Vec<(String, Option<String>)>,
}

/// What a lease expects of a remote ref.
#[derive(Clone, Copy)]
enum Expect {
    /// The value of our remote-tracking ref for it (absent: none).
    Tracking,
    /// This value (`None`: the ref must not exist).
    Value(Option<ObjectId>),
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
    let follow_tags = Cell::new(-1);
    let atomic = Cell::new(false);
    let push_options: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let ipv4 = Cell::new(false);
    let ipv6 = Cell::new(false);
    let leases: RefCell<LeaseArgs> = RefCell::new(LeaseArgs::default());
    let if_includes = Cell::new(-1);
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
                |v, unset| {
                    let mut l = leases.borrow_mut();
                    match (unset, v) {
                        (true, _) => *l = LeaseArgs::default(),
                        (false, None) => l.all = true,
                        (false, Some(v)) => l.refs.push(match v.split_once(':') {
                            Some((r, e)) => (r.to_owned(), Some(e.to_owned())),
                            None => (v.to_owned(), None),
                        }),
                    }
                    Ok(())
                },
            )
            .literal_argh()
            .optional(None),
            Opt::bool(None, "force-if-includes", &if_includes, msg::H_FORCE_IF_INCLUDES),
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
            Opt::bool(None, "follow-tags", &follow_tags, msg::H_FOLLOW_TAGS),
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
    if tags.get() && all.get() {
        return Err(Die::Fatal(msg::TAGS_WITH_ALL.into()));
    }

    let setup = repo::setup(ctx, Mode::Required)?;
    let config = repo::config(ctx, &setup);
    rgit_protocol::sideband::set_remote_colors(crate::color::remote_colors(&config));
    crate::color::set_sideband_controls(&config, None);
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
    for w in remotes.warnings() {
        eprintln!("warning: {w}");
    }

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
    // A remote group (`remotes.<group>`): each member in turn, like
    // `git push <member> ...`; a failing member does not stop the others.
    if let (Some(name), false) = (&repo_arg, remote.is_configured()) {
        if let Some(members) = remote_group(&config, name) {
            if atomic.get() {
                return Err(Die::Fatal(msg::ATOMIC_ONE_REMOTE.into()));
            }
            let pos = argv
                .iter()
                .skip(1)
                .position(|a| a == name)
                .map(|p| p + 1);
            let mut code = 0;
            for m in &members {
                if quiet.get() == 0 {
                    println!("{}", msg::pushing_to(m));
                }
                let mut child = argv.to_vec();
                match pos {
                    Some(p) => child[p] = m.clone(),
                    None => child.push(m.clone()),
                }
                let rc = run(ctx, &child).unwrap_or_else(Die::emit);
                if rc != 0 {
                    eprintln!("error: {}", msg::could_not_push_to(m));
                    code = 1;
                }
            }
            return Ok(code);
        }
    }
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
            if delete.get() {
                specs.push(format!(":refs/tags/{t}"));
            } else {
                specs.push(format!("refs/tags/{t}:refs/tags/{t}"));
            }
            i += 2;
            continue;
        }
        if delete.get() && (a.is_empty() || a.contains(':')) {
            return Err(Die::Fatal(msg::DELETE_EMPTY.into()));
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
    // `--mirror` (or `remote.<name>.mirror`): every ref, forced, and the
    // remote refs without a local one are deleted.
    let mirror_mode = mirror.get() || (remote.mirror && !all.get() && !tags.get());
    if mirror_mode {
        if all.get() {
            return Err(Die::Fatal(msg::incompatible("--all/--branches", "--mirror")));
        }
        if tags.get() {
            return Err(Die::Fatal(msg::incompatible("--tags", "--mirror")));
        }
        if !specs.is_empty() {
            return Err(Die::Fatal(msg::MIRROR_WITH_REFSPECS.into()));
        }
        specs.push("+refs/*:refs/*".into());
        prune.set(true);
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

    let lease_args = leases.take();
    let mut lease_list = Vec::new();
    if !lease_args.refs.is_empty() {
        let revs = refs_support::Revs::new(ctx, &setup)?;
        for (name, expect) in lease_args.refs {
            let expect = match expect {
                None => Expect::Tracking,
                Some(e) if e.is_empty() => Expect::Value(None),
                Some(e) => match revs.resolve(&e) {
                    Some(oid) => Expect::Value(Some(oid)),
                    None => return Err(Die::Error(msg::cannot_parse_expected(&e), 129)),
                },
            };
            lease_list.push((name, expect));
        }
    }
    let flags = Flags {
        send_pack: false,
        leases: lease_list,
        lease_all: lease_args.all,
        if_includes: match if_includes.get() {
            -1 => config
                .bool("push.useforceifincludes")
                .ok()
                .flatten()
                .unwrap_or(false),
            v => v > 0,
        },
        verbose: verbose.get() > 0,
        verbosity: verbose.get(),
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
        thin: thin.get(),
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
        prune: prune.get(),
        follow_tags: match follow_tags.get() {
            -1 => config
                .bool("push.followtags")
                .ok()
                .flatten()
                .unwrap_or(false),
            v => v > 0,
        },
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

/// The members of the remote group `remotes.<name>`, if there is one.
fn remote_group(config: &ConfigSet, name: &str) -> Option<Vec<String>> {
    let key = format!("remotes.{name}");
    let mut found = false;
    let mut out = Vec::new();
    for e in config.get_all(key.as_bytes()) {
        found = true;
        if let Some(v) = &e.value {
            out.extend(
                String::from_utf8_lossy(v)
                    .split_ascii_whitespace()
                    .map(str::to_owned),
            );
        }
    }
    found.then_some(out)
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

/// `--follow-tags`: annotated tags missing on the remote whose commit is
/// in the history being pushed go along.
fn follow_tags(
    list: &mut Vec<PushRef>,
    local_refs: &[(String, ObjectId)],
    remote: &[rgit_protocol::push::AdvertisedRef],
    odb: &ObjectDatabase,
    objects: &Objects<'_>,
) {
    let pushed: Vec<ObjectId> = list.iter().filter_map(|r| r.new).collect();
    if pushed.is_empty() {
        return;
    }
    for (name, oid) in local_refs {
        if !name.starts_with("refs/tags/")
            || objects.kind(oid) != Some(rgit_object::ObjectKind::Tag)
            || remote.iter().any(|r| r.name == *name)
            || list.iter().any(|r| r.dst == *name)
        {
            continue;
        }
        let Some(target) = rgit_fetch::graph::peel(odb, oid) else {
            continue;
        };
        if !pushed
            .iter()
            .any(|tip| rgit_fetch::graph::is_ancestor(odb, &target, tip))
        {
            continue;
        }
        list.push(PushRef {
            dst: name.clone(),
            old: None,
            new: Some(*oid),
            src: Some(name.clone()),
            force: false,
            status: Status::None,
            forced_update: false,
            remote_msg: None,
        });
    }
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
        if f.verbose && !f.send_pack {
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
        rgit_remote::check_allowed(
            rgit_fetch::protocol_name(url).as_bytes(),
            self.config,
            &env,
        )
        .map_err(fatal)?;
        let mut opts = ConnectOptions::new(Service::ReceivePack);
        opts.program = f.receive_pack.clone();
        // There is no protocol v2 push: asking for 2 asks for nothing.
        opts.protocol_version = match rgit_fetch::protocol_version(self.config, &env)
            .map_err(fatal)?
        {
            rgit_protocol::ProtocolVersion::V1 => rgit_protocol::ProtocolVersion::V1,
            _ => rgit_protocol::ProtocolVersion::V0,
        };
        opts.trace_name = Some("push".into());
        opts.verbosity = f.verbosity;
        rgit_protocol::session_id::set_advertise(
            self.config
                .bool("transfer.advertiseSID")
                .ok()
                .flatten()
                .unwrap_or(false),
        );
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
        let upstream = |branch: &str| {
            let short = branch.strip_prefix("refs/heads/")?;
            let bc = BranchConfig::from_config(self.config, short.as_bytes()).ok()?;
            bc.merge.first().map(|m| m.to_string())
        };
        let upstream_mode = PushDefault::from_config(self.config).ok() == Some(PushDefault::Upstream);
        let local = Local {
            refs: &local_refs,
            head: self.head,
            resolve: &resolve,
            kind: &kind,
            upstream: upstream_mode.then_some(&upstream as &matching::Upstream<'_>),
        };
        let (mut list, mut errors) = matching::match_refs(
            &local,
            &session.adv.refs,
            self.refspecs,
            self.remote_push,
            f.force,
            f.prune,
        );
        // send-pack leaves several updates of one ref to the server.
        if f.send_pack {
            errors.retain(|e| !list.iter().any(|r| e.error == msg::dst_multiple_src(&r.dst)));
        }
        if f.follow_tags {
            follow_tags(&mut list, &local_refs, &session.adv.refs, self.odb, &objects);
        }
        if !errors.is_empty() {
            for e in &errors {
                eprintln!("error: {}", e.error);
                if let Some(h) = &e.hint {
                    self.hint(Advice::PushUnqualifiedRefname, h)?;
                }
            }
            send::finish_empty(session);
            if f.send_pack {
                eprintln!("fatal: {}", msg::REMOTE_HUNG_UP);
                return Ok(255);
            }
            eprintln!("error: {}", msg::failed_to_push(&display));
            return Ok(1);
        }

        self.apply_leases(&mut list);
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
                shallow: rgit_fetch::shallow::read(
                    &rgit_fetch::shallow::file_for(&self.setup.common_dir),
                    hash,
                ),
                bases: Vec::new(),
                thin: f.thin,
                pack: crate::commands::pack_objects::config_options(self.config)?,
            };
            (out.objects, out.bases) = match send::pack_objects(&out, &session.adv) {
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
                    // A report line answers the first command for its ref
                    // (several updates of one ref: the later ones get none).
                    let mut answered: Vec<String> = Vec::new();
                    for r in list.iter_mut().filter(|r| r.status == Status::None) {
                        let first = !answered.contains(&r.dst);
                        answered.push(r.dst.clone());
                        let line = report.refs.iter().find(|x| x.name == r.dst);
                        match line.filter(|_| first) {
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
            if !f.send_pack {
                eprintln!("error: {}", msg::failed_to_push(&display));
                self.rejection_hints(&list)?;
            }
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

    /// `--force-with-lease`: a ref under a lease is forced when the server
    /// still has the expected value and rejected as stale otherwise;
    /// `--force-if-includes` also wants the remote-tracking tip a lease
    /// takes as expectation to be part of the local ref's history or of
    /// its reflog. `--force` and `+<refspec>` override leases, and refs
    /// that are up to date stay so.
    fn apply_leases(&self, list: &mut [PushRef]) {
        let f = self.flags;
        if f.leases.is_empty() && !f.lease_all {
            return;
        }
        for r in list.iter_mut() {
            if r.status != Status::None || (r.old.is_some() && r.old == r.new) {
                continue;
            }
            let named = f.leases.iter().find(|(name, _)| {
                !name.is_empty()
                    && rgit_remote::refspec::refname_match(name.as_bytes(), r.dst.as_bytes())
            });
            let expect = match named {
                Some((_, e)) => *e,
                None if f.lease_all => Expect::Tracking,
                None => continue,
            };
            // Forcing a ref under a lease is reported as a forced update
            // even when it fast-forwards.
            if r.force {
                r.forced_update = true;
                continue;
            }
            let (expected, tracking) = match expect {
                Expect::Value(v) => (v, false),
                Expect::Tracking => (self.tracking_value(&r.dst), true),
            };
            if r.old != expected {
                r.status = Status::RejectStale;
                continue;
            }
            if tracking && f.if_includes {
                if let (Some(tip), Some(new)) = (expected, r.new) {
                    if !self.integrated(r.src.as_deref(), new, &tip) {
                        r.status = Status::RejectRemoteUpdated;
                        continue;
                    }
                }
            }
            r.force = true;
        }
    }

    /// Our remote-tracking ref's value for the remote ref `dst`.
    fn tracking_value(&self, dst: &str) -> Option<ObjectId> {
        let tracking = self.remote.fetch.map_src(dst.as_bytes())?;
        self.refs.resolve(&tracking.to_string())
    }

    /// Whether `tip` is reachable from `new` or from an entry of the
    /// reflog of the local ref `src` stands for.
    fn integrated(&self, src: Option<&str>, new: ObjectId, tip: &ObjectId) -> bool {
        if rgit_fetch::graph::is_ancestor(self.odb, tip, &new) {
            return true;
        }
        let local = match src {
            Some("HEAD") => self.head,
            other => other,
        };
        let Some(entries) = local.and_then(|l| self.refs.reflog(l)) else {
            return false;
        };
        entries
            .iter()
            .rev()
            .any(|e| rgit_fetch::graph::is_ancestor(self.odb, tip, e))
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
        } else if has(Status::RejectRemoteUpdated) {
            self.hint(Advice::PushRefNeedsUpdate, msg::HINT_REF_NEEDS_UPDATE)
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

/// `git send-pack` (Documentation/git-send-pack.adoc): the named refs (or
/// the matching branches, `--all` branches, `--mirror` everything) to a
/// repository given as a URL or path, with push's matching, checks and
/// status table, without remote configuration, hooks, hints or
/// remote-tracking updates.
pub fn run_send_pack(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let verbose = Cell::new(0);
    let quiet = Cell::new(0);
    let receive_pack: RefCell<Option<String>> = RefCell::new(None);
    let remote_opt: RefCell<Option<String>> = RefCell::new(None);
    let all = Cell::new(false);
    let dry_run = Cell::new(0);
    let mirror = Cell::new(false);
    let force = Cell::new(0);
    let progress = Cell::new(-1);
    let thin = Cell::new(true);
    let atomic = Cell::new(false);
    let stdin = Cell::new(false);
    let push_options: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let leases: RefCell<LeaseArgs> = RefCell::new(LeaseArgs::default());
    let if_includes = Cell::new(-1);
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
        msg::SEND_PACK_USAGE,
        vec![
            Opt::verbose(&verbose, msg::H_VERBOSE),
            Opt::quiet(&quiet, msg::H_QUIET),
            Opt::string(None, "receive-pack", &receive_pack, "receive-pack", msg::H_RECEIVE_PACK),
            Opt::string(None, "exec", &receive_pack, "receive-pack", msg::H_RECEIVE_PACK),
            Opt::string(None, "remote", &remote_opt, "remote", msg::H_SP_REMOTE),
            Opt::flag(None, "all", &all, msg::H_SP_ALL),
            Opt::dry_run(&dry_run, msg::H_DRY_RUN),
            Opt::flag(None, "mirror", &mirror, msg::H_MIRROR),
            Opt::force(&force, msg::H_FORCE),
            Opt::callback(
                None,
                "signed",
                Some("(yes|no|if-asked)"),
                msg::H_SIGNED,
                reject("signed"),
            )
            .literal_argh()
            .optional(None),
            Opt::string_list(
                None,
                "push-option",
                &push_options,
                "server-specific",
                msg::H_PUSH_OPTION,
            ),
            Opt::bool(None, "progress", &progress, msg::H_PROGRESS),
            Opt::flag(None, "thin", &thin, msg::H_THIN),
            Opt::flag(None, "atomic", &atomic, msg::H_ATOMIC),
            Opt::callback(
                None,
                "stateless-rpc",
                None,
                msg::H_SP_STATELESS_RPC,
                reject("stateless-rpc"),
            )
            .noarg(),
            Opt::flag(None, "stdin", &stdin, msg::H_SP_STDIN),
            Opt::callback(
                None,
                "helper-status",
                None,
                msg::H_SP_HELPER_STATUS,
                reject("helper-status"),
            )
            .noarg(),
            Opt::callback(
                None,
                "force-with-lease",
                Some("<refname>:<expect>"),
                msg::H_FORCE_WITH_LEASE,
                |v, unset| {
                    let mut l = leases.borrow_mut();
                    match (unset, v) {
                        (true, _) => *l = LeaseArgs::default(),
                        (false, None) => l.all = true,
                        (false, Some(v)) => l.refs.push(match v.split_once(':') {
                            Some((r, e)) => (r.to_owned(), Some(e.to_owned())),
                            None => (v.to_owned(), None),
                        }),
                    }
                    Ok(())
                },
            )
            .literal_argh()
            .optional(None),
            Opt::bool(None, "force-if-includes", &if_includes, msg::H_FORCE_IF_INCLUDES),
        ],
    );
    let args = parser.parse(argv)?.args;
    if let Some(opt) = unsupported.borrow().clone() {
        return Err(Die::Fatal(msg::not_supported(&opt)));
    }
    let Some((dest, cli_refs)) = args.split_first() else {
        return Err(parser.usage_error().into());
    };
    let mut specs: Vec<String> = cli_refs.to_vec();
    if stdin.get() {
        for line in std::io::stdin().lines() {
            let line = line.map_err(fatal)?;
            let line = line.trim();
            if !line.is_empty() {
                specs.push(line.to_owned());
            }
        }
    }
    if (all.get() || mirror.get()) && !specs.is_empty() {
        return Err(parser.usage_error().into());
    }

    let setup = repo::setup(ctx, Mode::Required)?;
    let config = repo::config(ctx, &setup);
    rgit_protocol::sideband::set_remote_colors(crate::color::remote_colors(&config));
    let advisor = ctx.advisor(&config)?.without_color_ui();
    let hash = setup.format.object_format;
    let odb = repo::odb_with_config(&setup, &config)?;
    let refs = crate::refs::Refs::new(&setup);
    let head = refs.head_target();
    // The destination is a URL or path, never a configured remote.
    let remote = Remotes::from_config(&ConfigSet::default())
        .and_then(|r| r.get(dest.as_bytes(), None))
        .map_err(fatal)?;

    let specs = if mirror.get() {
        vec!["+refs/*:refs/*".to_owned()]
    } else if all.get() {
        vec!["refs/heads/*".to_owned()]
    } else if specs.is_empty() {
        vec![":".to_owned()]
    } else {
        specs
    };
    let mut refspecs = RefspecSet::default();
    for s in &specs {
        let r = Refspec::parse_with_hash(s, Direction::Push, hash)
            .map_err(|_| Die::Fatal(msg::invalid_refspec(s)))?;
        refspecs.items.push(r);
    }
    let lease_args = leases.take();
    let mut lease_list = Vec::new();
    if !lease_args.refs.is_empty() {
        let revs = refs_support::Revs::new(ctx, &setup)?;
        for (name, expect) in lease_args.refs {
            let expect = match expect {
                None => Expect::Tracking,
                Some(e) if e.is_empty() => Expect::Value(None),
                Some(e) => match revs.resolve(&e) {
                    Some(oid) => Expect::Value(Some(oid)),
                    None => return Err(Die::Error(msg::cannot_parse_expected(&e), 129)),
                },
            };
            lease_list.push((name, expect));
        }
    }
    let flags = Flags {
        send_pack: true,
        verbose: verbose.get() > 0,
        verbosity: verbose.get(),
        thin: thin.get(),
        quiet: quiet.get() > 0,
        porcelain: false,
        dry_run: dry_run.get() > 0,
        force: force.get() > 0,
        set_upstream: false,
        atomic: atomic.get(),
        progress: match progress.get() {
            -1 => quiet.get() == 0 && std::io::stderr().is_terminal(),
            p => p > 0,
        },
        push_options: push_options.borrow().clone(),
        receive_pack: receive_pack.borrow().clone(),
        ip: IpVersion::Any,
        verify: false,
        prune: mirror.get(),
        follow_tags: false,
        leases: lease_list,
        lease_all: lease_args.all,
        if_includes: if_includes.get() > 0,
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
        remote_push: None,
        flags: &flags,
    };
    push.to_url(dest)
}
