// SPDX-License-Identifier: MIT
//! `git update-ref` (Documentation/git-update-ref.adoc): update, create,
//! delete and verify references from the command line or from `--stdin`
//! (text or `-z` format), with transactions (`start`, `prepare`, `commit`,
//! `abort`).
//!
//! Not yet: `--batch-updates` (needs per-update rejection in `rgit-refs`).

use std::io::{BufRead, Write};

use rgit_hash::ObjectId;
use rgit_refs::files::FilesStore;
use rgit_refs::name::{RefNameFlags, is_valid_refname};
use rgit_refs::{OldValue, RefUpdate, Transaction, TransactionState};

use crate::messages::update_ref as msg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, UpdateHooks};
use crate::repo::{self, Mode};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let message: RefCell<Option<String>> = RefCell::new(None);
    let delete = Cell::new(false);
    let no_deref = Cell::new(false);
    let nul = Cell::new(false);
    let stdin = Cell::new(false);
    let create_reflog = Cell::new(false);
    let batch = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::string('m', None, &message, "reason", msg::HELP_MESSAGE),
            Opt::flag('d', None, &delete, msg::HELP_DELETE).noneg(),
            Opt::callback(None, "no-deref", None, msg::HELP_NO_DEREF, |_, _| {
                no_deref.set(true);
                Ok(())
            })
            .noarg()
            .noneg(),
            Opt::callback(None, "deref", None, msg::HELP_DEREF, |_, _| {
                no_deref.set(false);
                Ok(())
            })
            .noarg()
            .noneg(),
            Opt::flag('z', None, &nul, msg::HELP_Z).noneg(),
            Opt::flag(None, "stdin", &stdin, msg::HELP_STDIN),
            Opt::flag(None, "create-reflog", &create_reflog, msg::HELP_CREATE_REFLOG),
            Opt::flag('0', "batch-updates", &batch, msg::HELP_BATCH),
        ],
    );
    let args = parser.parse(argv)?.args;
    let message = message.borrow().clone();
    if message.as_deref() == Some("") {
        return Err(Die::Fatal(msg::EMPTY_MESSAGE.to_owned()));
    }
    let message = message.unwrap_or_default();

    if stdin.get() {
        if delete.get() || !args.is_empty() {
            return Err(parser.usage_error().into());
        }
    } else if nul.get() || batch.get() {
        return Err(parser.usage_error().into());
    } else if delete.get() {
        if args.is_empty() || args.len() > 2 {
            return Err(parser.usage_error().into());
        }
    } else if args.len() < 2 || args.len() > 3 {
        return Err(parser.usage_error().into());
    }
    if batch.get() {
        return Err(Die::Fatal(msg::BATCH_NOT_SUPPORTED.to_owned()));
    }

    let setup = repo::setup(ctx, Mode::Required)?;
    let config = refs_support::load_config(ctx, &setup)?;
    let store = refs_support::ref_store(&setup, &config);
    let objects = Objects::open(&setup);
    let revs = refs_support::Revs::new(ctx, &setup)?;
    let mut hooks = UpdateHooks::new(&setup, &config, &objects);

    let opts = Options {
        message,
        create_reflog: create_reflog.get(),
    };
    if stdin.get() {
        let input = std::io::stdin();
        let mut session = Session {
            store: &store,
            revs: &revs,
            objects: &objects,
            opts: &opts,
            z: nul.get(),
            no_deref: no_deref.get(),
            next_no_deref: false,
            input: input.lock(),
            symlinks: matches!(config.bool("core.prefersymlinkrefs"), Ok(Some(true))),
            symrefs: Vec::new(),
        };
        return session.run(&mut hooks);
    }

    let name = &args[0];
    let hash = setup.format.object_format;
    if delete.get() {
        let old = match args.get(1) {
            None => None,
            Some(s) if s.is_empty() => None,
            Some(s) => Some(
                revs.resolve(s)
                    .ok_or_else(|| Die::Fatal(msg::not_valid_old_sha1(s)))?,
            ),
        };
        let update = RefUpdate::delete(name.as_str(), hash, old)
            .no_deref(no_deref.get())
            .message(opts.message.as_str());
        let mut tx = Transaction::new(&store);
        return tx
            .push(update)
            .and_then(|()| tx.commit(&mut hooks))
            .map(|()| 0)
            .map_err(|e| Die::Error(e.to_string(), 1));
    }
    let new = revs.resolve(&args[1])
        .ok_or_else(|| Die::Fatal(msg::not_valid_sha1(&args[1])))?;
    let old = match args.get(2) {
        None => None,
        Some(s) if s.is_empty() => Some(ObjectId::null(hash)),
        Some(s) => Some(
            revs.resolve(s)
                .ok_or_else(|| Die::Fatal(msg::not_valid_old_sha1(s)))?,
        ),
    };
    let update = RefUpdate::update(name.as_str(), new, old)
        .no_deref(no_deref.get())
        .message(opts.message.as_str())
        .create_reflog(opts.create_reflog);
    let mut tx = Transaction::new(&store);
    tx.push(update)
        .and_then(|()| tx.commit(&mut hooks))
        .map(|()| 0)
        .map_err(|e| Die::Fatal(msg::update_ref_failed(name, &e.to_string())))
}

struct Options {
    message: String,
    create_reflog: bool,
}

/// A parsed value: present (maybe empty), missing, or end of input.
enum Field {
    Value(String),
    Missing,
    Eof,
}

/// Where the session's transaction stands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Collecting updates; committed at the end of input.
    Implicit,
    /// After `start`; aborted at the end of input.
    Started,
    /// After `prepare`.
    Prepared,
    /// After `commit` or `abort`.
    Closed,
}

struct Session<'a, R: BufRead> {
    store: &'a FilesStore,
    revs: &'a refs_support::Revs,
    objects: &'a Objects,
    opts: &'a Options,
    z: bool,
    no_deref: bool,
    next_no_deref: bool,
    input: R,
    /// `core.preferSymlinkRefs`: write symbolic references as symlinks.
    symlinks: bool,
    /// Symbolic references written by the open transaction.
    symrefs: Vec<(String, String)>,
}

fn fatal(m: String) -> Die {
    Die::Fatal(m)
}

impl<R: BufRead> Session<'_, R> {
    fn run(&mut self, hooks: &mut UpdateHooks<'_>) -> CmdResult {
        let store = self.store;
        let mut tx = Transaction::new(store);
        let mut phase = Phase::Implicit;
        let mut out = std::io::stdout();
        loop {
            let term = if self.z { 0 } else { b'\n' };
            let mut raw = Vec::new();
            let n = self
                .input
                .read_until(term, &mut raw)
                .map_err(|e| Die::Fatal(e.to_string()))?;
            if n == 0 {
                break;
            }
            // `line` keeps the newline in text mode (git prints it in
            // messages); in -z mode the NUL is dropped.
            let line = if self.z {
                let l = raw.strip_suffix(b"\0").unwrap_or(&raw);
                String::from_utf8_lossy(l).into_owned()
            } else {
                String::from_utf8_lossy(&raw).into_owned()
            };
            if (self.z && raw == b"\0") || (!self.z && line == "\n") || line.is_empty() {
                return Err(fatal(msg::EMPTY_COMMAND.to_owned()));
            }
            if line.starts_with(|c: char| c.is_ascii_whitespace()) {
                return Err(fatal(msg::whitespace_before_command(&line)));
            }
            let body = if self.z {
                line.as_str()
            } else {
                line.strip_suffix('\n').unwrap_or(&line)
            };
            let (cmd, rest) = match body.split_once(' ') {
                Some((c, _)) => (c, &line[c.len() + 1..]),
                None => (body, ""),
            };
            let has_args = body.contains(' ');
            let control = matches!(cmd, "start" | "prepare" | "commit" | "abort");
            let with_args = matches!(
                cmd,
                "update"
                    | "create"
                    | "delete"
                    | "verify"
                    | "symref-update"
                    | "symref-create"
                    | "symref-delete"
                    | "symref-verify"
                    | "option"
            );
            if !(control && !has_args) && !(with_args && has_args) {
                return Err(fatal(msg::unknown_command(&line)));
            }
            if phase == Phase::Prepared && !matches!(cmd, "commit" | "abort") {
                return Err(fatal(msg::PREPARED_ONLY_CLOSE.to_owned()));
            }
            if phase == Phase::Closed && !matches!(cmd, "start") {
                return Err(fatal(msg::TRANSACTION_CLOSED.to_owned()));
            }
            match cmd {
                "start" => {
                    if phase == Phase::Started {
                        return Err(fatal(msg::CANNOT_RESTART.to_owned()));
                    }
                    if phase == Phase::Closed {
                        tx = Transaction::new(store);
                    }
                    phase = Phase::Started;
                    let _ = writeln!(out, "{}", msg::ok("start"));
                    let _ = out.flush();
                }
                "prepare" => {
                    tx.prepare(hooks).map_err(|e| fatal(e.to_string()))?;
                    phase = Phase::Prepared;
                    let _ = writeln!(out, "{}", msg::ok("prepare"));
                    let _ = out.flush();
                }
                "commit" => {
                    let t = std::mem::replace(&mut tx, Transaction::new(store));
                    t.commit(hooks).map_err(|e| fatal(e.to_string()))?;
                    self.write_symlinks();
                    phase = Phase::Closed;
                    let _ = writeln!(out, "{}", msg::ok("commit"));
                    let _ = out.flush();
                }
                "abort" => {
                    let t = std::mem::replace(&mut tx, Transaction::new(store));
                    t.abort(hooks);
                    self.symrefs.clear();
                    phase = Phase::Closed;
                    let _ = writeln!(out, "{}", msg::ok("abort"));
                    let _ = out.flush();
                }
                "option" => {
                    let opt = if self.z {
                        rest
                    } else {
                        rest.strip_suffix('\n').unwrap_or(rest)
                    };
                    if opt == "no-deref" {
                        self.next_no_deref = true;
                    } else {
                        return Err(fatal(msg::option_unknown(rest)));
                    }
                }
                _ => {
                    let update = self.parse_update(cmd, rest)?;
                    if let Some(rgit_refs::NewValue::Symbolic(t)) = &update.new {
                        hooks.mark_symbolic(&update.name);
                        if update.no_deref {
                            self.symrefs.push((update.name.clone(), t.clone()));
                        }
                    }
                    self.next_no_deref = false;
                    tx.push(update).map_err(|e| fatal(e.to_string()))?;
                }
            }
        }
        match phase {
            Phase::Implicit => {
                if tx.state() == TransactionState::Open {
                    tx.commit(hooks).map_err(|e| fatal(e.to_string()))?;
                    self.write_symlinks();
                }
            }
            Phase::Started | Phase::Prepared => tx.abort(hooks),
            Phase::Closed => {}
        }
        Ok(0)
    }

    /// With `core.preferSymlinkRefs`, replaces the symbolic references just
    /// written by symbolic links (deprecated in git, which warns).
    fn write_symlinks(&mut self) {
        let symrefs = std::mem::take(&mut self.symrefs);
        if !self.symlinks || symrefs.is_empty() {
            return;
        }
        eprint!("{}", msg::PREFER_SYMLINK_REFS_WARNING);
        #[cfg(unix)]
        for (name, target) in symrefs {
            let path = self.store.ref_path(&name);
            let expected = format!("ref: {target}\n");
            if std::fs::read(&path).is_ok_and(|c| c == expected.as_bytes()) {
                let tmp = path.with_extension("symlink.tmp");
                if std::os::unix::fs::symlink(&target, &tmp).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
            }
        }
    }

    /// Parses one update command; `rest` follows `<cmd> `.
    fn parse_update(&mut self, cmd: &str, rest: &str) -> Result<RefUpdate, Die> {
        let symref_noderef = matches!(cmd, "symref-delete" | "symref-verify");
        let no_deref = self.no_deref || self.next_no_deref;
        if symref_noderef && !no_deref {
            return Err(fatal(msg::deref_mode(cmd)));
        }
        let (name, mut rest) = self.parse_refname(cmd, rest)?;
        let hash = self.objects.hash();
        let null = ObjectId::null(hash);
        let update = match cmd {
            "update" => {
                let new = match self.next_field(&mut rest)? {
                    Field::Value(v) => self.oid(cmd, &name, "<new-oid>", &v)?,
                    Field::Missing if self.z => {
                        eprintln!(
                            "warning: {}",
                            msg::missing_treat_zero(cmd, &name, "<new-oid>")
                        );
                        null
                    }
                    Field::Missing => {
                        return Err(fatal(msg::missing_field(cmd, &name, "<new-oid>")));
                    }
                    Field::Eof => {
                        return Err(fatal(msg::unexpected_eof(cmd, &name, "<new-oid>")));
                    }
                };
                let old = match self.next_field(&mut rest)? {
                    Field::Value(v) => Some(self.oid(cmd, &name, "<old-oid>", &v)?),
                    Field::Missing => None,
                    Field::Eof => {
                        return Err(fatal(msg::unexpected_eof(cmd, &name, "<old-oid>")));
                    }
                };
                RefUpdate::update(name.as_str(), new, old)
            }
            "create" => {
                let new = match self.next_field(&mut rest)? {
                    Field::Value(v) => self.oid(cmd, &name, "<new-oid>", &v)?,
                    Field::Missing => {
                        return Err(fatal(msg::missing_field(cmd, &name, "<new-oid>")));
                    }
                    Field::Eof => {
                        return Err(fatal(msg::unexpected_eof(cmd, &name, "<new-oid>")));
                    }
                };
                if new.is_null() {
                    return Err(fatal(msg::zero_field(cmd, &name, "<new-oid>")));
                }
                RefUpdate::create(name.as_str(), new)
            }
            "delete" => {
                let old = match self.next_field(&mut rest)? {
                    Field::Value(v) => {
                        let o = self.oid(cmd, &name, "<old-oid>", &v)?;
                        if o.is_null() {
                            return Err(fatal(msg::zero_field(cmd, &name, "<old-oid>")));
                        }
                        Some(o)
                    }
                    Field::Missing => None,
                    Field::Eof => {
                        return Err(fatal(msg::unexpected_eof(cmd, &name, "<old-oid>")));
                    }
                };
                RefUpdate::delete(name.as_str(), hash, old)
            }
            "verify" => {
                let old = match self.next_field(&mut rest)? {
                    Field::Value(v) => self.oid(cmd, &name, "<old-oid>", &v)?,
                    Field::Missing => null,
                    Field::Eof => {
                        return Err(fatal(msg::unexpected_eof(cmd, &name, "<old-oid>")));
                    }
                };
                RefUpdate::verify(name.as_str(), old)
            }
            "symref-update" => {
                let target = self.required_target(cmd, &name, &mut rest, "<new-target>")?;
                let old = match self.next_field(&mut rest)? {
                    Field::Missing | Field::Eof => None,
                    Field::Value(kind) if kind == "ref" => {
                        let t = self.required_target(cmd, &name, &mut rest, "<old-target>")?;
                        Some(OldValue::Symbolic(t))
                    }
                    Field::Value(kind) if kind == "oid" => match self.next_field(&mut rest)? {
                        Field::Value(v) => {
                            let o = self.parse_oid(&v).ok_or_else(|| {
                                fatal(msg::invalid_oid(cmd, &name, &v))
                            })?;
                            Some(OldValue::Object(o))
                        }
                        Field::Missing => {
                            return Err(fatal(msg::missing_field(cmd, &name, "<old-oid>")));
                        }
                        Field::Eof => {
                            return Err(fatal(msg::unexpected_eof(cmd, &name, "<old-oid>")));
                        }
                    },
                    Field::Value(other) => {
                        return Err(fatal(msg::invalid_old_arg(cmd, &name, &other)));
                    }
                };
                RefUpdate::symref_update(name.as_str(), target, old)
            }
            "symref-create" => {
                let target = self.required_target(cmd, &name, &mut rest, "<new-target>")?;
                RefUpdate::symref_create(name.as_str(), target)
            }
            "symref-delete" => {
                let old = self.optional_target(cmd, &name, &mut rest, "<old-target>")?;
                RefUpdate::symref_delete(name.as_str(), hash, old)
            }
            "symref-verify" => {
                let old = self.optional_target(cmd, &name, &mut rest, "<old-target>")?;
                RefUpdate::symref_verify(name.as_str(), old)
            }
            _ => unreachable!("checked by the caller"),
        };
        if !self.z && rest != "\n" && !rest.is_empty() {
            return Err(fatal(msg::extra_input(cmd, &name, rest)));
        }
        Ok(update
            .no_deref(no_deref)
            .message(self.opts.message.as_str())
            .create_reflog(self.opts.create_reflog))
    }

    fn required_target(
        &mut self,
        cmd: &str,
        name: &str,
        rest: &mut &str,
        field: &str,
    ) -> Result<String, Die> {
        match self.next_field(rest)? {
            Field::Value(v) if !v.is_empty() => Ok(v),
            Field::Value(_) | Field::Missing => Err(fatal(msg::missing_field(cmd, name, field))),
            Field::Eof => Err(fatal(msg::unexpected_eof(cmd, name, field))),
        }
    }

    fn optional_target(
        &mut self,
        cmd: &str,
        name: &str,
        rest: &mut &str,
        field: &str,
    ) -> Result<Option<String>, Die> {
        let _ = (cmd, name, field);
        match self.next_field(rest)? {
            Field::Value(v) if !v.is_empty() => Ok(Some(v)),
            Field::Value(_) | Field::Missing | Field::Eof => Ok(None),
        }
    }

    /// An object name of a field; the empty string and the null id are
    /// zero.
    fn oid(&self, cmd: &str, name: &str, field: &str, value: &str) -> Result<ObjectId, Die> {
        if value.is_empty() {
            return Ok(ObjectId::null(self.objects.hash()));
        }
        self.parse_oid(value)
            .ok_or_else(|| fatal(msg::invalid_field(cmd, name, field, value)))
    }

    fn parse_oid(&self, value: &str) -> Option<ObjectId> {
        self.revs.resolve(value)
    }

    /// The reference name at the start of `rest`.
    fn parse_refname<'r>(&self, cmd: &str, rest: &'r str) -> Result<(String, &'r str), Die> {
        let (name, after) = if self.z {
            (rest.to_owned(), "")
        } else {
            parse_arg(rest)?
        };
        if name.is_empty() {
            return Err(fatal(msg::missing_ref(cmd)));
        }
        if !is_valid_refname(name.as_bytes(), RefNameFlags::ONELEVEL) {
            return Err(fatal(msg::invalid_ref_format(cmd, &name)));
        }
        Ok((name, after))
    }

    /// The next value: in text mode from `rest` (` <arg>`), in -z mode the
    /// next NUL-terminated field of the input.
    fn next_field(&mut self, rest: &mut &str) -> Result<Field, Die> {
        if self.z {
            let mut buf = Vec::new();
            let n = self
                .input
                .read_until(0, &mut buf)
                .map_err(|e| Die::Fatal(e.to_string()))?;
            if n == 0 {
                return Ok(Field::Eof);
            }
            if buf.last() == Some(&0) {
                buf.pop();
            }
            if buf.is_empty() {
                return Ok(Field::Missing);
            }
            return Ok(Field::Value(String::from_utf8_lossy(&buf).into_owned()));
        }
        if let Some(r) = rest.strip_prefix(' ') {
            let (v, after) = parse_arg(r)?;
            *rest = after;
            return Ok(Field::Value(v));
        }
        if rest.starts_with('\n') {
            return Ok(Field::Missing);
        }
        Ok(Field::Eof)
    }
}

/// One text-mode argument: up to the next space or newline, or a C-quoted
/// string.
fn parse_arg(s: &str) -> Result<(String, &str), Die> {
    if s.starts_with('"') {
        let (value, used) = unquote(s).ok_or_else(|| fatal(msg::badly_quoted(s)))?;
        let after = &s[used..];
        if !(after.is_empty() || after.starts_with(' ') || after.starts_with('\n')) {
            return Err(fatal(msg::junk_after_quoted(s)));
        }
        return Ok((value, after));
    }
    let end = s.find([' ', '\n']).unwrap_or(s.len());
    Ok((s[..end].to_owned(), &s[end..]))
}

/// C-style unquoting of `s` (starting with `"`): the value and the number
/// of bytes used, including both quotes.
fn unquote(s: &str) -> Option<(String, usize)> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 1;
    while i < b.len() {
        match b[i] {
            b'"' => return Some((String::from_utf8_lossy(&out).into_owned(), i + 1)),
            b'\n' => return None,
            b'\\' => {
                i += 1;
                let c = *b.get(i)?;
                let v = match c {
                    b'a' => 0x07,
                    b'b' => 0x08,
                    b'f' => 0x0c,
                    b'n' => b'\n',
                    b'r' => b'\r',
                    b't' => b'\t',
                    b'v' => 0x0b,
                    b'\\' | b'"' => c,
                    b'0'..=b'3' => {
                        let d1 = *b.get(i + 1)?;
                        let d2 = *b.get(i + 2)?;
                        if !(b'0'..=b'7').contains(&d1) || !(b'0'..=b'7').contains(&d2) {
                            return None;
                        }
                        i += 2;
                        ((c - b'0') << 6) | ((d1 - b'0') << 3) | (d2 - b'0')
                    }
                    _ => return None,
                };
                out.push(v);
            }
            c => out.push(c),
        }
        i += 1;
    }
    None
}
