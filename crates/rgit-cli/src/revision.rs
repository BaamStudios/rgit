// SPDX-License-Identifier: MIT
//! Object names (`gitrevisions(7)`), the subset needed by the plumbing
//! commands until the revision crate exists (#32):
//!
//! - full and abbreviated hex ids (ambiguity reported like git),
//! - ref names with git's lookup rules, `@` for `HEAD`,
//! - `<rev>^`, `<rev>^<n>`, `<rev>~<n>`, `<rev>^{<type>}`, `<rev>^{}`,
//!   `<rev>^{/<regex>}`, `<ref>@{<n>}`,
//! - `<rev>:<path>`, `:<path>`, `:<n>:<path>`.

use std::collections::{BTreeSet, VecDeque};

use rgit_hash::{ObjectId, Prefix};
use rgit_object::{Commit, ObjectKind, Tag, TreeIter};
use rgit_odb::PrefixMatch;

use crate::messages::objects as msg;
use crate::repo_objects::Repo;

/// An object name resolved to an id.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub oid: ObjectId,
    /// The tree entry mode when the name was `<tree-ish>:<path>`.
    pub mode: Option<u32>,
}

/// Why a name did not resolve.
#[derive(Debug, Clone)]
pub enum Failure {
    /// No such object. `diagnosis` is the more specific `fatal:` message
    /// git gives for single-object commands (`path 'x' does not exist in
    /// 'HEAD'`).
    Missing { diagnosis: Option<String> },
    /// A short id matches several objects (already reported on stderr).
    Ambiguous,
    /// git dies even in batch mode (`log for 'HEAD' only has 1 entries`).
    Fatal(String),
}

impl Failure {
    fn missing() -> Failure {
        Failure::Missing { diagnosis: None }
    }

    fn diag(s: String) -> Failure {
        Failure::Missing { diagnosis: Some(s) }
    }
}

type Res<T> = Result<T, Failure>;

/// Where `--follow-symlinks` ended when it did not find an object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymlinkEnd {
    /// A link points to something that does not exist.
    Dangling,
    /// A path goes through something that is not a directory.
    NotDir,
    /// Too many links.
    Loop,
    /// A link leaves the tree; the path outside, relative to the top.
    OutOfTree(Vec<u8>),
}

/// Resolves object names in a repository.
pub struct Resolver<'a> {
    pub repo: &'a mut Repo,
    /// Path of the current directory below the top of the work tree,
    /// with a trailing `/` (empty at the top or without a work tree).
    prefix: String,
}

/// Symbolic links followed before giving up (`--follow-symlinks`).
const MAX_SYMLINKS: usize = 40;

/// Shortest prefix git accepts as an abbreviated id.
const MIN_ABBREV: usize = 4;

impl<'a> Resolver<'a> {
    pub fn new(repo: &'a mut Repo) -> Resolver<'a> {
        let prefix = repo
            .setup
            .prefix
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        Resolver { repo, prefix }
    }

    /// Read an object (after replacement).
    fn read(&mut self, oid: &ObjectId) -> Option<(ObjectKind, Vec<u8>)> {
        let real = self.repo.replaced(oid);
        let raw = self.repo.odb.read(&real).ok()??;
        Some((raw.kind, raw.data))
    }

    fn kind(&mut self, oid: &ObjectId) -> Option<ObjectKind> {
        let real = self.repo.replaced(oid);
        Some(self.repo.odb.header(&real).ok()??.kind)
    }

    /// Resolve `name`.
    pub fn resolve(&mut self, name: &str) -> Res<Resolved> {
        if let Some(rest) = name.strip_prefix(':') {
            return self.index_path(rest);
        }
        if let Some(i) = tree_path_colon(name) {
            let (rev, path) = (&name[..i], &name[i + 1..]);
            let oid = match self.rev(rev) {
                Ok(oid) => oid,
                Err(Failure::Missing { .. }) => {
                    return Err(Failure::diag(msg::invalid_object_name(rev)));
                }
                Err(e) => return Err(e),
            };
            let tree = self
                .peel_to(oid, ObjectKind::Tree, None)
                .ok_or_else(|| Failure::diag(msg::invalid_object_name(rev)))?;
            let path = self.relative_path(path);
            return match self.tree_lookup(tree, path.as_bytes()) {
                Some((oid, mode)) => Ok(Resolved { oid, mode }),
                None => Err(Failure::diag(self.path_not_in_tree(&path, rev))),
            };
        }
        self.rev(name).map(|oid| Resolved { oid, mode: None })
    }

    /// `<tree-ish>:<path>` with `--follow-symlinks`: `Ok(Err(end))` when
    /// the path cannot be followed to an object.
    pub fn resolve_follow(&mut self, name: &str) -> Res<Result<Resolved, SymlinkEnd>> {
        let Some(i) = tree_path_colon(name).filter(|_| !name.starts_with(':')) else {
            return self.resolve(name).map(Ok);
        };
        let (rev, path) = (&name[..i], &name[i + 1..]);
        let oid = self.rev(rev)?;
        let tree = self
            .peel_to(oid, ObjectKind::Tree, None)
            .ok_or_else(Failure::missing)?;
        match self.follow(tree, path.as_bytes()) {
            Followed::Found(oid, mode) => Ok(Ok(Resolved { oid, mode })),
            Followed::Missing => Err(Failure::missing()),
            Followed::End(e) => Ok(Err(e)),
        }
    }

    fn path_not_in_tree(&self, path: &str, rev: &str) -> String {
        let on_disk = self
            .repo
            .setup
            .work_tree
            .as_ref()
            .is_some_and(|wt| std::fs::symlink_metadata(wt.join(path)).is_ok());
        if on_disk && !path.is_empty() {
            msg::path_on_disk_not_in_rev(path, rev)
        } else {
            msg::path_not_in_rev(path, rev)
        }
    }

    /// A path after `:` is relative to the top, unless it starts with
    /// `./` or `../`.
    fn relative_path(&self, path: &str) -> String {
        if !(path.starts_with("./") || path.starts_with("../") || path == "." || path == "..") {
            return path.to_string();
        }
        let mut parts: Vec<&str> = self.prefix.split('/').filter(|s| !s.is_empty()).collect();
        for c in path.split('/') {
            match c {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                c => parts.push(c),
            }
        }
        let mut out = parts.join("/");
        if path.ends_with('/') && !out.is_empty() {
            out.push('/');
        }
        out
    }

    /// `:<path>`, `:<n>:<path>`: an index entry.
    fn index_path(&mut self, rest: &str) -> Res<Resolved> {
        if rest.starts_with('/') {
            return Err(Failure::missing());
        }
        let b = rest.as_bytes();
        let (stage, path) = if b.len() >= 2 && (b'0'..=b'3').contains(&b[0]) && b[1] == b':' {
            (b[0] - b'0', &rest[2..])
        } else {
            (0, rest)
        };
        let path = self.relative_path(path);
        let setup = &self.repo.setup;
        let index_file = rgit_index::index_path(&setup.git_dir);
        let index = rgit_index::Index::read_file(
            &index_file,
            self.repo.hash,
            &rgit_index::ReadOptions::default(),
        )
        .ok();
        let entries = index
            .as_ref()
            .map(|i| i.entries_for_path(path.as_bytes()).to_vec())
            .unwrap_or_default();
        if let Some(e) = entries.iter().find(|e| e.stage().get() == stage) {
            return Ok(Resolved {
                oid: e.id,
                mode: Some(e.mode.0),
            });
        }
        if let Some(e) = entries.first() {
            return Err(Failure::diag(msg::path_not_at_stage(
                &path,
                stage,
                e.stage().get(),
            )));
        }
        let on_disk = setup
            .work_tree
            .as_ref()
            .is_some_and(|wt| std::fs::symlink_metadata(wt.join(&path)).is_ok());
        Err(Failure::diag(if on_disk {
            msg::path_on_disk_not_in_index(&path)
        } else {
            msg::path_not_in_index(&path)
        }))
    }

    /// A revision without `:<path>`.
    pub fn rev(&mut self, s: &str) -> Res<ObjectId> {
        if s.ends_with('}') {
            if let Some(open) = matching_brace(s) {
                if open >= 1 {
                    let content = &s[open + 1..s.len() - 1];
                    match s.as_bytes()[open - 1] {
                        b'^' => {
                            let base = self.rev(&s[..open - 1])?;
                            return self.peel_spec(base, content, s);
                        }
                        b'@' => return self.reflog(&s[..open - 1], content),
                        _ => {}
                    }
                }
            }
        }
        let b = s.as_bytes();
        let digits = b.iter().rev().take_while(|c| c.is_ascii_digit()).count();
        if b.len() > digits {
            let op = b[b.len() - digits - 1];
            if op == b'^' || op == b'~' {
                let n: usize = if digits == 0 {
                    1
                } else {
                    s[s.len() - digits..]
                        .parse()
                        .map_err(|_| Failure::missing())?
                };
                let base = self.rev(&s[..s.len() - digits - 1])?;
                return if op == b'^' {
                    self.parent(base, n)
                } else {
                    let mut cur = base;
                    for _ in 0..n {
                        cur = self.parent(cur, 1)?;
                    }
                    self.peel_to(cur, ObjectKind::Commit, None)
                        .ok_or_else(Failure::missing)
                };
            }
        }
        self.base(s)
    }

    /// The `n`th parent (0: the commit itself).
    fn parent(&mut self, oid: ObjectId, n: usize) -> Res<ObjectId> {
        let commit = self
            .peel_to(oid, ObjectKind::Commit, None)
            .ok_or_else(Failure::missing)?;
        if n == 0 {
            return Ok(commit);
        }
        let (_, data) = self.read(&commit).ok_or_else(Failure::missing)?;
        let c = Commit::parse(self.repo.hash, &data).map_err(|_| Failure::missing())?;
        let parents = c.parents().map_err(|_| Failure::missing())?;
        parents.get(n - 1).copied().ok_or_else(Failure::missing)
    }

    /// `^{...}`.
    fn peel_spec(&mut self, oid: ObjectId, spec: &str, whole: &str) -> Res<ObjectId> {
        if spec.is_empty() {
            let mut cur = oid;
            for _ in 0..64 {
                match self.kind(&cur) {
                    Some(ObjectKind::Tag) => {
                        cur = self.tag_target(&cur).ok_or_else(Failure::missing)?
                    }
                    Some(_) => return Ok(cur),
                    None => return Err(Failure::missing()),
                }
            }
            return Err(Failure::missing());
        }
        if let Some(re) = spec.strip_prefix('/') {
            return self.search_message(oid, re);
        }
        if spec == "object" {
            return self.kind(&oid).map(|_| oid).ok_or_else(Failure::missing);
        }
        let want = ObjectKind::from_name(spec.as_bytes()).ok_or_else(Failure::missing)?;
        self.peel_to(oid, want, Some(whole))
            .ok_or_else(Failure::missing)
    }

    fn tag_target(&mut self, oid: &ObjectId) -> Option<ObjectId> {
        let (_, data) = self.read(oid)?;
        Tag::parse(self.repo.hash, &data).ok()?.object().ok()
    }

    /// Peel `oid` to an object of kind `want` (tags to their target,
    /// commits to their tree). With `report`, a failure is reported like
    /// git (`error: <name>: expected <type> type, but ...`).
    pub fn peel_to(
        &mut self,
        oid: ObjectId,
        want: ObjectKind,
        report: Option<&str>,
    ) -> Option<ObjectId> {
        let mut cur = oid;
        for _ in 0..64 {
            let kind = self.kind(&cur)?;
            if kind == want {
                return Some(cur);
            }
            let next = match kind {
                ObjectKind::Tag => self.tag_target(&cur),
                ObjectKind::Commit if want == ObjectKind::Tree => {
                    let (_, data) = self.read(&cur)?;
                    Commit::parse(self.repo.hash, &data).ok()?.tree().ok()
                }
                ObjectKind::Commit => {
                    let (_, data) = self.read(&cur)?;
                    let tree = Commit::parse(self.repo.hash, &data).ok()?.tree().ok();
                    if let (Some(name), Some(t)) = (report, tree) {
                        let tk = self.kind(&t).unwrap_or(ObjectKind::Tree);
                        eprintln!("error: {}", msg::peel_mismatch(name, want, tk));
                    }
                    return None;
                }
                _ => {
                    if let Some(name) = report {
                        eprintln!("error: {}", msg::peel_mismatch(name, want, kind));
                    }
                    return None;
                }
            };
            cur = next?;
        }
        None
    }

    /// `^{/<regex>}`: the youngest commit reachable from `oid` whose
    /// message matches.
    fn search_message(&mut self, oid: ObjectId, pattern: &str) -> Res<ObjectId> {
        let (negate, pattern) = match pattern.strip_prefix('!') {
            Some(p) if p.starts_with('-') => (true, &p[1..]),
            Some(p) if p.starts_with('!') => (false, p),
            Some(_) => return Err(Failure::missing()),
            None => (false, pattern),
        };
        let re = regex::bytes::RegexBuilder::new(pattern)
            .unicode(false)
            .build()
            .map_err(|_| Failure::missing())?;
        let start = self
            .peel_to(oid, ObjectKind::Commit, None)
            .ok_or_else(Failure::missing)?;
        // Newest first by committer date.
        let mut queue: Vec<(i64, ObjectId)> = Vec::new();
        let mut seen = BTreeSet::new();
        seen.insert(start);
        let push = |me: &mut Self, q: &mut Vec<(i64, ObjectId)>, id: ObjectId| -> Option<Commit> {
            let (_, data) = me.read(&id)?;
            let c = Commit::parse(me.repo.hash, &data).ok()?;
            let t = c
                .committer()
                .and_then(|s| s.time())
                .map(|t| t.seconds)
                .unwrap_or(0);
            q.push((t, id));
            Some(c)
        };
        push(self, &mut queue, start);
        while !queue.is_empty() {
            queue.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
            let (_, id) = queue.pop().expect("not empty");
            let Some((_, data)) = self.read(&id) else {
                continue;
            };
            let Ok(c) = Commit::parse(self.repo.hash, &data) else {
                continue;
            };
            if re.is_match(c.message()) != negate {
                return Ok(id);
            }
            for p in c.parents().unwrap_or_default() {
                if seen.insert(p) {
                    push(self, &mut queue, p);
                }
            }
        }
        Err(Failure::missing())
    }

    /// `<ref>@{<n>}`.
    fn reflog(&mut self, base: &str, spec: &str) -> Res<ObjectId> {
        let n: usize = spec.parse().map_err(|_| Failure::missing())?;
        let full = if base.is_empty() {
            self.repo.refs.head_target().ok_or_else(Failure::missing)?
        } else if base == "HEAD" || base == "@" {
            "HEAD".to_string()
        } else {
            self.dwim_ref(base).ok_or_else(Failure::missing)?
        };
        let entries = self.repo.refs.reflog(&full).ok_or_else(Failure::missing)?;
        if n < entries.len() {
            Ok(entries[entries.len() - 1 - n])
        } else {
            let shown = if base.is_empty() { &full } else { base };
            Err(Failure::Fatal(msg::reflog_too_short(shown, entries.len())))
        }
    }

    /// The full ref name `name` stands for (git's ref lookup rules).
    fn dwim_ref(&self, name: &str) -> Option<String> {
        let pseudo = name.starts_with("refs/")
            || (!name.is_empty()
                && name
                    .bytes()
                    .all(|c| c.is_ascii_uppercase() || c == b'_' || c == b'-')
                && name.ends_with("HEAD"));
        let candidates = [
            pseudo.then(|| name.to_string()),
            Some(format!("refs/{name}")),
            Some(format!("refs/tags/{name}")),
            Some(format!("refs/heads/{name}")),
            Some(format!("refs/remotes/{name}")),
            Some(format!("refs/remotes/{name}/HEAD")),
        ];
        candidates
            .into_iter()
            .flatten()
            .find(|c| self.repo.refs.resolve(c).is_some())
    }

    /// A name without suffixes: hex id or ref.
    fn base(&mut self, s: &str) -> Res<ObjectId> {
        if s.is_empty() {
            return Err(Failure::missing());
        }
        let s = if s == "@" { "HEAD" } else { s };
        let hash = self.repo.hash;
        let is_hex = s.bytes().all(|c| c.is_ascii_hexdigit());
        if is_hex && s.len() == hash.hex_len() {
            if let Ok(oid) = ObjectId::from_hex(hash, s.as_bytes()) {
                return Ok(oid);
            }
        }
        if let Some(full) = self.dwim_ref(s) {
            if let Some(oid) = self.repo.refs.resolve(&full) {
                return Ok(oid);
            }
        }
        if is_hex && s.len() >= MIN_ABBREV && s.len() < hash.hex_len() {
            let prefix = Prefix::from_hex(hash, s.as_bytes()).map_err(|_| Failure::missing())?;
            return match self.repo.odb.resolve_prefix(&prefix) {
                Ok(PrefixMatch::Unique(oid)) => Ok(oid),
                Ok(PrefixMatch::Ambiguous(ids)) => {
                    self.report_ambiguous(s, &ids);
                    Err(Failure::Ambiguous)
                }
                _ => Err(Failure::missing()),
            };
        }
        Err(Failure::missing())
    }

    /// `error: short object ID ... is ambiguous` with the candidates.
    fn report_ambiguous(&mut self, short: &str, ids: &[ObjectId]) {
        let mut lines: Vec<(u8, ObjectId, String)> = Vec::new();
        let len = self.unique_abbrev_len(ids);
        for id in ids {
            let abbrev = id.to_hex_with_len(len);
            let real = self.repo.replaced(id);
            let raw = self.repo.odb.read(&real).ok().flatten();
            let (order, text) = match raw {
                Some(raw) => match raw.kind {
                    ObjectKind::Tag => {
                        let t = Tag::parse(self.repo.hash, &raw.data).ok();
                        let name = t
                            .as_ref()
                            .and_then(|t| t.name().map(|n| String::from_utf8_lossy(n).into_owned()))
                            .unwrap_or_default();
                        let date = t
                            .as_ref()
                            .and_then(|t| t.tagger().and_then(|s| s.time()))
                            .map(|t| short_date(t.seconds, t.offset_minutes))
                            .unwrap_or_default();
                        (0, msg::candidate_tag(&abbrev, &date, &name))
                    }
                    ObjectKind::Commit => {
                        let c = Commit::parse(self.repo.hash, &raw.data).ok();
                        let date = c
                            .as_ref()
                            .and_then(|c| c.committer().and_then(|s| s.time()))
                            .map(|t| short_date(t.seconds, t.offset_minutes))
                            .unwrap_or_default();
                        let subject = c.as_ref().map(|c| subject(c.message())).unwrap_or_default();
                        (1, msg::candidate_commit(&abbrev, &date, &subject))
                    }
                    ObjectKind::Tree => (2, msg::candidate_other(&abbrev, "tree")),
                    ObjectKind::Blob => (3, msg::candidate_other(&abbrev, "blob")),
                },
                None => (4, msg::candidate_bad(&abbrev)),
            };
            lines.push((order, *id, text));
        }
        lines.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        let mut err = String::new();
        err.push_str(&format!("error: {}\n", msg::short_id_ambiguous(short)));
        err.push_str(&format!("hint: {}\n", msg::CANDIDATES_ARE));
        for (_, _, t) in lines {
            err.push_str(&format!("hint:   {t}\n"));
        }
        eprint!("{err}");
    }

    /// Length of the abbreviation that keeps every id in `ids` unique
    /// among all objects, at least the default abbreviation length.
    fn unique_abbrev_len(&mut self, ids: &[ObjectId]) -> usize {
        let hex_len = self.repo.hash.hex_len();
        let mut len = default_abbrev(self.repo);
        let all: Vec<ObjectId> = self
            .repo
            .odb
            .all_objects()
            .map(|s| s.into_iter().collect())
            .unwrap_or_default();
        for id in ids {
            let h = id.to_hex();
            loop {
                if len >= hex_len {
                    break;
                }
                let p = &h[..len];
                let clash = all.iter().any(|o| o != id && o.to_hex().starts_with(p));
                if !clash {
                    break;
                }
                len += 1;
            }
        }
        len
    }

    /// Look up `path` (components separated by `/`) below `tree`.
    pub fn tree_lookup(&mut self, tree: ObjectId, path: &[u8]) -> Option<(ObjectId, Option<u32>)> {
        if path.first() == Some(&b'/') {
            return None;
        }
        let mut cur = tree;
        let mut mode = None;
        let comps: Vec<&[u8]> = path
            .split(|&c| c == b'/')
            .filter(|c| !c.is_empty())
            .collect();
        for (i, comp) in comps.iter().enumerate() {
            let (kind, data) = self.read(&cur)?;
            if kind != ObjectKind::Tree {
                return None;
            }
            let entry = TreeIter::new(self.repo.hash, &data)
                .filter_map(Result::ok)
                .find(|e| e.name == *comp)?;
            if i + 1 < comps.len() && !entry.is_tree() {
                return None;
            }
            cur = entry.oid;
            mode = Some(entry.mode_bits);
        }
        if path.last() == Some(&b'/') && mode.is_some_and(|m| m & 0o170000 != 0o040000) {
            return None;
        }
        Some((cur, mode))
    }

    /// Entries of a tree object (name, mode, id).
    fn tree_entries(&mut self, tree: &ObjectId) -> Option<Vec<(Vec<u8>, u32, ObjectId)>> {
        let (kind, data) = self.read(tree)?;
        if kind != ObjectKind::Tree {
            return None;
        }
        Some(
            TreeIter::new(self.repo.hash, &data)
                .filter_map(Result::ok)
                .map(|e| (e.name.to_vec(), e.mode_bits, e.oid))
                .collect(),
        )
    }

    /// Walk `path` from `root`, following symbolic links inside the tree.
    fn follow(&mut self, root: ObjectId, path: &[u8]) -> Followed {
        // Directories from the root down to the current one: (name, tree).
        let mut dirs: Vec<ObjectId> = vec![root];
        let mut todo: VecDeque<Vec<u8>> = path.split(|&c| c == b'/').map(<[u8]>::to_vec).collect();
        let mut links = 0usize;
        let mut current: Option<(ObjectId, u32)> = None;
        while let Some(comp) = todo.pop_front() {
            if comp.is_empty() || comp == b"." {
                continue;
            }
            if let Some((_, mode)) = current {
                if mode & 0o170000 != 0o040000 {
                    return Followed::End(SymlinkEnd::NotDir);
                }
            }
            if comp == b".." {
                if dirs.len() == 1 {
                    let mut rest = comp.clone();
                    for c in todo {
                        rest.push(b'/');
                        rest.extend_from_slice(&c);
                    }
                    return Followed::End(SymlinkEnd::OutOfTree(rest));
                }
                dirs.pop();
                current = None;
                continue;
            }
            let dir = *dirs.last().expect("root");
            let Some(entries) = self.tree_entries(&dir) else {
                return Followed::Missing;
            };
            let Some((_, mode, oid)) = entries.into_iter().find(|e| e.0 == comp) else {
                return if links > 0 {
                    Followed::End(SymlinkEnd::Dangling)
                } else {
                    Followed::Missing
                };
            };
            match mode & 0o170000 {
                0o120000 => {
                    links += 1;
                    if links > MAX_SYMLINKS {
                        return Followed::End(SymlinkEnd::Loop);
                    }
                    let Some((_, target)) = self.read(&oid) else {
                        return Followed::Missing;
                    };
                    if target.first() == Some(&b'/') {
                        return Followed::End(SymlinkEnd::OutOfTree(target));
                    }
                    for c in target.split(|&c| c == b'/').rev() {
                        todo.push_front(c.to_vec());
                    }
                    current = None;
                }
                0o040000 => {
                    dirs.push(oid);
                    current = Some((oid, mode));
                }
                _ => current = Some((oid, mode)),
            }
        }
        match current {
            Some((oid, mode)) => Followed::Found(oid, Some(mode)),
            None => {
                let top = *dirs.last().expect("root");
                if dirs.len() == 1 {
                    Followed::Found(top, None)
                } else {
                    Followed::Found(top, Some(0o040000))
                }
            }
        }
    }
}

enum Followed {
    Found(ObjectId, Option<u32>),
    Missing,
    End(SymlinkEnd),
}

/// The default abbreviation length: `core.abbrev`, else derived from the
/// number of objects (at least 7).
pub fn default_abbrev(repo: &Repo) -> usize {
    let hex_len = repo.hash.hex_len();
    if let Ok(Some(v)) = repo.config.string("core.abbrev") {
        let v = v.to_string();
        if v.eq_ignore_ascii_case("no") {
            return hex_len;
        }
        if let Ok(n) = v.parse::<usize>() {
            return n.clamp(MIN_ABBREV, hex_len);
        }
    }
    let count = repo.approximate_object_count().max(1) as u64;
    let bits = 64 - count.leading_zeros() as usize;
    bits.div_ceil(2).max(7).min(hex_len)
}

/// The position of the `:` separating `<rev>` from `<path>`, outside of
/// `^{...}` and `@{...}`.
fn tree_path_colon(name: &str) -> Option<usize> {
    let b = name.as_bytes();
    let mut depth = 0usize;
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'{' if i > 0 && (b[i - 1] == b'^' || b[i - 1] == b'@') => depth += 1,
            b'}' if depth > 0 => depth -= 1,
            b':' if depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

/// The index of the `{` matching the final `}`.
fn matching_brace(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth = 0usize;
    for i in (0..b.len()).rev() {
        match b[i] {
            b'}' => depth += 1,
            b'{' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The first line of a message, with surrounding blank lines skipped.
fn subject(message: &[u8]) -> String {
    let text = String::from_utf8_lossy(message);
    let mut out = Vec::new();
    for line in text.lines().skip_while(|l| l.trim().is_empty()) {
        if line.trim().is_empty() {
            break;
        }
        out.push(line.trim());
    }
    out.join(" ")
}

/// `YYYY-MM-DD` in the given time zone.
fn short_date(seconds: i64, offset_minutes: i32) -> String {
    let t = seconds + i64::from(offset_minutes) * 60;
    let days = t.div_euclid(86400);
    // Civil from days (proleptic Gregorian).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colon() {
        assert_eq!(tree_path_colon("HEAD:a"), Some(4));
        assert_eq!(tree_path_colon("x^{/a:b}:c"), Some(8));
        assert_eq!(tree_path_colon("foo{bar:hello"), Some(7));
        assert_eq!(tree_path_colon("HEAD"), None);
    }

    #[test]
    fn dates() {
        assert_eq!(short_date(1112911993, -420), "2005-04-07");
        assert_eq!(short_date(0, 0), "1970-01-01");
    }

    #[test]
    fn subjects() {
        assert_eq!(subject(b"\n\nfirst\nsecond\n\nbody"), "first second");
    }
}
