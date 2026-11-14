// SPDX-License-Identifier: MIT
//! `git name-rev` (Documentation/git-name-rev.adoc): names of commits
//! relative to references, for arguments, `--all` or text on stdin. Also
//! serves `git describe --contains`.

use std::collections::HashMap;
use std::io::{BufRead, Write};

use rgit_hash::ObjectId;
use rgit_object::{Commit, ObjectKind, Tag};
use rgit_pathspec::wildmatch::{WildmatchFlags, wildmatch};
use rgit_refs::files::FilesStore;
use rgit_refs::{IterItem, IterOptions, RefStore};
use rgit_revwalk::name_rev::{CUTOFF_SLOP, Namer, RevName, Tip};
use rgit_revwalk::revision::{Flags, ObjectLookup, Resolver};
use rgit_revwalk::walk::{CommitInfo, CommitSource};

use crate::messages::name_rev as msg;
use crate::prelude::*;
use crate::refs_support::{self, Revs};
use crate::repo::{self, Mode};

/// What to name and how (the options of `name-rev`).
#[derive(Debug, Default, Clone)]
pub struct Options {
    pub name_only: bool,
    pub tags: bool,
    pub refs: Vec<String>,
    pub exclude: Vec<String>,
    pub all: bool,
    pub annotate_stdin: bool,
    pub allow_undefined: bool,
    pub always: bool,
    pub peel_tag: bool,
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let name_only = Cell::new(false);
    let tags = Cell::new(false);
    let refs = RefCell::new(Vec::new());
    let exclude = RefCell::new(Vec::new());
    let all = Cell::new(false);
    let stdin = Cell::new(false);
    let annotate = Cell::new(false);
    let undefined = Cell::new(true);
    let always = Cell::new(false);
    let peel_tag = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag(None, "name-only", &name_only, msg::HELP_NAME_ONLY),
            Opt::flag(None, "tags", &tags, msg::HELP_TAGS),
            Opt::string_list(None, "refs", &refs, "pattern", msg::HELP_REFS),
            Opt::string_list(None, "exclude", &exclude, "pattern", msg::HELP_EXCLUDE),
            Opt::group(""),
            Opt::flag(None, "all", &all, msg::HELP_ALL),
            Opt::flag(None, "stdin", &stdin, msg::HELP_STDIN).hidden(),
            Opt::flag(None, "annotate-stdin", &annotate, msg::HELP_ANNOTATE_STDIN),
            Opt::flag(None, "undefined", &undefined, msg::HELP_UNDEFINED),
            Opt::flag(None, "always", &always, msg::HELP_ALWAYS),
            Opt::flag(None, "peel-tag", &peel_tag, msg::HELP_PEEL_TAG).hidden(),
        ],
    );
    let args = parser.parse(argv)?.args;
    if stdin.get() {
        eprintln!("warning: {}", msg::STDIN_DEPRECATED);
        annotate.set(true);
    }
    let modes =
        usize::from(all.get()) + usize::from(!args.is_empty()) + usize::from(annotate.get());
    if modes > 1 {
        return Err(parser.error_with_usage(msg::LIST_OR_ALL).into());
    }
    drop(parser);
    let opts = Options {
        name_only: name_only.get(),
        tags: tags.get(),
        refs: refs.take(),
        exclude: exclude.take(),
        all: all.get(),
        annotate_stdin: annotate.get(),
        allow_undefined: undefined.get(),
        always: always.get(),
        peel_tag: peel_tag.get(),
    };
    name_revs(ctx, &opts, &args)
}

/// Commits read from the object database for the naming walk.
pub(crate) struct Source<'a> {
    pub objects: &'a dyn ObjectLookup,
}

impl CommitSource for Source<'_> {
    type Error = ObjectId;
    fn load(&mut self, id: &ObjectId) -> Result<CommitInfo, ObjectId> {
        let (kind, data) = self.objects.read(id).ok().flatten().ok_or(*id)?;
        if kind != ObjectKind::Commit {
            return Err(*id);
        }
        let commit = Commit::parse(self.objects.hash_kind(), &data).map_err(|_| *id)?;
        let parents = if self.objects.is_shallow(id) {
            Vec::new()
        } else {
            commit.parents().map_err(|_| *id)?
        };
        let time = commit
            .committer()
            .and_then(|c| c.time())
            .map_or(0, |t| t.seconds);
        Ok(CommitInfo { parents, time })
    }
}

/// `fatal: bad object <id>` for an unreadable commit.
pub(crate) fn bad_object(id: ObjectId) -> Die {
    Die::Fatal(msg::bad_object(&id.to_hex()))
}

/// Follow tags to the first object that is not a tag (`None` if one on
/// the way is missing).
pub(crate) fn peel(objects: &dyn ObjectLookup, id: &ObjectId) -> Option<ObjectId> {
    let mut cur = *id;
    for _ in 0..64 {
        let (kind, data) = read(objects, &cur)?;
        if kind != ObjectKind::Tag {
            return Some(cur);
        }
        cur = Tag::parse(objects.hash_kind(), &data).ok()?.object().ok()?;
    }
    None
}

/// Where `pattern` matches `path` or one of its tails after a `/`: the
/// offset of the first matching tail.
fn subpath_match(path: &str, pattern: &str) -> Option<usize> {
    let mut starts = vec![0];
    starts.extend(path.match_indices('/').map(|(i, _)| i + 1));
    starts.into_iter().find(|&s| {
        wildmatch(
            pattern.as_bytes(),
            &path.as_bytes()[s..],
            WildmatchFlags::NONE,
        )
    })
}

/// The type and content of `id`.
fn read(objects: &dyn ObjectLookup, id: &ObjectId) -> Option<(ObjectKind, Vec<u8>)> {
    objects.read(id).ok().flatten()
}

/// Committer date of a commit, 0 if unreadable.
fn commit_date(objects: &dyn ObjectLookup, id: &ObjectId) -> i64 {
    read(objects, id)
        .and_then(|(_, d)| Commit::parse(objects.hash_kind(), &d).ok().map(|c| {
            c.committer().and_then(|s| s.time()).map_or(0, |t| t.seconds)
        }))
        .unwrap_or(0)
}

/// Names for every object a reference leads to.
struct Names {
    /// Names of non-commit objects (tag objects and what they point to
    /// when that is not a commit).
    objects: HashMap<ObjectId, RevName>,
    /// Tips in processing order.
    tips: Vec<Tip>,
    /// Objects as git would have loaded them, for the `--all` order.
    loaded: Vec<ObjectId>,
}

fn collect_tips(objects: &dyn ObjectLookup, store: &FilesStore, opts: &Options) -> Result<Names, Die> {
    let listed = store
        .iter(&IterOptions {
            prefix: String::new(),
            include_root_refs: false,
        })
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let mut names = Names {
        objects: HashMap::new(),
        tips: Vec::new(),
        loaded: Vec::new(),
    };
    for item in listed {
        let IterItem::Ref(r) = item else { continue };
        let Some(oid) = r.oid else { continue };
        names.loaded.push(oid);
        let refname = r.name.as_str();
        if opts.tags && !refname.starts_with("refs/tags/") {
            continue;
        }
        if opts.exclude.iter().any(|p| subpath_match(refname, p).is_some()) {
            continue;
        }
        let mut abbreviate = false;
        if !opts.refs.is_empty() {
            let mut matched = false;
            for p in &opts.refs {
                match subpath_match(refname, p) {
                    None => {}
                    Some(0) => matched = true,
                    Some(_) => {
                        matched = true;
                        abbreviate = true;
                    }
                }
            }
            if !matched {
                continue;
            }
        }
        let from_tag = refname.starts_with("refs/tags/");
        let shown = if abbreviate {
            shorten_unambiguous(store, refname)
        } else if let Some(s) = refname.strip_prefix("refs/heads/") {
            s.to_owned()
        } else {
            refname.strip_prefix("refs/").unwrap_or(refname).to_owned()
        };
        let mut cur = oid;
        let mut deref = false;
        let mut date = None;
        for _ in 0..64 {
            let Some((kind, data)) = read(objects, &cur) else {
                break;
            };
            match kind {
                ObjectKind::Tag => {
                    let Ok(tag) = Tag::parse(objects.hash_kind(), &data) else {
                        break;
                    };
                    let offered = RevName {
                        tip: shown.as_str().into(),
                        generation: 0,
                        distance: 0,
                        from_tag,
                        date: date.unwrap_or(0),
                        deref,
                    };
                    offer(&mut names.objects, cur, offered);
                    if date.is_none() {
                        date = Some(
                            tag.tagger()
                                .and_then(|s| s.time())
                                .map_or(0, |t| t.seconds),
                        );
                    }
                    let Ok(next) = tag.object() else { break };
                    cur = next;
                    deref = true;
                    names.loaded.push(cur);
                }
                ObjectKind::Commit => {
                    names.tips.push(Tip {
                        commit: cur,
                        name: shown.clone(),
                        from_tag,
                        date: date.unwrap_or_else(|| commit_date(objects, &cur)),
                        deref,
                    });
                    break;
                }
                _ => {
                    let offered = RevName {
                        tip: shown.as_str().into(),
                        generation: 0,
                        distance: 0,
                        from_tag,
                        date: date.unwrap_or(0),
                        deref,
                    };
                    offer(&mut names.objects, cur, offered);
                    break;
                }
            }
        }
    }
    Ok(names)
}

/// The shortest unambiguous short name of `refname` (`refs/heads/x` is
/// `x` unless `refs/x` or `refs/tags/x` exists, ...).
fn shorten_unambiguous(store: &FilesStore, refname: &str) -> String {
    const RULES: [(&str, &str); 6] = [
        ("", ""),
        ("refs/", ""),
        ("refs/tags/", ""),
        ("refs/heads/", ""),
        ("refs/remotes/", ""),
        ("refs/remotes/", "/HEAD"),
    ];
    for i in (1..RULES.len()).rev() {
        let (pre, suf) = RULES[i];
        let Some(short) = refname
            .strip_prefix(pre)
            .and_then(|s| s.strip_suffix(suf))
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let taken = RULES.iter().enumerate().any(|(j, (p, s))| {
            let other = format!("{p}{short}{s}");
            j != i && other != refname && matches!(store.read_raw(&other), Ok(Some(_)))
        });
        if !taken {
            return short.to_owned();
        }
    }
    refname.to_owned()
}

fn offer(map: &mut HashMap<ObjectId, RevName>, id: ObjectId, name: RevName) {
    match map.get(&id) {
        Some(old) if !name.better_than(old) => {}
        _ => {
            map.insert(id, name);
        }
    }
}

/// The name as printed: `tags/` dropped for `--tags --name-only`.
fn shown(opts: &Options, name: &RevName) -> String {
    let s = name.display();
    if opts.tags && opts.name_only {
        if let Some(rest) = s.strip_prefix("tags/") {
            return rest.to_owned();
        }
    }
    s
}

/// Run name-rev with `opts` on `args` (revision arguments).
pub fn name_revs(ctx: &Context, opts: &Options, args: &[String]) -> CmdResult {
    let setup = repo::setup(ctx, Mode::Required)?;
    let config = refs_support::load_config(ctx, &setup)?;
    let store = refs_support::ref_store(&setup, &config);
    let revs = Revs::new(ctx, &setup)?;
    // Commits read through a commit-graph file do not bring their trees
    // into git's object table, which changes the `--all` order.
    let info = rgit_odb::objects_dir(&setup.common_dir).join("info");
    let graph = info.join("commit-graph").exists() || info.join("commit-graphs").exists();
    let graph = graph
        && config
            .bool("core.commitgraph")
            .ok()
            .flatten()
            .unwrap_or(true);
    revs.with_resolver(|r| run_with(r, &store, opts, args, !graph))
}

fn run_with(
    r: &Resolver<'_>,
    store: &FilesStore,
    opts: &Options,
    args: &[String],
    with_trees: bool,
) -> CmdResult {
    let objects = r.repo().objects;
    // The arguments, resolved.
    let mut wanted: Vec<(String, ObjectId)> = Vec::new();
    let mut oldest: Option<i64> = None;
    for a in args {
        let flags = Flags {
            quiet: true,
            ..Flags::default()
        };
        let Ok(mut oid) = r.resolve_with(a, flags) else {
            eprintln!("{}", msg::could_not_get_sha1(a));
            continue;
        };
        if opts.peel_tag {
            if let Some(peeled) = peel(objects, &oid) {
                oid = peeled;
            }
        }
        if objects.kind(&oid).ok().flatten() == Some(ObjectKind::Commit) {
            let d = commit_date(objects, &oid);
            oldest = Some(oldest.map_or(d, |o: i64| o.min(d)));
        }
        wanted.push((a.clone(), oid));
    }
    let cutoff = if opts.all || opts.annotate_stdin {
        i64::MIN
    } else {
        match oldest {
            Some(d) if d > CUTOFF_SLOP => d - CUTOFF_SLOP,
            Some(_) => i64::MIN,
            None => i64::MAX,
        }
    };

    let names = collect_tips(objects, store, opts)?;
    let mut source = Source { objects };
    let mut namer = Namer::new(&mut source, cutoff);
    // Every reference's commit is read, named or not.
    for id in &names.loaded {
        if objects.kind(id).ok().flatten() == Some(ObjectKind::Commit) {
            namer.load(id).map_err(bad_object)?;
        }
    }
    for tip in &names.tips {
        namer.name_tip(tip).map_err(bad_object)?;
    }
    let lookup = |id: &ObjectId| -> Option<RevName> {
        namer
            .get(id)
            .cloned()
            .or_else(|| names.objects.get(id).cloned())
    };

    let mut out = std::io::stdout().lock();
    if opts.annotate_stdin {
        let hexlen = objects.hash_kind().hex_len();
        let stdin = std::io::stdin();
        let mut line = Vec::new();
        let mut input = stdin.lock();
        loop {
            line.clear();
            match input.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let annotated = annotate_line(&line, hexlen, objects, |id| {
                lookup(id).map(|n| shown(opts, &n))
            }, opts.name_only);
            let _ = out.write_all(&annotated);
            let _ = out.flush();
        }
        return Ok(0);
    }
    if opts.all {
        let order = all_order(&names.loaded, &namer.order, objects, with_trees);
        for id in order {
            let name = lookup(&id);
            let mut line = String::new();
            if !opts.name_only {
                line.push_str(&id.to_hex());
                line.push(' ');
            }
            let _ = out.write_all(line.as_bytes());
            print_name(&mut out, r, opts, &id, name)?;
        }
        let _ = out.flush();
        return Ok(0);
    }
    for (arg, id) in &wanted {
        if !opts.name_only {
            let _ = write!(out, "{arg} ");
        }
        print_name(&mut out, r, opts, id, lookup(id))?;
    }
    let _ = out.flush();
    Ok(0)
}

/// Print the name of `id` (or the fallback) and a newline.
fn print_name(
    out: &mut impl Write,
    r: &Resolver<'_>,
    opts: &Options,
    id: &ObjectId,
    name: Option<RevName>,
) -> Result<(), Die> {
    let text = match name {
        Some(n) => shown(opts, &n),
        None if opts.allow_undefined => msg::UNDEFINED.to_owned(),
        None if opts.always => r
            .unique_abbrev(id, r.default_abbrev())
            .unwrap_or_else(|_| id.to_hex()),
        None => {
            let _ = out.flush();
            return Err(Die::Fatal(msg::cannot_describe(&id.to_hex())));
        }
    };
    let _ = writeln!(out, "{text}");
    Ok(())
}

/// Replace every run of exactly `hexlen` lowercase hex digits that names a
/// known object by `<hex> (<name>)` (or the name alone).
fn annotate_line(
    line: &[u8],
    hexlen: usize,
    objects: &dyn ObjectLookup,
    name_of: impl Fn(&ObjectId) -> Option<String>,
    name_only: bool,
) -> Vec<u8> {
    let is_hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
    let mut out = Vec::with_capacity(line.len());
    let mut i = 0;
    while i < line.len() {
        if !is_hex(line[i]) {
            out.push(line[i]);
            i += 1;
            continue;
        }
        let start = i;
        while i < line.len() && is_hex(line[i]) {
            i += 1;
        }
        let run = &line[start..i];
        let name = (run.len() == hexlen)
            .then(|| ObjectId::from_hex(objects.hash_kind(), run).ok())
            .flatten()
            .and_then(|id| name_of(&id));
        match name {
            Some(n) if name_only => out.extend_from_slice(n.as_bytes()),
            Some(n) => {
                out.extend_from_slice(run);
                out.extend_from_slice(b" (");
                out.extend_from_slice(n.as_bytes());
                out.push(b')');
            }
            None => out.extend_from_slice(run),
        }
    }
    out
}

/// The commits `--all` lists, in the order git's object table holds them:
/// slots by the first four bytes of the id (little endian), linear probing,
/// the table doubling from 32 entries whenever it would be half full.
/// `read` is the order the commits were read; each read commit also brings
/// its parents (and, without a commit-graph, its tree) into the table.
fn all_order(
    refs: &[ObjectId],
    read: &[ObjectId],
    objects: &dyn ObjectLookup,
    with_trees: bool,
) -> Vec<ObjectId> {
    let mut inserted: Vec<(ObjectId, bool)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut add = |id: ObjectId, commit: bool, inserted: &mut Vec<(ObjectId, bool)>| {
        if seen.insert(id) {
            inserted.push((id, commit));
        }
    };
    for id in refs.iter().chain(read) {
        let obj = read_obj(objects, id);
        let is_commit = matches!(obj, Some((ObjectKind::Commit, _)));
        add(*id, is_commit, &mut inserted);
        if let Some((ObjectKind::Commit, data)) = obj {
            if let Ok(c) = Commit::parse(objects.hash_kind(), &data) {
                if let Some(t) = c.tree().ok().filter(|_| with_trees) {
                    add(t, false, &mut inserted);
                }
                for p in c.parents().unwrap_or_default() {
                    add(p, true, &mut inserted);
                }
            }
        }
    }
    let mut table: Vec<Option<(ObjectId, bool)>> = Vec::new();
    let mut count = 0usize;
    let slot = |id: &ObjectId, size: usize| -> usize {
        let b = id.as_bytes();
        u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize & (size - 1)
    };
    let place = |table: &mut Vec<Option<(ObjectId, bool)>>, e: (ObjectId, bool)| {
        let size = table.len();
        let mut i = slot(&e.0, size);
        while table[i].is_some() {
            i = (i + 1) & (size - 1);
        }
        table[i] = Some(e);
    };
    for e in inserted {
        if table.len() <= count * 2 + 1 {
            let size = if table.len() < 32 { 32 } else { table.len() * 2 };
            let old = std::mem::replace(&mut table, vec![None; size]);
            for o in old.into_iter().flatten() {
                place(&mut table, o);
            }
        }
        place(&mut table, e);
        count += 1;
    }
    table
        .into_iter()
        .flatten()
        .filter(|(_, commit)| *commit)
        .map(|(id, _)| id)
        .collect()
}

fn read_obj(objects: &dyn ObjectLookup, id: &ObjectId) -> Option<(ObjectKind, Vec<u8>)> {
    read(objects, id)
}
