// SPDX-License-Identifier: MIT
//! `git add -i`: the main menu (Documentation/git-add.adoc,
//! "INTERACTIVE MODE"), checked against the `git` binary.
//!
//! The status shows per path the line counts of the staged change
//! (`HEAD` to the index) and of the unstaged one (index to work tree).
//! The selection lists accept numbers, ranges (`2-5`, `7-`), unique
//! prefixes, `-` to unselect and `*` for everything; an item's unique
//! prefix of at most four characters is highlighted. Staging and
//! reverting go through `update-index` and `reset`, the hunks through the
//! patch mode.

use std::collections::BTreeMap;

use super::Options;
use super::patch::{self, Colors, Input};
use crate::commands::stash::child::{Call, Git};
use crate::messages::interactive as msg;
use crate::prelude::*;
use crate::repository::Repo;

/// One path of the status.
#[derive(Clone)]
struct FileStatus {
    path: String,
    /// The staged change (`None`: unchanged).
    index: Option<String>,
    /// The unstaged change (`None`: nothing).
    worktree: Option<String>,
}

struct Menu<'a> {
    repo: &'a Repo,
    git: Git,
    opts: &'a Options,
    colors: Colors,
    input: Input,
}

pub(crate) fn run(ctx: &Context, repo: &Repo, opts: &Options) -> CmdResult {
    let mut m = Menu {
        repo,
        git: Git::new(ctx, repo)?,
        opts,
        colors: patch::colors(repo)?,
        input: Input { single_key: false },
    };
    m.run()
}

/// The length of the unique prefix of each name (at most four
/// characters), or 0.
fn unique_prefixes(names: &[&str]) -> Vec<usize> {
    names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let mut ends: Vec<usize> = name.char_indices().map(|(p, c)| p + c.len_utf8()).collect();
            ends.truncate(4);
            ends.into_iter()
                .find(|&k| {
                    let prefix = &name[..k];
                    names
                        .iter()
                        .enumerate()
                        .all(|(j, other)| j == i || !other.starts_with(prefix))
                })
                .unwrap_or(0)
        })
        .collect()
}

impl Menu<'_> {
    fn child<S: AsRef<std::ffi::OsStr>>(&self, args: &[S]) -> Call<'_> {
        let mut c = self.git.cmd(args);
        if let Some(i) = &self.opts.index_file {
            c = c.index(i);
        }
        c
    }

    /// `HEAD`, or the empty tree on an unborn branch.
    fn base(&self) -> String {
        match self.repo.refs.resolve("HEAD") {
            Some(_) => "HEAD".into(),
            None => rgit_hash::ObjectId::empty_tree(self.repo.hash).to_hex(),
        }
    }

    /// Runs a listing child in the starting directory with the pathspecs.
    fn listing(&self, mut args: Vec<String>) -> Result<Vec<u8>, Die> {
        args.push("--".into());
        args.extend(self.opts.pathspec.iter().cloned());
        let dir = patch::user_dir(self.repo)?;
        let (code, out) = self.child(&args).in_dir(&dir).output()?;
        if code != 0 {
            return Err(Die::Exit(code));
        }
        Ok(out)
    }

    fn numstat(&self, args: &[&str]) -> Result<Vec<(String, String)>, Die> {
        let mut a: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        a.push("--numstat".into());
        a.push("-z".into());
        let out = self.listing(a)?;
        let mut v = Vec::new();
        for rec in out.split(|b| *b == 0).filter(|r| !r.is_empty()) {
            let text = String::from_utf8_lossy(rec);
            let mut parts = text.splitn(3, '\t');
            let (Some(add), Some(del), Some(path)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let shown = if add == "-" {
                msg::BINARY.to_owned()
            } else {
                format!("+{add}/-{del}")
            };
            v.push((path.to_owned(), shown));
        }
        Ok(v)
    }

    fn status(&self) -> Result<Vec<FileStatus>, Die> {
        let mut map: BTreeMap<String, FileStatus> = BTreeMap::new();
        let base = self.base();
        for (path, s) in self.numstat(&["diff-index", "--cached", &base])? {
            map.entry(path.clone())
                .or_insert(FileStatus {
                    path,
                    index: None,
                    worktree: None,
                })
                .index = Some(s);
        }
        for (path, s) in self.numstat(&["diff-files", "--ignore-submodules=dirty"])? {
            map.entry(path.clone())
                .or_insert(FileStatus {
                    path,
                    index: None,
                    worktree: None,
                })
                .worktree = Some(s);
        }
        Ok(map.into_values().collect())
    }

    fn untracked(&self) -> Result<Vec<String>, Die> {
        let out = self.listing(vec![
            "ls-files".into(),
            "--others".into(),
            "--exclude-standard".into(),
            "--full-name".into(),
            "-z".into(),
        ])?;
        Ok(out
            .split(|b| *b == 0)
            .filter(|r| !r.is_empty())
            .map(|r| String::from_utf8_lossy(r).into_owned())
            .collect())
    }

    fn header(&self, text: &str) {
        println!("{}{text}{}", self.colors.header, self.colors.reset);
    }

    fn error(&self, text: &str) {
        use std::io::Write;
        let _ = std::io::stdout().flush();
        eprintln!("{}{text}{}", self.colors.error, self.colors.reset);
    }

    fn help_lines(&self, lines: &[&str]) {
        for l in lines {
            println!("{}{l}{}", self.colors.help, self.colors.reset);
        }
    }

    /// A name with its unique prefix highlighted.
    fn highlight(&self, name: &str, prefix: usize) -> String {
        if prefix == 0 {
            return name.to_owned();
        }
        let (p, rest) = name.split_at(prefix);
        if self.colors.prompt.is_empty() {
            format!("[{p}]{rest}")
        } else {
            format!("{}{p}{}{rest}", self.colors.prompt, self.colors.reset)
        }
    }

    fn file_line(&self, f: &FileStatus, name: &str) -> String {
        format!(
            "{:>12} {:>12} {name}",
            f.index.as_deref().unwrap_or(msg::UNCHANGED),
            f.worktree.as_deref().unwrap_or(msg::NOTHING)
        )
    }

    /// The `status` command (also shown at the start).
    fn show_status(&self) -> Result<(), Die> {
        let files = self.status()?;
        if !files.is_empty() {
            self.header(msg::STATUS_HEADER);
            for (i, f) in files.iter().enumerate() {
                println!(" {:>2}: {}", i + 1, self.file_line(f, &f.path));
            }
        }
        println!();
        Ok(())
    }

    fn show_menu(&self) {
        self.header(msg::COMMANDS_HEADER);
        let prefixes = unique_prefixes(&msg::COMMANDS);
        let mut line = String::new();
        for (i, c) in msg::COMMANDS.iter().enumerate() {
            if i % 4 != 0 {
                line.push('\t');
            }
            line.push_str(&format!(
                " {:>2}: {}",
                i + 1,
                self.highlight(c, prefixes[i])
            ));
            if i % 4 == 3 {
                println!("{line}");
                line.clear();
            }
        }
        if !line.is_empty() {
            println!("{line}");
        }
    }

    fn run(&mut self) -> CmdResult {
        self.show_status()?;
        loop {
            self.show_menu();
            print!(
                "{}{}{}> ",
                self.colors.prompt,
                msg::WHAT_NOW,
                self.colors.reset
            );
            let Some(answer) = self.input.line() else {
                println!();
                println!("{}", msg::BYE);
                return Ok(0);
            };
            // One choice: the first word counts.
            let answer = answer
                .split(|c: char| c == ',' || c.is_whitespace())
                .find(|t| !t.is_empty())
                .unwrap_or("");
            if answer.is_empty() {
                continue;
            }
            if answer == "?" {
                self.help_lines(&msg::SINGLE_HELP);
                continue;
            }
            if answer.starts_with('-') {
                // Unselecting selects nothing here.
                println!();
                continue;
            }
            let chosen = match answer.parse::<usize>() {
                Ok(n) if (1..=msg::COMMANDS.len()).contains(&n) => Some(n - 1),
                Ok(_) => None,
                Err(_) => {
                    let hits: Vec<usize> = (0..msg::COMMANDS.len())
                        .filter(|&i| msg::COMMANDS[i].starts_with(answer))
                        .collect();
                    (hits.len() == 1).then(|| hits[0])
                }
            };
            let Some(cmd) = chosen else {
                self.error(&msg::huh(answer));
                continue;
            };
            match msg::COMMANDS[cmd] {
                "status" => self.show_status()?,
                "update" => self.update()?,
                "revert" => self.revert()?,
                "add untracked" => self.add_untracked()?,
                "patch" => self.patch()?,
                "diff" => self.diff()?,
                "quit" => {
                    println!("{}", msg::BYE);
                    return Ok(0);
                }
                _ => self.help_lines(&msg::MENU_HELP),
            }
        }
    }

    /// Lets the user pick items: `lines(i, name)` shows item `i` with its
    /// highlighted name. Returns the chosen indices.
    fn choose(
        &mut self,
        prompt: &str,
        single: bool,
        names: &[&str],
        line: &dyn Fn(&Self, usize, &str) -> String,
    ) -> Vec<usize> {
        let n = names.len();
        let prefixes = unique_prefixes(names);
        let mut selected = vec![false; n];
        loop {
            self.header(msg::STATUS_HEADER);
            for i in 0..n {
                let mark = if selected[i] { '*' } else { ' ' };
                let name = self.highlight(names[i], prefixes[i]);
                println!("{mark}{:>2}: {}", i + 1, line(self, i, &name));
            }
            print!("{}{prompt}{}>> ", self.colors.prompt, self.colors.reset);
            let Some(answer) = self.input.line() else {
                break;
            };
            let answer = answer.trim();
            if answer.is_empty() {
                break;
            }
            if answer == "?" {
                self.help_lines(if single {
                    &msg::SINGLE_HELP
                } else {
                    &msg::MULTI_HELP
                });
                continue;
            }
            if answer == "*" {
                selected.iter_mut().for_each(|s| *s = true);
                break;
            }
            for token in answer
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|t| !t.is_empty())
            {
                if let Err(bad) = select(token, names, &mut selected) {
                    self.error(&msg::huh(bad));
                    break;
                }
            }
            if single && selected.iter().any(|s| *s) {
                break;
            }
        }
        (0..n).filter(|&i| selected[i]).collect()
    }

    fn choose_files(&mut self, prompt: &str, single: bool, files: &[FileStatus]) -> Vec<usize> {
        let names: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        let files = files.to_vec();
        self.choose(prompt, single, &names, &move |m: &Self, i, name| {
            m.file_line(&files[i], name)
        })
    }

    fn update_index(&self, args: &[&str], paths: &[&str]) -> Result<(), Die> {
        let mut a = vec!["update-index"];
        a.extend_from_slice(args);
        a.extend(["-z", "--stdin"]);
        let mut input = Vec::new();
        for p in paths {
            input.extend_from_slice(p.as_bytes());
            input.push(0);
        }
        self.child(&a).stdin(input).status()?;
        Ok(())
    }

    fn update(&mut self) -> Result<(), Die> {
        let files: Vec<FileStatus> = self
            .status()?
            .into_iter()
            .filter(|f| f.worktree.is_some())
            .collect();
        if !files.is_empty() {
            let chosen = self.choose_files(msg::UPDATE_PROMPT, false, &files);
            if !chosen.is_empty() {
                let paths: Vec<&str> = chosen.iter().map(|&i| files[i].path.as_str()).collect();
                self.update_index(&["--add", "--remove"], &paths)?;
                println!("{}", msg::paths_done("updated", chosen.len()));
            }
        }
        println!();
        Ok(())
    }

    fn revert(&mut self) -> Result<(), Die> {
        let files: Vec<FileStatus> = self
            .status()?
            .into_iter()
            .filter(|f| f.index.is_some())
            .collect();
        if !files.is_empty() {
            let chosen = self.choose_files(msg::REVERT_PROMPT, false, &files);
            if !chosen.is_empty() {
                let paths: Vec<&str> = chosen.iter().map(|&i| files[i].path.as_str()).collect();
                // Paths that are not in HEAD leave the index.
                let base = self.base();
                let mut args = vec![
                    "diff-index".to_owned(),
                    "--cached".to_owned(),
                    "--name-only".to_owned(),
                    "-z".to_owned(),
                    "--diff-filter=A".to_owned(),
                    base,
                    "--".to_owned(),
                ];
                args.extend(paths.iter().map(|p| format!(":(top,literal){p}")));
                let (_, out) = self.child(&args).output()?;
                let added: Vec<String> = out
                    .split(|b| *b == 0)
                    .filter(|r| !r.is_empty())
                    .map(|r| String::from_utf8_lossy(r).into_owned())
                    .collect();
                let others: Vec<&str> = paths
                    .iter()
                    .copied()
                    .filter(|p| !added.iter().any(|a| a == p))
                    .collect();
                if !added.is_empty() {
                    let a: Vec<&str> = added.iter().map(String::as_str).collect();
                    self.update_index(&["--force-remove"], &a)?;
                }
                if !others.is_empty() {
                    let mut args = vec!["reset".to_owned(), "-q".to_owned(), "--".to_owned()];
                    args.extend(others.iter().map(|p| format!(":(top,literal){p}")));
                    self.child(&args).status()?;
                }
                for a in &added {
                    println!("{}", msg::untracked_now(a));
                }
                println!("{}", msg::paths_done("reverted", chosen.len()));
            }
        }
        println!();
        Ok(())
    }

    fn add_untracked(&mut self) -> Result<(), Die> {
        let files = self.untracked()?;
        if files.is_empty() {
            println!("{}", msg::NO_UNTRACKED);
            println!();
            return Ok(());
        }
        let names: Vec<&str> = files.iter().map(String::as_str).collect();
        let chosen = self.choose(msg::ADD_UNTRACKED_PROMPT, false, &names, &|_, _, name| {
            name.to_owned()
        });
        if !chosen.is_empty() {
            let paths: Vec<&str> = chosen.iter().map(|&i| names[i]).collect();
            self.update_index(&["--add"], &paths)?;
            println!("{}", msg::paths_done("added", chosen.len()));
        }
        println!();
        Ok(())
    }

    fn patch(&mut self) -> Result<(), Die> {
        let files: Vec<FileStatus> = self
            .status()?
            .into_iter()
            .filter(|f| f.worktree.as_deref().is_some_and(|w| w != msg::BINARY))
            .collect();
        if files.is_empty() {
            println!("{}", msg::NO_CHANGES);
            return Ok(());
        }
        let chosen = self.choose_files(msg::PATCH_PROMPT, false, &files);
        if chosen.is_empty() {
            return Ok(());
        }
        let mut opts = Options::new(self.opts.mode);
        opts.index_file = self.opts.index_file.clone();
        opts.context = self.opts.context;
        opts.inter_hunk = self.opts.inter_hunk;
        opts.pathspec = chosen
            .iter()
            .map(|&i| format!(":(top,literal){}", files[i].path))
            .collect();
        patch::run_with(&self.git, self.repo, &opts)?;
        Ok(())
    }

    fn diff(&mut self) -> Result<(), Die> {
        let files: Vec<FileStatus> = self
            .status()?
            .into_iter()
            .filter(|f| f.index.is_some())
            .collect();
        if !files.is_empty() {
            let chosen = self.choose_files(msg::DIFF_PROMPT, true, &files);
            if !chosen.is_empty() {
                let mut args = vec![
                    "diff-index".to_owned(),
                    "-p".to_owned(),
                    "--cached".to_owned(),
                    if self.colors.diff {
                        "--color"
                    } else {
                        "--no-color"
                    }
                    .to_owned(),
                    self.base(),
                    "--".to_owned(),
                ];
                args.extend(
                    chosen
                        .iter()
                        .map(|&i| format!(":(top,literal){}", files[i].path)),
                );
                self.child(&args).status()?;
            }
        }
        println!();
        Ok(())
    }
}

/// Applies one selection token; the token when it selects nothing valid.
fn select<'t>(token: &'t str, names: &[&str], selected: &mut [bool]) -> Result<(), &'t str> {
    let n = names.len();
    let (unselect, body) = match token.strip_prefix('-') {
        Some(rest) if !rest.is_empty() => (true, rest),
        _ => (false, token),
    };
    let numeric = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let range = match body.split_once('-') {
        Some((a, b)) if numeric(a) && (b.is_empty() || numeric(b)) => {
            let a: usize = a.parse().map_err(|_| token)?;
            let b: usize = if b.is_empty() {
                n
            } else {
                b.parse().map_err(|_| token)?
            };
            Some((a, b.min(n)))
        }
        None if numeric(body) => {
            let a: usize = body.parse().map_err(|_| token)?;
            Some((a, a))
        }
        _ => None,
    };
    let (from, to) = match range {
        Some((a, b)) => {
            if a < 1 || a > n || b < a {
                return Err(token);
            }
            (a - 1, b - 1)
        }
        None => {
            let hits: Vec<usize> = (0..n).filter(|&i| names[i].starts_with(body)).collect();
            if hits.len() != 1 {
                return Err(token);
            }
            (hits[0], hits[0])
        }
    };
    for s in &mut selected[from..=to] {
        *s = !unselect;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_are_short_and_unique() {
        let names = ["ab1", "ab2", "abc1", "abcd1", "abcd2", "x.c", "f"];
        assert_eq!(unique_prefixes(&names), [3, 3, 4, 0, 0, 1, 1]);
    }

    #[test]
    fn selections() {
        let names = ["a", "b", "c", "d", "e"];
        let mut sel = vec![false; 5];
        select("2-3", &names, &mut sel).unwrap();
        select("5-", &names, &mut sel).unwrap();
        assert_eq!(sel, [false, true, true, false, true]);
        select("-2", &names, &mut sel).unwrap();
        assert_eq!(sel, [false, false, true, false, true]);
        assert!(select("9", &names, &mut sel).is_err());
        select("d", &names, &mut sel).unwrap();
        assert!(sel[3]);
    }
}
