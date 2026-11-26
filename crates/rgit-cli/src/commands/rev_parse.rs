// SPDX-License-Identifier: MIT
//! `git rev-parse` (Documentation/git-rev-parse.adoc): revision arguments
//! (Documentation/revisions.adoc), the filtering and output options, and
//! the options that describe the repository location.

use std::io::Write;
use std::path::Path;

use rgit_hash::ObjectId;
use rgit_pathspec::wildmatch::{WildmatchFlags, wildmatch};
use rgit_repo::Setup;
use rgit_revwalk::revision::{
    Diagnostic, Error as RevError, Flags, Level, Repo, Resolver, RevItem,
};

use crate::messages;
use crate::prelude::*;
use crate::repo::{self, Mode};
use crate::revisions::{self, Config, IndexFile, Objects, Refs, WorkTree};

#[path = "rev_parse/parseopt.rs"]
mod parseopt;

/// rev-parse options that are not implemented yet; they fail instead of
/// being passed through as unknown flags.
const NOT_YET: &[&str] = &[
    "--prefix",
    "--resolve-git-dir",
    "--path-format",
    "--local-env-vars",
    "--show-superproject-working-tree",
    "--exclude-hidden",
    "--output-object-format",
];

/// rev-list options that rev-parse treats as revision flags
/// (`--revs-only` keeps them).
const REV_LIST_FLAGS: &[&str] = &[
    "--max-count=",
    "--max-age=",
    "--min-age=",
    "--skip=",
    "--topo-order",
    "--date-order",
    "--author-date-order",
    "--parents",
    "--children",
    "--header",
    "--first-parent",
    "--no-merges",
    "--merges",
    "--min-parents=",
    "--max-parents=",
    "--no-min-parents",
    "--no-max-parents",
    "--sparse",
    "--dense",
    "--remove-empty",
    "--objects",
    "--objects-edge",
    "--reverse",
    "--boundary",
    "--left-right",
    "--cherry-pick",
    "--full-history",
    "--simplify-merges",
    "--simplify-by-decoration",
    "--ancestry-path",
    "--grep=",
    "--author=",
    "--committer=",
    "--all-match",
    "--regexp-ignore-case",
    "--extended-regexp",
    "--fixed-strings",
];

const DO_REVS: u8 = 1;
const DO_NOREV: u8 = 2;
const DO_FLAGS: u8 = 4;
const DO_NONFLAGS: u8 = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Symbolic {
    None,
    Plain,
    Full,
}

/// Quote for the shell like git's `sq_quote`.
fn sq_quote(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Whether `arg` is a glob (an unescaped `*`, `?` or `[`).
fn has_wildcard(arg: &str) -> bool {
    let b = arg.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 1,
            b'*' | b'?' | b'[' => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

fn is_rev_flag(arg: &str) -> bool {
    REV_LIST_FLAGS.iter().any(|f| {
        if f.ends_with('=') {
            arg.starts_with(f)
        } else {
            arg == *f
        }
    })
}

struct State<'r, 'a> {
    resolver: &'r Resolver<'a>,
    setup: &'r Setup,
    out: Vec<u8>,
    filter: u8,
    verify: bool,
    quiet: bool,
    sq: bool,
    abbrev: Option<usize>,
    symbolic: Symbolic,
    /// `--abbrev-ref`: strict mode.
    abbrev_ref: Option<bool>,
    reversed: bool,
    default: Option<String>,
    shown_rev: bool,
    as_is: u8,
    has_dashdash: bool,
    end_of_options: bool,
    revs_count: usize,
    verified: Option<(ObjectId, bool, String)>,
    excludes: Vec<String>,
}

/// How an argument loop step ends.
type Step = Result<(), Die>;

impl State<'_, '_> {
    fn emit(&mut self, s: &str) {
        if self.sq {
            self.out.extend_from_slice(&rgit_bstr::to_bytes(&sq_quote(s)));
            self.out.push(b' ');
        } else {
            self.out.extend_from_slice(&rgit_bstr::to_bytes(s));
            self.out.push(b'\n');
        }
    }

    fn flush(&mut self) {
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(&self.out);
        let _ = stdout.flush();
        self.out.clear();
    }

    /// Print messages collected by the resolver; hints are kept back when
    /// `hold_hints` (they follow the `fatal:` line).
    fn print_diags(&self, diags: &[Diagnostic], hints: bool) {
        let mut err = std::io::stderr();
        for d in diags {
            if (d.level == Level::Hint) == hints {
                let _ = err.write_all(d.render().as_bytes());
            }
        }
    }

    /// Print what the resolver reported (it leaves out what a quiet lookup
    /// must not print).
    fn drain_diags(&self) {
        let d = self.resolver.take_diagnostics();
        self.print_diags(&d, false);
        self.print_diags(&d, true);
    }

    /// End the command with `fatal: msg`. Standard output is written
    /// afterwards: git buffers it and it reaches the terminal at exit.
    fn die(&mut self, msg: String) -> Die {
        let _ = writeln!(std::io::stderr(), "fatal: {msg}");
        self.flush();
        Die::Exit(rgit_options::EXIT_FATAL)
    }

    fn die_no_single_rev(&mut self) -> Die {
        if self.quiet {
            self.flush();
            Die::Exit(1)
        } else {
            self.die(messages::NEEDED_SINGLE_REVISION.to_string())
        }
    }

    /// A fatal error from the resolver (silent with `--verify -q`).
    fn resolver_fatal(&mut self, msg: String) -> Die {
        self.drain_diags();
        if self.verify && self.quiet {
            self.flush();
            return Die::Exit(1);
        }
        self.die(msg)
    }

    fn show_rev(&mut self, negated: bool, oid: &ObjectId, name: Option<&str>) -> Step {
        if self.filter & DO_REVS == 0 {
            return Ok(());
        }
        self.default = None;
        self.shown_rev = true;
        let caret = if negated != self.reversed { "^" } else { "" };
        let symbolic = self.symbolic != Symbolic::None || self.abbrev_ref.is_some();
        if let (Some(name), true) = (name, symbolic) {
            match (self.symbolic, self.abbrev_ref) {
                (Symbolic::Plain, None) => self.emit(&format!("{caret}{name}")),
                _ => {
                    let d = match self.resolver.dwim_ref(name) {
                        Ok(d) => d,
                        Err(RevError::Fatal(m)) => return Err(self.resolver_fatal(m)),
                        Err(e @ RevError::Lookup(_)) => return Err(self.rev_error(e)),
                        Err(RevError::NotFound) => return Ok(()),
                    };
                    match d.count {
                        0 => {}
                        1 => {
                            let full = match self.abbrev_ref {
                                Some(strict) => {
                                    match self.resolver.shorten_ref(&d.full_name, strict) {
                                        Ok(s) => s,
                                        Err(e) => return Err(self.die(e.to_string())),
                                    }
                                }
                                None => d.full_name,
                            };
                            self.emit(&format!("{caret}{full}"));
                        }
                        _ => messages::print_error(
                            &rgit_revwalk::revision::messages::refname_ambiguous_error(name),
                        ),
                    }
                }
            }
            return Ok(());
        }
        let text = match self.abbrev {
            Some(len) => match self.resolver.unique_abbrev(oid, len) {
                Ok(s) => s,
                Err(e) => return Err(self.die(e.to_string())),
            },
            None => oid.to_hex(),
        };
        self.emit(&format!("{caret}{text}"));
        Ok(())
    }

    fn show_items(&mut self, items: &[RevItem]) -> Step {
        for it in items {
            self.show_rev(it.negated, &it.oid, it.name.as_deref())?;
        }
        Ok(())
    }

    /// Show a non-revision argument (`show_file`); false if filtered out.
    fn show_file(&mut self, arg: &str) -> bool {
        if self.filter & (DO_NONFLAGS | DO_NOREV) == (DO_NONFLAGS | DO_NOREV) {
            self.emit(arg);
            return true;
        }
        false
    }

    fn show_flag(&mut self, arg: &str) {
        if self.filter & DO_FLAGS == 0 {
            return;
        }
        let want = if is_rev_flag(arg) { DO_REVS } else { DO_NOREV };
        if self.filter & want != 0 {
            self.emit(arg);
        }
    }

    /// Whether `arg` names something in the work tree (`check_filename`).
    fn path_exists(&self, arg: &str) -> bool {
        let Some(top) = self.setup.work_tree.as_deref() else {
            return false;
        };
        // Pathspec magic: `:(...)` long form, `:/`, `:!`, `:^` short form.
        if let Some(rest) = arg.strip_prefix(':') {
            if rest.starts_with('(') {
                return true;
            }
            if rest.starts_with(['/', '!', '^']) {
                let rest = rest.trim_start_matches(['/', '!', '^']);
                if rest.is_empty() {
                    return true;
                }
                return std::fs::symlink_metadata(top.join(rest)).is_ok();
            }
        }
        if has_wildcard(arg) {
            return true;
        }
        let prefix = self
            .setup
            .prefix
            .as_ref()
            .map(Path::new)
            .unwrap_or(Path::new(""));
        std::fs::symlink_metadata(top.join(prefix).join(arg)).is_ok()
    }

    /// git's `verify_filename`: `arg` must be a path in the work tree.
    fn verify_filename(&mut self, arg: &str, diagnose: bool) -> Step {
        if arg.starts_with('-') {
            return Err(self.die(messages::option_after_path(arg)));
        }
        if self.path_exists(arg) {
            return Ok(());
        }
        if !diagnose {
            return Err(self.die(messages::no_such_path(arg)));
        }
        let reason = self.resolver.diagnose(arg);
        let diags = self.resolver.take_diagnostics();
        self.print_diags(&diags, false);
        if let Some(reason) = reason {
            let _ = writeln!(std::io::stderr(), "fatal: {reason}");
            self.print_diags(&diags, true);
            self.flush();
            return Err(Die::Exit(rgit_options::EXIT_FATAL));
        }
        self.print_diags(&diags, true);
        Err(self.die(messages::ambiguous_argument(arg)))
    }

    /// `--all`, `--branches`, `--tags`, `--remotes`, `--glob`.
    fn show_refs(&mut self, prefix: &str, pattern: Option<&str>, strip: bool) -> Step {
        let glob = pattern.map(|p| {
            let mut g = if strip || !p.starts_with("refs/") {
                format!("{prefix}{p}")
            } else {
                p.to_string()
            };
            if !has_wildcard(&g) {
                if !g.ends_with('/') {
                    g.push('/');
                }
                g.push('*');
            }
            g
        });
        let mut found: Vec<(String, ObjectId)> = Vec::new();
        let list_prefix = if strip { prefix } else { "refs/" };
        let r = self
            .resolver
            .repo()
            .refs
            .for_each_ref(list_prefix, &mut |name, oid| {
                found.push((name.to_string(), *oid));
            });
        if let Err(e) = r {
            return Err(self.rev_error(RevError::Lookup(e)));
        }
        let excludes = std::mem::take(&mut self.excludes);
        for (name, oid) in found {
            if let Some(g) = &glob {
                if !wildmatch(g.as_bytes(), name.as_bytes(), WildmatchFlags::NONE) {
                    continue;
                }
            }
            let shown = if strip {
                name[prefix.len()..].to_string()
            } else {
                name.clone()
            };
            if excludes
                .iter()
                .any(|x| wildmatch(x.as_bytes(), shown.as_bytes(), WildmatchFlags::NONE))
            {
                continue;
            }
            self.show_rev(false, &oid, Some(&shown))?;
        }
        Ok(())
    }

    /// `--bisect`: `refs/bisect/bad` and the excluded `refs/bisect/good-*`.
    /// `--bisect`: the refs `refs/bisect/<new>*`, then `^refs/bisect/<old>*`
    /// (prefix matches with the current terms, as the reference git).
    fn show_bisect_refs(&mut self) -> Step {
        let terms = crate::commands::bisect::Terms::of(self.setup);
        let new = format!("refs/bisect/{}", terms.new);
        let old = format!("refs/bisect/{}", terms.old);
        let mut found: Vec<(bool, String, ObjectId)> = Vec::new();
        let r = self
            .resolver
            .repo()
            .refs
            .for_each_ref("refs/bisect/", &mut |name, oid| {
                if name.starts_with(&new) {
                    found.push((false, name.to_string(), *oid));
                }
                if name.starts_with(&old) {
                    found.push((true, name.to_string(), *oid));
                }
            });
        if let Err(e) = r {
            return Err(self.rev_error(RevError::Lookup(e)));
        }
        found.sort_by_key(|f| f.0);
        for (negative, name, oid) in found {
            self.show_rev(negative, &oid, Some(&name))?;
        }
        Ok(())
    }

    fn disambiguate(&mut self, hex: &str) -> Step {
        let kind = self.resolver.repo().objects.hash_kind();
        if hex.is_empty() || hex.len() > kind.hex_len() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(());
        }
        let prefix = match rgit_hash::Prefix::from_hex(kind, hex.as_bytes()) {
            Ok(p) => p,
            Err(_) => return Ok(()),
        };
        match self.resolver.repo().objects.prefix_matches(&prefix) {
            Ok(mut ids) => {
                ids.sort();
                ids.dedup();
                for id in ids {
                    self.emit(&id.to_hex());
                }
                Ok(())
            }
            Err(e) => Err(self.rev_error(RevError::Lookup(e))),
        }
    }

    /// The location options handled before revisions were supported.
    fn location_option(&mut self, arg: &str) -> Option<Step> {
        let setup = self.setup;
        let line = match arg {
            "--git-dir" => git_dir(setup),
            "--absolute-git-dir" => setup.absolute_git_dir().display().to_string(),
            "--git-common-dir" => git_common_dir(setup),
            // The shared index of a split index; nothing otherwise.
            "--shared-index-path" => {
                let path = setup.cwd.join(rgit_index::index_path(&setup.git_dir));
                let index = rgit_index::Index::read_file_in(
                    &path,
                    setup.format.object_format,
                    &rgit_index::ReadOptions::default(),
                    Some(&setup.git_dir),
                );
                match index.ok().as_ref().and_then(|i| i.shared_index()) {
                    Some(id) if !id.is_null() => from_original_cwd(
                        setup,
                        &setup.git_dir.join(rgit_index::shared_index_name(id)),
                    ),
                    _ => return Some(Ok(())),
                }
            }
            "--show-toplevel" => match &setup.work_tree {
                Some(wt) => wt.display().to_string(),
                None => {
                    let d = repo::die(rgit_repo::Error::NoWorkTree);
                    self.flush();
                    return Some(Err(d));
                }
            },
            "--show-prefix" => setup
                .prefix
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            "--show-cdup" => match show_cdup(setup) {
                Some(l) => l,
                None => return Some(Ok(())),
            },
            "--is-bare-repository" => setup.is_bare().to_string(),
            "--show-ref-format" => match setup.format.ref_storage {
                rgit_repo::RefStorage::Files => "files".to_owned(),
                rgit_repo::RefStorage::Reftable => "reftable".to_owned(),
            },
            "--show-object-format"
            | "--show-object-format=storage"
            | "--show-object-format=input"
            | "--show-object-format=output" => setup.format.object_format.name().to_owned(),
            // An empty line without a compatibility format.
            "--show-object-format=compat" => setup
                .format
                .compat_object_format
                .map(|k| k.name().to_owned())
                .unwrap_or_default(),
            _ if arg.starts_with("--show-object-format=") => {
                let mode = &arg["--show-object-format=".len()..];
                return Some(Err(self.die(messages::unknown_object_format_mode(mode))));
            }
            "--is-inside-work-tree" => setup.is_inside_work_tree().to_string(),
            "--is-inside-git-dir" => setup.is_inside_git_dir().to_string(),
            "--is-shallow-repository" => {
                let file = rgit_fetch::shallow::file_for(&setup.common_dir);
                let shallow = std::fs::metadata(file).is_ok_and(|m| m.len() > 0);
                shallow.to_string()
            }
            _ => return None,
        };
        let _ = writeln!(self.out, "{line}");
        Some(Ok(()))
    }

    /// Handle an option; `Ok(false)` if `arg` is not a rev-parse option.
    fn option(&mut self, args: &[String], i: &mut usize) -> Result<bool, Die> {
        let arg = args[*i].as_str();
        if let Some(step) = self.location_option(arg) {
            step?;
            return Ok(true);
        }
        if arg == "--git-path" {
            *i += 1;
            let Some(p) = args.get(*i) else {
                return Err(self.die(messages::option_requires_argument("--git-path")));
            };
            let line = git_path(self.setup, p);
            let _ = writeln!(self.out, "{line}");
            return Ok(true);
        }
        if NOT_YET.iter().any(|o| arg == *o || arg.starts_with(&format!("{o}="))) {
            return Err(self.die(messages::rev_parse_not_supported(arg)));
        }
        match arg {
            "--verify" => {
                self.filter &= !(DO_FLAGS | DO_NOREV);
                self.verify = true;
            }
            "-q" | "--quiet" => self.quiet = true,
            "--short" => {
                self.filter &= !(DO_FLAGS | DO_NOREV);
                self.verify = true;
                self.abbrev = Some(self.resolver.default_abbrev());
            }
            "--symbolic" => self.symbolic = Symbolic::Plain,
            "--symbolic-full-name" => self.symbolic = Symbolic::Full,
            "--abbrev-ref" => {
                self.abbrev_ref = Some(self.resolver.options().warn_ambiguous_refs);
            }
            "--not" => self.reversed = !self.reversed,
            "--revs-only" => self.filter &= !DO_NOREV,
            "--no-revs" => self.filter &= !DO_REVS,
            "--flags" => self.filter &= !DO_NONFLAGS,
            "--no-flags" => self.filter &= !DO_FLAGS,
            "--sq" => self.sq = true,
            "--default" => {
                *i += 1;
                match args.get(*i) {
                    Some(d) => self.default = Some(d.clone()),
                    None => {
                        return Err(self.die(messages::option_requires_argument("--default")));
                    }
                }
            }
            "--all" => self.show_refs("refs/", None, false)?,
            "--branches" => self.show_refs("refs/heads/", None, true)?,
            "--tags" => self.show_refs("refs/tags/", None, true)?,
            "--remotes" => self.show_refs("refs/remotes/", None, true)?,
            "--bisect" => self.show_bisect_refs()?,
            _ => {
                let date = [
                    ("--since=", true),
                    ("--after=", true),
                    ("--until=", false),
                    ("--before=", false),
                ]
                .iter()
                .find_map(|(p, max)| arg.strip_prefix(p).map(|v| (*max, v)));
                if let Some((max, value)) = date {
                    // `--since`/`--after` become `--max-age`, `--until` and
                    // `--before` `--min-age`, with the date as a timestamp.
                    let ctx = rgit_date::DateContext::from_env();
                    let t = rgit_date::approxidate(value, &ctx);
                    let flag = if max { "--max-age" } else { "--min-age" };
                    self.show_flag(&format!("{flag}={t}"));
                } else if let Some(n) = arg.strip_prefix("--short=") {
                    self.filter &= !(DO_FLAGS | DO_NOREV);
                    self.verify = true;
                    let hexsz = self.resolver.repo().objects.hash_kind().hex_len();
                    let n: usize = n.parse().unwrap_or(0);
                    self.abbrev = Some(n.clamp(4, hexsz));
                } else if let Some(mode) = arg.strip_prefix("--abbrev-ref=") {
                    let strict = match mode {
                        "strict" => true,
                        "loose" => false,
                        _ => return Err(self.die(messages::unknown_abbrev_ref_mode(mode))),
                    };
                    self.abbrev_ref = Some(strict);
                } else if let Some(p) = arg.strip_prefix("--branches=") {
                    self.show_refs("refs/heads/", Some(p), true)?;
                } else if let Some(p) = arg.strip_prefix("--tags=") {
                    self.show_refs("refs/tags/", Some(p), true)?;
                } else if let Some(p) = arg.strip_prefix("--remotes=") {
                    self.show_refs("refs/remotes/", Some(p), true)?;
                } else if let Some(p) = arg.strip_prefix("--glob=") {
                    self.show_refs("refs/", Some(p), false)?;
                } else if let Some(p) = arg.strip_prefix("--exclude=") {
                    self.excludes.push(p.to_string());
                } else if let Some(p) = arg.strip_prefix("--disambiguate=") {
                    self.disambiguate(p)?;
                } else {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// A revision (or path) argument.
    fn revision(&mut self, arg: &str) -> Step {
        let flags = Flags {
            quiet: self.verify && self.quiet,
            ..Flags::default()
        };
        let items = match self.resolver.try_range(arg, flags) {
            Ok(Some(items)) => Some(items),
            Ok(None) => match self.resolver.try_parent_shorthand(arg, flags) {
                Ok(i) => i,
                Err(e) => return Err(self.rev_error(e)),
            },
            Err(e) => return Err(self.rev_error(e)),
        };
        if let Some(items) = items {
            self.drain_diags();
            return self.show_items(&items);
        }
        let (name, negated) = match arg.strip_prefix('^') {
            Some(n) => (n, true),
            None => (arg, false),
        };
        match self.resolver.resolve_with(name, flags) {
            Ok(oid) => {
                self.drain_diags();
                if self.verify {
                    self.revs_count += 1;
                    self.verified = Some((oid, negated, name.to_string()));
                    Ok(())
                } else {
                    self.show_rev(negated, &oid, Some(name))
                }
            }
            Err(RevError::NotFound) => {
                self.drain_diags();
                if self.verify {
                    return Err(self.die_no_single_rev());
                }
                if self.has_dashdash {
                    return Err(self.die(messages::bad_revision(arg)));
                }
                self.as_is = 1;
                self.show_default()?;
                if !self.show_file(arg) {
                    return Ok(());
                }
                self.verify_filename(arg, true)
            }
            Err(e) => Err(self.rev_error(e)),
        }
    }

    fn rev_error(&mut self, e: RevError) -> Die {
        match e {
            RevError::Fatal(m) => self.resolver_fatal(m),
            RevError::Lookup(e) => {
                self.drain_diags();
                for line in &e.errors {
                    messages::print_error(line);
                }
                match e.fatal {
                    Some(m) => self.die(m),
                    None => {
                        self.flush();
                        Die::Exit(rgit_options::EXIT_FATAL)
                    }
                }
            }
            RevError::NotFound => self.die_no_single_rev(),
        }
    }

    fn run(&mut self, args: &[String]) -> CmdResult {
        let mut i = 0;
        while i < args.len() {
            let arg = args[i].as_str();
            if self.as_is > 0 {
                if self.show_file(arg) && self.as_is < 2 {
                    self.verify_filename(arg, false)?;
                }
                i += 1;
                continue;
            }
            if arg == "--" || (!self.end_of_options && arg.starts_with('-')) {
                if arg == "--" {
                    self.as_is = 2;
                    if !self.verify {
                        self.show_default()?;
                    }
                    if self.filter & (DO_FLAGS | DO_REVS) != 0 {
                        self.show_file(arg);
                    }
                } else if arg == "--end-of-options" {
                    self.end_of_options = true;
                    if self.filter & (DO_FLAGS | DO_REVS) != 0 {
                        self.show_file(arg);
                    }
                } else if !self.option(args, &mut i)? {
                    self.show_flag(arg);
                }
                i += 1;
                continue;
            }
            self.revision(arg)?;
            i += 1;
        }
        if self.verify {
            if self.revs_count == 0 {
                if let Some(def) = self.default.take() {
                    self.revision(&def)?;
                }
            }
            if self.revs_count != 1 {
                return Err(self.die_no_single_rev());
            }
            if let Some((oid, negated, name)) = self.verified.take() {
                self.show_rev(negated, &oid, Some(&name))?;
            }
        } else {
            self.show_default()?;
        }
        self.flush();
        Ok(0)
    }

    /// `--default <rev>` when no revision was shown.
    fn show_default(&mut self) -> Step {
        let Some(def) = self.default.take() else {
            return Ok(());
        };
        if self.shown_rev {
            return Ok(());
        }
        match self.resolver.resolve_with(&def, Flags::default()) {
            Ok(oid) => {
                self.drain_diags();
                self.show_rev(false, &oid, Some(&def))
            }
            Err(RevError::NotFound) => {
                self.drain_diags();
                Ok(())
            }
            Err(e) => Err(self.rev_error(e)),
        }
    }
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let args = &argv[1.min(argv.len())..];
    match args.first().map(String::as_str) {
        Some("-h" | "--help-all") => {
            let _ = std::io::stdout().write_all(messages::REV_PARSE_USAGE.as_bytes());
            return Ok(0);
        }
        Some("--parseopt") => return parseopt::parseopt_mode(&args[1..]),
        Some("--sq-quote") => return parseopt::sq_quote_mode(&args[1..]),
        _ => {}
    }
    let setup = repo::setup(ctx, Mode::RequiredQuiet)?;
    let kind = setup.format.object_format;
    let config = revisions::load_config(ctx, &setup)?;
    let opts = revisions::options(ctx, &config, kind)?;
    let shallow = std::env::var_os("GIT_SHALLOW_FILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| setup.common_dir.join("shallow"));
    let mut objects = Objects::new(revisions::objects_dir(&setup), kind, &shallow);
    let refs = Refs::new(&setup, kind);
    objects.use_replace_refs(&refs, &config, ctx.no_replace_objects);
    objects.use_grafts(&setup.common_dir, &config);
    objects.use_commit_graph(&config, &setup);
    let cfg = Config(&config);
    let index = IndexFile::new(&setup, kind);
    let work_tree = setup.work_tree.clone().map(WorkTree);
    let repo = Repo {
        objects: &objects,
        refs: &refs,
        config: &cfg,
        index: Some(&index),
        work_tree: work_tree
            .as_ref()
            .map(|w| w as &dyn rgit_revwalk::revision::WorkTreeLookup),
        work_tree_info: revisions::work_tree_info(&setup),
    };
    let resolver = Resolver::new(repo, opts);
    let mut st = State {
        resolver: &resolver,
        setup: &setup,
        out: Vec::new(),
        filter: DO_REVS | DO_NOREV | DO_FLAGS | DO_NONFLAGS,
        verify: false,
        quiet: false,
        sq: false,
        abbrev: None,
        symbolic: Symbolic::None,
        abbrev_ref: None,
        reversed: false,
        default: None,
        shown_rev: false,
        as_is: 0,
        has_dashdash: args.iter().any(|a| a == "--"),
        end_of_options: false,
        revs_count: 0,
        verified: None,
        excludes: Vec::new(),
    };
    st.run(args)
}

/// `--git-dir`: as set up, but absolute when the command moved to the top
/// of the work tree.
fn git_dir(setup: &Setup) -> String {
    if setup.git_dir.is_absolute() || setup.prefix.is_none() {
        return setup.git_dir.display().to_string();
    }
    rgit_repo::path::normalize(&setup.cwd.join(&setup.git_dir))
        .display()
        .to_string()
}

/// `--git-common-dir`: the common directory, relative to the original
/// directory when it is relative.
fn git_common_dir(setup: &Setup) -> String {
    from_original_cwd(setup, &setup.common_dir)
}

/// `dir` (relative to the top of the work tree, or absolute) as seen from
/// the directory the command started in.
fn from_original_cwd(setup: &Setup, dir: &Path) -> String {
    if dir.is_absolute() {
        return dir.display().to_string();
    }
    match &setup.prefix {
        Some(p) => {
            let depth = Path::new(p).components().count();
            format!("{}{}", "../".repeat(depth), dir.display())
        }
        None => dir.display().to_string(),
    }
}

/// Whether `path` (below a git directory) is shared by all worktrees and
/// lives in the common directory (`gitrepository-layout(5)`; the table
/// follows the `git` binary's `rev-parse --git-path`).
fn is_common_path(path: &str) -> bool {
    const PRIVATE: &[&str] = &[
        "logs/HEAD",
        "info/sparse-checkout",
        "refs/bisect",
        "refs/worktree",
        "refs/rewritten",
        "logs/refs/bisect",
        "logs/refs/worktree",
        "logs/refs/rewritten",
    ];
    const DIRS: &[&str] = &[
        "branches", "common", "hooks", "info", "logs", "lost-found", "objects", "refs",
        "remotes", "rr-cache", "svn", "worktrees",
    ];
    const FILES: &[&str] = &["config", "gc.pid", "packed-refs", "shallow"];
    let under = |p: &str, d: &str| {
        p == d || p.strip_prefix(d).is_some_and(|r| r.starts_with('/'))
    };
    // Repeated slashes do not count, nor does a `.lock` suffix.
    let norm = collapse_slashes(path);
    let path = norm.strip_suffix(".lock").unwrap_or(&norm);
    if PRIVATE.iter().any(|d| under(path, d)) {
        return false;
    }
    DIRS.iter().any(|d| under(path, d))
        || FILES.contains(&path)
}

/// `path` with runs of slashes collapsed to one.
fn collapse_slashes(path: &str) -> String {
    let mut norm = String::with_capacity(path.len());
    for c in path.chars() {
        if !(c == '/' && norm.ends_with('/')) {
            norm.push(c);
        }
    }
    norm
}

/// `path` relocated by `GIT_INDEX_FILE` (`index`), `GIT_GRAFT_FILE`
/// (`info/grafts`) or `GIT_OBJECT_DIRECTORY` (`objects` and below), as
/// given in the environment.
fn relocated_path(path: &str) -> Option<String> {
    let norm = collapse_slashes(path);
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    match norm.as_str() {
        "index" => var("GIT_INDEX_FILE"),
        "info/grafts" => var("GIT_GRAFT_FILE"),
        _ => {
            let rest = norm.strip_prefix("objects")?;
            if !rest.is_empty() && !rest.starts_with('/') {
                return None;
            }
            var("GIT_OBJECT_DIRECTORY").map(|d| format!("{d}{rest}"))
        }
    }
}

/// `--git-path <path>`: `path` in the git directory or, for shared files,
/// the common directory.
fn git_path(setup: &Setup, path: &str) -> String {
    if let Some(p) = relocated_path(path) {
        return p;
    }
    let common = is_common_path(path);
    let base = if common {
        &setup.common_dir
    } else {
        &setup.git_dir
    };
    // A path moved to the common directory of a linked worktree loses
    // its repeated slashes.
    let path = if common && setup.common_dir != setup.git_dir {
        collapse_slashes(path)
    } else {
        path.to_string()
    };
    let full = if base.as_os_str() == "." {
        std::path::PathBuf::from(&path)
    } else {
        base.join(&path)
    };
    from_original_cwd(setup, &full)
}

/// `--show-cdup`: the way back up to the top of the work tree; the work
/// tree itself when the command started outside it; nothing without one.
fn show_cdup(setup: &Setup) -> Option<String> {
    let wt = setup.work_tree.as_deref()?;
    match &setup.prefix {
        Some(p) => {
            let depth = Path::new(p).components().count();
            Some("../".repeat(depth))
        }
        None if setup.original_cwd == wt => Some(String::new()),
        None => Some(wt.display().to_string()),
    }
}
