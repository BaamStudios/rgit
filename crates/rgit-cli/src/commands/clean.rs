// SPDX-License-Identifier: MIT
//! `git clean` (Documentation/git-clean.adoc, config/clean.adoc): remove
//! untracked files from the work tree. Options `-n`, `-f` (twice for
//! nested repositories), `-d`, `-x`, `-X`, `-e`, `-q`, pathspecs and
//! `clean.requireForce`, and the interactive mode (`-i`) with its menu
//! (clean, filter by pattern, select by numbers, ask each, quit, help).
//! Colours in the interactive mode are not implemented yet.
//!
//! What is removed comes from the shared work tree walk
//! ([`crate::untracked`]). A directory is removed depth first; when
//! something inside it cannot be removed (a nested repository without a
//! second `-f`, a failure), the paths that were removed are listed one by
//! one instead of the directory.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use rgit_pathspec::{ParseOptions, Pathspec};
use rgit_worktree::ignore::{Excludes, PatternList};

use super::ls_files::{ExcludeArg, build_excludes};
use crate::messages::clean as msg;
use crate::prelude::*;
use crate::quote::{relative_to, write_quoted};
use crate::repo::Mode;
use crate::repository::{self, Repo};
use crate::untracked::{self, CleanItem, Walker, is_nested_repo, os_path};

struct Cleaner<'a> {
    root: &'a Path,
    prefix: Vec<u8>,
    quote_high: bool,
    dry_run: bool,
    quiet: bool,
    force_repos: bool,
    failed: bool,
}

impl Cleaner<'_> {
    fn shown(&self, path: &[u8]) -> String {
        let rel = relative_to(path, &self.prefix);
        let mut out = Vec::new();
        write_quoted(&mut out, &rel, self.quote_high);
        String::from_utf8_lossy(&out).into_owned()
    }

    fn removed_line(&self, path: &[u8]) -> String {
        let shown = self.shown(path);
        if self.dry_run {
            msg::would_remove(&shown)
        } else {
            msg::removing(&shown)
        }
    }

    fn say(&self, line: &str) {
        if !self.quiet {
            println!("{line}");
        }
    }

    fn warn_failed(&mut self, path: &[u8], err: &std::io::Error) {
        self.failed = true;
        eprintln!(
            "warning: {}",
            msg::failed_to_remove(&self.shown(path), &crate::global::strerror(err))
        );
    }

    /// Removes a file (or symbolic link); `false` after a failure.
    fn remove_file(&mut self, path: &[u8]) -> bool {
        if self.dry_run {
            return true;
        }
        match std::fs::remove_file(os_path(self.root, path)) {
            Ok(()) => true,
            Err(e) => {
                self.warn_failed(path, &e);
                false
            }
        }
    }

    /// Removes the directory `dir` (no trailing slash) with everything in
    /// it. Returns whether it is gone; if not, the paths removed inside are
    /// listed. `top` is a directory the walk chose, shown with a slash.
    fn remove_dir(&mut self, dir: &[u8], top: bool) -> bool {
        let full = os_path(self.root, dir);
        if !top && !self.force_repos && is_nested_repo(&full) {
            let shown = self.shown(dir);
            self.say(&if self.dry_run {
                msg::would_skip_repo(&shown)
            } else {
                msg::skipping_repo(&shown)
            });
            return false;
        }
        let mut lines = Vec::new();
        let mut all_gone = true;
        if let Ok(rd) = std::fs::read_dir(&full) {
            let mut entries: Vec<(Vec<u8>, bool)> = rd
                .flatten()
                .filter_map(|e| {
                    let is_dir = e.file_type().ok()?.is_dir();
                    Some((e.file_name().as_encoded_bytes().to_vec(), is_dir))
                })
                .collect();
            entries.sort();
            for (name, is_dir) in entries {
                let mut path = dir.to_vec();
                path.push(b'/');
                path.extend_from_slice(&name);
                let gone = if is_dir {
                    self.remove_dir(&path, false)
                } else {
                    self.remove_file(&path)
                };
                if gone {
                    lines.push(self.removed_line(&path));
                } else {
                    all_gone = false;
                }
            }
        }
        let gone = all_gone
            && (self.dry_run
                || match std::fs::remove_dir(&full) {
                    Ok(()) => true,
                    Err(e) => {
                        let mut shown = dir.to_vec();
                        if top {
                            shown.push(b'/');
                        }
                        self.warn_failed(&shown, &e);
                        false
                    }
                });
        if !gone {
            for l in &lines {
                self.say(l);
            }
        }
        gone
    }
}

/// The width lists are laid out for: `$COLUMNS`, else 80.
fn term_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse::<usize>().ok())
        .filter(|w| *w > 0)
        .unwrap_or(80)
}

/// The columns `s` takes on a terminal; control characters take none.
fn display_width(s: &str) -> usize {
    s.chars().filter(|c| !c.is_control()).count()
}

/// Prints `cells` in columns, indented by two spaces, two spaces apart,
/// filled down the columns (`column_major`) or across the rows.
fn print_columns(out: &mut impl Write, cells: &[String], column_major: bool) {
    if cells.is_empty() {
        return;
    }
    let width = cells.iter().map(|c| display_width(c)).max().unwrap_or(0) + 2;
    let max_cols = (term_width().saturating_sub(3) / width).max(1);
    let n = cells.len();
    let (rows, cols) = if column_major {
        let rows = n.div_ceil(max_cols);
        (rows, n.div_ceil(rows))
    } else {
        let cols = max_cols.min(n);
        (n.div_ceil(cols), cols)
    };
    for r in 0..rows {
        let mut line = String::from("  ");
        let row: Vec<&String> = (0..cols)
            .filter_map(|c| {
                let i = if column_major { c * rows + r } else { r * cols + c };
                cells.get(i)
            })
            .collect();
        for (k, cell) in row.iter().enumerate() {
            line.push_str(cell);
            if k + 1 < row.len() {
                let pad = width - display_width(cell);
                line.extend(std::iter::repeat_n(' ', pad));
            }
        }
        let _ = writeln!(out, "{line}");
    }
}

/// Numbered cells (`*` marks the chosen ones).
fn numbered(names: &[String], chosen: &[bool]) -> Vec<String> {
    names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let mark = if chosen.get(i).copied().unwrap_or(false) {
                '*'
            } else {
                ' '
            };
            format!("{mark}{:>2}: {n}", i + 1)
        })
        .collect()
}

/// The interactive mode (Documentation/git-clean.adoc, "Interactive
/// mode").
struct Interactive {
    items: Vec<CleanItem>,
    /// How each item is shown (relative to the current directory, quoted,
    /// directories with a slash).
    names: Vec<String>,
    /// The same without quoting, for patterns.
    rel: Vec<Vec<u8>>,
    /// The same as text, for the numbered list and name prefixes.
    plain: Vec<String>,
    icase: bool,
    out: std::io::Stdout,
    /// The last answer without its line ending, untrimmed.
    raw: String,
}

impl Interactive {
    fn new(cleaner: &Cleaner<'_>, items: Vec<CleanItem>, icase: bool) -> Interactive {
        let mut names = Vec::new();
        let mut rel = Vec::new();
        for it in &items {
            let mut p = it.path.clone();
            if it.is_dir {
                p.push(b'/');
            }
            names.push(cleaner.shown(&p));
            rel.push(relative_to(&p, &cleaner.prefix));
        }
        let plain = rel
            .iter()
            .map(|r| String::from_utf8_lossy(r).into_owned())
            .collect();
        Interactive {
            items,
            names,
            rel,
            plain,
            icase,
            out: std::io::stdout(),
            raw: String::new(),
        }
    }

    fn prompt(&mut self, text: &str) -> Option<String> {
        let _ = write!(self.out, "{text}");
        let _ = self.out.flush();
        let mut line = String::new();
        match std::io::stdin().lock().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => {
                self.raw = line.trim_end_matches(['\n', '\r']).to_string();
                Some(line.trim().to_string())
            }
        }
    }

    fn say(&mut self, text: &str) {
        let _ = writeln!(self.out, "{text}");
    }

    /// Keeps only the items at the positions where `keep` is true.
    fn retain(&mut self, keep: &[bool]) {
        let mut k = keep.iter();
        self.items.retain(|_| *k.next().unwrap_or(&false));
        let mut k = keep.iter();
        self.names.retain(|_| *k.next().unwrap_or(&false));
        let mut k = keep.iter();
        self.rel.retain(|_| *k.next().unwrap_or(&false));
        let mut k = keep.iter();
        self.plain.retain(|_| *k.next().unwrap_or(&false));
    }

    fn show_items(&mut self) {
        self.say(msg::would_remove_header(self.items.len()));
        print_columns(&mut self.out, &self.names, true);
    }

    fn show_menu(&mut self) {
        self.say(msg::COMMANDS_HEADER);
        let names: Vec<String> = msg::MENU.iter().map(|s| (*s).to_string()).collect();
        print_columns(&mut self.out, &numbered(&names, &[]), false);
    }

    /// The menu loop. The items to remove, or `None` to remove nothing.
    fn run(mut self) -> Option<Vec<CleanItem>> {
        self.show_items();
        loop {
            self.show_menu();
            let Some(input) = self.prompt(msg::PROMPT_MENU) else {
                self.say(msg::BYE);
                return None;
            };
            if input.is_empty() {
                continue;
            }
            if input == "?" {
                self.say(msg::PROMPT_HELP_SINGLE);
                continue;
            }
            let choice = match input.parse::<usize>() {
                Ok(n) if (1..=msg::MENU.len()).contains(&n) => Some(n - 1),
                Ok(_) => None,
                Err(_) => {
                    let hits: Vec<usize> = (0..msg::MENU.len())
                        .filter(|&i| msg::MENU[i].starts_with(&input))
                        .collect();
                    (hits.len() == 1).then(|| hits[0])
                }
            };
            match choice {
                None => self.say(&msg::huh(&input)),
                Some(0) => return Some(self.items),
                Some(c @ (1 | 2)) => {
                    if c == 1 {
                        self.filter_by_pattern();
                    } else {
                        self.select_by_numbers();
                    }
                    if self.items.is_empty() {
                        self.say(msg::NO_MORE_FILES);
                        return None;
                    }
                    self.show_items();
                }
                Some(3) => return Some(self.ask_each()),
                Some(4) => {
                    self.say(msg::BYE);
                    return None;
                }
                _ => {
                    self.say(msg::MENU_HELP);
                    self.show_items();
                }
            }
        }
    }

    fn filter_by_pattern(&mut self) {
        print_columns(&mut self.out, &self.names, true);
        loop {
            let Some(input) = self.prompt(msg::PROMPT_FILTER) else {
                return;
            };
            if input.is_empty() {
                return;
            }
            // The words form one pattern list (the last matching pattern
            // decides, so "* !*.out" keeps the *.out items), applied like
            // ignore rules: a pattern for a leading directory counts too.
            let words: Vec<&str> = input.split_whitespace().collect();
            let mut ex = Excludes::new(PathBuf::new());
            ex.set_ignore_case(self.icase);
            ex.add_list(PatternList::from_bytes(
                words.join("\n").as_bytes(),
                None,
                Vec::new(),
            ));
            let keep: Vec<bool> = self
                .items
                .iter()
                .zip(&self.rel)
                .map(|(it, rel)| {
                    let bare = rel.strip_suffix(b"/").unwrap_or(rel);
                    !ex.match_with_leading_dirs(bare, it.is_dir)
                        .is_some_and(|m| m.is_excluded())
                })
                .collect();
            if keep.iter().all(|k| *k) {
                // Named like git: the answer up to the end of its first word.
                let raw = self.raw.clone();
                let start = raw.len() - raw.trim_start().len();
                let end = raw[start..]
                    .find(char::is_whitespace)
                    .map_or(raw.len(), |i| start + i);
                self.say(&msg::no_items_matched(&raw[..end]));
                continue;
            }
            self.retain(&keep);
            if self.items.is_empty() {
                return;
            }
            print_columns(&mut self.out, &self.names, true);
        }
    }

    /// One selection word: the positions it names, or `None` when it
    /// names nothing.
    fn parse_selection(&self, word: &str) -> Option<Vec<usize>> {
        let n = self.items.len();
        if word == "*" {
            return Some((0..n).collect());
        }
        let num = |s: &str| s.parse::<usize>().ok().filter(|v| (1..=n).contains(v));
        if word.starts_with(|c: char| c.is_ascii_digit()) {
            if let Some((a, b)) = word.split_once('-') {
                let a = num(a)?;
                let b = if b.is_empty() { n } else { num(b)? };
                return (a <= b).then(|| (a - 1..b).collect());
            }
            return num(word).map(|v| vec![v - 1]);
        }
        let hits: Vec<usize> = (0..n).filter(|&i| self.plain[i].starts_with(word)).collect();
        (hits.len() == 1).then_some(hits)
    }

    fn select_by_numbers(&mut self) {
        let mut chosen = vec![false; self.items.len()];
        loop {
            print_columns(&mut self.out, &numbered(&self.plain, &chosen), false);
            let input = match self.prompt(msg::PROMPT_SELECT) {
                Some(i) if !i.is_empty() => i,
                _ => break,
            };
            if input == "?" {
                self.say(msg::PROMPT_HELP_MULTI);
                continue;
            }
            for word in input.split(|c: char| c.is_whitespace() || c == ',') {
                if word.is_empty() {
                    continue;
                }
                let (unselect, w) = match word.strip_prefix('-') {
                    Some(rest) => (true, rest),
                    None => (false, word),
                };
                match self.parse_selection(w) {
                    Some(positions) => {
                        for p in positions {
                            chosen[p] = !unselect;
                        }
                    }
                    None => self.say(&msg::huh(word)),
                }
            }
        }
        self.retain(&chosen);
    }

    fn ask_each(mut self) -> Vec<CleanItem> {
        let mut keep = vec![false; self.items.len()];
        let questions: Vec<String> = self.names.iter().map(|n| msg::ask_remove(n)).collect();
        for (k, q) in keep.iter_mut().zip(&questions) {
            match self.prompt(q) {
                Some(a) => {
                    *k = a.eq_ignore_ascii_case("y") || a.eq_ignore_ascii_case("yes");
                }
                None => {
                    self.say("");
                    break;
                }
            }
        }
        self.retain(&keep);
        self.items
    }
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let quiet = Cell::new(false);
    let dry_run = Cell::new(false);
    let force = Cell::new(0);
    let interactive = Cell::new(false);
    let dirs = Cell::new(false);
    let excludes: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let no_ignore = Cell::new(false);
    let only_ignored = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag('q', "quiet", &quiet, msg::HELP_QUIET),
            Opt::flag('n', "dry-run", &dry_run, msg::HELP_DRY_RUN),
            Opt::force(&force, msg::HELP_FORCE),
            Opt::flag('i', "interactive", &interactive, msg::HELP_INTERACTIVE),
            Opt::flag('d', None, &dirs, msg::HELP_D).noneg(),
            Opt::callback('e', "exclude", Some("pattern"), msg::HELP_EXCLUDE, |v, _| {
                excludes
                    .borrow_mut()
                    .push(v.unwrap_or_default().to_string());
                Ok(())
            })
            .noneg(),
            Opt::flag('x', None, &no_ignore, msg::HELP_X).noneg(),
            Opt::flag('X', None, &only_ignored, msg::HELP_ONLY_IGNORED).noneg(),
        ],
    );
    let args = parser.parse(argv)?.args;
    drop(parser);

    let repo = Repo::open(ctx, Mode::Required)?;
    let require_force = repo
        .config
        .bool("clean.requireforce")
        .map_err(|e| Die::Fatal(e.to_string()))?
        .unwrap_or(true);
    if require_force && force.get() == 0 && !dry_run.get() && !interactive.get() {
        return Err(Die::Fatal(msg::REQUIRE_FORCE.to_string()));
    }
    if no_ignore.get() && only_ignored.get() {
        return Err(Die::Fatal(msg::X_AND_ONLY_IGNORED.to_string()));
    }
    let work_tree = repo.work_tree()?;
    let prefix = repo.prefix();
    let opts = ParseOptions {
        prefix: prefix.clone(),
        root: Some(work_tree.as_os_str().as_encoded_bytes().to_vec()),
        globals: repository::pathspec_globals(ctx),
        ..Default::default()
    };
    let explicit = !args.is_empty();
    let spec_args: Vec<String> = if explicit || prefix.is_empty() {
        args.clone()
    } else {
        vec![".".to_string()]
    };
    let pathspec =
        Pathspec::parse(&spec_args, &opts).map_err(|e| Die::Fatal(e.to_string()))?;

    let mut ex = if no_ignore.get() {
        let mut ex = Excludes::new(work_tree.clone());
        ex.set_ignore_case(repo.config.bool("core.ignorecase").ok().flatten().unwrap_or(false));
        ex
    } else {
        build_excludes(&repo, &[ExcludeArg::Standard])?
    };
    for p in excludes.borrow().iter() {
        ex.add_pattern(p.as_bytes());
    }

    let (index, _) = repo.read_index()?;
    if !no_ignore.get() {
        untracked::use_skip_worktree_ignores(&repo, &index, b".gitignore", &mut ex);
    }
    let roots = {
        let mut walker = Walker {
            root: &work_tree,
            index: &index,
            excludes: Some(&mut ex),
            pathspec: (!pathspec.is_empty()).then_some(&pathspec),
            scan_excluded: false,
        };
        walker.walk()
    };
    for w in ex.take_warnings() {
        eprintln!("warning: {w}");
    }
    let mut items = untracked::clean_items(
        &roots,
        only_ignored.get(),
        dirs.get() || explicit,
        force.get() > 1,
    );

    let mut cleaner = Cleaner {
        root: &work_tree,
        prefix,
        quote_high: repo.quote_path(),
        dry_run: dry_run.get(),
        quiet: quiet.get(),
        force_repos: force.get() > 1,
        failed: false,
    };
    if interactive.get() && !items.is_empty() {
        let icase = repo.config.bool("core.ignorecase").ok().flatten().unwrap_or(false);
        match Interactive::new(&cleaner, items, icase).run() {
            Some(chosen) => items = chosen,
            None => return Ok(0),
        }
    }
    for item in &items {
        if item.is_dir {
            if cleaner.remove_dir(&item.path, true) {
                let mut p = item.path.clone();
                p.push(b'/');
                let line = cleaner.removed_line(&p);
                cleaner.say(&line);
            }
        } else if cleaner.remove_file(&item.path) {
            let line = cleaner.removed_line(&item.path);
            cleaner.say(&line);
        }
    }
    Ok(i32::from(cleaner.failed))
}
