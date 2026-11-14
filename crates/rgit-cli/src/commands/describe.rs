// SPDX-License-Identifier: MIT
//! `git describe` (Documentation/git-describe.adoc): the nearest tag (or
//! reference) of a commit, `--contains` through name-rev, `--dirty` and
//! `--broken`, and blobs as `<commit-ish>:<path>`.

use std::collections::{HashMap, HashSet};
use std::io::Write;

use rgit_hash::ObjectId;
use rgit_object::{Commit, ObjectKind, Tag, TreeIter};
use rgit_pathspec::wildmatch::{WildmatchFlags, wildmatch};
use rgit_refs::{IterItem, IterOptions, RefStore};
use rgit_revwalk::describe::{self, NameUse};
use rgit_revwalk::revision::{Flags, ObjectLookup, Resolver};
use rgit_revwalk::walk::{Walk, WalkOptions};

use super::name_rev::{self, Source, bad_object, peel};
use crate::messages::describe as msg;
use crate::prelude::*;
use crate::refs_support::{self, Revs};
use crate::repo::{self, Mode};

const DEFAULT_CANDIDATES: i32 = 10;
const MIN_ABBREV: usize = 4;

/// A name found for an object.
struct Name {
    /// As printed (`v1.0`, or `tags/v1.0` with `--all`).
    path: String,
    /// 2 annotated tag, 1 lightweight tag, 0 other reference.
    prio: u8,
    /// The tag object of an annotated tag.
    tag: Option<ObjectId>,
}

struct Settings {
    all: bool,
    tags: bool,
    long: bool,
    first_parent: bool,
    debug: bool,
    always: bool,
    /// 0: no hex part; otherwise at least this many digits.
    abbrev: Option<usize>,
    candidates: usize,
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let contains = Cell::new(false);
    let debug = Cell::new(false);
    let all = Cell::new(false);
    let tags = Cell::new(false);
    let long = Cell::new(false);
    let first_parent = Cell::new(false);
    // None: default abbreviation; Some(0): none.
    let abbrev: Cell<Option<i64>> = Cell::new(None);
    let candidates = Cell::new(DEFAULT_CANDIDATES);
    let patterns = RefCell::new(Vec::new());
    let excludes = RefCell::new(Vec::new());
    let always = Cell::new(false);
    let dirty: RefCell<Option<String>> = RefCell::new(None);
    let broken: RefCell<Option<String>> = RefCell::new(None);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag(None, "contains", &contains, msg::HELP_CONTAINS),
            Opt::flag(None, "debug", &debug, msg::HELP_DEBUG),
            Opt::flag(None, "all", &all, msg::HELP_ALL),
            Opt::flag(None, "tags", &tags, msg::HELP_TAGS),
            Opt::flag(None, "long", &long, msg::HELP_LONG),
            Opt::flag(None, "first-parent", &first_parent, msg::HELP_FIRST_PARENT),
            Opt::callback(None, "abbrev", Some("n"), msg::HELP_ABBREV, |v, unset| {
                if unset {
                    abbrev.set(Some(0));
                    return Ok(());
                }
                match v {
                    None => abbrev.set(None),
                    Some(v) => match v.parse::<i64>() {
                        Ok(n) => abbrev.set(Some(n)),
                        Err(_) => {
                            return Err(CallbackError::Error(
                                crate::messages::abbrev_not_numeric("abbrev"),
                            ));
                        }
                    },
                }
                Ok(())
            })
            .optional(None),
            Opt::callback(None, "exact-match", None, msg::HELP_EXACT_MATCH, |_, unset| {
                candidates.set(if unset { DEFAULT_CANDIDATES } else { 0 });
                Ok(())
            })
            .noarg(),
            Opt::integer(None, "candidates", &candidates, msg::HELP_CANDIDATES),
            Opt::string_list(None, "match", &patterns, "pattern", msg::HELP_MATCH),
            Opt::string_list(None, "exclude", &excludes, "pattern", msg::HELP_EXCLUDE),
            Opt::flag(None, "always", &always, msg::HELP_ALWAYS),
            Opt::callback(None, "dirty", Some("mark"), msg::HELP_DIRTY, |v, unset| {
                *dirty.borrow_mut() = (!unset).then(|| v.unwrap_or("-dirty").to_owned());
                Ok(())
            })
            .optional(None),
            Opt::callback(None, "broken", Some("mark"), msg::HELP_BROKEN, |v, unset| {
                *broken.borrow_mut() = (!unset).then(|| v.unwrap_or("-broken").to_owned());
                Ok(())
            })
            .optional(None),
        ],
    );
    let mut args = parser.parse(argv)?.args;
    drop(parser);
    let abbrev = match abbrev.get() {
        None => None,
        Some(0) => Some(0),
        Some(n) => Some(usize::try_from(n).unwrap_or(0).max(MIN_ABBREV)),
    };
    if long.get() && abbrev == Some(0) {
        return Err(Die::Fatal(msg::incompatible("--long", "--abbrev=0")));
    }
    let patterns = patterns.take();
    let excludes = excludes.take();

    if contains.get() {
        let mut opts = name_rev::Options {
            name_only: true,
            tags: !all.get(),
            allow_undefined: false,
            always: always.get(),
            peel_tag: true,
            ..Default::default()
        };
        let kinds: &[&str] = if all.get() {
            &["refs/heads/", "refs/remotes/", "refs/tags/"]
        } else {
            &["refs/tags/"]
        };
        for p in &patterns {
            opts.refs.extend(kinds.iter().map(|k| format!("{k}{p}")));
        }
        for p in &excludes {
            opts.exclude.extend(kinds.iter().map(|k| format!("{k}{p}")));
        }
        if args.is_empty() {
            args.push("HEAD".into());
        }
        return name_rev::name_revs(ctx, &opts, &args);
    }

    let dirty = dirty.take();
    let broken = broken.take();
    if !args.is_empty() {
        if dirty.is_some() {
            return Err(Die::Fatal(msg::option_and_commitishes("--dirty")));
        }
        if broken.is_some() {
            return Err(Die::Fatal(msg::option_and_commitishes("--broken")));
        }
    }

    let setup = repo::setup(ctx, Mode::Required)?;
    let config = refs_support::load_config(ctx, &setup)?;
    let store = refs_support::ref_store(&setup, &config);
    let revs = Revs::new(ctx, &setup)?;
    let settings = Settings {
        all: all.get(),
        tags: tags.get(),
        long: long.get(),
        first_parent: first_parent.get(),
        debug: debug.get(),
        always: always.get(),
        abbrev,
        candidates: usize::try_from(candidates.get()).unwrap_or(0),
    };

    let suffix = if dirty.is_some() || broken.is_some() {
        work_tree_suffix(ctx, dirty.as_deref(), broken.as_deref())?
    } else {
        String::new()
    };
    if args.is_empty() {
        args.push("HEAD".into());
    }

    revs.with_resolver(|r| {
        let objects = r.repo().objects;
        let names = collect_names(objects, &store, &settings, &patterns, &excludes)?;
        if names.is_empty() && !settings.always {
            return Err(Die::Fatal(msg::NO_NAMES.to_owned()));
        }
        let d = Describer {
            r,
            objects,
            names: &names,
            s: &settings,
        };
        let mut out = std::io::stdout().lock();
        for arg in &args {
            let line = d.describe_arg(arg)?;
            let _ = writeln!(out, "{line}{suffix}");
            let _ = out.flush();
        }
        Ok(0)
    })
}

/// `--dirty`/`--broken`: refresh the index and compare the work tree with
/// `HEAD` through `update-index` and `diff-index` child processes.
fn work_tree_suffix(
    ctx: &Context,
    dirty: Option<&str>,
    broken: Option<&str>,
) -> Result<String, Die> {
    let exe = std::env::current_exe().map_err(|e| Die::Fatal(e.to_string()))?;
    let run = |args: &[&str]| -> Option<i32> {
        std::process::Command::new(&exe)
            .args(args)
            .envs(ctx.child_env())
            .status()
            .ok()
            .and_then(|s| s.code())
    };
    let dirty_mark = dirty.unwrap_or("-dirty").to_owned();
    let refreshed = run(&["update-index", "--unmerged", "-q", "--refresh"]);
    let diff = run(&["diff-index", "--quiet", "HEAD", "--"]);
    match broken {
        Some(mark) => Ok(match diff {
            Some(0) => String::new(),
            Some(1) => dirty_mark,
            _ => mark.to_owned(),
        }),
        None => {
            if !matches!(refreshed, Some(0 | 1)) {
                return Err(Die::Exit(refreshed.unwrap_or(rgit_messages::EXIT_FATAL)));
            }
            match diff {
                Some(0) => Ok(String::new()),
                Some(1) => Ok(dirty_mark),
                code => Err(Die::Exit(code.unwrap_or(rgit_messages::EXIT_FATAL))),
            }
        }
    }
}

/// The names describe may use, keyed by the (peeled) object they name.
fn collect_names(
    objects: &dyn ObjectLookup,
    store: &rgit_refs::files::FilesStore,
    s: &Settings,
    patterns: &[String],
    excludes: &[String],
) -> Result<HashMap<ObjectId, Name>, Die> {
    let listed = store
        .iter(&IterOptions {
            prefix: String::new(),
            include_root_refs: false,
        })
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let filtered = !patterns.is_empty() || !excludes.is_empty();
    let mut names: HashMap<ObjectId, Name> = HashMap::new();
    for item in listed {
        let IterItem::Ref(r) = item else { continue };
        let Some(oid) = r.oid else { continue };
        let refname = r.name.as_str();
        let is_tag = refname.starts_with("refs/tags/");
        if !s.all && !is_tag {
            continue;
        }
        let matched_path = ["refs/tags/", "refs/heads/", "refs/remotes/"]
            .iter()
            .find_map(|p| refname.strip_prefix(p));
        match matched_path {
            Some(path) => {
                let hit = |p: &String| wildmatch(p.as_bytes(), path.as_bytes(), WildmatchFlags::NONE);
                if excludes.iter().any(hit) {
                    continue;
                }
                if !patterns.is_empty() && !patterns.iter().any(hit) {
                    continue;
                }
            }
            None if filtered => continue,
            None => {}
        }
        let peeled = peel(objects, &oid).unwrap_or(oid);
        let annotated = peeled != oid;
        let prio = match (is_tag, annotated) {
            (true, true) => 2,
            (true, false) => 1,
            _ => 0,
        };
        let path = if s.all {
            refname.strip_prefix("refs/").unwrap_or(refname)
        } else {
            refname.strip_prefix("refs/tags/").unwrap_or(refname)
        };
        let new = Name {
            path: path.to_owned(),
            prio,
            tag: annotated.then_some(oid),
        };
        let replace = match names.get(&peeled) {
            None => true,
            Some(old) if old.prio < prio => true,
            Some(old) if old.prio == 2 && prio == 2 => {
                match tag_date(objects, old.tag.as_ref()) {
                    None => true,
                    Some(od) => tag_date(objects, new.tag.as_ref()).is_some_and(|nd| nd > od),
                }
            }
            _ => false,
        };
        if replace {
            names.insert(peeled, new);
        }
    }
    Ok(names)
}

fn read_tag(objects: &dyn ObjectLookup, id: Option<&ObjectId>) -> Option<Vec<u8>> {
    let (kind, data) = objects.read(id?).ok().flatten()?;
    (kind == ObjectKind::Tag).then_some(data)
}

/// Tagger date of a tag object; `None` if it cannot be read.
fn tag_date(objects: &dyn ObjectLookup, id: Option<&ObjectId>) -> Option<i64> {
    let data = read_tag(objects, id)?;
    let tag = Tag::parse(objects.hash_kind(), &data).ok()?;
    Some(tag.tagger().and_then(|s| s.time()).map_or(0, |t| t.seconds))
}

struct Describer<'a, 'r> {
    r: &'a Resolver<'r>,
    objects: &'a dyn ObjectLookup,
    names: &'a HashMap<ObjectId, Name>,
    s: &'a Settings,
}

impl Describer<'_, '_> {
    fn describe_arg(&self, arg: &str) -> Result<String, Die> {
        let flags = Flags {
            quiet: true,
            ..Flags::default()
        };
        let oid = self
            .r
            .resolve_with(arg, flags)
            .map_err(|_| Die::Fatal(msg::not_a_valid_object(arg)))?;
        let peeled = peel(self.objects, &oid);
        let kind = |id: &ObjectId| self.objects.kind(id).ok().flatten();
        if let Some(c) = peeled.filter(|p| kind(p) == Some(ObjectKind::Commit)) {
            return self.describe_commit(&c, arg);
        }
        if kind(&oid) == Some(ObjectKind::Blob) {
            return self.describe_blob(&oid);
        }
        Err(Die::Fatal(msg::neither_commit_nor_blob(arg)))
    }

    fn hex(&self, id: &ObjectId) -> String {
        let len = match self.s.abbrev {
            None => self.r.default_abbrev(),
            Some(0) => return id.to_hex(),
            Some(n) => n,
        };
        if len >= id.kind().hex_len() {
            return id.to_hex();
        }
        self.r
            .unique_abbrev(id, len)
            .unwrap_or_else(|_| id.to_hex_with_len(len))
    }

    /// The name to print for `n`, warning when an annotated tag's own name
    /// differs from its reference (such names always get the long form).
    fn display(&self, n: &Name) -> (String, bool) {
        if n.prio != 2 {
            return (n.path.clone(), false);
        }
        let own = read_tag(self.objects, n.tag.as_ref())
            .and_then(|d| {
                Tag::parse(self.objects.hash_kind(), &d)
                    .ok()
                    .and_then(|t| t.name().map(|b| String::from_utf8_lossy(b).into_owned()))
            });
        let Some(own) = own else {
            return (n.path.clone(), false);
        };
        let short = if self.s.all {
            n.path.strip_prefix("tags/").unwrap_or(&n.path)
        } else {
            n.path.as_str()
        };
        if own == short {
            return (n.path.clone(), false);
        }
        eprintln!("warning: {}", msg::externally_known(&n.path, &own));
        let shown = if self.s.all { format!("tags/{own}") } else { own };
        (shown, true)
    }

    fn format(&self, n: &Name, depth: usize, commit: &ObjectId) -> String {
        let (name, misnamed) = self.display(n);
        let long = misnamed || (self.s.abbrev != Some(0) && (depth > 0 || self.s.long));
        if !long {
            return name;
        }
        format!("{name}-{depth}-g{}", self.hex(commit))
    }

    fn describe_commit(&self, commit: &ObjectId, arg: &str) -> Result<String, Die> {
        let s = self.s;
        if s.debug {
            eprintln!("{}", msg::debug_describe(arg));
        }
        if let Some(n) = self.names.get(commit) {
            if s.tags || s.all || n.prio == 2 {
                return Ok(self.format(n, 0, commit));
            }
        }
        if s.candidates == 0 {
            return Err(Die::Fatal(msg::no_exact_match(&commit.to_hex())));
        }
        if s.debug {
            eprintln!("{}", msg::SEARCHING);
        }
        if self.names.is_empty() {
            return Ok(self.hex(commit));
        }
        let use_of = |id: &ObjectId| {
            self.names.get(id).map(|n| {
                if s.tags || s.all || n.prio == 2 {
                    NameUse::Usable {
                        annotated: n.prio == 2,
                    }
                } else {
                    NameUse::Unusable
                }
            })
        };
        let mut source = Source {
            objects: self.objects,
        };
        let out = describe::describe(
            &mut source,
            commit,
            use_of,
            describe::Options {
                max_candidates: s.candidates,
                first_parent: s.first_parent,
                total_names: self.names.len(),
            },
        )
        .map_err(bad_object)?;
        if out.candidates.is_empty() {
            if s.always {
                return Ok(self.hex(commit));
            }
            let hex = commit.to_hex();
            return Err(Die::Fatal(if out.unusable_seen > 0 {
                msg::no_annotated_tags(&hex)
            } else {
                msg::no_tags(&hex)
            }));
        }
        if s.debug {
            if let Some(f) = &out.finished_at {
                eprintln!("{}", msg::debug_finished(&f.to_hex()));
            }
            for c in &out.candidates {
                let n = &self.names[&c.commit];
                let kind = match n.prio {
                    2 => msg::KIND_ANNOTATED,
                    1 => msg::KIND_LIGHTWEIGHT,
                    _ => msg::KIND_HEAD,
                };
                eprintln!("{}", msg::debug_candidate(kind, c.depth, &n.path));
            }
            eprintln!("{}", msg::debug_traversed(out.traversed));
            if let Some(g) = &out.gave_up_at {
                let max = s.candidates.min(describe::MAX_CANDIDATES);
                eprintln!("{}", msg::debug_gave_up(max, &g.to_hex()));
            }
        }
        let best = &out.candidates[0];
        Ok(self.format(&self.names[&best.commit], best.depth, commit))
    }

    /// A blob as `<commit>:<path>`: the oldest commit (reverse walk from
    /// `HEAD`) whose tree holds it.
    fn describe_blob(&self, blob: &ObjectId) -> Result<String, Die> {
        let hex = blob.to_hex();
        let head = self.r.repo().refs.resolve("HEAD").ok().flatten();
        let Some(head) = head.and_then(|h| h.oid) else {
            return Err(Die::Fatal(msg::blob_unborn(&hex)));
        };
        let unreachable = || Die::Fatal(msg::blob_unreachable(&hex));
        let head = peel(self.objects, &head)
            .filter(|c| self.objects.kind(c).ok().flatten() == Some(ObjectKind::Commit))
            .ok_or_else(unreachable)?;
        let mut walk = Walk::new(
            Source {
                objects: self.objects,
            },
            WalkOptions {
                reverse: true,
                ..WalkOptions::default()
            },
        );
        walk.push(&head, false).map_err(bad_object)?;
        let mut seen_trees = HashSet::new();
        while let Some(c) = walk.next_commit().map_err(bad_object)? {
            let Some((_, data)) = self.objects.read(&c).ok().flatten() else {
                continue;
            };
            let Ok(tree) = Commit::parse(self.objects.hash_kind(), &data).and_then(|c| c.tree())
            else {
                continue;
            };
            if let Some(path) = self.find_blob(&tree, blob, "", &mut seen_trees) {
                let name = self.describe_commit(&c, &c.to_hex())?;
                return Ok(format!("{name}:{path}"));
            }
        }
        Err(unreachable())
    }

    fn find_blob(
        &self,
        tree: &ObjectId,
        blob: &ObjectId,
        prefix: &str,
        seen: &mut HashSet<ObjectId>,
    ) -> Option<String> {
        if !seen.insert(*tree) {
            return None;
        }
        let (kind, data) = self.objects.read(tree).ok().flatten()?;
        if kind != ObjectKind::Tree {
            return None;
        }
        for e in TreeIter::new(self.objects.hash_kind(), &data) {
            let Ok(e) = e else { break };
            let name = format!("{prefix}{}", String::from_utf8_lossy(e.name));
            if e.is_tree() {
                if let Some(p) = self.find_blob(&e.oid, blob, &format!("{name}/"), seen) {
                    return Some(p);
                }
            } else if e.oid == *blob {
                return Some(name);
            }
        }
        None
    }
}
