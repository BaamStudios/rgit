// SPDX-License-Identifier: MIT
//! Resolving one revision expression (no ranges).

use rgit_date::approxidate_careful;
use rgit_hash::ObjectId;
use rgit_object::{ObjectKind, TreeIter};

use super::abbrev::looks_hex;
use super::refname::{Interpreted, parse_previous, plausible_refname, tracking_keyword};
use super::{Error, Flags, Hint, Level, Resolver, messages};

/// What `^{...}` asks for.
enum Peel {
    Kind(ObjectKind),
    Object,
    NonTag,
    Search(String),
}

/// Position of the `:` that separates `<rev>` from `<path>`, skipping
/// `@{...}` and `^{...}` groups.
fn find_path_colon(name: &str) -> Option<usize> {
    let b = name.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'{' if i > 0 && (b[i - 1] == b'@' || b[i - 1] == b'^') => {
                i += name[i..].find('}')? + 1;
                continue;
            }
            b':' => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// `<base>^<n>` / `<base>~<n>` at the end of `name`.
fn split_ancestry(name: &str) -> Option<(&str, u8, &str)> {
    let b = name.as_bytes();
    let mut i = b.len();
    while i > 0 && b[i - 1].is_ascii_digit() {
        i -= 1;
    }
    if i == 0 {
        return None;
    }
    let op = b[i - 1];
    if op != b'^' && op != b'~' {
        return None;
    }
    Some((&name[..i - 1], op, &name[i..]))
}

/// `<anything>-<n>-g<hex>` (output of `git describe`): the hex part.
fn describe_hex(name: &str) -> Option<&str> {
    let pos = name.rfind("-g")?;
    let hex = &name[pos + 2..];
    if hex.len() < 4 || !looks_hex(hex) {
        return None;
    }
    let before = &name[..pos];
    let dash = before.rfind('-')?;
    let num = &before[dash + 1..];
    if dash == 0 || num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(hex)
}

impl Resolver<'_> {
    /// git's `get_oid_with_context`.
    pub(crate) fn get_oid(&self, name: &str, flags: Flags) -> Result<ObjectId, Error> {
        if let Some(pattern) = name.strip_prefix(":/") {
            if pattern.is_empty() {
                return Err(Error::NotFound);
            }
            let starts = self.all_ref_tips()?;
            return self.search_message(&starts, pattern);
        }
        if let Some(rest) = name.strip_prefix(':') {
            let rb = rest.as_bytes();
            let (stage, path) = if rb.len() >= 2 && (b'0'..=b'3').contains(&rb[0]) && rb[1] == b':'
            {
                (rb[0] - b'0', &rest[2..])
            } else {
                (0, rest)
            };
            return self.index_path(stage, path, flags);
        }
        if let Some(colon) = find_path_colon(name) {
            let rev = &name[..colon];
            let path = &name[colon + 1..];
            let tree_flags = Flags {
                hint: Hint::Treeish,
                ..flags
            };
            let base = match self.get_oid_1(rev, tree_flags) {
                Ok(o) => o,
                Err(Error::NotFound) if flags.diagnose => {
                    return Err(Error::Fatal(messages::invalid_object_name(rev)));
                }
                Err(e) => return Err(e),
            };
            return self.tree_path(&base, rev, path, flags);
        }
        self.get_oid_1(name, flags)
    }

    /// git's `get_oid_1`: suffixes, then names.
    fn get_oid_1(&self, name: &str, flags: Flags) -> Result<ObjectId, Error> {
        // A failed `^{...}` falls through to the other forms (as in git,
        // where the whole name may still parse as `<ref>@{<date>}`).
        if name.ends_with('}') {
            if let Some(pos) = name.rfind("^{") {
                match self.peel_suffix(name, pos, flags) {
                    Err(Error::NotFound) => {}
                    other => return other,
                }
            }
        }
        if let Some((base, op, digits)) = split_ancestry(name) {
            let n: u64 = if digits.is_empty() {
                1
            } else {
                digits.parse().map_err(|_| Error::NotFound)?
            };
            // git looks the base up with fresh flags: neither quiet nor
            // diagnosing.
            let base_flags = Flags {
                hint: Hint::Committish,
                ..Flags::default()
            };
            let oid = self.get_oid_1(base, base_flags)?;
            let commit = self.peel_to_commit(&oid)?;
            if op == b'^' {
                if n == 0 {
                    return Ok(commit);
                }
                let parents = self.parents_of(&commit)?;
                return usize::try_from(n - 1)
                    .ok()
                    .and_then(|i| parents.get(i).copied())
                    .ok_or(Error::NotFound);
            }
            let mut cur = commit;
            for _ in 0..n {
                cur = *self.parents_of(&cur)?.first().ok_or(Error::NotFound)?;
            }
            return Ok(cur);
        }
        match self.get_oid_basic(name, flags) {
            Err(Error::NotFound) => {}
            other => return other,
        }
        if let Some(hex) = describe_hex(name) {
            let f = Flags {
                hint: Hint::Commit,
                ..flags
            };
            return self.short_oid(hex, f);
        }
        Err(Error::NotFound)
    }

    fn peel_suffix(&self, name: &str, pos: usize, flags: Flags) -> Result<ObjectId, Error> {
        let base = &name[..pos];
        let spec = &name[pos + 2..];
        let peel = if spec.starts_with("commit}") {
            Peel::Kind(ObjectKind::Commit)
        } else if spec.starts_with("tag}") {
            Peel::Kind(ObjectKind::Tag)
        } else if spec.starts_with("tree}") {
            Peel::Kind(ObjectKind::Tree)
        } else if spec.starts_with("blob}") {
            Peel::Kind(ObjectKind::Blob)
        } else if spec.starts_with("object}") {
            Peel::Object
        } else if spec.starts_with('}') {
            Peel::NonTag
        } else if let Some(re) = spec.strip_prefix('/') {
            Peel::Search(re[..re.len() - 1].to_string())
        } else {
            return Err(Error::NotFound);
        };
        let hint = match peel {
            Peel::Kind(ObjectKind::Commit) | Peel::Search(_) => Hint::Committish,
            Peel::Kind(ObjectKind::Tree) => Hint::Treeish,
            _ => Hint::None,
        };
        let oid = self.get_oid_1(base, Flags { hint, ..flags })?;
        match peel {
            Peel::Object => match self.kind_of(&oid)? {
                Some(_) => Ok(oid),
                None => Err(Error::NotFound),
            },
            Peel::NonTag => self.peel_tags(&oid)?.map(|p| p.0).ok_or(Error::NotFound),
            Peel::Search(re) => {
                let commit = self.peel_to_commit(&oid)?;
                if re.is_empty() {
                    return Ok(commit);
                }
                self.search_message(&[commit], &re)
            }
            Peel::Kind(want) => {
                let mut cur = oid;
                for _ in 0..1000 {
                    let Some(kind) = self.kind_of(&cur)? else {
                        return Err(Error::NotFound);
                    };
                    if kind == want {
                        return Ok(cur);
                    }
                    let next = match kind {
                        ObjectKind::Tag => self.tag_target(&cur)?,
                        ObjectKind::Commit => self.commit_info(&cur)?.map(|c| c.tree),
                        _ => None,
                    };
                    match next {
                        Some(n) => cur = n,
                        None => {
                            // Reported even in quiet lookups, like git.
                            if matches!(kind, ObjectKind::Tree | ObjectKind::Blob) {
                                self.push_diag(
                                    Level::Error,
                                    messages::expected_type(name, want.name(), kind.name()),
                                );
                            }
                            return Err(Error::NotFound);
                        }
                    }
                }
                Err(Error::NotFound)
            }
        }
    }

    /// git's `get_oid_basic`: hex ids, reflog and tracking suffixes, ref
    /// names, abbreviated ids.
    fn get_oid_basic(&self, name: &str, flags: Flags) -> Result<ObjectId, Error> {
        let kind = self.repo.objects.hash_kind();
        if name.len() == kind.hex_len() && looks_hex(name) {
            let oid = ObjectId::from_hex(kind, name.as_bytes()).map_err(|_| Error::NotFound)?;
            if self.opts.warn_ambiguous_refs
                && !flags.quiet
                && !flags.diagnose
                && self.expand_ref(name)?.count > 0
            {
                let mut text = messages::refname_ambiguous(name);
                if self.opts.advice_object_name_warning {
                    text.push('\n');
                    text.push_str(messages::OBJECT_NAME_WARNING_ADVICE);
                }
                self.push_diag(Level::Warning, text);
            }
            return Ok(oid);
        }
        if let Some(inner) = name.strip_suffix('}') {
            if let Some(at) = inner.rfind("@{") {
                let content = &inner[at + 2..];
                if let Some((kind, kw_len)) = tracking_keyword(&name[at + 2..]) {
                    // git takes `@{u}` even when more follows the `}`; the
                    // rest is then left over and the name does not resolve.
                    let full = self.branch_tracking(&name[..at], kind)?;
                    if at + 2 + kw_len + 1 != name.len() {
                        return Err(Error::NotFound);
                    }
                    return self
                        .repo
                        .refs
                        .resolve(&full)?
                        .and_then(|v| v.oid)
                        .ok_or(Error::NotFound);
                }
                if content.starts_with('-') {
                    if at == 0 {
                        if let Some(n) = parse_previous(name) {
                            return match self.previous_branch(n)? {
                                Some(b) => self.get_oid_basic(&b, flags),
                                None => Err(Error::NotFound),
                            };
                        }
                    }
                    return Err(Error::NotFound);
                }
                return self.reflog_entry(&name[..at], content, flags);
            }
        }
        let name = if name == "@" { "HEAD" } else { name };
        let found = self.expand_ref(name)?;
        if let Some(oid) = found.oid {
            if self.opts.warn_ambiguous_refs && !flags.diagnose {
                let ambiguous = found.count > 1
                    || (name.len() >= 4
                        && looks_hex(name)
                        && self.prefix_candidates(name)?.is_some_and(|c| c.len() == 1));
                if ambiguous {
                    self.push_diag(Level::Warning, messages::refname_ambiguous(name));
                }
            }
            return Ok(oid);
        }
        if looks_hex(name) {
            return self.short_oid(name, flags);
        }
        Err(Error::NotFound)
    }

    /// `<ref>@{<n>}` and `<ref>@{<date>}`.
    fn reflog_entry(&self, refpart: &str, content: &str, flags: Flags) -> Result<ObjectId, Error> {
        let (full, display) = if refpart.is_empty() {
            let head = self.repo.refs.resolve("HEAD")?.ok_or(Error::NotFound)?;
            match head.name.strip_prefix("refs/heads/") {
                Some(b) => (head.name.clone(), b.to_string()),
                None => ("HEAD".to_string(), "HEAD".to_string()),
            }
        } else {
            match self.log_ref(refpart)? {
                Some(full) => (full, refpart.to_string()),
                None => return Err(Error::NotFound),
            }
        };
        let Some(log) = self.repo.refs.reflog(&full)? else {
            return Err(Error::NotFound);
        };
        if log.is_empty() {
            return Err(Error::NotFound);
        }
        if !content.is_empty() && content.bytes().all(|b| b.is_ascii_digit()) {
            let n: usize = content.parse().map_err(|_| Error::NotFound)?;
            if n >= log.len() {
                return Err(Error::Fatal(messages::log_only_has(&display, log.len())));
            }
            return Ok(log[log.len() - 1 - n].new);
        }
        let (at, bad) = approxidate_careful(content, &self.opts.date);
        if bad {
            return Err(Error::NotFound);
        }
        let entries: Vec<rgit_refs::reflog::Entry> = log
            .iter()
            .map(|e| rgit_refs::reflog::Entry {
                old: e.old,
                new: e.new,
                ident: Vec::new(),
                time: e.time,
                tz: e.tz,
                message: Vec::new(),
            })
            .collect();
        let current = self.repo.refs.resolve(&full)?.and_then(|v| v.oid);
        let target = rgit_refs::reflog::lookup::Target {
            refname: &full,
            display: &display,
            current,
        };
        let resolved =
            rgit_refs::reflog::resolve(&entries, rgit_refs::reflog::Selector::Date(at), target)
                .map_err(|e| Error::Fatal(e.to_string()))?;
        // `-q` silences only `only goes back`; gaps and a log that ended
        // early are always reported.
        for w in &resolved.warnings {
            let only_goes_back =
                matches!(w, rgit_refs::reflog::lookup::Warning::OnlyGoesBack { .. });
            if !flags.diagnose && !(flags.quiet && only_goes_back) {
                self.push_diag(Level::Warning, w.to_string());
            }
        }
        Ok(resolved.oid)
    }

    /// The full ref whose reflog `<name>@{...}` reads (git's `dwim_log`).
    fn log_ref(&self, name: &str) -> Result<Option<String>, Error> {
        if let Some(i) = self.interpret_branch_name(name)? {
            return Ok(match i {
                Interpreted::Full(f) => Some(f),
                Interpreted::Name(n) => {
                    let d = self.expand_ref(&n)?;
                    (d.count > 0).then_some(d.full_name)
                }
            });
        }
        for (i, rule) in super::REF_RULES.iter().enumerate() {
            if i == 0
                && !(name.starts_with("refs/")
                    || name.bytes().all(|b| b.is_ascii_uppercase() || b == b'_'))
            {
                continue;
            }
            let full = rule.replace("%s", name);
            if !plausible_refname(&full) {
                continue;
            }
            if self.repo.refs.resolve(&full)?.is_none() {
                continue;
            }
            if self.repo.refs.reflog(&full)?.is_some() {
                return Ok(Some(full));
            }
        }
        Ok(None)
    }

    /// Tips of all refs and `HEAD` (start points of `:/<regex>`).
    fn all_ref_tips(&self) -> Result<Vec<ObjectId>, Error> {
        let mut tips = Vec::new();
        if let Some(oid) = self.repo.refs.resolve("HEAD")?.and_then(|v| v.oid) {
            tips.push(oid);
        }
        self.repo
            .refs
            .for_each_ref("refs/", &mut |_, oid| tips.push(*oid))?;
        let mut commits = Vec::new();
        for t in tips {
            if let Some((c, ObjectKind::Commit)) = self.peel_tags(&t)? {
                commits.push(c);
            }
        }
        Ok(commits)
    }

    /// Turn `./` and `../` paths into paths from the top of the work tree.
    fn relative_path(&self, path: &str) -> Result<String, Error> {
        let is_relative =
            path == "." || path == ".." || path.starts_with("./") || path.starts_with("../");
        if !is_relative {
            return Ok(path.to_string());
        }
        let Some(info) = &self.repo.work_tree_info else {
            return Err(Error::Fatal(
                messages::RELATIVE_OUTSIDE_WORK_TREE.to_string(),
            ));
        };
        if !info.cwd_inside {
            return Err(Error::Fatal(
                messages::RELATIVE_OUTSIDE_WORK_TREE.to_string(),
            ));
        }
        let mut parts: Vec<&str> = info.prefix.split('/').filter(|s| !s.is_empty()).collect();
        let trailing_slash = path.ends_with('/');
        for comp in path.split('/') {
            match comp {
                "" | "." => {}
                ".." => {
                    if parts.pop().is_none() {
                        return Err(Error::Fatal(messages::outside_repository(path, &info.path)));
                    }
                }
                c => parts.push(c),
            }
        }
        let mut out = parts.join("/");
        if trailing_slash && !out.is_empty() {
            out.push('/');
        }
        Ok(out)
    }

    /// `:<n>:<path>` and `:<path>`.
    fn index_path(&self, stage: u8, path: &str, flags: Flags) -> Result<ObjectId, Error> {
        let path = self.relative_path(path)?;
        if let Some(index) = self.repo.index {
            if let Some(oid) = index.entry(path.as_bytes(), stage) {
                return Ok(oid);
            }
        }
        if flags.diagnose {
            return Err(Error::Fatal(self.diagnose_index_path(stage, &path)));
        }
        Err(Error::NotFound)
    }

    fn diagnose_index_path(&self, stage: u8, path: &str) -> String {
        let index = self.repo.index;
        let in_index = |p: &str, s: u8| index.is_some_and(|i| i.entry(p.as_bytes(), s).is_some());
        if stage != 0 && in_index(path, 0) {
            self.push_diag(Level::Hint, messages::did_you_mean_stage0(path));
            return messages::path_not_at_stage(path, stage);
        }
        let prefix = self
            .repo
            .work_tree_info
            .as_ref()
            .map(|i| i.prefix.clone())
            .unwrap_or_default();
        if !prefix.is_empty() {
            let full = format!("{prefix}{path}");
            if in_index(&full, stage) || in_index(&full, 0) {
                self.push_diag(Level::Hint, messages::did_you_mean_index_path(&full, path));
                return messages::path_in_index_but_not(&full, path);
            }
        }
        if self.repo.work_tree.is_some_and(|w| w.exists(path)) {
            return messages::path_on_disk_not_in_index(path);
        }
        messages::path_not_anywhere(path)
    }

    /// Look `path` up in the tree of `base` (`<rev>:<path>`).
    fn tree_path(
        &self,
        base: &ObjectId,
        rev: &str,
        path: &str,
        flags: Flags,
    ) -> Result<ObjectId, Error> {
        let path = self.relative_path(path)?;
        let tree = match self.peel_tags(base)? {
            Some((t, ObjectKind::Tree)) => t,
            Some((c, ObjectKind::Commit)) => match self.commit_info(&c)? {
                Some(i) => i.tree,
                None => return Err(Error::NotFound),
            },
            _ => return Err(Error::NotFound),
        };
        if let Some(oid) = self.lookup_tree_path(&tree, &path)? {
            return Ok(oid);
        }
        if flags.diagnose {
            return Err(Error::Fatal(self.diagnose_tree_path(&tree, rev, &path)?));
        }
        Err(Error::NotFound)
    }

    fn diagnose_tree_path(&self, tree: &ObjectId, rev: &str, path: &str) -> Result<String, Error> {
        if self.repo.work_tree.is_some_and(|w| w.exists(path)) {
            return Ok(messages::path_on_disk_not_in_rev(path, rev));
        }
        let prefix = self
            .repo
            .work_tree_info
            .as_ref()
            .map(|i| i.prefix.clone())
            .unwrap_or_default();
        if !prefix.is_empty() {
            let full = format!("{prefix}{path}");
            if self.lookup_tree_path(tree, &full)?.is_some() {
                self.push_diag(
                    Level::Hint,
                    messages::did_you_mean_rev_path(rev, &full, path),
                );
                return Ok(messages::path_exists_but_not(&full, path));
            }
        }
        Ok(messages::path_not_in_rev(path, rev))
    }

    /// Walk `path` through trees starting at `tree`. An empty path is the
    /// tree itself; a trailing slash requires a tree.
    pub(crate) fn lookup_tree_path(
        &self,
        tree: &ObjectId,
        path: &str,
    ) -> Result<Option<ObjectId>, Error> {
        if path.is_empty() {
            return Ok(Some(*tree));
        }
        let (path, want_tree) = match path.strip_suffix('/') {
            Some(p) => (p, true),
            None => (path, false),
        };
        let hash = self.repo.objects.hash_kind();
        let mut cur = *tree;
        let mut is_tree = true;
        for comp in path.split('/') {
            if !is_tree || comp.is_empty() {
                return Ok(None);
            }
            let Some((kind, data)) = self.repo.objects.read(&cur)? else {
                return Ok(None);
            };
            if kind != ObjectKind::Tree {
                return Ok(None);
            }
            let mut next = None;
            for entry in TreeIter::new(hash, &data) {
                let Ok(entry) = entry else {
                    return Ok(None);
                };
                if entry.name == comp.as_bytes() {
                    next = Some((entry.oid, entry.is_tree()));
                    break;
                }
            }
            let Some((oid, t)) = next else {
                return Ok(None);
            };
            cur = oid;
            is_tree = t;
        }
        if want_tree && !is_tree {
            return Ok(None);
        }
        Ok(Some(cur))
    }
}
