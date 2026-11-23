// SPDX-License-Identifier: MIT
//! `git submodule summary`: the commits between the recorded and the
//! current state of every changed submodule.

use std::collections::BTreeMap;
use std::path::Path;

use rgit_hash::ObjectId;

use crate::messages::submodule_cmd as msg;
use crate::prelude::*;

use super::common::{self, Super};

/// One side of a change: a gitlink, a blob or nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Absent,
    Gitlink(ObjectId),
    Blob(ObjectId),
}

impl Side {
    fn oid(self, null: ObjectId) -> ObjectId {
        match self {
            Side::Absent => null,
            Side::Gitlink(o) | Side::Blob(o) => o,
        }
    }
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let cached = Cell::new(false);
    let files = Cell::new(false);
    let for_status = Cell::new(false);
    let limit = Cell::new(-1i32);
    let quiet = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE_SUMMARY,
        vec![
            Opt::flag(None, "cached", &cached, msg::HELP_SUMMARY_CACHED),
            Opt::flag(None, "files", &files, msg::HELP_FILES),
            Opt::flag(None, "for-status", &for_status, msg::HELP_FOR_STATUS),
            Opt::integer('n', "summary-limit", &limit, msg::HELP_SUMMARY_LIMIT),
            Opt::flag('q', "quiet", &quiet, "").hidden(),
        ],
    )
    .flags(ParseFlags {
        keep_dashdash: true,
        ..Default::default()
    });
    let mut args = parser.parse(argv)?.args;
    drop(parser);
    if cached.get() && files.get() {
        return Err(Die::Fatal(msg::CACHED_AND_FILES.into()));
    }
    let mut sup = Super::open(ctx, None)?;
    // An optional commit, then paths.
    let mut rev: Option<String> = None;
    if let Some(first) = args.first().cloned() {
        if first == "--" {
            args.remove(0);
        } else {
            let id = sup.repo.resolve(&format!("{first}^{{commit}}"));
            if id.is_some() {
                rev = Some(first);
                args.remove(0);
                if args.first().map(String::as_str) == Some("--") {
                    args.remove(0);
                }
            }
        }
    }
    if let Some(i) = args.iter().position(|a| a == "--") {
        args.remove(i);
    }
    if limit.get() == 0 {
        return Ok(0);
    }
    if files.get() && rev.is_some() {
        return Err(Die::Fatal(msg::CACHED_AND_FILES.into()));
    }
    let null = ObjectId::null(sup.hash());

    // The paths to look at: the index, filtered.
    let list = sup.module_list_all(&args)?;
    let (index, _) = sup.repo.read_index()?;
    let index_side = |path: &[u8]| -> Side {
        match index.get(path, rgit_index::Stage::MERGED) {
            Some(e) if e.mode.is_gitlink() => Side::Gitlink(e.id),
            Some(e) => Side::Blob(e.id),
            None => Side::Absent,
        }
    };
    let worktree_side = |path: &str, in_index: Side| -> Side {
        if in_index == Side::Absent {
            return Side::Absent;
        }
        let full = sup.work_tree.join(path);
        match std::fs::symlink_metadata(&full) {
            Ok(m) if m.is_dir() => match common::sub_head(&full, sup.hash()) {
                Some(h) => Side::Gitlink(h),
                None => match in_index {
                    Side::Gitlink(o) => Side::Gitlink(o),
                    _ => Side::Absent,
                },
            },
            Ok(m) if m.is_file() => std::fs::read(&full)
                .ok()
                .and_then(|d| rgit_hash::hash_object(sup.hash(), "blob", &d).ok())
                .map_or(Side::Absent, Side::Blob),
            _ => Side::Absent,
        }
    };
    let tree: BTreeMap<String, Side> = if files.get() {
        BTreeMap::new()
    } else {
        tree_entries(&sup, rev.as_deref().unwrap_or("HEAD"))?
    };

    let mut changes: Vec<(String, Side, Side)> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for path in &list {
        seen.insert(path.clone());
        let idx = index_side(path.as_bytes());
        let (src, dst) = if files.get() {
            (idx, worktree_side(path, idx))
        } else if cached.get() {
            (tree.get(path).copied().unwrap_or(Side::Absent), idx)
        } else {
            (
                tree.get(path).copied().unwrap_or(Side::Absent),
                worktree_side(path, idx),
            )
        };
        changes.push((path.clone(), src, dst));
    }
    if !files.get() {
        // Submodules removed from the index.
        let ps = sup.pathspec(&args)?;
        for (path, side) in &tree {
            if !seen.contains(path) && ps.match_path(path.as_bytes()).is_some() {
                changes.push((path.clone(), *side, Side::Absent));
            }
        }
        changes.sort_by(|a, b| a.0.cmp(&b.0));
    }

    let mut out = String::new();
    for (path, src, dst) in changes {
        if src == dst {
            continue;
        }
        // A submodule that is not checked out (its directory empty) is
        // not compared with its work tree.
        let full = sup.work_tree.join(&path);
        if !cached.get() && full.is_dir() && !common::is_populated(&full) {
            continue;
        }
        let src_link = matches!(src, Side::Gitlink(_));
        let dst_link = matches!(dst, Side::Gitlink(_));
        if !src_link && !dst_link {
            continue;
        }
        if for_status.get() {
            let gm = sup.gitmodules();
            if let Some(name) = gm.name_for_path(&path) {
                let key = format!("submodule.{name}.ignore");
                let ignore = common::get_string(&sup.repo.config, &key).or_else(|| gm.get(&name, "ignore"));
                if ignore.as_deref() == Some("all") {
                    continue;
                }
            }
        }
        out.push_str(&summarize(&sup, &path, src, dst, null, limit.get()));
    }
    if !quiet.get() {
        print!("{out}");
    }
    Ok(0)
}

fn short(oid: ObjectId) -> String {
    oid.to_hex()[..7].to_owned()
}

/// The block of one changed submodule.
fn summarize(sup: &Super, path: &str, src: Side, dst: Side, null: ObjectId, limit: i32) -> String {
    let display = sup.display_path(path);
    let typechange = matches!(
        (src, dst),
        (Side::Gitlink(_), Side::Blob(_)) | (Side::Blob(_), Side::Gitlink(_))
    );
    let (s, d) = (src.oid(null), dst.oid(null));
    let mut text = if typechange {
        let kind = |x: Side| {
            if matches!(x, Side::Gitlink(_)) {
                msg::SUMMARY_SUBMODULE
            } else {
                msg::SUMMARY_BLOB
            }
        };
        format!(
            "* {display} {}({})->{}({})",
            short(s),
            kind(src),
            short(d),
            kind(dst)
        )
    } else {
        format!("* {display} {}...{}", short(s), short(d))
    };
    let dir = sup.work_tree.join(path);
    let repo_ok = common::is_populated(&dir);
    let mut body = String::new();
    if repo_ok {
        let exists = |o: ObjectId| has_commit(sup, &dir, &o.to_hex());
        let missing_src = matches!(src, Side::Gitlink(_)) && !exists(s);
        let missing_dst = matches!(dst, Side::Gitlink(_)) && !exists(d);
        if missing_src && missing_dst {
            body.push_str(&msg::summary_missing_commits(&display, &s.to_hex(), &d.to_hex()));
            body.push('\n');
        } else if missing_src || missing_dst {
            let o = if missing_src { s } else { d };
            body.push_str(&msg::summary_does_not_contain(&display, &o.to_hex()));
            body.push('\n');
        } else {
            let (range, one): (Vec<String>, Option<&str>) = match (src, dst) {
                (Side::Gitlink(a), Side::Gitlink(b)) => (vec![format!("{}...{}", a.to_hex(), b.to_hex())], None),
                (Side::Gitlink(a), _) => (vec![a.to_hex()], Some("<")),
                (_, Side::Gitlink(b)) => (vec![b.to_hex()], Some(">")),
                _ => (Vec::new(), None),
            };
            let mut count_args: Vec<&str> = vec!["rev-list", "--count"];
            count_args.extend(range.iter().map(String::as_str));
            let count = common::capture(&mut match sup.sub_git(&dir, &count_args) {
                Ok(c) => c,
                Err(_) => return String::new(),
            })
            .map(|c| c.trim().to_owned());
            if let Some(count) = count {
                text.push_str(&format!(" ({count})"));
                let pretty = match one {
                    Some(m) => format!("--pretty=format:  {m} %s"),
                    None => "--pretty=format:  %m %s".to_owned(),
                };
                let n = match (one, limit) {
                    (Some(_), _) => "-1".to_owned(),
                    (None, l) if l > 0 => format!("-{l}"),
                    _ => String::new(),
                };
                let mut log_args: Vec<&str> = vec!["log", &pretty, "--first-parent"];
                if !n.is_empty() {
                    log_args.push(&n);
                }
                log_args.extend(range.iter().map(String::as_str));
                if let Ok(mut c) = sup.sub_git(&dir, &log_args) {
                    if let Some(lines) = common::capture(&mut c) {
                        let lines = lines.trim_end_matches('\n');
                        if !lines.is_empty() {
                            body.push_str(lines);
                            body.push('\n');
                        }
                    }
                }
            }
        }
    }
    text.push_str(":\n");
    text.push_str(&body);
    text.push('\n');
    text
}

fn has_commit(sup: &Super, dir: &Path, hex: &str) -> bool {
    sup.sub_git(dir, &["rev-parse", "-q", "--verify", &format!("{hex}^{{commit}}")])
        .ok()
        .and_then(|mut c| common::capture(&mut c))
        .is_some()
}

/// Gitlinks and blobs of the tree of `rev` (empty when it does not exist).
fn tree_entries(sup: &Super, rev: &str) -> Result<BTreeMap<String, Side>, Die> {
    let mut out = BTreeMap::new();
    let Some(text) = common::capture(&mut sup.git(&["ls-tree", "-r", "-z", rev])?) else {
        return Ok(out);
    };
    for rec in text.split('\0').filter(|r| !r.is_empty()) {
        let Some((meta, path)) = rec.split_once('\t') else {
            continue;
        };
        let mut f = meta.split(' ');
        let (Some(mode), Some(_), Some(oid)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let Ok(oid) = ObjectId::from_hex(sup.hash(), oid.as_bytes()) else {
            continue;
        };
        let side = if mode == "160000" {
            Side::Gitlink(oid)
        } else {
            Side::Blob(oid)
        };
        out.insert(path.to_owned(), side);
    }
    Ok(out)
}
