// SPDX-License-Identifier: MIT
//! `git hash-object` (Documentation/git-hash-object.adoc).
//!
//! Content filters (`--path`, `--no-filters`) are accepted but no filter
//! runs yet (#111, #112): files are hashed as they are.

use std::io::{BufRead, Read, Write};

use rgit_hash::HashKind;
use rgit_object::ObjectKind;
use rgit_object::fsck::{self, Control, MsgId, Severity};

use crate::messages::hash_object as msg;
use crate::repo_objects::Repo;
use crate::prelude::*;
use crate::repo::{self, Mode};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let kind_name = RefCell::new(None);
    let write = Cell::new(false);
    let stdin = Cell::new(0);
    let stdin_paths = Cell::new(false);
    let no_filters = Cell::new(false);
    let literally = Cell::new(false);
    let path = RefCell::new(None);
    let mut parser = Parser::new(
        msg::HASH_OBJECT_USAGE,
        vec![
            Opt::string('t', None, &kind_name, "type", msg::HASH_OBJECT_HELP_T),
            Opt::flag('w', None, &write, msg::HASH_OBJECT_HELP_W),
            Opt::countup(None, "stdin", &stdin, msg::HASH_OBJECT_HELP_STDIN),
            Opt::flag(None, "stdin-paths", &stdin_paths, msg::HASH_OBJECT_HELP_STDIN_PATHS),
            Opt::flag(None, "no-filters", &no_filters, msg::HASH_OBJECT_HELP_NO_FILTERS),
            Opt::flag(None, "literally", &literally, msg::HASH_OBJECT_HELP_LITERALLY),
            Opt::string(None, "path", &path, "file", msg::HASH_OBJECT_HELP_PATH),
        ],
    );
    let args = parser.parse(argv)?.args;
    if stdin.get() > 1 {
        return Err(parser.error_with_usage(msg::MULTIPLE_STDIN).into());
    }
    if stdin_paths.get() {
        if stdin.get() > 0 {
            return Err(parser.error_with_usage(msg::STDIN_PATHS_WITH_STDIN).into());
        }
        if !args.is_empty() {
            return Err(parser.error_with_usage(msg::STDIN_PATHS_WITH_FILES).into());
        }
        if path.borrow().is_some() {
            return Err(parser.error_with_usage(msg::STDIN_PATHS_WITH_PATH).into());
        }
    }
    if no_filters.get() && path.borrow().is_some() {
        return Err(parser.error_with_usage(msg::PATH_WITH_NO_FILTERS).into());
    }
    drop(parser);

    let kind = match kind_name.borrow().as_deref() {
        None => ObjectKind::Blob,
        Some(t) => ObjectKind::from_name(t.as_bytes())
            .ok_or_else(|| Die::Fatal(msg::invalid_object_type(t)))?,
    };

    // Outside a repository hashing works with the default hash; writing
    // needs one.
    let setup = if write.get() {
        Some(repo::setup(ctx, Mode::Required)?)
    } else {
        repo::setup_gently(ctx)?
    };
    let prefix = setup
        .as_ref()
        .and_then(|s| s.prefix.as_ref())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let repo = match setup {
        Some(s) => Some(Repo::open(ctx, s)?),
        None => None,
    };
    let hasher = Hasher {
        hash: repo.as_ref().map_or(HashKind::Sha1, |r| r.hash),
        repo: repo.as_ref(),
        kind,
        write: write.get(),
        literally: literally.get(),
    };

    let mut out = std::io::stdout().lock();
    if stdin.get() > 0 {
        let mut data = Vec::new();
        std::io::stdin()
            .lock()
            .read_to_end(&mut data)
            .map_err(|e| Die::Fatal(msg::could_not_read_from_stdin(&crate::global::strerror(&e))))?;
        let oid = hasher.hash(&data)?;
        let _ = writeln!(out, "{oid}");
    }
    for a in &args {
        let file = prefixed(&prefix, a);
        let data = read_file(&file)?;
        let oid = hasher.hash(&data)?;
        let _ = writeln!(out, "{oid}");
    }
    if stdin_paths.get() {
        let stdin = std::io::stdin();
        for line in stdin.lock().split(b'\n') {
            let line = line.map_err(|e| Die::Fatal(crate::global::strerror(&e)))?;
            let name = if line.first() == Some(&b'"') {
                crate::quote::c_unquote(&line)
                    .ok_or_else(|| Die::Fatal(msg::LINE_BADLY_QUOTED.into()))?
            } else {
                line
            };
            // git does not apply the prefix to these paths.
            let file = String::from_utf8_lossy(&name).into_owned();
            let data = read_file(&file)?;
            let oid = hasher.hash(&data)?;
            let _ = writeln!(out, "{oid}");
            let _ = out.flush();
        }
    }
    Ok(0)
}

/// A path given on the command line, relative to where the command was
/// started.
fn prefixed(prefix: &str, path: &str) -> String {
    if prefix.is_empty() || std::path::Path::new(path).is_absolute() {
        path.to_string()
    } else {
        format!("{prefix}{path}")
    }
}

fn read_file(path: &str) -> Result<Vec<u8>, Die> {
    let meta = std::fs::metadata(path).map_err(|e| {
        Die::Fatal(msg::could_not_open_for_reading(
            path,
            &crate::global::strerror(&e),
        ))
    })?;
    // Character devices and FIFOs (e.g. /dev/null) are read like files.
    if meta.is_dir() {
        return Err(Die::Fatal(msg::unable_to_hash(path)));
    }
    std::fs::read(path).map_err(|e| {
        Die::Fatal(msg::could_not_open_for_reading(
            path,
            &crate::global::strerror(&e),
        ))
    })
}

struct Hasher<'a> {
    hash: HashKind,
    repo: Option<&'a Repo>,
    kind: ObjectKind,
    write: bool,
    literally: bool,
}

impl Hasher<'_> {
    fn hash(&self, data: &[u8]) -> Result<rgit_hash::ObjectId, Die> {
        if !self.literally && self.kind != ObjectKind::Blob && !passes_fsck(self.kind, self.hash, data) {
            return Err(Die::Fatal(msg::REFUSING_MALFORMED.into()));
        }
        match (self.write, self.repo) {
            (true, Some(repo)) => repo.odb.write(self.kind, data).map_err(|e| {
                for line in e.git_lines() {
                    eprintln!("error: {line}");
                }
                Die::Fatal(msg::unable_to_write_object(self.kind.name()))
            }),
            _ => rgit_hash::hash_object(self.hash, self.kind.name(), data)
                .map_err(|e| Die::Fatal(e.to_string())),
        }
    }
}

/// Check an object like git does before creating it: every finding that
/// is not ignored by default is an error (strict mode, `fsck.*`
/// configuration does not apply). Returns whether the object passes.
fn passes_fsck(kind: ObjectKind, hash: HashKind, data: &[u8]) -> bool {
    let mut failed = false;
    let mut extra_header = None;
    let mut err = std::io::stderr().lock();
    fsck::check_object(kind, hash, data, &fsck::Options::default(), &mut |f| {
        let ignored = f.id.default_severity() == Severity::Ignore;
        if f.id == MsgId::ExtraHeaderEntry {
            extra_header = Some(!ignored);
        }
        if !ignored {
            if let Some(cause) = &f.cause {
                let _ = writeln!(err, "error: {cause}");
            }
            let _ = writeln!(err, "error: {}", msg::object_fails_fsck(&f.to_string()));
            failed = true;
        }
        Control::Continue
    });
    // The extra-header check of a tag runs last and its verdict replaces
    // the earlier ones (git behaves this way; an ignored extra header lets
    // the tag pass).
    match (kind, extra_header) {
        (ObjectKind::Tag, Some(v)) => !v,
        _ => !failed,
    }
}
