// SPDX-License-Identifier: MIT
//! `git tag` (Documentation/git-tag.adoc, Documentation/config/tag.adoc,
//! Documentation/config/versionsort.adoc), core slice of #130:
//!
//! - lightweight tags; annotated tags with `-a`, `-m` (repeatable), `-F`
//!   (`-` for stdin), `-e`, the editor template in `TAG_EDITMSG` (reusing
//!   the message of a replaced annotated tag), `--cleanup`, `--trailer`
//!   (appended as `<token>: <value>` lines, without `trailer.*`); `-f`
//!   ("Updated tag ..."), `--create-reflog`, the reflog message and the
//!   `nestedTag` advice;
//! - `-d` ("Deleted tag ...");
//! - listing: patterns, `-i`, `-n[<num>]`, `--contains`, `--no-contains`,
//!   `--merged`, `--no-merged`, `--points-at`, `--sort` and `tag.sort`
//!   (the atoms of `for-each-ref` and `version:` with
//!   `versionsort.suffix`), `--format` with the format language of
//!   [`crate::ref_filter`], `--color` (and `color.ui`), `--omit-empty`;
//! - `-v` for tags without a signature (prints the tag and fails).
//!
//! Not yet: signing and verifying signatures (`-s`, `-u`, `tag.gpgSign`,
//! `tag.forceSignAnnotated`; rgit #51), `--column` and `column.tag`,
//! `trailer.*` configuration for `--trailer`.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use rgit_commit::message::{self as cmsg, Cleanup};
use rgit_config::SystemEnv;
use rgit_config::color::ColorBool;
use rgit_date::DateContext;
use rgit_hash::ObjectId;
use rgit_ident::ident::{IdentConfig, Resolver, Role, Strictness};
use rgit_object::fsck::{self, Control, Severity};
use rgit_object::{Object, ObjectKind, SignatureRef, Tag};
use rgit_pathspec::wildmatch::{WildmatchFlags, wildmatch};
use rgit_refs::files::FilesStore;
use rgit_refs::name::{RefNameFlags, check_refname_format};
use rgit_refs::{IterItem, IterOptions, RefStore, RefUpdate, Target, Transaction};

use crate::ref_filter::{self, RefFormat, RefValues, RepoParts, SortKey};
use crate::messages::tag as msg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, Revs, UpdateHooks};
use crate::repo::Mode;
use crate::repository::Repo;

const TAG_PREFIX: &str = "refs/tags/";

/// A piece of the message in command-line order.
enum Piece {
    Message(String),
    File(String),
}

/// The reference filters of the listing mode.
#[derive(Default)]
struct Filters {
    contains: Vec<ObjectId>,
    no_contains: Vec<ObjectId>,
    merged: Vec<ObjectId>,
    no_merged: Vec<ObjectId>,
    points_at: Vec<ObjectId>,
}

impl Filters {
    fn by_commit(&self) -> bool {
        !(self.contains.is_empty()
            && self.no_contains.is_empty()
            && self.merged.is_empty()
            && self.no_merged.is_empty())
    }
}

fn fatal(s: impl Into<String>) -> Die {
    Die::Fatal(s.into())
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let cmdmode = Cell::new(0);
    let lines = Cell::new(-1);
    let annotate = Cell::new(false);
    let pieces: RefCell<Vec<Piece>> = RefCell::new(Vec::new());
    let trailers: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let edit = Cell::new(-1);
    let sign = Cell::new(-1);
    let cleanup: RefCell<Option<String>> = RefCell::new(None);
    let local_user: RefCell<Option<String>> = RefCell::new(None);
    let force = Cell::new(false);
    let create_reflog = Cell::new(false);
    let column: RefCell<Option<String>> = RefCell::new(None);
    let omit_empty = Cell::new(false);
    let sorts: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let no_sort = Cell::new(false);
    let format: RefCell<Option<String>> = RefCell::new(None);
    let ignore_case = Cell::new(false);
    let color: Cell<Option<ColorBool>> = Cell::new(None);

    // The filter options in command-line order; resolved once the
    // repository is set up (`None`: `--no-points-at`).
    let specs: RefCell<Vec<(u8, Option<String>)>> = RefCell::new(Vec::new());
    let filter_opt = |long: &'static str, help: &'static str, which: u8| {
        let specs = &specs;
        Opt::callback(None, long, Some("commit"), help, move |v, _| {
            specs
                .borrow_mut()
                .push((which, Some(v.unwrap_or("HEAD").to_owned())));
            Ok(())
        })
        .lastarg_default("HEAD")
        .noneg()
    };
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::cmdmode('l', "list", &cmdmode, msg::HELP_LIST, 'l' as i32),
            Opt::integer('n', None, &lines, msg::HELP_LINES).optional(Some("1")),
            Opt::cmdmode('d', "delete", &cmdmode, msg::HELP_DELETE, 'd' as i32),
            Opt::cmdmode('v', "verify", &cmdmode, msg::HELP_VERIFY, 'v' as i32),
            Opt::group(msg::GROUP_CREATE),
            Opt::flag('a', "annotate", &annotate, msg::HELP_ANNOTATE),
            Opt::callback('m', "message", Some("message"), msg::HELP_MESSAGE, |v, _| {
                pieces
                    .borrow_mut()
                    .push(Piece::Message(v.unwrap_or_default().to_owned()));
                Ok(())
            })
            .noneg(),
            Opt::callback('F', "file", Some("file"), msg::HELP_FILE, |v, unset| {
                if unset {
                    pieces.borrow_mut().retain(|p| !matches!(p, Piece::File(_)));
                } else {
                    pieces
                        .borrow_mut()
                        .push(Piece::File(v.unwrap_or_default().to_owned()));
                }
                Ok(())
            }),
            Opt::string_list(None, "trailer", &trailers, "trailer", msg::HELP_TRAILER),
            Opt::bool('e', "edit", &edit, msg::HELP_EDIT),
            Opt::bool('s', "sign", &sign, msg::HELP_SIGN),
            Opt::string(None, "cleanup", &cleanup, "mode", msg::HELP_CLEANUP),
            Opt::string('u', "local-user", &local_user, "key-id", msg::HELP_LOCAL_USER),
            Opt::flag('f', "force", &force, msg::HELP_FORCE),
            Opt::flag(None, "create-reflog", &create_reflog, msg::HELP_CREATE_REFLOG),
            Opt::group(msg::GROUP_LIST),
            Opt::callback(None, "column", Some("style"), msg::HELP_COLUMN, |v, unset| {
                *column.borrow_mut() = Some(if unset {
                    "never".to_owned()
                } else {
                    v.unwrap_or("always").to_owned()
                });
                Ok(())
            })
            .optional(None),
            filter_opt("contains", msg::HELP_CONTAINS, 0),
            filter_opt("no-contains", msg::HELP_NO_CONTAINS, 1),
            filter_opt("with", msg::HELP_CONTAINS, 0).hidden(),
            filter_opt("without", msg::HELP_NO_CONTAINS, 1).hidden(),
            filter_opt("merged", msg::HELP_MERGED, 2),
            filter_opt("no-merged", msg::HELP_NO_MERGED, 3),
            Opt::flag(None, "omit-empty", &omit_empty, msg::HELP_OMIT_EMPTY),
            Opt::callback(None, "sort", Some("key"), msg::HELP_SORT, |v, unset| {
                if unset {
                    sorts.borrow_mut().clear();
                    no_sort.set(true);
                } else {
                    sorts.borrow_mut().push(v.unwrap_or_default().to_owned());
                }
                Ok(())
            }),
            Opt::callback(None, "points-at", Some("object"), msg::HELP_POINTS_AT, |v, unset| {
                let v = (!unset).then(|| v.unwrap_or("HEAD").to_owned());
                specs.borrow_mut().push((4, v));
                Ok(())
            })
            .lastarg_default("HEAD"),
            Opt::string(None, "format", &format, "format", msg::HELP_FORMAT),
            Opt::callback(None, "color", Some("when"), msg::HELP_COLOR, |v, unset| {
                color.set(Some(crate::color::parse_when(v, unset)?));
                Ok(())
            })
            .optional(None),
            Opt::flag('i', "ignore-case", &ignore_case, msg::HELP_IGNORE_CASE),
        ],
    );
    let args = parser.parse(argv)?.args;
    let usage = parser.usage_error();
    drop(parser);

    let mut repo = Repo::open(ctx, Mode::Required)?;
    let setup = repo.setup.clone();
    let objects = Objects::open(&setup);
    let revs = Revs::new(ctx, &setup)?;
    let filters = resolve_filters(&specs.into_inner(), &objects, &revs)?;

    let mut mode = cmdmode.get() as u8;
    if mode == 0 {
        if args.is_empty() || filters.by_commit() || !filters.points_at.is_empty() || lines.get() != -1
        {
            mode = b'l';
        }
    }
    let message_given = !pieces.borrow().is_empty();
    if mode != 0
        && (annotate.get()
            || sign.get() == 1
            || message_given
            || force.get()
            || local_user.borrow().is_some()
            || !trailers.borrow().is_empty()
            || edit.get() == 1)
    {
        return Err(usage.into());
    }
    if mode != b'l' {
        if lines.get() != -1 {
            return Err(fatal(msg::only_in_list_mode("-n")));
        }
        for (set, opt) in [
            (!filters.contains.is_empty(), "--contains"),
            (!filters.no_contains.is_empty(), "--no-contains"),
            (!filters.points_at.is_empty(), "--points-at"),
            (!filters.merged.is_empty(), "--merged"),
            (!filters.no_merged.is_empty(), "--no-merged"),
        ] {
            if set {
                return Err(fatal(msg::only_in_list_mode(opt)));
            }
        }
    }
    {
        let p = pieces.borrow();
        if p.iter().any(|p| matches!(p, Piece::Message(_)))
            && p.iter().any(|p| matches!(p, Piece::File(_)))
        {
            return Err(fatal(msg::incompatible("-F", "-m")));
        }
    }
    let config = refs_support::load_config(ctx, &setup)?;
    let mut sort_specs = sorts.into_inner();
    if sort_specs.is_empty() && !no_sort.get() {
        sort_specs = config
            .entries()
            .iter()
            .filter(|e| e.name == "tag.sort")
            .filter_map(|e| e.value.as_ref().map(|v| lossy(v)))
            .collect();
    }
    let keys = ref_filter::parse_sort_keys(&sort_specs)?;

    let store = refs_support::ref_store(&setup, &config);
    match mode {
        b'l' => {
            let col = column.borrow().clone();
            if lines.get() != -1 && col.as_deref().is_some_and(|c| c != "never") {
                return Err(fatal(msg::incompatible("--column", "-n")));
            }
            if col.as_deref().is_some_and(|c| c != "never") {
                return Err(fatal(msg::COLUMN_NOT_SUPPORTED));
            }
            let list = List {
                objects: &objects,
                store: &store,
                revs: &revs,
                config: &config,
                setup: &setup,
                color: crate::color::want(&config, None, color.get())?,
                patterns: &args,
                icase: ignore_case.get(),
                filters: &filters,
                keys: &keys,
                lines: lines.get(),
                format: format.borrow().clone(),
                omit_empty: omit_empty.get(),
            };
            list.run()
        }
        b'd' => delete(&repo, &setup, &config, &store, &objects, &args),
        b'v' => verify(&repo, &store, &args, format.borrow().is_some()),
        _ => {
            if args.len() > 2 {
                return Err(fatal(msg::TOO_MANY_ARGUMENTS));
            }
            let given = if message_given {
                Some(read_message(&pieces.borrow(), &setup.original_cwd)?)
            } else {
                None
            };
            let implied = message_given || !trailers.borrow().is_empty() || edit.get() == 1;
            // `-s`/`-u`, else `tag.gpgSign`, else `tag.forceSignAnnotated`
            // for tags annotated without `-a`.
            let sign = local_user.borrow().is_some()
                || match sign.get() {
                    -1 => {
                        config_bool(&config, "tag.gpgsign") == Some(true)
                            || (implied
                                && !annotate.get()
                                && config_bool(&config, "tag.forcesignannotated") == Some(true))
                    }
                    s => s == 1,
                };
            let create = Create {
                name: &args[0],
                object: args.get(1).map_or("HEAD", String::as_str),
                annotate: annotate.get() || implied,
                sign,
                given,
                trailers: &trailers.borrow(),
                edit: edit.get() == 1,
                cleanup: cleanup.borrow().clone(),
                force: force.get(),
                create_reflog: create_reflog.get(),
            };
            create.run(ctx, &mut repo, &config, &store, &objects, &revs)
        }
    }
}

/// Resolves the filter options; `--contains` and friends name a commit
/// (tags are peeled).
fn resolve_filters(
    specs: &[(u8, Option<String>)],
    objects: &Objects,
    revs: &Revs,
) -> Result<Filters, Die> {
    let usage_error = |m: String| Die::Error(m, 129);
    let mut f = Filters::default();
    for (which, spec) in specs {
        let Some(name) = spec.as_deref() else {
            f.points_at.clear();
            continue;
        };
        if *which == 4 {
            let id = revs
                .resolve(name)
                .ok_or_else(|| usage_error(msg::malformed_object_name_quoted(name)))?;
            f.points_at.push(id);
            continue;
        }
        let Some(id) = revs.resolve(name) else {
            return Err(usage_error(msg::malformed_object_name(name)));
        };
        let peeled = objects.peel(&id).unwrap_or(id);
        let kind = objects.kind(&peeled);
        if kind != Some(ObjectKind::Commit) {
            if let Some(k) = kind {
                eprintln!("error: {}", msg::not_a_commit(&id.to_hex(), k.name()));
            }
            return Err(usage_error(match which {
                2 => msg::must_point_to_commit("merged"),
                3 => msg::must_point_to_commit("no-merged"),
                _ => msg::no_such_commit(name),
            }));
        }
        match which {
            0 => f.contains.push(peeled),
            1 => f.no_contains.push(peeled),
            2 => f.merged.push(peeled),
            _ => f.no_merged.push(peeled),
        }
    }
    Ok(f)
}

/// A listed tag.
struct Item {
    name: String,
    oid: ObjectId,
}

/// The listing mode.
struct List<'a> {
    objects: &'a Objects,
    store: &'a FilesStore,
    revs: &'a Revs,
    config: &'a rgit_config::ConfigSet,
    setup: &'a rgit_repo::Setup,
    color: bool,
    patterns: &'a [String],
    icase: bool,
    filters: &'a Filters,
    keys: &'a [SortKey],
    lines: i32,
    format: Option<String>,
    omit_empty: bool,
}

impl List<'_> {
    fn run(&self) -> CmdResult {
        let format = match &self.format {
            Some(f) => Some(RefFormat::parse(f)?),
            None => None,
        };
        if format.as_ref().is_some_and(RefFormat::has_rest) {
            return Err(fatal(msg::rejected_atom("rest")));
        }
        let mut values = self.values();
        values.prepare(
            format
                .iter()
                .flat_map(|f| f.atoms())
                .chain(self.keys.iter().map(|k| &k.atom)),
        )?;
        let iter = IterOptions {
            prefix: TAG_PREFIX.to_owned(),
            include_root_refs: false,
        };
        let listed = self
            .store
            .iter(&iter)
            .map_err(|e| fatal(e.to_string()))?;
        let mut items = Vec::new();
        for entry in listed {
            let IterItem::Ref(r) = entry else { continue };
            let Some(oid) = r.oid else { continue };
            let short = &r.name[TAG_PREFIX.len()..];
            if !self.patterns.is_empty() && !self.matches(short) {
                continue;
            }
            if !self.filters.points_at.is_empty() && !self.points_at(&oid) {
                continue;
            }
            if self.filters.by_commit() && !self.commit_filters(&oid) {
                continue;
            }
            items.push(Item { name: r.name, oid });
        }
        let items = self.sort(&values, items)?;

        let mut out = std::io::stdout().lock();
        for it in &items {
            let short = &it.name[TAG_PREFIX.len()..];
            let mut line = match &format {
                Some(f) => values.render(f, &it.name, it.oid)?,
                None if self.lines > 0 => self.with_lines(short, &it.oid),
                None => short.as_bytes().to_vec(),
            };
            if self.omit_empty && line.is_empty() {
                continue;
            }
            line.push(b'\n');
            let _ = out.write_all(&line);
        }
        let _ = out.flush();
        Ok(0)
    }

    /// Patterns are shell wildcards on the name below `refs/tags/`.
    fn matches(&self, short: &str) -> bool {
        let flags = if self.icase {
            WildmatchFlags::CASEFOLD
        } else {
            WildmatchFlags::NONE
        };
        self.patterns
            .iter()
            .any(|p| wildmatch(p.as_bytes(), short.as_bytes(), flags))
    }

    /// The tag or one of the tags it points to (in a chain of tags) names
    /// one of the `--points-at` objects.
    fn points_at(&self, oid: &ObjectId) -> bool {
        let mut cur = *oid;
        for _ in 0..64 {
            if self.filters.points_at.contains(&cur) {
                return true;
            }
            match self.objects.read(&cur) {
                Some((ObjectKind::Tag, data)) => {
                    match Tag::parse(self.objects.hash(), &data).ok().and_then(|t| t.object().ok()) {
                        Some(next) => cur = next,
                        None => return false,
                    }
                }
                _ => return false,
            }
        }
        false
    }

    /// `--[no-]contains` and `--[no-]merged`; only tags of commits pass.
    fn commit_filters(&self, oid: &ObjectId) -> bool {
        let Some(commit) = self.objects.peel(oid) else {
            return false;
        };
        if self.objects.kind(&commit) != Some(ObjectKind::Commit) {
            return false;
        }
        let f = self.filters;
        if !f.contains.is_empty() && !f.contains.iter().any(|c| self.revs.is_ancestor(c, &commit)) {
            return false;
        }
        if f.no_contains.iter().any(|c| self.revs.is_ancestor(c, &commit)) {
            return false;
        }
        if !f.merged.is_empty() && !f.merged.iter().any(|m| self.revs.is_ancestor(&commit, m)) {
            return false;
        }
        if f.no_merged.iter().any(|m| self.revs.is_ancestor(&commit, m)) {
            return false;
        }
        true
    }

    /// The atom values of this repository.
    fn values(&self) -> RefValues<'_> {
        RefValues::new(self.objects, self.store)
            .with_repo(RepoParts {
                setup: self.setup,
                config: self.config,
                revs: self.revs,
            })
            .with_color(self.color)
    }

    fn sort(&self, values: &RefValues<'_>, items: Vec<Item>) -> Result<Vec<Item>, Die> {
        let suffixes = if self.keys.iter().any(|k| k.version) {
            ref_filter::version_suffixes(self.config)
        } else {
            Vec::new()
        };
        let mut keyed = Vec::with_capacity(items.len());
        for it in items {
            let mut vals = Vec::with_capacity(self.keys.len());
            for k in self.keys {
                vals.push(values.atom_value(&k.atom, &it.name, it.oid)?);
            }
            keyed.push((vals, it));
        }
        Ok(ref_filter::sort_items(
            keyed,
            self.keys,
            self.icase,
            &suffixes,
            |it: &Item| it.name.as_str(),
        ))
    }

    /// `-n<num>`: the name padded to 15 columns and the first lines of
    /// the tag message (of the commit message for a lightweight tag).
    fn with_lines(&self, short: &str, oid: &ObjectId) -> Vec<u8> {
        let mut out = format!("{short:<15} ").into_bytes();
        let text = match self.objects.read(oid) {
            Some((ObjectKind::Tag, data)) => Tag::parse(self.objects.hash(), &data)
                .map(|t| t.message_and_signature().0.to_vec())
                .unwrap_or_default(),
            Some((ObjectKind::Commit, data)) => {
                rgit_object::Commit::parse(self.objects.hash(), &data)
                    .map(|c| c.message().to_vec())
                    .unwrap_or_default()
            }
            _ => Vec::new(),
        };
        let body = text.strip_suffix(b"\n").unwrap_or(&text);
        if body.is_empty() {
            return out;
        }
        let lines = body.split(|&b| b == b'\n').skip_while(|l| l.is_empty());
        for (n, l) in lines.take(self.lines as usize).enumerate() {
            if n > 0 {
                out.extend_from_slice(b"\n    ");
            }
            out.extend_from_slice(l);
        }
        out
    }
}

/// `-d`: delete the named tags.
fn delete(
    repo: &Repo,
    setup: &rgit_repo::Setup,
    config: &rgit_config::ConfigSet,
    store: &FilesStore,
    objects: &Objects,
    names: &[String],
) -> CmdResult {
    let mut code = 0;
    let mut found = Vec::new();
    for name in names {
        let refname = format!("{TAG_PREFIX}{name}");
        match store.read_raw(&refname) {
            Ok(Some(Target::Object(oid))) => found.push((name, refname, oid)),
            _ => {
                eprintln!("error: {}", msg::not_found(name));
                code = 1;
            }
        }
    }
    if found.is_empty() {
        return Ok(code);
    }
    let mut hooks = UpdateHooks::new(setup, config, objects);
    let mut tx = Transaction::new(store);
    for (_, refname, oid) in &found {
        tx.push(RefUpdate::delete(refname.as_str(), repo.hash, Some(*oid)).no_deref(true))
            .map_err(|e| fatal(e.to_string()))?;
    }
    if let Err(e) = tx.commit(&mut hooks) {
        eprintln!("error: {e}");
        return Ok(1);
    }
    let mut out = std::io::stdout().lock();
    for (name, _, oid) in &found {
        let _ = writeln!(out, "{}", msg::deleted(name, &repo.abbrev(oid, repo.default_abbrev())));
    }
    Ok(code)
}

/// `-v`: tags without a signature are printed and fail; signatures need
/// rgit #51.
fn verify(repo: &Repo, store: &FilesStore, names: &[String], formatted: bool) -> CmdResult {
    let mut code = 0;
    for name in names {
        let oid = match store.read_raw(&format!("{TAG_PREFIX}{name}")) {
            Ok(Some(Target::Object(oid))) => oid,
            _ => {
                eprintln!("error: {}", msg::not_found(name));
                code = 1;
                continue;
            }
        };
        let Some(obj) = repo.read_object(&oid) else {
            eprintln!("error: {}", msg::not_found(name));
            code = 1;
            continue;
        };
        let Object::Tag(tag) = obj else {
            eprintln!("error: {}", msg::cannot_verify_non_tag(name, obj.kind().name()));
            code = 1;
            continue;
        };
        if tag.message_and_signature().1.is_some() {
            return Err(fatal(msg::SIGNING_NOT_SUPPORTED));
        }
        if !formatted {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(&tag.serialize());
            let _ = out.flush();
        }
        eprintln!("error: {}", msg::NO_SIGNATURE);
        code = 1;
    }
    Ok(code)
}

/// Creating a tag.
struct Create<'a> {
    name: &'a str,
    object: &'a str,
    annotate: bool,
    sign: bool,
    given: Option<Vec<u8>>,
    trailers: &'a [String],
    edit: bool,
    cleanup: Option<String>,
    force: bool,
    create_reflog: bool,
}

impl Create<'_> {
    fn run(
        &self,
        ctx: &Context,
        repo: &mut Repo,
        config: &rgit_config::ConfigSet,
        store: &FilesStore,
        objects: &Objects,
        revs: &Revs,
    ) -> CmdResult {
        let target = revs
            .resolve(self.object)
            .ok_or_else(|| fatal(msg::failed_to_resolve(self.object)))?;
        let refname = format!("{TAG_PREFIX}{}", self.name);
        if self.name.starts_with('-')
            || self.name == "HEAD"
            || check_refname_format(refname.as_bytes(), RefNameFlags::STRICT).is_err()
        {
            return Err(fatal(msg::invalid_name(self.name)));
        }
        let existing = match store.read_raw(&refname) {
            Ok(Some(_)) => store.resolve_oid(&refname).ok().flatten(),
            _ => None,
        };
        if existing.is_some() && !self.force {
            return Err(fatal(msg::already_exists(self.name)));
        }
        let cleanup = match self.cleanup.as_deref() {
            None | Some("strip") => Cleanup::Strip,
            Some("whitespace") => Cleanup::Whitespace,
            Some("verbatim") => Cleanup::Verbatim,
            Some(m) => return Err(fatal(msg::invalid_cleanup(m))),
        };
        let kind = objects
            .kind(&target)
            .ok_or_else(|| fatal(msg::failed_to_resolve(self.object)))?;

        let git_dir = repo.setup.cwd.join(&repo.setup.git_dir);
        let editmsg = git_dir.join("TAG_EDITMSG");
        let mut edited = false;
        let new = if self.annotate || self.sign {
            if self.sign {
                return Err(fatal(msg::SIGNING_NOT_SUPPORTED));
            }
            let (oid, used_editor) =
                self.write_tag(ctx, repo, config, objects, target, kind, existing, cleanup, &editmsg)?;
            edited = used_editor;
            if kind == ObjectKind::Tag {
                let advisor = ctx.advisor(config)?;
                advisor
                    .advise(Advice::NestedTag, &msg::hint_nested_tag(self.name, self.object))
                    .map_err(Die::Fatal)?;
            }
            oid
        } else {
            target
        };

        let abbrev = repo.abbrev(&target, repo.default_abbrev());
        let what = match kind {
            ObjectKind::Commit => match repo.read_object(&target) {
                Some(Object::Commit(c)) => {
                    let subject = lossy(cmsg::first_line(c.message()));
                    let date = c
                        .committer()
                        .and_then(|s| s.time())
                        .and_then(|t| short_date(t.seconds))
                        .unwrap_or_default();
                    msg::reflog_commit(&subject, &date)
                }
                _ => String::new(),
            },
            ObjectKind::Tree => msg::REFLOG_TREE.to_owned(),
            ObjectKind::Blob => msg::REFLOG_BLOB.to_owned(),
            ObjectKind::Tag => msg::REFLOG_TAG.to_owned(),
        };
        let update = match existing {
            Some(old) => RefUpdate::update(refname.as_str(), new, Some(old)),
            None => RefUpdate::create(refname.as_str(), new),
        }
        .message(msg::reflog_tagging(&abbrev, &what))
        .create_reflog(self.create_reflog);
        let mut hooks = UpdateHooks::new(&repo.setup, config, objects);
        let mut tx = Transaction::new(store);
        tx.push(update)
            .and_then(|()| tx.commit(&mut hooks))
            .map_err(|e| fatal(e.to_string()))?;
        if edited {
            let _ = std::fs::remove_file(&editmsg);
        }
        if let Some(old) = existing {
            if old != new {
                println!("{}", msg::updated(self.name, &repo.abbrev(&old, repo.default_abbrev())));
            }
        }
        Ok(0)
    }

    /// Builds the message, writes the tag object and returns its id and
    /// whether the editor ran.
    #[allow(clippy::too_many_arguments)]
    fn write_tag(
        &self,
        ctx: &Context,
        repo: &Repo,
        config: &rgit_config::ConfigSet,
        objects: &Objects,
        target: ObjectId,
        kind: ObjectKind,
        existing: Option<ObjectId>,
        cleanup: Cleanup,
        editmsg: &Path,
    ) -> Result<(ObjectId, bool), Die> {
        let message_given = self.given.is_some();
        let mut message = self.given.clone().unwrap_or_default();
        let use_editor = !message_given || self.edit;
        let comment = comment_string(config);
        let mut reused = false;
        if !message_given {
            message = b"\n".to_vec();
            if let Some(old) = existing {
                if let Some((ObjectKind::Tag, data)) = objects.read(&old) {
                    if let Ok(t) = Tag::parse(objects.hash(), &data) {
                        message = t.message_and_signature().0.to_vec();
                        reused = true;
                    }
                }
            }
        }
        if !self.trailers.is_empty() {
            message = add_trailers(&message, self.trailers);
        }
        if use_editor {
            if !message_given && !reused {
                let text = if cleanup == Cleanup::Strip {
                    msg::template_strip(self.name, &comment)
                } else {
                    msg::template_keep(self.name, &comment)
                };
                for l in text.lines() {
                    message.extend_from_slice(comment.as_bytes());
                    if !l.is_empty() {
                        message.push(b' ');
                    }
                    message.extend_from_slice(l.as_bytes());
                    message.push(b'\n');
                }
            }
            std::fs::write(editmsg, &message).map_err(|e| fatal(e.to_string()))?;
            let env = SystemEnv::default();
            let editor = rgit_ident::editor::editor(config, &env).map_err(|p| {
                p.print();
                Die::Exit(rgit_options::EXIT_FATAL)
            })?;
            let advisor = ctx.advisor(config)?;
            let opts = rgit_ident::editor::LaunchOptions {
                hint: advisor
                    .enabled(Advice::WaitingForEditor)
                    .map_err(Die::Fatal)?,
                env: ctx.child_env(),
                cwd: None,
            };
            if let Err(e) = rgit_ident::editor::launch(editor.as_deref(), editmsg, &opts) {
                e.print();
                eprintln!("{}", msg::SUPPLY_MESSAGE);
                return Err(Die::Exit(1));
            }
            message = std::fs::read(editmsg).map_err(|e| fatal(e.to_string()))?;
        }
        let cleaned = cmsg::cleanup(&message, cleanup, &comment);
        if use_editor && !message_given && cleaned.is_empty() {
            return Err(fatal(msg::NO_TAG_MESSAGE));
        }

        let icfg = IdentConfig::from_config(config).map_err(|p| {
            p.print();
            Die::Exit(rgit_options::EXIT_FATAL)
        })?;
        let env = SystemEnv::default();
        let tagger = Resolver::new(&icfg, &env)
            .ident(Role::Committer, Strictness::Strict)
            .map_err(|e| {
                eprint!("{}", e.stderr_text());
                Die::Exit(rgit_options::EXIT_FATAL)
            })?;
        let raw = tagger.to_bstring();
        let tagger = SignatureRef::parse(&raw)
            .and_then(|s| s.to_owned())
            .ok_or_else(|| fatal(lossy(&raw)))?;
        let data = Tag::new(target, kind, self.name, Some(&tagger), cleaned).serialize();
        // The object must pass the checks of `git mktag`.
        let mut problem = None;
        fsck::check_tag(repo.hash, &data, &mut |f| {
            if matches!(f.id.default_severity(), Severity::Fatal | Severity::Error) {
                problem.get_or_insert_with(|| f.to_string());
            }
            Control::Continue
        });
        if let Some(p) = problem {
            return Err(fatal(p));
        }
        let oid = repo
            .odb()
            .write(ObjectKind::Tag, &data)
            .map_err(|e| fatal(e.to_string()))?;
        Ok((oid, use_editor))
    }
}

/// The message of `-m` (paragraphs) and `-F` (`-`: standard input),
/// read before anything else is checked.
fn read_message(pieces: &[Piece], cwd: &Path) -> Result<Vec<u8>, Die> {
    let mut message = Vec::new();
    for piece in pieces {
        match piece {
            Piece::Message(m) => {
                if !message.is_empty() {
                    message.push(b'\n');
                }
                message.extend_from_slice(m.as_bytes());
                if !message.is_empty() && !message.ends_with(b"\n") {
                    message.push(b'\n');
                }
            }
            Piece::File(f) => {
                if f == "-" {
                    std::io::stdin()
                        .read_to_end(&mut message)
                        .map_err(|e| fatal(msg::could_not_read_file(f, &crate::global::strerror(&e))))?;
                } else {
                    let path = Path::new(f);
                    let path: PathBuf = if path.is_absolute() {
                        path.to_path_buf()
                    } else {
                        cwd.join(path)
                    };
                    let data = std::fs::read(&path).map_err(|e| {
                        fatal(msg::could_not_read_file(f, &crate::global::strerror(&e)))
                    })?;
                    message.extend_from_slice(&data);
                }
            }
        }
    }
    Ok(message)
}

fn config_bool(config: &rgit_config::ConfigSet, key: &str) -> Option<bool> {
    config.bool(key).ok().flatten()
}

/// The comment string (`core.commentChar` / `core.commentString`, the
/// last one wins; `auto` falls back to `#` for a fresh template).
fn comment_string(config: &rgit_config::ConfigSet) -> String {
    let v = config
        .entries()
        .iter()
        .rev()
        .find(|e| e.name == "core.commentchar" || e.name == "core.commentstring")
        .and_then(|e| e.value.as_ref())
        .map(|v| lossy(v));
    match v.as_deref() {
        Some(s) if !s.is_empty() && s != "auto" => s.to_owned(),
        _ => "#".into(),
    }
}

/// `YYYY-MM-DD` of `secs` in UTC.
fn short_date(secs: i64) -> Option<String> {
    let mode = rgit_date::parse_date_mode("short", false).ok()?;
    rgit_date::show_date(secs.max(0) as u64, 0, &mode, &DateContext::from_env()).ok()
}

/// Whether `line` looks like a trailer (`<token>: <value>`).
fn is_trailer(line: &[u8]) -> bool {
    let Some(colon) = line.iter().position(|&b| b == b':') else {
        return false;
    };
    colon > 0
        && line[..colon]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
}

/// `--trailer <token>[(=|:)<value>]...`: appended to the trailer block at
/// the end of `message`, or as a new paragraph.
fn add_trailers(message: &[u8], trailers: &[String]) -> Vec<u8> {
    let mut lines = Vec::new();
    for t in trailers {
        let (token, value) = match t.find(['=', ':']) {
            Some(i) => (t[..i].trim(), t[i + 1..].trim()),
            None => (t.trim(), ""),
        };
        lines.push(format!("{token}: {value}\n"));
    }
    let trimmed_len = message
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(0, |p| p + 1);
    let mut out;
    if trimmed_len == 0 {
        out = message.to_vec();
    } else {
        let body = &message[..trimmed_len];
        let last_para = match body.windows(2).rposition(|w| w == b"\n\n") {
            Some(p) => &body[p + 2..],
            None => body,
        };
        let has_block = body.windows(2).any(|w| w == b"\n\n")
            && last_para.split(|&b| b == b'\n').all(is_trailer);
        out = body.to_vec();
        out.extend_from_slice(if has_block { b"\n" } else { b"\n\n" });
    }
    for l in lines {
        out.extend_from_slice(l.as_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(names: &[&str], suffixes: &[&str]) -> Vec<String> {
        let suffixes: Vec<Vec<u8>> = suffixes.iter().map(|s| s.as_bytes().to_vec()).collect();
        let mut v: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        v.sort_by(|a, b| ref_filter::versioncmp(a.as_bytes(), b.as_bytes(), &suffixes));
        v
    }

    #[test]
    fn numbers_compare_by_value() {
        assert_eq!(sorted(&["v1.10", "v1.3", "v1.6"], &[]), ["v1.3", "v1.6", "v1.10"]);
        assert_eq!(
            sorted(&["10", "9", "0", "09", "010", "01", "00", "000", "1"], &[]),
            ["000", "00", "01", "010", "09", "0", "1", "9", "10"]
        );
    }

    #[test]
    fn suffixes_order_releases() {
        assert_eq!(
            sorted(&["1.6", "1.6-rc2", "1.6-rc1", "1.3"], &["-rc"]),
            ["1.3", "1.6-rc1", "1.6-rc2", "1.6"]
        );
        assert_eq!(
            sorted(
                &["1.10-delta", "1.10-x", "1.10", "1.10-gamma", "1.10-beta", "1.10-alpha"],
                &["-alpha", "-beta", "", "-gamma", "-delta"]
            ),
            ["1.10-alpha", "1.10-beta", "1.10", "1.10-x", "1.10-gamma", "1.10-delta"]
        );
        assert_eq!(
            sorted(&["1.9-prerelease1", "1.9-pre2", "1.9-pre1"], &["-pre", "-prerelease"]),
            ["1.9-pre1", "1.9-pre2", "1.9-prerelease1"]
        );
        assert_eq!(
            sorted(&["1.8", "1.8-foo-bar", "1.8-foo-baz"], &["-bar", "-foo-baz", "-foo-bar"]),
            ["1.8-foo-baz", "1.8-foo-bar", "1.8"]
        );
    }

    #[test]
    fn trailers() {
        assert_eq!(add_trailers(b"msg\n", &["a=b".into()]), b"msg\n\na: b\n");
        assert_eq!(add_trailers(b"msg\n\nX: y\n", &["c:d".into()]), b"msg\n\nX: y\nc: d\n");
        assert_eq!(add_trailers(b"\n", &["e: f".into()]), b"\ne: f\n");
    }
}
