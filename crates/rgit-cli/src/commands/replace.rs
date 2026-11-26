// SPDX-License-Identifier: MIT
//! `git replace` (Documentation/git-replace.adoc): create, list and delete
//! the refs below `refs/replace/` (`$GIT_REPLACE_REF_BASE`) that make
//! commands read one object in place of another; build replacements by
//! editing an object (`--edit`) or changing a commit's parents (`--graft`,
//! `--convert-graft-file`).

use std::io::Write;
use std::path::Path;

use rgit_hash::ObjectId;
use rgit_object::fsck::{self, Severity};
use rgit_object::{ObjectKind, TreeIter};
use rgit_refs::files::FilesStore;
use rgit_refs::{RefStore, RefUpdate, Target, Transaction};

use crate::messages::replace as msg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, Revs, UpdateHooks};
use crate::repo::{self, Mode};
use crate::repo_objects::Repo;

const MODE_LIST: i32 = b'l' as i32;
const MODE_DELETE: i32 = b'd' as i32;
const MODE_EDIT: i32 = b'e' as i32;
const MODE_GRAFT: i32 = b'g' as i32;
const MODE_CONVERT: i32 = b'c' as i32;
const MODE_REPLACE: i32 = b'r' as i32;

/// `return -1` in git: an error was printed, exit 255.
const FAILED: i32 = 255;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mode = Cell::new(0);
    let force = Cell::new(false);
    let raw = Cell::new(false);
    let format: RefCell<Option<String>> = RefCell::new(None);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::cmdmode('l', "list", &mode, msg::HELP_LIST, MODE_LIST),
            Opt::cmdmode('d', "delete", &mode, msg::HELP_DELETE, MODE_DELETE),
            Opt::cmdmode('e', "edit", &mode, msg::HELP_EDIT, MODE_EDIT),
            Opt::cmdmode('g', "graft", &mode, msg::HELP_GRAFT, MODE_GRAFT),
            Opt::cmdmode(None, "convert-graft-file", &mode, msg::HELP_CONVERT, MODE_CONVERT),
            Opt::flag('f', "force", &force, msg::HELP_FORCE),
            Opt::flag(None, "raw", &raw, msg::HELP_RAW),
            Opt::string(None, "format", &format, "format", msg::HELP_FORMAT),
        ],
    );
    let args = parser.parse(argv)?.args;
    let mut mode = mode.get();
    if mode == 0 {
        mode = if args.is_empty() { MODE_LIST } else { MODE_REPLACE };
    }
    let format = format.borrow().clone();
    if format.is_some() && mode != MODE_LIST {
        return Err(parser.usage_msg(msg::FORMAT_NOT_LISTING).into());
    }
    let writes = matches!(mode, MODE_REPLACE | MODE_EDIT | MODE_GRAFT | MODE_CONVERT);
    if force.get() && !writes {
        return Err(parser.usage_msg(msg::FORCE_NOT_WRITING).into());
    }
    if raw.get() && mode != MODE_EDIT {
        return Err(parser.usage_msg(msg::RAW_WITHOUT_EDIT).into());
    }
    let arg_error = match mode {
        MODE_DELETE if args.is_empty() => Some(msg::DELETE_NEEDS_ARG),
        MODE_REPLACE if args.len() != 2 => Some(msg::BAD_NUMBER_OF_ARGS),
        MODE_EDIT if args.len() != 1 => Some(msg::EDIT_NEEDS_ONE),
        MODE_GRAFT if args.is_empty() => Some(msg::GRAFT_NEEDS_ARG),
        MODE_CONVERT if !args.is_empty() => Some(msg::CONVERT_TAKES_NO_ARG),
        MODE_LIST if args.len() > 1 => Some(msg::ONE_PATTERN),
        _ => None,
    };
    if let Some(m) = arg_error {
        return Err(parser.usage_msg(m).into());
    }
    drop(parser);

    let setup = repo::setup(ctx, Mode::Required)?;
    let mut repo = Repo::open(ctx, setup)?;
    let config = repo.config.clone();
    let revs = Revs::new(ctx, &repo.setup)?;
    let objects = Objects::open(&repo.setup);
    let store = refs_support::ref_store(&repo.setup, &config);
    let mut cmd = Replace {
        ctx,
        repo: &mut repo,
        revs: &revs,
        objects: &objects,
        store: &store,
        base: crate::replace_objects::ref_base(),
        force: force.get(),
    };
    match mode {
        MODE_LIST => cmd.list(args.first().map(String::as_str), format.as_deref()),
        MODE_DELETE => cmd.delete(&args),
        MODE_REPLACE => cmd.replace(&args[0], &args[1]),
        MODE_EDIT => cmd.edit(&args[0], raw.get()),
        MODE_GRAFT => Ok(match cmd.graft(&args, false) {
            Ok(()) => 0,
            Err(()) => FAILED,
        }),
        _ => cmd.convert_graft_file(),
    }
}

struct Replace<'a> {
    ctx: &'a Context,
    repo: &'a mut Repo,
    revs: &'a Revs,
    objects: &'a Objects,
    store: &'a FilesStore,
    base: String,
    force: bool,
}

/// One header of a commit: its name and its raw lines (with the
/// continuation lines and newlines).
struct Header<'d> {
    name: &'d [u8],
    raw: &'d [u8],
}

/// The headers and the rest (the blank line and the message) of a commit.
fn split_headers(data: &[u8]) -> (Vec<Header<'_>>, &[u8]) {
    let mut headers: Vec<Header<'_>> = Vec::new();
    let mut pos = 0;
    let mut start = 0;
    while pos < data.len() && data[pos] != b'\n' {
        let end = data[pos..]
            .iter()
            .position(|&c| c == b'\n')
            .map_or(data.len(), |i| pos + i + 1);
        if data[pos] != b' ' && pos > start {
            let raw = &data[start..pos];
            headers.push(Header {
                name: header_name(raw),
                raw,
            });
            start = pos;
        }
        pos = end;
    }
    if pos > start {
        let raw = &data[start..pos];
        headers.push(Header {
            name: header_name(raw),
            raw,
        });
    }
    (headers, &data[pos..])
}

fn header_name(raw: &[u8]) -> &[u8] {
    let end = raw
        .iter()
        .position(|&c| c == b' ' || c == b'\n')
        .unwrap_or(raw.len());
    &raw[..end]
}

/// The value of a multi-line header: the text after the name with the
/// continuation lines unfolded.
fn header_value(h: &Header<'_>) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, line) in h.raw.split_inclusive(|&c| c == b'\n').enumerate() {
        if i == 0 {
            out.extend_from_slice(line.get(h.name.len() + 1..).unwrap_or_default());
        } else {
            out.extend_from_slice(line.strip_prefix(b" ").unwrap_or(line));
        }
    }
    out
}

impl Replace<'_> {
    fn refname(&self, oid: &ObjectId) -> String {
        format!("{}{}", self.base, oid.to_hex())
    }

    /// The current value of the replace ref of `oid`.
    fn current(&self, oid: &ObjectId) -> Option<ObjectId> {
        match self.store.read_raw(&self.refname(oid)) {
            Ok(Some(Target::Object(id))) => Some(id),
            _ => None,
        }
    }

    /// Resolve a name like git's `get_oid` (no peeling).
    fn resolve(&self, name: &str) -> Option<ObjectId> {
        self.revs.resolve(name)
    }

    /// The type of the stored object (replacements ignored).
    fn kind(&self, oid: &ObjectId) -> Option<ObjectKind> {
        self.repo
            .odb
            .header_unreplaced(oid)
            .ok()
            .flatten()
            .map(|h| h.kind)
    }

    /// `-l [<pattern>]`.
    fn list(&mut self, pattern: Option<&str>, format: Option<&str>) -> CmdResult {
        let format = format.unwrap_or("short");
        if !matches!(format, "short" | "medium" | "long") {
            return Err(Die::Error(msg::invalid_format(format), FAILED));
        }
        let pattern = pattern.unwrap_or("*");
        let refs = crate::refs::Refs::new(&self.repo.setup).list(&self.base);
        let mut out = std::io::stdout().lock();
        for (name, target) in refs {
            let short = &name[self.base.len().min(name.len())..];
            if !rgit_pathspec::wildmatch::wildmatch(
                pattern.as_bytes(),
                short.as_bytes(),
                rgit_pathspec::wildmatch::WildmatchFlags::NONE,
            ) {
                continue;
            }
            let _ = match format {
                "short" => writeln!(out, "{short}"),
                "medium" => writeln!(out, "{short} -> {target}"),
                _ => {
                    let replaced = ObjectId::from_hex(self.repo.hash, short.as_bytes())
                        .ok()
                        .and_then(|id| self.kind(&id));
                    let name = |k: Option<ObjectKind>| k.map_or("unknown", ObjectKind::name);
                    writeln!(
                        out,
                        "{short} ({}) -> {target} ({})",
                        name(replaced),
                        name(self.kind(&target))
                    )
                }
            };
        }
        Ok(0)
    }

    /// `-d <object>...`.
    fn delete(&mut self, names: &[String]) -> CmdResult {
        let mut code = 0;
        // Written at the end, like git's buffered standard output.
        let mut deleted = Vec::new();
        for name in names {
            let Some(oid) = self.resolve(name) else {
                eprintln!("error: {}", msg::failed_to_resolve(name));
                code = 1;
                continue;
            };
            let hex = oid.to_hex();
            let Some(old) = self.current(&oid) else {
                eprintln!("error: {}", msg::not_found(&hex));
                code = 1;
                continue;
            };
            let mut hooks = UpdateHooks::new(&self.repo.setup, &self.repo.config, self.objects);
            let mut tx = Transaction::new(self.store);
            let refname = self.refname(&oid);
            let done = tx
                .push(RefUpdate::delete(refname, self.repo.hash, Some(old)).no_deref(true))
                .and_then(|()| tx.commit(&mut hooks));
            match done {
                Ok(()) => deleted.push(msg::deleted(&hex)),
                Err(e) => {
                    eprintln!("error: {e}");
                    code = 1;
                }
            }
        }
        let mut out = std::io::stdout().lock();
        for line in deleted {
            let _ = writeln!(out, "{line}");
        }
        Ok(code)
    }

    /// Point the replace ref of `object` at `replacement` (`-f`: even if it
    /// exists).
    fn write_ref(&self, object: &ObjectId, replacement: &ObjectId) -> Result<(), ()> {
        let refname = self.refname(object);
        let existing = self.current(object);
        if existing.is_some() && !self.force {
            eprintln!("error: {}", msg::already_exists(&refname));
            return Err(());
        }
        let update = match existing {
            Some(old) => RefUpdate::update(refname, *replacement, Some(old)),
            None => RefUpdate::create(refname, *replacement),
        };
        let mut hooks = UpdateHooks::new(&self.repo.setup, &self.repo.config, self.objects);
        let mut tx = Transaction::new(self.store);
        if let Err(e) = tx.push(update).and_then(|()| tx.commit(&mut hooks)) {
            eprintln!("error: {e}");
            return Err(());
        }
        Ok(())
    }

    /// Fail with "already exists" unless `-f` or the ref is new.
    fn check_new(&self, object: &ObjectId) -> Result<(), ()> {
        if !self.force && self.current(object).is_some() {
            eprintln!("error: {}", msg::already_exists(&self.refname(object)));
            return Err(());
        }
        Ok(())
    }

    /// `<object> <replacement>`.
    fn replace(&mut self, object: &str, replacement: &str) -> CmdResult {
        let Some(from) = self.resolve(object) else {
            return Err(Die::Error(msg::failed_to_resolve(object), FAILED));
        };
        let Some(to) = self.resolve(replacement) else {
            return Err(Die::Error(msg::failed_to_resolve(replacement), FAILED));
        };
        if !self.force {
            let a = self.kind(&from);
            let b = self.kind(&to);
            if a != b {
                let name = |k: Option<ObjectKind>| k.map_or("unknown", ObjectKind::name);
                return Err(Die::Error(
                    msg::type_mismatch(object, name(a), replacement, name(b)),
                    FAILED,
                ));
            }
        }
        Ok(match self.write_ref(&from, &to) {
            Ok(()) => 0,
            Err(()) => FAILED,
        })
    }

    /// `--edit <object>`: pretty-print the stored object into
    /// `$GIT_DIR/REPLACE_EDITOBJ`, let the user edit it and replace the
    /// object with the result.
    fn edit(&mut self, name: &str, raw: bool) -> CmdResult {
        let Some(oid) = self.resolve(name) else {
            return Err(Die::Error(msg::failed_to_resolve(name), FAILED));
        };
        if self.check_new(&oid).is_err() {
            return Ok(FAILED);
        }
        let obj = match self.repo.odb.read_unreplaced(&oid) {
            Ok(Some(o)) => o,
            _ => return Err(Die::Fatal(msg::unable_to_read(&oid.to_hex()))),
        };
        let kind = obj.kind;
        let git_dir = self.repo.setup.cwd.join(&self.repo.setup.git_dir);
        let path = git_dir.join("REPLACE_EDITOBJ");
        let mut text = Vec::new();
        if kind == ObjectKind::Tree && !raw {
            for e in TreeIter::new(self.repo.hash, &obj.data) {
                let Ok(e) = e else { break };
                let k = match e.mode_bits & 0o170000 {
                    0o040000 => "tree",
                    0o160000 => "commit",
                    _ => "blob",
                };
                let _ = write!(text, "{:06o} {k} {}\t", e.mode_bits, e.oid);
                text.extend_from_slice(e.name);
                text.push(b'\n');
            }
        } else {
            text = obj.data;
        }
        std::fs::write(&path, &text)
            .map_err(|e| Die::Fatal(format!("{}: {}", path.display(), crate::global::strerror(&e))))?;

        let env = rgit_config::SystemEnv::default();
        let editor = rgit_ident::editor::editor(&self.repo.config, &env).map_err(|p| {
            p.print();
            Die::Exit(rgit_options::EXIT_FATAL)
        })?;
        let advisor = self.ctx.advisor(&self.repo.config)?;
        let opts = rgit_ident::editor::LaunchOptions {
            hint: advisor
                .enabled(Advice::WaitingForEditor)
                .map_err(Die::Fatal)?,
            env: self.ctx.child_env(),
            cwd: None,
        };
        if let Err(e) = rgit_ident::editor::launch(editor.as_deref(), &path, &opts) {
            e.print();
            return Err(Die::Error(msg::EDITING_FAILED.into(), FAILED));
        }

        let new = if kind == ObjectKind::Tree && !raw {
            match self.mktree(&path)? {
                Some(id) => id,
                None => return Err(Die::Error(msg::MKTREE_FAILED.into(), FAILED)),
            }
        } else {
            let data = std::fs::read(&path).map_err(|e| {
                Die::Fatal(format!("{}: {}", path.display(), crate::global::strerror(&e)))
            })?;
            let problem = fsck::findings(kind, self.repo.hash, &data)
                .into_iter()
                .find(|f| matches!(f.id.default_severity(), Severity::Fatal | Severity::Error));
            if let Some(f) = problem {
                eprintln!("error: {}", msg::fails_fsck(&f.to_string()));
                return Err(Die::Fatal(msg::MALFORMED.into()));
            }
            self.repo
                .odb
                .write(kind, &data)
                .map_err(|e| Die::Fatal(e.to_string()))?
        };
        if new == oid {
            return Err(Die::Error(msg::same_object(&oid.to_hex()), FAILED));
        }
        Ok(match self.write_ref(&oid, &new) {
            Ok(()) => 0,
            Err(()) => FAILED,
        })
    }

    /// Run `mktree` on the edited file (as git does); `None` if it failed.
    fn mktree(&self, path: &Path) -> Result<Option<ObjectId>, Die> {
        let exe = std::env::current_exe().map_err(|e| Die::Fatal(e.to_string()))?;
        let input = std::fs::File::open(path)
            .map_err(|e| Die::Fatal(format!("{}: {}", path.display(), crate::global::strerror(&e))))?;
        let out = std::process::Command::new(exe)
            .arg("mktree")
            .envs(self.ctx.child_env())
            .stdin(input)
            .stderr(std::process::Stdio::inherit())
            .output()
            .map_err(|e| Die::Fatal(e.to_string()))?;
        if !out.status.success() {
            return Ok(None);
        }
        let hex = String::from_utf8_lossy(&out.stdout);
        Ok(ObjectId::from_hex(self.repo.hash, hex.trim().as_bytes()).ok())
    }

    /// The commit `oid` stands for (tags are peeled); prints why not.
    fn peel_commit(&self, oid: ObjectId, name: &str, as_parent: bool) -> Result<ObjectId, ()> {
        let mut cur = oid;
        for _ in 0..64 {
            let obj = match self.repo.odb.read_unreplaced(&cur) {
                Ok(Some(o)) => o,
                _ => break,
            };
            match obj.kind {
                ObjectKind::Commit => return Ok(cur),
                ObjectKind::Tag => {
                    match rgit_object::Tag::parse(self.repo.hash, &obj.data).and_then(|t| t.object()) {
                        Ok(next) => cur = next,
                        Err(_) => break,
                    }
                }
                k => {
                    eprintln!("error: {}", msg::not_a_commit(&cur.to_hex(), k.name()));
                    break;
                }
            }
        }
        if as_parent {
            eprintln!("error: {}", msg::could_not_parse_as_commit(name));
        } else {
            eprintln!("error: {}", msg::could_not_parse(name));
        }
        Err(())
    }

    /// `--graft <commit> [<parent>...]` (`converting`: a line of the graft
    /// file, which may name the parents the commit has anyway).
    fn graft(&mut self, args: &[String], converting: bool) -> Result<(), ()> {
        let name = &args[0];
        let Some(oid) = self.resolve(name) else {
            eprintln!("error: {}", msg::not_a_valid_object_name(name));
            return Err(());
        };
        let commit = self.peel_commit(oid, name, false)?;
        // Parsing a commit reads the graft file, as in git.
        crate::grafts::Lazy::new(&self.repo.setup.common_dir, self.repo.hash, &self.repo.config)
            .touch();
        let mut parents = Vec::new();
        for p in &args[1..] {
            let Some(id) = self.resolve(p) else {
                eprintln!("error: {}", msg::not_a_valid_object_name(p));
                return Err(());
            };
            parents.push(self.peel_commit(id, p, true)?);
        }
        self.check_new(&commit)?;
        let Ok(Some(obj)) = self.repo.odb.read_unreplaced(&commit) else {
            eprintln!("error: {}", msg::could_not_parse(name));
            return Err(());
        };
        let hex = commit.to_hex();
        let (headers, rest) = split_headers(&obj.data);
        let mut data = Vec::with_capacity(obj.data.len());
        let mut warned = false;
        for h in &headers {
            match h.name {
                b"parent" => {}
                b"tree" => {
                    data.extend_from_slice(h.raw);
                    for p in &parents {
                        let _ = writeln!(data, "parent {p}");
                    }
                }
                b"gpgsig" | b"gpgsig-sha256" => {
                    if !warned {
                        eprintln!("warning: {}", msg::has_signature(&hex));
                        eprintln!("warning: {}", msg::SIGNATURE_REMOVED);
                        warned = true;
                    }
                }
                b"mergetag" => {
                    let value = header_value(h);
                    let tagged = rgit_object::Tag::parse(self.repo.hash, &value)
                        .and_then(|t| t.object())
                        .ok();
                    if !tagged.is_some_and(|t| parents.contains(&t)) {
                        let tag = rgit_hash::hash_object(self.repo.hash, "tag", &value)
                            .map_or_else(|_| String::new(), |id| id.to_hex());
                        eprintln!("error: {}", msg::mergetag_discarded(&hex, &tag));
                        return Err(());
                    }
                    data.extend_from_slice(h.raw);
                }
                _ => data.extend_from_slice(h.raw),
            }
        }
        data.extend_from_slice(rest);
        let new = match self.repo.odb.write(ObjectKind::Commit, &data) {
            Ok(id) => id,
            Err(e) => {
                eprintln!("error: {e}");
                return Err(());
            }
        };
        if new == commit {
            if converting {
                eprintln!("warning: {}", msg::graft_unnecessary(&hex));
                return Ok(());
            }
            eprintln!("error: {}", msg::same_commit(&hex));
            return Err(());
        }
        self.write_ref(&commit, &new)
    }

    /// `--convert-graft-file`: a graft commit for every line of the graft
    /// file, which is removed when all of them succeed.
    fn convert_graft_file(&mut self) -> CmdResult {
        crate::grafts::quiet_hint();
        let path = crate::grafts::file(&self.repo.setup.common_dir);
        let Ok(data) = std::fs::read(&path) else {
            return Ok(1);
        };
        let text = String::from_utf8_lossy(&data).into_owned();
        let mut failed = String::new();
        for line in text.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let args: Vec<String> = line
                .split(|c: char| c.is_ascii_whitespace())
                .filter(|w| !w.is_empty())
                .map(str::to_owned)
                .collect();
            if args.is_empty() || self.graft(&args, true).is_err() {
                failed.push_str("\n\t");
                failed.push_str(line);
            }
        }
        if !failed.is_empty() {
            eprintln!("warning: {}", msg::could_not_convert(&failed));
            return Ok(1);
        }
        let _ = std::fs::remove_file(&path);
        Ok(0)
    }
}
