// SPDX-License-Identifier: MIT
//! `git bisect` (Documentation/git-bisect.adoc, git-bisect-lk2009.adoc):
//! binary search for the commit that introduced a change.
//!
//! The state lives in the (per-worktree) git directory, in the files git
//! uses: `BISECT_START` (the branch or commit to return to),
//! `BISECT_TERMS`, `BISECT_LOG`, `BISECT_NAMES`, `BISECT_EXPECTED_REV`,
//! `BISECT_ANCESTORS_OK`, `BISECT_FIRST_PARENT`, `BISECT_HEAD`
//! (`--no-checkout`), `BISECT_RUN`, `BISECT_RESET_WHEN_FOUND`, and the refs
//! `refs/bisect/<new>`, `refs/bisect/<old>-<id>` and `refs/bisect/skip-<id>`.
//!
//! The commit to test next is chosen by [`rgit_revwalk::bisect`]; the
//! candidates come from the same walk as `rev-list <new> --not <old>...`.
//! Checking out, the log of the first bad commit and `visualize` run
//! rgit's own `checkout`, `log` and `diff-tree` as child processes.
//!
//! The pathspec of `start` (`BISECT_NAMES`) limits the walk like
//! `rev-list -- <paths>`.
//!
//! Not yet: the interactive "start for you?" question, and `gitk` for
//! `visualize` (it always runs `log`).

#[path = "bisect/next.rs"]
mod next;
#[path = "bisect/runner.rs"]
mod runner;

use std::io::Write;
use std::path::{Path, PathBuf};

use rgit_hash::ObjectId;
use rgit_refs::files::FilesStore;
use rgit_refs::{NoHooks, RefUpdate, Transaction};
use rgit_repo::Setup;

use crate::context::sq_quote;
use crate::messages::bisect as msg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, Revs};
use crate::repo::{self, Mode};

/// Subcommands that cannot be used as terms.
const BUILTINS: &[&str] = &[
    "help",
    "start",
    "skip",
    "next",
    "reset",
    "visualize",
    "view",
    "replay",
    "log",
    "run",
    "terms",
];

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let args = &argv[1..];
    let Some(cmd) = args.first().map(String::as_str) else {
        return usage_error(msg::NEED_COMMAND);
    };
    match cmd {
        "-h" | "--help-all" => {
            let _ = std::io::stdout().write_all(msg::USAGE.trim_end_matches('\n').as_bytes());
            let _ = std::io::stdout().write_all(b"\n\n");
            return Ok(0);
        }
        "help" => {
            let _ = std::io::stderr().write_all(msg::USAGE.as_bytes());
            return Ok(rgit_options::EXIT_USAGE);
        }
        _ => {}
    }
    let mut b = Bisect::open(ctx)?;
    let rest = &args[1..];
    match cmd {
        "start" => b.start(rest),
        "next" => {
            if !rest.is_empty() {
                return Err(Die::Error(msg::NEXT_ARGS.into(), 1));
            }
            if !b.is_bisecting() {
                return b.need_start();
            }
            b.next_cmd()
        }
        "reset" => b.reset(rest),
        "terms" => b.terms_cmd(rest),
        "log" => b.log_cmd(),
        "replay" => b.replay(rest),
        "visualize" | "view" => b.visualize(rest),
        "run" => runner::run(&mut b, rest),
        "skip" => b.state_cmd("skip", rest),
        _ => b.state_cmd(cmd, rest),
    }
}

/// `fatal: <msg>`, an empty line and the usage, exit 129.
fn usage_error(m: &str) -> CmdResult {
    let mut e = std::io::stderr().lock();
    let _ = writeln!(e, "fatal: {m}\n");
    let _ = e.write_all(msg::USAGE.as_bytes());
    Err(Die::Exit(rgit_options::EXIT_USAGE))
}

/// The two terms: `new` (default `bad`) and `old` (default `good`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Terms {
    pub new: String,
    pub old: String,
}

impl Default for Terms {
    fn default() -> Self {
        Terms {
            new: "bad".into(),
            old: "good".into(),
        }
    }
}

impl Terms {
    /// The terms of the bisection in the (per-worktree) git directory of
    /// `setup` (`BISECT_TERMS`); `bad`/`good` without one. `rev-list
    /// --bisect` and `rev-parse --bisect` use them.
    pub fn of(setup: &Setup) -> Terms {
        let text = std::fs::read_to_string(setup.cwd.join(&setup.git_dir).join("BISECT_TERMS"));
        let mut lines = text.as_deref().unwrap_or("").lines();
        match (lines.next(), lines.next()) {
            (Some(new), Some(old)) => Terms {
                new: new.to_string(),
                old: old.to_string(),
            },
            _ => Terms::default(),
        }
    }
}

/// What a step ended with (the exit code of the command).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// A commit to test was checked out, or more marks are needed.
    Continue,
    /// The first new commit was found.
    Found,
    /// Only skipped commits are left (exit 2).
    OnlySkipped,
    /// The step failed with this exit code (message already printed).
    Failed(i32),
}

/// A repository being bisected.
pub(crate) struct Bisect<'c> {
    pub ctx: &'c Context,
    pub setup: Setup,
    /// The per-worktree git directory (absolute).
    pub git_dir: PathBuf,
    pub revs: Revs,
    pub objects: Objects,
    pub store: FilesStore,
    /// Standard output of the current step (printed, and kept for
    /// `BISECT_RUN`).
    pub out: String,
}

impl<'c> Bisect<'c> {
    fn open(ctx: &'c Context) -> Result<Bisect<'c>, Die> {
        let setup = repo::setup(ctx, Mode::Required)?;
        let config = refs_support::load_config(ctx, &setup)?;
        let revs = Revs::new(ctx, &setup)?;
        let objects = Objects::open(&setup);
        let store = refs_support::ref_store(&setup, &config);
        let git_dir = std::path::absolute(setup.cwd.join(&setup.git_dir))
            .unwrap_or_else(|_| setup.cwd.join(&setup.git_dir));
        Ok(Bisect {
            ctx,
            setup,
            git_dir,
            revs,
            objects,
            store,
            out: String::new(),
        })
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.git_dir.join(name)
    }

    fn read(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.path(name)).ok()
    }

    fn exists(&self, name: &str) -> bool {
        self.path(name).exists()
    }

    fn write(&self, name: &str, text: &str) -> Result<(), Die> {
        std::fs::write(self.path(name), text)
            .map_err(|e| Die::Fatal(msg::could_not_write(&self.path(name).display().to_string(), &e.to_string())))
    }

    fn remove(&self, name: &str) {
        let _ = std::fs::remove_file(self.path(name));
    }

    pub fn is_bisecting(&self) -> bool {
        self.read("BISECT_START").is_some_and(|s| !s.is_empty())
    }

    /// `You need to start by "git bisect start"`, exit 1.
    fn need_start(&self) -> CmdResult {
        let mut e = std::io::stderr().lock();
        let _ = writeln!(e, "{}", msg::NEED_START);
        Err(Die::Exit(1))
    }

    /// The terms of this bisection (`BISECT_TERMS`), if written.
    pub fn read_terms(&self) -> Option<Terms> {
        let text = self.read("BISECT_TERMS")?;
        let mut lines = text.lines();
        let new = lines.next()?.to_string();
        let old = lines.next()?.to_string();
        Some(Terms { new, old })
    }

    pub fn terms(&self) -> Terms {
        self.read_terms().unwrap_or_default()
    }

    fn append_log(&self, text: &str) -> Result<(), Die> {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path("BISECT_LOG"))
            .map_err(|e| Die::Fatal(e.to_string()))?;
        f.write_all(text.as_bytes())
            .map_err(|e| Die::Fatal(e.to_string()))
    }

    /// Print a line to standard output (buffered for `BISECT_RUN`).
    pub fn say(&mut self, line: &str) {
        self.out.push_str(line);
        self.out.push('\n');
    }

    /// Print what was buffered.
    pub fn flush(&mut self) {
        let text = std::mem::take(&mut self.out);
        let mut o = std::io::stdout().lock();
        let _ = o.write_all(text.as_bytes());
        let _ = o.flush();
    }

    /// `name` as a commit (tags peeled).
    pub fn commit(&self, name: &str) -> Option<ObjectId> {
        let id = self.revs.resolve(name)?;
        self.revs
            .with_resolver(|r| r.peel_to_commit(&id).ok())
    }

    pub fn subject(&self, id: &ObjectId) -> String {
        crate::switch_branch::subject(&self.objects, id)
    }

    fn update_ref(&self, name: &str, id: &ObjectId) -> Result<(), Die> {
        let mut tx = Transaction::new(&self.store);
        tx.push(RefUpdate::update(name, *id, None).no_deref(true))
            .map_err(|e| Die::Fatal(e.to_string()))?;
        tx.commit(&mut NoHooks)
            .map_err(|e| Die::Fatal(e.to_string()))
    }

    fn delete_ref(&self, name: &str) {
        let mut tx = Transaction::new(&self.store);
        if tx
            .push(RefUpdate::delete(name, self.setup.format.object_format, None).no_deref(true))
            .is_ok()
        {
            let _ = tx.commit(&mut NoHooks);
        }
    }

    /// The refs below `refs/bisect/`.
    pub fn bisect_refs(&self) -> Vec<(String, ObjectId)> {
        crate::refs::Refs::new(&self.setup).list("refs/bisect/")
    }

    /// `refs/bisect/<new>`, the `<old>` commits and the skipped ones.
    pub fn marks(&self, terms: &Terms) -> (Option<ObjectId>, Vec<ObjectId>, Vec<ObjectId>) {
        let mut bad = None;
        let mut good = Vec::new();
        let mut skip = Vec::new();
        let new_ref = format!("refs/bisect/{}", terms.new);
        let old_prefix = format!("refs/bisect/{}-", terms.old);
        for (name, id) in self.bisect_refs() {
            if name == new_ref {
                bad = Some(id);
            } else if name.starts_with(&old_prefix) {
                good.push(id);
            } else if name.starts_with("refs/bisect/skip-") {
                skip.push(id);
            }
        }
        (bad, good, skip)
    }

    /// Write `BISECT_EXPECTED_REV` (and `BISECT_HEAD` without checkout).
    pub fn write_pseudo_ref(&self, name: &str, id: &ObjectId) -> Result<(), Die> {
        self.write(name, &format!("{}\n", id.to_hex()))
    }

    pub fn no_checkout(&self) -> bool {
        self.exists("BISECT_HEAD")
    }

    /// Remove the bisection state.
    pub fn clean_state(&self) {
        for (name, _) in self.bisect_refs() {
            self.delete_ref(&name);
        }
        for f in [
            "BISECT_EXPECTED_REV",
            "BISECT_ANCESTORS_OK",
            "BISECT_LOG",
            "BISECT_TERMS",
            "BISECT_NAMES",
            "BISECT_RUN",
            "BISECT_FIRST_PARENT",
            "BISECT_HEAD",
            "BISECT_RESET_WHEN_FOUND",
            "BISECT_START",
        ] {
            self.remove(f);
        }
    }

    /// Run rgit as a child (`args`), standard output as given.
    pub fn child(&self, args: &[&str]) -> std::process::Command {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("git"));
        let mut c = std::process::Command::new(exe);
        c.args(args)
            .envs(self.ctx.child_env())
            .current_dir(self.work_dir());
        c
    }

    pub fn work_dir(&self) -> PathBuf {
        self.setup
            .work_tree
            .clone()
            .unwrap_or_else(|| self.setup.cwd.clone())
    }

    /// Check out `target` with `checkout` (quietly, or with its messages).
    pub fn checkout(&self, target: &str, quiet: bool) -> Result<i32, Die> {
        let mut args = vec!["checkout"];
        if quiet {
            args.push("-q");
        } else {
            args.push("--ignore-other-worktrees");
        }
        args.push(target);
        if quiet {
            args.push("--");
        }
        let _ = std::io::stdout().flush();
        let status = self
            .child(&args)
            .status()
            .map_err(|e| Die::Fatal(e.to_string()))?;
        Ok(status.code().unwrap_or(128))
    }

    /// `git bisect terms [--term-old | --term-new]`.
    fn terms_cmd(&mut self, args: &[String]) -> CmdResult {
        if args.len() > 1 {
            return Err(Die::Error(msg::TERMS_ARGS.into(), 1));
        }
        let Some(t) = self.read_terms() else {
            return Err(Die::Error(msg::NO_TERMS.into(), 1));
        };
        let text = match args.first().map(String::as_str) {
            None => msg::current_terms(&t.old, &t.new),
            Some("--term-good" | "--term-old") => t.old.clone(),
            Some("--term-bad" | "--term-new") => t.new.clone(),
            Some(other) => return Err(Die::Error(msg::invalid_terms_argument(other), 1)),
        };
        println!("{text}");
        Ok(0)
    }

    /// `git bisect log`.
    fn log_cmd(&mut self) -> CmdResult {
        match std::fs::read(self.path("BISECT_LOG")) {
            Ok(data) if self.is_bisecting() || !data.is_empty() => {
                let mut o = std::io::stdout().lock();
                let _ = o.write_all(&data);
                Ok(0)
            }
            _ => Err(Die::Error(msg::NOT_BISECTING.into(), 1)),
        }
    }

    /// `git bisect reset [<commit>]`.
    fn reset(&mut self, args: &[String]) -> CmdResult {
        if args.len() > 1 {
            return Err(Die::Error(msg::RESET_ARGS.into(), 1));
        }
        let Some(start) = self.read("BISECT_START").filter(|s| !s.is_empty()) else {
            self.clean_state();
            return Ok(0);
        };
        let target = match args.first() {
            Some(a) => {
                if self.commit(a).is_none() {
                    return Err(Die::Error(msg::not_a_valid_commit(a), 1));
                }
                a.clone()
            }
            None => start.trim_end_matches('\n').to_string(),
        };
        if !self.no_checkout() && self.setup.work_tree.is_some() {
            let code = self.checkout(&target, false)?;
            if code != 0 {
                return Err(Die::Error(msg::cannot_check_out_original(&target), 1));
            }
        }
        self.clean_state();
        Ok(0)
    }

    /// `git bisect visualize [<log options>]`: `log` of the remaining
    /// suspects.
    fn visualize(&mut self, args: &[String]) -> CmdResult {
        if !self.is_bisecting() {
            return Err(Die::Exit(1));
        }
        let terms = self.terms();
        let mut a: Vec<String> = vec!["log".into()];
        a.extend(args.iter().take_while(|x| *x != "--").cloned());
        let (bad, goods, _) = self.marks(&terms);
        if bad.is_some() {
            a.push(format!("refs/bisect/{}", terms.new));
        }
        if !goods.is_empty() {
            a.push("--not".into());
            for (name, _) in self.bisect_refs() {
                if name.starts_with(&format!("refs/bisect/{}-", terms.old)) {
                    a.push(name);
                }
            }
        }
        let names = self.names();
        let paths: Vec<String> = args
            .iter()
            .skip_while(|x| *x != "--")
            .cloned()
            .chain(names)
            .filter(|x| x != "--")
            .collect();
        a.push("--".into());
        a.extend(paths);
        let refs: Vec<&str> = a.iter().map(String::as_str).collect();
        let _ = std::io::stdout().flush();
        let status = self
            .child(&refs)
            .status()
            .map_err(|e| Die::Fatal(e.to_string()))?;
        Ok(status.code().unwrap_or(128))
    }

    /// The pathspec of `BISECT_NAMES`.
    pub fn names(&self) -> Vec<String> {
        let text = self.read("BISECT_NAMES").unwrap_or_default();
        split_words(text.trim_end_matches('\n'))
    }

    /// A term given to `start` cannot be empty.
    fn term_format(term: &str) -> Result<String, Die> {
        if term.is_empty() {
            return Err(Die::Error(msg::not_a_valid_term(term), 1));
        }
        Ok(term.to_string())
    }

    /// Check a term given to `start`: `orig` is `bad` for the new term,
    /// `good` for the old one.
    fn check_term(term: &str, orig: &str) -> Result<(), Die> {
        let full = format!("refs/bisect/{term}");
        if rgit_refs::name::check_refname_format(full.as_bytes(), Default::default()).is_err() {
            return Err(Die::Error(msg::not_a_valid_term(term), 1));
        }
        if BUILTINS.contains(&term) {
            return Err(Die::Error(msg::builtin_as_term(term), 1));
        }
        let changes = match orig {
            "bad" => term == "good" || term == "old",
            _ => term == "bad" || term == "new",
        };
        if changes {
            return Err(Die::Error(msg::change_meaning(term), 1));
        }
        Ok(())
    }

    /// `git bisect start`.
    fn start(&mut self, args: &[String]) -> CmdResult {
        let outcome = self.start_inner(args)?;
        self.flush();
        match outcome {
            Outcome::Failed(code) => {
                self.clean_state();
                Err(Die::Exit(code))
            }
            o => Ok(outcome_code(o)),
        }
    }

    /// `start` without printing: the outcome of the first step.
    fn start_inner(&mut self, args: &[String]) -> Result<Outcome, Die> {
        let mut terms = Terms::default();
        let mut no_checkout = false;
        let mut first_parent = false;
        let mut reset_when_found: Option<String> = None;
        let mut terms_given = false;
        let has_dashdash = args.iter().any(|a| a == "--");
        let mut revs: Vec<ObjectId> = Vec::new();
        let mut names_at: Option<usize> = None;
        let mut i = 0;
        while i < args.len() {
            let a = args[i].as_str();
            let value = |prefix: &str| a.strip_prefix(prefix).and_then(|r| r.strip_prefix('='));
            if a == "--" {
                names_at = Some(i);
                break;
            } else if a == "--no-checkout" {
                no_checkout = true;
            } else if a == "--first-parent" {
                first_parent = true;
            } else if a == "--term-good" || a == "--term-old" {
                i += 1;
                terms.old = Self::term_format(args.get(i).map_or("", String::as_str))?;
                terms_given = true;
            } else if a == "--term-bad" || a == "--term-new" {
                i += 1;
                terms.new = Self::term_format(args.get(i).map_or("", String::as_str))?;
                terms_given = true;
            } else if let Some(v) = value("--term-good").or_else(|| value("--term-old")) {
                terms.old = Self::term_format(v)?;
                terms_given = true;
            } else if let Some(v) = value("--term-bad").or_else(|| value("--term-new")) {
                terms.new = Self::term_format(v)?;
                terms_given = true;
            } else if a == "--reset-when-found" {
                reset_when_found = Some("original".into());
            } else if let Some(v) = value("--reset-when-found") {
                reset_when_found = Some(v.to_string());
            } else if a.starts_with("--") {
                return Err(Die::Error(msg::unrecognized_option(a), 1));
            } else if let Some(id) = self.commit(a) {
                revs.push(id);
            } else if has_dashdash {
                return Err(Die::Fatal(msg::not_a_valid_revision(a)));
            } else {
                names_at = Some(i);
                break;
            }
            i += 1;
        }
        if let Some(w) = &reset_when_found {
            if w != "original" && w != "found" {
                return Err(Die::Error(msg::invalid_reset_when_found(w), 1));
            }
            if no_checkout {
                return Err(Die::Error(msg::RESET_WHEN_FOUND_NO_CHECKOUT.into(), 1));
            }
        }
        // A lone argument after the revisions is not kept as a pathspec.
        let names: &[String] = match names_at {
            Some(n) if args.len() - n > 1 => &args[n..],
            _ => &[],
        };
        if self.setup.is_bare() || self.setup.work_tree.is_none() {
            no_checkout = true;
        }
        // An earlier bisection is ended first.
        if let Some(old) = self.read("BISECT_START").filter(|s| !s.is_empty()) {
            if !self.no_checkout() && !no_checkout {
                let code = self.checkout(old.trim_end_matches('\n'), false)?;
                if code != 0 {
                    return Err(Die::Error(
                        msg::checking_out_failed(old.trim_end_matches('\n')),
                        1,
                    ));
                }
            }
            self.clean_state();
        }
        let refs = crate::refs::Refs::new(&self.setup);
        let head_id = refs.resolve("HEAD");
        let start_head = match refs.head_target() {
            Some(t) => t.strip_prefix("refs/heads/").unwrap_or(&t).to_string(),
            None => match head_id {
                Some(id) => id.to_hex(),
                None => return Err(Die::Fatal(msg::BAD_HEAD.into())),
            },
        };
        self.write("BISECT_START", &format!("{start_head}\n"))?;
        if no_checkout {
            if let Some(id) = head_id {
                self.write_pseudo_ref("BISECT_HEAD", &id)?;
            }
        }
        let quoted: Vec<String> = names.iter().map(|n| format!(" {}", sq_quote(n))).collect();
        self.write("BISECT_NAMES", &format!("{}\n", quoted.concat()))?;
        if first_parent {
            self.write("BISECT_FIRST_PARENT", "\n")?;
        }
        Self::check_term(&terms.new, "bad")?;
        Self::check_term(&terms.old, "good")?;
        if terms.new == terms.old {
            return Err(Die::Error(msg::TWO_TERMS.into(), 1));
        }
        if terms_given || !revs.is_empty() {
            self.write("BISECT_TERMS", &format!("{}\n{}\n", terms.new, terms.old))?;
        }
        if let Some(w) = &reset_when_found {
            self.write("BISECT_RESET_WHEN_FOUND", &format!("{w}\n"))?;
        }
        let mut log = String::new();
        for (n, id) in revs.iter().enumerate() {
            let word = if n == 0 { &terms.new } else { &terms.old };
            let name = if n == 0 {
                format!("refs/bisect/{word}")
            } else {
                format!("refs/bisect/{word}-{}", id.to_hex())
            };
            self.update_ref(&name, id)?;
            log.push_str(&format!("# {word}: [{}] {}\n", id.to_hex(), self.subject(id)));
        }
        log.push_str("git bisect start");
        for a in args {
            log.push(' ');
            log.push_str(&sq_quote(a));
        }
        log.push('\n');
        self.append_log(&log)?;
        self.auto_next(&terms)
    }

    /// `git bisect next`.
    fn next_cmd(&mut self) -> CmdResult {
        let terms = self.terms();
        let (bad, goods, _) = self.marks(&terms);
        if bad.is_none() {
            let (new, old) = if terms == Terms::default() {
                ("bad|new".to_string(), "good|old".to_string())
            } else {
                (terms.new.clone(), terms.old.clone())
            };
            eprintln!("error: {}", msg::need_both(&new, &old));
            return Err(Die::Exit(1));
        }
        if goods.is_empty() {
            eprintln!("warning: {}", msg::ONLY_BAD);
        }
        let o = self.next_step(&terms)?;
        self.flush();
        finish(o)
    }

    /// The status line while marks are missing.
    fn status_line(&self, terms: &Terms, bad: bool, goods: usize) -> String {
        match (bad, goods) {
            (false, 0) => msg::waiting_both(&terms.old, &terms.new),
            (true, _) => msg::waiting_old(&terms.old, &terms.new),
            (false, n) => msg::waiting_new(&terms.new, n, &terms.old),
        }
    }

    /// After a mark: the next step when both kinds of marks exist, else
    /// the status (also logged).
    fn auto_next(&mut self, terms: &Terms) -> Result<Outcome, Die> {
        let (bad, goods, _) = self.marks(terms);
        if bad.is_none() || goods.is_empty() {
            let st = self.status_line(terms, bad.is_some(), goods.len());
            self.say(&st);
            self.append_log(&format!("# {st}\n"))?;
            return Ok(Outcome::Continue);
        }
        self.next_step(terms)
    }

    /// `git bisect (bad|good|<term>|skip) [<rev>...]`.
    fn state_cmd(&mut self, word: &str, args: &[String]) -> CmdResult {
        let terms = match self.read_terms() {
            Some(t) => t,
            None => {
                // The first mark decides between bad/good and new/old.
                let t = match word {
                    "bad" | "good" => Terms::default(),
                    "new" | "old" => Terms {
                        new: "new".into(),
                        old: "old".into(),
                    },
                    "skip" => Terms::default(),
                    _ => return usage_error(&msg::unknown_command(word)),
                };
                if word != "skip" {
                    self.write("BISECT_TERMS", &format!("{}\n{}\n", t.new, t.old))?;
                }
                t
            }
        };
        if word != "skip" && word != terms.new && word != terms.old {
            if matches!(word, "bad" | "good" | "new" | "old") {
                eprintln!(
                    "error: {}",
                    msg::invalid_command_terms(&terms.new, &terms.old)
                );
            }
            return usage_error(&msg::unknown_command(word));
        }
        if !self.is_bisecting() {
            return self.need_start();
        }
        let outcome = self.mark(&terms, word, args)?;
        self.flush();
        finish(outcome)
    }

    /// Mark the commits `args` (default `HEAD`, or `BISECT_HEAD`) with
    /// `word`, then go on.
    pub fn mark(&mut self, terms: &Terms, word: &str, args: &[String]) -> Result<Outcome, Die> {
        if word == terms.new && args.len() > 1 {
            return Err(Die::Error(msg::only_one_argument(word), 1));
        }
        let mut ids: Vec<ObjectId> = Vec::new();
        if args.is_empty() {
            let head = if self.no_checkout() {
                "BISECT_HEAD"
            } else {
                "HEAD"
            };
            match self.commit(head) {
                Some(id) => ids.push(id),
                None => return Err(Die::Error(msg::bad_rev_input(head), 1)),
            }
        }
        for a in args {
            if word == "skip" && a.contains("..") {
                let list = crate::history::walk_commits(self.ctx, std::slice::from_ref(a))
                    .map_err(|_| Die::Error(msg::bad_rev_input(a), 1))?;
                ids.extend(list);
                continue;
            }
            match self.commit(a) {
                Some(id) => ids.push(id),
                None => return Err(Die::Error(msg::bad_rev_input(a), 1)),
            }
        }
        let mut log = String::new();
        for id in &ids {
            let hex = id.to_hex();
            let name = if word == terms.new {
                format!("refs/bisect/{word}")
            } else {
                format!("refs/bisect/{word}-{hex}")
            };
            self.update_ref(&name, id)?;
            log.push_str(&format!("# {word}: [{hex}] {}\n", self.subject(id)));
            log.push_str(&format!("git bisect {word} {hex}\n"));
            if word == terms.old {
                let (bad, _, _) = self.marks(terms);
                if let Some(bad) = bad
                    && !self.revs.is_ancestor(id, &bad)
                {
                    self.remove("BISECT_ANCESTORS_OK");
                }
            }
        }
        self.append_log(&log)?;
        self.auto_next(terms)
    }

    /// `git bisect replay <logfile>`.
    fn replay(&mut self, args: &[String]) -> CmdResult {
        let Some(file) = args.first() else {
            return Err(Die::Error(msg::NO_LOGFILE.into(), 1));
        };
        let Ok(text) = std::fs::read_to_string(Path::new(file)) else {
            return Err(Die::Error(msg::cannot_read_replay(file), 1));
        };
        self.reset(&[])?;
        let mut terms = Terms::default();
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            let rest = line
                .strip_prefix("git bisect ")
                .or_else(|| line.strip_prefix("git-bisect "));
            let Some(rest) = rest else {
                continue;
            };
            let (cmd, wargs) = rest.split_once(' ').unwrap_or((rest, ""));
            if cmd == "start" {
                // Only fully quoted arguments (as the log writes them).
                let words = sq_dequote(wargs).unwrap_or_default();
                let o = self.start_inner(&words)?;
                self.flush();
                if let Outcome::Failed(c) = o {
                    self.clean_state();
                    return Err(Die::Exit(c));
                }
                terms = self.terms();
                continue;
            }
            if matches!(cmd, "new" | "old") && self.read_terms().is_none() {
                // As in a live session, the first mark decides the terms.
                terms = Terms {
                    new: "new".into(),
                    old: "old".into(),
                };
            }
            if cmd == terms.new || cmd == terms.old || cmd == "skip" {
                self.replay_mark(&terms, cmd, &split_words(wargs))?;
                continue;
            }
            if cmd == "terms" {
                continue;
            }
            self.flush();
            eprintln!("error: {}", msg::what_are_you_talking_about(cmd));
            return Err(Die::Exit(1));
        }
        let o = self.auto_next(&terms)?;
        self.flush();
        finish(o)
    }

    /// A mark of a replayed log: recorded without going on.
    /// The names are used as written (in the ref names and the log), as
    /// the reference git does.
    fn replay_mark(&mut self, terms: &Terms, word: &str, args: &[String]) -> Result<(), Die> {
        if self.read_terms().is_none() {
            self.write("BISECT_TERMS", &format!("{}\n{}\n", terms.new, terms.old))?;
        }
        let mut log = String::new();
        for a in args {
            let id = self
                .commit(a)
                .ok_or_else(|| Die::Error(msg::bad_rev_input(a), 1))?;
            let name = if word == terms.new {
                format!("refs/bisect/{word}")
            } else {
                format!("refs/bisect/{word}-{a}")
            };
            self.update_ref(&name, &id)?;
            log.push_str(&format!("# {word}: [{}] {}\n", id.to_hex(), self.subject(&id)));
            log.push_str(&format!("git bisect {word} {a}\n"));
        }
        self.append_log(&log)
    }
}

/// The exit code of an outcome.
fn outcome_code(o: Outcome) -> i32 {
    match o {
        Outcome::Continue | Outcome::Found => 0,
        Outcome::OnlySkipped => 2,
        Outcome::Failed(c) => c,
    }
}

fn finish(o: Outcome) -> CmdResult {
    match o {
        Outcome::Failed(c) => Err(Die::Exit(c)),
        o => Ok(outcome_code(o)),
    }
}

/// The words of `s` if it consists of single-quoted words only (git's
/// `sq_dequote_to_argv`).
fn sq_dequote(s: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let Some(c) = chars.next() else {
            return Some(out);
        };
        if c != '\'' {
            return None;
        }
        let mut word = String::new();
        loop {
            match chars.next() {
                None => return None,
                Some('\'') => match chars.peek() {
                    Some('\\') => {
                        chars.next();
                        let e = chars.next()?;
                        if e != '\'' && e != '!' {
                            return None;
                        }
                        word.push(e);
                        if chars.next() != Some('\'') {
                            return None;
                        }
                    }
                    Some(c) if !c.is_whitespace() => return None,
                    _ => break,
                },
                Some(c) => word.push(c),
            }
        }
        out.push(word);
    }
}

/// Split shell words as `sq_quote` writes them (`'a b' 'c'\''d'`, plain
/// words, backslash escapes).
pub(crate) fn split_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    cur.push(q);
                }
            }
            '\\' => {
                in_word = true;
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            c if c.is_whitespace() => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::split_words;

    #[test]
    fn words() {
        assert_eq!(split_words(" '--' 'f'"), ["--", "f"]);
        assert_eq!(split_words("'a b' c 'd'\\''e'"), ["a b", "c", "d'e"]);
        assert_eq!(split_words("'x '\\!'= y'"), ["x != y"]);
        assert!(split_words("").is_empty());
    }

    #[test]
    fn dequote() {
        use super::sq_dequote;
        assert_eq!(sq_dequote("'a' 'b c'"), Some(vec!["a".into(), "b c".into()]));
        assert_eq!(sq_dequote("'d'\\''e'"), Some(vec!["d'e".into()]));
        assert_eq!(sq_dequote("t7 t1"), None);
        assert_eq!(sq_dequote(""), Some(vec![]));
    }
}
