// SPDX-License-Identifier: MIT
//! `git for-each-ref` (Documentation/git-for-each-ref.adoc,
//! for-each-ref-options.adoc, ref-reachability-filters.adoc): patterns,
//! `--exclude`, `--stdin`, `--include-root-refs`, `--start-after`, the
//! reachability filters `--points-at`, `--[no-]merged`,
//! `--[no-]contains`, `--sort` (several keys, `version:`), `--count`,
//! `--color`, quoting styles and the format language of
//! [`crate::ref_filter`].

use std::collections::{HashMap, HashSet};
use std::io::Write;

use rgit_config::color::ColorBool;
use rgit_hash::ObjectId;
use rgit_object::ObjectKind;
use rgit_pathspec::wildmatch::{WildmatchFlags, wildmatch};
use rgit_refs::{IterItem, IterOptions, RefStore, Target};

use crate::messages::for_each_ref as msg;
use crate::messages::ref_filter as rmsg;
use crate::prelude::*;
use crate::ref_filter::{
    self, Env, FormatError, ItemCache, Quote, RefFormat, RefItem, RepoParts, SortKey,
};
use crate::refs_support::{self, Objects, Revs};
use crate::repo::{self, Mode};

/// Which reachability filter an option sets.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Which {
    Merged,
    NoMerged,
    Contains,
    NoContains,
}

/// The reachability filters, resolved to commits.
#[derive(Default)]
struct Filters {
    points_at: Vec<ObjectId>,
    merged: Vec<ObjectId>,
    no_merged: Vec<ObjectId>,
    contains: Vec<ObjectId>,
    no_contains: Vec<ObjectId>,
}

impl Filters {
    fn by_commit(&self) -> bool {
        !(self.merged.is_empty()
            && self.no_merged.is_empty()
            && self.contains.is_empty()
            && self.no_contains.is_empty())
    }
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let quote = Cell::new(Quote::None);
    let quote_count = Cell::new(0);
    let omit_empty = Cell::new(false);
    let count = Cell::new(0);
    let format: RefCell<Option<String>> = RefCell::new(None);
    let start_after: RefCell<Option<String>> = RefCell::new(None);
    let excludes: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let sorts: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let points_at: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let reach: RefCell<Vec<(Which, String)>> = RefCell::new(Vec::new());
    let color: Cell<Option<ColorBool>> = Cell::new(None);
    let ignore_case = Cell::new(false);
    let stdin = Cell::new(false);
    let root_refs = Cell::new(false);
    let quote_opt = |short: Option<char>, long: &'static str, q: Quote, help: &'static str| {
        let quote = &quote;
        let quote_count = &quote_count;
        Opt::callback(short, long, None, help, move |_, unset| {
            if unset {
                quote.set(Quote::None);
            } else {
                quote.set(q);
                quote_count.set(quote_count.get() + 1);
            }
            Ok(())
        })
        .noarg()
    };
    let reach_opt = |long: &'static str, which: Which, help: &'static str| {
        let reach = &reach;
        Opt::callback(None, long, Some("commit"), help, move |v, _| {
            reach
                .borrow_mut()
                .push((which, v.unwrap_or("HEAD").to_owned()));
            Ok(())
        })
        .lastarg_default("HEAD")
        .noneg()
    };
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            quote_opt(Some('s'), "shell", Quote::Shell, msg::HELP_SHELL),
            quote_opt(Some('p'), "perl", Quote::Perl, msg::HELP_PERL),
            quote_opt(None, "python", Quote::Python, msg::HELP_PYTHON),
            quote_opt(None, "tcl", Quote::Tcl, msg::HELP_TCL),
            Opt::flag(None, "omit-empty", &omit_empty, msg::HELP_OMIT_EMPTY),
            Opt::group(""),
            Opt::integer(None, "count", &count, msg::HELP_COUNT),
            Opt::string(None, "format", &format, "format", msg::HELP_FORMAT),
            Opt::string(
                None,
                "start-after",
                &start_after,
                "marker",
                msg::HELP_START_AFTER,
            ),
            Opt::callback(None, "color", Some("when"), msg::HELP_COLOR, |v, unset| {
                color.set(Some(crate::color::parse_when(v, unset)?));
                Ok(())
            })
            .optional(None),
            Opt::string_list(None, "exclude", &excludes, "pattern", msg::HELP_EXCLUDE),
            Opt::string_list(None, "sort", &sorts, "key", msg::HELP_SORT),
            Opt::string_list(None, "points-at", &points_at, "object", msg::HELP_POINTS_AT),
            reach_opt("merged", Which::Merged, msg::HELP_MERGED),
            reach_opt("no-merged", Which::NoMerged, msg::HELP_NO_MERGED),
            reach_opt("contains", Which::Contains, msg::HELP_CONTAINS),
            reach_opt("no-contains", Which::NoContains, msg::HELP_NO_CONTAINS),
            Opt::flag(None, "ignore-case", &ignore_case, msg::HELP_IGNORE_CASE),
            Opt::flag(None, "stdin", &stdin, msg::HELP_STDIN),
            Opt::flag(
                None,
                "include-root-refs",
                &root_refs,
                msg::HELP_INCLUDE_ROOT_REFS,
            ),
        ],
    );
    let args = parser.parse(argv)?.args;
    let max = match count.get() {
        n if n < 0 => {
            return Err(parser
                .error_with_usage(&msg::invalid_count(&n.to_string()))
                .into());
        }
        0 => None,
        n => Some(n as usize),
    };
    if quote_count.get() > 1 {
        return Err(parser.error_with_usage(msg::MORE_THAN_ONE_QUOTING).into());
    }
    let fmt = format
        .borrow()
        .clone()
        .unwrap_or_else(|| msg::DEFAULT_FORMAT.to_owned());
    let fmt = match RefFormat::try_parse(&fmt) {
        Ok(f) => f,
        Err(FormatError::Malformed(rest)) => {
            return Err(parser
                .error_with_usage(&rmsg::malformed_format(&rest))
                .into());
        }
        Err(FormatError::Fatal(m)) => return Err(Die::Fatal(m)),
    };
    drop(parser);
    let quote = quote.get();
    if fmt.has_raw() && matches!(quote, Quote::Shell | Quote::Python | Quote::Tcl) {
        return Err(Die::Fatal(rmsg::RAW_QUOTED.to_owned()));
    }
    if fmt.has_rest() {
        return Err(Die::Fatal(rmsg::rejected_atom("rest")));
    }
    let custom_sort = !sorts.borrow().is_empty();
    let keys = if custom_sort {
        ref_filter::parse_sort_keys(&sorts.borrow())?
    } else {
        vec![SortKey::parse("refname")?]
    };

    let mut patterns = args.clone();
    if stdin.get() {
        if !args.is_empty() {
            return Err(Die::Fatal(msg::STDIN_AND_PATTERNS.to_owned()));
        }
        let input = std::io::stdin();
        let mut line = String::new();
        while std::io::BufRead::read_line(&mut input.lock(), &mut line).unwrap_or(0) > 0 {
            patterns.push(line.trim_end_matches('\n').to_owned());
            line.clear();
        }
    }
    let start_after = start_after.borrow().clone();
    if start_after.is_some() {
        if !patterns.is_empty() {
            return Err(Die::Fatal(msg::START_AFTER_PATTERNS.to_owned()));
        }
        if custom_sort {
            return Err(Die::Fatal(msg::START_AFTER_SORT.to_owned()));
        }
    }

    let setup = repo::setup(ctx, Mode::Required)?;
    let config = refs_support::load_config(ctx, &setup)?;
    let store = refs_support::ref_store(&setup, &config);
    let objects = Objects::open(&setup);
    let revs = Revs::new(ctx, &setup)?;

    let filters = resolve_filters(&points_at.borrow(), &reach.borrow(), &objects, &revs)?;

    let mut env = Env::new(&objects, &store);
    env.repo = Some(RepoParts {
        setup: &setup,
        config: &config,
        revs: &revs,
    });
    env.quote = quote;
    env.color = match color.get() {
        Some(ColorBool::Always) => true,
        Some(ColorBool::Never) => false,
        Some(ColorBool::Auto) | None => crate::color::auto(&config),
    };
    env.prepare(fmt.atoms().chain(keys.iter().map(|k| &k.atom)))?;

    let iter_opts = IterOptions {
        prefix: String::new(),
        include_root_refs: root_refs.get(),
    };
    let listed = store
        .iter(&iter_opts)
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let icase = ignore_case.get();
    let excludes = excludes.borrow();
    let mut reach = Reach::new(&objects, &filters);
    let mut items = Vec::new();
    for entry in listed {
        let r = match entry {
            IterItem::Ref(r) => r,
            IterItem::Broken(n) | IterItem::BadName(n) => {
                if patterns.is_empty() || matches_any(&patterns, &n, icase) {
                    eprintln!("warning: {}", msg::ignoring_broken_ref(&n));
                }
                continue;
            }
        };
        let Some(oid) = r.oid else { continue };
        // Special refs are not root refs (gitglossary(7), "special ref").
        if matches!(r.name.as_str(), "FETCH_HEAD" | "MERGE_HEAD") {
            continue;
        }
        if oid.is_null() {
            if patterns.is_empty() || matches_any(&patterns, &r.name, icase) {
                eprintln!("warning: {}", msg::ignoring_broken_ref(&r.name));
            }
            continue;
        }
        if !patterns.is_empty() && !matches_any(&patterns, &r.name, icase) {
            continue;
        }
        if matches_any(&excludes, &r.name, icase) {
            continue;
        }
        if let Some(marker) = start_after.as_deref() {
            if r.name.as_bytes() <= marker.as_bytes() {
                continue;
            }
        }
        if !filters.points_at.is_empty() && !points_at_any(&objects, &oid, &filters.points_at) {
            continue;
        }
        if filters.by_commit() && !reach.passes(&oid)? {
            continue;
        }
        let symref = match &r.target {
            Target::Symbolic(t) => Some(t.clone()),
            Target::Object(_) => None,
        };
        items.push(RefItem {
            name: r.name,
            oid,
            symref,
        });
    }

    let mut keyed = Vec::with_capacity(items.len());
    for it in items {
        let cache = ItemCache::default();
        let mut vals = Vec::with_capacity(keys.len());
        for k in &keys {
            vals.push(env.value(&k.atom, &it, &cache)?);
        }
        keyed.push((vals, (it, cache)));
    }
    let suffixes = if keys.iter().any(|k| k.version) {
        ref_filter::version_suffixes(&config)
    } else {
        Vec::new()
    };
    let items = ref_filter::sort_items(keyed, &keys, icase, &suffixes, |(it, _)| it.name.as_str());

    let mut out = std::io::stdout().lock();
    for (n, (it, cache)) in items.iter().enumerate() {
        if max.is_some_and(|m| n >= m) {
            break;
        }
        let mut line = env.render(&fmt.pieces, it, cache, fmt.reset_at_eol)?;
        if omit_empty.get() && line.is_empty() {
            continue;
        }
        line.push(b'\n');
        let _ = out.write_all(&line);
    }
    let _ = out.flush();
    Ok(0)
}

/// Resolves `--points-at` (any object) and the reachability filters (a
/// commit; tags are peeled).
fn resolve_filters(
    points_at: &[String],
    reach: &[(Which, String)],
    objects: &Objects,
    revs: &Revs,
) -> Result<Filters, Die> {
    let mut f = Filters::default();
    for p in points_at {
        let id = revs
            .resolve(p)
            .ok_or_else(|| Die::Error(rmsg::malformed_object_name_quoted(p), 129))?;
        f.points_at.push(id);
    }
    for (which, name) in reach {
        let Some(id) = revs.resolve(name) else {
            return Err(match which {
                Which::Merged | Which::NoMerged => Die::Fatal(rmsg::malformed_object_name(name)),
                _ => Die::Error(rmsg::malformed_object_name(name), 129),
            });
        };
        let peeled = objects.peel(&id).unwrap_or(id);
        let kind = objects.kind(&peeled);
        if kind != Some(ObjectKind::Commit) {
            if let Some(k) = kind {
                eprintln!("error: {}", rmsg::not_a_commit(&id.to_hex(), k.name()));
            }
            return Err(Die::Error(
                match which {
                    Which::Merged => rmsg::must_point_to_commit("merged"),
                    Which::NoMerged => rmsg::must_point_to_commit("no-merged"),
                    _ => rmsg::no_such_commit(name),
                },
                129,
            ));
        }
        match which {
            Which::Merged => f.merged.push(peeled),
            Which::NoMerged => f.no_merged.push(peeled),
            Which::Contains => f.contains.push(peeled),
            Which::NoContains => f.no_contains.push(peeled),
        }
    }
    Ok(f)
}

/// The reference or one of the tags it points to (in a chain of tags)
/// names one of `points`, or the object at the end of the chain does.
fn points_at_any(objects: &Objects, oid: &ObjectId, points: &[ObjectId]) -> bool {
    let mut cur = *oid;
    for _ in 0..64 {
        if points.contains(&cur) {
            return true;
        }
        match objects.read(&cur) {
            Some((ObjectKind::Tag, data)) => {
                match rgit_object::Tag::parse(objects.hash(), &data)
                    .ok()
                    .and_then(|t| t.object().ok())
                {
                    Some(next) => cur = next,
                    None => return false,
                }
            }
            _ => return false,
        }
    }
    false
}

/// Reachability for `--[no-]contains` and `--[no-]merged`, with the
/// answers for every commit seen kept across references.
struct Reach<'a> {
    objects: &'a Objects,
    contains: Memo,
    no_contains: Memo,
    /// Commits reachable from the `--merged` commits.
    merged: Option<HashSet<ObjectId>>,
    no_merged: Option<HashSet<ObjectId>>,
}

/// Whether commits reach one of `targets`.
struct Memo {
    targets: Vec<ObjectId>,
    known: HashMap<ObjectId, bool>,
}

fn parents(objects: &Objects, id: &ObjectId) -> Option<Vec<ObjectId>> {
    match objects.read(id)? {
        (ObjectKind::Commit, data) => rgit_object::Commit::parse(objects.hash(), &data)
            .ok()?
            .parents()
            .ok(),
        _ => None,
    }
}

impl Memo {
    fn new(targets: &[ObjectId]) -> Memo {
        Memo {
            targets: targets.to_vec(),
            known: HashMap::new(),
        }
    }

    /// Whether `tip` reaches a target; `Err` names a commit that cannot be
    /// read.
    fn reaches(&mut self, objects: &Objects, tip: ObjectId) -> Result<bool, ObjectId> {
        let mut stack: Vec<(ObjectId, Option<Vec<ObjectId>>)> = vec![(tip, None)];
        while let Some((id, ps)) = stack.pop() {
            if self.known.contains_key(&id) {
                continue;
            }
            if self.targets.contains(&id) {
                self.known.insert(id, true);
                continue;
            }
            match ps {
                None => {
                    let ps = parents(objects, &id).ok_or(id)?;
                    let pending: Vec<ObjectId> = ps
                        .iter()
                        .filter(|p| !self.known.contains_key(p))
                        .copied()
                        .collect();
                    stack.push((id, Some(ps)));
                    stack.extend(pending.into_iter().map(|p| (p, None)));
                }
                Some(ps) => {
                    let v = ps.iter().any(|p| self.known.get(p) == Some(&true));
                    self.known.insert(id, v);
                }
            }
        }
        Ok(self.known.get(&tip) == Some(&true))
    }
}

/// Every commit reachable from `tips` (unreadable commits end the walk).
fn ancestors(objects: &Objects, tips: &[ObjectId]) -> HashSet<ObjectId> {
    let mut seen = HashSet::new();
    let mut todo = tips.to_vec();
    while let Some(id) = todo.pop() {
        if !seen.insert(id) {
            continue;
        }
        todo.extend(parents(objects, &id).unwrap_or_default());
    }
    seen
}

impl<'a> Reach<'a> {
    fn new(objects: &'a Objects, f: &Filters) -> Reach<'a> {
        Reach {
            objects,
            contains: Memo::new(&f.contains),
            no_contains: Memo::new(&f.no_contains),
            merged: (!f.merged.is_empty()).then(|| ancestors(objects, &f.merged)),
            no_merged: (!f.no_merged.is_empty()).then(|| ancestors(objects, &f.no_merged)),
        }
    }

    /// Whether the ref at `oid` passes; only refs to commits (after
    /// peeling tags) do.
    fn passes(&mut self, oid: &ObjectId) -> Result<bool, Die> {
        let Some(commit) = self.objects.peel(oid) else {
            return Ok(false);
        };
        if self.objects.kind(&commit) != Some(ObjectKind::Commit) {
            return Ok(false);
        }
        let objects = self.objects;
        let failed = |id: ObjectId| {
            eprintln!("error: {}", rmsg::could_not_read(&id.to_hex()));
            eprintln!("error: {}", rmsg::could_not_parse_commit(&id.to_hex()));
            Die::Fatal(rmsg::REACHABILITY_FAILED.to_owned())
        };
        if !self.contains.targets.is_empty()
            && !self.contains.reaches(objects, commit).map_err(failed)?
        {
            return Ok(false);
        }
        if !self.no_contains.targets.is_empty()
            && self.no_contains.reaches(objects, commit).map_err(failed)?
        {
            return Ok(false);
        }
        if self.merged.as_ref().is_some_and(|m| !m.contains(&commit)) {
            return Ok(false);
        }
        if self.no_merged.as_ref().is_some_and(|m| m.contains(&commit)) {
            return Ok(false);
        }
        Ok(true)
    }
}

/// Literal prefix match up to a `/`, or a glob with `*` not crossing `/`.
fn matches_any(patterns: &[String], name: &str, icase: bool) -> bool {
    patterns.iter().any(|p| {
        let (p, n) = if icase {
            (p.to_ascii_lowercase(), name.to_ascii_lowercase())
        } else {
            (p.clone(), name.to_owned())
        };
        if let Some(rest) = n.strip_prefix(p.as_str()) {
            if rest.is_empty() || p.ends_with('/') || rest.starts_with('/') {
                return true;
            }
        }
        p.contains(['*', '?', '[', '\\'])
            && wildmatch(p.as_bytes(), n.as_bytes(), WildmatchFlags::PATHNAME)
    })
}
