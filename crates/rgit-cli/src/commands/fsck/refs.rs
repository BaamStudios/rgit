// SPDX-License-Identifier: MIT
//! The references side of `fsck`: consistency of the loose ref files
//! (`git refs verify` for the files backend, the subset rgit knows), and
//! the heads of the reachability walk (refs, `HEAD` of every worktree,
//! reflogs, indexes and their cache trees).

use std::path::{Path, PathBuf};

use rgit_hash::ObjectId;
use rgit_object::ObjectKind;
use rgit_object::fsck::{MsgId, Severity};

use super::{ERROR_REACHABLE, ERROR_REFS, Fsck};
use crate::messages::fsck as msg;
use crate::repo_objects::Repo;

/// A worktree's git directory and how its per-worktree refs are named.
struct Wt {
    git_dir: PathBuf,
    /// `""` for the current worktree, `worktrees/<id>/` for others.
    prefix: String,
    /// Shown name of its index (`.git/index`, `.git/worktrees/<id>/index`).
    index_name: String,
    current: bool,
}

/// The current worktree first, then the others (main, linked).
fn worktrees(repo: &Repo) -> Vec<Wt> {
    let setup = &repo.setup;
    let common = &setup.common_dir;
    let mut out = vec![Wt {
        git_dir: setup.git_dir.clone(),
        prefix: String::new(),
        index_name: setup.git_dir.join("index").display().to_string(),
        current: true,
    }];
    let cur = crate::worktrees::real(&setup.git_dir);
    if crate::worktrees::real(common) != cur {
        out.push(Wt {
            git_dir: common.clone(),
            prefix: "main-worktree/".into(),
            index_name: common.join("index").display().to_string(),
            current: false,
        });
    }
    for id in crate::worktrees::linked_ids(common) {
        let dir = common.join("worktrees").join(&id);
        if crate::worktrees::real(&dir) == cur {
            continue;
        }
        out.push(Wt {
            index_name: dir.join("index").display().to_string(),
            git_dir: dir,
            prefix: format!("worktrees/{id}/"),
            current: false,
        });
    }
    out
}

fn report(f: &Fsck, name: &str, id: MsgId, text: String) -> i32 {
    match f.severity(id) {
        Severity::Ignore => 0,
        Severity::Error | Severity::Fatal => {
            eprintln!("{}", msg::ref_finding("error", name, &text));
            ERROR_REFS
        }
        _ => {
            eprintln!("{}", msg::ref_finding("warning", name, &text));
            0
        }
    }
}

/// Check the content of one loose ref file.
fn check_ref_file(f: &Fsck, name: &str, path: &Path) -> i32 {
    let Ok(data) = std::fs::read(path) else {
        return 0;
    };
    if let Some(target) = data.strip_prefix(b"ref:") {
        let target = String::from_utf8_lossy(target).trim().to_string();
        if name.ends_with("HEAD") && !name.starts_with("refs/") && !target.starts_with("refs/heads/") {
            return report(f, name, MsgId::BadHeadTarget, msg::bad_head_target(&target));
        }
        return 0;
    }
    let hex = f.hash.hex_len();
    let valid = data.len() >= hex
        && data[..hex].iter().all(|b| b.is_ascii_hexdigit())
        && ObjectId::from_hex(f.hash, &data[..hex]).is_ok();
    if !valid {
        let shown = String::from_utf8_lossy(&data).trim_end_matches('\n').to_string();
        return report(f, name, MsgId::BadRefContent, msg::bad_ref_content(&shown));
    }
    let oid = ObjectId::from_hex(f.hash, &data[..hex]).expect("checked");
    let mut errors = 0;
    if oid.is_null() {
        errors |= report(f, name, MsgId::BadRefOid, msg::bad_ref_oid(&oid.to_string()));
    }
    let rest = &data[hex..];
    if rest.is_empty() {
        errors |= report(f, name, MsgId::RefMissingNewline, msg::REF_MISSING_NEWLINE.into());
    } else if rest != b"\n" {
        let shown = String::from_utf8_lossy(rest).into_owned();
        if !rest[0].is_ascii_whitespace() {
            errors |= report(f, name, MsgId::BadRefContent, msg::bad_ref_content(String::from_utf8_lossy(&data).trim_end_matches('\n')));
        } else {
            errors |= report(f, name, MsgId::TrailingRefContent, msg::trailing_ref_content(&shown));
        }
    }
    errors
}

/// Loose ref files below `dir/refs`, sorted by name.
fn loose_refs(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let mut stack = vec![(String::from("refs"), dir.join("refs"))];
    while let Some((name, path)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&path) else {
            continue;
        };
        for e in rd.filter_map(Result::ok) {
            let n = e.file_name().to_string_lossy().into_owned();
            let child = format!("{name}/{n}");
            let p = e.path();
            if p.is_dir() {
                stack.push((child, p));
            } else if !n.ends_with(".lock") {
                out.push((child, p));
            }
        }
    }
    out.sort();
    out
}

/// `--references`: check the ref files.
pub(crate) fn verify(repo: &Repo, f: &Fsck) -> i32 {
    let mut errors = 0;
    if f.opts.verbose {
        eprintln!("{}", msg::CHECKING_REFS_CONSISTENCY);
    }
    for (name, path) in loose_refs(&repo.setup.common_dir) {
        if f.opts.verbose {
            eprintln!("{}", msg::checking(&name));
        }
        errors |= check_ref_file(f, &name, &path);
    }
    for wt in worktrees(repo) {
        if wt.prefix == "main-worktree/" {
            errors |= check_ref_file(f, "HEAD", &wt.git_dir.join("HEAD"));
            continue;
        }
        let name = format!("{}HEAD", wt.prefix);
        if f.opts.verbose {
            eprintln!("{}", msg::checking(&name));
        }
        errors |= check_ref_file(f, &name, &wt.git_dir.join("HEAD"));
    }
    if f.opts.verbose {
        let packed = repo.setup.common_dir.join("packed-refs");
        eprintln!("{}", msg::checking(&format!("packed-refs file {}", packed.display())));
    }
    errors
}

/// The value of a ref file (following symbolic refs through `repo`).
fn read_head(repo: &Repo, git_dir: &Path) -> Option<ObjectId> {
    let data = std::fs::read(git_dir.join("HEAD")).ok()?;
    if let Some(target) = data.strip_prefix(b"ref:") {
        let target = String::from_utf8_lossy(target).trim().to_string();
        return repo.refs.resolve(&target);
    }
    let hex = repo.hash.hex_len();
    let oid = ObjectId::from_hex(repo.hash, data.get(..hex)?).ok()?;
    (!oid.is_null()).then_some(oid)
}

/// Add a head, reporting a missing object. Returns whether it counted.
fn head(f: &mut Fsck, heads: &mut Vec<(ObjectId, String)>, oid: ObjectId, name: &str) {
    if !f.objs.get(&oid).is_some_and(|o| o.has) {
        eprintln!("error: {}", msg::invalid_sha1_pointer(name, &oid.to_string()));
        f.errors |= ERROR_REACHABLE;
        return;
    }
    if name.starts_with("refs/heads/") && f.objs.get(&oid).and_then(|o| o.kind) != Some(ObjectKind::Commit) {
        eprintln!("error: {}", msg::not_a_commit(name));
        f.errors |= ERROR_REFS;
    }
    heads.push((oid, name.to_string()));
}

/// Refs, worktree `HEAD`s and reflogs. Returns the number of refs found.
pub(crate) fn default_heads(f: &mut Fsck, repo: &Repo, heads: &mut Vec<(ObjectId, String)>) -> usize {
    let mut count = 0;
    let store = rgit_refs::files::FilesStore::new(&repo.setup.common_dir, repo.hash);
    use rgit_refs::RefStore;
    let items = store
        .iter(&rgit_refs::IterOptions::prefix(""))
        .unwrap_or_default();
    for item in items {
        count += 1;
        let name = item.name().to_string();
        match repo.refs.resolve(&name) {
            Some(oid) => head(f, heads, oid, &name),
            None => {
                // Unreadable content counts as the null id.
                let null = repo.hash.null();
                eprintln!("error: {}", msg::invalid_sha1_pointer(&name, &null.to_string()));
                f.errors |= ERROR_REACHABLE;
            }
        }
    }
    let wts = worktrees(repo);
    for wt in &wts {
        if wt.prefix == "main-worktree/" {
            continue;
        }
        let name = format!("{}HEAD", wt.prefix);
        if let Some(oid) = read_head(repo, &wt.git_dir) {
            count += 1;
            head(f, heads, oid, &name);
        } else if let Ok(data) = std::fs::read(wt.git_dir.join("HEAD")) {
            // A detached HEAD whose object is missing.
            let hex = repo.hash.hex_len();
            if let Some(oid) = data.get(..hex).and_then(|h| ObjectId::from_hex(repo.hash, h).ok()) {
                if !oid.is_null() {
                    count += 1;
                    head(f, heads, oid, &name);
                }
            }
        }
    }
    if f.opts.reflogs {
        for wt in &wts {
            if wt.prefix == "main-worktree/" {
                continue;
            }
            reflog_heads(f, repo, &wt.git_dir.join("logs").join("HEAD"), &format!("{}HEAD", wt.prefix), heads);
        }
        for (name, path) in reflog_files(&repo.setup.common_dir) {
            reflog_heads(f, repo, &path, &name, heads);
        }
    }
    count
}

/// Reflog files below `dir/logs/refs`, sorted by ref name.
fn reflog_files(dir: &Path) -> Vec<(String, PathBuf)> {
    loose_refs(&dir.join("logs"))
}

fn reflog_heads(f: &mut Fsck, repo: &Repo, path: &Path, name: &str, heads: &mut Vec<(ObjectId, String)>) {
    let Ok(data) = std::fs::read(path) else {
        return;
    };
    for e in rgit_refs::reflog::Entry::parse_all(&data, repo.hash) {
        if f.opts.verbose {
            eprintln!("{}", msg::checking(&format!("reflog {}->{}", e.old, e.new)));
        }
        for oid in [e.old, e.new] {
            if oid.is_null() {
                continue;
            }
            if !f.objs.get(&oid).is_some_and(|o| o.has) {
                eprintln!("error: {}", msg::invalid_reflog_entry(name, &oid.to_string()));
                f.errors |= ERROR_REACHABLE;
                continue;
            }
            heads.push((oid, format!("{name}@{{{}}}", e.time)));
        }
    }
}

/// Index entries and cache trees of every worktree.
pub(crate) fn index_heads(f: &mut Fsck, repo: &Repo, heads: &mut Vec<(ObjectId, String)>) {
    for wt in worktrees(repo) {
        let path = if wt.current {
            match std::env::var_os("GIT_INDEX_FILE") {
                Some(p) if !p.is_empty() => PathBuf::from(p),
                _ => wt.git_dir.join("index"),
            }
        } else {
            wt.git_dir.join("index")
        };
        if !path.exists() {
            continue;
        }
        let index = match rgit_index::Index::read_file(&path, repo.hash, &rgit_index::ReadOptions::verified()) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("error: {e}");
                f.errors |= ERROR_REFS;
                continue;
            }
        };
        let label = if wt.current { String::new() } else { wt.index_name.clone() };
        if f.opts.verbose {
            eprintln!("{}", msg::checking(&format!("cache tree of {}", wt.index_name)));
        }
        for e in index.entries() {
            if e.mode.is_gitlink() {
                continue;
            }
            let name = format!("{label}:{}", String::from_utf8_lossy(e.path()));
            let o = f.obj(&e.id);
            o.used = true;
            if o.kind.is_none() {
                o.kind = Some(ObjectKind::Blob);
            }
            heads.push((e.id, name));
        }
        if let Some(tree) = index.cache_tree() {
            let mut stack = vec![(&tree.root, String::new())];
            while let Some((node, dir)) = stack.pop() {
                if let Some(id) = node.id() {
                    if !f.objs.get(id).is_some_and(|o| o.has) {
                        eprintln!("error: {}", msg::invalid_cache_tree(&id.to_string(), &wt.index_name));
                        f.errors |= ERROR_REFS;
                        continue;
                    } else {
                        let o = f.obj(id);
                        o.used = true;
                        heads.push((*id, format!("{label}:{dir}")));
                    }
                }
                for c in node.children.iter().rev() {
                    stack.push((c, format!("{dir}{}/", String::from_utf8_lossy(&c.name))));
                }
            }
        }
    }
}
