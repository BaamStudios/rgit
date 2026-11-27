// SPDX-License-Identifier: MIT
//! `git notes` (Documentation/git-notes.adoc,
//! Documentation/config/notes.adoc): `list`, `add`, `copy` (`--stdin`,
//! `--for-rewrite`), `append`, `edit`, `show`, `merge` (see
//! [`merge`]), `remove`, `prune` and `get-ref`, with `--ref`,
//! `core.notesRef` and `GIT_NOTES_REF`.
//!
//! Notes trees are read and written with [`rgit_notes`] (fan-out, non-note
//! entries); every change is a commit on the notes ref with the message
//! `Notes added by 'git notes <subcommand>'` (or `removed`), written only
//! when the notes changed.

use crate::argv::CommandArgs as _;
#[path = "notes/merge.rs"]
mod merge;

use std::io::{BufRead, Read, Write};
use std::path::PathBuf;

use rgit_hash::ObjectId;
use rgit_notes::refname;
use rgit_object::ObjectKind;

use crate::messages::notes as msg;
use crate::notes::{self as support, Loaded};
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;

const SUBCOMMANDS: &[&str] = &[
    "list", "add", "copy", "append", "edit", "show", "merge", "remove", "prune", "get-ref",
];

fn fatal(s: impl Into<String>) -> Die {
    Die::Fatal(s.into())
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let notes_ref: RefCell<Option<String>> = RefCell::new(None);
    let mut opts = vec![
        Opt::callback(None, "ref", Some("notes-ref"), msg::HELP_REF, |v, unset| {
            *notes_ref.borrow_mut() = if unset { None } else { v.map(str::to_owned) };
            Ok(())
        }),
    ];
    opts.extend(SUBCOMMANDS.iter().map(|s| Opt::subcommand(s)));
    let mut parser = Parser::new(msg::USAGE, opts).flags(ParseFlags {
        subcommand_optional: true,
        ..Default::default()
    });
    let parsed = parser.parse(argv)?;
    if parsed.subcommand.is_none() {
        if let Some(arg) = parsed.args.first() {
            return Err(parser.error_with_usage(&msg::unknown_subcommand(arg)).into());
        }
    }
    drop(parser);
    let (sub, args) = match parsed.subcommand {
        Some(s) => (s, parsed.args),
        None => ("list".to_owned(), vec!["list".to_owned()]),
    };

    let mut repo = Repo::open(ctx, Mode::Required)?;
    let name = match notes_ref.into_inner() {
        Some(r) => refname::expand(&r),
        None => support::default_ref(&repo.config),
    };
    let mut n = Cmd {
        ctx,
        repo: &mut repo,
        name,
    };
    match sub.as_str() {
        "list" => n.list(&args),
        "add" => n.add(&args),
        "copy" => n.copy(&args),
        "append" | "edit" => n.append_edit(&args),
        "show" => n.show(&args),
        "merge" => merge::run(n.ctx, n.repo, &n.name, &args),
        "remove" => n.remove(&args),
        "prune" => n.prune(&args),
        _ => n.get_ref(&args),
    }
}

/// A piece of the note message in command-line order.
enum Piece {
    Message(String),
    File(String),
    Reuse(String),
    Reedit(String),
}

/// The message options of `add`, `append` and `edit`.
struct MessageOpts {
    pieces: RefCell<Vec<Piece>>,
    edit: Cell<i32>,
    allow_empty: Cell<bool>,
    force: Cell<bool>,
    separator: RefCell<Option<String>>,
    stripspace: Cell<i32>,
}

impl MessageOpts {
    fn new() -> Self {
        MessageOpts {
            pieces: RefCell::new(Vec::new()),
            edit: Cell::new(-1),
            allow_empty: Cell::new(false),
            force: Cell::new(false),
            separator: RefCell::new(Some("\n".to_owned())),
            stripspace: Cell::new(-1),
        }
    }

    /// The options in git's order for `sub`.
    fn options(&self, sub: &str) -> Vec<Opt<'_>> {
        let piece = |f: fn(String) -> Piece| {
            move |v: Option<&str>, _unset: bool| {
                self.pieces.borrow_mut().push(f(v.unwrap_or_default().to_owned()));
                Ok(())
            }
        };
        let m = Opt::callback('m', "message", Some("message"), msg::HELP_MESSAGE, piece(Piece::Message)).noneg();
        let f = Opt::callback('F', "file", Some("file"), msg::HELP_FILE, piece(Piece::File)).noneg();
        let c = Opt::callback('c', "reedit-message", Some("object"), msg::HELP_REEDIT, piece(Piece::Reedit)).noneg();
        let cc = Opt::callback('C', "reuse-message", Some("object"), msg::HELP_REUSE, piece(Piece::Reuse)).noneg();
        let e = Opt::bool('e', "edit", &self.edit, msg::HELP_EDIT);
        let mut v = if sub == "add" { vec![m, f, c, e, cc] } else { vec![m, f, c, cc, e] };
        v.push(Opt::flag(None, "allow-empty", &self.allow_empty, msg::HELP_ALLOW_EMPTY));
        if sub == "add" {
            v.push(Opt::flag('f', "force", &self.force, msg::HELP_FORCE_ADD));
        }
        v.push(
            Opt::callback(None, "separator", Some("paragraph-break"), msg::HELP_SEPARATOR, |val, unset| {
                *self.separator.borrow_mut() = if unset {
                    None
                } else {
                    Some(val.unwrap_or("\n").to_owned())
                };
                Ok(())
            })
            .optional(None),
        );
        v.push(Opt::bool(None, "stripspace", &self.stripspace, msg::HELP_STRIPSPACE));
        v
    }
}

/// The note message being prepared.
struct NoteData {
    /// Message options were given (`-m`, `-F`, `-c`, `-C`).
    given: bool,
    /// The editor is started.
    use_editor: bool,
    buf: Vec<u8>,
    /// The final message is cleaned up with `stripspace`.
    strip: bool,
}

struct Cmd<'a> {
    ctx: &'a Context,
    repo: &'a mut Repo,
    /// The notes ref.
    name: String,
}

impl Cmd<'_> {
    fn check_prefix(&self, verb: &str) -> Result<(), Die> {
        if !self.name.starts_with(refname::PREFIX) {
            return Err(fatal(msg::refusing(verb, &self.name)));
        }
        Ok(())
    }

    fn load(&mut self, verb: &str, writable: bool) -> Result<Loaded, Die> {
        self.check_prefix(verb)?;
        support::load(self.repo, &self.name.clone(), writable)
    }

    /// Resolves an object name (`HEAD` by default).
    fn object(&mut self, name: &str) -> Result<ObjectId, Die> {
        self.repo
            .resolve(name)
            .ok_or_else(|| fatal(msg::failed_to_resolve(name)))
    }

    fn get_ref(&mut self, args: &[String]) -> CmdResult {
        let parser = Parser::new(msg::USAGE_GET_REF, vec![]);
        let mut parser = parser;
        let rest = parser.parse(args)?.args;
        if !rest.is_empty() {
            return Err(parser.error_with_usage(msg::TOO_MANY_ARGUMENTS).into());
        }
        println!("{}", self.name);
        Ok(0)
    }

    fn list(&mut self, args: &[String]) -> CmdResult {
        let mut parser = Parser::new(msg::USAGE_LIST, vec![]);
        let rest = parser.parse(args)?.args;
        if rest.len() > 1 {
            return Err(parser.error_with_usage(msg::TOO_MANY_ARGUMENTS).into());
        }
        drop(parser);
        let object = match rest.first() {
            Some(name) => Some(self.object(name)?),
            None => None,
        };
        let loaded = self.load("list", false)?;
        let mut out = std::io::stdout().lock();
        match object {
            Some(id) => match loaded.notes.get(&id) {
                Some(note) => {
                    let _ = writeln!(out, "{}", note.to_hex());
                    Ok(0)
                }
                None => {
                    eprintln!("error: {}", msg::no_note_found(&id.to_hex()));
                    Ok(1)
                }
            },
            None => {
                for (obj, note) in loaded.notes.list() {
                    let _ = writeln!(out, "{} {}", note.to_hex(), obj.to_hex());
                }
                Ok(0)
            }
        }
    }

    fn show(&mut self, args: &[String]) -> CmdResult {
        let mut parser = Parser::new(msg::USAGE_SHOW, vec![]);
        let rest = parser.parse(args)?.args;
        if rest.len() > 1 {
            return Err(parser.error_with_usage(msg::TOO_MANY_ARGUMENTS).into());
        }
        drop(parser);
        let name = rest.first().map_or("HEAD", String::as_str);
        let id = self.object(name)?;
        let loaded = self.load("show", false)?;
        let Some(note) = loaded.notes.get(&id) else {
            eprintln!("error: {}", msg::no_note_found(&id.to_hex()));
            return Ok(1);
        };
        let data = support::blob(self.repo.odb(), &note)
            .ok_or_else(|| fatal(format!("unable to read {}", note.to_hex())))?;
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(&data);
        let _ = out.flush();
        Ok(0)
    }

    /// Reads the message pieces (`-m`, `-F`, `-c`, `-C`) into one buffer.
    fn collect(&mut self, o: &MessageOpts) -> Result<NoteData, Die> {
        let explicit = match o.stripspace.get() {
            -1 => None,
            v => Some(v == 1),
        };
        let separator = o.separator.borrow().clone().map(|mut s| {
            if !s.ends_with('\n') {
                s.push('\n');
            }
            s
        });
        let mut d = NoteData {
            given: false,
            use_editor: o.edit.get() == 1,
            buf: Vec::new(),
            strip: explicit.unwrap_or(true),
        };
        for piece in o.pieces.borrow().iter() {
            let (text, default_strip) = match piece {
                Piece::Message(m) => (m.as_bytes().to_vec(), true),
                Piece::File(f) => (self.read_file(f)?, true),
                Piece::Reuse(name) => (self.read_note_object(name)?, false),
                Piece::Reedit(name) => {
                    d.use_editor = true;
                    (self.read_note_object(name)?, true)
                }
            };
            let strip = explicit.unwrap_or(default_strip);
            if !d.buf.is_empty() {
                if let Some(sep) = &separator {
                    d.buf.extend_from_slice(sep.as_bytes());
                }
            }
            d.buf.extend_from_slice(&text);
            if strip && !d.buf.is_empty() && !d.buf.ends_with(b"\n") {
                d.buf.push(b'\n');
            }
            d.given = true;
            d.strip = strip;
        }
        if !d.given {
            d.use_editor = true;
        }
        Ok(d)
    }

    fn read_file(&self, f: &str) -> Result<Vec<u8>, Die> {
        let mut data = Vec::new();
        if f == "-" {
            std::io::stdin()
                .read_to_end(&mut data)
                .map_err(|e| fatal(msg::could_not_read(f, &crate::global::strerror(&e))))?;
            return Ok(data);
        }
        let mut path = PathBuf::from(f);
        if path.is_relative() {
            path = self.repo.setup.original_cwd.join(path);
        }
        std::fs::read(&path).map_err(|e| fatal(msg::could_not_read(f, &crate::global::strerror(&e))))
    }

    fn read_note_object(&mut self, name: &str) -> Result<Vec<u8>, Die> {
        let id = self.object(name)?;
        match self.repo.odb().read(&id).ok().flatten() {
            Some(raw) if raw.kind == ObjectKind::Blob => Ok(raw.data),
            _ => Err(fatal(msg::non_blob(name))),
        }
    }

    /// Runs the editor on `initial` and the template for `object`, and
    /// cleans the result up.
    fn edit(&self, object: &ObjectId, initial: &[u8], strip: bool) -> Result<Vec<u8>, Die> {
        let config = &self.repo.config;
        let comment = crate::comment::comment(config)?.or_hash();
        let path = self.repo.setup.cwd.join(&self.repo.setup.git_dir).join(msg::NOTES_EDITMSG);
        let mut text = initial.to_vec();
        if !text.is_empty() && !text.ends_with(b"\n") {
            text.push(b'\n');
        }
        text.push(b'\n');
        text.extend_from_slice(&rgit_commit::message::comment_lines(
            msg::EDITOR_INTRO.as_bytes(),
            &comment,
        ));
        let shown = self.show_object(object);
        let mut block = b"\n".to_vec();
        block.extend_from_slice(&shown);
        text.extend_from_slice(&rgit_commit::message::comment_lines(&block, &comment));
        std::fs::write(&path, &text).map_err(|e| fatal(e.to_string()))?;
        let env = rgit_config::SystemEnv::default();
        let editor = rgit_ident::editor::editor(config, &env).map_err(|p| {
            p.print();
            Die::Exit(rgit_options::EXIT_FATAL)
        })?;
        let advisor = self.ctx.advisor(config)?;
        let opts = rgit_ident::editor::LaunchOptions {
            hint: advisor.enabled(Advice::WaitingForEditor).map_err(Die::Fatal)?,
            env: self.ctx.child_env(),
            cwd: None,
        };
        if let Err(e) = rgit_ident::editor::launch(editor.as_deref(), &path, &opts) {
            e.print();
            return Err(fatal(msg::SUPPLY_CONTENTS));
        }
        let data = std::fs::read(&path).map_err(|e| fatal(e.to_string()))?;
        let _ = std::fs::remove_file(&path);
        Ok(if strip {
            rgit_commit::message::stripspace(&data, Some(&comment))
        } else {
            data
        })
    }

    /// `git show --stat --no-notes <object>` for the editor template.
    fn show_object(&self, object: &ObjectId) -> Vec<u8> {
        let _ = std::io::stdout().flush();
        let Ok(exe) = std::env::current_exe() else {
            return Vec::new();
        };
        std::process::Command::new(exe)
            .raw_args(["show", "--stat", "--no-notes", &object.to_hex()])
            .envs(self.ctx.child_env())
            .env("GIT_PAGER", "cat")
            .current_dir(&self.repo.setup.cwd)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .map(|o| o.stdout)
            .unwrap_or_default()
    }

    /// Finishes the message: the editor when asked for, else `stripspace`.
    fn finish_message(&self, object: &ObjectId, d: &mut NoteData, existing: &[u8]) -> Result<(), Die> {
        if d.use_editor {
            let initial = if d.given { d.buf.clone() } else { existing.to_vec() };
            d.buf = self.edit(object, &initial, d.strip)?;
        } else if d.strip {
            d.buf = rgit_commit::message::stripspace(&d.buf, None);
        }
        Ok(())
    }

    fn write_blob(&self, data: &[u8]) -> Result<ObjectId, Die> {
        self.repo
            .odb()
            .write(ObjectKind::Blob, data)
            .map_err(|e| fatal(e.to_string()))
    }

    fn add(&mut self, args: &[String]) -> CmdResult {
        let o = MessageOpts::new();
        let mut parser = Parser::new(msg::USAGE_ADD, o.options("add"));
        let rest = parser.parse(args)?.args;
        if rest.len() > 1 {
            return Err(parser.error_with_usage(msg::TOO_MANY_ARGUMENTS).into());
        }
        drop(parser);
        let mut d = self.collect(&o)?;
        let name = rest.first().map_or("HEAD", String::as_str);
        let object = self.object(name)?;
        let mut loaded = self.load("add", true)?;
        let existing = loaded.notes.get(&object);
        let hex = object.to_hex();
        let mut old = Vec::new();
        if let Some(note) = existing {
            if !o.force.get() {
                if !d.given {
                    // `add` without a message edits the existing note.
                    let data = support::blob(self.repo.odb(), &note).unwrap_or_default();
                    return self.store(loaded, object, d, &data, "edit", o.allow_empty.get(), None);
                }
                eprintln!("error: {}", msg::cannot_add(&hex));
                return Ok(1);
            }
            eprintln!("{}", msg::overwriting(&hex));
            old = support::blob(self.repo.odb(), &note).unwrap_or_default();
        }
        self.finish_message(&object, &mut d, &old)?;
        if d.buf.is_empty() && !o.allow_empty.get() {
            eprintln!("{}", msg::removing(&hex));
            if loaded.notes.remove(&object).is_some() {
                loaded.dirty = true;
            }
            loaded.commit(self.repo, &msg::removed_by("notes add"))?;
            return Ok(0);
        }
        let blob = self.write_blob(&d.buf)?;
        loaded.notes.set(object, blob);
        loaded.dirty = true;
        loaded.commit(self.repo, &msg::added_by("notes add"))?;
        Ok(0)
    }

    /// Stores the note of an `edit` (also `add` morphed into `edit`):
    /// `existing` is shown in the editor; with `append`, the new message
    /// is appended to it.
    #[allow(clippy::too_many_arguments)]
    fn store(
        &mut self,
        mut loaded: Loaded,
        object: ObjectId,
        mut d: NoteData,
        existing: &[u8],
        sub: &str,
        allow_empty: bool,
        append: Option<(&[u8], Option<String>)>,
    ) -> CmdResult {
        let hex = object.to_hex();
        match append {
            Some((old, separator)) => {
                self.finish_message(&object, &mut d, b"")?;
                if !old.is_empty() || loaded.notes.get(&object).is_some() {
                    if !d.buf.is_empty() {
                        let mut buf = old.to_vec();
                        if let Some(sep) = separator {
                            buf.extend_from_slice(sep.as_bytes());
                        }
                        buf.extend_from_slice(&d.buf);
                        d.buf = buf;
                    } else {
                        d.buf = old.to_vec();
                    }
                }
            }
            None => self.finish_message(&object, &mut d, existing)?,
        }
        if d.buf.is_empty() && !allow_empty {
            eprintln!("{}", msg::removing(&hex));
            if loaded.notes.remove(&object).is_some() {
                loaded.dirty = true;
            }
            loaded.commit(self.repo, &msg::removed_by(&format!("notes {sub}")))?;
            return Ok(0);
        }
        let blob = self.write_blob(&d.buf)?;
        loaded.notes.set(object, blob);
        loaded.dirty = true;
        loaded.commit(self.repo, &msg::added_by(&format!("notes {sub}")))?;
        Ok(0)
    }

    fn append_edit(&mut self, args: &[String]) -> CmdResult {
        let sub = args[0].clone();
        let o = MessageOpts::new();
        let usage = if sub == "edit" { msg::USAGE_EDIT } else { msg::USAGE_APPEND };
        let mut parser = Parser::new(usage, o.options(&sub));
        let rest = parser.parse(args)?.args;
        if rest.len() > 1 {
            return Err(parser.error_with_usage(msg::TOO_MANY_ARGUMENTS).into());
        }
        drop(parser);
        if sub == "edit" && !o.pieces.borrow().is_empty() {
            eprint!("{}", msg::EDIT_DEPRECATED);
        }
        let d = self.collect(&o)?;
        let name = rest.first().map_or("HEAD", String::as_str);
        let object = self.object(name)?;
        let loaded = self.load(&sub, true)?;
        let existing = loaded
            .notes
            .get(&object)
            .and_then(|n| support::blob(self.repo.odb(), &n))
            .unwrap_or_default();
        if sub == "append" {
            let separator = o.separator.borrow().clone().map(|mut s| {
                if !s.ends_with('\n') {
                    s.push('\n');
                }
                s
            });
            self.store(loaded, object, d, b"", "append", o.allow_empty.get(), Some((&existing, separator)))
        } else {
            self.store(loaded, object, d, &existing, "edit", o.allow_empty.get(), None)
        }
    }

    fn copy(&mut self, args: &[String]) -> CmdResult {
        let force = Cell::new(false);
        let from_stdin = Cell::new(false);
        let rewrite: RefCell<Option<String>> = RefCell::new(None);
        let mut parser = Parser::new(
            msg::USAGE_COPY,
            vec![
                Opt::flag('f', "force", &force, msg::HELP_FORCE_ADD),
                Opt::flag(None, "stdin", &from_stdin, msg::HELP_STDIN_COPY),
                Opt::string(None, "for-rewrite", &rewrite, "command", msg::HELP_FOR_REWRITE),
            ],
        );
        let rest = parser.parse(args)?.args;
        let rewrite = rewrite.borrow().clone();
        let stdin_mode = from_stdin.get() || rewrite.is_some();
        if stdin_mode {
            if !rest.is_empty() {
                return Err(parser.error_with_usage(msg::TOO_MANY_ARGUMENTS).into());
            }
            drop(parser);
            return self.copy_stdin(force.get(), rewrite.as_deref());
        }
        if rest.len() > 2 {
            return Err(parser.error_with_usage(msg::TOO_MANY_ARGUMENTS).into());
        }
        if rest.is_empty() {
            return Err(parser.error_with_usage(msg::TOO_FEW_ARGUMENTS).into());
        }
        drop(parser);
        let from = self.object(&rest[0])?;
        let to_name = rest.get(1).map_or("HEAD", String::as_str);
        let to = self.object(to_name)?;
        let mut loaded = self.load("copy", true)?;
        if loaded.notes.get(&to).is_some() {
            if !force.get() {
                eprintln!("error: {}", msg::cannot_copy(&to.to_hex()));
                return Ok(1);
            }
            eprintln!("{}", msg::overwriting(&to.to_hex()));
        }
        let Some(note) = loaded.notes.get(&from) else {
            eprintln!("error: {}", msg::missing_source(&from.to_hex()));
            return Ok(1);
        };
        loaded.notes.set(to, note);
        loaded.dirty = true;
        loaded.commit(self.repo, &msg::added_by("notes copy"))?;
        Ok(0)
    }

    /// `copy --stdin` and `copy --for-rewrite=<command>`.
    fn copy_stdin(&mut self, force: bool, rewrite: Option<&str>) -> CmdResult {
        let mut input = String::new();
        let _ = std::io::stdin().lock().read_to_string(&mut input);
        let mut pairs = Vec::new();
        let mut names = Vec::new();
        for line in input.lines() {
            let mut words = line.splitn(3, ' ');
            let (Some(a), Some(b)) = (words.next(), words.next()) else {
                return Err(fatal(msg::malformed_line(line)));
            };
            let b = b.trim_end();
            let from = self.object(a)?;
            let to = self.object(b)?;
            pairs.push((from, to));
            names.push((a.to_owned(), b.to_owned()));
        }
        if let Some(cmd) = rewrite {
            let Some(mut rw) = support::Rewrite::init(self.repo, cmd)? else {
                return Ok(0);
            };
            for (from, to) in &pairs {
                rw.copy(self.repo.odb(), from, to)?;
            }
            rw.finish(self.repo, &msg::added_by("notes copy"), None)?;
            return Ok(0);
        }
        let mut loaded = self.load("copy", true)?;
        let mut code = 0;
        for ((from, to), (a, b)) in pairs.into_iter().zip(names) {
            if loaded.notes.get(&to).is_some() && !force {
                eprintln!("error: {}", msg::failed_to_copy(&a, &b));
                code = 1;
                continue;
            }
            let Some(note) = loaded.notes.get(&from) else {
                continue;
            };
            loaded.notes.set(to, note);
            loaded.dirty = true;
        }
        if code == 0 {
            loaded.commit(self.repo, &msg::added_by("notes copy"))?;
        }
        Ok(code)
    }

    fn remove(&mut self, args: &[String]) -> CmdResult {
        let ignore_missing = Cell::new(false);
        let from_stdin = Cell::new(false);
        let mut parser = Parser::new(
            msg::USAGE_REMOVE,
            vec![
                Opt::flag(None, "ignore-missing", &ignore_missing, msg::HELP_IGNORE_MISSING),
                Opt::flag(None, "stdin", &from_stdin, msg::HELP_STDIN_REMOVE),
            ],
        );
        let mut names = parser.parse(args)?.args;
        drop(parser);
        let mut loaded = self.load("remove", true)?;
        if names.is_empty() && !from_stdin.get() {
            names.push("HEAD".to_owned());
        }
        if from_stdin.get() {
            for line in std::io::stdin().lock().lines().map_while(Result::ok) {
                let line = line.trim_end().to_owned();
                if !line.is_empty() {
                    names.push(line);
                }
            }
        }
        let mut code = 0;
        for name in &names {
            let Some(id) = self.repo.resolve(name) else {
                eprintln!("error: {}", msg::failed_to_resolve_cap(name));
                code = 1;
                continue;
            };
            if loaded.notes.remove(&id).is_some() {
                eprintln!("{}", msg::removing(name));
                loaded.dirty = true;
            } else {
                eprintln!("{}", msg::has_no_note(name));
                if !ignore_missing.get() {
                    code = 1;
                }
            }
        }
        if code == 0 {
            loaded.commit(self.repo, &msg::removed_by("notes remove"))?;
        }
        Ok(code)
    }

    fn prune(&mut self, args: &[String]) -> CmdResult {
        let dry_run = Cell::new(0);
        let verbose = Cell::new(0);
        let mut parser = Parser::new(
            msg::USAGE_PRUNE,
            vec![
                Opt::dry_run(&dry_run, msg::HELP_DRY_RUN),
                Opt::bool('v', "verbose", &verbose, msg::HELP_PRUNE_VERBOSE),
            ],
        );
        let rest = parser.parse(args)?.args;
        if !rest.is_empty() {
            return Err(parser.error_with_usage(msg::TOO_MANY_ARGUMENTS).into());
        }
        drop(parser);
        let mut loaded = self.load("prune", true)?;
        let dry = dry_run.get() == 1;
        let mut out = std::io::stdout().lock();
        for (obj, _) in loaded.notes.list() {
            if self.repo.odb().contains(&obj) {
                continue;
            }
            if dry || verbose.get() == 1 {
                let _ = writeln!(out, "{}", obj.to_hex());
            }
            if !dry {
                loaded.notes.remove(&obj);
                loaded.dirty = true;
            }
        }
        let _ = out.flush();
        if !dry {
            loaded.commit(self.repo, &msg::removed_by("notes prune"))?;
        }
        Ok(0)
    }
}
