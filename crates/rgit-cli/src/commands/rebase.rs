// SPDX-License-Identifier: MIT
//! `git rebase` (Documentation/git-rebase.adoc): the arguments; the work
//! is done by [`crate::rebase`] (merge back end, non-interactive, #154).

use crate::messages::rebase as msg;
use crate::prelude::*;
use crate::rebase::{self, Empty, Options, RebaseMerges};
use crate::signing::Sign;

const CONTINUE: i32 = 1;
const SKIP: i32 = 2;
const ABORT: i32 = 3;
const QUIT: i32 = 4;
const EDIT_TODO: i32 = 5;
const SHOW_CURRENT_PATCH: i32 = 6;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let onto: RefCell<Option<String>> = RefCell::new(None);
    let keep_base = Cell::new(-1);
    let no_verify = Cell::new(false);
    let quiet = Cell::new(0);
    let verbose = Cell::new(0);
    let stat = Cell::new(-1);
    let trailers: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let signoff = Cell::new(-1);
    let cdate = Cell::new(-1);
    let reset_date = Cell::new(-1);
    let apply_c: RefCell<Option<String>> = RefCell::new(None);
    let ignore_ws = Cell::new(-1);
    let whitespace: RefCell<Option<String>> = RefCell::new(None);
    let force = Cell::new(-1);
    let no_ff = Cell::new(-1);
    let cmd = Cell::new(0);
    let apply = Cell::new(false);
    let merge = Cell::new(false);
    let interactive = Cell::new(false);
    let preserve = Cell::new(-1);
    let rerere = Cell::new(-1);
    let keep_empty = Cell::new(-1);
    let allow_empty_message = Cell::new(-1);
    let empty: RefCell<Option<Empty>> = RefCell::new(None);
    let autosquash = Cell::new(-1);
    let update_refs = Cell::new(-1);
    let gpg_sign: RefCell<Option<Option<Sign>>> = RefCell::new(None);
    let autostash = Cell::new(-1);
    let exec: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let rebase_merges: RefCell<Option<RebaseMerges>> = RefCell::new(None);
    let fork_point = Cell::new(-1);
    let strategy: RefCell<Option<String>> = RefCell::new(None);
    let xopts: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let root = Cell::new(-1);
    let reschedule = Cell::new(-1);
    let reapply = Cell::new(-1);
    let opts = vec![
        Opt::string(None, "onto", &onto, "revision", msg::HELP_ONTO),
        Opt::bool(None, "keep-base", &keep_base, msg::HELP_KEEP_BASE),
        Opt::callback(None, "no-verify", None, msg::HELP_NO_VERIFY, |_, _| {
            no_verify.set(true);
            Ok(())
        })
        .noarg()
        .noneg(),
        Opt::callback(None, "verify", None, msg::HELP_VERIFY, |_, _| {
            no_verify.set(false);
            Ok(())
        })
        .noarg()
        .noneg(),
        Opt::quiet(&quiet, msg::HELP_QUIET),
        Opt::verbose(&verbose, msg::HELP_VERBOSE),
        Opt::callback('n', "no-stat", None, msg::HELP_NO_STAT, |_, _| {
            stat.set(0);
            Ok(())
        })
        .noarg()
        .noneg(),
        Opt::callback(None, "stat", None, msg::HELP_STAT, |_, _| {
            stat.set(1);
            Ok(())
        })
        .noarg()
        .noneg(),
        Opt::string_list(None, "trailer", &trailers, "trailer", msg::HELP_TRAILER),
        Opt::bool(None, "signoff", &signoff, msg::HELP_SIGNOFF),
        Opt::bool(
            None,
            "committer-date-is-author-date",
            &cdate,
            msg::HELP_CDATE_IS_ADATE,
        ),
        Opt::bool(None, "reset-author-date", &reset_date, msg::HELP_RESET_AUTHOR_DATE),
        Opt::bool(None, "ignore-date", &reset_date, msg::HELP_IGNORE_DATE).hidden(),
        Opt::string('C', None, &apply_c, "n", msg::HELP_C),
        Opt::bool(None, "ignore-whitespace", &ignore_ws, msg::HELP_IGNORE_WHITESPACE),
        Opt::string(None, "whitespace", &whitespace, "action", msg::HELP_WHITESPACE),
        Opt::bool('f', "force-rebase", &force, msg::HELP_FORCE_REBASE),
        Opt::callback(None, "no-ff", None, msg::HELP_FORCE_REBASE, |_, _| {
            no_ff.set(1);
            Ok(())
        })
        .noarg()
        .noneg(),
        Opt::callback(None, "ff", None, msg::HELP_FF, |_, _| {
            no_ff.set(0);
            Ok(())
        })
        .noarg()
        .noneg(),
        Opt::cmdmode(None, "continue", &cmd, msg::HELP_CONTINUE, CONTINUE),
        Opt::cmdmode(None, "skip", &cmd, msg::HELP_SKIP, SKIP),
        Opt::cmdmode(None, "abort", &cmd, msg::HELP_ABORT, ABORT),
        Opt::cmdmode(None, "quit", &cmd, msg::HELP_QUIT, QUIT),
        Opt::cmdmode(None, "edit-todo", &cmd, msg::HELP_EDIT_TODO, EDIT_TODO),
        Opt::cmdmode(
            None,
            "show-current-patch",
            &cmd,
            msg::HELP_SHOW_CURRENT_PATCH,
            SHOW_CURRENT_PATCH,
        ),
        Opt::flag(None, "apply", &apply, msg::HELP_APPLY).noneg(),
        Opt::flag('m', "merge", &merge, msg::HELP_MERGE).noneg(),
        Opt::flag('i', "interactive", &interactive, msg::HELP_INTERACTIVE).noneg(),
        Opt::bool('p', "preserve-merges", &preserve, msg::HELP_PRESERVE_MERGES).hidden(),
        Opt::bool(None, "rerere-autoupdate", &rerere, msg::HELP_RERERE),
        Opt::bool('k', "keep-empty", &keep_empty, msg::HELP_KEEP_EMPTY).hidden(),
        Opt::bool(
            None,
            "allow-empty-message",
            &allow_empty_message,
            msg::HELP_ALLOW_EMPTY_MESSAGE,
        )
        .hidden(),
        Opt::callback(None, "empty", Some("(drop|keep|stop)"), msg::HELP_EMPTY, |v, _| {
            let v = v.unwrap_or_default();
            let mode = match v.to_ascii_lowercase().as_str() {
                "drop" => Empty::Drop,
                "keep" => Empty::Keep,
                "stop" => Empty::Stop,
                "ask" => {
                    eprintln!("warning: {}", msg::EMPTY_ASK_DEPRECATED);
                    Empty::Stop
                }
                _ => return Err(CallbackError::Fatal(msg::invalid_empty(v))),
            };
            *empty.borrow_mut() = Some(mode);
            Ok(())
        })
        .literal_argh()
        .noneg(),
        Opt::bool(None, "autosquash", &autosquash, msg::HELP_AUTOSQUASH),
        Opt::bool(None, "update-refs", &update_refs, msg::HELP_UPDATE_REFS),
        Opt::callback('S', "gpg-sign", Some("key-id"), msg::HELP_GPG_SIGN, |v, unset| {
            *gpg_sign.borrow_mut() = Some((!unset).then(|| Sign::from_arg(v)));
            Ok(())
        })
        .optional(None),
        Opt::bool(None, "autostash", &autostash, msg::HELP_AUTOSTASH),
        Opt::string_list('x', "exec", &exec, "exec", msg::HELP_EXEC),
        Opt::callback(
            'r',
            "rebase-merges",
            Some("mode"),
            msg::HELP_REBASE_MERGES,
            |v, unset| {
                let mode = if unset {
                    RebaseMerges::Off
                } else {
                    let v = v.unwrap_or_default();
                    RebaseMerges::parse(v)
                        .ok_or_else(|| CallbackError::Fatal(msg::invalid_rebase_merges(v)))?
                };
                *rebase_merges.borrow_mut() = Some(mode);
                Ok(())
            },
        )
        .optional(None),
        Opt::bool(None, "fork-point", &fork_point, msg::HELP_FORK_POINT),
        Opt::string('s', "strategy", &strategy, "strategy", msg::HELP_STRATEGY),
        Opt::string_list('X', "strategy-option", &xopts, "option", msg::HELP_STRATEGY_OPTION),
        Opt::bool(None, "root", &root, msg::HELP_ROOT),
        Opt::bool(
            None,
            "reschedule-failed-exec",
            &reschedule,
            msg::HELP_RESCHEDULE,
        ),
        Opt::bool(None, "reapply-cherry-picks", &reapply, msg::HELP_REAPPLY),
    ];
    let mut parser = Parser::new(msg::USAGE, opts);
    let rest = parser.parse(argv)?.args;
    let usage_error = parser.usage_error();
    drop(parser);

    let mode = cmd.get();
    if mode != 0 {
        // An action (--continue, --abort, ...) takes no other argument.
        if !rest.is_empty() || argv.len() != 2 {
            return Err(Die::Output(usage_error));
        }
        return match mode {
            CONTINUE => rebase::continue_rebase(ctx),
            SKIP => rebase::skip(ctx),
            ABORT => rebase::abort(ctx),
            QUIT => rebase::quit(ctx),
            EDIT_TODO => rebase::edit_todo(ctx),
            _ => Err(Die::Fatal(msg::not_supported("--show-current-patch"))),
        };
    }

    // Not implemented yet: the interactive machinery (#155), the apply
    // back end (#136) and a few options.
    let unsupported = [
        ("--apply (#136)", apply.get()),
        ("-C (#136)", apply_c.borrow().is_some()),
        ("--whitespace (#136)", whitespace.borrow().is_some()),
        ("--ignore-whitespace", ignore_ws.get() == 1),
        ("--trailer", !trailers.borrow().is_empty()),
        ("--rerere-autoupdate", rerere.get() == 1),
        ("--preserve-merges", preserve.get() == 1),
    ];
    if let Some((what, _)) = unsupported.iter().find(|(_, set)| *set) {
        return Err(Die::Fatal(msg::not_supported(what)));
    }
    let _ = (merge.get(), allow_empty_message.get());

    for x in exec.borrow().iter() {
        if x.trim().is_empty() {
            return Err(Die::Error(msg::EMPTY_EXEC.to_owned(), 1));
        }
        if x.contains('\n') {
            return Err(Die::Error(msg::EXEC_NEWLINE.to_owned(), 1));
        }
    }
    if keep_base.get() == 1 && onto.borrow().is_some() {
        return Err(Die::Fatal(msg::incompatible("--keep-base", "--onto")));
    }
    if keep_base.get() == 1 && root.get() == 1 {
        return Err(Die::Fatal(msg::incompatible("--keep-base", "--root")));
    }
    if root.get() == 1 && fork_point.get() == 1 {
        return Err(Die::Fatal(msg::incompatible("--root", "--fork-point")));
    }

    let root_mode = root.get() == 1;
    let max_args = if root_mode { 1 } else { 2 };
    if rest.len() > max_args {
        return Err(Die::Output(usage_error));
    }
    let (upstream, branch) = if root_mode {
        (None, rest.first().cloned())
    } else {
        (rest.first().cloned(), rest.get(1).cloned())
    };
    if keep_base.get() == 1 && upstream.is_none() {
        // --keep-base without <upstream> uses the configured upstream.
    }

    let o = Options {
        upstream,
        branch,
        onto: onto.borrow().clone(),
        keep_base: keep_base.get() == 1,
        root: root_mode,
        fork_point: match fork_point.get() {
            -1 => None,
            v => Some(v == 1),
        },
        force: force.get() == 1 || no_ff.get() == 1,
        quiet: quiet.get() > 0,
        verbose: verbose.get() > 0,
        stat: match stat.get() {
            -1 => None,
            v => Some(v == 1),
        },
        no_verify: no_verify.get(),
        signoff: signoff.get() == 1,
        committer_date_is_author_date: cdate.get() == 1,
        ignore_date: reset_date.get() == 1,
        empty: *empty.borrow(),
        no_keep_empty: keep_empty.get() == 0,
        reapply_cherry_picks: match reapply.get() {
            -1 => None,
            v => Some(v == 1),
        },
        strategy: strategy.borrow().clone(),
        xopts: xopts.borrow().clone(),
        autostash: match autostash.get() {
            -1 => None,
            v => Some(v == 1),
        },
        interactive: interactive.get(),
        exec: exec.borrow().clone(),
        autosquash: match autosquash.get() {
            -1 => None,
            v => Some(v == 1),
        },
        rebase_merges: *rebase_merges.borrow(),
        reschedule_failed_exec: match reschedule.get() {
            -1 => None,
            v => Some(v == 1),
        },
        gpg_sign: gpg_sign.borrow().clone(),
        update_refs: match update_refs.get() {
            -1 => None,
            v => Some(v == 1),
        },
    };
    rebase::start(ctx, &o)
}
