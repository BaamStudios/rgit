// SPDX-License-Identifier: MIT
//! The hunk selection loop of the patch modes.
//!
//! Each mode names the diff it shows, how the selected hunks are applied
//! and the words of its prompts (checked against the `git` binary with
//! `GIT_TRACE`):
//!
//! | mode | diff | apply | prompt |
//! |---|---|---|---|
//! | `add -p` | `diff-files` | `--cached` | Stage |
//! | `stash -p` | `diff-index HEAD` | `--cached` (a temporary index) | Stash |
//! | `reset -p [HEAD]` | `diff-index --cached HEAD` | `-R --cached` | Unstage |
//! | `reset -p <rev>` | `diff-index -R --cached <rev>` | `--cached` | Apply ... to index |
//! | `checkout -p` | `diff-files` | `-R` | Discard ... from worktree |
//! | `checkout -p HEAD` | `diff-index HEAD` | `-R` to index and work tree | Discard ... from index and worktree |
//! | `checkout -p <rev>` | `diff-index -R <rev>` | to index and work tree | Apply ... to index and worktree |
//! | `restore -p -s HEAD` | `diff-index HEAD` | `-R` | Discard ... from worktree |
//! | `restore -p -s <rev>` | `diff-index -R <rev>` | (work tree) | Apply ... to worktree |
//!
//! When the selected hunks of a file do not apply to the index in the
//! modes that change both, the user is asked whether to apply them to the
//! work tree alone. The selected hunks of a file are applied when the
//! file is done; then a line feed ends the file's prompts.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use super::hunks::{self, Decision, Edited, FileDiff, Hunk, HunkKind};
use crate::commands::stash::child::Git;
use crate::messages::interactive as msg;
use crate::prelude::*;
use crate::repository::Repo;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Mode {
    Stage,
    Stash,
    ResetHead,
    ResetRev,
    CheckoutIndex,
    CheckoutHead,
    CheckoutRev,
    WorktreeHead,
    WorktreeRev,
}

impl Mode {
    fn verb(self) -> msg::Verb {
        match self {
            Mode::Stage => msg::STAGE,
            Mode::Stash => msg::STASH,
            Mode::ResetHead => msg::UNSTAGE,
            Mode::ResetRev => msg::APPLY_INDEX,
            Mode::CheckoutIndex | Mode::WorktreeHead => msg::DISCARD_WORKTREE,
            Mode::CheckoutHead => msg::DISCARD_BOTH,
            Mode::CheckoutRev => msg::APPLY_BOTH,
            Mode::WorktreeRev => msg::APPLY_WORKTREE,
        }
    }

    /// The diff command before the common options.
    fn diff(self, rev: &str) -> Vec<String> {
        let v = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let r = rev.to_owned();
        match self {
            Mode::Stage | Mode::CheckoutIndex => v(&["diff-files"]),
            Mode::Stash | Mode::CheckoutHead | Mode::WorktreeHead => {
                vec!["diff-index".into(), r]
            }
            Mode::ResetHead => vec!["diff-index".into(), "--cached".into(), r],
            Mode::ResetRev => vec!["diff-index".into(), "-R".into(), "--cached".into(), r],
            Mode::CheckoutRev | Mode::WorktreeRev => vec!["diff-index".into(), "-R".into(), r],
        }
    }

    /// The selected hunks are applied in reverse.
    fn reverse(self) -> bool {
        matches!(
            self,
            Mode::ResetHead | Mode::CheckoutIndex | Mode::CheckoutHead | Mode::WorktreeHead
        )
    }

    /// Applied to the index (and not to the work tree).
    fn cached(self) -> bool {
        matches!(
            self,
            Mode::Stage | Mode::Stash | Mode::ResetHead | Mode::ResetRev
        )
    }

    /// Applied to both the index and the work tree.
    fn both(self) -> bool {
        matches!(self, Mode::CheckoutHead | Mode::CheckoutRev)
    }
}

/// What to run the selection on.
pub(crate) struct Options {
    pub(crate) mode: Mode,
    /// The revision of the `diff-index` modes (`HEAD` or an object name).
    pub(crate) rev: String,
    /// The pathspecs as given, relative to the current directory.
    pub(crate) pathspec: Vec<String>,
    /// `-U`/`--unified`.
    pub(crate) context: Option<i32>,
    /// `--inter-hunk-context`.
    pub(crate) inter_hunk: Option<i32>,
    /// Another index file (`commit -p`, `stash -p`).
    pub(crate) index_file: Option<PathBuf>,
    /// The pathspecs are relative to the top of the work tree.
    pub(crate) top_relative: bool,
    /// Go on to the next file once all hunks of a file are decided
    /// (`--no-auto-advance` turns it off: then `<` and `>` move between
    /// files and everything is applied at the end).
    pub(crate) auto_advance: bool,
}

/// `-U`/`--inter-hunk-context` were not given (git treats `-1` the same).
pub(crate) const UNSET: i32 = -1;

/// Checks `-U`, `--inter-hunk-context` and `--no-auto-advance` of a
/// command with a patch mode; `needs` names the option they need.
pub(crate) fn check_options(
    patch: bool,
    unified: i32,
    inter_hunk: i32,
    no_auto_advance: bool,
    needs: &str,
) -> Result<(), Die> {
    let named = [("--unified", unified), ("--inter-hunk-context", inter_hunk)];
    if patch {
        for (name, v) in named {
            if v != UNSET && v < 0 {
                return Err(Die::Fatal(msg::cannot_be_negative(name)));
            }
        }
        return Ok(());
    }
    for (name, v) in named {
        if v != UNSET {
            return Err(Die::Fatal(msg::requires(name, needs)));
        }
    }
    if no_auto_advance {
        return Err(Die::Fatal(msg::requires("--no-auto-advance", needs)));
    }
    Ok(())
}

impl Options {
    /// `-U` and `--inter-hunk-context` ([`UNSET`] when not given).
    pub(crate) fn set_context(&mut self, unified: i32, inter_hunk: i32) {
        self.context = (unified != UNSET).then_some(unified);
        self.inter_hunk = (inter_hunk != UNSET).then_some(inter_hunk);
    }

    pub(crate) fn new(mode: Mode) -> Options {
        Options {
            mode,
            rev: "HEAD".into(),
            pathspec: Vec::new(),
            context: None,
            inter_hunk: None,
            index_file: None,
            top_relative: false,
            auto_advance: true,
        }
    }
}

/// How the selection ended.
#[derive(Default)]
pub(crate) struct Outcome {
    /// Some hunks were applied.
    pub(crate) applied: bool,
}

/// The colours of the selection, empty when not colouring.
pub(crate) struct Colors {
    pub(crate) prompt: String,
    pub(crate) header: String,
    pub(crate) help: String,
    pub(crate) error: String,
    pub(crate) reset: &'static str,
    pub(crate) diff: bool,
    frag: String,
    old: String,
    new: String,
    context: String,
    diff_reset: &'static str,
}

pub(crate) fn colors(repo: &Repo) -> Result<Colors, Die> {
    let cfg = &repo.config;
    let ui = crate::color::want(cfg, Some("color.interactive"), None)?;
    let diff = crate::color::want(cfg, Some("color.diff"), None)?;
    let slot = |on: bool, key: &str, default: &str| -> Result<String, Die> {
        if on {
            crate::color::slot(cfg, key, default)
        } else {
            Ok(String::new())
        }
    };
    let context = if cfg.get("color.diff.context").is_some() {
        slot(diff, "color.diff.context", "")?
    } else {
        slot(diff, "color.diff.plain", "")?
    };
    Ok(Colors {
        prompt: slot(ui, "color.interactive.prompt", "\x1b[1;34m")?,
        header: slot(ui, "color.interactive.header", "\x1b[1m")?,
        help: slot(ui, "color.interactive.help", "\x1b[1;31m")?,
        error: slot(ui, "color.interactive.error", "\x1b[1;31m")?,
        reset: if ui { crate::color::RESET } else { "" },
        diff,
        frag: slot(diff, "color.diff.frag", "\x1b[36m")?,
        old: slot(diff, "color.diff.old", "\x1b[31m")?,
        new: slot(diff, "color.diff.new", "\x1b[32m")?,
        context,
        diff_reset: if diff { crate::color::RESET } else { "" },
    })
}

/// Reads the answers from standard input.
pub(crate) struct Input {
    pub(crate) single_key: bool,
}

impl Input {
    /// One line without its line feed; `None` at the end of the input.
    pub(crate) fn line(&mut self) -> Option<String> {
        let _ = std::io::stdout().flush();
        let mut buf = Vec::new();
        match std::io::stdin().lock().read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => None,
            Ok(_) => {
                while buf.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
                    buf.pop();
                }
                Some(String::from_utf8_lossy(&buf).into_owned())
            }
        }
    }

    /// An answer to the hunk prompt: one key with `interactive.singleKey`
    /// on a terminal, else a line.
    fn answer(&mut self) -> Option<String> {
        if self.single_key {
            if let Some(c) = read_key() {
                if c == '\n' || c == '\r' {
                    println!();
                    return Some(String::new());
                }
                println!("{c}");
                if c == 'g' || c == '/' {
                    return Some(format!("{c}{}", self.line().unwrap_or_default()));
                }
                return Some(c.to_string());
            }
        }
        self.line()
    }
}

/// Reads one key from a terminal on standard input.
#[cfg(unix)]
fn read_key() -> Option<char> {
    use std::io::{IsTerminal, Read};
    if !std::io::stdin().is_terminal() {
        return None;
    }
    let _ = std::io::stdout().flush();
    // SAFETY: plain termios calls on standard input with a zeroed struct
    // that tcgetattr fills in.
    unsafe {
        let mut old: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut old) != 0 {
            return None;
        }
        let mut raw = old;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO);
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        if libc::tcsetattr(0, libc::TCSANOW, &raw) != 0 {
            return None;
        }
        let mut b = [0u8; 1];
        let r = std::io::stdin().read(&mut b);
        libc::tcsetattr(0, libc::TCSANOW, &old);
        match r {
            Ok(1) => Some(b[0] as char),
            _ => None,
        }
    }
}

#[cfg(not(unix))]
fn read_key() -> Option<char> {
    None
}

/// The state of one patch mode run.
struct Session<'a> {
    repo: &'a Repo,
    git: &'a Git,
    opts: &'a Options,
    colors: Colors,
    input: Input,
    comment: String,
    /// Unmerged paths, left out of the `diff-files` modes.
    unmerged: Vec<String>,
}

enum FileEnd {
    Done,
    Quit,
    Next,
    Prev,
}

/// Runs the hunk selection.
pub(crate) fn run(ctx: &Context, repo: &Repo, opts: &Options) -> Result<Outcome, Die> {
    let git = Git::new(ctx, repo)?;
    run_with(&git, repo, opts)
}

/// [`run`] with the child commands of `git`.
pub(crate) fn run_with(git: &Git, repo: &Repo, opts: &Options) -> Result<Outcome, Die> {
    check_context_config(repo)?;
    let colors = colors(repo)?;
    let single_key = repo
        .config
        .bool("interactive.singlekey")
        .ok()
        .flatten()
        .unwrap_or(false);
    let comment =
        String::from_utf8_lossy(&crate::comment::comment(&repo.config)?.or_hash()).into_owned();
    let mut s = Session {
        repo,
        git,
        opts,
        colors,
        input: Input { single_key },
        comment,
        unmerged: Vec::new(),
    };
    if matches!(opts.mode, Mode::Stage | Mode::CheckoutIndex) {
        s.unmerged = unmerged_paths(repo, opts)?;
    }
    s.run()
}

/// `diff.context` and `diff.interHunkContext` must not be negative.
fn check_context_config(repo: &Repo) -> Result<(), Die> {
    for (key, m) in [
        ("diff.context", msg::CONTEXT_NEGATIVE),
        ("diff.interhunkcontext", msg::INTERHUNK_NEGATIVE),
    ] {
        if let Ok(Some(v)) = repo.config.int(key) {
            if v < 0 {
                return Err(Die::Fatal(m.to_owned()));
            }
        }
    }
    Ok(())
}

impl Session<'_> {
    fn diff_args(&self, color: bool) -> Vec<String> {
        let mut args = self.opts.mode.diff(&self.opts.rev);
        args.push(if color { "--color" } else { "--no-color" }.into());
        args.push("--ignore-submodules=dirty".into());
        args.push("-p".into());
        let cfg = &self.repo.config;
        let config_int = |key: &str| cfg.int(key).ok().flatten();
        if let Some(u) = self
            .opts
            .context
            .map(i64::from)
            .or_else(|| config_int("diff.context"))
        {
            args.push(format!("-U{u}"));
        }
        let inter_hunk = self
            .opts
            .inter_hunk
            .map(i64::from)
            .or_else(|| config_int("diff.interhunkcontext"));
        if let Some(i) = inter_hunk {
            args.push(format!("--inter-hunk-context={i}"));
        }
        if let Ok(Some(a)) = cfg.string("diff.algorithm") {
            args.push(format!("--diff-algorithm={a}"));
        }
        args.push("--".into());
        args.extend(self.opts.pathspec.iter().cloned());
        if self.opts.pathspec.is_empty() && !self.unmerged.is_empty() {
            args.push(":/".into());
        }
        for p in &self.unmerged {
            args.push(format!(":(top,exclude,literal){p}"));
        }
        args
    }

    fn child(&self, args: &[String]) -> crate::commands::stash::child::Call<'_> {
        let mut c = self.git.cmd(args);
        if let Some(i) = &self.opts.index_file {
            c = c.index(i);
        }
        c
    }

    /// The diff, plain and as shown.
    fn diff(&self) -> Result<(Vec<u8>, Option<Vec<u8>>), Die> {
        let cwd = user_dir(self.repo)?;
        let run = |color: bool| {
            let call = self.child(&self.diff_args(color));
            if self.opts.top_relative {
                call.output()
            } else {
                call.in_dir(&cwd).output()
            }
        };
        let (code, plain) = run(false)?;
        if code != 0 {
            return Err(Die::Error(msg::COULD_NOT_PARSE_DIFF.to_owned(), 1));
        }
        if !self.colors.diff {
            return Ok((plain, None));
        }
        let (code, mut shown) = run(true)?;
        if code != 0 {
            return Err(Die::Error(msg::COULD_NOT_PARSE_DIFF.to_owned(), 1));
        }
        let filter = self
            .repo
            .config
            .string("interactive.difffilter")
            .ok()
            .flatten()
            .map(|v| v.to_string());
        if let Some(filter) = filter {
            shown = run_filter(&filter, &shown)?;
            let count = |d: &[u8]| d.iter().filter(|b| **b == b'\n').count();
            if count(&shown) != count(&plain) {
                eprintln!("error: {}", msg::FILTER_MISMATCH);
                for l in msg::FILTER_HINT.lines() {
                    eprintln!("hint: {l}");
                }
                return Err(Die::Exit(1));
            }
        }
        Ok((plain, Some(shown)))
    }

    fn run(&mut self) -> Result<Outcome, Die> {
        let (plain, shown) = self.diff()?;
        let all = hunks::parse(&plain, shown.as_deref());
        let any_binary = all.iter().any(|f| f.binary);
        let mut files: Vec<FileDiff> = all
            .into_iter()
            .filter(|f| !f.binary && !f.hunks.is_empty())
            .collect();
        let mut out = Outcome::default();
        if files.is_empty() {
            println!(
                "{}",
                if any_binary {
                    msg::ONLY_BINARY
                } else {
                    msg::NO_CHANGES
                }
            );
            return Ok(out);
        }
        let reverse = self.opts.mode.reverse();
        if self.opts.auto_advance {
            for file in &mut files {
                for l in &file.header {
                    print_bytes(&l.shown);
                }
                let mut cur = 0;
                let end = self.file_loop(file, &mut cur, 1)?;
                if let Some(patch) =
                    hunks::file_patch(file, &|h| h.decision == Decision::Use, reverse)
                {
                    if self.apply_file(patch)? {
                        out.applied = true;
                    }
                }
                println!();
                if matches!(end, FileEnd::Quit) {
                    break;
                }
            }
        } else {
            let count = files.len();
            let mut at = 0;
            loop {
                for l in &files[at].header {
                    print_bytes(&l.shown);
                }
                // A file starts at its first hunk again.
                let mut cur = 0;
                let end = self.file_loop(&mut files[at], &mut cur, count)?;
                println!();
                match end {
                    FileEnd::Next => at = (at + 1) % count,
                    FileEnd::Prev => at = (at + count - 1) % count,
                    FileEnd::Done | FileEnd::Quit => break,
                }
            }
            for file in &files {
                if let Some(patch) =
                    hunks::file_patch(file, &|h| h.decision == Decision::Use, reverse)
                {
                    if self.apply_file(patch)? {
                        out.applied = true;
                    }
                }
            }
        }
        if out.applied {
            let _ = self
                .child(&["update-index".into(), "-q".into(), "--refresh".into()])
                .output();
        }
        Ok(out)
    }

    fn apply(&self, args: &[&str], patch: &[u8]) -> Result<bool, Die> {
        let mut a = vec!["apply".to_owned()];
        a.extend(args.iter().map(|s| (*s).to_owned()));
        Ok(self.child(&a).stdin(patch.to_vec()).status()? == 0)
    }

    /// Applies the selected hunks of a file; whether anything was applied.
    fn apply_file(&mut self, patch: Vec<u8>) -> Result<bool, Die> {
        let mode = self.opts.mode;
        let mut args: Vec<&str> = Vec::new();
        if mode.reverse() {
            args.push("-R");
        }
        if !mode.both() {
            if mode.cached() {
                args.push("--cached");
            }
            let ok = self.apply(&args, &patch)?;
            if !ok {
                self.apply_failed(&args);
            }
            return Ok(ok);
        }
        let with = |extra: &[&'static str]| {
            let mut a = args.clone();
            a.extend_from_slice(extra);
            a
        };
        let index_ok = self.apply(&with(&["--cached", "--check"]), &patch)?;
        let tree_ok = self.apply(&with(&["--check"]), &patch)?;
        if index_ok && tree_ok {
            let a = self.apply(&with(&["--cached"]), &patch)?;
            let b = self.apply(&args, &patch)?;
            return Ok(a || b);
        }
        if !tree_ok {
            return Ok(false);
        }
        println!("{}", msg::NOT_TO_INDEX);
        print!("{}", msg::APPLY_ANYWAY);
        let answer = self.input.line().unwrap_or_default();
        if answer.trim_start().starts_with(['y', 'Y']) {
            return self.apply(&args, &patch);
        }
        println!("{}", msg::NOTHING_APPLIED);
        Ok(false)
    }

    fn error(&self, text: &str) {
        println!("{}{text}{}", self.colors.error, self.colors.reset);
    }

    fn show_hunk(&self, h: &Hunk) {
        let mut out = Vec::new();
        self.render_hunk(h, &mut out);
        print_bytes(&out);
    }

    fn render_hunk(&self, h: &Hunk, out: &mut Vec<u8>) {
        if h.kind == HunkKind::Mode {
            for l in &h.lines {
                out.extend_from_slice(&l.shown);
            }
            return;
        }
        if h.lines.is_empty() {
            return;
        }
        let c = &self.colors;
        match &h.header {
            Some(hd) if !h.edited => out.extend_from_slice(&hd.shown),
            _ => {
                let text = h.generated_header();
                out.extend_from_slice(
                    format!("{}{}{}\n", c.frag, text.trim_end(), c.diff_reset).as_bytes(),
                );
            }
        }
        for l in &h.lines {
            if !h.edited || !c.diff {
                out.extend_from_slice(if h.edited { &l.plain } else { &l.shown });
                continue;
            }
            let color = match l.kind() {
                b'-' => &c.old,
                b'+' => &c.new,
                _ => &c.context,
            };
            let text = l.plain.strip_suffix(b"\n").unwrap_or(&l.plain);
            out.extend_from_slice(color.as_bytes());
            out.extend_from_slice(text);
            out.extend_from_slice(c.diff_reset.as_bytes());
            out.push(b'\n');
        }
    }

    fn what(file: &FileDiff, h: &Hunk) -> msg::What {
        if h.kind == HunkKind::Mode {
            msg::What::ModeChange
        } else if file.deletion {
            msg::What::Deletion
        } else if file.addition {
            msg::What::Addition
        } else {
            msg::What::Hunk
        }
    }

    /// The selection loop over the hunks of one file.
    #[allow(clippy::too_many_lines)]
    fn file_loop(
        &mut self,
        file: &mut FileDiff,
        position: &mut usize,
        files: usize,
    ) -> Result<FileEnd, Die> {
        let navigate = !self.opts.auto_advance;
        let mut cur = *position;
        let mut show = true;
        let end = |e: FileEnd, cur: usize, position: &mut usize| {
            *position = cur;
            Ok(e)
        };
        loop {
            let n = file.hunks.len();
            let h = &file.hunks[cur];
            if show {
                self.show_hunk(h);
                show = false;
            }
            let others = (0..n).any(|i| i != cur && file.hunks[i].decision == Decision::Undecided);
            let can_split = h.can_split();
            let can_edit = h.kind == HunkKind::Text && !file.deletion && !h.lines.is_empty();
            let printable = !(file.addition && h.lines.is_empty() && h.kind == HunkKind::Text);
            let mut choices = String::from("y,n,q,a,d");
            if others {
                choices.push_str(",k");
            }
            if n > 1 {
                choices.push_str(",K");
            }
            if others {
                choices.push_str(",j");
            }
            if n > 1 {
                choices.push_str(",J,g,/");
            }
            if can_split {
                choices.push_str(",s");
            }
            if can_edit {
                choices.push_str(",e");
            }
            if navigate && files > 1 {
                choices.push_str(",>,<");
            }
            if printable {
                choices.push_str(",p,P");
            }
            choices.push_str(",?");
            let was = match h.decision {
                Decision::Undecided => "",
                Decision::Use => " (was: y)",
                Decision::Skip => " (was: n)",
            };
            print!(
                "{}({}/{}) {}{was} [{choices}]? {}",
                self.colors.prompt,
                cur + 1,
                n,
                msg::prompt(self.opts.mode.verb(), Self::what(file, h)),
                self.colors.reset
            );
            let Some(answer) = self.input.answer() else {
                return end(FileEnd::Quit, cur, position);
            };
            let answer = answer.trim();
            let Some(key) = answer.chars().next() else {
                continue;
            };
            if answer.len() > 1 && key != 'g' && key != '/' {
                self.error(&msg::one_letter(answer));
                continue;
            }
            let next_undecided = |hunks: &[Hunk], from: usize| {
                (1..hunks.len())
                    .map(|i| (from + i) % hunks.len())
                    .find(|&i| hunks[i].decision == Decision::Undecided)
            };
            let prev_undecided = |hunks: &[Hunk], from: usize| {
                (1..hunks.len())
                    .map(|i| (from + hunks.len() - i) % hunks.len())
                    .find(|&i| hunks[i].decision == Decision::Undecided)
            };
            let decided = match key {
                'y' => {
                    file.hunks[cur].decision = Decision::Use;
                    true
                }
                'n' => {
                    file.hunks[cur].decision = Decision::Skip;
                    true
                }
                'a' | 'd' => {
                    let d = if key == 'a' {
                        Decision::Use
                    } else {
                        Decision::Skip
                    };
                    file.hunks[cur].decision = d;
                    for h in &mut file.hunks[cur + 1..] {
                        if h.decision == Decision::Undecided {
                            h.decision = d;
                        }
                    }
                    true
                }
                'q' => {
                    if file.hunks[cur].decision == Decision::Undecided {
                        file.hunks[cur].decision = Decision::Skip;
                    }
                    return end(FileEnd::Quit, cur, position);
                }
                '>' | '<' if navigate => {
                    if files > 1 {
                        let e = if key == '>' {
                            FileEnd::Next
                        } else {
                            FileEnd::Prev
                        };
                        return end(e, cur, position);
                    }
                    self.error(if key == '>' {
                        msg::NO_NEXT_FILE
                    } else {
                        msg::NO_PREVIOUS_FILE
                    });
                    false
                }
                'j' | 'k' => {
                    let found = if key == 'j' {
                        next_undecided(&file.hunks, cur)
                    } else {
                        prev_undecided(&file.hunks, cur)
                    };
                    match found {
                        Some(i) => {
                            cur = i;
                            show = true;
                        }
                        None => self.error(msg::NO_OTHER_UNDECIDED),
                    }
                    false
                }
                'J' | 'K' => {
                    if n > 1 {
                        cur = if key == 'J' {
                            (cur + 1) % n
                        } else {
                            (cur + n - 1) % n
                        };
                        show = true;
                    } else {
                        self.error(msg::NO_OTHER_HUNK);
                    }
                    false
                }
                'g' => {
                    if n <= 1 {
                        self.error(msg::NO_GOTO);
                    } else if let Some(i) = self.goto(file, cur, answer[1..].trim()) {
                        show = i != cur;
                        cur = i;
                    }
                    false
                }
                '/' => {
                    if n <= 1 {
                        self.error(msg::NO_SEARCH);
                    } else if let Some(i) = self.search(file, cur, answer[1..].trim()) {
                        show = i != cur;
                        cur = i;
                    }
                    false
                }
                's' => {
                    if can_split {
                        let parts = hunks::split(&file.hunks[cur]);
                        println!(
                            "{}{}{}",
                            self.colors.header,
                            msg::split_into(parts.len()),
                            self.colors.reset
                        );
                        file.hunks.splice(cur..=cur, parts);
                        show = true;
                    } else {
                        self.error(msg::CANNOT_SPLIT);
                    }
                    false
                }
                'e' => {
                    if !can_edit {
                        self.error(msg::CANNOT_EDIT);
                        false
                    } else if let Some(new) = self.edit(file, cur)? {
                        file.hunks[cur] = new;
                        true
                    } else {
                        false
                    }
                }
                'p' => {
                    if printable {
                        show = true;
                    }
                    false
                }
                'P' => {
                    if printable {
                        let mut text = Vec::new();
                        self.render_hunk(&file.hunks[cur], &mut text);
                        self.page(&text);
                    }
                    false
                }
                '?' => {
                    self.help(
                        n,
                        others,
                        can_split,
                        can_edit,
                        printable,
                        navigate && files > 1,
                    );
                    if navigate && !file.hunks.iter().any(|h| h.decision == Decision::Undecided) {
                        let used = file
                            .hunks
                            .iter()
                            .filter(|h| h.decision == Decision::Use)
                            .count();
                        println!("{}", msg::hunks_summary(n, used, n - used));
                        println!();
                    }
                    false
                }
                _ => {
                    self.error(&msg::unknown_command(answer));
                    false
                }
            };
            if decided {
                match next_undecided(&file.hunks, cur) {
                    Some(i) => {
                        cur = i;
                        show = true;
                    }
                    None if navigate => {
                        if n > 1 {
                            cur = (cur + 1) % n;
                            show = true;
                        }
                    }
                    None => return end(FileEnd::Done, cur, position),
                }
            }
        }
    }

    fn help(&self, n: usize, others: bool, split: bool, edit: bool, print: bool, files: bool) {
        let (c, r) = (&self.colors.help, self.colors.reset);
        print!("{c}{}{r}", msg::help_head(self.opts.mode.verb()));
        let mut lines = Vec::new();
        if others {
            lines.push(msg::HELP_J);
        }
        if n > 1 {
            lines.push(msg::HELP_BIG_J);
        }
        if others {
            lines.push(msg::HELP_K);
        }
        if n > 1 {
            lines.extend([msg::HELP_BIG_K, msg::HELP_G, msg::HELP_SEARCH]);
        }
        if split {
            lines.push(msg::HELP_S);
        }
        if edit {
            lines.push(msg::HELP_E);
        }
        if print {
            lines.extend([msg::HELP_P, msg::HELP_BIG_P]);
        }
        if files {
            lines.extend([msg::HELP_NEXT_FILE, msg::HELP_PREVIOUS_FILE]);
        }
        lines.push(msg::HELP_HELP);
        for l in lines {
            println!("{c}{l}{r}");
        }
    }

    /// `g`: the hunk to go to (asking with a list when `arg` is empty).
    fn goto(&mut self, file: &FileDiff, cur: usize, arg: &str) -> Option<usize> {
        let n = file.hunks.len();
        let mut arg = arg.to_owned();
        if arg.is_empty() {
            let start = cur.saturating_sub(10);
            for (i, h) in file.hunks.iter().enumerate().skip(start).take(21) {
                let mark = match h.decision {
                    Decision::Use => '+',
                    Decision::Skip => '-',
                    Decision::Undecided => ' ',
                };
                let first = h
                    .lines
                    .iter()
                    .find(|l| matches!(l.kind(), b'+' | b'-'))
                    .map(|l| {
                        String::from_utf8_lossy(l.plain.strip_suffix(b"\n").unwrap_or(&l.plain))
                            .into_owned()
                    })
                    .unwrap_or_default();
                let range = if h.kind == HunkKind::Mode {
                    String::new()
                } else {
                    h.range_text()
                };
                println!("{mark}{:>2}:  {range:<19}{first}", i + 1);
            }
            loop {
                print!("{}", msg::GOTO_PROMPT);
                match self.input.line() {
                    None => break,
                    Some(l) if l.trim().is_empty() => continue,
                    Some(l) => {
                        arg = l.trim().to_owned();
                        break;
                    }
                }
            }
        }
        match arg.parse::<i64>() {
            Err(_) => {
                self.error(&msg::invalid_number(&arg));
                None
            }
            Ok(v) if v < 1 || v as usize > n => {
                self.error(&msg::only_hunks(n));
                None
            }
            Ok(v) => Some(v as usize - 1),
        }
    }

    /// `/`: the next hunk matching a regular expression.
    fn search(&mut self, file: &FileDiff, cur: usize, arg: &str) -> Option<usize> {
        let mut pattern = arg.to_owned();
        while pattern.is_empty() {
            print!("{}", msg::SEARCH_PROMPT);
            pattern = self.input.line()?.trim().to_owned();
        }
        let re = match regex::bytes::Regex::new(&pattern) {
            Ok(r) => r,
            Err(_) => {
                self.error(&msg::malformed_regex(
                    &pattern,
                    "Invalid regular expression",
                ));
                return None;
            }
        };
        let n = file.hunks.len();
        let matches = |h: &Hunk| {
            let header = h.generated_header();
            let mut texts: Vec<&[u8]> = vec![header.as_bytes()];
            if let Some(hd) = &h.header {
                texts.push(&hd.plain);
            }
            texts.iter().any(|t| re.is_match(t)) || h.lines.iter().any(|l| re.is_match(&l.plain))
        };
        let found = (1..=n)
            .map(|i| (cur + i) % n)
            .find(|&i| matches(&file.hunks[i]));
        if found.is_none() {
            self.error(msg::NO_MATCH);
        }
        found
    }

    /// Shows `text` through the pager.
    fn page(&self, text: &[u8]) {
        let env = rgit_config::SystemEnv::default();
        let Some(cmd) = crate::pager::command(&self.repo.config, &env, None) else {
            print_bytes(text);
            return;
        };
        let _ = std::io::stdout().flush();
        let child = std::process::Command::new(rgit_ident::editor::SHELL_PATH)
            .arg("-c")
            .arg(&cmd)
            .stdin(std::process::Stdio::piped())
            .spawn();
        if let Ok(mut child) = child {
            if let Some(mut input) = child.stdin.take() {
                let _ = input.write_all(text);
            }
            let _ = child.wait();
        }
    }

    /// `e`: lets the user edit hunk `cur`; the new hunk when it applies.
    fn edit(&mut self, file: &FileDiff, cur: usize) -> Result<Option<Hunk>, Die> {
        let orig = &file.hunks[cur];
        let git_dir = self.repo.setup.cwd.join(&self.repo.setup.git_dir);
        let path = git_dir.join("addp-hunk-edit.diff");
        let c = self.comment.clone();
        let mut text = msg::edit_intro(&c).into_bytes();
        text.extend_from_slice(orig.generated_header().as_bytes());
        for l in &orig.lines {
            text.extend_from_slice(&l.plain);
        }
        text.extend_from_slice(
            msg::edit_guide(&c, self.opts.mode.verb(), self.opts.mode.reverse()).as_bytes(),
        );
        if let Err(e) = std::fs::write(&path, &text) {
            self.error(&msg::could_not_write(
                &path.to_string_lossy(),
                &crate::global::strerror(&e),
            ));
            return Ok(None);
        }
        let env = rgit_config::SystemEnv::default();
        let editor = rgit_ident::editor::editor(&self.repo.config, &env)
            .ok()
            .flatten();
        let launch = rgit_ident::editor::LaunchOptions {
            hint: false,
            env: Vec::new(),
            cwd: None,
        };
        loop {
            let _ = std::io::stdout().flush();
            let edited = match rgit_ident::editor::launch(editor.as_deref(), &path, &launch) {
                Ok(()) => std::fs::read(&path).ok(),
                Err(e) => {
                    e.print();
                    None
                }
            };
            let result = edited.map(|t| hunks::read_edited(&t, c.as_bytes(), orig));
            match result {
                Some(Edited::Empty) => {
                    let _ = std::fs::remove_file(&path);
                    return Ok(None);
                }
                Some(Edited::Hunk(h)) => {
                    if self.applies(file, &h)? {
                        let _ = std::fs::remove_file(&path);
                        return Ok(Some(h));
                    }
                }
                Some(Edited::Garbage(h)) => {
                    let _ = self.applies(file, &h)?;
                }
                None => {}
            }
            loop {
                print!(
                    "{}{}{}",
                    self.colors.prompt,
                    msg::EDIT_AGAIN,
                    self.colors.reset
                );
                match self.input.line().map(|l| l.trim().to_owned()) {
                    None => {
                        let _ = std::fs::remove_file(&path);
                        return Ok(None);
                    }
                    Some(a) if a.starts_with(['y', 'Y']) => break,
                    Some(a) if a.starts_with(['n', 'N']) => {
                        let _ = std::fs::remove_file(&path);
                        return Ok(None);
                    }
                    Some(_) => {}
                }
            }
        }
    }

    /// Whether `h` alone applies (`apply --check` in the mode's direction).
    fn applies(&self, file: &FileDiff, h: &Hunk) -> Result<bool, Die> {
        let mut only = FileDiff {
            header: file.header.clone(),
            mode_lines: Vec::new(),
            hunks: vec![h.clone()],
            deletion: file.deletion,
            addition: file.addition,
            binary: false,
        };
        only.hunks[0].decision = Decision::Use;
        let Some(patch) = hunks::file_patch(&only, &|_| true, self.opts.mode.reverse()) else {
            return Ok(false);
        };
        let mode = self.opts.mode;
        let mut args = Vec::new();
        if mode.reverse() {
            args.push("-R");
        }
        if mode.cached() {
            args.push("--cached");
        }
        let mut check = args.clone();
        check.push("--check");
        let ok = self.apply(&check, &patch)?;
        if !ok {
            self.apply_failed(&args);
        }
        Ok(ok)
    }

    /// git's error after a failed `apply` child.
    fn apply_failed(&self, args: &[&str]) {
        let _ = std::io::stdout().flush();
        let mut cmd = String::from("git apply");
        for a in args {
            cmd.push(' ');
            cmd.push_str(a);
        }
        eprintln!("error: {}", msg::child_failed(&cmd));
    }
}

/// The unmerged paths of the index the selection works on.
fn unmerged_paths(repo: &Repo, opts: &Options) -> Result<Vec<String>, Die> {
    let path = match &opts.index_file {
        Some(p) => p.clone(),
        None => repo.setup.cwd.join(repo.index_path()),
    };
    if !path.exists() {
        return Ok(Vec::new());
    }
    let index = rgit_index::Index::read_file(&path, repo.hash, &rgit_index::ReadOptions::default())
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let mut out: Vec<String> = Vec::new();
    for e in index.entries() {
        if e.stage().is_conflict() {
            let p = String::from_utf8_lossy(e.path()).into_owned();
            if out.last() != Some(&p) {
                out.push(p);
            }
        }
    }
    Ok(out)
}

/// The directory the command was started in (pathspecs are relative to
/// it): the top of the work tree and the prefix.
pub(crate) fn user_dir(repo: &Repo) -> Result<PathBuf, Die> {
    let mut dir = repo.work_tree()?;
    let prefix = repo.prefix();
    if !prefix.is_empty() {
        dir.push(String::from_utf8_lossy(&prefix).trim_end_matches('/'));
    }
    Ok(dir)
}

fn print_bytes(b: &[u8]) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(b);
}

/// Runs `interactive.diffFilter` on the coloured diff.
fn run_filter(filter: &str, input: &[u8]) -> Result<Vec<u8>, Die> {
    let mut child = std::process::Command::new(rgit_ident::editor::SHELL_PATH)
        .arg("-c")
        .arg(filter)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let mut stdin = child.stdin.take().expect("stdin");
    let data = input.to_vec();
    // Write from another thread: a large diff fills the pipes.
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&data);
    });
    let out = child
        .wait_with_output()
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let _ = writer.join();
    Ok(out.stdout)
}
