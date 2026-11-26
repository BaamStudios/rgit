// SPDX-License-Identifier: MIT
//! `git show-branch` (Documentation/git-show-branch.adoc,
//! Documentation/config/showbranch.adoc): the commits of up to 27 refs (or
//! reflog entries of one ref) side by side, one column per ref, down to
//! their common ancestor; `--list`, `--independent` and `--merge-base`.
//!
//! The layout, the order of the commits and their names were determined
//! from the behaviour of the `git` binary.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::io::Write;

use rgit_hash::ObjectId;
use rgit_object::{Commit, ObjectKind};
use rgit_refs::files::FilesStore;
use rgit_refs::reflog::FilesReflog;
use rgit_refs::{IterItem, IterOptions, RefStore, Target};
use rgit_revwalk::revision::{Flags, ObjectLookup, Resolver};

use crate::messages::show_branch as msg;
use crate::prelude::*;
use crate::refs_support::{self, Revs};
use crate::repo::{self, Mode};

/// The most refs (columns) shown at once.
const MAX_REVS: usize = 27;
/// Reflog entries shown by `--reflog` without a count.
const DEFAULT_REFLOG: usize = 4;

/// Column colours, used round-robin.
const COLUMN_COLORS: [&str; 12] = [
    "\x1b[31m",
    "\x1b[32m",
    "\x1b[33m",
    "\x1b[34m",
    "\x1b[35m",
    "\x1b[36m",
    "\x1b[1;31m",
    "\x1b[1;32m",
    "\x1b[1;33m",
    "\x1b[1;34m",
    "\x1b[1;35m",
    "\x1b[1;36m",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
    Topo,
    Date,
}

/// `--reflog[=<n>[,<base>]]`.
#[derive(Clone, Debug, Default)]
struct ReflogArg {
    count: usize,
    base: Option<String>,
}

fn parse_reflog(v: Option<&str>) -> Result<ReflogArg, CallbackError> {
    let Some(v) = v else {
        return Ok(ReflogArg::default());
    };
    let digits_end = |s: &str| {
        let b = s.as_bytes();
        let mut i = 0;
        if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
            i += 1;
        }
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        (start, i)
    };
    let (start, end) = digits_end(v);
    let count = if end > start {
        let neg = v.starts_with('-');
        let n: u64 = v[start..end].parse().unwrap_or(u64::MAX);
        if neg { 0 } else { usize::try_from(n).unwrap_or(usize::MAX) }
    } else {
        0
    };
    let rest = if end > start { &v[end..] } else { v };
    let base = if rest.is_empty() {
        None
    } else if let Some(b) = rest.strip_prefix(',') {
        Some(b.to_owned())
    } else {
        return Err(CallbackError::Error(msg::unrecognized_reflog_param(v)));
    };
    Ok(ReflogArg { count, base })
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    // Without any argument the values of `showbranch.default` are the
    // arguments.
    let mut argv = argv.to_vec();
    let mut early = None;
    if argv.len() == 1 {
        let setup = repo::setup(ctx, Mode::Required)?;
        let config = refs_support::load_config(ctx, &setup)?;
        {
            let values = config.get_all("showbranch.default");
            for v in values {
                if let Some(v) = &v.value {
                    argv.push(v.to_string());
                }
            }
        }
        early = Some((setup, config));
    }

    let all = Cell::new(false);
    let remotes = Cell::new(false);
    let color = Cell::new(None);
    let extra = Cell::new(0i32);
    let no_name = Cell::new(false);
    let current = Cell::new(false);
    let sha1_name = Cell::new(false);
    let merge_base = Cell::new(false);
    let independent = Cell::new(false);
    let sort = Cell::new(0i32);
    let topics = Cell::new(false);
    let sparse = Cell::new(false);
    let reflog: RefCell<Option<ReflogArg>> = RefCell::new(None);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag('a', "all", &all, msg::HELP_ALL),
            Opt::flag('r', "remotes", &remotes, msg::HELP_REMOTES),
            Opt::callback(None, "color", Some("when"), msg::HELP_COLOR, |v, neg| {
                color.set(Some(crate::color::parse_when(v, neg)?));
                Ok(())
            })
            .optional(None),
            Opt::integer(None, "more", &extra, msg::HELP_MORE).optional(Some("1")),
            Opt::set_int(None, "list", &extra, msg::HELP_LIST, -1),
            Opt::flag(None, "no-name", &no_name, msg::HELP_NO_NAME),
            Opt::flag(None, "current", &current, msg::HELP_CURRENT),
            Opt::flag(None, "sha1-name", &sha1_name, msg::HELP_SHA1_NAME),
            Opt::flag(None, "merge-base", &merge_base, msg::HELP_MERGE_BASE),
            Opt::flag(None, "independent", &independent, msg::HELP_INDEPENDENT),
            Opt::set_int(None, "topo-order", &sort, msg::HELP_TOPO_ORDER, 1).noneg(),
            Opt::flag(None, "topics", &topics, msg::HELP_TOPICS),
            Opt::flag(None, "sparse", &sparse, msg::HELP_SPARSE),
            Opt::set_int(None, "date-order", &sort, msg::HELP_DATE_ORDER, 2).noneg(),
            Opt::callback('g', "reflog", Some(msg::ARGH_REFLOG), msg::HELP_REFLOG, |v, _| {
                *reflog.borrow_mut() = Some(parse_reflog(v)?);
                Ok(())
            })
            .optional(None)
            .literal_argh()
            .noneg(),
        ],
    )
    .flags(ParseFlags {
        stop_at_non_option: true,
        ..ParseFlags::default()
    });
    let args = parser.parse(&argv)?.args;
    let extra = extra.get();
    let reflog = reflog.take();
    if merge_base.get() && (extra != 0 || reflog.is_some()) {
        return Err(parser.usage_error().into());
    }
    if independent.get() && (extra != 0 || reflog.is_some()) {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    if reflog.is_some() {
        if all.get() || remotes.get() || independent.get() || merge_base.get() || extra > 0 {
            return Err(Die::Fatal(msg::REFLOG_INCOMPATIBLE.to_owned()));
        }
        if current.get() {
            return Err(Die::Fatal(msg::REFLOG_CURRENT.to_owned()));
        }
    }
    let opts = Options {
        all_heads: all.get(),
        all_remotes: all.get() || remotes.get(),
        color: color.get(),
        extra,
        no_name: no_name.get(),
        current: current.get(),
        sha1_name: sha1_name.get(),
        merge_base: merge_base.get(),
        independent: independent.get(),
        sort: if sort.get() == 2 { Sort::Date } else { Sort::Topo },
        topics: topics.get(),
        sparse: sparse.get(),
        reflog,
    };
    let (setup, config) = match early {
        Some(s) => s,
        None => {
            let setup = repo::setup(ctx, Mode::Required)?;
            let config = refs_support::load_config(ctx, &setup)?;
            (setup, config)
        }
    };
    crate::pager::setup_for(ctx, &config, "show-branch", false)?;
    let use_color = crate::color::want(&config, Some("color.showbranch"), opts.color)?;
    let store = refs_support::ref_store(&setup, &config);
    let reflogs = FilesReflog::new(
        setup.git_dir.clone(),
        setup.common_dir.clone(),
        setup.format.object_format,
    );
    let revs = Revs::new(ctx, &setup)?;
    revs.with_resolver(|r| {
        let env = Env {
            r,
            store: &store,
            reflogs: &reflogs,
            use_color,
        };
        show_branch(&env, &opts, &args)
    })
}

struct Options {
    all_heads: bool,
    all_remotes: bool,
    color: Option<rgit_config::color::ColorBool>,
    extra: i32,
    no_name: bool,
    current: bool,
    sha1_name: bool,
    merge_base: bool,
    independent: bool,
    sort: Sort,
    topics: bool,
    sparse: bool,
    reflog: Option<ReflogArg>,
}

struct Env<'a, 'r> {
    r: &'a Resolver<'r>,
    store: &'a FilesStore,
    reflogs: &'a FilesReflog,
    use_color: bool,
}

/// One column: a ref (or reflog entry) and its commit.
struct Rev {
    name: String,
    commit: ObjectId,
    /// What the header line shows after the name: the subject, or the
    /// date and message of a reflog entry.
    headline: Option<Vec<u8>>,
}

/// The columns being collected, with git's limits.
#[derive(Default)]
struct RevList {
    revs: Vec<Rev>,
}

impl RevList {
    /// Add `name` unless a column of that name exists; `false` when
    /// nothing was added.
    fn append(&mut self, name: String, commit: ObjectId) -> bool {
        if self.revs.iter().any(|r| r.name == name) {
            return false;
        }
        if self.revs.len() >= MAX_REVS {
            eprintln!("warning: {}", msg::ignoring_ref(&name, MAX_REVS));
            return false;
        }
        self.revs.push(Rev {
            name,
            commit,
            headline: None,
        });
        true
    }

    /// Sort the columns from `from` on by name.
    fn sort_from(&mut self, from: usize) {
        self.revs[from..].sort_by(|a, b| version_cmp(a.name.as_bytes(), b.name.as_bytes()));
    }
}

/// Order of ref names: runs of digits compare as (32-bit) numbers, the
/// text between them byte by byte.
fn version_cmp(a: &[u8], b: &[u8]) -> Ordering {
    let (mut i, mut j) = (0, 0);
    loop {
        let ai = i + a[i..].iter().take_while(|c| !c.is_ascii_digit()).count();
        let bj = j + b[j..].iter().take_while(|c| !c.is_ascii_digit()).count();
        match a[i..ai].cmp(&b[j..bj]) {
            Ordering::Equal => {}
            o => return o,
        }
        let number = |s: &[u8], mut k: usize| {
            let mut n: u32 = 0;
            while k < s.len() && s[k].is_ascii_digit() {
                n = n.wrapping_mul(10).wrapping_add(u32::from(s[k] - b'0'));
                k += 1;
            }
            (n, k)
        };
        let (na, ai) = number(a, ai);
        let (nb, bj) = number(b, bj);
        match na.cmp(&nb) {
            Ordering::Equal => {}
            o => return o,
        }
        i = ai;
        j = bj;
        if i >= a.len() && j >= b.len() {
            return Ordering::Equal;
        }
    }
}

/// `fnmatch` with `FNM_PATHNAME`: `*`, `?` and brackets do not match `/`.
fn glob_match(pat: &[u8], s: &[u8]) -> bool {
    match pat.first() {
        None => s.is_empty(),
        Some(b'*') => {
            let rest = &pat[1..];
            for k in 0..=s.len() {
                if glob_match(rest, &s[k..]) {
                    return true;
                }
                if k < s.len() && s[k] == b'/' {
                    break;
                }
            }
            false
        }
        Some(b'?') => !s.is_empty() && s[0] != b'/' && glob_match(&pat[1..], &s[1..]),
        Some(b'[') => {
            let Some((&c, rest_s)) = s.split_first() else {
                return false;
            };
            if c == b'/' {
                return false;
            }
            let mut k = 1;
            let negate = matches!(pat.get(k), Some(b'!' | b'^'));
            if negate {
                k += 1;
            }
            let mut matched = false;
            let mut first = true;
            loop {
                let Some(&p) = pat.get(k) else {
                    // No closing bracket: a literal `[`.
                    return c == b'[' && glob_match(&pat[1..], rest_s);
                };
                if p == b']' && !first {
                    k += 1;
                    break;
                }
                first = false;
                let lo = if p == b'\\' {
                    k += 1;
                    match pat.get(k) {
                        Some(&e) => e,
                        None => return false,
                    }
                } else {
                    p
                };
                if pat.get(k + 1) == Some(&b'-') && pat.get(k + 2).is_some_and(|&h| h != b']') {
                    let hi = pat[k + 2];
                    if lo <= c && c <= hi {
                        matched = true;
                    }
                    k += 3;
                } else {
                    if lo == c {
                        matched = true;
                    }
                    k += 1;
                }
            }
            matched != negate && glob_match(&pat[k..], rest_s)
        }
        Some(b'\\') if pat.len() > 1 => {
            !s.is_empty() && s[0] == pat[1] && glob_match(&pat[2..], &s[1..])
        }
        Some(&p) => !s.is_empty() && s[0] == p && glob_match(&pat[1..], &s[1..]),
    }
}

fn is_glob(arg: &str) -> bool {
    arg.contains(['*', '?', '['])
}

/// Does `pattern` match `refname` or one of its tails after a `/`?
fn tail_match(pattern: &str, refname: &str) -> bool {
    let mut starts = vec![0];
    starts.extend(refname.match_indices('/').map(|(i, _)| i + 1));
    starts
        .into_iter()
        .any(|s| glob_match(pattern.as_bytes(), &refname.as_bytes()[s..]))
}

/// Is the column `name` the checked-out branch (`head`: its full name, or
/// `HEAD` when detached)?
fn rev_is_head(head: Option<&str>, name: &str) -> bool {
    let Some(head) = head else { return false };
    let head = head.strip_prefix("refs/heads/").unwrap_or(head);
    let name = name
        .strip_prefix("refs/heads/")
        .or_else(|| name.strip_prefix("heads/"))
        .unwrap_or(name);
    head == name
}

impl Env<'_, '_> {
    fn objects(&self) -> &dyn ObjectLookup {
        self.r.repo().objects
    }

    /// The commit `oid` leads to (through tags), if it is one.
    fn peel_commit(&self, oid: &ObjectId) -> Option<ObjectId> {
        let c = self.r.peel_to_commit(oid).ok()?;
        (self.objects().kind(&c).ok().flatten() == Some(ObjectKind::Commit)).then_some(c)
    }

    fn refs(&self) -> Result<Vec<(String, ObjectId)>, Die> {
        let items = self
            .store
            .iter(&IterOptions {
                prefix: "refs/".to_owned(),
                include_root_refs: false,
            })
            .map_err(|e| Die::Fatal(e.to_string()))?;
        Ok(items
            .into_iter()
            .filter_map(|i| match i {
                IterItem::Ref(r) => r.oid.map(|o| (r.name, o)),
                _ => None,
            })
            .collect())
    }

    /// The checked-out branch (full name), `HEAD` when detached, `None`
    /// when HEAD cannot be read.
    fn head(&self) -> Option<(String, Option<ObjectId>)> {
        match self.store.read_raw("HEAD").ok().flatten()? {
            Target::Symbolic(name) => {
                let oid = self.store.resolve_oid(&name).ok().flatten();
                Some((name, oid))
            }
            Target::Object(oid) => Some(("HEAD".to_owned(), Some(oid))),
        }
    }
}

fn show_branch(env: &Env<'_, '_>, opts: &Options, args: &[String]) -> CmdResult {
    let head = env.head();
    let head_name = head.as_ref().map(|(n, _)| n.as_str());
    let mut list = RevList::default();
    let reflog_mode = opts.reflog.is_some();

    if let Some(rl) = &opts.reflog {
        if args.len() > 1 {
            return Err(Die::Fatal(msg::REFLOG_ONE_BRANCH.to_owned()));
        }
        let count = if rl.count == 0 { DEFAULT_REFLOG } else { rl.count };
        if count > MAX_REVS {
            return Err(Die::Fatal(msg::only_entries(MAX_REVS)));
        }
        let (full, shown) = if let Some(a) = args.first() {
            let dwim = env
                .r
                .dwim_ref(a)
                .map_err(|e| Die::Fatal(e.to_string()))?;
            if dwim.count == 0 {
                return Err(Die::Fatal(msg::no_such_ref(a)));
            }
            (dwim.full_name, a.clone())
        } else {
            match &head {
                Some((name, Some(_))) => (name.clone(), name.clone()),
                _ => return Err(Die::Fatal(msg::HEAD_NOT_VALID.to_owned())),
            }
        };
        let mut entries = env.reflogs.read(&full).ok().flatten().unwrap_or_default();
        entries.reverse();
        let dctx = rgit_date::DateContext::from_env();
        let base = match &rl.base {
            None => Some(0),
            Some(b) if !b.is_empty() && b.bytes().all(|c| c.is_ascii_digit()) => {
                Some(b.parse::<usize>().unwrap_or(usize::MAX))
            }
            Some(b) => {
                let when = rgit_date::approxidate(b, &dctx);
                entries.iter().position(|e| e.time <= when)
            }
        };
        if let Some(base) = base {
            for (n, e) in entries.iter().enumerate().skip(base).take(count) {
                let Some(commit) = env.peel_commit(&e.new) else {
                    continue;
                };
                let mut headline = format!(
                    "({}) ",
                    rgit_date::show_date_relative(e.time, dctx.now)
                )
                .into_bytes();
                let m = &e.message;
                let end = m.iter().position(|&c| c == b'\n').unwrap_or(m.len());
                headline.extend_from_slice(&m[..end]);
                list.revs.push(Rev {
                    name: format!("{shown}@{{{n}}}"),
                    commit,
                    headline: Some(headline),
                });
            }
        }
    } else {
        let mut refs_cache: Option<Vec<(String, ObjectId)>> = None;
        let mut all_refs = |env: &Env<'_, '_>| -> Result<Vec<(String, ObjectId)>, Die> {
            if refs_cache.is_none() {
                refs_cache = Some(env.refs()?);
            }
            Ok(refs_cache.clone().unwrap_or_default())
        };
        for a in args {
            if is_glob(a) {
                let from = list.revs.len();
                let mut added = 0;
                for (name, oid) in all_refs(env)? {
                    if !tail_match(a, &name) {
                        continue;
                    }
                    let Some(commit) = env.peel_commit(&oid) else {
                        continue;
                    };
                    let shown = if let Some(s) = name.strip_prefix("refs/heads/") {
                        s.to_owned()
                    } else if name.starts_with("refs/tags/") {
                        name["refs/".len()..].to_owned()
                    } else {
                        name.clone()
                    };
                    if list.append(shown, commit) {
                        added += 1;
                    }
                }
                if added == 0 {
                    eprintln!("error: {}", msg::no_matching_refs(a));
                }
                list.sort_from(from);
                continue;
            }
            let flags = Flags {
                quiet: true,
                ..Flags::default()
            };
            let Ok(oid) = env.r.resolve_with(a, flags) else {
                return Err(Die::Fatal(msg::bad_sha1_reference(a)));
            };
            if let Some(commit) = env.peel_commit(&oid) {
                list.append(a.clone(), commit);
            }
        }
        // The local branches by default, and as the topics of a single ref.
        let default = !opts.all_remotes && (args.is_empty() || (opts.topics && args.len() == 1));
        for (prefix, wanted) in [
            ("refs/heads/", opts.all_heads || default),
            ("refs/remotes/", opts.all_remotes),
        ] {
            if !wanted {
                continue;
            }
            let from = list.revs.len();
            for (name, oid) in all_refs(env)? {
                let Some(short) = name.strip_prefix(prefix) else {
                    continue;
                };
                if let Some(commit) = env.peel_commit(&oid) {
                    list.append(short.to_owned(), commit);
                }
            }
            list.sort_from(from);
        }
        if opts.current {
            if let Some((name, Some(oid))) = &head {
                if !list.revs.iter().any(|r| rev_is_head(head_name, &r.name)) {
                    let shown = name.strip_prefix("refs/heads/").unwrap_or(name);
                    if let Some(commit) = env.peel_commit(oid) {
                        list.append(shown.to_owned(), commit);
                    }
                }
            }
        }
    }

    if list.revs.is_empty() {
        eprintln!("{}", msg::NO_REVS);
        return Ok(0);
    }

    let mut graph = Graph::new(env.objects());
    let tips: Vec<usize> = list
        .revs
        .iter()
        .map(|r| graph.node(&r.commit))
        .collect::<Result<_, _>>()?;
    for (rev, &tip) in list.revs.iter_mut().zip(&tips) {
        if rev.headline.is_none() {
            rev.headline = Some(graph.nodes[tip].subject.clone());
        }
    }
    let num_rev = list.revs.len();
    let is_head: Vec<bool> = list
        .revs
        .iter()
        .map(|r| !reflog_mode && rev_is_head(head_name, &r.name))
        .collect();

    let mut out = Vec::new();
    if opts.extra < 0 {
        for (rev, &head) in list.revs.iter().zip(&is_head) {
            out.extend_from_slice(if head { b"* [" } else { b"  [" });
            out.extend_from_slice(rev.name.as_bytes());
            out.extend_from_slice(b"] ");
            out.extend_from_slice(rev.headline.as_deref().unwrap_or_default());
            out.push(b'\n');
        }
        return write_out(&out);
    }

    let all_mask: u32 = if num_rev >= 32 {
        u32::MAX
    } else {
        (1u32 << num_rev) - 1
    };
    let walk_extra = if opts.merge_base || opts.independent {
        0
    } else {
        opts.extra
    };
    let seen = graph.join_revs(&tips, all_mask, walk_extra)?;

    if opts.merge_base {
        for &c in &seen {
            let n = &graph.nodes[c];
            if n.flags & all_mask == all_mask && !n.poisoned {
                out.extend_from_slice(n.oid.to_hex().as_bytes());
                out.push(b'\n');
            }
        }
        return write_out(&out);
    }
    if opts.independent {
        for (i, &tip) in tips.iter().enumerate() {
            if graph.nodes[tip].flags & all_mask == 1 << i {
                out.extend_from_slice(graph.nodes[tip].oid.to_hex().as_bytes());
                out.push(b'\n');
            }
        }
        return write_out(&out);
    }

    let sorted = match opts.sort {
        Sort::Topo => graph.topo_sort(&seen, false),
        Sort::Date => graph.topo_sort(&seen, true),
    };
    let names: Vec<&str> = list.revs.iter().map(|r| r.name.as_str()).collect();
    graph.name_commits(&sorted, &tips, &names);

    let color = |i: usize, ch: u8, out: &mut Vec<u8>| {
        if env.use_color {
            out.extend_from_slice(COLUMN_COLORS[i % COLUMN_COLORS.len()].as_bytes());
            out.push(ch);
            out.extend_from_slice(crate::color::RESET.as_bytes());
        } else {
            out.push(ch);
        }
    };

    if num_rev > 1 {
        for (i, rev) in list.revs.iter().enumerate() {
            out.extend(std::iter::repeat_n(b' ', i));
            color(i, if is_head[i] { b'*' } else { b'!' }, &mut out);
            out.extend_from_slice(b" [");
            out.extend_from_slice(rev.name.as_bytes());
            out.extend_from_slice(b"] ");
            out.extend_from_slice(rev.headline.as_deref().unwrap_or_default());
            out.push(b'\n');
        }
        out.extend(std::iter::repeat_n(b'-', num_rev));
        out.push(b'\n');
    }

    // The commit lines mark the last column of the checked-out branch.
    let head_col = is_head.iter().rposition(|&h| h);
    let is_tip: std::collections::HashSet<usize> = tips.iter().copied().collect();
    // Once the first commit all tips reach is shown, `--more` more lines.
    let mut extra = opts.extra;
    let mut shown_merge_point = false;
    for &c in &sorted {
        let n = &graph.nodes[c];
        let flags = n.flags & all_mask;
        let is_merge = n.parent_ids.len() > 1;
        let common = flags == all_mask;
        let skip = (opts.topics && flags & 1 != 0 && !common)
            || (!opts.sparse && is_merge && !common && flags.count_ones() == 1 && !is_tip.contains(&c));
        if skip {
            continue;
        }
        {
            if num_rev > 1 {
                for i in 0..num_rev {
                    if flags & (1 << i) == 0 {
                        out.push(b' ');
                    } else if is_merge {
                        color(i, b'-', &mut out);
                    } else if head_col == Some(i) {
                        color(i, b'*', &mut out);
                    } else {
                        color(i, b'+', &mut out);
                    }
                }
                out.push(b' ');
            }
            if !opts.no_name {
                out.push(b'[');
                if opts.sha1_name {
                    let len = env.r.default_abbrev();
                    let abbrev = env
                        .r
                        .unique_abbrev(&n.oid, len)
                        .unwrap_or_else(|_| n.oid.to_hex());
                    out.extend_from_slice(abbrev.as_bytes());
                } else {
                    out.extend_from_slice(graph.printed_name(c).as_bytes());
                }
                out.extend_from_slice(b"] ");
            }
            out.extend_from_slice(&n.subject);
            out.push(b'\n');
        }
        shown_merge_point |= common;
        if shown_merge_point {
            extra -= 1;
            if extra < 0 {
                break;
            }
        }
    }
    write_out(&out)
}

fn write_out(out: &[u8]) -> CmdResult {
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(out);
    let _ = o.flush();
    Ok(0)
}

/// The name of a commit: a tip name and how many first parents down.
#[derive(Clone)]
struct Name {
    head: String,
    generation: usize,
}

struct Node {
    oid: ObjectId,
    parent_ids: Vec<ObjectId>,
    /// Node indices of the parents, once loaded.
    parents: Vec<usize>,
    parents_loaded: bool,
    date: i64,
    subject: Vec<u8>,
    flags: u32,
    /// Reached from a commit that all tips reach.
    poisoned: bool,
    /// Taken by the walk while all tips reach it.
    common: bool,
    seen: bool,
    /// Waiting in the queue of the walk.
    queued: bool,
    name: Option<Name>,
}

struct Graph<'a> {
    objects: &'a dyn ObjectLookup,
    nodes: Vec<Node>,
    index: HashMap<ObjectId, usize>,
}

/// The subject of a commit message: its first paragraph, lines joined
/// with spaces.
fn subject_of(message: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut started = false;
    for line in message.split(|&c| c == b'\n') {
        let end = line
            .iter()
            .rposition(|c| !c.is_ascii_whitespace())
            .map_or(0, |i| i + 1);
        let line = &line[..end];
        if line.is_empty() {
            if started {
                break;
            }
            continue;
        }
        if started {
            out.push(b' ');
        }
        started = true;
        out.extend_from_slice(line);
    }
    out
}

impl<'a> Graph<'a> {
    fn new(objects: &'a dyn ObjectLookup) -> Self {
        Graph {
            objects,
            nodes: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// The node of commit `oid`.
    fn node(&mut self, oid: &ObjectId) -> Result<usize, Die> {
        if let Some(&i) = self.index.get(oid) {
            return Ok(i);
        }
        let bad = || Die::Fatal(format!("bad object {}", oid.to_hex()));
        let (kind, data) = self.objects.read(oid).ok().flatten().ok_or_else(bad)?;
        if kind != ObjectKind::Commit {
            return Err(bad());
        }
        let commit = Commit::parse(self.objects.hash_kind(), &data).map_err(|_| bad())?;
        let parent_ids = if self.objects.is_shallow(oid) {
            Vec::new()
        } else if let Some(grafted) = self.objects.grafted_parents(oid) {
            grafted
        } else {
            commit.parents().map_err(|_| bad())?
        };
        let date = commit
            .committer()
            .and_then(|c| c.time())
            .map_or(0, |t| t.seconds);
        let i = self.nodes.len();
        self.index.insert(*oid, i);
        self.nodes.push(Node {
            oid: *oid,
            parent_ids,
            parents: Vec::new(),
            parents_loaded: false,
            date,
            subject: subject_of(commit.message()),
            flags: 0,
            poisoned: false,
            common: false,
            seen: false,
            queued: false,
            name: None,
        });
        Ok(i)
    }

    fn parents(&mut self, c: usize) -> Result<Vec<usize>, Die> {
        if !self.nodes[c].parents_loaded {
            let ids = self.nodes[c].parent_ids.clone();
            let mut ps = Vec::with_capacity(ids.len());
            for id in &ids {
                ps.push(self.node(id)?);
            }
            self.nodes[c].parents = ps;
            self.nodes[c].parents_loaded = true;
        }
        Ok(self.nodes[c].parents.clone())
    }

    /// The parents of `c` that have nodes (all of them once `c` was
    /// taken by the walk), by position.
    fn known_parents(&self, c: usize) -> Vec<Option<usize>> {
        self.nodes[c]
            .parent_ids
            .iter()
            .map(|id| self.index.get(id).copied())
            .collect()
    }

    /// Insert `c` into `queue` (newest first) after the commits of the
    /// same date, unless it is waiting there already.
    fn insert_by_date(&mut self, queue: &mut Vec<usize>, c: usize) {
        if self.nodes[c].queued {
            return;
        }
        self.nodes[c].queued = true;
        let d = self.nodes[c].date;
        let pos = queue
            .iter()
            .position(|&q| self.nodes[q].date < d)
            .unwrap_or(queue.len());
        queue.insert(pos, c);
    }

    /// Walk from the tips (tip `i` marks bit `i`) while a commit waiting
    /// is not known to be reachable from all tips, then on until `extra`
    /// more commits were seen. Returns the commits seen, newest first
    /// (of equal dates the one seen last first).
    fn join_revs(
        &mut self,
        tips: &[usize],
        all_mask: u32,
        mut extra: i32,
    ) -> Result<Vec<usize>, Die> {
        // The walk starts with the tips in the order given, not by date.
        let mut queue = Vec::new();
        for (i, &t) in tips.iter().enumerate() {
            self.nodes[t].flags |= 1 << i;
            if !self.nodes[t].queued {
                self.nodes[t].queued = true;
                queue.push(t);
            }
        }
        let mut order = Vec::new();
        while !queue.is_empty() {
            let still_interesting = queue
                .iter()
                .any(|&q| !self.nodes[q].poisoned && !self.nodes[q].common);
            let c = queue.remove(0);
            self.nodes[c].queued = false;
            if !still_interesting && extra <= 0 {
                break;
            }
            self.mark_seen(c, &mut order);
            let flags = self.nodes[c].flags & all_mask;
            if flags == all_mask {
                self.nodes[c].common = true;
            }
            let poison = self.nodes[c].common || self.nodes[c].poisoned;
            for p in self.parents(c)? {
                let pn = &self.nodes[p];
                if pn.flags & flags == flags && (!poison || pn.poisoned) {
                    continue;
                }
                if self.mark_seen(p, &mut order) && !still_interesting {
                    extra -= 1;
                }
                let pn = &mut self.nodes[p];
                pn.flags |= flags;
                if poison {
                    pn.poisoned = true;
                }
                self.insert_by_date(&mut queue, p);
            }
        }
        order.reverse();
        order.sort_by_key(|&c| std::cmp::Reverse(self.nodes[c].date));
        Ok(order)
    }

    /// Add `c` to `seen` unless it is there; whether it was added.
    fn mark_seen(&mut self, c: usize, seen: &mut Vec<usize>) -> bool {
        if self.nodes[c].seen {
            return false;
        }
        self.nodes[c].seen = true;
        seen.push(c);
        true
    }

    /// `list` in topological order: children before parents; among the
    /// commits ready to be shown the newest (`by_date`) or the one that
    /// became ready last.
    fn topo_sort(&mut self, list: &[usize], by_date: bool) -> Vec<usize> {
        let in_list: std::collections::HashSet<usize> = list.iter().copied().collect();
        let mut indegree: HashMap<usize, usize> = list.iter().map(|&c| (c, 0)).collect();
        for &c in list {
            for p in self.known_parents(c).into_iter().flatten() {
                if let Some(d) = indegree.get_mut(&p) {
                    *d += 1;
                }
            }
        }
        let mut out = Vec::with_capacity(list.len());
        if by_date {
            let mut heap = std::collections::BinaryHeap::new();
            let mut seq = 0u64;
            for &c in list {
                if indegree[&c] == 0 {
                    heap.push((self.nodes[c].date, std::cmp::Reverse(seq), c));
                    seq += 1;
                }
            }
            while let Some((_, _, c)) = heap.pop() {
                out.push(c);
                for p in self.known_parents(c).into_iter().flatten() {
                    if !in_list.contains(&p) {
                        continue;
                    }
                    let d = indegree.get_mut(&p).expect("in list");
                    *d -= 1;
                    if *d == 0 {
                        heap.push((self.nodes[p].date, std::cmp::Reverse(seq), p));
                        seq += 1;
                    }
                }
            }
        } else {
            let mut stack: Vec<usize> = list
                .iter()
                .copied()
                .filter(|c| indegree[c] == 0)
                .collect();
            stack.reverse();
            while let Some(c) = stack.pop() {
                out.push(c);
                for p in self.known_parents(c).into_iter().flatten() {
                    if !in_list.contains(&p) {
                        continue;
                    }
                    let d = indegree.get_mut(&p).expect("in list");
                    *d -= 1;
                    if *d == 0 {
                        stack.push(p);
                    }
                }
            }
        }
        out
    }

    /// Name the commits of `list`: the tips by their column names, then
    /// along first parents, then the other parents of merges (`^<n>`).
    fn name_commits(&mut self, list: &[usize], tips: &[usize], names: &[&str]) {
        let in_list: std::collections::HashSet<usize> = list.iter().copied().collect();
        for (&t, &n) in tips.iter().zip(names) {
            if self.nodes[t].name.is_none() {
                self.nodes[t].name = Some(Name {
                    head: n.to_owned(),
                    generation: 0,
                });
            }
        }
        loop {
            let mut changed = false;
            for &c in list {
                if self.nodes[c].name.is_some() {
                    changed |= self.name_first_parents(c, &in_list);
                }
            }
            if !changed {
                break;
            }
        }
        loop {
            let mut changed = false;
            for &c in list {
                let Some(name) = self.nodes[c].name.clone() else {
                    continue;
                };
                let parents = self.known_parents(c);
                for (k, p) in parents.into_iter().enumerate() {
                    let Some(p) = p else { continue };
                    if !in_list.contains(&p) || self.nodes[p].name.is_some() {
                        continue;
                    }
                    let new = if k == 0 {
                        Name {
                            head: name.head.clone(),
                            generation: name.generation + 1,
                        }
                    } else {
                        Name {
                            head: format!("{}^{}", self.printed_name(c), k + 1),
                            generation: 0,
                        }
                    };
                    self.nodes[p].name = Some(new);
                    changed = true;
                    self.name_first_parents(p, &in_list);
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// Name the unnamed first parents below the named commit `c`; whether
    /// any was named.
    fn name_first_parents(&mut self, c: usize, in_list: &std::collections::HashSet<usize>) -> bool {
        let Some(name) = self.nodes[c].name.clone() else {
            return false;
        };
        let mut changed = false;
        let mut cur = c;
        let mut generation = name.generation;
        while let Some(Some(p)) = self.known_parents(cur).first().copied() {
            if !in_list.contains(&p) || self.nodes[p].name.is_some() {
                break;
            }
            generation += 1;
            self.nodes[p].name = Some(Name {
                head: name.head.clone(),
                generation,
            });
            changed = true;
            cur = p;
        }
        changed
    }

    fn printed_name(&self, c: usize) -> String {
        match &self.nodes[c].name {
            None => self.nodes[c].oid.to_hex(),
            Some(Name {
                head,
                generation: 0,
            }) => head.clone(),
            Some(Name {
                head,
                generation: 1,
            }) => format!("{head}^"),
            Some(Name { head, generation }) => format!("{head}~{generation}"),
        }
    }
}
