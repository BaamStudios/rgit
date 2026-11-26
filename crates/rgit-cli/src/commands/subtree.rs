// SPDX-License-Identifier: MIT
//! `git subtree` (contrib/subtree/git-subtree.adoc): merge a project into
//! a subdirectory (`add`, `merge`, `pull`) and extract the history of a
//! subdirectory as a project of its own (`split`, `push`).
//!
//! The porcelain steps (fetching, reading the subproject into the index,
//! merging, pushing) run this binary as a child, the way the commands
//! would be combined by hand. The synthetic history of `split` is built
//! in-process, see [`split`].
//!
//! Commits written by the command carry trailers that later runs read
//! back: `git-subtree-dir: <prefix>`, `git-subtree-mainline: <commit>`
//! (the main project side of an `add` or `split --rejoin` merge) and
//! `git-subtree-split: <commit>` (the subproject side).

use std::path::PathBuf;
use std::process::{Command, Stdio};

use rgit_hash::ObjectId;

use crate::messages::subtree as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;

#[path = "subtree/split.rs"]
mod split;

/// The subcommands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cmd {
    Add,
    Merge,
    Pull,
    Push,
    Split,
}

impl Cmd {
    fn parse(s: &str) -> Option<Cmd> {
        Some(match s {
            "add" => Cmd::Add,
            "merge" => Cmd::Merge,
            "pull" => Cmd::Pull,
            "push" => Cmd::Push,
            "split" => Cmd::Split,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Cmd::Add => "add",
            Cmd::Merge => "merge",
            Cmd::Pull => "pull",
            Cmd::Push => "push",
            Cmd::Split => "split",
        }
    }
}

/// Which commands accept an option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// `split` and `push`.
    Split,
    /// `add`, `merge`, `pull`, and `split`/`push` with `--rejoin`.
    AddMerge,
}

/// The options, in command-line order with their spelling for messages.
#[derive(Debug, Default, Clone)]
pub(crate) struct Options {
    pub quiet: bool,
    pub debug: bool,
    pub prefix: Option<String>,
    pub annotate: Option<String>,
    pub branch: Option<String>,
    pub ignore_joins: bool,
    pub onto: Option<String>,
    pub rejoin: bool,
    pub squash: bool,
    pub message: Option<String>,
    /// `-S[<keyid>]` (`Some(Some(_))`) or `--no-gpg-sign` (`Some(None)`).
    pub gpg_sign: Option<Option<String>>,
}

impl Options {
    /// The `-S`/`--no-gpg-sign` argument for child commands.
    fn gpg_arg(&self) -> Option<String> {
        match &self.gpg_sign {
            Some(Some(k)) if k.is_empty() => Some("-S".into()),
            Some(Some(k)) => Some(format!("-S{k}")),
            Some(None) => Some("--no-gpg-sign".into()),
            None => None,
        }
    }
}

/// The running command: options, repository location and child runner.
pub(crate) struct Subtree {
    pub opts: Options,
    /// The prefix without trailing slashes.
    pub dir: String,
    pub exe: PathBuf,
    pub env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    pub top: PathBuf,
}

/// `fatal: <msg>` and exit 1, as the command reports its own errors.
pub(crate) fn die(m: &str) -> Die {
    eprintln!("fatal: {m}");
    Die::Exit(1)
}

impl Subtree {
    pub fn debug(&self, m: &str) {
        if self.opts.debug {
            eprintln!("{m}");
        }
    }

    /// Progress and informational messages, silenced by `--quiet`.
    pub fn say(&self, m: &str) {
        if !self.opts.quiet {
            eprintln!("{m}");
        }
    }

    pub fn progress(&self, m: &str) {
        if self.opts.debug {
            self.debug(&format!("progress: {m}"));
        } else if !self.opts.quiet {
            use std::io::Write as _;
            let _ = std::io::stderr().lock().write_all(format!("{m}\r").as_bytes());
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        let mut cmd = Command::new(&self.exe);
        cmd.args(args)
            .current_dir(&self.top)
            .envs(self.env.iter().map(|(k, v)| (k, v)));
        cmd
    }

    /// Run a child with our standard streams; its exit code.
    pub fn run(&self, args: &[&str]) -> Result<i32, Die> {
        let status = self
            .command(args)
            .status()
            .map_err(|e| Die::Fatal(e.to_string()))?;
        Ok(status.code().unwrap_or(128))
    }

    /// Run a child whose standard output goes to our standard error.
    pub fn run_to_stderr(&self, args: &[&str]) -> Result<i32, Die> {
        let status = self
            .command(args)
            .stdout(Stdio::from(std::io::stderr()))
            .status()
            .map_err(|e| Die::Fatal(e.to_string()))?;
        Ok(status.code().unwrap_or(128))
    }

    /// Run a child quietly (no standard output or error); its exit code.
    pub fn run_silent(&self, args: &[&str]) -> Result<i32, Die> {
        let status = self
            .command(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| Die::Fatal(e.to_string()))?;
        Ok(status.code().unwrap_or(128))
    }

    /// Run a child and capture its standard output (standard error is
    /// ours unless `quiet_err`); `input` is fed to its standard input.
    pub fn capture(
        &self,
        args: &[&str],
        input: Option<&[u8]>,
        quiet_err: bool,
    ) -> Result<(i32, Vec<u8>), Die> {
        let mut cmd = self.command(args);
        cmd.stdout(Stdio::piped());
        if quiet_err {
            cmd.stderr(Stdio::null());
        }
        cmd.stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::inherit()
        });
        let mut child = cmd.spawn().map_err(|e| Die::Fatal(e.to_string()))?;
        if let Some(data) = input {
            use std::io::Write as _;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(data);
            }
        }
        let out = child
            .wait_with_output()
            .map_err(|e| Die::Fatal(e.to_string()))?;
        Ok((out.status.code().unwrap_or(128), out.stdout))
    }

    /// `rev-parse -q --verify <name>`: the object id, if it resolves.
    pub fn verify(&self, name: &str) -> Result<Option<ObjectId>, Die> {
        let (code, out) = self.capture(&["rev-parse", "-q", "--verify", name], None, true)?;
        if code != 0 {
            return Ok(None);
        }
        Ok(ObjectId::from_hex_any(String::from_utf8_lossy(&out).trim().as_bytes()).ok())
    }

    /// `<name>^{commit}`, or `fatal: '<name>' does not refer to a commit`.
    fn commit_of(&self, name: &str) -> Result<ObjectId, Die> {
        self.verify(&format!("{name}^{{commit}}"))?
            .ok_or_else(|| die(&msg::not_a_commit(name)))
    }

    /// The working tree and the index match `HEAD`.
    fn ensure_clean(&self) -> Result<(), Die> {
        let tree = self.run(&["diff-index", "HEAD", "--exit-code", "--quiet"])?;
        if tree != 0 {
            return Err(die(msg::NOT_CLEAN));
        }
        let index = self.run(&["diff-index", "--cached", "HEAD", "--exit-code", "--quiet"])?;
        if index != 0 {
            return Err(die(msg::NOT_CLEAN));
        }
        Ok(())
    }

    fn check_ref_format(&self, name: &str) -> Result<(), Die> {
        let full = format!("refs/heads/{name}");
        if self.run_silent(&["check-ref-format", &full])? != 0 {
            return Err(die(&msg::not_a_ref(name)));
        }
        Ok(())
    }

    /// `commit-tree` with `message` on standard input; the new commit.
    pub fn commit_tree(
        &self,
        tree: &ObjectId,
        parents: &[ObjectId],
        message: &str,
    ) -> Result<ObjectId, Die> {
        let tree = tree.to_hex();
        let mut args: Vec<String> = vec!["commit-tree".into(), tree];
        for p in parents {
            args.push("-p".into());
            args.push(p.to_hex());
        }
        if let Some(g) = self.opts.gpg_arg() {
            args.push(g);
        }
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let (code, out) = self.capture(&argv, Some(message.as_bytes()), false)?;
        if code != 0 {
            return Err(Die::Exit(code));
        }
        ObjectId::from_hex_any(String::from_utf8_lossy(&out).trim().as_bytes())
            .map_err(|_| Die::Exit(128))
    }

    fn short(&self, id: &ObjectId) -> Result<String, Die> {
        let (code, out) = self.capture(&["rev-parse", "--short", &id.to_hex()], None, true)?;
        if code != 0 {
            return Ok(id.to_hex());
        }
        Ok(String::from_utf8_lossy(&out).trim().to_string())
    }

    /// `log` output for a squash message: one line per commit.
    fn log_lines(&self, format: &str, range: &str) -> Result<String, Die> {
        let fmt = format!("--pretty=tformat:{format}");
        let (_, out) = self.capture(&["log", "--no-show-signature", &fmt, range], None, false)?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    /// Commits whose message mentions our prefix in a `git-subtree-dir`
    /// trailer (or, with `add_only`, an `add` subject), newest first, as
    /// `START <id>`, subject, body, `END` blocks.
    pub fn trailer_log(&self, rev: &str, add_only: bool) -> Result<Vec<u8>, Die> {
        let grep = if add_only {
            format!("--grep=^Add '{}/' from commit '", self.dir)
        } else {
            format!("--grep=^git-subtree-dir: {}/*$", self.dir)
        };
        let (_, out) = self.capture(
            &[
                "log",
                &grep,
                "--no-show-signature",
                "--pretty=format:START %H%n%s%n%n%b%nEND%n",
                rev,
            ],
            None,
            false,
        )?;
        Ok(out)
    }

    /// The commit named by a `git-subtree-split` trailer. Older versions
    /// recorded annotated tags there; a tag missing locally is fetched
    /// from `repository` when one was given.
    pub fn split_commit(
        &self,
        hash: &str,
        commit: &str,
        repository: Option<&str>,
    ) -> Result<ObjectId, Die> {
        let name = format!("{hash}^{{commit}}");
        if let Some(id) = self.verify(&name)? {
            return Ok(id);
        }
        if let Some(repo) = repository {
            let code = self.run(&["fetch", repo, hash])?;
            if code == 0
                && let Some(id) = self.verify(&name)?
            {
                return Ok(id);
            }
        }
        Err(die(&msg::bad_split_hash(hash, commit)))
    }

    /// The latest squash commit of the subtree and the subproject commit
    /// it holds: `(squash, split)`. For an `add` or `--rejoin` merge the
    /// "squash" is the merge's second parent.
    pub fn find_latest_squash(
        &self,
        repository: Option<&str>,
    ) -> Result<Option<(ObjectId, ObjectId)>, Die> {
        self.debug(&format!(
            "Looking for latest squash (dir={}, repository={})...",
            self.dir,
            repository.unwrap_or("")
        ));
        let log = self.trailer_log("HEAD", false)?;
        let mut sq = String::new();
        let mut main = String::new();
        let mut sub: Option<ObjectId> = None;
        for line in log.split(|&b| b == b'\n') {
            let line = String::from_utf8_lossy(line);
            let mut words = line.split_ascii_whitespace();
            let a = words.next().unwrap_or("");
            let b = words.next().unwrap_or("");
            let junk = words.collect::<Vec<_>>().join(" ");
            self.debug(&format!("  {a} {b} {junk}"));
            self.debug(&format!(
                "  {{{{{sq}/{main}/{}}}}}",
                sub.map(|s| s.to_hex()).unwrap_or_default()
            ));
            match a {
                "START" => sq = b.to_string(),
                _ if a == msg::TRAILER_MAINLINE => main = b.to_string(),
                _ if a == msg::TRAILER_SPLIT => {
                    sub = Some(self.split_commit(b, &sq, repository)?);
                }
                "END" => {
                    if let Some(sub) = sub {
                        let squash = if main.is_empty() {
                            ObjectId::from_hex_any(sq.as_bytes()).ok()
                        } else {
                            // A merge: the subproject side is its second parent.
                            self.verify(&format!("{sq}^2"))?
                        };
                        if let Some(squash) = squash {
                            self.debug(&format!("  Squash found: {squash} {sub}"));
                            return Ok(Some((squash, sub)));
                        }
                    }
                    sq.clear();
                    main.clear();
                    sub = None;
                }
                _ => {}
            }
        }
        Ok(None)
    }

    /// The message of a squash commit from `old` to `new`.
    fn squash_msg(&self, old: Option<&ObjectId>, new: &ObjectId) -> Result<String, Die> {
        let new_short = self.short(new)?;
        let mut m = String::new();
        match old {
            Some(old) => {
                let old_short = self.short(old)?;
                m.push_str(&msg::squash_changes_subject(&self.dir, &old_short, &new_short));
                m.push_str("\n\n");
                m.push_str(&self.log_lines("%h %s", &format!("{old}..{new}"))?);
                m.push_str(&self.log_lines("REVERT: %h %s", &format!("{new}..{old}"))?);
                m.push('\n');
            }
            None => {
                m.push_str(&msg::squash_content_subject(&self.dir, &new_short));
                m.push_str("\n\n");
            }
        }
        m.push_str(&format!("{} {}\n", msg::TRAILER_DIR, self.dir));
        m.push_str(&format!("{} {new}\n", msg::TRAILER_SPLIT));
        Ok(m)
    }

    /// A commit with the tree of subproject commit `new`, on top of the
    /// previous squash commit `old` (which held `old_sub`).
    fn new_squash_commit(
        &self,
        old: Option<(&ObjectId, &ObjectId)>,
        new: &ObjectId,
    ) -> Result<ObjectId, Die> {
        let tree = self
            .verify(&format!("{new}^{{tree}}"))?
            .ok_or(Die::Exit(128))?;
        let message = self.squash_msg(old.map(|(_, s)| s), new)?;
        let parents: Vec<ObjectId> = old.map(|(o, _)| *o).into_iter().collect();
        self.commit_tree(&tree, &parents, &message)
    }

    fn trailers(&self, mainline: &ObjectId, split: &ObjectId) -> String {
        format!(
            "\n\n{} {}\n{} {mainline}\n{} {split}\n",
            msg::TRAILER_DIR,
            self.dir,
            msg::TRAILER_MAINLINE,
            msg::TRAILER_SPLIT
        )
    }

    /// The message of the merge commit of `split --rejoin`.
    pub fn rejoin_msg(&self, mainline: &ObjectId, split: &ObjectId) -> String {
        let subject = match &self.opts.message {
            Some(m) => m.clone(),
            None => msg::rejoin_subject(&self.dir, &split.to_hex()),
        };
        subject + &self.trailers(mainline, split)
    }

    fn cmd_add(&self, args: &[String]) -> Result<i32, Die> {
        self.ensure_clean()?;
        match args.len() {
            1 => {
                let rev = self.commit_of(&args[0])?;
                self.add_commit(&rev, None)
            }
            2 => {
                self.check_ref_format(&args[1])?;
                println!("{}", msg::echo_fetch(&args[0], &args[1]));
                let code = self.run(&["fetch", &args[0], &args[1]])?;
                if code != 0 {
                    return Ok(code);
                }
                let rev = self.commit_of("FETCH_HEAD")?;
                self.add_commit(&rev, None)
            }
            _ => Err(die(&msg::add_params(&args.join(" ")))),
        }
    }

    /// Join subproject commit `rev` to the main project. With `rejoin` (the
    /// message of a first `split --rejoin`) the subtree is in place already.
    pub fn add_commit(&self, rev: &ObjectId, rejoin: Option<&str>) -> Result<i32, Die> {
        self.debug(&format!("Adding {} as '{rev}'...", self.dir));
        let imported = if self.opts.squash {
            self.new_squash_commit(None, rev)?
        } else {
            *rev
        };
        if rejoin.is_none() {
            let prefix = format!("--prefix={}", self.dir);
            let code = self.run(&["read-tree", &prefix, &imported.to_hex()])?;
            if code != 0 {
                return Ok(code);
            }
        }
        let code = self.run(&["checkout", "--", &self.dir])?;
        if code != 0 {
            return Ok(code);
        }
        let (code, out) = self.capture(&["write-tree"], None, false)?;
        if code != 0 {
            return Ok(code);
        }
        let tree = ObjectId::from_hex_any(String::from_utf8_lossy(&out).trim().as_bytes())
            .map_err(|_| Die::Exit(128))?;
        let head = self.verify("HEAD")?;
        let mut parents: Vec<ObjectId> = Vec::new();
        if let Some(h) = head
            && h != imported
        {
            parents.push(h);
        }
        parents.push(imported);
        let message = if let Some(m) = rejoin {
            m.to_string()
        } else if self.opts.squash {
            let subject = match &self.opts.message {
                Some(m) => m.clone(),
                None => msg::add_squashed_subject(&imported.to_hex(), &self.dir),
            };
            subject + "\n"
        } else {
            let subject = match &self.opts.message {
                Some(m) => m.clone(),
                None => msg::add_subject(&self.dir, &rev.to_hex()),
            };
            let mainline = head.map(|h| h.to_hex()).unwrap_or_default();
            format!(
                "{subject}\n\n{} {}\n{} {mainline}\n{} {rev}\n",
                msg::TRAILER_DIR,
                self.dir,
                msg::TRAILER_MAINLINE,
                msg::TRAILER_SPLIT
            )
        };
        let commit = self.commit_tree(&tree, &parents, &message)?;
        let code = self.run(&["reset", &commit.to_hex()])?;
        if code != 0 {
            return Ok(code);
        }
        self.say(&msg::added_dir(&self.dir));
        Ok(0)
    }

    /// `merge`: merge subproject commit `rev` into the subtree. The merge
    /// output goes to standard error for `split --rejoin`.
    pub fn cmd_merge(
        &self,
        rev: &str,
        repository: Option<&str>,
        message: Option<&str>,
        to_stderr: bool,
    ) -> Result<i32, Die> {
        let mut rev = self.commit_of(rev)?;
        self.ensure_clean()?;
        if self.opts.squash {
            let Some((old, old_sub)) = self.find_latest_squash(repository)? else {
                return Err(die(&msg::never_added(&self.dir)));
            };
            if old_sub == rev {
                // This ends the whole command, also `split --rejoin`.
                self.say(&msg::already_at(&rev.to_hex()));
                return Err(Die::Exit(0));
            }
            rev = self.new_squash_commit(Some((&old, &old_sub)), &rev)?;
            self.debug(&format!("New squash commit: {rev}"));
        }
        let mut args: Vec<String> = vec![
            "merge".into(),
            "--no-ff".into(),
            format!("-Xsubtree={}", self.dir),
        ];
        if let Some(m) = message.map(str::to_owned).or_else(|| self.opts.message.clone()) {
            args.push(format!("--message={m}"));
        }
        if let Some(g) = self.opts.gpg_arg() {
            args.push(g);
        }
        args.push(rev.to_hex());
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        if to_stderr {
            self.run_to_stderr(&argv)
        } else {
            self.run(&argv)
        }
    }

    fn cmd_pull(&self, args: &[String]) -> Result<i32, Die> {
        if args.len() != 2 {
            return Err(die(msg::PULL_ARGS));
        }
        self.ensure_clean()?;
        self.check_ref_format(&args[1])?;
        let code = self.run(&["fetch", &args[0], &args[1]])?;
        if code != 0 {
            return Ok(code);
        }
        self.cmd_merge("FETCH_HEAD", Some(&args[0]), None, false)
    }

    fn cmd_push(&self, ctx: &Context, args: &[String]) -> Result<i32, Die> {
        if args.len() != 2 {
            return Err(die(msg::PUSH_ARGS));
        }
        let repository = &args[0];
        let refspec = args[1].strip_prefix('+').unwrap_or(&args[1]);
        let (local, remote) = match refspec.split_once(':') {
            Some((l, r)) => (l, r),
            None => ("HEAD", refspec),
        };
        self.check_ref_format(remote)?;
        let rev = self.commit_of(local)?;
        self.require_dir_in(&rev)?;
        println!("{}", msg::echo_push(repository, refspec));
        // git runs the split in a command substitution: a failure is
        // reported with an empty line and exit code 1.
        let split = match split::split(ctx, self, &rev, Some(repository), true) {
            Ok(Ok(id)) => id,
            Ok(Err(_)) => {
                eprintln!();
                return Ok(1);
            }
            Err(e) => {
                e.emit();
                eprintln!();
                return Ok(1);
            }
        };
        let dst = format!("{}:refs/heads/{remote}", split.to_hex());
        self.run(&["push", repository, &dst])
    }

    fn cmd_split(&self, ctx: &Context, args: &[String]) -> Result<i32, Die> {
        let (rev, repository) = match args.len() {
            0 => {
                // Unverified, so that an unborn branch is reported.
                let (code, out) = self.capture(&["rev-parse", "HEAD"], None, false)?;
                let head = (code == 0)
                    .then(|| ObjectId::from_hex_any(String::from_utf8_lossy(&out).trim().as_bytes()).ok())
                    .flatten();
                (head, None)
            }
            1 | 2 => (Some(self.commit_of(&args[0])?), args.get(1).map(String::as_str)),
            _ => return Err(die(&msg::split_args(&args.join(" ")))),
        };
        let Some(rev) = rev else {
            return Err(die(&msg::dir_missing(&self.dir)));
        };
        self.require_dir_in(&rev)?;
        match split::split(ctx, self, &rev, repository, false)? {
            Ok(id) => {
                println!("{id}");
                Ok(0)
            }
            Err(code) => Ok(code),
        }
    }

    /// `<rev>:<prefix>` exists.
    fn require_dir_in(&self, rev: &ObjectId) -> Result<(), Die> {
        let spec = format!("{rev}:{}", self.dir);
        if self.run_silent(&["cat-file", "-e", &spec])? != 0 {
            return Err(die(&msg::dir_missing(&self.dir)));
        }
        Ok(())
    }
}

/// One option occurrence, for the "does not make sense" check.
struct Given {
    spelling: String,
    scope: Scope,
}

fn parse(argv: &[String]) -> Result<(Options, Vec<String>, Vec<Given>), Die> {
    let opts = RefCell::new(Options::default());
    let given: RefCell<Vec<Given>> = RefCell::new(Vec::new());
    let note = |spelling: String, scope: Scope| {
        given.borrow_mut().push(Given { spelling, scope });
    };
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::callback('q', "quiet", None, msg::H_QUIET, |_, _| {
                opts.borrow_mut().quiet = true;
                Ok(())
            })
            .noarg()
            .noneg(),
            Opt::callback('d', "debug", None, msg::H_DEBUG, |_, _| {
                opts.borrow_mut().debug = true;
                Ok(())
            })
            .noarg()
            .noneg(),
            Opt::callback('P', "prefix", Some(msg::ARGH_ANY), msg::H_PREFIX, |v, unset| {
                opts.borrow_mut().prefix = (!unset).then(|| v.unwrap_or_default().to_string());
                Ok(())
            })
            .literal_argh(),
            Opt::group(msg::GROUP_SPLIT),
            Opt::callback(
                None,
                "annotate",
                Some(msg::ARGH_ANY),
                msg::H_ANNOTATE,
                |v, unset| {
                    let v = (!unset).then(|| v.unwrap_or_default().to_string());
                    note(
                        match &v {
                            Some(v) => format!("--annotate={v}"),
                            None => "--no-annotate".into(),
                        },
                        Scope::Split,
                    );
                    opts.borrow_mut().annotate = v;
                    Ok(())
                },
            )
            .literal_argh(),
            Opt::callback('b', "branch", Some(msg::ARGH_ANY), msg::H_BRANCH, |v, _| {
                let v = v.unwrap_or_default().to_string();
                note(format!("--branch={v}"), Scope::Split);
                opts.borrow_mut().branch = Some(v);
                Ok(())
            })
            .literal_argh()
            .noneg(),
            Opt::callback(None, "ignore-joins", None, msg::H_IGNORE_JOINS, |_, unset| {
                note(
                    if unset { "--no-ignore-joins" } else { "--ignore-joins" }.into(),
                    Scope::Split,
                );
                opts.borrow_mut().ignore_joins = !unset;
                Ok(())
            })
            .noarg(),
            Opt::callback(None, "onto", Some(msg::ARGH_ANY), msg::H_ONTO, |v, unset| {
                let v = (!unset).then(|| v.unwrap_or_default().to_string());
                note(
                    match &v {
                        Some(v) => format!("--onto={v}"),
                        None => "--no-onto".into(),
                    },
                    Scope::Split,
                );
                opts.borrow_mut().onto = v;
                Ok(())
            })
            .literal_argh(),
            Opt::callback(None, "rejoin", None, msg::H_REJOIN, |_, unset| {
                note(
                    if unset { "--no-rejoin" } else { "--rejoin" }.into(),
                    Scope::Split,
                );
                opts.borrow_mut().rejoin = !unset;
                Ok(())
            })
            .noarg(),
            Opt::group(msg::GROUP_MERGE),
            Opt::callback(None, "squash", None, msg::H_SQUASH, |_, unset| {
                note(
                    if unset { "--no-squash" } else { "--squash" }.into(),
                    Scope::AddMerge,
                );
                opts.borrow_mut().squash = !unset;
                Ok(())
            })
            .noarg(),
            Opt::callback('m', "message", Some(msg::ARGH_ANY), msg::H_MESSAGE, |v, _| {
                let v = v.unwrap_or_default().to_string();
                note(format!("--message={v}"), Scope::AddMerge);
                opts.borrow_mut().message = Some(v);
                Ok(())
            })
            .literal_argh()
            .noneg(),
            Opt::callback(
                'S',
                "gpg-sign",
                Some(msg::ARGH_KEY_ID),
                msg::H_GPG_SIGN,
                |v, unset| {
                    opts.borrow_mut().gpg_sign =
                        Some((!unset).then(|| v.unwrap_or_default().to_string()));
                    Ok(())
                },
            )
            .optional(None),
        ],
    );
    if argv.len() <= 1 {
        print!("{}", with_help_line(&parser.usage_text(false)));
        return Err(Die::Exit(0));
    }
    let args = match parser.parse(argv) {
        Ok(p) => p.args,
        Err(e) if e.code == 0 || !e.stdout.is_empty() => {
            print!("{}", with_help_line(&e.stdout));
            return Err(Die::Exit(0));
        }
        Err(mut e) => {
            if e.stderr.contains("\n\n") {
                e.stderr = with_help_line(&e.stderr);
            }
            return Err(e.into());
        }
    };
    drop(parser);
    let given = given.into_inner();
    Ok((opts.into_inner(), args, given))
}

/// The usage as `rev-parse --parseopt` prints it, which lists `-h`.
fn with_help_line(usage: &str) -> String {
    match usage.split_once("\n\n") {
        Some((head, rest)) => format!("{head}\n\n    -h, --help            {}\n{rest}", msg::H_HELP),
        None => usage.to_string(),
    }
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let (opts, mut args, given) = match parse(argv) {
        Ok(p) => p,
        Err(Die::Exit(0)) => return Ok(0),
        Err(e) => return Err(e),
    };
    let repo = Repo::open(ctx, Mode::Required)?;
    if !repo.prefix().is_empty() || repo.setup.work_tree.is_none() {
        eprintln!("{}", msg::NOT_TOPLEVEL);
        return Ok(1);
    }
    let top = repo.work_tree()?;
    let mut env = ctx.child_env();
    if ctx.git_dir.is_some() || ctx.work_tree.is_some() {
        let git_dir = repo.setup.cwd.join(&repo.setup.git_dir);
        env.retain(|(k, _)| k != "GIT_DIR" && k != "GIT_WORK_TREE");
        env.push(("GIT_DIR".into(), git_dir.into_os_string()));
        env.push(("GIT_WORK_TREE".into(), top.clone().into_os_string()));
    }
    drop(repo);

    let name = if args.is_empty() {
        String::new()
    } else {
        args.remove(0)
    };
    let Some(cmd) = Cmd::parse(&name) else {
        return Err(die(&msg::unknown_command(&name)));
    };
    let splitting = matches!(cmd, Cmd::Split | Cmd::Push);
    for g in &given {
        let ok = match g.scope {
            Scope::Split => splitting,
            Scope::AddMerge => !splitting || opts.rejoin,
        };
        if !ok {
            return Err(die(&msg::wrong_flag(&g.spelling, cmd.name())));
        }
    }
    let dir = match &opts.prefix {
        Some(p) if !p.is_empty() => p.trim_end_matches('/').to_string(),
        _ => return Err(die(msg::NO_PREFIX)),
    };
    let exe = std::env::current_exe().map_err(|e| Die::Fatal(e.to_string()))?;
    let st = Subtree {
        opts,
        dir,
        exe,
        env,
        top,
    };
    if st.opts.debug {
        eprintln!("command: {{{}}}", cmd.name());
        eprintln!("quiet: {{{}}}", if st.opts.quiet { "1" } else { "" });
        eprintln!("dir: {{{}}}", st.dir);
        eprintln!("opts: {{{}}}", args.join(" "));
        eprintln!("gpg-sign: {{{}}}", st.opts.gpg_arg().unwrap_or_default());
        eprintln!();
    }
    let exists = std::fs::symlink_metadata(st.top.join(&st.dir)).is_ok();
    match cmd {
        Cmd::Add => {
            if exists {
                return Err(die(&msg::prefix_exists(&st.dir)));
            }
        }
        Cmd::Merge | Cmd::Pull | Cmd::Push => {
            if !exists {
                return Err(die(&msg::dir_missing(&st.dir)));
            }
        }
        Cmd::Split => {}
    }
    match cmd {
        Cmd::Add => st.cmd_add(&args),
        Cmd::Merge => {
            if args.is_empty() || args.len() > 2 {
                return Err(die(&msg::merge_args(&args.join(" "))));
            }
            st.cmd_merge(&args[0], args.get(1).map(String::as_str), None, false)
        }
        Cmd::Pull => st.cmd_pull(&args),
        Cmd::Push => st.cmd_push(ctx, &args),
        Cmd::Split => st.cmd_split(ctx, &args),
    }
}
