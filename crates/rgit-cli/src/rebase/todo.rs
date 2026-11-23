// SPDX-License-Identifier: MIT
//! The todo list of the merge back end (`.git/rebase-merge/git-rebase-todo`,
//! Documentation/git-rebase.adoc "INTERACTIVE MODE" and "REBASING
//! MERGES"): its commands, parsing, writing (full or abbreviated object
//! names, `rebase.abbreviateCommands`), `--autosquash` and `--exec`.
//!
//! The details (which lines survive editing, how arguments are trimmed,
//! the error lines) were taken from the behaviour of the `git` binary.

use rgit_hash::ObjectId;

use crate::messages::rebase as msg;
use crate::messages::sequencer as seqmsg;
use crate::repository::Repo;

/// A todo command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cmd {
    Pick,
    Reword,
    Edit,
    Squash,
    Fixup,
    Exec,
    Break,
    Drop,
    Label,
    Reset,
    Merge,
    UpdateRef,
    Noop,
}

const COMMANDS: &[(Cmd, &str, Option<char>)] = &[
    (Cmd::Pick, "pick", Some('p')),
    (Cmd::Reword, "reword", Some('r')),
    (Cmd::Edit, "edit", Some('e')),
    (Cmd::Squash, "squash", Some('s')),
    (Cmd::Fixup, "fixup", Some('f')),
    (Cmd::Exec, "exec", Some('x')),
    (Cmd::Break, "break", Some('b')),
    (Cmd::Drop, "drop", Some('d')),
    (Cmd::Label, "label", Some('l')),
    (Cmd::Reset, "reset", Some('t')),
    (Cmd::Merge, "merge", Some('m')),
    (Cmd::UpdateRef, "update-ref", Some('u')),
    (Cmd::Noop, "noop", None),
];

impl Cmd {
    pub(crate) fn name(self) -> &'static str {
        COMMANDS
            .iter()
            .find(|(c, _, _)| *c == self)
            .map(|(_, n, _)| *n)
            .unwrap_or("noop")
    }

    fn abbrev(self) -> Option<char> {
        COMMANDS
            .iter()
            .find(|(c, _, _)| *c == self)
            .and_then(|(_, _, a)| *a)
    }

    fn from_word(word: &str) -> Option<Cmd> {
        COMMANDS.iter().find_map(|(c, name, abbrev)| {
            let short = abbrev.is_some_and(|a| word.len() == 1 && word.starts_with(a));
            (*name == word || short).then_some(*c)
        })
    }

    /// Commands whose argument is a commit to apply.
    pub(crate) fn picks(self) -> bool {
        matches!(
            self,
            Cmd::Pick | Cmd::Reword | Cmd::Edit | Cmd::Squash | Cmd::Fixup
        )
    }

    /// Commands that name a commit (`drop` too).
    fn takes_commit(self) -> bool {
        self.picks() || self == Cmd::Drop
    }

    /// `squash` or `fixup`.
    pub(crate) fn melds(self) -> bool {
        matches!(self, Cmd::Squash | Cmd::Fixup)
    }
}

/// One command of the todo list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Item {
    pub cmd: Cmd,
    /// `fixup -C`/`-c`, `merge -C`/`-c`: `'C'` or `'c'`.
    pub flag: Option<char>,
    /// The commit (picks, `drop`, `merge -C`).
    pub commit: Option<ObjectId>,
    /// The rest of the line: the commit's description, the command of
    /// `exec`, the label of `label`/`reset`, the labels of `merge`, the
    /// ref of `update-ref`.
    pub arg: String,
}

impl Item {
    pub(crate) fn pick(commit: ObjectId, arg: String) -> Item {
        Item {
            cmd: Cmd::Pick,
            flag: None,
            commit: Some(commit),
            arg,
        }
    }

    pub(crate) fn new(cmd: Cmd, arg: impl Into<String>) -> Item {
        Item {
            cmd,
            flag: None,
            commit: None,
            arg: arg.into(),
        }
    }

    /// The line, with the full object name.
    pub(crate) fn line(&self) -> String {
        self.render(false, &|id: &ObjectId| id.to_hex())
    }

    /// The line; `abbrev_cmd` writes the one-letter command, `name`
    /// spells the commit.
    pub(crate) fn render(&self, abbrev_cmd: bool, name: &dyn Fn(&ObjectId) -> String) -> String {
        let mut out = String::new();
        match self.cmd.abbrev() {
            Some(a) if abbrev_cmd => out.push(a),
            _ => out.push_str(self.cmd.name()),
        }
        if let Some(f) = self.flag {
            out.push_str(" -");
            out.push(f);
        }
        if let Some(c) = &self.commit {
            out.push(' ');
            out.push_str(&name(c));
        }
        if !self.arg.is_empty() {
            out.push(' ');
            out.push_str(&self.arg);
        }
        out
    }
}

/// A line of the todo list: a command, or a blank or comment line kept
/// as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Line {
    Cmd(Item),
    Comment(String),
}

impl Line {
    pub(crate) fn item(&self) -> Option<&Item> {
        match self {
            Line::Cmd(i) => Some(i),
            Line::Comment(_) => None,
        }
    }
}

/// A todo list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Todo {
    pub lines: Vec<Line>,
}

impl Todo {
    pub(crate) fn from_items(items: Vec<Item>) -> Todo {
        Todo {
            lines: items.into_iter().map(Line::Cmd).collect(),
        }
    }

    pub(crate) fn items(&self) -> impl Iterator<Item = &Item> {
        self.lines.iter().filter_map(Line::item)
    }

    /// The number of commands.
    pub(crate) fn count(&self) -> usize {
        self.items().count()
    }

    /// Removes and returns the first command, dropping the blank and
    /// comment lines before it.
    pub(crate) fn pop(&mut self) -> Option<Item> {
        while !self.lines.is_empty() {
            if let Line::Cmd(i) = self.lines.remove(0) {
                return Some(i);
            }
        }
        None
    }

    pub(crate) fn push_front(&mut self, item: Item) {
        self.lines.insert(0, Line::Cmd(item));
    }

    /// The next command, if any.
    pub(crate) fn peek(&self) -> Option<&Item> {
        self.items().next()
    }

    /// The text with full object names.
    pub(crate) fn text(&self) -> String {
        self.render(false, &|id: &ObjectId| id.to_hex())
    }

    pub(crate) fn render(&self, abbrev_cmd: bool, name: &dyn Fn(&ObjectId) -> String) -> String {
        let mut out = String::new();
        for l in &self.lines {
            match l {
                Line::Cmd(i) => out.push_str(&i.render(abbrev_cmd, name)),
                Line::Comment(c) => out.push_str(c),
            }
            out.push('\n');
        }
        out
    }

    /// Removes the comment lines (not the blank ones), as after the user
    /// edited the list.
    pub(crate) fn strip_comments(&mut self, comment: &str) {
        self.lines.retain(|l| match l {
            Line::Comment(c) => !c.trim_start().starts_with(comment),
            Line::Cmd(_) => true,
        });
        while matches!(self.lines.last(), Some(Line::Comment(c)) if c.trim().is_empty()) {
            self.lines.pop();
        }
    }
}

/// A line that could not be parsed: the error lines git prints, and the
/// advice (`rebaseTodoError`) shown after the first one.
#[derive(Debug, Clone)]
pub(crate) struct BadLine {
    pub errors: Vec<String>,
    pub hint: Option<&'static str>,
}

impl BadLine {
    /// Prints the lines; `advise` shows the hint.
    pub(crate) fn print(&self, advise: &dyn Fn(&str)) {
        for (k, e) in self.errors.iter().enumerate() {
            eprintln!("error: {e}");
            if k == 0
                && let Some(h) = self.hint
            {
                advise(h);
            }
        }
    }
}

/// A commit a todo line names, and whether it is a merge.
pub(crate) struct Named {
    pub id: ObjectId,
    pub merge: bool,
}

/// Parses `text`; `resolve` names commits. Blank lines at the start are
/// dropped, the other blank and comment lines are kept.
pub(crate) fn parse(
    text: &str,
    comment: &str,
    resolve: &mut dyn FnMut(&str) -> Option<Named>,
) -> Result<Todo, Vec<BadLine>> {
    let mut todo = Todo::default();
    let mut bad = Vec::new();
    let mut started = false;
    let mut offset = 0;
    for (n, raw) in text.lines().enumerate() {
        let start = offset;
        offset += raw.len() + 1;
        let line = raw.trim_start();
        if line.is_empty() || line.starts_with(comment) {
            if started || !line.is_empty() {
                todo.lines.push(Line::Comment(raw.to_owned()));
                started = true;
            }
            continue;
        }
        started = true;
        // git quotes the rest of the file after `break`/`noop` arguments.
        let tail = &text[(start + raw.len() - line.len()).min(text.len())..];
        match parse_line(line, tail, resolve) {
            Ok(item) => todo.lines.push(Line::Cmd(item)),
            Err((e, hint)) => bad.push(BadLine {
                errors: vec![e, seqmsg::invalid_line(n + 1, raw)],
                hint,
            }),
        }
    }
    if bad.is_empty() { Ok(todo) } else { Err(bad) }
}

fn split_word(s: &str) -> (&str, &str) {
    let end = s.find([' ', '\t']).unwrap_or(s.len());
    (&s[..end], s[end..].trim_start_matches([' ', '\t']))
}

/// Why a line is bad: the error, and the advice to show with it.
type LineError = (String, Option<&'static str>);

fn parse_line(
    line: &str,
    tail: &str,
    resolve: &mut dyn FnMut(&str) -> Option<Named>,
) -> Result<Item, LineError> {
    parse_line_inner(line, tail, resolve).map_err(|e| match e {
        Bad::Plain(m) => (m, None),
        Bad::Hinted(m, h) => (m, Some(h)),
    })
}

enum Bad {
    Plain(String),
    Hinted(String, &'static str),
}

impl From<String> for Bad {
    fn from(m: String) -> Bad {
        Bad::Plain(m)
    }
}

fn parse_line_inner(
    line: &str,
    tail: &str,
    resolve: &mut dyn FnMut(&str) -> Option<Named>,
) -> Result<Item, Bad> {
    let (word, rest) = split_word(line);
    let Some(cmd) = Cmd::from_word(word) else {
        return Err(seqmsg::invalid_command(word).into());
    };
    let rest = rest.trim_end();
    let mut item = Item::new(cmd, "");
    match cmd {
        Cmd::Noop | Cmd::Break => {
            if !rest.is_empty() {
                let (_, after) = split_word(tail);
                return Err(msg::does_not_accept_arguments(cmd.name(), after).into());
            }
        }
        Cmd::Exec | Cmd::Label | Cmd::Reset | Cmd::UpdateRef => {
            if rest.is_empty() {
                return Err(msg::missing_arguments(cmd.name()).into());
            }
            item.arg = rest.to_owned();
            check_name(cmd, rest)?;
        }
        Cmd::Merge => {
            let mut rest = rest;
            let (w, r) = split_word(rest);
            if let Some((flag, attached)) = flag_word(w) {
                item.flag = Some(flag);
                let (name, r) = if attached.is_empty() {
                    split_word(r)
                } else {
                    (attached, r)
                };
                item.commit = Some(lookup(resolve, name)?.id);
                rest = r;
            }
            if rest.is_empty() {
                return Err(msg::missing_arguments(cmd.name()).into());
            }
            item.arg = rest.to_owned();
        }
        _ => {
            debug_assert!(cmd.takes_commit());
            let mut rest = rest;
            if cmd == Cmd::Fixup {
                let (w, r) = split_word(rest);
                if let Some((flag, attached)) = flag_word(w) {
                    item.flag = Some(flag);
                    // `-C<commit>`: the commit follows directly.
                    rest = if attached.is_empty() { r } else { &rest[2..] };
                }
            }
            if rest.is_empty() && item.flag.is_none() {
                return Err(msg::missing_arguments(cmd.name()).into());
            }
            let (name, r) = split_word(rest);
            let named = lookup(resolve, name)?;
            if named.merge {
                match cmd {
                    Cmd::Pick => {
                        return Err(Bad::Hinted(
                            msg::no_merge_commits(cmd.name()),
                            msg::PICK_MERGE_HINT,
                        ));
                    }
                    Cmd::Reword => {
                        return Err(Bad::Hinted(
                            msg::no_merge_commits(cmd.name()),
                            msg::REWORD_MERGE_HINT,
                        ));
                    }
                    Cmd::Edit => {
                        return Err(Bad::Hinted(
                            msg::no_merge_commits(cmd.name()),
                            msg::EDIT_MERGE_HINT,
                        ));
                    }
                    Cmd::Fixup | Cmd::Squash => {
                        return Err(msg::CANNOT_SQUASH_MERGE.to_owned().into());
                    }
                    _ => {}
                }
            }
            item.commit = Some(named.id);
            item.arg = r.to_owned();
        }
    }
    Ok(item)
}

/// `-C`/`-c`, possibly with the commit attached (`-C<commit>`).
fn flag_word(word: &str) -> Option<(char, &str)> {
    let rest = word.strip_prefix('-')?;
    let flag = rest.chars().next().filter(|c| matches!(c, 'C' | 'c'))?;
    Some((flag, &rest[1..]))
}

fn lookup(resolve: &mut dyn FnMut(&str) -> Option<Named>, name: &str) -> Result<Named, Bad> {
    if name.is_empty() {
        return Err(seqmsg::could_not_parse(name).into());
    }
    resolve(name).ok_or_else(|| seqmsg::could_not_parse(name).into())
}

/// The names `label` and `update-ref` take must be valid ref names
/// (`update-ref` a full one).
fn check_name(cmd: Cmd, name: &str) -> Result<(), Bad> {
    use rgit_refs::name::{RefNameFlags, is_valid_refname};
    let valid = is_valid_refname(name.as_bytes(), RefNameFlags::ONELEVEL);
    match cmd {
        Cmd::Label if name == "#" || !valid => Err(msg::invalid_label(name).into()),
        Cmd::UpdateRef if !valid => Err(msg::invalid_refname(name).into()),
        Cmd::UpdateRef if !name.starts_with("refs/") => {
            Err(msg::update_ref_needs_full_name(name).into())
        }
        _ => Ok(()),
    }
}

/// The commits a todo list picks or drops (for `rebase.missingCommitsCheck`).
pub(crate) fn commits(todo: &Todo) -> Vec<ObjectId> {
    todo.items()
        .filter(|i| i.cmd.takes_commit())
        .filter_map(|i| i.commit)
        .collect()
}

/// `--autosquash`: moves the `fixup!`, `squash!` and `amend!` commits
/// after the commit they name (by subject prefix or object name) and
/// turns them into `fixup`, `squash` or `fixup -C`.
pub(crate) fn autosquash(repo: &mut Repo, todo: &mut Todo) {
    let subjects: Vec<Option<String>> = todo
        .lines
        .iter()
        .map(|l| {
            let i = l.item()?;
            // A `merge -C` is a target too (by its subject).
            if !i.cmd.picks() && i.cmd != Cmd::Merge {
                return None;
            }
            let c = i.commit?;
            // Subjects are compared in the commit encoding.
            match crate::sequencer::read_commit(repo, &c) {
                Ok(cm) => Some(
                    String::from_utf8_lossy(&rgit_commit::message::subject(cm.message()))
                        .into_owned(),
                ),
                Err(_) => None,
            }
        })
        .collect();
    let n = todo.lines.len();
    // For each line: the line it is attached to, and how.
    let mut target: Vec<Option<(usize, Cmd, Option<char>)>> = vec![None; n];
    for i in 0..n {
        let Some(subject) = &subjects[i] else {
            continue;
        };
        if todo.lines[i].item().is_some_and(|it| it.cmd == Cmd::Merge) {
            continue;
        }
        let mut s = subject.as_str();
        let mut kind: Option<(Cmd, Option<char>)> = None;
        // Repeated prefixes ("fixup! fixup! x") name the same target.
        loop {
            let (k, rest) = if let Some(r) = s.strip_prefix("fixup! ") {
                ((Cmd::Fixup, None), r)
            } else if let Some(r) = s.strip_prefix("squash! ") {
                ((Cmd::Squash, None), r)
            } else if let Some(r) = s.strip_prefix("amend! ") {
                ((Cmd::Fixup, Some('C')), r)
            } else {
                break;
            };
            kind.get_or_insert(k);
            s = rest;
        }
        let Some((cmd, flag)) = kind else {
            continue;
        };
        let wanted = s.trim_start();
        let found = (0..i).find_map(|j| {
            let sj = subjects[j].as_deref()?;
            (sj == wanted).then_some(j)
        });
        let found = found.or_else(|| {
            // An object name.
            if wanted.contains(' ') || wanted.is_empty() {
                return None;
            }
            let id = super::resolve_commit(repo, wanted)?;
            (0..i).find(|&j| todo.lines[j].item().and_then(|it| it.commit) == Some(id))
        });
        let found = found.or_else(|| {
            (0..i).find(|&j| {
                subjects[j]
                    .as_deref()
                    .is_some_and(|sj| !wanted.is_empty() && sj.starts_with(wanted))
            })
        });
        if let Some(j) = found {
            target[i] = Some((j, cmd, flag));
        }
    }
    // Each marked commit follows the commit it names, after the ones that
    // named it before (and their own followers).
    fn emit(
        i: usize,
        lines: &[Line],
        target: &[Option<(usize, Cmd, Option<char>)>],
        out: &mut Vec<Line>,
    ) {
        let mut line = lines[i].clone();
        if let (Some((_, cmd, flag)), Line::Cmd(it)) = (target[i], &mut line) {
            it.cmd = cmd;
            it.flag = flag;
        }
        out.push(line);
        for k in i + 1..lines.len() {
            if target[k].is_some_and(|(t, _, _)| t == i) {
                emit(k, lines, target, out);
            }
        }
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        if target[i].is_none() {
            emit(i, &todo.lines, &target, &mut out);
        }
    }
    todo.lines = out;
}

/// `--exec`: the commands after each pick (after the last `fixup` or
/// `squash` of a chain), and at the end.
pub(crate) fn add_exec(todo: &mut Todo, execs: &[String]) {
    if execs.is_empty() {
        return;
    }
    let lines = std::mem::take(&mut todo.lines);
    let mut out = Vec::new();
    let mut pending = false;
    for l in lines {
        let is_meld = l.item().is_some_and(|i| i.cmd.melds());
        let is_pick = l
            .item()
            .is_some_and(|i| i.cmd.picks() || i.cmd == Cmd::Merge);
        // Anything but a `fixup`/`squash` ends the chain (the comments of
        // `--update-refs` too).
        if pending && !is_meld {
            for e in execs {
                out.push(Line::Cmd(Item::new(Cmd::Exec, e.clone())));
            }
            pending = false;
        }
        if is_pick {
            pending = true;
        }
        out.push(l);
    }
    if pending {
        for e in execs {
            out.push(Line::Cmd(Item::new(Cmd::Exec, e.clone())));
        }
    }
    todo.lines = out;
}
