// SPDX-License-Identifier: MIT
//! Notes support shared by `git notes`, the `log` family (display) and the
//! commands that rewrite commits (`commit --amend`, `rebase`), on top of
//! [`rgit_notes`].

use rgit_config::{ConfigSet, SystemEnv};
use rgit_hash::ObjectId;
use rgit_ident::ident::{IdentConfig, Resolver, Role, Strictness};
use rgit_notes::{Combine, Notes, refname};
use rgit_object::{Object, ObjectKind, Signature, SignatureRef};
use rgit_odb::ObjectDatabase;
use rgit_pathspec::wildmatch::{WildmatchFlags, wildmatch};
use rgit_refs::files::FilesStore;
use rgit_refs::{IterItem, IterOptions, RefStore, RefUpdate, Transaction};

use crate::Die;
use crate::messages::notes as msg;
use crate::refs_support::{self, Objects, UpdateHooks};
use crate::repository::Repo;

/// The notes ref to read and write: `GIT_NOTES_REF`, `core.notesRef` or
/// `refs/notes/commits`.
pub fn default_ref(config: &ConfigSet) -> String {
    let env = std::env::var("GIT_NOTES_REF").ok();
    let cfg = config
        .string("core.notesref")
        .ok()
        .flatten()
        .map(|v| v.to_string());
    refname::default_ref(env.as_deref(), cfg.as_deref())
}

/// The tree of the notes ref (or revision) `name`, `None` when it does not
/// resolve. A commit gives its tree.
pub fn notes_tree(odb: &ObjectDatabase, id: &ObjectId) -> Option<ObjectId> {
    match odb.read_object(id).ok().flatten()? {
        Object::Commit(c) => c.tree().ok(),
        Object::Tree(_) => Some(*id),
        _ => None,
    }
}

/// A notes ref loaded for changing it.
pub struct Loaded {
    /// The full ref name.
    pub refname: String,
    /// The commit the ref points to (the parent of the next notes commit).
    pub commit: Option<ObjectId>,
    /// The notes.
    pub notes: Notes,
    /// Whether the notes changed (a commit is due).
    pub dirty: bool,
}

/// Loads the notes of ref `name` (read through revision syntax, so
/// `refs/notes/commits^{tree}` works for reading). With `writable`, a
/// name that resolves without being a ref is refused.
pub fn load(repo: &mut Repo, name: &str, writable: bool) -> Result<Loaded, Die> {
    let store = refs_support::ref_store(&repo.setup, &repo.config);
    let as_ref = store.resolve_oid(name).ok().flatten();
    let id = match as_ref {
        Some(id) => Some(id),
        None => {
            let resolved = if rgit_refs::name::is_valid_refname(
                name.as_bytes(),
                rgit_refs::name::RefNameFlags::default(),
            ) {
                None
            } else {
                repo.resolve(name)
            };
            if resolved.is_some() && writable {
                return Err(Die::Fatal(msg::cannot_use_ref(name)));
            }
            resolved
        }
    };
    let tree = id.and_then(|id| notes_tree(repo.odb(), &id));
    let commit = id.filter(|id| matches!(repo.object_kind(id), Some(ObjectKind::Commit)));
    let notes = Notes::load(repo.odb(), tree.as_ref()).map_err(|e| Die::Fatal(e.to_string()))?;
    Ok(Loaded {
        refname: name.to_owned(),
        commit,
        notes,
        dirty: false,
    })
}

/// An identity for notes commits.
pub fn signature(config: &ConfigSet, role: Role) -> Result<Signature, Die> {
    let icfg = IdentConfig::from_config(config).map_err(|p| {
        p.print();
        Die::Exit(rgit_options::EXIT_FATAL)
    })?;
    let env = SystemEnv::default();
    let ident = Resolver::new(&icfg, &env)
        .ident(role, Strictness::Strict)
        .map_err(|e| {
            eprint!("{}", e.stderr_text());
            Die::Exit(rgit_options::EXIT_FATAL)
        })?;
    let raw = ident.to_bstring();
    SignatureRef::parse(&raw)
        .and_then(|s| s.to_owned())
        .ok_or_else(|| Die::Fatal(String::from_utf8_lossy(&raw).into_owned()))
}

/// Writes a commit with the tree of `notes`, `parents` and message
/// `message`.
pub fn write_commit(
    odb: &ObjectDatabase,
    config: &ConfigSet,
    notes: &Notes,
    parents: &[ObjectId],
    message: &[u8],
) -> Result<ObjectId, Die> {
    write_commit_by(odb, config, notes, parents, message, None)
}

/// [`write_commit`] with the author `author` instead of the configured
/// one.
pub fn write_commit_by(
    odb: &ObjectDatabase,
    config: &ConfigSet,
    notes: &Notes,
    parents: &[ObjectId],
    message: &[u8],
    author: Option<&Signature>,
) -> Result<ObjectId, Die> {
    let tree = notes.write(odb).map_err(|e| Die::Fatal(e.to_string()))?;
    let author = match author {
        Some(a) => a.clone(),
        None => signature(config, Role::Author)?,
    };
    let committer = signature(config, Role::Committer)?;
    let new = rgit_commit::NewCommit {
        tree,
        parents,
        author: &author,
        committer: &committer,
        encoding: None,
        message,
    };
    rgit_commit::create_commit(odb, &new).map_err(|e| Die::Fatal(e.to_string()))
}

/// Points `refname` at `new` (expected to be at `old`), with the reflog
/// message `notes: <message>`.
pub fn update_ref(
    setup: &rgit_repo::Setup,
    config: &ConfigSet,
    refname: &str,
    new: ObjectId,
    old: Option<ObjectId>,
    message: &str,
) -> Result<(), Die> {
    let store = refs_support::ref_store(setup, config);
    let objects = Objects::open(setup);
    let mut hooks = UpdateHooks::new(setup, config, &objects);
    let update = RefUpdate::update(refname, new, old).message(format!("notes: {message}"));
    let mut tx = Transaction::new(&store);
    tx.push(update)
        .and_then(|()| tx.commit(&mut hooks))
        .map_err(|e| Die::Fatal(format!("update_ref failed for ref '{refname}': {e}")))
}

impl Loaded {
    /// Commits the notes when they changed: a commit on top of the
    /// current one with message `message`, and the ref moved to it.
    pub fn commit(&mut self, repo: &Repo, message: &str) -> Result<(), Die> {
        self.commit_by(repo, message, None)
    }

    /// [`Loaded::commit`] with the author `author`.
    pub fn commit_by(
        &mut self,
        repo: &Repo,
        message: &str,
        author: Option<&Signature>,
    ) -> Result<(), Die> {
        if !self.dirty {
            return Ok(());
        }
        let parents: Vec<ObjectId> = self.commit.into_iter().collect();
        let mut text = message.as_bytes().to_vec();
        text.push(b'\n');
        let new = write_commit_by(
            repo.odb(),
            &repo.config,
            &self.notes,
            &parents,
            &text,
            author,
        )?;
        update_ref(
            &repo.setup,
            &repo.config,
            &self.refname,
            new,
            self.commit,
            message,
        )?;
        self.commit = Some(new);
        self.dirty = false;
        Ok(())
    }
}

/// The names of the existing refs that match the glob `pattern`, sorted.
pub fn glob_refs(store: &FilesStore, pattern: &str) -> Vec<String> {
    let Ok(items) = store.iter(&IterOptions::default()) else {
        return Vec::new();
    };
    let mut out: Vec<String> = items
        .iter()
        .filter_map(|i| match i {
            IterItem::Ref(r) => Some(r.name.clone()),
            _ => None,
        })
        .filter(|n| wildmatch(pattern.as_bytes(), n.as_bytes(), WildmatchFlags::NONE))
        .collect();
    out.sort();
    out
}

/// Whether `s` contains glob characters.
pub fn has_glob(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

/// Copying notes from rewritten commits to their rewrites
/// (`notes.rewrite.<command>`, `notes.rewriteRef`, `notes.rewriteMode`,
/// `GIT_NOTES_REWRITE_REF`, `GIT_NOTES_REWRITE_MODE`).
pub struct Rewrite {
    refs: Vec<Loaded>,
    /// `None` after an invalid mode: nothing is copied, but the refs
    /// still get a commit (observed).
    combine: Option<Combine>,
}

impl Rewrite {
    /// The rewriting set up for `command` (`amend`, `rebase`, or what
    /// `notes copy --for-rewrite` names); `None` when nothing is to be
    /// copied.
    pub fn init(repo: &mut Repo, command: &str) -> Result<Option<Rewrite>, Die> {
        let config = repo.config.clone();
        let env_refs = std::env::var("GIT_NOTES_REWRITE_REF").ok();
        if env_refs.is_none() {
            let key = format!("notes.rewrite.{command}");
            if let Ok(Some(false)) = config.bool(key.as_str()) {
                return Ok(None);
            }
        }
        let combine = match std::env::var("GIT_NOTES_REWRITE_MODE") {
            Ok(v) => {
                let c = Combine::from_rewrite_mode(&v);
                if c.is_none() {
                    eprintln!("error: {}", msg::bad_value("GIT_NOTES_REWRITE_MODE", &v));
                }
                c
            }
            Err(_) => match config.string("notes.rewritemode").ok().flatten() {
                Some(v) => {
                    let v = v.to_string();
                    let c = Combine::from_rewrite_mode(&v);
                    if c.is_none() {
                        eprintln!("error: {}", msg::bad_value("notes.rewriteMode", &v));
                    }
                    c
                }
                None => Some(Combine::Concatenate),
            },
        };
        let patterns: Vec<String> = match &env_refs {
            Some(v) => v
                .split(':')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
            None => config
                .entries()
                .iter()
                .filter(|e| e.name == "notes.rewriteref")
                .filter_map(|e| {
                    e.value
                        .as_ref()
                        .map(|v| String::from_utf8_lossy(v).into_owned())
                })
                .collect(),
        };
        let store = refs_support::ref_store(&repo.setup, &repo.config);
        let mut names: Vec<String> = Vec::new();
        for p in patterns {
            if !p.starts_with(refname::PREFIX) {
                eprintln!("warning: {}", msg::refusing_rewrite(&p));
                continue;
            }
            let found = if has_glob(&p) {
                glob_refs(&store, &p)
            } else {
                vec![p]
            };
            for n in found {
                if !names.contains(&n) {
                    names.push(n);
                }
            }
        }
        if names.is_empty() {
            return Ok(None);
        }
        let mut refs = Vec::new();
        for n in names {
            refs.push(load(repo, &n, true)?);
        }
        Ok(Some(Rewrite { refs, combine }))
    }

    /// Copies the notes of `from` onto `to` in every ref.
    pub fn copy(
        &mut self,
        odb: &ObjectDatabase,
        from: &ObjectId,
        to: &ObjectId,
    ) -> Result<(), Die> {
        for r in &mut self.refs {
            if let (Some(note), Some(combine)) = (r.notes.get(from), self.combine) {
                r.notes
                    .add(odb, *to, note, combine)
                    .map_err(|e| Die::Fatal(e.to_string()))?;
            }
            r.dirty = true;
        }
        Ok(())
    }

    /// Commits every ref with message `message` (and the author
    /// `author` instead of the configured one).
    pub fn finish(
        mut self,
        repo: &Repo,
        message: &str,
        author: Option<&Signature>,
    ) -> Result<(), Die> {
        for r in &mut self.refs {
            r.commit_by(repo, message, author)?;
        }
        Ok(())
    }
}

/// Copies notes for `pairs` of (rewritten, rewrite) after `command`
/// (`amend`, `rebase`) with commit message `message`. `commit --amend`
/// passes the author of the amended commit: git writes the notes commit
/// with it (observed).
pub fn rewrite_after(
    ctx: &crate::Context,
    command: &str,
    pairs: &[(ObjectId, ObjectId)],
    message: &str,
    author: Option<&Signature>,
) -> Result<(), Die> {
    if pairs.is_empty() {
        return Ok(());
    }
    let mut repo = Repo::open(ctx, crate::repo::Mode::Required)?;
    let Some(mut rw) = Rewrite::init(&mut repo, command)? else {
        return Ok(());
    };
    for (from, to) in pairs {
        rw.copy(repo.odb(), from, to)?;
    }
    rw.finish(&repo, message, author)
}

/// [`rewrite_after`] for a rewritten list as the `post-rewrite` hook gets
/// it (`<old> <new>` lines).
pub fn rewrite_list(
    ctx: &crate::Context,
    command: &str,
    list: &[u8],
    message: &str,
) -> Result<(), Die> {
    let mut pairs = Vec::new();
    for line in list.split(|&b| b == b'\n') {
        let mut words = line.split(|&b| b == b' ');
        let (Some(a), Some(b)) = (words.next(), words.next()) else {
            continue;
        };
        if let (Ok(a), Ok(b)) = (ObjectId::from_hex_any(a), ObjectId::from_hex_any(b)) {
            pairs.push((a, b));
        }
    }
    rewrite_after(ctx, command, &pairs, message, None)
}

/// Which notes the `log` family shows (`--notes`, `--no-notes`,
/// `--show-notes`, `--[no-]standard-notes`, `notes.displayRef`,
/// `GIT_NOTES_DISPLAY_REF`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DisplayOptions {
    /// Notes are shown at all (`None`: the command's default).
    pub show: Option<bool>,
    /// The default and display refs are used (`None`: unless
    /// `--notes=<ref>` asked for specific refs).
    pub standard: Option<bool>,
    /// Refs given with `--notes=<ref>` / `--show-notes=<ref>` (expanded).
    pub extra: Vec<String>,
    /// `--show-notes-by-default`: the default notes are shown unless an
    /// option says otherwise, whatever the format.
    pub by_default: bool,
}

impl DisplayOptions {
    /// Handles one of the notes options; `false` when `arg` is none.
    pub fn parse(&mut self, arg: &str) -> bool {
        match arg {
            "--notes" => {
                self.show = Some(true);
                self.standard = Some(true);
            }
            "--show-notes" => self.show = Some(true),
            "--no-notes" => {
                self.show = Some(false);
                self.standard = Some(false);
                self.extra.clear();
            }
            "--show-notes-by-default" => self.by_default = true,
            "--standard-notes" => self.standard = Some(true),
            "--no-standard-notes" => self.standard = Some(false),
            _ => {
                if let Some(r) = arg.strip_prefix("--notes=") {
                    self.show = Some(true);
                    if self.standard.is_none() {
                        self.standard = Some(false);
                    }
                    self.extra.push(refname::expand(r));
                } else if let Some(r) = arg.strip_prefix("--show-notes=") {
                    self.show = Some(true);
                    self.extra.push(refname::expand(r));
                } else {
                    return false;
                }
            }
        }
        true
    }

    /// Whether notes are shown, `default` when no option said so.
    pub fn shown(&self, default: bool) -> bool {
        self.show.unwrap_or(default || self.by_default)
    }
}

/// `notes.displayRef` without a value is an error (checked whenever notes
/// are to be shown).
pub fn check_display_config(config: &ConfigSet) -> Result<(), Die> {
    for e in config.entries() {
        if e.name == "notes.displayref" && e.value.is_none() {
            let name = String::from_utf8_lossy(e.name.as_ref()).into_owned();
            return Err(crate::comment::bad_entry(
                e,
                &crate::messages::comment::missing_value(&name),
            ));
        }
    }
    Ok(())
}

/// The notes of the refs to show, in order.
pub struct Display {
    refs: Vec<(String, Notes)>,
}

impl Display {
    /// Loads the notes refs `opts` select. `resolve` gives the object a
    /// ref name stands for. Fails on `notes.displayRef` without value.
    pub fn load(
        opts: &DisplayOptions,
        config: &ConfigSet,
        store: &FilesStore,
        odb: &ObjectDatabase,
    ) -> Result<Display, Die> {
        check_display_config(config)?;
        let mut names: Vec<(String, bool)> = Vec::new();
        if opts.standard != Some(false) {
            names.push((default_ref(config), false));
            match std::env::var("GIT_NOTES_DISPLAY_REF") {
                Ok(v) => {
                    for p in v.split(':').filter(|s| !s.is_empty()) {
                        names.push((p.to_owned(), false));
                    }
                }
                Err(_) => {
                    for e in config.entries() {
                        if e.name != "notes.displayref" {
                            continue;
                        }
                        let Some(v) = &e.value else {
                            let name = String::from_utf8_lossy(e.name.as_ref()).into_owned();
                            return Err(crate::comment::bad_entry(
                                e,
                                &crate::messages::comment::missing_value(&name),
                            ));
                        };
                        names.push((String::from_utf8_lossy(v).into_owned(), false));
                    }
                }
            }
        }
        for e in &opts.extra {
            names.push((e.clone(), true));
        }
        let mut refs: Vec<(String, Notes)> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for (name, warn) in names {
            let found = if has_glob(&name) {
                glob_refs(store, &name)
            } else {
                match store.resolve_oid(&name).ok().flatten() {
                    Some(_) => vec![name],
                    None => {
                        if warn {
                            eprintln!("warning: {}", msg::invalid_display_ref(&name));
                        }
                        Vec::new()
                    }
                }
            };
            for n in found {
                if seen.contains(&n) {
                    continue;
                }
                seen.push(n.clone());
                let Some(id) = store.resolve_oid(&n).ok().flatten() else {
                    continue;
                };
                let tree = notes_tree(odb, &id);
                if let Ok(notes) = Notes::load(odb, tree.as_ref()) {
                    refs.push((n, notes));
                }
            }
        }
        Ok(Display { refs })
    }

    /// The notes of `id` as `%N` shows them: the notes of all refs, one
    /// after the other.
    pub fn raw(&self, odb: &ObjectDatabase, id: &ObjectId) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        let mut any = false;
        for (_, notes) in &self.refs {
            if let Some(note) = notes.get(id) {
                if let Some(data) = blob(odb, &note) {
                    any = true;
                    out.extend_from_slice(&data);
                }
            }
        }
        any.then_some(out)
    }

    /// The notes of `id` as the built-in formats show them: for every ref
    /// with a note `\nNotes (<ref>):\n` and the indented lines.
    pub fn formatted(&self, odb: &ObjectDatabase, id: &ObjectId) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, notes) in &self.refs {
            if let Some(note) = notes.get(id) {
                if let Some(data) = blob(odb, &note) {
                    out.push(b'\n');
                    rgit_notes::display::append(&mut out, name, &data, true);
                }
            }
        }
        out
    }
}

/// The content of blob `id`.
pub fn blob(odb: &ObjectDatabase, id: &ObjectId) -> Option<Vec<u8>> {
    match odb.read(id).ok().flatten() {
        Some(raw) if raw.kind == ObjectKind::Blob => Some(raw.data),
        _ => None,
    }
}
