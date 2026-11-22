// SPDX-License-Identifier: MIT
//! `git shortlog` (Documentation/git-shortlog.adoc): commits grouped by
//! author (or committer, trailer values, a format), counted or listed with
//! their subjects. Without revisions and with standard input not being a
//! terminal, the output of `git log` is read from standard input instead.
//!
//! Details determined with the reference `git`: groups are sorted by
//! name byte-wise (`-n`: by count, then name); each group lists its
//! commits oldest first; a description is the first paragraph of the
//! subject (or of the `--format` expansion) on one line, `<none>` when it
//! is empty, and a subject starting with `[PATCH...]` loses that prefix.

use std::collections::{HashMap, HashSet};
use std::io::{IsTerminal, Read, Write};

use crate::history::layout;
use crate::history::mailmap::Mailmap;
use crate::messages::shortlog as msg;
use crate::prelude::*;

/// What commits are grouped by.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Group {
    Author,
    Committer,
    /// A trailer key (compared case-insensitively).
    Trailer(String),
    /// A user format.
    Format(String),
}

/// `-w[<width>[,<indent1>[,<indent2>]]]`.
#[derive(Debug, Clone, Copy)]
struct Wrap {
    width: usize,
    indent1: usize,
    indent2: usize,
}

struct Settings {
    numbered: bool,
    summary: bool,
    email: bool,
    wrap: Option<Wrap>,
}

fn parse_wrap(v: Option<&str>) -> Option<Wrap> {
    let mut w = Wrap {
        width: 76,
        indent1: 6,
        indent2: 9,
    };
    let Some(v) = v else {
        return Some(w);
    };
    let mut parts = v.split(',');
    let mut field = |slot: &mut usize| -> Option<()> {
        if let Some(p) = parts.next() {
            *slot = p.parse().ok()?;
        }
        Some(())
    };
    field(&mut w.width)?;
    field(&mut w.indent1)?;
    field(&mut w.indent2)?;
    if parts.next().is_some() {
        return None;
    }
    Some(w)
}

fn parse_group(v: &str) -> Result<Group, String> {
    if v.eq_ignore_ascii_case("author") {
        Ok(Group::Author)
    } else if v.eq_ignore_ascii_case("committer") {
        Ok(Group::Committer)
    } else if let Some(k) = v.strip_prefix("trailer:") {
        Ok(Group::Trailer(k.to_owned()))
    } else if let Some(f) = v.strip_prefix("format:") {
        Ok(Group::Format(f.to_owned()))
    } else if v.contains('%') {
        Ok(Group::Format(v.to_owned()))
    } else {
        Err(msg::unknown_group(v))
    }
}

/// A revision argument among the arguments left for the walk.
fn has_revisions(args: &[String]) -> bool {
    for a in args {
        if a == "--" {
            return false;
        }
        let pseudo = ["--all", "--branches", "--tags", "--remotes", "--glob"]
            .iter()
            .any(|p| a == p || a.starts_with(&format!("{p}=")));
        if pseudo || !a.starts_with('-') {
            return true;
        }
    }
    false
}

/// The first paragraph of `text` on one line (newlines become spaces);
/// `None` when it is empty.
fn first_paragraph(text: &[u8]) -> Vec<u8> {
    let end = text
        .windows(2)
        .position(|w| w == b"\n\n")
        .unwrap_or(text.len());
    let para = &text[..end];
    let para = para.strip_suffix(b"\n").unwrap_or(para);
    para.iter()
        .map(|&b| if b == b'\n' { b' ' } else { b })
        .collect()
}

/// The first paragraph of a commit message on one line, each line
/// trimmed.
fn subject(message: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut started = false;
    for line in message.split(|&b| b == b'\n') {
        let line = line.trim_ascii();
        if line.is_empty() {
            if started {
                break;
            }
            continue;
        }
        if started {
            out.push(b' ');
        }
        started = true;
        out.extend_from_slice(line);
    }
    out
}

/// `[PATCH...]` and the whitespace after it removed.
fn strip_patch(s: &[u8]) -> &[u8] {
    if s.starts_with(b"[PATCH")
        && let Some(i) = s.iter().position(|&b| b == b']')
    {
        return s[i + 1..].trim_ascii_start();
    }
    s
}

/// `Name <email>` split into name and email; `None` without `<...>`.
fn split_ident(s: &[u8]) -> Option<(&[u8], &[u8])> {
    let lt = s.iter().position(|&b| b == b'<')?;
    let gt = lt + s[lt..].iter().position(|&b| b == b'>')?;
    Some((s[..lt].trim_ascii(), &s[lt + 1..gt]))
}

/// The group name of an identity: mailmapped, with `<email>` for `-e`.
fn ident_key(mailmap: &Mailmap, name: &[u8], email: &[u8], with_email: bool) -> Vec<u8> {
    let (name, email) = mailmap.map(name, email);
    let mut key = name.to_vec();
    if with_email {
        key.extend_from_slice(b" <");
        key.extend_from_slice(email);
        key.push(b'>');
    }
    key
}

/// The collected groups.
#[derive(Default)]
struct Groups {
    names: Vec<Vec<u8>>,
    index: HashMap<Vec<u8>, usize>,
    /// The descriptions of each group, newest first.
    entries: Vec<Vec<Vec<u8>>>,
}

impl Groups {
    fn add(&mut self, key: Vec<u8>, desc: Vec<u8>) {
        let i = match self.index.get(&key) {
            Some(&i) => i,
            None => {
                self.names.push(key.clone());
                self.entries.push(Vec::new());
                self.index.insert(key, self.names.len() - 1);
                self.names.len() - 1
            }
        };
        self.entries[i].push(desc);
    }

    fn output(&self, s: &Settings) -> Vec<u8> {
        let mut order: Vec<usize> = (0..self.names.len()).collect();
        order.sort_by(|&a, &b| {
            let by_name = self.names[a].cmp(&self.names[b]);
            if s.numbered {
                self.entries[b]
                    .len()
                    .cmp(&self.entries[a].len())
                    .then(by_name)
            } else {
                by_name
            }
        });
        let mut out = Vec::new();
        for i in order {
            let name = &self.names[i];
            let entries = &self.entries[i];
            if s.summary {
                out.extend_from_slice(format!("{:6}\t", entries.len()).as_bytes());
                out.extend_from_slice(name);
                out.push(b'\n');
                continue;
            }
            out.extend_from_slice(name);
            out.extend_from_slice(format!(" ({}):\n", entries.len()).as_bytes());
            for desc in entries.iter().rev() {
                match s.wrap {
                    Some(w) => {
                        out.extend_from_slice(&layout::wrap(desc, w.width, w.indent1, w.indent2));
                    }
                    None => {
                        out.extend_from_slice(b"      ");
                        out.extend_from_slice(desc);
                    }
                }
                out.push(b'\n');
            }
            out.push(b'\n');
        }
        out
    }
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let committer = Cell::new(false);
    let numbered = Cell::new(false);
    let summary = Cell::new(false);
    let email = Cell::new(false);
    let wrap: Cell<Option<Wrap>> = Cell::new(None);
    let groups: RefCell<Vec<Group>> = RefCell::new(Vec::new());
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::callback('c', "committer", None, msg::HELP_COMMITTER, |_, unset| {
                committer.set(!unset);
                Ok(())
            })
            .noarg(),
            Opt::flag('n', "numbered", &numbered, msg::HELP_NUMBERED),
            Opt::flag('s', "summary", &summary, msg::HELP_SUMMARY),
            Opt::flag('e', "email", &email, msg::HELP_EMAIL),
            Opt::callback('w', None, Some(msg::ARGH_WRAP), msg::HELP_WRAP, |v, unset| {
                if unset {
                    wrap.set(None);
                    return Ok(());
                }
                match parse_wrap(v) {
                    Some(w) => {
                        wrap.set(Some(w));
                        Ok(())
                    }
                    None => Err(CallbackError::Error(msg::BAD_WRAP.to_owned())),
                }
            })
            .optional(None)
            .literal_argh(),
            Opt::callback(None, "group", Some("field"), msg::HELP_GROUP, |v, unset| {
                if unset {
                    groups.borrow_mut().clear();
                    return Ok(());
                }
                let g = parse_group(v.unwrap_or_default()).map_err(CallbackError::Error)?;
                groups.borrow_mut().push(g);
                Ok(())
            }),
        ],
    )
    .flags(rgit_options::ParseFlags {
        keep_unknown_opt: true,
        keep_dashdash: true,
        ..Default::default()
    });
    let rest = parser.parse(argv)?.args;
    let mut groups = groups.take();
    if committer.get() {
        groups.push(Group::Committer);
    }
    if groups.is_empty() {
        groups.push(Group::Author);
    }
    let settings = Settings {
        numbered: numbered.get(),
        summary: summary.get(),
        email: email.get(),
        wrap: wrap.get(),
    };
    // Options of our own among the walk arguments.
    let mut format: Option<String> = None;
    let mut output: Option<String> = None;
    let mut walk_args = Vec::new();
    let mut seen_dashdash = false;
    for a in rest {
        if !seen_dashdash {
            if let Some(f) = a
                .strip_prefix("--format=")
                .or_else(|| a.strip_prefix("--pretty="))
            {
                format = Some(f.to_owned());
                continue;
            }
            if let Some(o) = a.strip_prefix("--output=") {
                output = Some(o.to_owned());
                continue;
            }
            if a == "--oneline" || a == "--pretty" {
                continue;
            }
            seen_dashdash = a == "--";
        }
        walk_args.push(a);
    }
    let setup = crate::repo::setup_gently_quiet(ctx)?;
    let from_stdin = match &setup {
        None => {
            if walk_args.iter().any(|a| !a.starts_with('-')) {
                return Err(parser.error_with_usage(msg::TOO_MANY_ARGUMENTS).into());
            }
            true
        }
        Some(_) => {
            !has_revisions(&walk_args)
                && (!std::io::stdin().is_terminal() || no_current_branch(ctx))
        }
    };
    let out = if from_stdin {
        if groups.len() > 1 {
            return Err(Die::Fatal(msg::STDIN_MULTIPLE.to_owned()));
        }
        match &groups[0] {
            Group::Trailer(_) => return Err(Die::Fatal(msg::STDIN_TRAILER.to_owned())),
            Group::Format(_) => return Err(Die::Fatal(msg::STDIN_FORMAT.to_owned())),
            _ => {}
        }
        let mailmap = match &setup {
            Some(_) => crate::history::mailmap::Mailmap::load_for(ctx)?,
            None => {
                let mut mm = Mailmap::default();
                if let Ok(text) = std::fs::read(".mailmap") {
                    mm.add(&text);
                }
                mm
            }
        };
        let mut input = Vec::new();
        let _ = std::io::stdin().lock().read_to_end(&mut input);
        let mut collected = Groups::default();
        read_log(
            &input,
            groups[0] == Group::Committer,
            &mailmap,
            &settings,
            &mut collected,
        );
        collected.output(&settings)
    } else {
        walk(ctx, &walk_args, format.as_deref(), &groups, &settings).map_err(|e| {
            // An option neither shortlog nor the walk knows.
            let unknown = walk_args
                .iter()
                .find(|a| matches!(&e, Die::Fatal(m) if *m == crate::messages::rev_list::unrecognized_argument(a)));
            match unknown {
                Some(a) => parser.error_with_usage(&msg::unknown_option(a)).into(),
                None => e,
            }
        })?
    };
    match output {
        Some(path) => {
            std::fs::write(&path, &out).map_err(|e| Die::Fatal(crate::global::strerror(&e)))?;
        }
        None => {
            let mut stdout = std::io::stdout().lock();
            let _ = stdout.write_all(&out);
            let _ = stdout.flush();
        }
    }
    Ok(0)
}

/// `HEAD` does not point to a branch with commits.
fn no_current_branch(ctx: &Context) -> bool {
    match crate::repository::Repo::open(ctx, crate::repo::Mode::Required) {
        Ok(mut repo) => repo.resolve("HEAD").is_none(),
        Err(_) => true,
    }
}

/// Group the commits of a walk.
fn walk(
    ctx: &Context,
    args: &[String],
    format: Option<&str>,
    groups: &[Group],
    s: &Settings,
) -> Result<Vec<u8>, Die> {
    let mut formats: Vec<Vec<u8>> = Vec::new();
    if let Some(f) = format {
        formats.push(f.as_bytes().to_vec());
    }
    let group_formats: Vec<usize> = groups
        .iter()
        .filter_map(|g| match g {
            Group::Format(f) => {
                formats.push(f.as_bytes().to_vec());
                Some(formats.len() - 1)
            }
            _ => None,
        })
        .collect();
    let (items, mailmap) = crate::history::walk_formatted(ctx, args, &formats)?;
    let trailer_conf = rgit_trailer::Config::default();
    let mut collected = Groups::default();
    for item in &items {
        // Without `--format`: the subject as stored (not sanitized like
        // `%s`).
        let text = match format {
            Some(_) => first_paragraph(&item.expanded[0]),
            None => subject(item.commit.message()),
        };
        let desc = if text.is_empty() {
            msg::NONE.as_bytes().to_vec()
        } else {
            strip_patch(text.trim_ascii_start()).to_vec()
        };
        let mut keys: Vec<Vec<u8>> = Vec::new();
        let mut next_format = group_formats.iter();
        for g in groups {
            match g {
                Group::Author | Group::Committer => {
                    let sig = if *g == Group::Author {
                        item.commit.author()
                    } else {
                        item.commit.committer()
                    };
                    if let Some(sig) = sig {
                        keys.push(ident_key(&mailmap, sig.name, sig.email, s.email));
                    }
                }
                Group::Format(_) => {
                    if let Some(&i) = next_format.next() {
                        keys.push(item.expanded[i].clone());
                    }
                }
                Group::Trailer(key) => {
                    let info = rgit_trailer::parse(
                        item.commit.message(),
                        &trailer_conf,
                        &rgit_trailer::ParseOptions::default(),
                    );
                    for t in info.trailers() {
                        if !t.token.eq_ignore_ascii_case(key.as_bytes()) {
                            continue;
                        }
                        let value = rgit_trailer::unfold_value(&t.value);
                        keys.push(match split_ident(&value) {
                            Some((name, mail)) => ident_key(&mailmap, name, mail, s.email),
                            None => value,
                        });
                    }
                }
            }
        }
        let mut seen = HashSet::new();
        for k in keys {
            if seen.insert(k.clone()) {
                collected.add(k, desc.clone());
            }
        }
    }
    Ok(collected.output(s))
}

/// Group the commits of `git log` output: each `Author:` (`Commit:` for
/// committers; `author`/`committer` in the raw format) line names the
/// identity, the line after the next empty line is the description.
fn read_log(input: &[u8], committer: bool, mailmap: &Mailmap, s: &Settings, out: &mut Groups) {
    let (pretty, raw): (&[u8], &[u8]) = if committer {
        (b"Commit: ", b"committer ")
    } else {
        (b"Author: ", b"author ")
    };
    let mut lines = input.split(|&b| b == b'\n').peekable();
    while let Some(line) = lines.next() {
        let ident = if let Some(rest) = line.strip_prefix(pretty) {
            rest
        } else if let Some(rest) = line.strip_prefix(raw) {
            // Without the date after the email.
            match rest.iter().rposition(|&b| b == b'>') {
                Some(i) => &rest[..=i],
                None => rest,
            }
        } else {
            continue;
        };
        // An identity without an email is not counted.
        let Some(lt) = ident.iter().position(|&b| b == b'<') else {
            continue;
        };
        let end = ident[lt..]
            .iter()
            .position(|&b| b == b'>')
            .map_or(ident.len(), |i| lt + i);
        let (name, email) = (ident[..lt].trim_ascii_end(), &ident[lt + 1..end]);
        let key = ident_key(mailmap, name, email, s.email);
        // The description: the line after the empty line that ends the
        // header.
        let mut desc = Vec::new();
        let mut body = false;
        for next in lines.by_ref() {
            if body {
                desc = strip_patch(next.trim_ascii()).to_vec();
                break;
            }
            body = next.is_empty();
        }
        out.add(key, desc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptions() {
        assert_eq!(strip_patch(b"[PATCH 2/3] x"), b"x");
        assert_eq!(strip_patch(b"[PATCHx"), b"[PATCHx");
        assert_eq!(first_paragraph(b"two\n lines\n"), b"two  lines");
        assert_eq!(first_paragraph(b"two\n\nlines"), b"two");
    }

    #[test]
    fn wrap_option() {
        let w = parse_wrap(Some("20,2")).unwrap();
        assert_eq!((w.width, w.indent1, w.indent2), (20, 2, 9));
        assert!(parse_wrap(Some("x")).is_none());
        assert!(parse_wrap(Some("1,2,3,4")).is_none());
    }
}
