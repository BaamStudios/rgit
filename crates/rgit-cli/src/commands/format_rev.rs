// SPDX-License-Identifier: MIT
//! `git format-rev` (Documentation/git-format-rev.adoc, experimental):
//! pretty-format the commits named on standard input.
//!
//! `--stdin-mode=revs` (or `rev`) formats each record as a revision;
//! `--stdin-mode=text` replaces every run of exactly as many lower-case hex
//! digits as a full object name has, when it names a commit, by the
//! formatted commit and leaves everything else as it is. Each record is
//! written (and flushed) as soon as it is read.
//!
//! A commit is formatted like `git log --format=<pretty>` shows it, without
//! the `commit <id>` line of the built-in formats and without the
//! terminator of `tformat:` formats; the record terminator follows it.

use std::collections::HashMap;
use std::io::{BufRead, Write};

use rgit_hash::ObjectId;
use rgit_object::ObjectKind;

use crate::messages::format_rev as msg;
use crate::prelude::*;
use crate::repository::Repo;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Revs,
    Text,
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let format: RefCell<Option<String>> = RefCell::new(None);
    let mode: RefCell<Option<String>> = RefCell::new(None);
    let notes: RefCell<Option<String>> = RefCell::new(None);
    let null_input = Cell::new(false);
    let null_output = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::string(None, "format", &format, "format", msg::H_FORMAT),
            Opt::string(None, "stdin-mode", &mode, "stdin-mode", msg::H_STDIN_MODE),
            Opt::callback(None, "notes", Some("notes"), msg::H_NOTES, |v, neg| {
                *notes.borrow_mut() = if neg { None } else { v.map(str::to_owned) };
                Ok(())
            }),
            Opt::callback('z', "null", None, msg::H_NULL, |_, _| {
                null_input.set(true);
                null_output.set(true);
                Ok(())
            })
            .noarg()
            .noneg(),
            Opt::flag(None, "null-input", &null_input, msg::H_NULL_INPUT),
            Opt::flag(None, "null-output", &null_output, msg::H_NULL_OUTPUT),
        ],
    );
    let args = parser.parse(argv)?.args;
    if !args.is_empty() {
        eprintln!("error: {}", msg::TOO_MANY_ARGUMENTS);
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let Some(format) = format.into_inner() else {
        return Err(Die::Fatal(msg::FORMAT_REQUIRED.into()));
    };
    let mode = match mode.into_inner().as_deref() {
        None => return Err(Die::Fatal(msg::MODE_REQUIRED.into())),
        Some("revs" | "rev") => Mode::Revs,
        Some("text") => Mode::Text,
        Some(_) => return Err(Die::Fatal(msg::BAD_MODE.into())),
    };
    let in_term = if null_input.get() { 0 } else { b'\n' };
    let out_term = if null_output.get() { 0 } else { b'\n' };

    ctx.paginate = Some(false);
    let mut repo = Repo::open(ctx, crate::repo::Mode::Required)?;
    let mut formatter = Formatter {
        format,
        notes: notes.into_inner(),
        cache: HashMap::new(),
    };
    let hex_len = repo.hash.hex_len();
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut stdout = std::io::stdout();
    let mut record = Vec::new();
    loop {
        record.clear();
        match input.read_until(in_term, &mut record) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if record.last() == Some(&in_term) {
            record.pop();
        }
        let mut out = Vec::new();
        match mode {
            Mode::Revs => {
                let rev = String::from_utf8_lossy(&record).into_owned();
                match lookup(&mut repo, &rev) {
                    Ok(id) => {
                        out.extend_from_slice(&formatter.format(ctx, &id)?);
                        out.push(out_term);
                    }
                    Err(m) => {
                        eprintln!("{m}");
                        continue;
                    }
                }
            }
            Mode::Text => {
                let mut i = 0;
                while i < record.len() {
                    let run = record[i..]
                        .iter()
                        .take_while(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                        .count();
                    if run == 0 {
                        out.push(record[i]);
                        i += 1;
                        continue;
                    }
                    let word = &record[i..i + run];
                    let commit = (run == hex_len)
                        .then(|| ObjectId::from_hex(repo.hash, word).ok())
                        .flatten()
                        .filter(|id| repo.object_kind(id) == Some(ObjectKind::Commit));
                    match commit {
                        Some(id) => out.extend_from_slice(&formatter.format(ctx, &id)?),
                        None => out.extend_from_slice(word),
                    }
                    i += run;
                }
                out.push(out_term);
            }
        }
        let mut lock = stdout.lock();
        let _ = lock.write_all(&out);
        let _ = lock.flush();
    }
    let _ = stdout.flush();
    Ok(0)
}

/// The commit `rev` names, or the message for a lookup failure.
fn lookup(repo: &mut Repo, rev: &str) -> Result<ObjectId, String> {
    let id = repo.resolve(rev).ok_or_else(|| msg::no_object_name(rev))?;
    if repo.object_kind(&id).is_none() {
        return Err(msg::no_object(rev));
    }
    repo.peel_to(id, ObjectKind::Commit)
        .ok_or_else(|| msg::no_commit(rev))
}

/// Formats commits with `log`, remembering the results.
struct Formatter {
    format: String,
    notes: Option<String>,
    cache: HashMap<ObjectId, Vec<u8>>,
}

impl Formatter {
    fn format(&mut self, ctx: &Context, id: &ObjectId) -> Result<Vec<u8>, Die> {
        if let Some(done) = self.cache.get(id) {
            return Ok(done.clone());
        }
        let mut args = vec![
            "--no-walk".to_owned(),
            format!("--format={}", self.format),
        ];
        let f = self.format.as_str();
        let builtin = matches!(f, "short" | "medium" | "full" | "fuller" | "raw");
        // Notes only show through `%N` (`--no-notes` goes back to the
        // default notes).
        match &self.notes {
            _ if builtin || f == "oneline" => args.push("--no-notes".to_owned()),
            Some(r) => args.push(format!("--notes={r}")),
            None => {}
        }
        args.push(id.to_string());
        args.push("--".to_owned());
        let mut text = crate::history::log_captured(ctx, &args)?;
        if builtin {
            // Without the `commit <id>` line.
            let end = text.iter().position(|&b| b == b'\n').map_or(0, |i| i + 1);
            text.drain(..end);
        } else if f == "oneline" {
            let end = text.iter().position(|&b| b == b' ').map_or(0, |i| i + 1);
            text.drain(..end);
            text.pop_if(|b| *b == b'\n');
        } else if !f.starts_with("format:") {
            text.pop_if(|b| *b == b'\n');
        }
        self.cache.insert(*id, text.clone());
        Ok(text)
    }
}
