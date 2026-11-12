// SPDX-License-Identifier: MIT
//! `git cat-file` (Documentation/git-cat-file.adoc).
//!
//! Not implemented yet: `--textconv`, `--filters` (#112, #147),
//! `--use-mailmap` (#127) and `--filter=` (#24); they are parsed and
//! checked like git, then refused.

use std::io::{BufRead, BufWriter, Write};

use rgit_hash::ObjectId;
use rgit_object::{ObjectKind, TreeIter};

use crate::messages::cat_file as msg;
use crate::objects::Repo;
use crate::prelude::*;
use crate::repo::{self, Mode};
use crate::revision::{Failure, Resolved, Resolver, SymlinkEnd};

const DEFAULT_FORMAT: &str = "%(objectname) %(objecttype) %(objectsize)";

/// `--batch`, `--batch-check` or `--batch-command`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Batch {
    Contents,
    Check,
    Command,
}

const MODE_E: i32 = b'e' as i32;
const MODE_P: i32 = b'p' as i32;
const MODE_T: i32 = b't' as i32;
const MODE_S: i32 = b's' as i32;
const MODE_TEXTCONV: i32 = b'c' as i32;
const MODE_FILTERS: i32 = b'w' as i32;
const MODE_ALL: i32 = b'b' as i32;

fn mode_name(mode: i32) -> &'static str {
    match mode {
        MODE_E => "-e",
        MODE_P => "-p",
        MODE_T => "-t",
        MODE_S => "-s",
        MODE_TEXTCONV => "--textconv",
        MODE_FILTERS => "--filters",
        _ => "",
    }
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mode = Cell::new(0);
    let mailmap = Cell::new(false);
    let unknown_type = Cell::new(false);
    let batch: RefCell<Option<(Batch, Option<String>)>> = RefCell::new(None);
    let nul_both = Cell::new(false);
    let nul_in = Cell::new(false);
    let buffer = Cell::new(false);
    let follow = Cell::new(false);
    let unordered = Cell::new(false);
    let path = RefCell::new(None);
    let filter = RefCell::new(None::<String>);
    let batch_cb = |which: Batch| {
        let batch = &batch;
        move |arg: Option<&str>, _unset: bool| -> Result<(), CallbackError> {
            let mut b = batch.borrow_mut();
            if b.is_some() {
                return Err(CallbackError::Error(msg::ONLY_ONE_BATCH.into()));
            }
            *b = Some((which, arg.map(str::to_string)));
            Ok(())
        }
    };
    let mut parser = Parser::new(
        msg::CAT_FILE_USAGE,
        vec![
            Opt::group(msg::CAT_FILE_GROUP_CHECK),
            Opt::cmdmode('e', None, &mode, msg::CAT_FILE_HELP_E, MODE_E),
            Opt::cmdmode('p', None, &mode, msg::CAT_FILE_HELP_P, MODE_P),
            Opt::group(msg::CAT_FILE_GROUP_ATTRS),
            Opt::cmdmode('t', None, &mode, msg::CAT_FILE_HELP_T, MODE_T),
            Opt::cmdmode('s', None, &mode, msg::CAT_FILE_HELP_S, MODE_S),
            Opt::flag(None, "allow-unknown-type", &unknown_type, msg::CAT_FILE_HELP_UNKNOWN_TYPE)
                .hidden(),
            Opt::flag(None, "use-mailmap", &mailmap, msg::CAT_FILE_HELP_MAILMAP),
            Opt::alias(None, "mailmap", "use-mailmap"),
            Opt::group(msg::CAT_FILE_GROUP_BATCH),
            Opt::callback(
                None,
                "batch",
                Some("format"),
                msg::CAT_FILE_HELP_BATCH,
                batch_cb(Batch::Contents),
            )
            .optional(None)
            .noneg(),
            Opt::callback(
                None,
                "batch-check",
                Some("format"),
                msg::CAT_FILE_HELP_BATCH_CHECK,
                batch_cb(Batch::Check),
            )
            .optional(None)
            .noneg(),
            Opt::flag('z', None, &nul_in, msg::CAT_FILE_HELP_Z_LOWER).hidden(),
            Opt::flag('Z', None, &nul_both, msg::CAT_FILE_HELP_Z_UPPER),
            Opt::callback(
                None,
                "batch-command",
                Some("format"),
                msg::CAT_FILE_HELP_BATCH_COMMAND,
                batch_cb(Batch::Command),
            )
            .optional(None)
            .noneg(),
            Opt::cmdmode(None, "batch-all-objects", &mode, msg::CAT_FILE_HELP_ALL, MODE_ALL),
            Opt::group(msg::CAT_FILE_GROUP_OPTIMIZE),
            Opt::flag(None, "buffer", &buffer, msg::CAT_FILE_HELP_BUFFER),
            Opt::flag(None, "follow-symlinks", &follow, msg::CAT_FILE_HELP_FOLLOW),
            Opt::flag(None, "unordered", &unordered, msg::CAT_FILE_HELP_UNORDERED),
            Opt::group(msg::CAT_FILE_GROUP_FILTER),
            Opt::cmdmode(None, "textconv", &mode, msg::CAT_FILE_HELP_TEXTCONV, MODE_TEXTCONV),
            Opt::cmdmode(None, "filters", &mode, msg::CAT_FILE_HELP_FILTERS, MODE_FILTERS),
            Opt::string(None, "path", &path, "blob|tree", msg::CAT_FILE_HELP_PATH).literal_argh(),
            Opt::string(None, "filter", &filter, "args", msg::CAT_FILE_HELP_FILTER),
        ],
    );
    let args = parser.parse(argv)?.args;

    let mut opt = mode.get();
    let all = opt == MODE_ALL;
    if all {
        opt = 0;
    }
    let batch = batch.borrow().clone();
    let path = path.borrow().clone();

    // Option combinations, checked in git's order.
    let filtering = opt == MODE_TEXTCONV || opt == MODE_FILTERS;
    if path.is_some() && !filtering {
        return Err(parser.usage_msg(msg::PATH_NEEDS_FILTERS).into());
    }
    if batch.is_none() {
        for (set, name) in [
            (follow.get(), "--follow-symlinks"),
            (buffer.get(), "--buffer"),
            (all, "--batch-all-objects"),
            (nul_in.get(), "-z"),
            (nul_both.get(), "-Z"),
        ] {
            if set {
                return Err(parser.usage_msg(&msg::requires_batch(name)).into());
            }
        }
    }
    if batch.is_some() {
        if opt != 0 && !filtering {
            return Err(parser
                .usage_msg(&msg::incompatible_with_batch(mode_name(opt)))
                .into());
        }
        if !args.is_empty() {
            return Err(parser.usage_msg(msg::BATCH_NO_ARGS).into());
        }
    } else if filtering {
        if args.len() > 2 {
            return Err(parser.usage_msg(msg::TOO_MANY_ARGS).into());
        }
        if args.is_empty() {
            return Err(parser
                .usage_msg(&msg::object_required("<rev>", mode_name(opt)))
                .into());
        }
    } else if opt == 0 {
        if args.is_empty() {
            return Err(parser.usage_error().into());
        }
        if args.len() != 2 {
            return Err(parser.usage_msg(&msg::only_two_args(args.len())).into());
        }
    } else {
        if args.len() > 1 {
            return Err(parser.usage_msg(msg::TOO_MANY_ARGS).into());
        }
        if args.is_empty() {
            return Err(parser
                .usage_msg(&msg::object_required("<object>", mode_name(opt)))
                .into());
        }
    }
    drop(parser);

    if opt == MODE_TEXTCONV || opt == MODE_FILTERS {
        return Err(Die::Fatal(msg::cat_file_not_supported(mode_name(opt))));
    }
    let use_mailmap = mailmap.get();
    if filter.borrow().is_some() {
        return Err(Die::Fatal(msg::cat_file_not_supported("--filter")));
    }

    let setup = repo::setup(ctx, Mode::Required)?;
    let mut repo = Repo::open(ctx, setup)?;
    if use_mailmap && mailmap_present(&mut repo) {
        return Err(Die::Fatal(msg::cat_file_not_supported("--use-mailmap")));
    }

    if let Some((kind, format)) = batch {
        let fmt = Format::parse(format.as_deref().unwrap_or(DEFAULT_FORMAT))?;
        let nul_out = nul_both.get();
        let opts = BatchOptions {
            kind,
            fmt,
            input_nul: nul_in.get() || nul_out,
            output_nul: nul_out,
            buffer: buffer.get(),
            follow: follow.get(),
        };
        return if all {
            batch_all(&mut repo, &opts)
        } else {
            batch_stdin(&mut repo, &opts)
        };
    }

    if opt == 0 {
        return show_typed(&mut repo, &args[0], &args[1]);
    }
    single(&mut repo, opt, &args[0])
}

/// Whether a mailmap would change anything: `mailmap.file`,
/// `mailmap.blob`, `.mailmap` in the work tree, or `HEAD:.mailmap` in a
/// bare repository.
fn mailmap_present(repo: &mut Repo) -> bool {
    let configured = |k: &str| repo.config.get(k).is_some();
    if configured("mailmap.file") || configured("mailmap.blob") {
        return true;
    }
    match repo.setup.work_tree.clone() {
        Some(wt) => wt.join(".mailmap").exists(),
        None => Resolver::new(repo).resolve("HEAD:.mailmap").is_ok(),
    }
}

/// Print the lines of a store error as git does.
fn report(e: &rgit_odb::Error) {
    for line in e.git_lines() {
        eprintln!("error: {line}");
    }
}

fn is_invalid_type(e: &rgit_odb::Error) -> bool {
    matches!(e, rgit_odb::Error::InvalidType { .. })
}

/// Resolve a name given on the command line; dies like git.
fn resolve_arg(repo: &mut Repo, name: &str) -> Result<Resolved, Die> {
    match Resolver::new(repo).resolve(name) {
        Ok(r) => Ok(r),
        Err(Failure::Missing {
            diagnosis: Some(d),
        }) => Err(Die::Fatal(d)),
        Err(Failure::Fatal(m)) => Err(Die::Fatal(m)),
        Err(_) => Err(Die::Fatal(msg::not_valid_object_name(name))),
    }
}

/// `-e`, `-p`, `-t`, `-s`.
fn single(repo: &mut Repo, opt: i32, name: &str) -> CmdResult {
    let r = resolve_arg(repo, name)?;
    let oid = repo.replaced(&r.oid);
    if opt == MODE_E {
        return Ok(if repo.odb.contains(&oid) { 0 } else { 1 });
    }
    let header = match repo.odb.header(&oid) {
        Ok(Some(h)) => h,
        Ok(None) if opt == MODE_P => return Err(Die::Fatal(msg::not_valid_object_name(name))),
        Ok(None) => return Err(Die::Fatal(msg::COULD_NOT_GET_INFO.into())),
        Err(e) if is_invalid_type(&e) => return Err(Die::Fatal(e.to_string())),
        Err(e) => {
            report(&e);
            return Err(Die::Fatal(if opt == MODE_P {
                msg::not_valid_object_name(name)
            } else {
                msg::COULD_NOT_GET_INFO.into()
            }));
        }
    };
    let mut out = std::io::stdout().lock();
    match opt {
        MODE_T => {
            let _ = writeln!(out, "{}", header.kind);
        }
        MODE_S => {
            let _ = writeln!(out, "{}", header.size);
        }
        _ => {
            let data = read_data(repo, &oid, name)?;
            if header.kind == ObjectKind::Tree {
                pretty_tree(repo, &data, &mut out);
            } else {
                let _ = out.write_all(&data);
            }
        }
    }
    Ok(0)
}

fn read_data(repo: &Repo, oid: &ObjectId, name: &str) -> Result<Vec<u8>, Die> {
    match repo.odb.read(oid) {
        Ok(Some(raw)) => Ok(raw.data),
        Ok(None) => Err(Die::Fatal(msg::bad_file(name))),
        Err(e) if is_invalid_type(&e) => Err(Die::Fatal(e.to_string())),
        Err(e) => {
            report(&e);
            Err(Die::Fatal(msg::bad_file(name)))
        }
    }
}

/// `-p` of a tree: one `ls-tree` line per entry.
fn pretty_tree(repo: &Repo, data: &[u8], out: &mut impl Write) {
    for e in TreeIter::new(repo.hash, data) {
        let Ok(e) = e else { break };
        let kind = match e.mode_bits & 0o170000 {
            0o040000 => "tree",
            0o160000 => "commit",
            _ => "blob",
        };
        let _ = write!(out, "{:06o} {kind} {}\t", e.mode_bits, e.oid);
        let _ = out.write_all(e.name);
        let _ = out.write_all(b"\n");
    }
}

/// `git cat-file <type> <object>`: the object, peeled to `<type>`.
fn show_typed(repo: &mut Repo, type_name: &str, name: &str) -> CmdResult {
    let want = ObjectKind::from_name(type_name.as_bytes())
        .ok_or_else(|| Die::Fatal(msg::invalid_object_type(type_name)))?;
    let r = resolve_arg(repo, name)?;
    let mut oid = r.oid;
    for _ in 0..64 {
        let real = repo.replaced(&oid);
        let raw = match repo.odb.read(&real) {
            Ok(Some(raw)) => raw,
            Ok(None) => return Err(Die::Fatal(msg::bad_file(name))),
            Err(e) if is_invalid_type(&e) => return Err(Die::Fatal(e.to_string())),
            Err(e) => {
                // git looks the object up three times before it gives up
                // on a damaged loose copy, reporting each attempt.
                if let Some(crate::objects::Location::Loose(path)) = repo.locate(&real) {
                    for _ in 0..3 {
                        report(&e);
                    }
                    return Err(Die::Fatal(msg::loose_object_corrupt(
                        &real.to_hex(),
                        &path.display().to_string(),
                    )));
                }
                report(&e);
                return Err(Die::Fatal(msg::bad_file(name)));
            }
        };
        if raw.kind == want {
            let _ = std::io::stdout().lock().write_all(&raw.data);
            return Ok(0);
        }
        let next = match raw.kind {
            ObjectKind::Tag => rgit_object::Tag::parse(repo.hash, &raw.data)
                .ok()
                .and_then(|t| t.object().ok()),
            ObjectKind::Commit if want == ObjectKind::Tree => {
                rgit_object::Commit::parse(repo.hash, &raw.data)
                    .ok()
                    .and_then(|c| c.tree().ok())
            }
            _ => None,
        };
        match next {
            Some(n) => oid = n,
            None => break,
        }
    }
    Err(Die::Fatal(msg::bad_file(name)))
}

// ----- batch mode ------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Atom {
    Literal(Vec<u8>),
    Name,
    Type,
    Size,
    DiskSize,
    DeltaBase,
    Rest,
    Mode,
}

#[derive(Debug, Clone)]
struct Format {
    atoms: Vec<Atom>,
    uses_rest: bool,
}

impl Format {
    fn parse(s: &str) -> Result<Format, Die> {
        let mut atoms = Vec::new();
        let mut lit = Vec::new();
        let b = s.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' && b.get(i + 1) == Some(&b'(') {
                let Some(end) = s[i..].find(')') else {
                    return Err(Die::Fatal(msg::bad_format_unterminated(&s[i + 1..])));
                };
                let name = &s[i + 2..i + end];
                let atom = match name {
                    "objectname" => Atom::Name,
                    "objecttype" => Atom::Type,
                    "objectsize" => Atom::Size,
                    "objectsize:disk" => Atom::DiskSize,
                    "deltabase" => Atom::DeltaBase,
                    "rest" => Atom::Rest,
                    "objectmode" => Atom::Mode,
                    _ => return Err(Die::Fatal(msg::bad_format(&s[i..=i + end]))),
                };
                if !lit.is_empty() {
                    atoms.push(Atom::Literal(std::mem::take(&mut lit)));
                }
                atoms.push(atom);
                i += end + 1;
            } else {
                lit.push(b[i]);
                i += 1;
            }
        }
        if !lit.is_empty() {
            atoms.push(Atom::Literal(lit));
        }
        let uses_rest = atoms.contains(&Atom::Rest);
        Ok(Format { atoms, uses_rest })
    }
}

struct BatchOptions {
    kind: Batch,
    fmt: Format,
    input_nul: bool,
    output_nul: bool,
    buffer: bool,
    follow: bool,
}

/// What one request asks for.
struct Request<'a> {
    name: &'a [u8],
    rest: &'a [u8],
    contents: bool,
}

struct Out {
    w: Option<BufWriter<std::io::StdoutLock<'static>>>,
    term: u8,
    buffer: bool,
}

impl Out {
    fn new(term: u8, buffer: bool) -> Out {
        Out {
            w: Some(BufWriter::with_capacity(65536, std::io::stdout().lock())),
            term,
            buffer,
        }
    }

    fn w(&mut self) -> &mut BufWriter<std::io::StdoutLock<'static>> {
        self.w.as_mut().expect("writer")
    }

    fn put(&mut self, b: &[u8]) {
        let _ = self.w().write_all(b);
    }

    fn end(&mut self) {
        let t = self.term;
        let _ = self.w().write_all(&[t]);
    }

    /// Flush after a request unless `--buffer`.
    fn done(&mut self) {
        if !self.buffer {
            let _ = self.w().flush();
        }
    }

    fn flush(&mut self) {
        let _ = self.w().flush();
    }

    /// Drop buffered output without writing it (test hook of git:
    /// `GIT_TEST_CAT_FILE_NO_FLUSH_ON_EXIT`).
    fn discard(&mut self) {
        if let Some(w) = self.w.take() {
            let (_, _) = w.into_parts();
        }
    }
}

fn batch_stdin(repo: &mut Repo, opts: &BatchOptions) -> CmdResult {
    let mut out = Out::new(if opts.output_nul { 0 } else { b'\n' }, opts.buffer);
    let delim = if opts.input_nul { 0 } else { b'\n' };
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = input
            .read_until(delim, &mut line)
            .map_err(|e| Die::Fatal(crate::global::strerror(&e)))?;
        if n == 0 {
            break;
        }
        if line.last() == Some(&delim) {
            line.pop();
            if delim == b'\n' && line.last() == Some(&b'\r') {
                line.pop();
            }
        }
        if opts.kind == Batch::Command {
            if let Err(d) = command(repo, opts, &line, &mut out) {
                out.flush();
                return Err(d);
            }
            continue;
        }
        let (name, rest) = if opts.fmt.uses_rest {
            split_rest(&line)
        } else {
            (&line[..], &b""[..])
        };
        let req = Request {
            name,
            rest,
            contents: opts.kind == Batch::Contents,
        };
        if let Err(d) = one(repo, opts, &req, &mut out) {
            out.flush();
            return Err(d);
        }
        out.done();
    }
    if opts.kind == Batch::Command
        && opts.buffer
        && crate::objects::env_bool("GIT_TEST_CAT_FILE_NO_FLUSH_ON_EXIT", false)
    {
        out.discard();
    } else {
        out.flush();
    }
    Ok(0)
}

/// Split at the first whitespace: the name and the rest after the blanks.
fn split_rest(line: &[u8]) -> (&[u8], &[u8]) {
    match line.iter().position(u8::is_ascii_whitespace) {
        Some(i) => {
            let rest = &line[i..];
            let skip = rest.iter().take_while(|c| c.is_ascii_whitespace()).count();
            (&line[..i], &rest[skip..])
        }
        None => (line, b""),
    }
}

/// One line of `--batch-command`.
fn command(repo: &mut Repo, opts: &BatchOptions, line: &[u8], out: &mut Out) -> Result<(), Die> {
    let text = String::from_utf8_lossy(line);
    if line.is_empty() {
        return Err(Die::Fatal(msg::EMPTY_COMMAND.into()));
    }
    if line[0].is_ascii_whitespace() {
        return Err(Die::Fatal(msg::whitespace_before_command(&text)));
    }
    let (cmd, arg) = match line.iter().position(|&c| c == b' ') {
        Some(i) => (&line[..i], Some(&line[i + 1..])),
        None => (line, None),
    };
    match cmd {
        b"contents" | b"info" => {
            let name = String::from_utf8_lossy(cmd);
            let Some(arg) = arg else {
                return Err(Die::Fatal(msg::requires_arguments(&name)));
            };
            let req = Request {
                name: arg,
                rest: b"",
                contents: cmd == b"contents",
            };
            one(repo, opts, &req, out)?;
            out.done();
        }
        b"flush" => {
            if arg.is_some() {
                return Err(Die::Fatal(msg::takes_no_arguments("flush")));
            }
            if !opts.buffer {
                return Err(Die::Fatal(msg::only_for_buffer("flush")));
            }
            out.flush();
        }
        _ => return Err(Die::Fatal(msg::unknown_command(&text))),
    }
    Ok(())
}

/// Answer one request.
fn one(repo: &mut Repo, opts: &BatchOptions, req: &Request<'_>, out: &mut Out) -> Result<(), Die> {
    let name = String::from_utf8_lossy(req.name).into_owned();
    let resolved = {
        let mut r = Resolver::new(repo);
        if opts.follow {
            r.resolve_follow(&name)
        } else {
            r.resolve(&name).map(Ok)
        }
    };
    let resolved = match resolved {
        Ok(Ok(r)) => r,
        Ok(Err(end)) => {
            let (word, shown) = match end {
                SymlinkEnd::Dangling => ("dangling", req.name.to_vec()),
                SymlinkEnd::NotDir => ("notdir", req.name.to_vec()),
                SymlinkEnd::Loop => ("loop", req.name.to_vec()),
                SymlinkEnd::OutOfTree(p) => ("symlink", p),
            };
            out.put(format!("{word} {}", shown.len()).as_bytes());
            out.end();
            out.put(&shown);
            out.end();
            return Ok(());
        }
        Err(Failure::Fatal(m)) => {
            out.flush();
            return Err(Die::Fatal(m));
        }
        Err(Failure::Ambiguous) => {
            out.put(req.name);
            out.put(b" ambiguous");
            out.end();
            return Ok(());
        }
        Err(Failure::Missing { .. }) => {
            out.put(req.name);
            out.put(b" missing");
            out.end();
            return Ok(());
        }
    };
    let real = repo.replaced(&resolved.oid);
    show_object(repo, opts, &resolved.oid, &real, resolved.mode, req, out)
}

/// Print the formatted line (and contents) for `oid`, read as `real`.
#[allow(clippy::too_many_arguments)]
fn show_object(
    repo: &mut Repo,
    opts: &BatchOptions,
    oid: &ObjectId,
    real: &ObjectId,
    mode: Option<u32>,
    req: &Request<'_>,
    out: &mut Out,
) -> Result<(), Die> {
    let header = match repo.odb.header(real) {
        Ok(Some(h)) => h,
        Ok(None) => {
            if mode.is_some_and(|m| m & 0o170000 == 0o160000) {
                out.put(format!("{oid} submodule").as_bytes());
            } else {
                out.put(req.name);
                out.put(b" missing");
            }
            out.end();
            return Ok(());
        }
        Err(e) if is_invalid_type(&e) => return Err(Die::Fatal(e.to_string())),
        Err(e) => {
            report(&e);
            out.put(req.name);
            out.put(b" missing");
            out.end();
            return Ok(());
        }
    };
    let mut line = Vec::new();
    for atom in &opts.fmt.atoms {
        match atom {
            Atom::Literal(l) => line.extend_from_slice(l),
            Atom::Name => line.extend_from_slice(oid.to_hex().as_bytes()),
            Atom::Type => line.extend_from_slice(header.kind.name().as_bytes()),
            Atom::Size => line.extend_from_slice(header.size.to_string().as_bytes()),
            Atom::DiskSize => line.extend_from_slice(repo.disk_size(real).to_string().as_bytes()),
            Atom::DeltaBase => line.extend_from_slice(repo.delta_base(real).to_hex().as_bytes()),
            Atom::Rest => line.extend_from_slice(req.rest),
            Atom::Mode => {
                if let Some(m) = mode {
                    line.extend_from_slice(format!("{m:06o}").as_bytes());
                }
            }
        }
    }
    let data = if req.contents {
        match repo.odb.read(real) {
            Ok(Some(raw)) => Some(raw.data),
            Ok(None) => return Err(Die::Fatal(msg::bad_file(&oid.to_hex()))),
            Err(e) => {
                report(&e);
                return Err(Die::Fatal(msg::bad_file(&oid.to_hex())));
            }
        }
    } else {
        None
    };
    out.put(&line);
    out.end();
    if let Some(data) = data {
        out.put(&data);
        out.end();
    }
    Ok(())
}

/// `--batch-all-objects`: every object in the repository and its
/// alternates, sorted by id, without replacements.
fn batch_all(repo: &mut Repo, opts: &BatchOptions) -> CmdResult {
    let ids = repo.odb.all_objects().map_err(|e| Die::Fatal(e.to_string()))?;
    let mut out = Out::new(if opts.output_nul { 0 } else { b'\n' }, opts.buffer);
    let contents = opts.kind == Batch::Contents;
    for oid in ids {
        let hex = oid.to_hex();
        let req = Request {
            name: hex.as_bytes(),
            rest: b"",
            contents,
        };
        if let Err(d) = show_object(repo, opts, &oid, &oid, None, &req, &mut out) {
            out.flush();
            return Err(d);
        }
        out.done();
    }
    out.flush();
    Ok(0)
}
