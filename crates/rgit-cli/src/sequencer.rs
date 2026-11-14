// SPDX-License-Identifier: MIT
//! The sequencer behind `git cherry-pick` and `git revert`
//! (Documentation/git-cherry-pick.adoc, git-revert.adoc, sequencer.adoc,
//! config/revert.adoc), part of #58.
//!
//! - Picking or reverting one commit after the other: a three-way merge of
//!   the commit's change (`rgit-merge`, labels "HEAD", "<abbrev>
//!   (<subject>)" and "parent of ..."), the two-way update of the index and
//!   work tree from `HEAD` (refusing to lose local changes), the commit with
//!   the original author (cherry-pick) or the revert message, the summary
//!   and the reflog entry "<action>: <subject>";
//! - the state of a sequence in `.git/sequencer/` (`todo` with the current
//!   commit first, `head`, `abort-safety`, `opts`), `CHERRY_PICK_HEAD`,
//!   `REVERT_HEAD` and `MERGE_MSG` after a conflict, and `--continue`,
//!   `--skip`, `--abort`, `--quit`;
//! - a single commit given on its own is picked without sequencer state, so
//!   that it can be used in the middle of a sequence.
//!
//! The behaviour was derived from the documentation and differential tests
//! against the `git` binary (`tests/sequencer.rs`). The sequencer only
//! knows `pick` and `revert` so far; rebase (#154) will add its commands.
//!
//! Not yet: the merge strategies `resolve`, `octopus` and `subtree`
//! (#153), rerere (#156), `-S` (#51).

use std::io::Write as _;
use std::path::{Path, PathBuf};

use rgit_commit::message::{self as cmsg, Cleanup};
use rgit_config::SystemEnv;
use rgit_fs::{LockFile, LockOptions};
use rgit_hash::ObjectId;
use rgit_hook::RunOptions;
use rgit_ident::ident::{IdentConfig, Resolver as IdentResolver, Role, Strictness};
use rgit_index::Stage;
use rgit_merge::MergeResult;
use rgit_object::{Commit, Object, ObjectKind, Signature};
use rgit_refs::{RefUpdate, Transaction};
use rgit_unpack::{Identity, Merge, Porcelain, Worktree};
use rgit_worktree::stat::smudge_racy_entries;

use crate::commands::read_tree::{lock_failed, path_options, unpack_failed};
use crate::messages::sequencer as msg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, UpdateHooks};
use crate::repo::Mode;
use crate::repository::Repo;

/// What a command of the sequence does with its commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Pick,
    Revert,
}

impl Action {
    /// The command name ("cherry-pick", "revert").
    pub fn name(self) -> &'static str {
        match self {
            Action::Pick => "cherry-pick",
            Action::Revert => "revert",
        }
    }

    /// The word in the todo list.
    fn word(self) -> &'static str {
        match self {
            Action::Pick => "pick",
            Action::Revert => "revert",
        }
    }

    /// The pseudo-ref recording the commit being applied.
    pub fn head_file(self) -> &'static str {
        match self {
            Action::Pick => "CHERRY_PICK_HEAD",
            Action::Revert => "REVERT_HEAD",
        }
    }
}

/// The options of a sequence (stored in `.git/sequencer/opts`).
#[derive(Debug, Clone)]
pub struct Opts {
    pub action: Action,
    pub no_commit: bool,
    /// `-e`/`--no-edit`; `None` when neither was given.
    pub edit: Option<bool>,
    pub allow_empty: bool,
    pub allow_empty_message: bool,
    /// `--empty=keep`, `--keep-redundant-commits`.
    pub keep_redundant: bool,
    /// `--empty=drop`.
    pub drop_redundant: bool,
    pub signoff: bool,
    /// `-x`.
    pub record_origin: bool,
    pub allow_ff: bool,
    /// `-m`: the parent number, 0 when not given.
    pub mainline: usize,
    pub strategy: Option<String>,
    pub xopts: Vec<String>,
    /// `--cleanup`.
    pub cleanup: Option<String>,
    /// `revert --reference`, `revert.reference`.
    pub reference: bool,
}

impl Opts {
    pub fn new(action: Action) -> Opts {
        Opts {
            action,
            no_commit: false,
            edit: None,
            allow_empty: false,
            allow_empty_message: false,
            keep_redundant: false,
            drop_redundant: false,
            signoff: false,
            record_origin: false,
            allow_ff: false,
            mainline: 0,
            strategy: None,
            xopts: Vec::new(),
            cleanup: None,
            reference: false,
        }
    }

    /// The `opts` file (a configuration file with an `[options]` section).
    fn to_file(&self) -> String {
        let mut out = String::from("[options]\n");
        let mut kv = |k: &str, v: &str| {
            out.push('\t');
            out.push_str(k);
            out.push_str(" = ");
            out.push_str(&config_quote(v));
            out.push('\n');
        };
        if self.no_commit {
            kv("no-commit", "true");
        }
        if let Some(e) = self.edit {
            kv("edit", if e { "true" } else { "false" });
        }
        if self.allow_empty {
            kv("allow-empty", "true");
        }
        if self.allow_empty_message {
            kv("allow-empty-message", "true");
        }
        if self.keep_redundant {
            kv("keep-redundant-commits", "true");
        }
        if self.drop_redundant {
            kv("drop-redundant-commits", "true");
        }
        if self.signoff {
            kv("signoff", "true");
        }
        if self.record_origin {
            kv("record-origin", "true");
        }
        if self.allow_ff {
            kv("allow-ff", "true");
        }
        if self.mainline > 0 {
            kv("mainline", &self.mainline.to_string());
        }
        if let Some(s) = &self.strategy {
            kv("strategy", s);
        }
        for x in &self.xopts {
            kv("strategy-option", x);
        }
        if let Some(c) = &self.cleanup {
            kv("default-msg-cleanup", c);
        }
        out
    }

    /// Reads the `opts` file into `self` (missing keys keep their value).
    fn read_file(&mut self, data: &[u8]) {
        let text = String::from_utf8_lossy(data);
        let mut in_options = false;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_options = line
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .trim()
                    .eq_ignore_ascii_case("options");
                continue;
            }
            if !in_options || line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            let (key, value) = match line.split_once('=') {
                Some((k, v)) => (k.trim().to_ascii_lowercase(), config_unquote(v.trim())),
                None => (line.to_ascii_lowercase(), "true".to_owned()),
            };
            let truth = || rgit_config::value::parse_bool(Some(value.as_bytes())).unwrap_or(false);
            match key.as_str() {
                "no-commit" => self.no_commit = truth(),
                "edit" => self.edit = Some(truth()),
                "allow-empty" => self.allow_empty = truth(),
                "allow-empty-message" => self.allow_empty_message = truth(),
                "keep-redundant-commits" => self.keep_redundant = truth(),
                "drop-redundant-commits" => self.drop_redundant = truth(),
                "signoff" => self.signoff = truth(),
                "record-origin" => self.record_origin = truth(),
                "allow-ff" => self.allow_ff = truth(),
                "mainline" => self.mainline = value.parse().unwrap_or(0),
                "strategy" => self.strategy = Some(value),
                "strategy-option" => self.xopts.push(value),
                "default-msg-cleanup" => self.cleanup = Some(value),
                _ => {}
            }
        }
    }
}

/// A value for a configuration file, quoted when needed.
fn config_quote(v: &str) -> String {
    let needs =
        v.starts_with(' ') || v.ends_with(' ') || v.contains(['"', '\\', '#', ';', '\n', '\t']);
    if !needs {
        return v.to_owned();
    }
    let mut out = String::from("\"");
    for c in v.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn config_unquote(v: &str) -> String {
    let mut out = String::new();
    let mut quoted = false;
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(c) => out.push(c),
                None => {}
            },
            '#' | ';' if !quoted => break,
            c => out.push(c),
        }
    }
    if !quoted {
        let trimmed = out.trim_end().len();
        out.truncate(trimmed);
    }
    out
}

/// One line of the todo list.
#[derive(Debug, Clone)]
pub struct Item {
    pub action: Action,
    pub commit: ObjectId,
    /// The rest of the line as written (abbreviated id and subject).
    pub line: String,
}

/// Why a step failed. The caller adds "fatal: <action> failed".
#[derive(Debug)]
pub enum Fail {
    /// Print "error: <message>".
    Error(String),
    /// The messages were printed already.
    Printed,
    /// Pass this on unchanged.
    Die(Die),
}

impl From<Die> for Fail {
    fn from(d: Die) -> Fail {
        Fail::Die(d)
    }
}

/// How a step ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Applied (and committed unless `--no-commit`).
    Done,
    /// Stopped for the user (a conflict, or a commit that failed); exit 1.
    Stopped,
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// The first line of a commit message.
fn subject_line(message: &[u8]) -> &[u8] {
    let m = message.strip_prefix(b"\n").unwrap_or(message);
    let end = m.iter().position(|&b| b == b'\n').unwrap_or(m.len());
    &m[..end]
}

/// The sequencer working on one repository.
pub struct Sequencer<'c> {
    pub ctx: &'c mut Context,
    pub opts: Opts,
    pub git_dir: PathBuf,
    /// `GIT_REFLOG_ACTION` or the action name.
    pub reflog_action: String,
}

impl<'c> Sequencer<'c> {
    pub fn new(ctx: &'c mut Context, opts: Opts) -> Result<Sequencer<'c>, Die> {
        let repo = Repo::open(ctx, Mode::Required)?;
        repo.work_tree()?;
        let git_dir = repo.setup.cwd.join(&repo.setup.git_dir);
        let reflog_action = std::env::var("GIT_REFLOG_ACTION")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| opts.action.name().to_owned());
        Ok(Sequencer {
            ctx,
            opts,
            git_dir,
            reflog_action,
        })
    }

    fn repo(&self) -> Result<Repo, Die> {
        Repo::open(self.ctx, Mode::Required)
    }

    fn dir(&self) -> PathBuf {
        self.git_dir.join("sequencer")
    }

    fn path(&self, name: &str) -> PathBuf {
        self.git_dir.join(name)
    }

    /// The todo file as git names it in messages.
    fn todo_display(&self, repo: &Repo) -> String {
        repo.setup
            .git_dir
            .join("sequencer/todo")
            .to_string_lossy()
            .into_owned()
    }

    pub fn in_progress(&self) -> bool {
        self.dir().is_dir()
    }

    /// Whether `CHERRY_PICK_HEAD` or `REVERT_HEAD` names a commit.
    fn pseudo_head(&self, action: Action) -> Option<ObjectId> {
        let data = std::fs::read_to_string(self.path(action.head_file())).ok()?;
        let hex = data.trim_end();
        ObjectId::from_hex(
            if hex.len() == 64 {
                rgit_hash::HashKind::Sha256
            } else {
                rgit_hash::HashKind::Sha1
            },
            hex.as_bytes(),
        )
        .ok()
    }

    fn write_file(&self, name: &str, data: &[u8]) -> Result<(), Die> {
        std::fs::write(self.path(name), data).map_err(|e| Die::Fatal(e.to_string()))
    }

    fn remove(&self, name: &str) {
        let _ = std::fs::remove_file(self.path(name));
    }

    // ------------------------------------------------------------------
    // Starting a sequence.

    /// Applies `commits`. With `single`, no sequencer state is written (a
    /// single commit named on the command line).
    pub fn pick_revisions(&mut self, commits: &[ObjectId], single: bool) -> CmdResult {
        if single {
            let r = self.do_pick(&commits[0]);
            return self.finish_step(r);
        }
        if self.in_progress() {
            let action = self.opts.action.name();
            eprintln!("error: {}", msg::already_in_progress(action));
            let skip = self.pseudo_head(Action::Pick).is_some()
                || self.pseudo_head(Action::Revert).is_some();
            let repo = self.repo()?;
            self.ctx
                .advisor(&repo.config)?
                .advise_without_footer(
                    Advice::SequencerInUse,
                    &msg::already_in_progress_hint(action, skip),
                )
                .map_err(Die::Fatal)?;
            return Err(Die::Fatal(msg::failed(action)));
        }
        let mut repo = self.repo()?;
        let items: Vec<Item> = commits
            .iter()
            .map(|c| Item {
                action: self.opts.action,
                commit: *c,
                line: todo_rest(&repo, c),
            })
            .collect();
        let head = head_commit(&mut repo);
        std::fs::create_dir_all(self.dir()).map_err(|e| Die::Fatal(e.to_string()))?;
        if let Some(h) = head {
            let hex = format!("{}\n", h.to_hex());
            std::fs::write(self.dir().join("head"), &hex).map_err(|e| Die::Fatal(e.to_string()))?;
        }
        self.write_todo(&items)?;
        let opts_file = self.opts.to_file();
        if opts_file.lines().count() > 1 {
            std::fs::write(self.dir().join("opts"), opts_file)
                .map_err(|e| Die::Fatal(e.to_string()))?;
        }
        self.write_abort_safety(head)?;
        self.run_todo(items)
    }

    /// Prints the failure of a step and turns it into the exit code.
    fn finish_step(&self, r: Result<Step, Fail>) -> CmdResult {
        match r {
            Ok(Step::Done) => Ok(0),
            Ok(Step::Stopped) => Ok(1),
            Err(f) => Err(self.fail(f)),
        }
    }

    fn fail(&self, f: Fail) -> Die {
        match f {
            Fail::Error(m) => {
                eprintln!("error: {m}");
                Die::Fatal(msg::failed(self.opts.action.name()))
            }
            Fail::Printed => Die::Fatal(msg::failed(self.opts.action.name())),
            Fail::Die(d) => d,
        }
    }

    fn write_todo(&self, items: &[Item]) -> Result<(), Die> {
        let mut out = String::new();
        for i in items {
            out.push_str(i.action.word());
            out.push(' ');
            out.push_str(&i.line);
            out.push('\n');
        }
        std::fs::write(self.dir().join("todo"), out).map_err(|e| Die::Fatal(e.to_string()))
    }

    fn write_abort_safety(&self, head: Option<ObjectId>) -> Result<(), Die> {
        let data = head
            .map(|h| format!("{}\n", h.to_hex()))
            .unwrap_or_default();
        std::fs::write(self.dir().join("abort-safety"), data).map_err(|e| Die::Fatal(e.to_string()))
    }

    /// Applies the items one after the other, keeping the todo list (with
    /// the current item first) up to date; removes the state at the end.
    fn run_todo(&mut self, mut items: Vec<Item>) -> CmdResult {
        while !items.is_empty() {
            self.write_todo(&items)?;
            let item = items[0].clone();
            if item.action != self.opts.action {
                // The todo list was edited: every item uses the action of
                // its own line.
                self.opts.action = item.action;
            }
            let r = self.do_pick(&item.commit);
            let mut repo = self.repo()?;
            let head = head_commit(&mut repo);
            match r {
                Ok(Step::Done) => {
                    self.write_abort_safety(head)?;
                    items.remove(0);
                }
                Ok(Step::Stopped) => {
                    self.write_abort_safety(head)?;
                    return Ok(1);
                }
                Err(f) => {
                    self.write_abort_safety(head)?;
                    return Err(self.fail(f));
                }
            }
        }
        let _ = std::fs::remove_dir_all(self.dir());
        Ok(0)
    }

    // ------------------------------------------------------------------
    // One step.

    /// Picks or reverts `id` onto `HEAD`.
    fn do_pick(&mut self, id: &ObjectId) -> Result<Step, Fail> {
        let mut repo = self.repo()?;
        let action = self.opts.action;
        let (index, _) = repo.read_index()?;
        if index.has_conflicts() {
            eprintln!(
                "error: {}",
                match action {
                    Action::Pick => msg::CHERRY_PICK_UNMERGED,
                    Action::Revert => msg::REVERT_UNMERGED,
                }
            );
            self.ctx
                .advisor(&repo.config)?
                .advise_without_footer(
                    Advice::ResolveConflict,
                    crate::messages::commit::UNMERGED_HINT,
                )
                .map_err(Die::Fatal)?;
            return Err(Fail::Printed);
        }
        let head = head_commit(&mut repo);
        let empty_tree = ObjectId::empty_tree(repo.hash);
        let head_tree = match head {
            Some(h) => tree_of(&repo, &h)?,
            None => empty_tree,
        };
        // With --no-commit the index is "ours" (an empty index is the empty
        // tree, which is not written); otherwise it must match HEAD.
        let index_tree = if !self.opts.no_commit {
            head_tree
        } else if index.entries().is_empty() {
            empty_tree
        } else {
            crate::commands::write_tree::write_tree(&repo, &index, true, None)?
        };
        let clean = self.opts.no_commit || index_matches_tree(&repo, &index, &head_tree)?;
        drop(index);
        if !clean {
            eprintln!("error: {}", msg::local_changes(action.name()));
            let advisor = self.ctx.advisor(&repo.config)?;
            eprint!("{}", advisor.hint(msg::LOCAL_CHANGES_HINT));
            return Err(Fail::Printed);
        }

        let commit = read_commit(&repo, id)?;
        let parents = commit.parents().unwrap_or_default();
        let abbrev = repo.abbrev(id, repo.default_abbrev());
        let parent = if parents.len() > 1 {
            if self.opts.mainline == 0 {
                return Err(Fail::Error(msg::merge_without_mainline(&id.to_hex())));
            }
            match parents.get(self.opts.mainline - 1) {
                Some(p) => Some(*p),
                None => {
                    return Err(Fail::Error(msg::no_such_parent(
                        &id.to_hex(),
                        self.opts.mainline,
                    )));
                }
            }
        } else if self.opts.mainline > 1 && self.opts.mainline > parents.len() {
            return Err(Fail::Error(msg::no_such_parent(
                &id.to_hex(),
                self.opts.mainline,
            )));
        } else {
            parents.first().copied()
        };

        // --ff: HEAD is the parent (or both are missing).
        if self.opts.allow_ff && action == Action::Pick && !self.opts.no_commit && parent == head {
            return self.fast_forward(&repo, head, head_tree, id);
        }

        let subject = subject_line(commit.message()).to_vec();
        let label = format!("{abbrev} ({})", lossy(&subject));
        let commit_tree = commit.tree().map_err(|e| Fail::Error(e.to_string()))?;
        let parent_tree = match parent {
            Some(p) => tree_of(&repo, &p)?,
            None => empty_tree,
        };
        let comment = crate::commands::commit::comment_string(&repo, b"")?;
        let parent_label = match parent {
            Some(_) => msg::parent_of(&label),
            None => msg::EMPTY_TREE_LABEL.to_owned(),
        };
        let (base, theirs, base_label, their_label, message) = match action {
            Action::Pick => (
                parent_tree,
                commit_tree,
                parent_label,
                label.clone(),
                self.pick_message(&repo, id, &commit, &comment)?,
            ),
            Action::Revert => (
                commit_tree,
                parent_tree,
                label.clone(),
                parent_label,
                self.revert_message(
                    &repo,
                    id,
                    &commit,
                    parent.filter(|_| parents.len() > 1),
                    &comment,
                )?,
            ),
        };
        let message = if self.opts.signoff {
            self.signoff(&repo, &message, &comment)?
        } else {
            message
        };

        let ours = if self.opts.no_commit {
            index_tree
        } else {
            head_tree
        };
        let result = self.merge(&repo, &base, &ours, &theirs, &base_label, &their_label)?;
        if result.tree == empty_tree {
            repo.odb()
                .write(ObjectKind::Tree, b"")
                .map_err(|e| Fail::Die(Die::Fatal(e.to_string())))?;
        }
        // The index and work tree are updated before the messages appear.
        self.checkout(&repo, ours, &result)?;
        {
            let mut out = Vec::new();
            for m in &result.messages {
                out.extend_from_slice(&m.text);
                out.push(b'\n');
            }
            let mut o = std::io::stdout().lock();
            let _ = o.write_all(&out);
            let _ = o.flush();
        }
        crate::commands::merge_tree::advise_submodules(self.ctx, &repo, &result)?;
        self.write_file(
            "AUTO_MERGE",
            format!("{}\n", result.tree.to_hex()).as_bytes(),
        )?;

        let help_env = match action {
            Action::Pick => std::env::var("GIT_CHERRY_PICK_HELP").ok(),
            Action::Revert => None,
        };
        let record_head = !self.opts.no_commit && help_env.is_none();
        let cleanup = self.cleanup_mode(&repo)?;

        if !result.clean {
            let mut m = message.clone();
            if cleanup == Cleanup::Scissors {
                m.push(b'\n');
                m.extend_from_slice(&crate::commands::merge::scissors_block(&comment));
                m.extend_from_slice(comment.as_bytes());
            }
            m.extend_from_slice(&conflicts_block(
                &comment,
                &result
                    .conflicts
                    .iter()
                    .map(|c| c.path.clone())
                    .collect::<Vec<_>>(),
                repo.quote_path(),
            ));
            self.write_file("MERGE_MSG", &m)?;
            if record_head || (action == Action::Revert && self.opts.no_commit) {
                self.write_file(action.head_file(), format!("{}\n", id.to_hex()).as_bytes())?;
            }
            let text = match action {
                Action::Pick => msg::could_not_apply(&abbrev, &lossy(&subject)),
                Action::Revert => msg::could_not_revert(&abbrev, &lossy(&subject)),
            };
            eprintln!("error: {text}");
            let advice = match (&help_env, self.opts.no_commit) {
                (Some(h), _) => h.clone(),
                (None, true) => msg::CONFLICT_ADVICE_NO_COMMIT.to_owned(),
                (None, false) => msg::conflict_advice(action.name()),
            };
            self.ctx
                .advisor(&repo.config)?
                .advise(Advice::MergeConflict, &advice)
                .map_err(Die::Fatal)?;
            return Ok(Step::Stopped);
        }

        if self.opts.no_commit {
            self.write_file("MERGE_MSG", &message)?;
            if action == Action::Revert {
                self.write_file(action.head_file(), format!("{}\n", id.to_hex()).as_bytes())?;
            }
            return Ok(Step::Done);
        }

        // Empty results.
        let mut allow_empty = self.opts.allow_empty;
        if result.tree == head_tree {
            let initially_empty = action == Action::Pick && commit_tree == parent_tree;
            if initially_empty {
                // Kept only with --allow-empty (or --empty=keep).
            } else if self.opts.drop_redundant {
                eprintln!("{}", msg::dropping(&id.to_hex(), &lossy(&subject)));
                self.remove("AUTO_MERGE");
                return Ok(Step::Done);
            } else {
                allow_empty = self.opts.keep_redundant;
            }
        }

        let edit = self.opts.edit.unwrap_or(false);
        if edit || (result.tree == head_tree && !allow_empty) {
            // Let `git commit` do it: it edits the message or explains why
            // there is nothing to commit.
            self.write_file("MERGE_MSG", &message)?;
            // An empty revert is left to `git commit` without REVERT_HEAD
            // (observed with the git binary).
            if action == Action::Pick || edit {
                self.write_file(action.head_file(), format!("{}\n", id.to_hex()).as_bytes())?;
            }
            let mut args: Vec<String> = vec!["-n".into()];
            args.push(if edit { "-e" } else { "--no-edit" }.into());
            if let Some(c) = &self.opts.cleanup {
                args.push(format!("--cleanup={c}"));
            }
            if allow_empty {
                args.push("--allow-empty".into());
            }
            if self.opts.allow_empty_message {
                args.push("--allow-empty-message".into());
            }
            let env = vec![("GIT_REFLOG_ACTION".to_owned(), self.reflog_action.clone())];
            let code = self.run_commit(&args, &env)?;
            if code != 0 {
                return Ok(Step::Stopped);
            }
            return Ok(Step::Done);
        }

        self.write_file("MERGE_MSG", &message)?;
        let r = self.commit(
            &mut repo,
            head,
            result.tree,
            id,
            &commit,
            message,
            &comment,
            cleanup,
        );
        match r {
            Ok(step) => {
                if step == Step::Done {
                    // A commit also ends a pick that stopped before.
                    for f in ["MERGE_MSG", "CHERRY_PICK_HEAD", "REVERT_HEAD"] {
                        self.remove(f);
                    }
                }
                Ok(step)
            }
            Err(e) => Err(e),
        }
    }

    /// The message of a cherry-picked commit, with `-x`.
    fn pick_message(
        &self,
        repo: &Repo,
        id: &ObjectId,
        commit: &Commit,
        comment: &str,
    ) -> Result<Vec<u8>, Fail> {
        let mut m = commit.message().to_vec();
        if self.opts.record_origin {
            if !m.is_empty() && !m.ends_with(b"\n") {
                m.push(b'\n');
            }
            if !has_conforming_footer(repo, &m, comment)? {
                m.push(b'\n');
            }
            m.extend_from_slice(msg::cherry_picked_from(&id.to_hex()).as_bytes());
            m.push(b'\n');
        }
        Ok(m)
    }

    /// `Revert "<subject>"` (or `Reapply` for a revert of a revert) and
    /// the reference to the reverted commit.
    fn revert_message(
        &self,
        repo: &Repo,
        id: &ObjectId,
        commit: &Commit,
        mainline_parent: Option<ObjectId>,
        comment: &str,
    ) -> Result<Vec<u8>, Fail> {
        let subject = subject_line(commit.message());
        let mut m = Vec::new();
        if self.opts.reference {
            m.extend_from_slice(comment.as_bytes());
            m.extend_from_slice(msg::SAY_WHY.as_bytes());
        } else if let Some(rest) = subject
            .strip_prefix(b"Revert ")
            .filter(|s| s.starts_with(b"\"") && !s.starts_with(b"\"Revert \""))
        {
            // A revert of a revert reapplies (git-revert(1), DISCUSSION).
            m.extend_from_slice(msg::REAPPLY.as_bytes());
            m.extend_from_slice(rest);
        } else {
            m.extend_from_slice(&msg::revert_subject(subject));
        }
        m.extend_from_slice(b"\n\n");
        let name = if self.opts.reference {
            reference_name(repo, id, commit)
        } else {
            id.to_hex()
        };
        m.extend_from_slice(msg::this_reverts(&name).as_bytes());
        if let Some(p) = mainline_parent {
            let pname = if self.opts.reference {
                match repo.read_object(&p) {
                    Some(Object::Commit(pc)) => reference_name(repo, &p, &pc),
                    _ => p.to_hex(),
                }
            } else {
                p.to_hex()
            };
            m.extend_from_slice(msg::reversing_changes(&pname).as_bytes());
        }
        m.extend_from_slice(b".\n");
        Ok(m)
    }

    /// `--cleanup`, else `commit.cleanup`, else verbatim (whitespace with
    /// `-x`).
    fn cleanup_mode(&self, repo: &Repo) -> Result<Cleanup, Die> {
        let name = self.opts.cleanup.clone().or_else(|| {
            repo.config
                .get("commit.cleanup")
                .and_then(|e| e.value.as_ref())
                .map(|v| lossy(v))
        });
        match name.as_deref() {
            Some(m) => Cleanup::parse(m)
                .ok_or_else(|| Die::Fatal(crate::messages::commit::invalid_cleanup(m))),
            None if self.opts.record_origin => Ok(Cleanup::Whitespace),
            None => Ok(Cleanup::Verbatim),
        }
    }

    fn signoff(&self, repo: &Repo, message: &[u8], comment: &str) -> Result<Vec<u8>, Fail> {
        let committer = ident(repo, Role::Committer)?;
        let mut line = b"Signed-off-by: ".to_vec();
        line.extend_from_slice(&committer.name);
        line.extend_from_slice(b" <");
        line.extend_from_slice(&committer.email);
        line.push(b'>');
        let conf = trailer_config(repo)?;
        Ok(rgit_trailer::append_signoff(
            message,
            &line,
            &conf,
            comment.as_bytes(),
        ))
    }

    fn merge(
        &self,
        repo: &Repo,
        base: &ObjectId,
        ours: &ObjectId,
        theirs: &ObjectId,
        base_label: &str,
        their_label: &str,
    ) -> Result<MergeResult, Fail> {
        match self.opts.strategy.as_deref() {
            None | Some("ort" | "recursive") => {}
            Some(s) => return Err(Fail::Die(Die::Fatal(msg::strategy_not_supported(s)))),
        }
        let mut opts = rgit_merge::Options {
            branch1: b"HEAD".to_vec(),
            branch2: their_label.as_bytes().to_vec(),
            ancestor: base_label.as_bytes().to_vec(),
            style: crate::commands::merge_file::config_style(&repo.config)?,
            ..rgit_merge::Options::default()
        };
        for x in &self.opts.xopts {
            use crate::commands::merge_tree::StrategyOptionError as E;
            match crate::commands::merge_tree::apply_strategy_option(&mut opts, x) {
                Ok(()) => {}
                Err(E::Unknown) => {
                    return Err(Fail::Die(Die::Fatal(
                        crate::messages::merge_tree::unknown_strategy_option(x),
                    )));
                }
                Err(E::Unsupported) => {
                    return Err(Fail::Die(Die::Fatal(msg::option_not_supported(&format!(
                        "-X{x}"
                    )))));
                }
            }
        }
        rgit_merge::merge_trees(repo.odb(), base, ours, theirs, &opts)
            .map_err(|e| Fail::Die(Die::Fatal(e.to_string())))
    }

    /// Moves the index and work tree from the tree `old` to the merge
    /// result and records the conflicted paths' stages.
    fn checkout(&self, repo: &Repo, old: ObjectId, result: &MergeResult) -> Result<(), Fail> {
        two_way(repo, old, result.tree, Some(result), true)
    }

    fn fast_forward(
        &mut self,
        repo: &Repo,
        head: Option<ObjectId>,
        head_tree: ObjectId,
        id: &ObjectId,
    ) -> Result<Step, Fail> {
        let tree = tree_of(repo, id)?;
        two_way(repo, head_tree, tree, None, true)?;
        // The fast-forward is logged under the command's name even with
        // GIT_REFLOG_ACTION set (observed).
        let what = msg::reflog(self.opts.action.name(), msg::FAST_FORWARD);
        update_head(self.ctx, repo, *id, head, &what)?;
        Ok(Step::Done)
    }

    /// Runs `git commit` with `args`.
    fn run_commit(&self, args: &[String], env: &[(String, String)]) -> Result<i32, Fail> {
        let _ = std::io::stdout().flush();
        let exe = std::env::current_exe().map_err(|e| Fail::Die(Die::Fatal(e.to_string())))?;
        let status = std::process::Command::new(exe)
            .arg("commit")
            .args(args)
            .envs(self.ctx.child_env())
            .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .status()
            .map_err(|e| Fail::Die(Die::Fatal(e.to_string())))?;
        Ok(status.code().unwrap_or(128))
    }

    /// The commit of a clean pick without editing: hooks
    /// `prepare-commit-msg` and `post-commit`, the reflog entry and the
    /// summary.
    #[allow(clippy::too_many_arguments)]
    fn commit(
        &mut self,
        repo: &mut Repo,
        head: Option<ObjectId>,
        tree: ObjectId,
        id: &ObjectId,
        picked: &Commit,
        message: Vec<u8>,
        comment: &str,
        cleanup: Cleanup,
    ) -> Result<Step, Fail> {
        let author = match self.opts.action {
            Action::Pick => picked
                .author()
                .and_then(|a| a.to_owned())
                .ok_or_else(|| Fail::Error(msg::cannot_get_message(&id.to_hex())))?,
            Action::Revert => ident(repo, Role::Author)?,
        };
        let committer = ident(repo, Role::Committer)?;
        let hooks = crate::hooks::for_repo(self.ctx, &repo.setup, &repo.config)?;
        // The message goes through COMMIT_EDITMSG only for the
        // prepare-commit-msg hook (git leaves the file alone otherwise).
        let edited = if hooks.exists("prepare-commit-msg") {
            let editmsg = self.path("COMMIT_EDITMSG");
            std::fs::write(&editmsg, &message).map_err(|e| Fail::Die(Die::Fatal(e.to_string())))?;
            let shown = repo.setup.git_dir.join("COMMIT_EDITMSG");
            let shown = shown.to_string_lossy().into_owned();
            let mut o = RunOptions::with_args([shown.as_str(), "message"]);
            o.env.push(("GIT_EDITOR".into(), ":".into()));
            let _ = std::io::stdout().flush();
            let outcome = crate::hooks::run(&hooks, "prepare-commit-msg", &o)?;
            if !outcome.ok() {
                return Err(Fail::Printed);
            }
            std::fs::read(&editmsg).map_err(|e| Fail::Die(Die::Fatal(e.to_string())))?
        } else {
            message.clone()
        };
        let cleaned = cmsg::cleanup(&edited, cleanup.resolve(false), comment);
        if cleaned.is_empty() && !message.is_empty() && !self.opts.allow_empty_message {
            eprintln!("{}", msg::EMPTY_MESSAGE);
            return Err(Fail::Printed);
        }
        let parents: Vec<ObjectId> = head.into_iter().collect();
        let encoding = picked
            .encoding()
            .filter(|_| self.opts.action == Action::Pick)
            .map(|e| e.to_vec());
        let new = rgit_commit::NewCommit {
            tree,
            parents: &parents,
            author: &author,
            committer: &committer,
            encoding: encoding.as_deref(),
            message: &cleaned,
        };
        let oid = rgit_commit::create_commit(repo.odb(), &new)
            .map_err(|e| Fail::Die(Die::Fatal(e.to_string())))?;
        let what = msg::reflog(&self.reflog_action, &lossy(cmsg::first_line(&cleaned)));
        update_head(self.ctx, repo, oid, head, &what)?;
        let o = RunOptions::with_args(std::iter::empty::<&str>());
        crate::hooks::run(&hooks, "post-commit", &o)?;
        self.print_summary(repo, head, oid, tree, &author, &committer, &cleaned)?;
        Ok(Step::Done)
    }

    #[allow(clippy::too_many_arguments)]
    fn print_summary(
        &self,
        repo: &Repo,
        head: Option<ObjectId>,
        oid: ObjectId,
        tree: ObjectId,
        author: &Signature,
        committer: &Signature,
        message: &[u8],
    ) -> Result<(), Die> {
        use crate::messages::commit as cmsgs;
        let mut out = Vec::new();
        out.push(b'[');
        match repo.refs.head_target() {
            Some(b) => {
                out.extend_from_slice(b.strip_prefix("refs/heads/").unwrap_or(&b).as_bytes())
            }
            None => out.extend_from_slice(cmsgs::DETACHED_HEAD.as_bytes()),
        }
        out.push(b' ');
        out.extend_from_slice(repo.abbrev(&oid, repo.default_abbrev()).as_bytes());
        out.extend_from_slice(b"] ");
        out.extend_from_slice(&cmsg::subject(message));
        out.push(b'\n');
        if author.name != committer.name || author.email != committer.email {
            out.extend_from_slice(
                cmsgs::summary_author(&crate::commands::commit::name_email(author)).as_bytes(),
            );
            out.push(b'\n');
        }
        out.extend_from_slice(
            cmsgs::summary_date(&crate::commands::commit::show_date(author)).as_bytes(),
        );
        out.push(b'\n');
        let mut old = crate::tree_changes::Flat::new();
        if let Some(h) = head {
            let t = tree_of(repo, &h).map_err(|f| match f {
                Fail::Die(d) => d,
                _ => Die::Exit(128),
            })?;
            crate::tree_changes::flatten_tree(repo, &t, &mut old)?;
        }
        let mut new = crate::tree_changes::Flat::new();
        crate::tree_changes::flatten_tree(repo, &tree, &mut new)?;
        let pairs = crate::tree_changes::diff(repo, &old, &new, true);
        out.extend_from_slice(&crate::commands::commit::diff_summary(
            repo,
            &pairs,
            repo.quote_path(),
        ));
        let mut o = std::io::stdout().lock();
        let _ = o.write_all(&out);
        let _ = o.flush();
        Ok(())
    }

    // ------------------------------------------------------------------
    // --continue, --skip, --abort, --quit.

    /// `--quit`: forget the sequence, keep everything else.
    pub fn quit(&mut self) -> CmdResult {
        let _ = std::fs::remove_dir_all(self.dir());
        let mut stopped = false;
        for action in [Action::Pick, Action::Revert] {
            if self.pseudo_head(action).is_some() {
                self.remove(action.head_file());
                stopped = true;
            }
        }
        if stopped {
            self.remove("MERGE_MSG");
            self.remove("AUTO_MERGE");
        }
        Ok(0)
    }

    /// Reads the todo list and the options of the sequence in progress.
    fn read_state(&mut self) -> Result<Vec<Item>, Die> {
        let opts_path = self.dir().join("opts");
        if let Ok(data) = std::fs::read(&opts_path) {
            let action = self.opts.action;
            let mut o = Opts::new(action);
            o.read_file(&data);
            o.reference = self.opts.reference;
            self.opts = o;
        }
        let mut repo = self.repo()?;
        let display = self.todo_display(&repo);
        let data = std::fs::read(self.dir().join("todo")).map_err(|_| {
            eprintln!("error: {}", msg::could_not_read(&display));
            Die::Fatal(msg::failed(self.opts.action.name()))
        })?;
        let items = match parse_todo(&mut repo, &data) {
            Ok(items) => items,
            Err(()) => {
                eprintln!("error: {}", msg::unusable_todo(&display));
                return Err(Die::Fatal(msg::failed(self.opts.action.name())));
            }
        };
        let action = self.opts.action;
        if items
            .iter()
            .any(|i| i.as_ref().is_none_or(|i| i.action != action))
        {
            eprintln!(
                "error: {}",
                match self.opts.action {
                    Action::Pick => msg::PICK_DURING_REVERT,
                    Action::Revert => msg::REVERT_DURING_PICK,
                }
            );
            return Err(Die::Fatal(msg::failed(self.opts.action.name())));
        }
        Ok(items.into_iter().flatten().collect())
    }

    /// `--continue`: commit the resolved commit, then go on with the rest.
    pub fn continue_(&mut self) -> CmdResult {
        let action = self.opts.action;
        if !self.in_progress() {
            for a in [Action::Pick, Action::Revert] {
                if self.pseudo_head(a).is_some() {
                    // A single pick: just commit it.
                    let code = self.commit_resolved(a)?;
                    return Ok(code);
                }
            }
            eprintln!("error: {}", msg::NOTHING_IN_PROGRESS);
            return Err(Die::Fatal(msg::failed(action.name())));
        }
        let mut items = self.read_state()?;
        for a in [Action::Pick, Action::Revert] {
            if self.pseudo_head(a).is_some() {
                let code = self.commit_resolved(a)?;
                if code != 0 {
                    return Ok(code);
                }
            }
        }
        if !items.is_empty() {
            items.remove(0);
        }
        self.run_todo(items)
    }

    /// Commits the resolution of a stopped pick through `git commit`.
    fn commit_resolved(&mut self, action: Action) -> Result<i32, Die> {
        let _ = action;
        let mut args: Vec<String> = Vec::new();
        args.push(
            if self.opts.edit == Some(true) {
                "-e"
            } else {
                "--no-edit"
            }
            .into(),
        );
        let cleanup = self
            .opts
            .cleanup
            .clone()
            .unwrap_or_else(|| "strip".to_owned());
        args.push(format!("--cleanup={cleanup}"));
        if self.opts.allow_empty {
            args.push("--allow-empty".into());
        }
        if self.opts.allow_empty_message {
            args.push("--allow-empty-message".into());
        }
        self.run_commit(&args, &[]).map_err(|f| self.fail(f))
    }

    /// `--skip`: drop the commit that stopped and go on.
    pub fn skip(&mut self) -> CmdResult {
        let action = self.opts.action;
        let has_head = self.pseudo_head(action).is_some();
        let other = match action {
            Action::Pick => Action::Revert,
            Action::Revert => Action::Pick,
        };
        if !has_head && (self.pseudo_head(other).is_some() || !self.in_progress()) {
            eprintln!(
                "error: {}",
                match action {
                    Action::Pick => msg::NO_CHERRY_PICK,
                    Action::Revert => msg::NO_REVERT,
                }
            );
            return Err(Die::Fatal(msg::failed(action.name())));
        }
        if self.in_progress() {
            let items = self.read_state()?;
            if !has_head && self.head_moved()? {
                eprintln!("error: {}", msg::NOTHING_TO_SKIP);
                let repo = self.repo()?;
                let advisor = self.ctx.advisor(&repo.config)?;
                eprint!(
                    "{}",
                    advisor.hint(&msg::nothing_to_skip_hint(action.name()))
                );
                return Err(Die::Fatal(msg::failed(action.name())));
            }
            self.rollback_to_head()?;
            let mut items = items;
            if !items.is_empty() {
                items.remove(0);
            }
            return self.run_todo(items);
        }
        self.rollback_to_head()?;
        Ok(0)
    }

    /// Whether `HEAD` moved since the sequencer last touched it.
    fn head_moved(&self) -> Result<bool, Die> {
        let mut repo = self.repo()?;
        let head = head_commit(&mut repo);
        let safety = std::fs::read_to_string(self.dir().join("abort-safety")).unwrap_or_default();
        let safety = safety.trim();
        Ok(match head {
            Some(h) => h.to_hex() != safety,
            None => !safety.is_empty(),
        })
    }

    /// Throws away the stopped pick: `reset --merge HEAD`, without the
    /// pseudo-refs.
    fn rollback_to_head(&mut self) -> Result<(), Die> {
        let mut repo = self.repo()?;
        let head = head_commit(&mut repo);
        for a in [Action::Pick, Action::Revert] {
            self.remove(a.head_file());
        }
        self.remove("MERGE_MSG");
        if let Some(h) = head {
            reset_merge(self.ctx, &h.to_hex())?;
        }
        Ok(())
    }

    /// `--abort`: back to the commit the sequence started on.
    pub fn abort(&mut self) -> CmdResult {
        let action = self.opts.action;
        if !self.in_progress() {
            // A single pick in progress.
            for a in [Action::Pick, Action::Revert] {
                if self.pseudo_head(a).is_some() {
                    let mut repo = self.repo()?;
                    if head_commit(&mut repo).is_none() {
                        eprintln!("error: {}", msg::ABORT_UNBORN);
                        return Err(Die::Fatal(msg::failed(action.name())));
                    }
                    self.rollback_to_head()?;
                    return Ok(0);
                }
            }
            eprintln!("error: {}", msg::NOTHING_IN_PROGRESS);
            return Err(Die::Fatal(msg::failed(action.name())));
        }
        let head_file = std::fs::read_to_string(self.dir().join("head")).unwrap_or_default();
        let start = head_file.trim().to_owned();
        if start.is_empty() {
            let mut repo = self.repo()?;
            if head_commit(&mut repo).is_none() {
                eprintln!("error: {}", msg::ABORT_UNBORN);
                return Err(Die::Fatal(msg::failed(action.name())));
            }
        }
        if self.head_moved()? {
            eprintln!("warning: {}", msg::MOVED_HEAD);
            let _ = std::fs::remove_dir_all(self.dir());
            for a in [Action::Pick, Action::Revert] {
                self.remove(a.head_file());
            }
            return Ok(0);
        }
        let mut repo = self.repo()?;
        if head_commit(&mut repo).is_none() {
            eprintln!("error: {}", msg::ABORT_UNBORN);
            return Err(Die::Fatal(msg::failed(action.name())));
        }
        drop(repo);
        let code = match reset_merge(self.ctx, &start) {
            Ok(c) => c,
            Err(e) => {
                print_die(e);
                return Err(Die::Fatal(msg::failed(action.name())));
            }
        };
        if code != 0 {
            return Ok(code);
        }
        let _ = std::fs::remove_dir_all(self.dir());
        for a in [Action::Pick, Action::Revert] {
            self.remove(a.head_file());
        }
        Ok(0)
    }
}

/// `git reset --merge <commit>` in-process; the exit code.
fn reset_merge(ctx: &mut Context, commit: &str) -> Result<i32, Die> {
    let argv = vec![
        "reset".to_owned(),
        "--merge".to_owned(),
        "-q".to_owned(),
        commit.to_owned(),
    ];
    crate::commands::reset::run(ctx, &argv)
}

/// Prints what a [`Die`] would print at the end of the command.
fn print_die(e: Die) {
    match e {
        Die::Fatal(m) => eprintln!("fatal: {m}"),
        Die::Error(m, _) => eprintln!("error: {m}"),
        Die::Output(o) => {
            o.emit();
        }
        Die::Exit(_) => {}
    }
}

/// Whether the stage-0 entries of `index` are exactly the tree `tree`
/// (without writing any object).
fn index_matches_tree(
    repo: &Repo,
    index: &rgit_index::Index,
    tree: &ObjectId,
) -> Result<bool, Die> {
    let entries = rgit_unpack::tree_entries(repo.odb(), tree).map_err(unpack_failed)?;
    let ours: Vec<&rgit_index::Entry> = index.entries().iter().collect();
    if ours.len() != entries.len() {
        return Ok(false);
    }
    Ok(ours.iter().zip(entries.iter()).all(|(e, t)| {
        e.stage() == Stage::MERGED
            && !e.flags.intent_to_add
            && e.path() == t.path()
            && e.mode == t.mode
            && e.id == t.id
    }))
}

/// The commit `HEAD` points to; `None` on an unborn branch.
fn head_commit(repo: &mut Repo) -> Option<ObjectId> {
    repo.refs.resolve("HEAD")?;
    let id = repo.resolve("HEAD")?;
    repo.peel_to(id, ObjectKind::Commit)
}

fn read_commit(repo: &Repo, id: &ObjectId) -> Result<Commit, Fail> {
    match repo.read_object(id) {
        Some(Object::Commit(c)) => Ok(c),
        _ => Err(Fail::Error(rgit_merge::messages::not_a_commit(
            &id.to_hex(),
        ))),
    }
}

fn tree_of(repo: &Repo, id: &ObjectId) -> Result<ObjectId, Fail> {
    crate::switch_branch::commit_tree(repo, id)
        .ok_or_else(|| Fail::Error(rgit_merge::messages::not_a_commit(&id.to_hex())))
}

/// "<abbrev> <subject>" for the todo list.
fn todo_rest(repo: &Repo, id: &ObjectId) -> String {
    let abbrev = repo.abbrev(id, repo.default_abbrev());
    let subject = match repo.read_object(id) {
        Some(Object::Commit(c)) => lossy(subject_line(c.message())),
        _ => String::new(),
    };
    format!("{abbrev} {subject}")
}

/// Parses the todo list; errors are printed.
/// Comment and empty lines come back as `None` (they are not commands a
/// cherry-pick or revert accepts).
fn parse_todo(repo: &mut Repo, data: &[u8]) -> Result<Vec<Option<Item>>, ()> {
    let text = String::from_utf8_lossy(data);
    let mut items = Vec::new();
    let mut bad = false;
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim_start_matches([' ', '\t']);
        if line.is_empty() || line.starts_with('#') {
            items.push(None);
            continue;
        }
        let word_end = line.find([' ', '\t']).unwrap_or(line.len());
        let word = &line[..word_end];
        let action = match word {
            "pick" | "p" => Action::Pick,
            "revert" => Action::Revert,
            _ => {
                eprintln!("error: {}", msg::invalid_command(word));
                eprintln!("error: {}", msg::invalid_line(n + 1, raw));
                bad = true;
                continue;
            }
        };
        let rest = line[word_end..].trim_start_matches([' ', '\t']);
        let id_end = rest.find([' ', '\t']).unwrap_or(rest.len());
        let name = &rest[..id_end];
        let found = repo
            .resolve(name)
            .and_then(|id| repo.peel_to(id, ObjectKind::Commit))
            .filter(|_| !name.is_empty());
        let Some(commit) = found else {
            eprintln!("error: {}", msg::could_not_parse(name));
            eprintln!("error: {}", msg::invalid_line(n + 1, raw));
            bad = true;
            continue;
        };
        items.push(Some(Item {
            action,
            commit,
            line: rest.to_owned(),
        }));
    }
    if bad { Err(()) } else { Ok(items) }
}

/// The `reference` pretty format of a commit: `<abbrev> (<subject>,
/// <short date>)`.
fn reference_name(repo: &Repo, id: &ObjectId, commit: &Commit) -> String {
    let abbrev = repo.abbrev(id, repo.default_abbrev());
    let subject = lossy(&cmsg::subject(commit.message()));
    let date = commit
        .author()
        .and_then(|a| a.to_owned())
        .map(|a| {
            let mins = a.time.offset_minutes.abs();
            let mut tz = mins / 60 * 100 + mins % 60;
            if a.time.negative || a.time.offset_minutes < 0 {
                tz = -tz;
            }
            rgit_date::show_date(
                a.time.seconds.max(0) as u64,
                tz,
                &rgit_date::DateMode::new(rgit_date::DateStyle::Short),
                &rgit_date::DateContext::from_env(),
            )
            .unwrap_or_default()
        })
        .unwrap_or_default();
    format!("{abbrev} ({subject}, {date})")
}

/// Whether the message ends with a trailer block (`-x` then adds its line
/// without an empty line before it).
fn has_conforming_footer(repo: &Repo, message: &[u8], comment: &str) -> Result<bool, Fail> {
    let conf = trailer_config(repo)?;
    let end = rgit_trailer::message_end(message, comment.as_bytes());
    let info = rgit_trailer::parse(
        &message[..end],
        &conf,
        &rgit_trailer::ParseOptions {
            comment: comment.as_bytes(),
            divider: false,
        },
    );
    Ok(info.found)
}

fn trailer_config(repo: &Repo) -> Result<rgit_trailer::Config, Die> {
    let (conf, warnings) = rgit_trailer::Config::from_config(&repo.config).map_err(|entry| {
        let name = String::from_utf8_lossy(&entry.name).into_owned();
        crate::comment::bad_entry(&entry, &crate::messages::comment::missing_value(&name))
    })?;
    for w in warnings {
        eprintln!("warning: {w}");
    }
    Ok(conf)
}

/// The "Conflicts:" block of `MERGE_MSG`.
fn conflicts_block(comment: &str, paths: &[Vec<u8>], quote: bool) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(b'\n');
    out.extend_from_slice(comment.as_bytes());
    out.push(b' ');
    out.extend_from_slice(crate::messages::merge::CONFLICTS.as_bytes());
    out.push(b'\n');
    for p in paths {
        out.extend_from_slice(comment.as_bytes());
        out.push(b'\t');
        crate::quote::write_quoted(&mut out, p, quote);
        out.push(b'\n');
    }
    out
}

/// The user's identity.
fn ident(repo: &Repo, role: Role) -> Result<Signature, Die> {
    let env = SystemEnv::default();
    let icfg = IdentConfig::from_config(&repo.config).map_err(|p| {
        p.print();
        Die::Exit(rgit_options::EXIT_FATAL)
    })?;
    let resolver = IdentResolver::new(&icfg, &env);
    let ident = resolver.ident(role, Strictness::Strict).map_err(|e| {
        eprint!("{}", e.stderr_text());
        Die::Exit(rgit_options::EXIT_FATAL)
    })?;
    let raw = ident.to_bstring();
    rgit_object::SignatureRef::parse(&raw)
        .and_then(|s| s.to_owned())
        .ok_or_else(|| Die::Fatal(lossy(&raw)))
}

/// Moves `HEAD` (its branch) to `new` with the reflog message `message`.
fn update_head(
    ctx: &Context,
    repo: &Repo,
    new: ObjectId,
    old: Option<ObjectId>,
    message: &str,
) -> Result<(), Die> {
    let setup = &repo.setup;
    let config = refs_support::load_config(ctx, setup)?;
    let store = refs_support::ref_store(setup, &config);
    let objects = Objects::open(setup);
    let mut hooks = UpdateHooks::new(setup, &config, &objects);
    let update = match old {
        Some(o) => RefUpdate::update("HEAD", new, Some(o)),
        None => RefUpdate::create("HEAD", new),
    };
    let mut tx = Transaction::new(&store);
    tx.push(update.message(message.to_owned()))
        .and_then(|()| tx.commit(&mut hooks))
        .map_err(|e| Die::Fatal(e.to_string()))
}

/// Moves the index and work tree from the tree `old` to `new` (a two-way
/// merge that keeps local changes or refuses), then records the
/// conflicted paths of `result`.
fn two_way(
    repo: &Repo,
    old: ObjectId,
    new: ObjectId,
    result: Option<&MergeResult>,
    overwrite_ignore: bool,
) -> Result<(), Fail> {
    let work_tree = repo.work_tree()?;
    let index_path = repo.setup.cwd.join(repo.index_path());
    let lock = LockFile::acquire(&index_path, &LockOptions::default()).map_err(lock_failed)?;
    let (mut index, mtime) = repo.read_index()?;
    let checker = repo.checker(mtime)?;
    crate::switch_branch::refresh_stat(&mut index, &checker);
    let mut excludes = crate::commands::add::build_excludes(repo, &work_tree);
    let wt = Worktree {
        root: &work_tree,
        checker: &checker,
    };
    let mut opts = rgit_unpack::Options {
        update: true,
        show_all_errors: true,
        paths: path_options(repo),
        excludes: overwrite_ignore.then_some(&mut excludes),
        ..rgit_unpack::Options::default()
    };
    let merge = Merge::TwoWay { old, new };
    let plan = match rgit_unpack::plan(repo.odb(), &index, &merge, Some(&wt), &mut opts) {
        Ok(p) => p,
        Err(rgit_unpack::Error::Rejected(r)) => {
            for m in r.porcelain_messages(Porcelain::Merge) {
                eprintln!("error: {m}");
            }
            eprintln!("{}", rgit_unpack::messages::ABORTING);
            return Err(Fail::Printed);
        }
        Err(e) => return Err(Fail::Die(unpack_failed(e))),
    };
    let applied = plan.apply(repo.odb(), &wt, &Identity);
    let mut index = applied.index;
    if let Some(result) = result {
        crate::commands::merge::add_conflicts(&mut index, result)?;
    }
    if let Some(reference) = mtime {
        let _ = smudge_racy_entries(&mut index, reference, &checker);
    }
    crate::switch_branch::write_index(repo, &index, lock)?;
    if !applied.errors.is_empty() {
        for e in &applied.errors {
            eprintln!("error: {e}");
        }
        return Err(Fail::Die(Die::Exit(128)));
    }
    Ok(())
}

/// Whether the sequence in `git_dir` stops at its last commit (the todo
/// list holds at most one line): committing or resetting then ends it.
pub fn at_last_pick(git_dir: &Path) -> bool {
    let Ok(data) = std::fs::read(git_dir.join("sequencer/todo")) else {
        return false;
    };
    let count = String::from_utf8_lossy(&data)
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .count();
    count <= 1
}

/// After `git commit` concluded a stopped pick (or `git reset` dropped
/// it): remove the pseudo-ref, and the sequencer state when that was the
/// last commit of the sequence.
pub fn post_commit_cleanup(git_dir: &Path) {
    let had = ["CHERRY_PICK_HEAD", "REVERT_HEAD"]
        .iter()
        .any(|f| git_dir.join(f).exists());
    for f in ["CHERRY_PICK_HEAD", "REVERT_HEAD"] {
        let _ = std::fs::remove_file(git_dir.join(f));
    }
    if had && at_last_pick(git_dir) {
        let _ = std::fs::remove_dir_all(git_dir.join("sequencer"));
    }
}
