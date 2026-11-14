// SPDX-License-Identifier: MIT
//! `git cherry-pick` (Documentation/git-cherry-pick.adoc) on the sequencer
//! ([`crate::sequencer`]); `git revert` shares the argument handling.

use std::io::IsTerminal as _;

use crate::messages::sequencer as msg;
use crate::prelude::*;
use crate::sequencer::{Action, Opts, Sequencer};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    run_action(ctx, argv, Action::Pick)
}

const QUIT: i32 = 1;
const CONTINUE: i32 = 2;
const ABORT: i32 = 3;
const SKIP: i32 = 4;

/// `cherry-pick` and `revert`.
pub(crate) fn run_action(ctx: &mut Context, argv: &[String], action: Action) -> CmdResult {
    let cmd = Cell::new(0);
    let cleanup = RefCell::new(None);
    let no_commit = Cell::new(-1);
    let edit = Cell::new(-1);
    let signoff = Cell::new(-1);
    let mainline: Cell<i64> = Cell::new(-1);
    let rerere = Cell::new(-1);
    let strategy = RefCell::new(None);
    let xopts = RefCell::new(Vec::new());
    let gpg_sign = Cell::new(false);
    let record_origin = Cell::new(-1);
    let ff = Cell::new(-1);
    let allow_empty = Cell::new(-1);
    let allow_empty_message = Cell::new(-1);
    let keep_redundant = Cell::new(-1);
    let empty: RefCell<Option<String>> = RefCell::new(None);
    let reference = Cell::new(-1);
    let mut opts = vec![
        Opt::cmdmode(None, "quit", &cmd, msg::HELP_QUIT, QUIT),
        Opt::cmdmode(None, "continue", &cmd, msg::HELP_CONTINUE, CONTINUE),
        Opt::cmdmode(None, "abort", &cmd, msg::HELP_ABORT, ABORT),
        Opt::cmdmode(None, "skip", &cmd, msg::HELP_SKIP, SKIP),
        Opt::string(None, "cleanup", &cleanup, "mode", msg::HELP_CLEANUP),
        Opt::bool('n', "no-commit", &no_commit, msg::HELP_NO_COMMIT),
        Opt::bool('e', "edit", &edit, msg::HELP_EDIT),
        Opt::bool('s', "signoff", &signoff, msg::HELP_SIGNOFF),
        Opt::callback(
            'm',
            "mainline",
            Some("parent-number"),
            msg::HELP_MAINLINE,
            |v, unset| {
                if unset {
                    mainline.set(0);
                    return Ok(());
                }
                match v.and_then(|v| v.parse::<i64>().ok()) {
                    Some(n) if n > 0 => {
                        mainline.set(n);
                        Ok(())
                    }
                    _ => Err(CallbackError::Error(msg::MAINLINE_NOT_POSITIVE.to_owned())),
                }
            },
        ),
        Opt::bool(None, "rerere-autoupdate", &rerere, msg::HELP_RERERE),
        Opt::string(None, "strategy", &strategy, "strategy", msg::HELP_STRATEGY),
        Opt::string_list('X', "strategy-option", &xopts, "option", msg::HELP_STRATEGY_OPTION),
        Opt::callback('S', "gpg-sign", Some("key-id"), msg::HELP_GPG_SIGN, |_, unset| {
            gpg_sign.set(!unset);
            Ok(())
        })
        .optional(None),
    ];
    match action {
        Action::Pick => {
            opts.extend([
                Opt::bool('x', None, &record_origin, msg::HELP_X).noneg(),
                Opt::noop('r', None),
                Opt::bool(None, "ff", &ff, msg::HELP_FF),
                Opt::bool(None, "allow-empty", &allow_empty, msg::HELP_ALLOW_EMPTY),
                Opt::bool(
                    None,
                    "allow-empty-message",
                    &allow_empty_message,
                    msg::HELP_ALLOW_EMPTY_MESSAGE,
                ),
                Opt::bool(
                    None,
                    "keep-redundant-commits",
                    &keep_redundant,
                    msg::HELP_KEEP_REDUNDANT,
                ),
                Opt::callback(None, "empty", Some("(stop|drop|keep)"), msg::HELP_EMPTY, |v, _| {
                    let v = v.unwrap_or_default();
                    if !matches!(v, "stop" | "drop" | "keep") {
                        return Err(CallbackError::Error(msg::invalid_empty(v)));
                    }
                    *empty.borrow_mut() = Some(v.to_owned());
                    Ok(())
                })
                .literal_argh()
                .noneg(),
            ]);
        }
        Action::Revert => {
            opts.push(Opt::bool(None, "reference", &reference, msg::HELP_REFERENCE));
        }
    }
    let usage = match action {
        Action::Pick => msg::CHERRY_PICK_USAGE,
        Action::Revert => msg::REVERT_USAGE,
    };
    let mut parser = Parser::new(usage, opts).flags(ParseFlags {
        keep_unknown_opt: true,
        ..ParseFlags::default()
    });
    let rest = parser.parse(argv)?.args;
    let usage_error = parser.usage_error();
    drop(parser);
    let name = action.name();

    let mode = cmd.get();
    if mode != 0 {
        let mode_name = match mode {
            QUIT => "--quit",
            CONTINUE => "--continue",
            ABORT => "--abort",
            _ => "--skip",
        };
        let given = [
            ("--no-commit", no_commit.get() == 1),
            ("--signoff", signoff.get() == 1),
            ("--mainline", mainline.get() > 0),
            ("--strategy", strategy.borrow().is_some()),
            ("--strategy-option", !xopts.borrow().is_empty()),
            ("-x", record_origin.get() == 1),
            ("--ff", ff.get() == 1),
            ("--rerere-autoupdate", rerere.get() == 1),
            ("--no-rerere-autoupdate", rerere.get() == 0),
            ("--allow-empty", allow_empty.get() == 1),
            ("--allow-empty-message", allow_empty_message.get() == 1),
            ("--keep-redundant-commits", keep_redundant.get() == 1),
            ("--empty", empty.borrow().is_some()),
        ];
        if let Some((opt, _)) = given.iter().find(|(_, set)| *set) {
            return Err(Die::Fatal(msg::incompatible(name, opt, mode_name)));
        }
        if !rest.is_empty() {
            return Err(Die::Output(usage_error));
        }
    } else if rest.is_empty() {
        return Err(Die::Output(usage_error));
    }

    if ff.get() == 1 {
        for (opt, set) in [
            ("--signoff", signoff.get() == 1),
            ("--no-commit", no_commit.get() == 1),
            ("-x", record_origin.get() == 1),
            ("--edit", edit.get() == 1),
        ] {
            if set {
                return Err(Die::Fatal(msg::incompatible(name, opt, "--ff")));
            }
        }
    }
    if gpg_sign.get() {
        return Err(Die::Fatal(msg::option_not_supported("-S/--gpg-sign")));
    }
    if let Some(c) = cleanup.borrow().as_deref()
        && rgit_commit::message::Cleanup::parse(c).is_none()
    {
        return Err(Die::Fatal(crate::messages::commit::invalid_cleanup(c)));
    }

    let mut o = Opts::new(action);
    o.no_commit = no_commit.get() == 1;
    o.edit = match edit.get() {
        -1 => None,
        v => Some(v == 1),
    };
    o.signoff = signoff.get() == 1;
    o.mainline = usize::try_from(mainline.get()).unwrap_or(0);
    o.strategy = strategy.borrow().clone();
    o.xopts = xopts.borrow().clone();
    o.record_origin = record_origin.get() == 1;
    o.allow_ff = ff.get() == 1;
    o.allow_empty = allow_empty.get() == 1;
    o.allow_empty_message = allow_empty_message.get() == 1;
    o.cleanup = cleanup.borrow().clone();
    match empty.borrow().as_deref() {
        Some("keep") => o.keep_redundant = true,
        Some("drop") => o.drop_redundant = true,
        _ => {}
    }
    if keep_redundant.get() == 1 {
        o.keep_redundant = true;
    }
    if o.keep_redundant {
        o.allow_empty = true;
    }

    let mut seq = Sequencer::new(ctx, o)?;
    if action == Action::Revert {
        let repo = crate::repository::Repo::open(seq.ctx, crate::repo::Mode::Required)?;
        seq.opts.reference = match reference.get() {
            -1 => repo
                .config
                .bool("revert.reference")
                .ok()
                .flatten()
                .unwrap_or(false),
            v => v == 1,
        };
    }
    if seq.opts.edit.is_none()
        && action == Action::Revert
        && mode == 0
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
    {
        seq.opts.edit = Some(true);
    }
    match mode {
        QUIT => return seq.quit(),
        CONTINUE => return seq.continue_(),
        ABORT => return seq.abort(),
        SKIP => return seq.skip(),
        _ => {}
    }

    // The commits: rev-list arguments, `-` for the previous branch.
    let rev_args: Vec<String> = rest
        .iter()
        .map(|a| if a == "-" { "@{-1}".to_owned() } else { a.clone() })
        .collect();
    let found = match crate::history::sequencer_commits(seq.ctx, &rev_args, action == Action::Pick) {
        Ok(f) => f,
        Err(Die::Fatal(m)) if m.starts_with("unrecognized argument: ") => {
            return Err(Die::Output(usage_error));
        }
        Err(e) => return Err(e),
    };
    if let Some((name_bytes, kind)) = found.other.first() {
        let kind = match kind {
            rgit_object::ObjectKind::Tree => "tree",
            rgit_object::ObjectKind::Blob => "blob",
            rgit_object::ObjectKind::Tag => "tag",
            rgit_object::ObjectKind::Commit => "commit",
        };
        let shown = String::from_utf8_lossy(name_bytes).into_owned();
        let shown = if shown.is_empty() {
            rest.iter().find(|a| !a.starts_with('-')).cloned().unwrap_or_default()
        } else {
            shown
        };
        eprintln!("error: {}", msg::cannot_pick_object(&shown, name, kind));
        return Err(Die::Fatal(msg::failed(name)));
    }
    if found.commits.is_empty() {
        eprintln!("error: {}", msg::EMPTY_SET);
        return Err(Die::Fatal(msg::failed(name)));
    }
    let revs: Vec<&String> = rest.iter().filter(|a| !a.starts_with('-') || *a == "-").collect();
    let single = revs.len() == 1
        && !revs[0].contains("..")
        && !revs[0].starts_with('^')
        && !revs[0].ends_with("^!")
        && !revs[0].ends_with("^@")
        && found.commits.len() == 1;
    seq.pick_revisions(&found.commits, single)
}
