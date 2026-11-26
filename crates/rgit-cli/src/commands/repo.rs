// SPDX-License-Identifier: MIT
//! `git repo info` and `git repo structure` (Documentation/git-repo.adoc):
//! metadata of the repository by key, and statistics about its references
//! and reachable objects.
//!
//! Details not in the documentation were determined black-box against git
//! 2.56.1: relative paths are relative to the directory the command was
//! started in (`./` for that directory itself, `../` for a parent), the
//! table's value column is at least as wide as its header plus one, and
//! counts and sizes are shortened with two decimals once they exceed 1000
//! (`k`, `M`, `G`) or 1024 bytes (`KiB`, `MiB`, `GiB`).

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use rgit_hash::ObjectId;
use rgit_object::ObjectKind;
use rgit_repo::Setup;

use crate::messages::repo_cmd as msg;
use crate::prelude::*;
use crate::repo_objects::Repo;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mut parser = Parser::new(
        &[msg::USAGE_INFO, msg::USAGE_KEYS, msg::USAGE_STRUCTURE],
        vec![Opt::subcommand("info"), Opt::subcommand("structure")],
    );
    let parsed = parser.parse(argv)?;
    drop(parser);
    match parsed.subcommand.as_deref() {
        Some("info") => info(ctx, &parsed.args),
        _ => structure(ctx, &parsed.args),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Table,
    Lines,
    Nul,
}

/// `--format=<format>` and `-z`, the last one wins.
fn format_options<'a>(format: &'a Cell<Option<Format>>) -> Vec<Opt<'a>> {
    vec![
        Opt::callback(None, "format", Some("format"), msg::H_FORMAT, move |v, _| {
            let f = match v.unwrap_or_default() {
                "table" => Format::Table,
                "lines" => Format::Lines,
                "nul" => Format::Nul,
                other => return Err(CallbackError::Fatal(msg::invalid_format(other))),
            };
            format.set(Some(f));
            Ok(())
        })
        .noneg(),
        Opt::callback('z', None, None, msg::H_Z, move |_, _| {
            format.set(Some(Format::Nul));
            Ok(())
        })
        .noarg()
        .noneg(),
    ]
}

const KEYS: &[&str] = &[
    "layout.bare",
    "layout.shallow",
    "object.format",
    "path.commondir.absolute",
    "path.commondir.relative",
    "path.gitdir.absolute",
    "path.gitdir.relative",
    "references.format",
];

fn info(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let format = Cell::new(None);
    let all = Cell::new(false);
    let keys = Cell::new(false);
    let mut opts = format_options(&format);
    opts.push(Opt::flag(None, "all", &all, msg::H_ALL));
    opts.push(Opt::flag(None, "keys", &keys, msg::H_KEYS));
    let mut parser = Parser::new(&[msg::USAGE_INFO, msg::USAGE_KEYS], opts);
    let args = parser.parse(argv)?.args;
    drop(parser);
    let format = format.get().unwrap_or(Format::Lines);
    let mut out = Vec::new();
    if keys.get() {
        if all.get() || !args.is_empty() {
            return Err(Die::Fatal(msg::KEYS_WITH_KEY.into()));
        }
        if format == Format::Table {
            return Err(Die::Fatal(msg::KEYS_FORMAT.into()));
        }
        for k in KEYS {
            out.extend_from_slice(k.as_bytes());
            out.push(if format == Format::Nul { 0 } else { b'\n' });
        }
        return write_out(&out, 0);
    }
    if format == Format::Table {
        return Err(Die::Fatal(msg::UNSUPPORTED_FORMAT.into()));
    }
    if all.get() && !args.is_empty() {
        return Err(Die::Fatal(msg::ALL_AND_KEY.into()));
    }
    let setup = crate::repo::setup(ctx, crate::repo::Mode::Required)?;
    let quote_high = crate::repo::load_config(ctx, &setup)?
        .bool("core.quotepath")
        .ok()
        .flatten()
        .unwrap_or(true);
    let requested: Vec<String> = if all.get() {
        KEYS.iter().map(|k| k.to_string()).collect()
    } else {
        args
    };
    let mut code = 0;
    for key in &requested {
        let Some(value) = info_value(&setup, key) else {
            eprintln!("error: {}", msg::key_not_found(key));
            code = 255;
            continue;
        };
        out.extend_from_slice(key.as_bytes());
        if format == Format::Nul {
            out.push(b'\n');
            out.extend_from_slice(&value);
            out.push(0);
        } else {
            out.push(b'=');
            crate::quote::write_quoted(&mut out, &value, quote_high);
            out.push(b'\n');
        }
    }
    write_out(&out, code)
}

fn write_out(out: &[u8], code: i32) -> CmdResult {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(out);
    let _ = stdout.flush();
    Ok(code)
}

fn info_value(setup: &Setup, key: &str) -> Option<Vec<u8>> {
    let bool_text = |b: bool| if b { b"true".to_vec() } else { b"false".to_vec() };
    let path_text = |p: &Path| p.as_os_str().as_encoded_bytes().to_vec();
    Some(match key {
        "layout.bare" => bool_text(setup.is_bare()),
        "layout.shallow" => {
            let file = rgit_fetch::shallow::file_for(&setup.common_dir);
            bool_text(std::fs::metadata(file).is_ok_and(|m| m.len() > 0))
        }
        "object.format" => setup.format.object_format.name().as_bytes().to_vec(),
        "path.commondir.absolute" => path_text(&absolute(setup, &setup.common_dir)),
        "path.commondir.relative" => relative(setup, &setup.common_dir),
        "path.gitdir.absolute" => path_text(&absolute(setup, &setup.git_dir)),
        "path.gitdir.relative" => relative(setup, &setup.git_dir),
        "references.format" => match setup.format.ref_storage {
            rgit_repo::RefStorage::Files => b"files".to_vec(),
            rgit_repo::RefStorage::Reftable => b"reftable".to_vec(),
        },
        _ => return None,
    })
}

fn absolute(setup: &Setup, dir: &Path) -> PathBuf {
    rgit_repo::path::real_path_lossy(dir, &setup.cwd)
}

/// `dir` relative to the directory the command was started in.
fn relative(setup: &Setup, dir: &Path) -> Vec<u8> {
    let target = absolute(setup, dir);
    let base = rgit_repo::path::real_path_lossy(Path::new("."), &setup.original_cwd);
    relative_path(&target, &base)
}

/// `target` as seen from `base` (both absolute and normalized): `./` for
/// `base` itself, `../` for each directory to go up.
fn relative_path(target: &Path, base: &Path) -> Vec<u8> {
    let t: Vec<Component> = target.components().collect();
    let b: Vec<Component> = base.components().collect();
    let common = t.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut out = Vec::new();
    for _ in common..b.len() {
        out.extend_from_slice(b"../");
    }
    for (i, c) in t[common..].iter().enumerate() {
        if i > 0 {
            out.push(b'/');
        }
        out.extend_from_slice(c.as_os_str().as_encoded_bytes());
    }
    if out.is_empty() {
        out.extend_from_slice(b"./");
    }
    out
}

/// Statistics for one object type.
#[derive(Default)]
struct TypeStats {
    count: u64,
    inflated: u64,
    disk: u64,
    max_size: Option<(u64, ObjectId)>,
    /// Parents of commits, entries of trees.
    max_fanout: Option<(u64, ObjectId)>,
}

impl TypeStats {
    fn add(&mut self, oid: &ObjectId, size: u64, disk: u64) {
        self.count += 1;
        self.inflated += size;
        self.disk += disk;
        if self.max_size.is_none_or(|(m, _)| size > m) {
            self.max_size = Some((size, *oid));
        }
    }

    fn fanout(&mut self, oid: &ObjectId, n: u64) {
        if self.max_fanout.is_none_or(|(m, _)| n > m) {
            self.max_fanout = Some((n, *oid));
        }
    }
}

/// Reads one statistic of a type.
type Getter = fn(&TypeStats) -> u64;

#[derive(Default)]
struct Stats {
    branches: u64,
    tags: u64,
    remotes: u64,
    others: u64,
    commits: TypeStats,
    trees: TypeStats,
    blobs: TypeStats,
    tag_objects: TypeStats,
}

fn structure(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let format = Cell::new(None);
    let progress = Cell::new(-1);
    let mut opts = format_options(&format);
    opts.push(Opt::bool(None, "progress", &progress, msg::H_PROGRESS));
    let mut parser = Parser::new(&[msg::USAGE_STRUCTURE], opts);
    let args = parser.parse(argv)?.args;
    if !args.is_empty() {
        return Err(Die::Output(rgit_options::Exit {
            code: rgit_options::EXIT_USAGE,
            stdout: String::new(),
            stderr: format!("usage: {}\n", msg::TOO_MANY_ARGUMENTS),
        }));
    }
    drop(parser);
    let format = format.get().unwrap_or(Format::Table);
    let setup = crate::repo::setup(ctx, crate::repo::Mode::Required)?;
    let show = match progress.get() {
        -1 => rgit_progress::stderr_is_terminal(),
        n => n != 0,
    };
    let repo = Repo::open(ctx, setup)?;
    let stats = collect(&repo, show);
    let null = repo.hash.null();
    let out = match format {
        Format::Table => table(&stats),
        _ => lines(&stats, format == Format::Nul, &null),
    };
    write_out(&out, 0)
}

fn collect(repo: &Repo, show: bool) -> Stats {
    let mut stats = Stats::default();
    let mut progress = rgit_progress::Progress::start_delayed(msg::COUNTING_REFERENCES, 0, show);
    let refs = repo.refs.list("refs/");
    for (name, _) in &refs {
        if name.starts_with("refs/heads/") {
            stats.branches += 1;
        } else if name.starts_with("refs/tags/") {
            stats.tags += 1;
        } else if name.starts_with("refs/remotes/") {
            stats.remotes += 1;
        } else {
            stats.others += 1;
        }
        progress.inc();
    }
    progress.stop();

    let mut progress = rgit_progress::Progress::start_delayed(msg::COUNTING_OBJECTS, 0, show);
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let hash = repo.hash;
    // Annotated tags first, then the commits newest first, then the trees
    // and blobs of each commit in that order.
    let mut commit_tips: Vec<ObjectId> = Vec::new();
    let mut loose_roots: Vec<ObjectId> = Vec::new();
    let mut tag_queue: VecDeque<ObjectId> = refs.iter().map(|(_, o)| *o).collect();
    while let Some(oid) = tag_queue.pop_front() {
        let Ok(Some(header)) = repo.odb.header(&oid) else {
            continue;
        };
        match header.kind {
            ObjectKind::Tag => {
                if !seen.insert(oid) {
                    continue;
                }
                stats
                    .tag_objects
                    .add(&oid, header.size, repo.disk_size(&oid));
                progress.inc();
                if let Ok(Some(raw)) = repo.odb.read(&oid)
                    && let Ok(tag) = rgit_object::Tag::parse(hash, &raw.data)
                    && let Ok(target) = tag.object()
                {
                    tag_queue.push_back(target);
                }
            }
            ObjectKind::Commit => commit_tips.push(oid),
            _ => loose_roots.push(oid),
        }
    }

    // Commits by committer date, newest first.
    let mut queue: BinaryHeap<(i64, Reverse<u64>, ObjectId)> = BinaryHeap::new();
    let mut bodies: HashMap<ObjectId, Vec<u8>> = HashMap::new();
    let mut seq = 0u64;
    let mut trees: Vec<ObjectId> = Vec::new();
    let date = |data: &[u8]| -> i64 {
        rgit_object::Commit::parse(hash, data)
            .ok()
            .and_then(|c| c.committer().and_then(|s| s.time()).map(|t| t.seconds))
            .unwrap_or(0)
    };
    for tip in commit_tips {
        if seen.insert(tip)
            && let Ok(Some(raw)) = repo.odb.read(&tip)
        {
            queue.push((date(&raw.data), Reverse(seq), tip));
            bodies.insert(tip, raw.data);
            seq += 1;
        }
    }
    while let Some((_, _, oid)) = queue.pop() {
        let data = bodies.remove(&oid).unwrap_or_default();
        let Ok(commit) = rgit_object::Commit::parse(hash, &data) else {
            continue;
        };
        stats
            .commits
            .add(&oid, data.len() as u64, repo.disk_size(&oid));
        let parents = commit.parents().unwrap_or_default();
        stats.commits.fanout(&oid, parents.len() as u64);
        progress.inc();
        if let Ok(tree) = commit.tree() {
            trees.push(tree);
        }
        for p in parents {
            if seen.insert(p)
                && let Ok(Some(praw)) = repo.odb.read(&p)
            {
                queue.push((date(&praw.data), Reverse(seq), p));
                bodies.insert(p, praw.data);
                seq += 1;
            }
        }
    }
    trees.extend(loose_roots);
    for root in trees {
        walk_tree(repo, root, &mut seen, &mut stats, &mut progress);
    }
    progress.stop();
    stats
}

/// Count `root` (a tree or blob) and everything below it, depth first.
fn walk_tree(
    repo: &Repo,
    root: ObjectId,
    seen: &mut HashSet<ObjectId>,
    stats: &mut Stats,
    progress: &mut rgit_progress::Progress,
) {
    let mut stack = vec![root];
    while let Some(oid) = stack.pop() {
        if !seen.insert(oid) {
            continue;
        }
        let Ok(Some(header)) = repo.odb.header(&oid) else {
            continue;
        };
        match header.kind {
            ObjectKind::Blob => {
                stats.blobs.add(&oid, header.size, repo.disk_size(&oid));
                progress.inc();
            }
            ObjectKind::Tree => {
                let Ok(Some(raw)) = repo.odb.read(&oid) else {
                    continue;
                };
                stats
                    .trees
                    .add(&oid, raw.data.len() as u64, repo.disk_size(&oid));
                progress.inc();
                let mut children = Vec::new();
                let mut entries = 0;
                for entry in rgit_object::TreeIter::new(repo.hash, &raw.data) {
                    let Ok(entry) = entry else { break };
                    entries += 1;
                    if entry.mode_bits & 0o170000 != 0o160000 {
                        children.push(entry.oid);
                    }
                }
                stats.trees.fanout(&oid, entries);
                stack.extend(children.into_iter().rev());
            }
            _ => {}
        }
    }
}

/// `n` shortened like git: two decimals and a unit above 1000 (counts) or
/// 1024 bytes (sizes).
fn humanize(n: u64, bytes: bool) -> (String, &'static str) {
    let (base, units): (u128, [&'static str; 4]) = if bytes {
        (1024, ["B", "KiB", "MiB", "GiB"])
    } else {
        (1000, ["", "k", "M", "G"])
    };
    let mut scale: u128 = 1;
    let mut unit = 0;
    while unit < 3 && u128::from(n) > scale * base {
        scale *= base;
        unit += 1;
    }
    if unit == 0 {
        return (n.to_string(), units[0]);
    }
    let h = (u128::from(n) * 100 + scale / 2) / scale;
    (format!("{}.{:02}", h / 100, h % 100), units[unit])
}

enum Row {
    Blank,
    Title(usize, &'static str),
    Value(usize, &'static str, String, &'static str, Option<ObjectId>),
}

fn table(s: &Stats) -> Vec<u8> {
    let types = [&s.commits, &s.trees, &s.blobs, &s.tag_objects];
    let names = [msg::COMMITS, msg::TREES, msg::BLOBS, msg::TAGS];
    let mut rows = Vec::new();
    let count = |n: u64| humanize(n, false);
    let size = |n: u64| humanize(n, true);
    let value = |level, label, (num, unit): (String, &'static str), oid| {
        Row::Value(level, label, num, unit, oid)
    };
    rows.push(Row::Title(0, msg::REFERENCES));
    let refs = [s.branches, s.tags, s.remotes, s.others];
    rows.push(value(1, msg::COUNT, count(refs.iter().sum()), None));
    for (n, label) in refs
        .iter()
        .zip([msg::BRANCHES, msg::TAGS, msg::REMOTES, msg::OTHERS])
    {
        rows.push(value(2, label, count(*n), None));
    }
    rows.push(Row::Blank);
    rows.push(Row::Title(0, msg::REACHABLE_OBJECTS));
    let sections: [(&str, Getter, bool); 3] = [
        (msg::COUNT, |t| t.count, false),
        (msg::INFLATED_SIZE, |t| t.inflated, true),
        (msg::DISK_SIZE, |t| t.disk, true),
    ];
    for (label, get, bytes) in sections {
        let total = types.iter().map(|t| get(t)).sum();
        rows.push(value(1, label, humanize(total, bytes), None));
        for (t, name) in types.iter().zip(names) {
            rows.push(value(2, name, humanize(get(t), bytes), None));
        }
    }
    rows.push(Row::Blank);
    rows.push(Row::Title(0, msg::LARGEST_OBJECTS));
    for (i, (t, name)) in types.iter().zip(names).enumerate() {
        rows.push(Row::Title(1, name));
        let (n, oid) = t.max_size.map_or((0, None), |(n, o)| (n, Some(o)));
        rows.push(value(2, msg::MAXIMUM_SIZE, size(n), oid));
        let fanout_label = match i {
            0 => msg::MAXIMUM_PARENTS,
            1 => msg::MAXIMUM_ENTRIES,
            _ => continue,
        };
        let (n, oid) = t.max_fanout.map_or((0, None), |(n, o)| (n, Some(o)));
        rows.push(value(2, fanout_label, count(n), oid));
    }

    // Column widths.
    let mut footnotes: Vec<ObjectId> = Vec::new();
    let mut labels: Vec<String> = Vec::new();
    for row in &rows {
        labels.push(match row {
            Row::Blank => String::new(),
            Row::Title(level, label) => format!("{}* {label}", "  ".repeat(*level)),
            Row::Value(level, label, _, _, oid) => {
                let indent = "  ".repeat(*level);
                let largest = matches!(
                    *label,
                    msg::MAXIMUM_SIZE | msg::MAXIMUM_PARENTS | msg::MAXIMUM_ENTRIES
                );
                match oid {
                    Some(oid) => {
                        footnotes.push(*oid);
                        format!("{indent}* {label:<15} [{}]", footnotes.len())
                    }
                    None if largest => format!("{indent}* {label:<15}    "),
                    None => format!("{indent}* {label}"),
                }
            }
        });
    }
    let label_w = labels
        .iter()
        .map(String::len)
        .chain([msg::TABLE_HEADER.len()])
        .max()
        .unwrap_or(0);
    let num_w = rows
        .iter()
        .filter_map(|r| match r {
            Row::Value(_, _, num, _, _) => Some(num.len()),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let unit_w = rows
        .iter()
        .filter_map(|r| match r {
            Row::Value(_, _, _, unit, _) => Some(unit.len()),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let mut value_w = num_w + 1 + unit_w;
    if value_w < msg::TABLE_VALUE.len() {
        value_w = msg::TABLE_VALUE.len() + 1;
    }
    let num_w = value_w - 1 - unit_w;

    let mut out = String::new();
    out.push_str(&format!(
        "| {:<label_w$} | {:<value_w$} |\n",
        msg::TABLE_HEADER,
        msg::TABLE_VALUE
    ));
    out.push_str(&format!(
        "| {} | {} |\n",
        "-".repeat(label_w),
        "-".repeat(value_w)
    ));
    for (row, label) in rows.iter().zip(&labels) {
        let cell = match row {
            Row::Value(_, _, num, unit, _) => format!("{num:>num_w$} {unit:<unit_w$}"),
            _ => String::new(),
        };
        out.push_str(&format!("| {label:<label_w$} | {cell:<value_w$} |\n"));
    }
    if !footnotes.is_empty() {
        out.push('\n');
        for (i, oid) in footnotes.iter().enumerate() {
            out.push_str(&format!("[{}] {oid}\n", i + 1));
        }
    }
    out.into_bytes()
}

fn lines(s: &Stats, nul: bool, null: &ObjectId) -> Vec<u8> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let refs = [
        ("branches", s.branches),
        ("tags", s.tags),
        ("remotes", s.remotes),
        ("others", s.others),
    ];
    for (name, n) in refs {
        pairs.push((format!("references.{name}.count"), n.to_string()));
    }
    let types = [
        ("commits", &s.commits),
        ("trees", &s.trees),
        ("blobs", &s.blobs),
        ("tags", &s.tag_objects),
    ];
    let fields: [(&str, Getter); 3] = [
        ("count", |t| t.count),
        ("inflated_size", |t| t.inflated),
        ("disk_size", |t| t.disk),
    ];
    for (field, get) in fields {
        for (name, t) in types {
            pairs.push((format!("objects.{name}.{field}"), get(t).to_string()));
        }
    }
    let max = |key: String, m: Option<(u64, ObjectId)>, pairs: &mut Vec<(String, String)>| {
        let (n, oid) = m.unwrap_or((0, *null));
        pairs.push((key.clone(), n.to_string()));
        pairs.push((format!("{key}_oid"), oid.to_string()));
    };
    for (name, t) in types {
        max(format!("objects.{name}.max_size"), t.max_size, &mut pairs);
    }
    max(
        "objects.commits.max_parents".into(),
        s.commits.max_fanout,
        &mut pairs,
    );
    max(
        "objects.trees.max_entries".into(),
        s.trees.max_fanout,
        &mut pairs,
    );
    let mut out = Vec::new();
    for (k, v) in pairs {
        out.extend_from_slice(k.as_bytes());
        out.push(if nul { b'\n' } else { b'=' });
        out.extend_from_slice(v.as_bytes());
        out.push(if nul { 0 } else { b'\n' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanized() {
        assert_eq!(humanize(999, false), ("999".into(), ""));
        assert_eq!(humanize(1005, false), ("1.01".into(), "k"));
        assert_eq!(humanize(3016, false), ("3.02".into(), "k"));
        assert_eq!(humanize(1024, true), ("1024".into(), "B"));
        assert_eq!(humanize(1100, true), ("1.07".into(), "KiB"));
        assert_eq!(humanize(1 << 20, true), ("1024.00".into(), "KiB"));
    }

    #[test]
    fn relative_paths() {
        let r = |t: &str, b: &str| String::from_utf8(relative_path(Path::new(t), Path::new(b))).unwrap();
        assert_eq!(r("/r/.git", "/r"), ".git");
        assert_eq!(r("/r/.git", "/r/a/b"), "../../.git");
        assert_eq!(r("/r/.git", "/r/.git"), "./");
        assert_eq!(r("/r/.git", "/r/.git/objects"), "../");
        assert_eq!(r("/x/.git", "/r"), "../x/.git");
    }
}
