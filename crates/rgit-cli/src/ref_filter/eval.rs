// SPDX-License-Identifier: MIT
//! The values of atoms for one reference, and expanding a parsed format
//! (opening atoms `%(if)` and `%(align)`, quoting, colours).

use std::cell::OnceCell;
use std::cmp::Ordering;
use std::collections::HashMap;

use bstr::ByteSlice;
use rgit_config::ConfigSet;
use rgit_date::{DateContext, DateMode};
use rgit_hash::ObjectId;
use rgit_object::{Commit, ObjectKind, SignatureRef, Tag};
use rgit_refs::files::FilesStore;
use rgit_refs::{RefStore, Target};
use rgit_remote::{BranchConfig, PushDefault, Remotes};
use rgit_repo::Setup;
use rgit_revwalk::walk::{CommitInfo, CommitSource, Walk, WalkOptions};

use super::format::{
    Abbrev, Atom, Contents, IfCond, Kind, NameMod, PersonPart, Piece, Position, RemoteMod,
    TrailerOpts, Who,
};
use crate::Die;
use crate::history::mailmap::Mailmap;
use crate::messages::ref_filter as msg;
use crate::refs_support::{Objects, Revs};

/// Quoting of atom values (`--shell`, `--perl`, `--python`, `--tcl`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Quote {
    None,
    Shell,
    Perl,
    Python,
    Tcl,
}

/// A listed reference.
#[derive(Clone, Debug)]
pub(crate) struct RefItem {
    pub(crate) name: String,
    pub(crate) oid: ObjectId,
    /// The target of a symbolic reference.
    pub(crate) symref: Option<String>,
}

/// The value of an atom: bytes, and a number for atoms that sort
/// numerically.
#[derive(Clone, Debug, Default)]
pub(crate) struct Value {
    pub(crate) bytes: Vec<u8>,
    pub(crate) num: Option<u64>,
}

impl Value {
    fn text(b: impl Into<Vec<u8>>) -> Value {
        Value {
            bytes: b.into(),
            num: None,
        }
    }

    fn number(n: u64) -> Value {
        Value {
            bytes: n.to_string().into_bytes(),
            num: Some(n),
        }
    }

    fn empty() -> Value {
        Value::default()
    }

    /// Compares for sorting: numbers as numbers, everything else by bytes.
    pub(crate) fn compare(&self, other: &Value, icase: bool) -> Ordering {
        match (self.num, other.num) {
            (Some(a), Some(b)) => a.cmp(&b),
            _ if icase => self
                .bytes
                .to_ascii_lowercase()
                .cmp(&other.bytes.to_ascii_lowercase()),
            _ => self.bytes.cmp(&other.bytes),
        }
    }
}

/// Repository parts that `upstream`, `push`, `ahead-behind`,
/// `worktreepath` and `:mailmap` need.
#[derive(Clone, Copy)]
pub(crate) struct RepoParts<'a> {
    pub(crate) setup: &'a Setup,
    pub(crate) config: &'a ConfigSet,
    pub(crate) revs: &'a Revs,
}

/// An object as read from the database.
struct Loaded {
    id: ObjectId,
    kind: ObjectKind,
    data: Vec<u8>,
}

/// What one reference's atoms read, loaded at most once.
#[derive(Default)]
pub(crate) struct ItemCache {
    object: OnceCell<Option<std::rc::Rc<Loaded>>>,
    peeled: OnceCell<Option<std::rc::Rc<Loaded>>>,
}

/// Evaluates atoms over the references of one repository.
pub(crate) struct Env<'a> {
    pub(crate) objects: &'a Objects,
    pub(crate) store: &'a FilesStore,
    pub(crate) repo: Option<RepoParts<'a>>,
    pub(crate) color: bool,
    pub(crate) quote: Quote,
    head: OnceCell<Option<Target>>,
    date_ctx: DateContext,
    abbrev: OnceCell<usize>,
    mailmap: OnceCell<Mailmap>,
    worktrees: OnceCell<HashMap<String, String>>,
    remotes: OnceCell<Option<Remotes>>,
    bases: HashMap<String, ObjectId>,
    /// References whose non-commit object was reported already.
    reported: std::cell::RefCell<std::collections::HashSet<String>>,
}

impl<'a> Env<'a> {
    pub(crate) fn new(objects: &'a Objects, store: &'a FilesStore) -> Env<'a> {
        Env {
            objects,
            store,
            repo: None,
            color: false,
            quote: Quote::None,
            head: OnceCell::new(),
            date_ctx: DateContext::from_env(),
            abbrev: OnceCell::new(),
            mailmap: OnceCell::new(),
            worktrees: OnceCell::new(),
            remotes: OnceCell::new(),
            bases: HashMap::new(),
            reported: Default::default(),
        }
    }

    /// Resolves what the atoms refer to (the `<committish>` of
    /// `%(ahead-behind:...)`), and rejects atoms rgit does not support.
    pub(crate) fn prepare<'p>(
        &mut self,
        atoms: impl IntoIterator<Item = &'p Atom>,
    ) -> Result<(), Die> {
        for atom in atoms {
            match &atom.kind {
                Kind::AheadBehind(name) if !self.bases.contains_key(name) => {
                    let id = self
                        .repo
                        .and_then(|r| r.revs.resolve(name))
                        .and_then(|id| self.objects.peel(&id))
                        .ok_or_else(|| Die::Fatal(msg::failed_to_find(name)))?;
                    self.bases.insert(name.clone(), id);
                }
                Kind::Unsupported(name) => {
                    return Err(Die::Fatal(msg::atom_not_supported(name)));
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// `HEAD` of the current work tree.
    fn head(&self) -> Option<&Target> {
        self.head
            .get_or_init(|| match self.repo {
                Some(r) => {
                    let text = std::fs::read_to_string(r.setup.git_dir.join("HEAD")).ok()?;
                    let text = text.trim_end();
                    match text.strip_prefix("ref: ") {
                        Some(name) => Some(Target::Symbolic(name.to_owned())),
                        None => ObjectId::from_hex(self.objects.hash(), text.as_bytes())
                            .ok()
                            .map(Target::Object),
                    }
                }
                None => self.store.read_raw("HEAD").ok().flatten(),
            })
            .as_ref()
    }

    fn missing(&self, it: &RefItem) -> Die {
        Die::Fatal(msg::missing_object(&it.oid.to_hex(), &it.name))
    }

    fn load(&self, id: &ObjectId) -> Option<std::rc::Rc<Loaded>> {
        let (kind, data) = self.objects.read(id)?;
        Some(std::rc::Rc::new(Loaded {
            id: *id,
            kind,
            data,
        }))
    }

    /// The object of `it` (`deref`: what its annotated tag points to, or
    /// `None` when it is not a tag).
    fn object(
        &self,
        it: &RefItem,
        cache: &ItemCache,
        deref: bool,
    ) -> Result<Option<std::rc::Rc<Loaded>>, Die> {
        let main = cache
            .object
            .get_or_init(|| self.load(&it.oid))
            .clone()
            .ok_or_else(|| self.missing(it))?;
        if !deref {
            return Ok(Some(main));
        }
        if main.kind != ObjectKind::Tag {
            return Ok(None);
        }
        if let Some(p) = cache.peeled.get() {
            return Ok(p.clone());
        }
        let mut cur = main.clone();
        for _ in 0..64 {
            if cur.kind != ObjectKind::Tag {
                break;
            }
            let tag = Tag::parse(self.objects.hash(), &cur.data).map_err(|_| self.missing(it))?;
            let (Ok(next), Ok(claimed)) = (tag.object(), tag.target_kind()) else {
                return Err(self.missing(it));
            };
            let loaded = self.load(&next).ok_or_else(|| self.missing(it))?;
            if loaded.kind != claimed {
                eprintln!(
                    "error: {}",
                    msg::not_a_commit(&next.to_hex(), claimed.name())
                        .replace(", not a commit", &format!(", not a {}", loaded.kind.name()))
                );
                return Err(Die::Fatal(msg::BAD_TAG.to_owned()));
            }
            cur = loaded;
        }
        let _ = cache.peeled.set(Some(cur.clone()));
        Ok(Some(cur))
    }

    /// The default abbreviation length (`core.abbrev`, else from the
    /// number of objects).
    fn default_abbrev(&self) -> usize {
        *self.abbrev.get_or_init(|| {
            let hex_len = self.objects.hash().hex_len();
            if let Some(cfg) = self.repo.map(|r| r.config) {
                if let Ok(Some(v)) = cfg.string("core.abbrev") {
                    let v = v.to_string();
                    if v.eq_ignore_ascii_case("no") {
                        return hex_len;
                    }
                    if let Ok(n) = v.parse::<usize>() {
                        return n.clamp(4, hex_len);
                    }
                }
            }
            let count = self.objects.approximate_count().max(1) as u64;
            let bits = 64 - count.leading_zeros() as usize;
            bits.div_ceil(2).max(7).min(hex_len)
        })
    }

    fn abbrev(&self, id: &ObjectId, a: Abbrev) -> Vec<u8> {
        match a {
            Abbrev::Full => id.to_hex().into_bytes(),
            Abbrev::Default => self.objects.abbrev(id, self.default_abbrev()).into_bytes(),
            Abbrev::Len(n) => self.objects.abbrev(id, n).into_bytes(),
        }
    }

    /// The value of `atom` for `it`.
    pub(crate) fn value(&self, atom: &Atom, it: &RefItem, cache: &ItemCache) -> Result<Value, Die> {
        let deref = atom.deref;
        let name_value = |name: &str, m: NameMod| {
            let mut v = self.shorten(name, m).into_bytes();
            if deref {
                v.extend_from_slice(b"^{}");
            }
            Value::text(v)
        };
        match &atom.kind {
            Kind::Refname(m) => return Ok(name_value(&it.name, *m)),
            Kind::Symref(m) => {
                return Ok(match &it.symref {
                    Some(t) => name_value(t, *m),
                    None if deref => Value::text(b"^{}".to_vec()),
                    None => Value::empty(),
                });
            }
            Kind::Upstream(m) => return Ok(self.upstream(it, *m, false)),
            Kind::Push(m) => return Ok(self.upstream(it, *m, true)),
            Kind::Head => {
                let is_head = matches!(self.head(), Some(Target::Symbolic(t)) if *t == it.name);
                return Ok(Value::text(if is_head { "*" } else { " " }));
            }
            Kind::Color(seq) => {
                return Ok(if self.color {
                    Value::text(seq.as_bytes().to_vec())
                } else {
                    Value::empty()
                });
            }
            Kind::AheadBehind(name) => {
                let Some(base) = self.bases.get(name) else {
                    return Ok(Value::empty());
                };
                let Some(tip) = self.objects.peel(&it.oid) else {
                    return Ok(Value::empty());
                };
                match self.objects.kind(&tip) {
                    Some(ObjectKind::Commit) => {}
                    Some(k) => {
                        if self.reported.borrow_mut().insert(it.name.clone()) {
                            eprintln!("error: {}", msg::not_a_commit(&it.oid.to_hex(), k.name()));
                        }
                        return Ok(Value::empty());
                    }
                    None => return Ok(Value::empty()),
                }
                let ahead = count_only(self.objects, &tip, base);
                let behind = count_only(self.objects, base, &tip);
                return Ok(Value::text(format!("{ahead} {behind}")));
            }
            Kind::Worktreepath => {
                return Ok(Value::text(
                    self.worktrees()
                        .get(&it.name)
                        .cloned()
                        .unwrap_or_default()
                        .into_bytes(),
                ));
            }
            Kind::Align { .. }
            | Kind::If(_)
            | Kind::Then
            | Kind::Else
            | Kind::End
            | Kind::Rest
            | Kind::Unsupported(_) => return Ok(Value::empty()),
            Kind::Objectname(a) if !deref => return Ok(Value::text(self.abbrev(&it.oid, *a))),
            _ => {}
        }
        let numeric = matches!(
            &atom.kind,
            Kind::Objectsize { .. }
                | Kind::Raw { size: true }
                | Kind::Contents(Contents::Size)
                | Kind::Person(_, PersonPart::Date(None))
        );
        let none = || {
            Ok(if numeric {
                Value {
                    bytes: Vec::new(),
                    num: Some(0),
                }
            } else {
                Value::empty()
            })
        };
        let Some(obj) = self.object(it, cache, deref)? else {
            return none();
        };
        let hash = self.objects.hash();
        let v = match &atom.kind {
            Kind::Objectname(a) => Value::text(self.abbrev(&obj.id, *a)),
            Kind::Objecttype => Value::text(obj.kind.name()),
            Kind::Objectsize { disk: false } => Value::number(obj.data.len() as u64),
            Kind::Objectsize { disk: true } => Value::number(self.objects.disk_size(&obj.id)),
            Kind::Deltabase => Value::text(self.objects.delta_base(&obj.id).to_hex()),
            Kind::Raw { size: false } => Value::text(obj.data.clone()),
            Kind::Raw { size: true } => Value::number(obj.data.len() as u64),
            Kind::Tree(_) | Kind::Parent(_) | Kind::Numparent => {
                if obj.kind != ObjectKind::Commit {
                    return none();
                }
                let c = Commit::parse(hash, &obj.data).map_err(|_| self.missing(it))?;
                match &atom.kind {
                    Kind::Tree(a) => match c.tree() {
                        Ok(t) => Value::text(self.abbrev(&t, *a)),
                        Err(_) => Value::empty(),
                    },
                    Kind::Parent(a) => {
                        let parents = c.parents().unwrap_or_default();
                        let mut out = Vec::new();
                        for (i, p) in parents.iter().enumerate() {
                            if i > 0 {
                                out.push(b' ');
                            }
                            out.extend_from_slice(&self.abbrev(p, *a));
                        }
                        Value::text(out)
                    }
                    _ => Value::number(c.parents().map_or(0, |p| p.len() as u64)),
                }
            }
            Kind::Object | Kind::Type | Kind::Tag => {
                if obj.kind != ObjectKind::Tag {
                    return none();
                }
                let t = Tag::parse(hash, &obj.data).map_err(|_| self.missing(it))?;
                let header = match &atom.kind {
                    Kind::Object => "object",
                    Kind::Type => "type",
                    _ => "tag",
                };
                Value::text(
                    t.headers()
                        .iter()
                        .find(|h| h.name == header.as_bytes())
                        .map(|h| h.value.clone())
                        .unwrap_or_default(),
                )
            }
            Kind::Person(who, part) => self.person(&obj, *who, part)?.unwrap_or_else(|| {
                if numeric {
                    Value {
                        bytes: Vec::new(),
                        num: Some(0),
                    }
                } else {
                    Value::empty()
                }
            }),
            Kind::Subject { sanitize } => {
                let Some(m) = message(&obj, hash) else {
                    return none();
                };
                let parts = MessageParts::new(&m);
                let s = parts.subject();
                Value::text(if *sanitize { sanitize_subject(&s) } else { s })
            }
            Kind::Body => {
                let Some(m) = message(&obj, hash) else {
                    return none();
                };
                Value::text(MessageParts::new(&m).body_with_signature().to_vec())
            }
            Kind::Contents(c) => {
                let Some(m) = message(&obj, hash) else {
                    return none();
                };
                let parts = MessageParts::new(&m);
                match c {
                    Contents::All => Value::text(parts.contents().to_vec()),
                    Contents::Subject => Value::text(parts.subject()),
                    Contents::Body => Value::text(parts.body().to_vec()),
                    Contents::Signature => Value::text(parts.signature().to_vec()),
                    Contents::Size => Value::number(parts.contents().len() as u64),
                    Contents::Lines(n) => Value::text(parts.lines(*n)),
                    Contents::Trailers(opts) => Value::text(self.trailers(parts.text(), opts)),
                }
            }
            _ => Value::empty(),
        };
        Ok(v)
    }

    fn person(&self, obj: &Loaded, who: Who, part: &PersonPart) -> Result<Option<Value>, Die> {
        let hash = self.objects.hash();
        let (raw, sig): (Option<Vec<u8>>, Option<OwnedSig>) = match (obj.kind, who) {
            (ObjectKind::Commit, Who::Author | Who::Committer | Who::Creator) => {
                let Ok(c) = Commit::parse(hash, &obj.data) else {
                    return Ok(None);
                };
                let (raw, sig) = if who == Who::Author {
                    (c.author_raw(), c.author())
                } else {
                    (c.committer_raw(), c.committer())
                };
                (raw.map(<[u8]>::to_vec), sig.as_ref().map(OwnedSig::from))
            }
            (ObjectKind::Tag, Who::Tagger | Who::Creator) => {
                let Ok(t) = Tag::parse(hash, &obj.data) else {
                    return Ok(None);
                };
                (
                    t.tagger_raw().map(<[u8]>::to_vec),
                    t.tagger().as_ref().map(OwnedSig::from),
                )
            }
            _ => return Ok(None),
        };
        let Some(raw) = raw else {
            return Ok(None);
        };
        Ok(Some(match part {
            PersonPart::Whole => Value::text(raw),
            PersonPart::Nothing => Value::empty(),
            PersonPart::Name { mailmap } => {
                let Some(sig) = sig else {
                    return Ok(Some(Value::empty()));
                };
                let (name, _) = self.mapped(&sig, *mailmap);
                Value::text(name)
            }
            PersonPart::Email {
                mailmap,
                trim,
                localpart,
            } => {
                let Some(sig) = sig else {
                    return Ok(Some(Value::empty()));
                };
                let (_, email) = self.mapped(&sig, *mailmap);
                if *localpart {
                    let end = email.iter().position(|&b| b == b'@').unwrap_or(email.len());
                    Value::text(email[..end].to_vec())
                } else if *trim {
                    Value::text(email)
                } else {
                    let mut v = b"<".to_vec();
                    v.extend_from_slice(&email);
                    v.push(b'>');
                    Value::text(v)
                }
            }
            PersonPart::Date(mode) => {
                let Some((secs, tz)) = sig.and_then(|s| s.time) else {
                    return Ok(Some(Value {
                        bytes: Vec::new(),
                        num: mode.is_none().then_some(0),
                    }));
                };
                let default = DateMode::default();
                let text = rgit_date::show_date(
                    secs,
                    tz,
                    mode.as_ref().unwrap_or(&default),
                    &self.date_ctx,
                )
                .unwrap_or_default();
                Value {
                    bytes: text.into_bytes(),
                    num: mode.is_none().then_some(secs),
                }
            }
        }))
    }

    fn mapped(&self, sig: &OwnedSig, mailmap: bool) -> (Vec<u8>, Vec<u8>) {
        if !mailmap {
            return (sig.name.clone(), sig.email.clone());
        }
        let mm = self.mailmap.get_or_init(|| self.load_mailmap());
        let (n, e) = mm.map(&sig.name, &sig.email);
        (n.to_vec(), e.to_vec())
    }

    /// `.mailmap` in the work tree, `mailmap.blob` (default
    /// `HEAD:.mailmap` in a bare repository), `mailmap.file`.
    fn load_mailmap(&self) -> Mailmap {
        let mut mm = Mailmap::default();
        let Some(r) = self.repo else {
            return mm;
        };
        if let Some(top) = &r.setup.work_tree {
            let path = r.setup.cwd.join(top).join(".mailmap");
            if let Ok(meta) = path.symlink_metadata()
                && meta.is_file()
                && let Ok(text) = std::fs::read(&path)
            {
                mm.add(&text);
            }
        }
        let blob = match r.config.string("mailmap.blob") {
            Ok(Some(v)) => Some(v.to_string()),
            _ if r.setup.work_tree.is_none() => Some("HEAD:.mailmap".to_owned()),
            _ => None,
        };
        if let Some(spec) = blob
            && let Some(id) = r.revs.resolve(&spec)
            && let Some((ObjectKind::Blob, data)) = self.objects.read(&id)
        {
            mm.add(&data);
        }
        if let Ok(Some(file)) = r.config.string("mailmap.file") {
            let file = file.to_string();
            let path = match file.strip_prefix("~/") {
                Some(rest) => std::env::var_os("HOME")
                    .map(|h| std::path::PathBuf::from(h).join(rest))
                    .unwrap_or_else(|| file.clone().into()),
                None => file.into(),
            };
            if let Ok(text) = std::fs::read(path) {
                mm.add(&text);
            }
        }
        mm
    }

    /// `%(trailers)`: the trailer block of `text`.
    fn trailers(&self, text: &[u8], opts: &TrailerOpts) -> Vec<u8> {
        let conf = self
            .repo
            .and_then(|r| rgit_trailer::Config::from_config(r.config).ok())
            .map(|(c, _)| c)
            .unwrap_or_default();
        let parse_opts = rgit_trailer::ParseOptions {
            comment: b"#",
            divider: false,
        };
        let info = rgit_trailer::parse(text, &conf, &parse_opts);
        if !info.found {
            return Vec::new();
        }
        if !opts.formatted {
            // The block as written, without the empty lines after it.
            let block = &text[info.start..info.end.min(text.len())];
            let Some(last) = block.iter().rposition(|b| !b.is_ascii_whitespace()) else {
                return Vec::new();
            };
            let end = block[last..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(block.len(), |nl| last + nl + 1);
            return block[..end].to_vec();
        }
        let only = opts.only.unwrap_or(!opts.keys.is_empty());
        let mut pieces: Vec<Vec<u8>> = Vec::new();
        for line in &info.lines {
            match line {
                rgit_trailer::Line::Trailer(t) => {
                    if !opts.keys.is_empty()
                        && !opts.keys.iter().any(|k| k.eq_ignore_ascii_case(&t.token))
                    {
                        continue;
                    }
                    let value = if opts.unfold {
                        rgit_trailer::unfold_value(&t.value)
                    } else {
                        t.value.to_vec()
                    };
                    let mut p = Vec::new();
                    if !opts.valueonly {
                        p.extend_from_slice(&t.token);
                    }
                    if !opts.keyonly && !opts.valueonly {
                        p.extend_from_slice(opts.kv_separator.as_deref().unwrap_or(b": "));
                    }
                    if !opts.keyonly {
                        p.extend_from_slice(&value);
                    }
                    pieces.push(p);
                }
                rgit_trailer::Line::Other(raw) => {
                    if !only {
                        let raw: &[u8] = raw;
                        pieces.push(if opts.separator.is_some() {
                            raw.trim_end().to_vec()
                        } else {
                            raw.to_vec()
                        });
                    }
                }
            }
        }
        match &opts.separator {
            Some(sep) => pieces.join(sep.as_slice()),
            None => {
                let mut out = Vec::new();
                for p in pieces {
                    out.extend_from_slice(&p);
                    out.push(b'\n');
                }
                out
            }
        }
    }

    fn remotes(&self) -> Option<&Remotes> {
        self.remotes
            .get_or_init(|| self.repo.and_then(|r| Remotes::from_config(r.config).ok()))
            .as_ref()
    }

    /// `%(upstream...)` and `%(push...)`.
    fn upstream(&self, it: &RefItem, m: RemoteMod, push: bool) -> Value {
        let (Some(r), Some(short)) = (self.repo, it.name.strip_prefix("refs/heads/")) else {
            return Value::empty();
        };
        let Ok(b) = BranchConfig::from_config(r.config, short.as_bytes()) else {
            return Value::empty();
        };
        let Some(remotes) = self.remotes() else {
            return Value::empty();
        };
        let git_dir = Some(r.setup.git_dir.as_path());
        let push_default = PushDefault::from_config(r.config).unwrap_or_default();
        let target = if push {
            rgit_remote::push_ref(remotes, git_dir, Some(&b), push_default).ok()
        } else {
            rgit_remote::upstream_ref(remotes, git_dir, Some(&b)).ok()
        };
        let target = target.map(|t| t.to_str_lossy().into_owned());
        match m {
            RemoteMod::Name(nm) => match target {
                Some(t) => Value::text(self.shorten(&t, nm)),
                None => Value::empty(),
            },
            RemoteMod::RemoteName => {
                if push {
                    Value::text(Self::push_remote_name(remotes, &b).unwrap_or_default())
                } else if b.merge.is_empty() {
                    Value::empty()
                } else {
                    Value::text(b.remote.clone().map(|r| r.to_vec()).unwrap_or_default())
                }
            }
            RemoteMod::RemoteRef => {
                if push {
                    Value::text(self.push_remote_ref(remotes, &b))
                } else if b.remote.is_some() {
                    Value::text(b.merge.first().map(|m| m.to_vec()).unwrap_or_default())
                } else {
                    Value::empty()
                }
            }
            RemoteMod::Track { brackets } => {
                let Some(t) = target else {
                    return Value::empty();
                };
                let text = match self.track_counts(it, &t) {
                    None => msg::GONE.to_owned(),
                    Some((0, 0)) => return Value::empty(),
                    Some((a, 0)) => msg::ahead(a),
                    Some((0, b)) => msg::behind(b),
                    Some((a, b)) => msg::ahead_behind(a, b),
                };
                Value::text(if brackets { format!("[{text}]") } else { text })
            }
            RemoteMod::TrackShort => {
                let Some(t) = target else {
                    return Value::empty();
                };
                Value::text(match self.track_counts(it, &t) {
                    None => "",
                    Some((0, 0)) => "=",
                    Some((_, 0)) => ">",
                    Some((0, _)) => "<",
                    Some(_) => "<>",
                })
            }
        }
    }

    /// The explicitly configured push remote of a branch:
    /// `branch.<name>.pushRemote`, `remote.pushDefault`,
    /// `branch.<name>.remote` (no `origin` fallback).
    fn push_remote_name(remotes: &Remotes, b: &BranchConfig) -> Option<Vec<u8>> {
        b.push_remote
            .as_ref()
            .map(|r| r.to_vec())
            .or_else(|| remotes.push_default().map(|r| r.to_vec()))
            .or_else(|| b.remote.as_ref().map(|r| r.to_vec()))
    }

    /// The destination ref on the push remote (`%(push:remoteref)`): only
    /// known from `remote.<name>.push` refspecs.
    fn push_remote_ref(&self, remotes: &Remotes, b: &BranchConfig) -> Vec<u8> {
        let (Some(r), Some(name)) = (self.repo, Self::push_remote_name(remotes, b)) else {
            return Vec::new();
        };
        let Ok(remote) = remotes.get(&name, Some(r.setup.git_dir.as_path())) else {
            return Vec::new();
        };
        if remote.push.is_empty() {
            return Vec::new();
        }
        remote
            .push
            .map_src(&b.full_name())
            .map(|d| d.to_vec())
            .unwrap_or_default()
    }

    /// Commits `it` is ahead of and behind `upstream`; `None` when the
    /// upstream ref does not exist.
    fn track_counts(&self, it: &RefItem, upstream: &str) -> Option<(usize, usize)> {
        let up = match self.store.read_raw(upstream) {
            Ok(Some(Target::Object(o))) => o,
            Ok(Some(Target::Symbolic(_))) => self.store.resolve_oid(upstream).ok().flatten()?,
            _ => return None,
        };
        let ahead = count_only(self.objects, &it.oid, &up);
        let behind = count_only(self.objects, &up, &it.oid);
        Some((ahead, behind))
    }

    /// Branch refs checked out in a work tree, with the work tree's path.
    fn worktrees(&self) -> &HashMap<String, String> {
        self.worktrees.get_or_init(|| {
            let mut map = HashMap::new();
            let Some(r) = self.repo else {
                return map;
            };
            let common = &r.setup.common_dir;
            let abs = |p: &std::path::Path| {
                std::path::absolute(r.setup.cwd.join(p))
                    .unwrap_or_else(|_| p.to_path_buf())
                    .to_string_lossy()
                    .into_owned()
            };
            let head_of = |dir: &std::path::Path| {
                std::fs::read_to_string(dir.join("HEAD"))
                    .ok()
                    .and_then(|t| t.trim_end().strip_prefix("ref: ").map(str::to_owned))
            };
            let bare = r.config.bool("core.bare").ok().flatten().unwrap_or(false);
            if !bare {
                let main = if r.setup.git_dir == *common {
                    r.setup.work_tree.clone()
                } else {
                    None
                }
                .map(|w| r.setup.cwd.join(w))
                .or_else(|| common.parent().map(|p| p.to_path_buf()));
                if let (Some(main), Some(head)) = (main, head_of(common)) {
                    map.insert(head, abs(&main));
                }
            }
            if let Ok(entries) = std::fs::read_dir(common.join("worktrees")) {
                for e in entries.flatten() {
                    let dir = e.path();
                    let Some(head) = head_of(&dir) else { continue };
                    let Ok(gitdir) = std::fs::read_to_string(dir.join("gitdir")) else {
                        continue;
                    };
                    let path = std::path::PathBuf::from(gitdir.trim_end());
                    if let Some(wt) = path.parent() {
                        map.entry(head).or_insert_with(|| abs(wt));
                    }
                }
            }
            map
        })
    }

    fn shorten(&self, name: &str, m: NameMod) -> String {
        match m {
            NameMod::Full => name.to_owned(),
            NameMod::Short => self.shorten_unambiguous(name),
            NameMod::Lstrip(n) => lstrip(name, n),
            NameMod::Rstrip(n) => rstrip(name, n),
        }
    }

    /// The shortest name that still resolves to `name` with git's lookup
    /// rules (`refname:short`); with `core.warnAmbiguousRefs` off, only
    /// the rule the name came from must not match another ref first.
    fn shorten_unambiguous(&self, name: &str) -> String {
        const RULES: [(&str, &str); 6] = [
            ("", ""),
            ("refs/", ""),
            ("refs/tags/", ""),
            ("refs/heads/", ""),
            ("refs/remotes/", ""),
            ("refs/remotes/", "/HEAD"),
        ];
        let strict = self
            .repo
            .and_then(|r| r.config.bool("core.warnambiguousrefs").ok().flatten())
            .unwrap_or(true);
        for i in (1..RULES.len()).rev() {
            let (pre, suf) = RULES[i];
            let Some(short) = name
                .strip_prefix(pre)
                .and_then(|s| s.strip_suffix(suf))
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let rules_to_check = if strict { RULES.len() } else { i };
            let ambiguous = RULES[..rules_to_check]
                .iter()
                .enumerate()
                .any(|(j, (p, s))| {
                    if j == i {
                        return false;
                    }
                    let candidate = format!("{p}{short}{s}");
                    candidate != name && matches!(self.store.read_raw(&candidate), Ok(Some(_)))
                });
            if !ambiguous {
                return short.to_owned();
            }
        }
        name.to_owned()
    }

    /// Expands `pieces` for one reference.
    pub(crate) fn render(
        &self,
        pieces: &[Piece],
        it: &RefItem,
        cache: &ItemCache,
        reset_at_eol: bool,
    ) -> Result<Vec<u8>, Die> {
        let mut stack: Vec<Frame> = vec![Frame {
            kind: FrameKind::Base,
            buf: Vec::new(),
        }];
        for p in pieces {
            match p {
                Piece::Lit(b) => stack.last_mut().expect("base").buf.extend_from_slice(b),
                Piece::Atom(a) => match &a.kind {
                    Kind::Align { width, position } => stack.push(Frame {
                        kind: FrameKind::Align {
                            width: *width,
                            position: *position,
                        },
                        buf: Vec::new(),
                    }),
                    Kind::If(cond) => stack.push(Frame {
                        kind: FrameKind::If {
                            cond: cond.clone(),
                            state: IfState::Condition,
                            result: false,
                            then_part: Vec::new(),
                        },
                        buf: Vec::new(),
                    }),
                    Kind::Then => {
                        let top = stack.last_mut().expect("base");
                        let FrameKind::If {
                            cond,
                            state,
                            result,
                            ..
                        } = &mut top.kind
                        else {
                            return Err(fatal(msg::THEN_WITHOUT_IF));
                        };
                        if *state != IfState::Condition {
                            return Err(fatal(msg::THEN_TWICE));
                        }
                        *result = match cond {
                            IfCond::NotEmpty => top.buf.iter().any(|b| !b.is_ascii_whitespace()),
                            IfCond::Equals(s) => top.buf == *s,
                            IfCond::NotEquals(s) => top.buf != *s,
                        };
                        *state = IfState::Then;
                        top.buf.clear();
                    }
                    Kind::Else => {
                        let top = stack.last_mut().expect("base");
                        let FrameKind::If {
                            state, then_part, ..
                        } = &mut top.kind
                        else {
                            return Err(fatal(msg::ELSE_WITHOUT_IF));
                        };
                        match state {
                            IfState::Condition => return Err(fatal(msg::ELSE_WITHOUT_THEN)),
                            IfState::Else => return Err(fatal(msg::ELSE_TWICE)),
                            IfState::Then => {}
                        }
                        *state = IfState::Else;
                        *then_part = std::mem::take(&mut top.buf);
                    }
                    Kind::End => {
                        if stack.len() == 1 {
                            return Err(fatal(msg::END_WITHOUT));
                        }
                        let frame = stack.pop().expect("frame");
                        let out = match frame.kind {
                            FrameKind::Base => unreachable!("base frame is never popped"),
                            FrameKind::Align { width, position } => {
                                align(frame.buf, width, position)
                            }
                            FrameKind::If {
                                state,
                                result,
                                then_part,
                                ..
                            } => match state {
                                IfState::Condition => return Err(fatal(msg::IF_WITHOUT_THEN)),
                                IfState::Then if result => frame.buf,
                                IfState::Then => Vec::new(),
                                IfState::Else if result => then_part,
                                IfState::Else => frame.buf,
                            },
                        };
                        self.append(&mut stack, &out);
                    }
                    _ => {
                        let v = self.value(a, it, cache)?;
                        self.append(&mut stack, &v.bytes);
                    }
                },
            }
        }
        if stack.len() > 1 {
            return Err(fatal(msg::END_MISSING));
        }
        if reset_at_eol && self.color {
            self.append(&mut stack, crate::color::RESET.as_bytes());
        }
        Ok(stack.pop().expect("base").buf)
    }

    /// Appends an expanded value: quoted when it lands at the top level.
    fn append(&self, stack: &mut [Frame], v: &[u8]) {
        let top_level = stack.len() == 1;
        let buf = &mut stack.last_mut().expect("base").buf;
        if top_level {
            quote_into(buf, v, self.quote);
        } else {
            buf.extend_from_slice(v);
        }
    }
}

fn fatal(m: &str) -> Die {
    Die::Fatal(m.to_owned())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IfState {
    Condition,
    Then,
    Else,
}

enum FrameKind {
    Base,
    Align {
        width: usize,
        position: Position,
    },
    If {
        cond: IfCond,
        state: IfState,
        result: bool,
        then_part: Vec<u8>,
    },
}

struct Frame {
    kind: FrameKind,
    buf: Vec<u8>,
}

/// Display width: characters, without colour escape sequences.
fn display_width(b: &[u8]) -> usize {
    let s = String::from_utf8_lossy(b);
    let mut n = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        n += 1;
    }
    n
}

fn align(content: Vec<u8>, width: usize, position: Position) -> Vec<u8> {
    let w = display_width(&content);
    if w >= width {
        return content;
    }
    let pad = width - w;
    let (left, right) = match position {
        Position::Left => (0, pad),
        Position::Right => (pad, 0),
        Position::Middle => (pad / 2, pad - pad / 2),
    };
    let mut out = vec![b' '; left];
    out.extend_from_slice(&content);
    out.resize(out.len() + right, b' ');
    out
}

/// Appends `v` quoted for `q`.
pub(crate) fn quote_into(out: &mut Vec<u8>, v: &[u8], q: Quote) {
    match q {
        Quote::None => out.extend_from_slice(v),
        Quote::Shell => {
            out.push(b'\'');
            for &c in v {
                if c == b'\'' || c == b'!' {
                    out.extend_from_slice(b"'\\");
                    out.push(c);
                    out.push(b'\'');
                } else {
                    out.push(c);
                }
            }
            out.push(b'\'');
        }
        Quote::Perl => {
            out.push(b'\'');
            for &c in v {
                if c == b'\'' || c == b'\\' {
                    out.push(b'\\');
                }
                out.push(c);
            }
            out.push(b'\'');
        }
        Quote::Python => {
            out.push(b'\'');
            for &c in v {
                match c {
                    b'\'' | b'\\' => {
                        out.push(b'\\');
                        out.push(c);
                    }
                    b'\n' => out.extend_from_slice(b"\\n"),
                    _ => out.push(c),
                }
            }
            out.push(b'\'');
        }
        Quote::Tcl => {
            out.push(b'"');
            for &c in v {
                match c {
                    b'[' | b']' | b'{' | b'}' | b'$' | b'\\' | b'"' => {
                        out.push(b'\\');
                        out.push(c);
                    }
                    0x0c => out.extend_from_slice(b"\\f"),
                    b'\r' => out.extend_from_slice(b"\\r"),
                    b'\n' => out.extend_from_slice(b"\\n"),
                    b'\t' => out.extend_from_slice(b"\\t"),
                    0x0b => out.extend_from_slice(b"\\v"),
                    _ => out.push(c),
                }
            }
            out.push(b'"');
        }
    }
}

/// An identity with an owned name and email.
struct OwnedSig {
    name: Vec<u8>,
    email: Vec<u8>,
    /// Seconds and `±hhmm` offset.
    time: Option<(u64, i32)>,
}

impl From<&SignatureRef<'_>> for OwnedSig {
    fn from(s: &SignatureRef<'_>) -> OwnedSig {
        OwnedSig {
            name: s.name.to_vec(),
            email: s.email.to_vec(),
            time: s.time().map(|t| {
                let abs = t.offset_minutes.abs();
                let hhmm = abs / 60 * 100 + abs % 60;
                (
                    u64::try_from(t.seconds).unwrap_or(0),
                    if t.offset_minutes < 0 { -hhmm } else { hhmm },
                )
            }),
        }
    }
}

/// The message of a commit or tag.
fn message(obj: &Loaded, hash: rgit_hash::HashKind) -> Option<Vec<u8>> {
    match obj.kind {
        ObjectKind::Commit => Commit::parse(hash, &obj.data)
            .ok()
            .map(|c| c.message().to_vec()),
        ObjectKind::Tag => Tag::parse(hash, &obj.data)
            .ok()
            .map(|t| t.message().to_vec()),
        _ => None,
    }
}

/// A message cut into subject, body and signature.
struct MessageParts<'m> {
    /// From the subject (leading empty lines skipped) to the end.
    all: &'m [u8],
    /// `all` without the signature.
    text: &'m [u8],
    /// Length of the subject paragraph in `text`.
    subject_len: usize,
    /// Start of the body in `all`.
    body_start: usize,
}

/// The length of an empty line (`\n` or `\r\n`) at the start of `b`.
fn empty_line(b: &[u8]) -> Option<usize> {
    if b.starts_with(b"\n") {
        Some(1)
    } else if b.starts_with(b"\r\n") {
        Some(2)
    } else {
        None
    }
}

impl<'m> MessageParts<'m> {
    fn new(msg: &'m [u8]) -> MessageParts<'m> {
        let mut s = 0;
        while let Some(n) = empty_line(&msg[s..]) {
            s += n;
        }
        let all = &msg[s..];
        let (text, _) = rgit_object::split_signature(all);
        let mut subject_len = text.len();
        let mut i = 0;
        while let Some(nl) = text[i..].iter().position(|&b| b == b'\n') {
            let at = i + nl;
            if empty_line(&text[at + 1..]).is_some() {
                subject_len = at;
                break;
            }
            i = at + 1;
        }
        let mut body_start = subject_len;
        while let Some(n) = empty_line(&all[body_start..]) {
            body_start += n;
        }
        MessageParts {
            all,
            text,
            subject_len,
            body_start,
        }
    }

    fn contents(&self) -> &'m [u8] {
        self.all
    }

    fn text(&self) -> &'m [u8] {
        self.text
    }

    fn subject(&self) -> Vec<u8> {
        let region = &self.text[..self.subject_len];
        let mut lines: Vec<&[u8]> = region
            .split(|&b| b == b'\n')
            .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
            .collect();
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines.join(&b' ')
    }

    fn body_with_signature(&self) -> &'m [u8] {
        &self.all[self.body_start.min(self.all.len())..]
    }

    fn body(&self) -> &'m [u8] {
        if self.body_start >= self.text.len() {
            return b"";
        }
        &self.text[self.body_start..]
    }

    fn signature(&self) -> &'m [u8] {
        &self.all[self.text.len()..]
    }

    /// `contents:lines=<n>`: the first `n` lines, continued lines
    /// indented by four spaces.
    fn lines(&self, n: usize) -> Vec<u8> {
        let text = self.text.strip_suffix(b"\n").unwrap_or(self.text);
        if n == 0 || self.text.is_empty() {
            return Vec::new();
        }
        let lines: Vec<&[u8]> = text.split(|&b| b == b'\n').take(n).collect();
        lines.join(&b"\n    "[..])
    }
}

/// `subject:sanitize`: a file name (like `%f` of `git log`).
fn sanitize_subject(s: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(s.len());
    for &c in s {
        if c.is_ascii_alphanumeric() || c == b'_' {
            out.push(c);
        } else if c == b'.' {
            if out.last() != Some(&b'.') {
                out.push(c);
            }
        } else if !out.is_empty() && out.last() != Some(&b'-') {
            out.push(b'-');
        }
    }
    while out.last().is_some_and(|&c| c == b'.' || c == b'-') {
        out.pop();
    }
    out
}

/// `:lstrip=<n>`: drop `n` leading components (negative: keep `-n`
/// trailing ones).
pub(crate) fn lstrip(name: &str, n: i64) -> String {
    let parts: Vec<&str> = name.split('/').collect();
    let drop = if n >= 0 {
        n as usize
    } else {
        parts.len().saturating_sub(n.unsigned_abs() as usize)
    };
    if drop >= parts.len() {
        return String::new();
    }
    parts[drop..].join("/")
}

/// `:rstrip=<n>`: drop `n` trailing components (negative: keep `-n`
/// leading ones).
pub(crate) fn rstrip(name: &str, n: i64) -> String {
    let parts: Vec<&str> = name.split('/').collect();
    let keep = if n >= 0 {
        parts.len().saturating_sub(n as usize)
    } else {
        (n.unsigned_abs() as usize).min(parts.len())
    };
    parts[..keep].join("/")
}

/// A walk source over the object database.
struct Commits<'a>(&'a Objects);

impl CommitSource for Commits<'_> {
    type Error = ();
    fn load(&mut self, id: &ObjectId) -> Result<CommitInfo, ()> {
        let (kind, data) = self.0.read(id).ok_or(())?;
        if kind != ObjectKind::Commit {
            return Err(());
        }
        let commit = Commit::parse(self.0.hash(), &data).map_err(|_| ())?;
        Ok(CommitInfo {
            parents: commit.parents().map_err(|_| ())?,
            time: commit
                .committer()
                .and_then(|s| s.time())
                .map_or(0, |t| t.seconds),
        })
    }
}

/// Commits reachable from `a` but not from `b`.
pub(crate) fn count_only(objects: &Objects, a: &ObjectId, b: &ObjectId) -> usize {
    let mut walk = Walk::new(Commits(objects), WalkOptions::default());
    if walk.push(a, false).is_err() || walk.push(b, true).is_err() {
        return 0;
    }
    walk.collect_all().map(|v| v.len()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_parts() {
        let m = b"\n\nline one\nline two\n\n\n  body\n";
        let p = MessageParts::new(m);
        assert_eq!(p.subject(), b"line one line two");
        assert_eq!(p.body(), b"  body\n");
        assert_eq!(p.lines(2), b"line one\n    line two");
        assert_eq!(p.contents().len(), m.len() - 2);

        let m = b"subj\n-----BEGIN PGP SIGNATURE-----\nx\n-----END PGP SIGNATURE-----\n";
        let p = MessageParts::new(m);
        assert_eq!(p.subject(), b"subj");
        assert_eq!(p.body(), b"");
        assert!(p.body_with_signature().starts_with(b"-----BEGIN"));
        assert!(p.signature().starts_with(b"-----BEGIN"));

        let p = MessageParts::new(b"subj crlf\r\nmore\r\n\r\nbody\r\n");
        assert_eq!(p.subject(), b"subj crlf more");
        assert_eq!(p.body(), b"body\r\n");
    }

    #[test]
    fn sanitize() {
        for (i, o) in [
            ("a..b", "a.b"),
            (".hidden", ".hidden"),
            ("..x..", ".x"),
            ("x -- y", "x-y"),
            ("trail.", "trail"),
            ("--lead", "lead"),
            ("a. .b", "a.-.b"),
            ("Tagging at 1151968727", "Tagging-at-1151968727"),
        ] {
            assert_eq!(sanitize_subject(i.as_bytes()), o.as_bytes(), "{i}");
        }
    }

    #[test]
    fn strip_and_align() {
        assert_eq!(lstrip("refs/heads/main", -2), "heads/main");
        assert_eq!(lstrip("refs/heads/main", 5), "");
        assert_eq!(rstrip("refs/heads/main", -1), "refs");
        assert_eq!(align(b"ab".to_vec(), 5, Position::Middle), b" ab  ");
        assert_eq!(
            align(b"\x1b[31mab".to_vec(), 3, Position::Right),
            b" \x1b[31mab"
        );
    }

    #[test]
    fn quoting() {
        let mut o = Vec::new();
        quote_into(&mut o, b"it's", Quote::Shell);
        assert_eq!(o, b"'it'\\''s'");
    }
}
