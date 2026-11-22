// SPDX-License-Identifier: MIT
//! `git am` (Documentation/git-am.adoc, config/am.adoc): apply a series
//! of patches from mailboxes, one commit per mail.
//!
//! The mails are split with [`rgit_mailinfo::split`], taken apart with
//! [`rgit_mailinfo::info()`] and applied with `rgit apply --index`; `-3`
//! falls back to a three-way merge ([`rgit_merge`]) of a base tree
//! rebuilt from the blob ids in the patch (`apply --build-fake-ancestor`).
//!
//! The session lives in `.git/rebase-apply` in git's layout (determined
//! by running `git am`): the mails `0001`..., `next` and `last`, the
//! options (`threeway`, `quiet`, `sign`, `utf8`, `keep`, `messageid`,
//! `scissors`, `quoted-cr`, `apply-opt`), `abort-safety`, and for the
//! current mail `info`, `msg`, `patch`, `final-commit` and
//! `author-script`. Options given with `--continue`, `--skip` or
//! `--retry` apply to the current mail only.
//!
//! Not done yet (#136): `--interactive`, rerere, `--rebasing` (the
//! `rebase --apply` back end), GPG signing.

use std::ffi::OsString;
use std::io::{IsTerminal as _, Read, Write};
use std::path::{Path, PathBuf};

use rgit_config::{ConfigSet, Environment, SystemEnv};
use rgit_hash::ObjectId;
use rgit_hook::RunOptions;
use rgit_ident::ident::{IdentConfig, Resolver as IdentResolver, Role, Strictness};
use rgit_mailinfo::QuotedCr;
use rgit_object::{ObjectKind, Signature};

use crate::messages::am as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;
use crate::sequencer::{self as seq, Fail};

const CONTINUE: i32 = 1;
const SKIP: i32 = 2;
const ABORT: i32 = 3;
const QUIT: i32 = 4;
const SHOW: i32 = 5;
const RETRY: i32 = 6;
const ALLOW_EMPTY: i32 = 7;

/// `keep` of the state directory.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Keep {
    No,
    Yes,
    NonPatch,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Empty {
    Stop,
    Drop,
    Keep,
}

/// The options saved in the state directory.
#[derive(Clone)]
struct Settings {
    threeway: bool,
    quiet: bool,
    sign: bool,
    utf8: bool,
    keep: Keep,
    message_id: bool,
    scissors: Option<bool>,
    quoted_cr: Option<QuotedCr>,
    apply_opts: Vec<String>,
}

/// Options given on the command line (`None`: not given).
#[derive(Default)]
struct Given {
    threeway: Option<bool>,
    quiet: Option<bool>,
    sign: Option<bool>,
    utf8: Option<bool>,
    keep: Option<Keep>,
    message_id: Option<bool>,
    scissors: Option<bool>,
    quoted_cr: Option<QuotedCr>,
    apply_opts: Option<Vec<String>>,
}

impl Settings {
    fn apply(&mut self, g: &Given) {
        if let Some(v) = g.threeway {
            self.threeway = v;
        }
        if let Some(v) = g.quiet {
            self.quiet = v;
        }
        if let Some(v) = g.sign {
            self.sign = v;
        }
        if let Some(v) = g.utf8 {
            self.utf8 = v;
        }
        if let Some(v) = g.keep {
            self.keep = v;
        }
        if let Some(v) = g.message_id {
            self.message_id = v;
        }
        if let Some(v) = g.scissors {
            self.scissors = Some(v);
        }
        if let Some(v) = g.quoted_cr {
            self.quoted_cr = Some(v);
        }
        if let Some(v) = &g.apply_opts {
            self.apply_opts.clone_from(v);
        }
    }
}

fn tf(b: bool) -> &'static str {
    if b { "t" } else { "f" }
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn first_line(m: &[u8]) -> &[u8] {
    let end = m.iter().position(|&b| b == b'\n').unwrap_or(m.len());
    &m[..end]
}

fn io_die(e: std::io::Error) -> Die {
    Die::Fatal(crate::global::strerror(&e))
}

/// Quotes `s` for the shell like git's `sq_quote`.
fn sq(s: &str) -> String {
    crate::context::sq_quote(s)
}

/// Splits `'a' 'b\''c'` into its words.
fn sq_dequote_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                for d in chars.by_ref() {
                    if d == '\'' {
                        break;
                    }
                    cur.push(d);
                }
            }
            '\\' => {
                if let Some(d) = chars.next() {
                    cur.push(d);
                }
                in_word = true;
            }
            ' ' | '\t' | '\n' => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            _ => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        out.push(cur);
    }
    out
}

/// The process environment with some variables replaced.
struct Overlay {
    base: SystemEnv,
    vars: Vec<(&'static str, OsString)>,
}

impl Environment for Overlay {
    fn var(&self, name: &str) -> Option<OsString> {
        match self.vars.iter().find(|(k, _)| *k == name) {
            Some((_, v)) => Some(v.clone()),
            None => self.base.var(name),
        }
    }

    fn user_home(&self, user: &str) -> Option<PathBuf> {
        self.base.user_home(user)
    }
}

/// What the info file of a mail says.
#[derive(Default)]
struct MailInfo {
    author: Vec<u8>,
    email: Vec<u8>,
    date: Vec<u8>,
    subject: Vec<u8>,
}

fn parse_info(data: &[u8]) -> MailInfo {
    let mut out = MailInfo::default();
    let mut subjects: Vec<&[u8]> = Vec::new();
    for line in data.split(|&b| b == b'\n') {
        if let Some(v) = line.strip_prefix(b"Author: ") {
            out.author = v.to_vec();
        } else if let Some(v) = line.strip_prefix(b"Email: ") {
            out.email = v.to_vec();
        } else if let Some(v) = line.strip_prefix(b"Date: ") {
            out.date = v.to_vec();
        } else if let Some(v) = line.strip_prefix(b"Subject: ") {
            subjects.push(v);
        }
    }
    out.subject = subjects.join(&b'\n');
    out
}

/// The author of the current mail, from `author-script`.
#[derive(Default, Clone)]
struct Author {
    name: String,
    email: String,
    date: String,
}

fn author_script(a: &Author) -> String {
    format!(
        "GIT_AUTHOR_NAME={}\nGIT_AUTHOR_EMAIL={}\nGIT_AUTHOR_DATE={}\n",
        sq(&a.name),
        sq(&a.email),
        sq(&a.date)
    )
}

fn read_author_script(data: &str) -> Author {
    let mut a = Author::default();
    for line in data.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = sq_dequote_words(v).join(" ");
        match k {
            "GIT_AUTHOR_NAME" => a.name = v,
            "GIT_AUTHOR_EMAIL" => a.email = v,
            "GIT_AUTHOR_DATE" => a.date = v,
            _ => {}
        }
    }
    a
}

struct Am<'c> {
    ctx: &'c mut Context,
    /// The state directory.
    dir: PathBuf,
    /// The state directory as messages name it.
    shown_dir: String,
    git_dir: PathBuf,
    work_tree: PathBuf,
    config: ConfigSet,
    saved: Settings,
    cur: Settings,
    resolvemsg: Option<String>,
    committer_date_is_author_date: bool,
    ignore_date: bool,
    no_verify: bool,
    empty: Empty,
}

/// How a patch ended when it did not end in a commit.
enum Stop {
    /// Failed; the message and hints are printed, exit 128.
    Failed,
    /// Pass this on.
    Die(Die),
}

impl From<Die> for Stop {
    fn from(d: Die) -> Stop {
        Stop::Die(d)
    }
}

impl From<Fail> for Stop {
    fn from(f: Fail) -> Stop {
        match f {
            Fail::Error(m) => {
                eprintln!("error: {m}");
                Stop::Failed
            }
            Fail::Printed => Stop::Failed,
            Fail::Die(d) => Stop::Die(d),
        }
    }
}

fn out(line: &str) {
    let mut o = std::io::stdout().lock();
    let _ = writeln!(o, "{line}");
    let _ = o.flush();
}

impl<'c> Am<'c> {
    fn repo(&self) -> Result<Repo, Die> {
        Repo::open(self.ctx, Mode::Required)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn read(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.path(name)).ok()
    }

    fn write(&self, name: &str, data: impl AsRef<[u8]>) -> Result<(), Die> {
        std::fs::write(self.path(name), data).map_err(io_die)
    }

    fn number(&self, name: &str) -> usize {
        self.read(name)
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    fn msgnum(&self, n: usize) -> String {
        let prec = self.number("prec").clamp(4, 9);
        format!("{n:0prec$}")
    }

    fn in_progress(dir: &Path) -> bool {
        dir.join("last").is_file() && dir.join("next").is_file()
    }

    fn save_settings(&self) -> Result<(), Die> {
        let s = &self.saved;
        self.write("threeway", tf(s.threeway))?;
        self.write("quiet", tf(s.quiet))?;
        self.write("sign", tf(s.sign))?;
        self.write("utf8", tf(s.utf8))?;
        self.write(
            "keep",
            match s.keep {
                Keep::No => "f",
                Keep::Yes => "t",
                Keep::NonPatch => "b",
            },
        )?;
        self.write("messageid", tf(s.message_id))?;
        self.write(
            "scissors",
            match s.scissors {
                None => "",
                Some(b) => tf(b),
            },
        )?;
        self.write("quoted-cr", s.quoted_cr.map_or("", QuotedCr::name))?;
        let mut opts = String::new();
        for o in &s.apply_opts {
            opts.push(' ');
            opts.push_str(&sq(o));
        }
        self.write("apply-opt", opts)?;
        Ok(())
    }

    fn load_settings(&mut self) {
        let flag = |name: &str| self.read(name).map(|s| s.trim() == "t").unwrap_or(false);
        let s = Settings {
            threeway: flag("threeway"),
            quiet: flag("quiet"),
            sign: flag("sign"),
            utf8: self.read("utf8").map(|s| s.trim() != "f").unwrap_or(true),
            keep: match self.read("keep").as_deref().map(str::trim) {
                Some("t") => Keep::Yes,
                Some("b") => Keep::NonPatch,
                _ => Keep::No,
            },
            message_id: flag("messageid"),
            scissors: match self.read("scissors").as_deref().map(str::trim) {
                Some("t") => Some(true),
                Some("f") => Some(false),
                _ => None,
            },
            quoted_cr: self
                .read("quoted-cr")
                .and_then(|s| QuotedCr::parse(s.trim())),
            apply_opts: sq_dequote_words(&self.read("apply-opt").unwrap_or_default()),
        };
        self.saved = s.clone();
        self.cur = s;
    }

    /// The hints after a failure.
    fn advise_failure(&self, show_patch: bool, empty: bool) -> Result<(), Die> {
        let mut text = String::new();
        if show_patch {
            text.push_str(msg::HINT_SHOW_PATCH);
            text.push('\n');
        }
        match &self.resolvemsg {
            Some(r) => {
                text.push_str(r);
                text.push('\n');
            }
            None => {
                text.push_str(msg::HINT_CONTINUE);
                text.push('\n');
                text.push_str(msg::HINT_SKIP);
                text.push('\n');
                if empty {
                    text.push_str(msg::HINT_ALLOW_EMPTY);
                    text.push('\n');
                }
                text.push_str(msg::HINT_ABORT);
                text.push('\n');
            }
        }
        let repo = self.repo()?;
        self.ctx
            .advisor(&repo.config)?
            .advise(Advice::MergeConflict, text.trim_end_matches('\n'))
            .map_err(Die::Fatal)
    }

    /// Runs `rgit <args>` in the work tree.
    fn run_self(
        &self,
        args: &[String],
        env: &[(&str, OsString)],
        quiet: bool,
        capture: bool,
    ) -> Result<(i32, Vec<u8>), Die> {
        let _ = std::io::stdout().flush();
        let exe = std::env::current_exe().map_err(io_die)?;
        let mut cmd = std::process::Command::new(exe);
        cmd.args(args)
            .current_dir(&self.work_tree)
            .envs(self.ctx.child_env())
            .envs(env.iter().map(|(k, v)| (*k, v)));
        use std::process::Stdio;
        cmd.stdin(Stdio::null());
        cmd.stderr(if quiet { Stdio::null() } else { Stdio::inherit() });
        cmd.stdout(if capture {
            Stdio::piped()
        } else if quiet {
            Stdio::null()
        } else {
            Stdio::inherit()
        });
        let output = cmd.output().map_err(io_die)?;
        Ok((output.status.code().unwrap_or(128), output.stdout))
    }

    fn head(&self) -> Result<Option<ObjectId>, Die> {
        let mut repo = self.repo()?;
        Ok(seq::head_commit(&mut repo))
    }

    fn head_tree(&self, repo: &Repo, head: Option<ObjectId>) -> Result<ObjectId, Die> {
        match head {
            Some(h) => seq::tree_of(repo, &h).map_err(|_| Die::Fatal(h.to_hex())),
            None => Ok(ObjectId::empty_tree(repo.hash)),
        }
    }

    /// Paths where the index differs from `HEAD`.
    fn dirty_paths(&self) -> Result<Vec<String>, Die> {
        let repo = self.repo()?;
        let head = self.head()?;
        let (index, _) = repo.read_index()?;
        let mut tree = crate::tree_changes::Flat::new();
        if let Some(h) = head {
            let t = self.head_tree(&repo, Some(h))?;
            crate::tree_changes::flatten_tree(&repo, &t, &mut tree)?;
        }
        let mut dirty = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for e in index.entries() {
            let p = e.path().to_vec();
            if !seen.insert(p.clone()) {
                continue;
            }
            match tree.get(&p) {
                Some(s) if s.oid == e.id && s.mode == e.mode.0 && e.stage() == rgit_index::Stage::MERGED => {}
                _ => dirty.push(lossy(&p)),
            }
        }
        for p in tree.keys() {
            if !seen.contains(p) {
                dirty.push(lossy(p));
            }
        }
        dirty.sort();
        Ok(dirty)
    }

    // ------------------------------------------------------------------
    // Parsing a mail.

    fn mailinfo_options(&self) -> rgit_mailinfo::Options {
        let mut o = rgit_mailinfo::Options::default();
        let _ = crate::commands::mailinfo::config_defaults(&self.config, &mut o);
        o.keep_subject = self.cur.keep == Keep::Yes;
        o.keep_non_patch = self.cur.keep == Keep::NonPatch;
        o.add_message_id = self.cur.message_id;
        if let Some(s) = self.cur.scissors {
            o.use_scissors = s;
        }
        if let Some(q) = self.cur.quoted_cr {
            o.quoted_cr = q;
        }
        o.metainfo_charset = self
            .cur
            .utf8
            .then(|| crate::commands::mailinfo::commit_encoding(&self.config));
        o
    }

    /// Takes mail `n` apart; `Ok(false)` when it has no patch and is to be
    /// dropped.
    fn parse_mail(&self, n: usize) -> Result<bool, Stop> {
        let mail_path = self.path(&self.msgnum(n));
        let data = std::fs::read(&mail_path).map_err(io_die)?;
        let info = rgit_mailinfo::info(&data, &self.mailinfo_options());
        rgit_mailinfo_report(&info);
        if info.failed {
            return Err(Stop::Die(Die::Fatal(msg::could_not_parse(
                &mail_path.to_string_lossy(),
            ))));
        }
        self.write("info", &info.info)?;
        self.write("msg", &info.msg)?;
        self.write("patch", &info.patch)?;
        if info.format_flowed {
            eprintln!("warning: {}", msg::FORMAT_FLOWED);
        }
        let mi = parse_info(&info.info);
        if mi.subject == msg::PINE_INTERNAL_DATA.as_bytes() {
            // The first message of a Pine mailbox holds the folder's data.
            return Ok(false);
        }
        let mut message = mi.subject.clone();
        message.extend_from_slice(b"\n\n");
        message.extend_from_slice(&info.msg);
        let mut message = rgit_commit::message::stripspace(&message, None);
        if self.cur.sign {
            message = self.signoff(&message)?;
        }
        let author = Author {
            name: lossy(&mi.author),
            email: lossy(&mi.email),
            date: lossy(&mi.date),
        };
        self.write("author-script", author_script(&author))?;
        self.write("final-commit", &message)?;
        if info.patch.is_empty() {
            let subject = lossy(first_line(&message));
            match self.empty {
                Empty::Drop => {
                    out(&msg::skipping(&subject));
                    return Ok(false);
                }
                Empty::Keep => {
                    out(&msg::creating_empty(&subject));
                }
                Empty::Stop => {
                    out(msg::PATCH_IS_EMPTY);
                    self.advise_failure(false, true)?;
                    return Err(Stop::Die(Die::Exit(128)));
                }
            }
        }
        Ok(true)
    }

    fn signoff(&self, message: &[u8]) -> Result<Vec<u8>, Die> {
        let repo = self.repo()?;
        let committer = seq::ident(&repo, Role::Committer)?;
        let mut line = b"Signed-off-by: ".to_vec();
        line.extend_from_slice(&committer.name);
        line.extend_from_slice(b" <");
        line.extend_from_slice(&committer.email);
        line.push(b'>');
        let conf = seq::trailer_config(&repo)?;
        Ok(rgit_trailer::append_signoff(message, &line, &conf, b"#"))
    }

    // ------------------------------------------------------------------
    // Applying.

    fn apply_args(&self, extra: &[&str]) -> Vec<String> {
        let mut args: Vec<String> = vec!["apply".into()];
        args.extend(extra.iter().map(|s| (*s).to_owned()));
        args.extend(self.cur.apply_opts.iter().cloned());
        args.push(self.patch_arg());
        args
    }

    /// The patch file as `apply` (run in the work tree) is given it: relative
    /// to the work tree when the state directory is inside it.
    fn patch_arg(&self) -> String {
        let p = self.path("patch");
        p.strip_prefix(&self.work_tree)
            .unwrap_or(&p)
            .to_string_lossy()
            .into_owned()
    }

    /// Applies the patch to index and work tree; with `-3` falls back to
    /// a three-way merge. `Ok(false)`: failed (messages printed).
    fn apply_patch(&self, subject: &str) -> Result<bool, Stop> {
        if !self.cur.threeway {
            let (code, _) = self.run_self(&self.apply_args(&["--index"]), &[], false, false)?;
            return Ok(code == 0);
        }
        let (code, _) = self.run_self(&self.apply_args(&["--index"]), &[], true, false)?;
        if code == 0 {
            return Ok(true);
        }
        self.three_way(subject)
    }

    fn three_way(&self, subject: &str) -> Result<bool, Stop> {
        let tmp = self.path("patch-merge-index");
        let _ = std::fs::remove_file(&tmp);
        let fake = format!("--build-fake-ancestor={}", tmp.to_string_lossy());
        let (code, _) = self.run_self(
            &{
                let mut a = vec!["apply".to_owned(), fake];
                a.extend(self.cur.apply_opts.iter().cloned());
                a.push(self.patch_arg());
                a
            },
            &[],
            false,
            false,
        )?;
        if code != 0 {
            eprintln!("error: {}", msg::NO_FAKE_ANCESTOR);
            return Ok(false);
        }
        let env = [("GIT_INDEX_FILE", tmp.clone().into_os_string())];
        let tree_of_tmp = |am: &Am| -> Result<Option<ObjectId>, Die> {
            let (code, stdout) = am.run_self(&["write-tree".into()], &env, false, true)?;
            if code != 0 {
                return Ok(None);
            }
            let repo = am.repo()?;
            let hex = lossy(&stdout);
            Ok(ObjectId::from_hex(repo.hash, hex.trim().as_bytes()).ok())
        };
        let Some(base) = tree_of_tmp(self)? else {
            return Ok(false);
        };
        let repo = self.repo()?;
        let head = self.head()?;
        let head_tree = self.head_tree(&repo, head)?;
        if !self.cur.quiet {
            out(msg::USING_INDEX_INFO);
            let mut base_flat = crate::tree_changes::Flat::new();
            crate::tree_changes::flatten_tree(&repo, &base, &mut base_flat)?;
            let mut head_flat = crate::tree_changes::Flat::new();
            crate::tree_changes::flatten_tree(&repo, &head_tree, &mut head_flat)?;
            let mut text = String::new();
            for (p, side) in &base_flat {
                let status = match head_flat.get(p) {
                    None => "D",
                    Some(h) if h == side => continue,
                    Some(_) => "M",
                };
                text.push_str(status);
                text.push('\t');
                let mut q = Vec::new();
                crate::quote::write_quoted(&mut q, p, repo.quote_path());
                text.push_str(&lossy(&q));
                text.push('\n');
            }
            let mut o = std::io::stdout().lock();
            let _ = o.write_all(text.as_bytes());
            let _ = o.flush();
        }
        let mut args = vec!["apply".to_owned(), "--cached".to_owned()];
        args.extend(self.cur.apply_opts.iter().cloned());
        args.push(self.patch_arg());
        let (code, _) = self.run_self(&args, &env, false, false)?;
        if code != 0 {
            return Ok(false);
        }
        let Some(theirs) = tree_of_tmp(self)? else {
            return Ok(false);
        };
        let _ = std::fs::remove_file(&tmp);
        if !self.cur.quiet {
            out(msg::FALLING_BACK);
        }
        let opts = rgit_merge::Options {
            branch1: b"HEAD".to_vec(),
            branch2: subject.as_bytes().to_vec(),
            ancestor: b"constructed merge base".to_vec(),
            style: crate::commands::merge_file::config_style(&repo.config)?,
            ..rgit_merge::Options::default()
        };
        let result = rgit_merge::merge_trees(repo.odb(), &base, &head_tree, &theirs, &opts)
            .map_err(|e| Die::Fatal(e.to_string()))?;
        if result.tree == ObjectId::empty_tree(repo.hash) {
            repo.odb()
                .write(ObjectKind::Tree, b"")
                .map_err(|e| Die::Fatal(e.to_string()))?;
        }
        seq::two_way(&repo, head_tree, result.tree, Some(&result), true)?;
        if !self.cur.quiet {
            let mut text = Vec::new();
            for m in &result.messages {
                text.extend_from_slice(&m.text);
                text.push(b'\n');
            }
            let mut o = std::io::stdout().lock();
            let _ = o.write_all(&text);
            let _ = o.flush();
        }
        if !result.clean {
            eprintln!("error: {}", msg::FAILED_TO_MERGE);
            return Ok(false);
        }
        Ok(true)
    }

    // ------------------------------------------------------------------
    // Committing.

    fn commit(&self, message: &[u8]) -> Result<(), Die> {
        if message.contains(&0) {
            return Err(Die::Error(msg::NUL_IN_MESSAGE.to_owned(), 128));
        }
        let mut repo = self.repo()?;
        let (index, _) = repo.read_index()?;
        let tree = crate::commands::write_tree::write_tree(&repo, &index, false, None)?;
        drop(index);
        let head = seq::head_commit(&mut repo);
        let author_data = self.read("author-script").unwrap_or_default();
        let a = read_author_script(&author_data);
        let icfg = IdentConfig::from_config(&repo.config).map_err(|p| {
            p.print();
            Die::Exit(rgit_options::EXIT_FATAL)
        })?;
        let author_env = Overlay {
            base: SystemEnv::default(),
            vars: vec![
                ("GIT_AUTHOR_NAME", a.name.clone().into()),
                ("GIT_AUTHOR_EMAIL", a.email.clone().into()),
                (
                    "GIT_AUTHOR_DATE",
                    if self.ignore_date {
                        OsString::new()
                    } else {
                        a.date.clone().into()
                    },
                ),
            ],
        };
        let author = IdentResolver::new(&icfg, &author_env)
            .ident(Role::Author, Strictness::Strict)
            .map_err(|e| {
                eprint!("{}", e.stderr_text());
                Die::Exit(rgit_options::EXIT_FATAL)
            })?;
        let mut committer_vars = Vec::new();
        if self.committer_date_is_author_date {
            committer_vars.push((
                "GIT_COMMITTER_DATE",
                OsString::from(format!("@{} {:+05}", author.time, author.tz)),
            ));
        }
        let committer_env = Overlay {
            base: SystemEnv::default(),
            vars: committer_vars,
        };
        let committer = IdentResolver::new(&icfg, &committer_env)
            .ident(Role::Committer, Strictness::Strict)
            .map_err(|e| {
                eprint!("{}", e.stderr_text());
                Die::Exit(rgit_options::EXIT_FATAL)
            })?;
        let to_sig = |raw: rgit_ident::ident::Ident| -> Result<Signature, Die> {
            let raw = raw.to_bstring();
            rgit_object::SignatureRef::parse(&raw)
                .and_then(|s| s.to_owned())
                .ok_or_else(|| Die::Fatal(lossy(&raw)))
        };
        let author = to_sig(author)?;
        let committer = to_sig(committer)?;
        let encoding = crate::commands::mailinfo::commit_encoding(&repo.config);
        let encoding = (!rgit_mailinfo::charset::same(&encoding, "UTF-8"))
            .then(|| encoding.into_bytes());
        let parents: Vec<ObjectId> = head.into_iter().collect();
        let new = rgit_commit::NewCommit {
            tree,
            parents: &parents,
            author: &author,
            committer: &committer,
            encoding: encoding.as_deref(),
            message,
        };
        let oid = rgit_commit::create_commit(repo.odb(), &new)
            .map_err(|e| Die::Fatal(e.to_string()))?;
        let action = std::env::var("GIT_REFLOG_ACTION")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "am".to_owned());
        let what = format!("{action}: {}", lossy(first_line(message)));
        seq::update_head(self.ctx, &repo, oid, head, &what)?;
        self.write("abort-safety", format!("{}\n", oid.to_hex()))?;
        Ok(())
    }

    fn run_hook(&self, name: &str, args: &[String]) -> Result<bool, Die> {
        let repo = self.repo()?;
        let hooks = crate::hooks::for_repo(self.ctx, &repo.setup, &repo.config)?;
        if !hooks.exists(name) {
            return Ok(true);
        }
        let _ = std::io::stdout().flush();
        let o = RunOptions::with_args(args.iter().map(String::as_str));
        Ok(crate::hooks::run(&hooks, name, &o)?.ok())
    }

    // ------------------------------------------------------------------
    // The loop.

    fn next_patch(&mut self) -> Result<(), Die> {
        let n = self.number("next") + 1;
        self.write("next", format!("{n}\n"))?;
        for f in ["author-script", "final-commit"] {
            let _ = std::fs::remove_file(self.path(f));
        }
        // The command line options were for the patch just finished.
        self.cur = self.saved.clone();
        Ok(())
    }

    /// Fails the current patch: "Patch failed at", hints, exit 128.
    fn fail_patch(&self, n: usize, subject: &str) -> Result<i32, Die> {
        out(&msg::patch_failed(&self.msgnum(n), subject));
        self.advise_failure(true, false)?;
        Ok(128)
    }

    /// Applies the patches from `next` on. With `retry` the current one is
    /// not parsed again.
    fn run(&mut self, mut retry: bool) -> CmdResult {
        self.refresh_index()?;
        loop {
            let n = self.number("next");
            let last = self.number("last");
            if n > last || n == 0 {
                break;
            }
            if !self.path(&self.msgnum(n)).is_file() {
                self.next_patch()?;
                retry = false;
                continue;
            }
            if !retry {
                match self.parse_mail(n) {
                    Ok(true) => {}
                    Ok(false) => {
                        self.next_patch()?;
                        continue;
                    }
                    Err(Stop::Failed) => return Ok(128),
                    Err(Stop::Die(d)) => return Err(d),
                }
                if !self.no_verify {
                    let shown = self.path("final-commit").to_string_lossy().into_owned();
                    if !self.run_hook("applypatch-msg", &[shown])? {
                        return Ok(1);
                    }
                }
            }
            retry = false;
            let message = std::fs::read(self.path("final-commit")).unwrap_or_default();
            let subject = lossy(first_line(&message));
            let patch_empty = std::fs::metadata(self.path("patch"))
                .map(|m| m.len() == 0)
                .unwrap_or(true);
            if !self.cur.quiet && !(patch_empty && self.empty == Empty::Keep) {
                out(&msg::applying(&subject));
            }
            if !patch_empty {
                match self.apply_patch(&subject) {
                    Ok(true) => {}
                    Ok(false) | Err(Stop::Failed) => return self.fail_patch(n, &subject),
                    Err(Stop::Die(d)) => return Err(d),
                }
            }
            if !self.no_verify && !self.run_hook("pre-applypatch", &[])? {
                return Ok(1);
            }
            self.commit(&message)?;
            let _ = self.run_hook("post-applypatch", &[])?;
            self.next_patch()?;
        }
        let _ = std::fs::remove_dir_all(&self.dir);
        crate::maintenance::run_auto(self.ctx);
        Ok(0)
    }

    // ------------------------------------------------------------------
    // Resuming.

    /// `--continue` (and `--allow-empty`).
    fn resolved(&mut self, allow_empty: bool) -> CmdResult {
        let mut message = std::fs::read(self.path("final-commit")).unwrap_or_default();
        let subject = lossy(first_line(&message));
        out(&msg::applying(&subject));
        let repo = self.repo()?;
        let (index, _) = repo.read_index()?;
        if index.has_conflicts() {
            out(msg::UNMERGED);
            self.advise_failure(false, false)?;
            return Ok(128);
        }
        let head = self.head()?;
        let head_tree = self.head_tree(&repo, head)?;
        if seq::index_matches_tree(&repo, &index, &head_tree)? {
            let patch_empty = std::fs::metadata(self.path("patch"))
                .map(|m| m.len() == 0)
                .unwrap_or(false);
            if !(allow_empty && patch_empty) {
                out(msg::NO_CHANGES);
                self.advise_failure(false, false)?;
                return Ok(128);
            }
            out(msg::RECORDED_EMPTY);
        }
        drop(index);
        if self.cur.sign {
            message = self.signoff(&message)?;
        }
        self.commit(&message)?;
        let _ = self.run_hook("post-applypatch", &[])?;
        self.next_patch()?;
        self.run(false)
    }

    /// Resets index and work tree to `HEAD` (to the empty tree on an
    /// unborn branch).
    /// Refuses to reset when a path of the index is now a directory with
    /// files in it (they would be lost). Prints the error; false then.
    fn safe_to_reset(&self) -> Result<bool, Die> {
        let repo = self.repo()?;
        let (index, _) = repo.read_index()?;
        let mut ok = true;
        let mut last: Option<&[u8]> = None;
        for e in index.entries() {
            if last == Some(e.path()) {
                continue;
            }
            last = Some(e.path());
            let p = self.work_tree.join(lossy(e.path()));
            let is_dir = std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_dir());
            if is_dir && std::fs::read_dir(&p).is_ok_and(|mut d| d.next().is_some()) {
                eprintln!(
                    "error: {}",
                    rgit_unpack::messages::not_uptodate_dir(&lossy(e.path()))
                );
                ok = false;
            }
        }
        Ok(ok)
    }

    fn reset_to_head(&mut self) -> Result<i32, Die> {
        if !self.safe_to_reset()? {
            return Ok(128);
        }
        match self.head()? {
            Some(h) => {
                // `reset` records ORIG_HEAD, which `--abort` still needs.
                let orig_path = self.git_dir.join("ORIG_HEAD");
                let orig = std::fs::read(&orig_path).ok();
                let code = seq::reset_merge(self.ctx, &h.to_hex());
                match orig {
                    Some(o) => {
                        let _ = std::fs::write(&orig_path, o);
                    }
                    None => {
                        let _ = std::fs::remove_file(&orig_path);
                    }
                }
                code
            }
            None => {
                self.clear_index()?;
                Ok(0)
            }
        }
    }

    /// Refreshes the stat data of the index entries, so that `apply`
    /// does not take unchanged files for modified ones.
    fn refresh_index(&self) -> Result<(), Die> {
        let repo = self.repo()?;
        let path = repo.setup.cwd.join(repo.index_path());
        let Ok(lock) = rgit_fs::LockFile::acquire(&path, &repo.index_lock_options()) else {
            return Ok(());
        };
        let (mut index, mtime) = repo.read_index()?;
        let checker = repo.checker(mtime)?;
        crate::switch_branch::refresh_stat(&mut index, &checker);
        if let Some(reference) = mtime {
            let _ = rgit_worktree::stat::smudge_racy_entries(&mut index, reference, &checker);
        }
        crate::switch_branch::write_index(&repo, &index, lock)
    }

    /// Removes every path of the index from the work tree and empties the
    /// index.
    fn clear_index(&self) -> Result<(), Die> {
        let repo = self.repo()?;
        let (index, _) = repo.read_index()?;
        for e in index.entries() {
            let p = self.work_tree.join(lossy(e.path()));
            let _ = std::fs::remove_file(&p);
            let mut parent = p.parent();
            while let Some(d) = parent {
                if d == self.work_tree || std::fs::remove_dir(d).is_err() {
                    break;
                }
                parent = d.parent();
            }
        }
        let empty = rgit_index::Index::new(repo.hash);
        let path = repo.setup.cwd.join(repo.index_path());
        let lock = rgit_fs::LockFile::acquire(&path, &rgit_fs::LockOptions::default())
            .map_err(crate::commands::read_tree::lock_failed)?;
        crate::switch_branch::write_index(&repo, &empty, lock)?;
        Ok(())
    }

    fn skip(&mut self) -> CmdResult {
        let code = self.reset_to_head()?;
        if code != 0 {
            return Ok(code);
        }
        self.next_patch()?;
        self.run(false)
    }

    fn abort(&mut self) -> CmdResult {
        if !Am::in_progress(&self.dir) {
            let _ = std::fs::remove_dir_all(&self.dir);
            return Ok(0);
        }
        if self.path("dirtyindex").exists() {
            let _ = std::fs::remove_dir_all(&self.dir);
            return Ok(0);
        }
        let head = self.head()?;
        let safety = self.read("abort-safety").map(|s| s.trim().to_owned());
        let head_hex = head.map(|h| h.to_hex()).unwrap_or_default();
        if safety.as_deref().unwrap_or("") != head_hex {
            eprintln!("warning: {}", msg::MOVED_HEAD);
            let _ = std::fs::remove_dir_all(&self.dir);
            return Ok(0);
        }
        let orig = std::fs::read_to_string(self.git_dir.join("ORIG_HEAD"))
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        if !self.safe_to_reset()? {
            return Ok(128);
        }
        match orig {
            Some(o) => {
                let code = seq::reset_merge(self.ctx, &o)?;
                if code != 0 {
                    return Ok(code);
                }
            }
            None => {
                // The session started on an unborn branch.
                self.clear_index()?;
                let repo = self.repo()?;
                if let Some(target) = repo.refs.head_target() {
                    let setup = &repo.setup;
                    let config = crate::refs_support::load_config(self.ctx, setup)?;
                    let store = crate::refs_support::ref_store(setup, &config);
                    let objects = crate::refs_support::Objects::open(setup);
                    let mut hooks =
                        crate::refs_support::UpdateHooks::new(setup, &config, &objects);
                    let mut tx = rgit_refs::Transaction::new(&store);
                    let _ = tx
                        .push(rgit_refs::RefUpdate::delete(target.clone(), repo.hash, None))
                        .and_then(|()| tx.commit(&mut hooks));
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
        Ok(0)
    }

    fn show_current_patch(&self, diff: bool) -> CmdResult {
        let name = if diff {
            "patch".to_owned()
        } else {
            self.msgnum(self.number("next"))
        };
        let data = std::fs::read(self.path(&name)).map_err(|e| {
            Die::Error(
                msg::could_not_open(
                    &self.path(&name).to_string_lossy(),
                    &crate::global::strerror(&e),
                ),
                128,
            )
        })?;
        crate::pager::setup(self.ctx, "am", false)?;
        let mut o = std::io::stdout().lock();
        let _ = o.write_all(&data);
        let _ = o.flush();
        Ok(0)
    }
}

fn rgit_mailinfo_report(info: &rgit_mailinfo::Info) {
    crate::commands::mailinfo::report(info);
}

// ----------------------------------------------------------------------
// Reading the mailboxes.

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Mbox,
    Mboxrd,
    Stgit,
    StgitSeries,
    Hg,
}

fn is_header_line(line: &[u8]) -> bool {
    let Some(colon) = line.iter().position(|&b| b == b':') else {
        return false;
    };
    colon > 0 && line[..colon].iter().all(|&b| b > b' ' && b < 0x7f)
}

/// Guesses the format of `data` (the first input).
fn detect_format(data: &[u8]) -> Option<Format> {
    let mut lines = data
        .split(|&b| b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
        .skip_while(|l| l.iter().all(u8::is_ascii_whitespace));
    let l1 = lines.next()?;
    if l1.is_empty() {
        return None;
    }
    if rgit_mailinfo::split::is_from_line(l1) || l1.starts_with(b"From ") {
        return Some(Format::Mbox);
    }
    if l1.starts_with(msg::STGIT_SERIES_MARK.as_bytes()) {
        return Some(Format::StgitSeries);
    }
    if l1 == msg::HG_MARK.as_bytes() {
        return Some(Format::Hg);
    }
    let l2 = lines.next();
    let l3 = lines.next();
    if l2 == Some(b"".as_slice())
        && l3.is_some_and(|l| {
            l.starts_with(b"From:") || l.starts_with(b"Author:") || l.starts_with(b"Date:")
        })
    {
        return Some(Format::Stgit);
    }
    is_header_line(l1).then_some(Format::Mbox)
}

/// An stgit patch as a mail: `<subject>\n\n<headers>\n\n<body>`.
fn stgit_to_mail(data: &[u8]) -> Vec<u8> {
    let lines: Vec<&[u8]> = data.split_inclusive(|&b| b == b'\n').collect();
    let mut i = 0;
    let mut subject = Vec::new();
    while i < lines.len() && lines[i] != b"\n" {
        subject.extend_from_slice(lines[i].strip_suffix(b"\n").unwrap_or(lines[i]));
        i += 1;
    }
    while i < lines.len() && lines[i] == b"\n" {
        i += 1;
    }
    let mut out = Vec::new();
    while i < lines.len() && lines[i] != b"\n" {
        let l = lines[i];
        if let Some(v) = l.strip_prefix(b"Author:") {
            out.extend_from_slice(b"From:");
            out.extend_from_slice(v);
        } else if l.starts_with(b"From:") || l.starts_with(b"Date:") {
            out.extend_from_slice(l);
        } else {
            break;
        }
        i += 1;
    }
    out.extend_from_slice(b"Subject: ");
    out.extend_from_slice(&subject);
    out.extend_from_slice(b"\n\n");
    while i < lines.len() && lines[i] == b"\n" {
        i += 1;
    }
    for l in &lines[i..] {
        out.extend_from_slice(l);
    }
    out
}

/// An hg export as a mail.
fn hg_to_mail(data: &[u8]) -> Vec<u8> {
    let lines: Vec<&[u8]> = data.split_inclusive(|&b| b == b'\n').collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() && lines[i].starts_with(b"# ") {
        let l = lines[i].strip_suffix(b"\n").unwrap_or(lines[i]);
        if let Some(user) = l.strip_prefix(b"# User ") {
            out.extend_from_slice(b"From: ");
            out.extend_from_slice(user);
            out.push(b'\n');
        } else if let Some(date) = l.strip_prefix(b"# Date ") {
            let text = lossy(date);
            let mut parts = text.split_whitespace();
            let secs: Option<i64> = parts.next().and_then(|s| s.parse().ok());
            let west: Option<i64> = parts.next().and_then(|s| s.parse().ok());
            if let (Some(secs), Some(west)) = (secs, west) {
                let mins = -west / 60;
                let tz = (mins / 60 * 100 + mins % 60) as i32;
                let formatted = rgit_date::show_date(
                    secs.max(0) as u64,
                    tz,
                    &rgit_date::DateMode::new(rgit_date::DateStyle::Rfc2822),
                    &rgit_date::DateContext::from_env(),
                )
                .unwrap_or_default();
                out.extend_from_slice(b"Date: ");
                out.extend_from_slice(formatted.as_bytes());
                out.push(b'\n');
            }
        }
        i += 1;
    }
    let mut subject = Vec::new();
    while i < lines.len() && lines[i] != b"\n" {
        if !subject.is_empty() {
            subject.push(b' ');
        }
        subject.extend_from_slice(lines[i].strip_suffix(b"\n").unwrap_or(lines[i]));
        i += 1;
    }
    out.extend_from_slice(b"Subject: ");
    out.extend_from_slice(&subject);
    out.extend_from_slice(b"\n\n");
    while i < lines.len() && lines[i] == b"\n" {
        i += 1;
    }
    for l in &lines[i..] {
        out.extend_from_slice(l);
    }
    out
}

/// The mails of all inputs (`None`: standard input).
fn read_mails(
    inputs: &[PathBuf],
    format: Option<Format>,
    keep_cr: bool,
) -> Result<Result<Vec<Vec<u8>>, ()>, Die> {
    let mut datas: Vec<(Option<PathBuf>, Vec<u8>)> = Vec::new();
    if inputs.is_empty() {
        let mut data = Vec::new();
        std::io::stdin()
            .lock()
            .read_to_end(&mut data)
            .map_err(io_die)?;
        datas.push((None, data));
    } else {
        for p in inputs {
            if p.is_dir() {
                for f in crate::commands::mailsplit::maildir_files(p) {
                    let d = std::fs::read(&f).map_err(io_die)?;
                    datas.push((Some(f), d));
                }
                continue;
            }
            let d = std::fs::read(p).map_err(|e| {
                Die::Fatal(msg::could_not_open(
                    &p.to_string_lossy(),
                    &crate::global::strerror(&e),
                ))
            })?;
            datas.push((Some(p.clone()), d));
        }
    }
    let format = match format {
        Some(f) => f,
        None => match datas.first().and_then(|(_, d)| detect_format(d)) {
            Some(f) => f,
            None => return Ok(Err(())),
        },
    };
    let mut mails = Vec::new();
    for (path, data) in datas {
        match format {
            Format::Mbox | Format::Mboxrd => {
                let opts = rgit_mailinfo::split::Options {
                    keep_cr,
                    mboxrd: format == Format::Mboxrd,
                    allow_bare: true,
                };
                match rgit_mailinfo::split::split(&data, &opts) {
                    Ok(m) => mails.extend(m),
                    Err(_) => return Ok(Err(())),
                }
            }
            Format::Stgit => mails.push(stgit_to_mail(&data)),
            Format::Hg => mails.push(hg_to_mail(&data)),
            Format::StgitSeries => {
                let base = path
                    .as_deref()
                    .and_then(Path::parent)
                    .map(Path::to_path_buf)
                    .unwrap_or_default();
                for line in String::from_utf8_lossy(&data).lines() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }
                    let d = std::fs::read(base.join(line)).map_err(io_die)?;
                    mails.push(stgit_to_mail(&d));
                }
            }
        }
    }
    Ok(Ok(mails))
}

// ----------------------------------------------------------------------

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let interactive = Cell::new(-1);
    let no_verify = Cell::new(0);
    let binary = Cell::new(-1);
    let threeway = Cell::new(-1);
    let quiet = Cell::new(-1);
    let signoff = Cell::new(-1);
    let utf8 = Cell::new(-1);
    let keep = Cell::new(-1);
    let keep_non_patch = Cell::new(-1);
    let message_id = Cell::new(-1);
    let keep_cr = Cell::new(-1);
    let scissors = Cell::new(-1);
    let quoted_cr: Cell<Option<QuotedCr>> = Cell::new(None);
    let apply_opts: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let apply_given = Cell::new(false);
    let patch_format: RefCell<Option<String>> = RefCell::new(None);
    let resolvemsg: RefCell<Option<String>> = RefCell::new(None);
    let cmd = Cell::new(0);
    let show: RefCell<Option<String>> = RefCell::new(None);
    let show_conflict: RefCell<Option<String>> = RefCell::new(None);
    let committer_date = Cell::new(-1);
    let ignore_date = Cell::new(-1);
    let rerere = Cell::new(-1);
    let gpg_sign = Cell::new(false);
    let empty: Cell<Empty> = Cell::new(Empty::Stop);
    let empty_error: RefCell<Option<String>> = RefCell::new(None);
    let rebasing = Cell::new(0);
    let push_apply = |name: &str, v: Option<&str>, unset: bool, with_value: bool| {
        apply_given.set(true);
        if unset {
            return Ok(());
        }
        let arg = match (with_value, v) {
            (true, Some(v)) if name.len() == 2 => format!("{name}{v}"),
            (true, Some(v)) => format!("{name}={v}"),
            _ => name.to_owned(),
        };
        apply_opts.borrow_mut().push(arg);
        Ok(())
    };
    let opts = vec![
        Opt::bool('i', "interactive", &interactive, msg::HELP_INTERACTIVE),
        Opt::set_int('n', "no-verify", &no_verify, msg::HELP_NO_VERIFY, 1).noneg(),
        Opt::set_int(None, "verify", &no_verify, msg::HELP_VERIFY, 0).noneg(),
        Opt::bool('b', "binary", &binary, msg::HELP_BINARY).hidden(),
        Opt::bool('3', "3way", &threeway, msg::HELP_3WAY),
        Opt::bool('q', "quiet", &quiet, msg::HELP_QUIET),
        Opt::bool('s', "signoff", &signoff, msg::HELP_SIGNOFF),
        Opt::bool('u', "utf8", &utf8, msg::HELP_UTF8),
        Opt::bool('k', "keep", &keep, msg::HELP_KEEP),
        Opt::bool(None, "keep-non-patch", &keep_non_patch, msg::HELP_KEEP_NON_PATCH),
        Opt::bool('m', "message-id", &message_id, msg::HELP_MESSAGE_ID),
        Opt::bool(None, "keep-cr", &keep_cr, msg::HELP_KEEP_CR),
        Opt::bool('c', "scissors", &scissors, msg::HELP_SCISSORS),
        Opt::callback(None, "quoted-cr", Some("action"), msg::HELP_QUOTED_CR, |v, _| {
            let v = v.unwrap_or_default();
            match QuotedCr::parse(v) {
                Some(q) => {
                    quoted_cr.set(Some(q));
                    Ok(())
                }
                None => Err(CallbackError::Error(
                    crate::messages::mailinfo::bad_quoted_cr(v, "--quoted-cr"),
                )),
            }
        })
        .noneg(),
        Opt::callback(None, "whitespace", Some("action"), msg::HELP_APPLY, |v, u| {
            push_apply("--whitespace", v, u, true)
        }),
        Opt::callback(None, "ignore-space-change", None, msg::HELP_APPLY, |v, u| {
            push_apply("--ignore-space-change", v, u, false)
        })
        .noarg(),
        Opt::callback(None, "ignore-whitespace", None, msg::HELP_APPLY, |v, u| {
            push_apply("--ignore-whitespace", v, u, false)
        })
        .noarg(),
        Opt::callback(None, "directory", Some("root"), msg::HELP_APPLY, |v, u| {
            push_apply("--directory", v, u, true)
        }),
        Opt::callback(None, "exclude", Some("path"), msg::HELP_APPLY, |v, u| {
            push_apply("--exclude", v, u, true)
        }),
        Opt::callback(None, "include", Some("path"), msg::HELP_APPLY, |v, u| {
            push_apply("--include", v, u, true)
        }),
        Opt::callback('C', None, Some("n"), msg::HELP_APPLY, |v, u| push_apply("-C", v, u, true)),
        Opt::callback('p', None, Some("num"), msg::HELP_APPLY, |v, u| push_apply("-p", v, u, true)),
        Opt::string(None, "patch-format", &patch_format, "format", msg::HELP_PATCH_FORMAT),
        Opt::callback(None, "reject", None, msg::HELP_APPLY, |v, u| {
            push_apply("--reject", v, u, false)
        })
        .noarg(),
        Opt::string(None, "resolvemsg", &resolvemsg, "", msg::HELP_RESOLVEMSG).argh("..."),
        Opt::cmdmode(None, "continue", &cmd, msg::HELP_CONTINUE, CONTINUE),
        Opt::cmdmode('r', "resolved", &cmd, msg::HELP_RESOLVED, CONTINUE),
        Opt::cmdmode(None, "skip", &cmd, msg::HELP_SKIP, SKIP),
        Opt::cmdmode(None, "abort", &cmd, msg::HELP_ABORT, ABORT),
        Opt::cmdmode(None, "quit", &cmd, msg::HELP_QUIT, QUIT),
        Opt::callback(
            None,
            "show-current-patch",
            Some("(diff|raw)"),
            msg::HELP_SHOW_CURRENT_PATCH,
            |v, _| {
                let spelled = match v {
                    Some(v) => format!("--show-current-patch={v}"),
                    None => "--show-current-patch".to_owned(),
                };
                let v = v.unwrap_or("raw");
                if v != "diff" && v != "raw" {
                    return Err(CallbackError::Error(msg::invalid_value(
                        "--show-current-patch",
                        v,
                    )));
                }
                let prev = show.borrow().clone();
                if let Some(p) = prev {
                    if p != v {
                        let shown_prev = show_conflict.borrow().clone().unwrap_or_default();
                        return Err(CallbackError::Error(msg::cannot_be_used_together(
                            &spelled,
                            &shown_prev,
                        )));
                    }
                }
                *show_conflict.borrow_mut() = Some(spelled);
                *show.borrow_mut() = Some(v.to_owned());
                cmd.set(SHOW);
                Ok(())
            },
        )
        .optional(None)
        .literal_argh()
        .noneg(),
        Opt::cmdmode(None, "retry", &cmd, msg::HELP_RETRY, RETRY),
        Opt::cmdmode(None, "allow-empty", &cmd, msg::HELP_ALLOW_EMPTY, ALLOW_EMPTY),
        Opt::bool(
            None,
            "committer-date-is-author-date",
            &committer_date,
            msg::HELP_COMMITTER_DATE,
        ),
        Opt::bool(None, "ignore-date", &ignore_date, msg::HELP_IGNORE_DATE),
        Opt::bool(None, "rerere-autoupdate", &rerere, msg::HELP_RERERE),
        Opt::callback('S', "gpg-sign", Some("key-id"), msg::HELP_GPG_SIGN, |_, unset| {
            gpg_sign.set(!unset);
            Ok(())
        })
        .optional(None),
        Opt::callback(None, "empty", Some("(stop|drop|keep)"), msg::HELP_EMPTY, |v, _| {
            let v = v.unwrap_or_default();
            match v {
                "stop" => empty.set(Empty::Stop),
                "drop" => empty.set(Empty::Drop),
                "keep" => empty.set(Empty::Keep),
                _ => *empty_error.borrow_mut() = Some(v.to_owned()),
            }
            Ok(())
        })
        .literal_argh()
        .noneg(),
        Opt::set_int(None, "rebasing", &rebasing, msg::HELP_REBASING, 1).hidden(),
    ];
    let mut parser = Parser::new(msg::USAGE, opts);
    let args = parser.parse(argv)?.args;
    drop(parser);
    if let Some(v) = empty_error.borrow().as_deref() {
        return Err(Die::Error(msg::invalid_value("--empty", v), 129));
    }
    if binary.get() == 1 {
        eprintln!("{}", msg::BINARY_NOOP);
    }
    if interactive.get() == 1 {
        return Err(Die::Fatal(msg::INTERACTIVE_UNSUPPORTED.to_owned()));
    }
    if gpg_sign.get() {
        return Err(Die::Fatal(msg::GPG_UNSUPPORTED.to_owned()));
    }
    let format = match patch_format.borrow().as_deref() {
        None => None,
        Some("mbox") => Some(Format::Mbox),
        Some("mboxrd") => Some(Format::Mboxrd),
        Some("stgit") => Some(Format::Stgit),
        Some("stgit-series") => Some(Format::StgitSeries),
        Some("hg") => Some(Format::Hg),
        Some(other) => return Err(Die::Fatal(msg::invalid_patch_format(other))),
    };

    // Mailbox names are relative to where the command started (setting up
    // the repository changes to the top of the work tree).
    let start = std::env::current_dir().map_err(io_die)?;
    let repo = Repo::open(ctx, Mode::Required)?;
    let work_tree = repo.work_tree()?;
    let git_dir = repo.setup.cwd.join(&repo.setup.git_dir);
    let shown_dir = repo
        .setup
        .git_dir
        .join("rebase-apply")
        .to_string_lossy()
        .into_owned();
    let config = repo.config.clone();
    drop(repo);
    let dir = git_dir.join("rebase-apply");

    let threeway_default = config.bool("am.threeway").ok().flatten().unwrap_or(false);
    let keep_cr_default = config.bool("am.keepcr").ok().flatten().unwrap_or(false);
    let message_id_default = config.bool("am.messageid").ok().flatten().unwrap_or(false);
    let given = Given {
        threeway: (threeway.get() != -1).then(|| threeway.get() == 1),
        quiet: (quiet.get() != -1).then(|| quiet.get() == 1),
        sign: (signoff.get() != -1).then(|| signoff.get() == 1),
        utf8: (utf8.get() != -1).then(|| utf8.get() == 1),
        keep: if keep.get() == 1 {
            Some(Keep::Yes)
        } else if keep_non_patch.get() == 1 {
            Some(Keep::NonPatch)
        } else if keep.get() == 0 || keep_non_patch.get() == 0 {
            Some(Keep::No)
        } else {
            None
        },
        message_id: (message_id.get() != -1).then(|| message_id.get() == 1),
        scissors: (scissors.get() != -1).then(|| scissors.get() == 1),
        quoted_cr: quoted_cr.get(),
        apply_opts: apply_given.get().then(|| apply_opts.borrow().clone()),
    };
    let defaults = Settings {
        threeway: threeway_default || rebasing.get() == 1,
        quiet: false,
        sign: false,
        utf8: true,
        keep: Keep::No,
        message_id: message_id_default,
        scissors: None,
        quoted_cr: None,
        apply_opts: Vec::new(),
    };
    let mut am = Am {
        ctx,
        dir: dir.clone(),
        shown_dir,
        git_dir: git_dir.clone(),
        work_tree,
        config,
        saved: defaults.clone(),
        cur: defaults,
        resolvemsg: resolvemsg.borrow().clone(),
        committer_date_is_author_date: committer_date.get() == 1,
        ignore_date: ignore_date.get() == 1,
        no_verify: no_verify.get() == 1,
        empty: empty.get(),
    };

    let in_progress = Am::in_progress(&dir);
    let mut mode = cmd.get();
    if in_progress {
        am.load_settings();
        am.cur.apply(&given);
        if mode == 0 {
            if !args.is_empty() || !std::io::stdin().is_terminal() {
                return Err(Die::Fatal(msg::previous_dir_exists(&am.shown_dir)));
            }
            mode = RETRY;
        }
    } else if mode != 0 {
        if mode == ABORT && dir.is_dir() {
            let _ = std::fs::remove_dir_all(&dir);
            return Ok(0);
        }
        return Err(Die::Fatal(msg::NOT_RESUMING.to_owned()));
    }
    match mode {
        CONTINUE => return am.resolved(false),
        ALLOW_EMPTY => return am.resolved(true),
        SKIP => return am.skip(),
        ABORT => return am.abort(),
        QUIT => {
            let _ = std::fs::remove_dir_all(&dir);
            return Ok(0);
        }
        SHOW => return am.show_current_patch(show.borrow().as_deref() == Some("diff")),
        RETRY => return am.run(true),
        _ => {}
    }

    // A new session.
    if dir.exists() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    am.saved.apply(&given);
    am.cur = am.saved.clone();
    let keep_cr = match keep_cr.get() {
        -1 => keep_cr_default,
        v => v == 1,
    };
    let inputs: Vec<PathBuf> = args.iter().map(|a| start.join(a)).collect();
    let mails = match read_mails(&inputs, format, keep_cr)? {
        Ok(m) => m,
        Err(()) => {
            eprintln!("{}", msg::FORMAT_DETECTION_FAILED);
            return Ok(128);
        }
    };
    std::fs::create_dir_all(&dir).map_err(io_die)?;
    for (i, m) in mails.iter().enumerate() {
        am.write(&am.msgnum(i + 1), m)?;
    }
    am.write("last", format!("{}\n", mails.len()))?;
    am.write("next", "1\n")?;
    am.save_settings()?;
    am.write("applying", "")?;
    let head = am.head()?;
    let orig = git_dir.join("ORIG_HEAD");
    match head {
        Some(h) => {
            std::fs::write(&orig, format!("{}\n", h.to_hex())).map_err(io_die)?;
            am.write("abort-safety", format!("{}\n", h.to_hex()))?;
        }
        None => {
            let _ = std::fs::remove_file(&orig);
            am.write("abort-safety", "")?;
        }
    }
    let dirty = am.dirty_paths()?;
    if !dirty.is_empty() {
        am.write("dirtyindex", "t")?;
        return Err(Die::Fatal(msg::dirty_index(&dirty.join(" "))));
    }
    am.run(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dequote() {
        assert_eq!(
            sq_dequote_words(" '-p1' '--include=*' 'a'\\''b'"),
            vec!["-p1", "--include=*", "a'b"]
        );
        let a = Author {
            name: "O'Neil".into(),
            email: "o@x".into(),
            date: "Thu, 7 Apr 2005 15:13:13 -0700".into(),
        };
        let back = read_author_script(&author_script(&a));
        assert_eq!(back.name, "O'Neil");
        assert_eq!(back.date, a.date);
    }

    #[test]
    fn formats() {
        assert!(detect_format(b"From x Mon Sep 17 00:00:00 2001\n") == Some(Format::Mbox));
        assert!(detect_format(b"\n   \nX-Fake: y\nFrom: a\n\nbody\n") == Some(Format::Mbox));
        assert!(detect_format(b"# HG changeset patch\n") == Some(Format::Hg));
        assert!(detect_format(b"subject\n\nFrom: a <a@b>\n\nbody\n") == Some(Format::Stgit));
        assert!(detect_format(b"") .is_none());
    }
}
