// SPDX-License-Identifier: MIT
//! `git show-ref` (Documentation/git-show-ref.adoc): list references
//! matching patterns, `--verify` exact names, `--exists` and
//! `--exclude-existing`.

use std::collections::BTreeSet;
use std::io::{BufRead, Write};

use rgit_hash::ObjectId;
use rgit_object::ObjectKind;
use rgit_refs::files::FilesStore;
use rgit_refs::name::{RefNameFlags, is_root_ref_syntax, is_valid_refname};
use rgit_refs::{IterItem, IterOptions, RefStore, Target};

use crate::messages::show_ref as msg;
use crate::prelude::*;
use crate::refs_support::{self, Objects};
use crate::repo::{self, Mode};

/// Default abbreviation length (`core.abbrev` is not read yet).
const DEFAULT_ABBREV: usize = 7;

struct Show<'a> {
    objects: &'a Objects,
    deref: bool,
    hash_only: bool,
    abbrev: usize,
    quiet: bool,
    out: std::io::StdoutLock<'static>,
}

impl Show<'_> {
    fn hex(&self, id: &ObjectId) -> String {
        if self.abbrev == 0 {
            id.to_hex()
        } else {
            self.objects.abbrev(id, self.abbrev)
        }
    }

    /// Prints one reference (and its peeled value with `-d`).
    fn show(&mut self, name: &str, id: &ObjectId) -> Result<(), Die> {
        if !self.objects.contains(id) {
            return Err(Die::Fatal(msg::bad_ref(name, &id.to_hex())));
        }
        if self.quiet {
            return Ok(());
        }
        let hex = self.hex(id);
        if self.hash_only {
            let _ = writeln!(self.out, "{hex}");
        } else {
            let _ = writeln!(self.out, "{hex} {name}");
        }
        if self.deref && self.objects.kind(id) == Some(ObjectKind::Tag) {
            if let Some(p) = self.objects.peel(id) {
                let hex = self.hex(&p);
                let _ = writeln!(self.out, "{hex} {name}^{{}}");
            }
        }
        Ok(())
    }
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let tags = Cell::new(false);
    let branches = Cell::new(false);
    let exists = Cell::new(false);
    let verify = Cell::new(false);
    let head = Cell::new(false);
    let deref = Cell::new(false);
    let quiet = Cell::new(false);
    let hash_only = Cell::new(false);
    let abbrev = Cell::new(0usize);
    let exclude_existing: RefCell<Option<String>> = RefCell::new(None);
    let parse_abbrev = |v: Option<&str>| -> Result<usize, CallbackError> {
        match v {
            None => Ok(DEFAULT_ABBREV),
            Some(v) => v
                .parse::<i64>()
                .map(|n| n.max(0) as usize)
                .map_err(|_| CallbackError::Error(String::new())),
        }
    };
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag(None, "tags", &tags, msg::HELP_TAGS),
            Opt::flag(None, "branches", &branches, msg::HELP_BRANCHES),
            Opt::flag(None, "heads", &branches, msg::HELP_BRANCHES).hidden(),
            Opt::flag(None, "exists", &exists, msg::HELP_EXISTS),
            Opt::flag(None, "verify", &verify, msg::HELP_VERIFY),
            Opt::flag(None, "head", &head, msg::HELP_HEAD),
            Opt::flag('d', "dereference", &deref, msg::HELP_DEREFERENCE),
            Opt::callback('s', "hash", Some("n"), msg::HELP_HASH, |v, unset| {
                hash_only.set(!unset);
                if v.is_some() {
                    match parse_abbrev(v) {
                        Ok(n) => abbrev.set(n),
                        Err(_) => return Err(CallbackError::Error(msg::expects_number("hash"))),
                    }
                }
                Ok(())
            })
            .optional(None),
            Opt::callback(None, "abbrev", Some("n"), msg::HELP_ABBREV, |v, unset| {
                if unset {
                    abbrev.set(0);
                    return Ok(());
                }
                match parse_abbrev(v) {
                    Ok(n) => {
                        abbrev.set(n);
                        Ok(())
                    }
                    Err(_) => Err(CallbackError::Error(msg::expects_number("abbrev"))),
                }
            })
            .optional(None),
            Opt::flag('q', "quiet", &quiet, msg::HELP_QUIET),
            Opt::callback(
                None,
                "exclude-existing",
                Some("pattern"),
                msg::HELP_EXCLUDE_EXISTING,
                |v, _| {
                    *exclude_existing.borrow_mut() = Some(v.unwrap_or("").to_owned());
                    Ok(())
                },
            )
            .optional(None)
            .noneg(),
        ],
    );
    let args = parser.parse(argv)?.args;
    let excl = exclude_existing.borrow().clone();
    if excl.is_some() && exists.get() {
        return Err(Die::Fatal(msg::incompatible(
            "--exclude-existing",
            "--exists",
        )));
    }
    if verify.get() && exists.get() {
        return Err(Die::Fatal(msg::incompatible(
            "--verify", "--exists",
        )));
    }
    if excl.is_some() && verify.get() {
        return Err(Die::Fatal(msg::incompatible(
            "--exclude-existing",
            "--verify",
        )));
    }

    let setup = repo::setup(ctx, Mode::Required)?;
    let config = refs_support::load_config(ctx, &setup);
    let store = refs_support::ref_store(&setup, &config);
    let objects = Objects::open(&setup);

    if let Some(pattern) = excl {
        return exclude_existing_mode(&store, &pattern);
    }
    if exists.get() {
        return exists_mode(&store, &args);
    }

    let mut show = Show {
        objects: &objects,
        deref: deref.get(),
        hash_only: hash_only.get(),
        abbrev: abbrev.get(),
        quiet: quiet.get(),
        out: std::io::stdout().lock(),
    };

    if verify.get() {
        if args.is_empty() {
            return Err(Die::Fatal(msg::VERIFY_REQUIRES_REF.to_owned()));
        }
        for name in &args {
            let ok = name.starts_with("refs/") || is_root_ref_syntax(name);
            let id = if ok {
                store.resolve(name).ok().flatten().and_then(|r| r.oid)
            } else {
                None
            };
            match id {
                Some(id) => show.show(name, &id)?,
                None if quiet.get() => return Ok(1),
                None => {
                    let _ = show.out.flush();
                    return Err(Die::Fatal(msg::not_a_valid_ref(name)));
                }
            }
        }
        return Ok(0);
    }

    let mut found = false;
    if head.get() {
        if let Ok(Some(r)) = store.resolve("HEAD") {
            if let Some(id) = r.oid {
                show.show("HEAD", &id)?;
                found = true;
            }
        }
    }
    let items = store
        .iter(&IterOptions::default())
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let null = ObjectId::null(setup.format.object_format);
    for item in items {
        let name = item.name().to_owned();
        if tags.get() || branches.get() {
            let t = tags.get() && name.starts_with("refs/tags/");
            let b = branches.get() && name.starts_with("refs/heads/");
            if !t && !b {
                continue;
            }
        }
        if !args.is_empty() && !args.iter().any(|p| tail_matches(&name, p)) {
            continue;
        }
        let id = match &item {
            IterItem::Ref(r) => r.oid.unwrap_or(null),
            _ => null,
        };
        show.show(&name, &id)?;
        found = true;
    }
    Ok(if found { 0 } else { 1 })
}

/// Whether `pattern` matches the end of `name` at a component boundary.
fn tail_matches(name: &str, pattern: &str) -> bool {
    match name.strip_suffix(pattern) {
        Some("") => true,
        Some(head) => head.ends_with('/') || pattern.starts_with('/'),
        None => false,
    }
}

/// `--exists <ref>`: exit 0 if it exists (even dangling), 2 if not.
fn exists_mode(store: &FilesStore, args: &[String]) -> CmdResult {
    match args.len() {
        0 => return Err(Die::Fatal(msg::EXISTS_REQUIRES_REF.to_owned())),
        1 => {}
        _ => return Err(Die::Fatal(msg::EXISTS_ONE_REF.to_owned())),
    }
    let name = &args[0];
    let path = store.ref_path(name);
    if path.is_dir() {
        return Err(Die::Error(msg::DOES_NOT_EXIST.to_owned(), 2));
    }
    match store.read_raw(name) {
        Ok(Some(_)) => Ok(0),
        Ok(None) => Err(Die::Error(msg::DOES_NOT_EXIST.to_owned(), 2)),
        Err(_) => Err(Die::Error(msg::failed_lookup("Invalid argument"), 1)),
    }
}

/// `--exclude-existing[=<pattern>]`: filter stdin lines whose reference
/// does not exist locally.
fn exclude_existing_mode(store: &FilesStore, pattern: &str) -> CmdResult {
    let mut existing = BTreeSet::new();
    if let Ok(items) = store.iter(&IterOptions::default()) {
        for item in items {
            if let IterItem::Ref(r) = item {
                if !matches!(r.target, Target::Symbolic(_)) || r.oid.is_some() {
                    existing.insert(r.name);
                }
            }
        }
    }
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.strip_suffix("^{}").unwrap_or(&line);
        let refname = match line.find(' ') {
            Some(i) => &line[i + 1..],
            None => line,
        };
        if !pattern.is_empty() && !refname.starts_with(pattern) {
            continue;
        }
        if existing.contains(refname) {
            continue;
        }
        if !is_valid_refname(refname.as_bytes(), RefNameFlags::default()) {
            eprintln!("{}", msg::bad_ref_format(refname));
            continue;
        }
        let _ = writeln!(out, "{line}");
    }
    Ok(0)
}
