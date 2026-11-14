// SPDX-License-Identifier: MIT
//! Content conversion in commands (rgit-convert): the attributes and
//! configuration of a repository, applied to work tree files on their way
//! into the repository and to blobs on their way out.
//!
//! Attributes come from the work tree with the index as fall-back when
//! comparing or adding files ([`Converter::worktree`]) or checking out
//! single paths ([`Converter::paths`]), and from the index first when a
//! checkout switches trees ([`Converter::checkout`]), as gitattributes(5)
//! describes. `text=auto`'s rule for files that already
//! have CR LF in the index looks at the stage-0 entry of the index file.
//!
//! Filter drivers (`filter=`, #112) are not run; [`Converter::filter`]
//! names them so that commands can warn.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use rgit_attr::{AttrId, Attributes, Order, Source};
use rgit_config::ConfigSet;
use rgit_convert::{AutoCrlf, Config, Conversion, Eol, Notice, Options, SafeCrlf, Stats};
use rgit_hash::ObjectId;
use rgit_index::{Index, ReadOptions, Stage};
use rgit_object::ObjectKind;
use rgit_odb::ObjectDatabase;
use rgit_repo::Setup;

use crate::Die;
use crate::repo_objects::Repo;

/// Reads `core.autocrlf`, `core.eol`, `core.safecrlf` and
/// `core.checkRoundtripEncoding`.
///
/// # Errors
/// Values git rejects (`core.autocrlf` and `core.safecrlf` must be
/// booleans or `input` / `warn`).
pub fn config(cfg: &ConfigSet) -> Result<Config, Die> {
    let mut out = Config::default();
    let bad = |e: rgit_config::ValueError| Die::Fatal(e.to_string());
    if let Some(e) = cfg.get("core.autocrlf") {
        out.autocrlf = if e.value.as_deref().map(|v| v.as_slice()) == Some(b"input".as_slice()) {
            AutoCrlf::Input
        } else if e.bool().map_err(bad)? {
            AutoCrlf::True
        } else {
            AutoCrlf::False
        };
    }
    if let Ok(Some(v)) = cfg.string("core.eol") {
        match v.as_ref() as &[u8] {
            b"native" => out.eol = Eol::native(),
            v => {
                if let Some(e) = Eol::parse(v) {
                    out.eol = e;
                }
            }
        }
    }
    if let Some(e) = cfg.get("core.safecrlf") {
        out.safecrlf = if e.value.as_deref().map(|v| v.as_slice()) == Some(b"warn".as_slice()) {
            SafeCrlf::Warn
        } else if e.bool().map_err(bad)? {
            SafeCrlf::Fail
        } else {
            SafeCrlf::False
        };
    }
    if let Ok(Some(v)) = cfg.string("core.checkroundtripencoding") {
        out.roundtrip = String::from_utf8_lossy(v.as_ref())
            .split(|c: char| c == ',' || c.is_ascii_whitespace())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
    }
    Ok(out)
}

/// Prints a conversion message with git's prefix.
pub fn print_notice(n: Notice) {
    match n {
        Notice::Warning(m) => eprintln!("warning: {m}"),
        Notice::Hint(m) => eprintln!("hint: {m}"),
        Notice::Error(m) => eprintln!("error: {m}"),
    }
}

/// Where the index (for `.gitattributes` fall-backs and the CR LF check)
/// comes from.
enum IndexSource {
    /// Read from this file on first use.
    File(PathBuf),
    /// Given.
    Given,
}

/// The attributes, created on first use.
struct Lookup {
    attrs: Attributes<'static>,
    ids: [AttrId; 6],
}

/// Applies the conversions of a repository.
pub struct Converter {
    cfg: Config,
    root: Option<PathBuf>,
    attr_opts: rgit_attr::Options,
    order: Order,
    setup: Setup,
    git_config: ConfigSet,
    hash: rgit_hash::HashKind,
    index_source: IndexSource,
    /// The index, once loaded (`None` inside: there is none).
    index: RefCell<Option<Option<Rc<Index>>>>,
    snapshot: RefCell<Option<HashMap<Vec<u8>, Vec<u8>>>>,
    odb: OnceCell<Option<ObjectDatabase>>,
    lookup: RefCell<Option<Lookup>>,
    skipped_filter: Cell<bool>,
    renormalize: Cell<bool>,
    /// Paths a warning was printed for (git warns once per path).
    warned: RefCell<std::collections::HashSet<Vec<u8>>>,
}

impl Converter {
    fn base(repo: &Repo, order: Order, index_source: IndexSource) -> Result<Converter, Die> {
        Ok(Converter {
            cfg: config(&repo.config)?,
            root: repo.setup.work_tree.clone(),
            attr_opts: crate::attributes::options(repo),
            order,
            setup: repo.setup.clone(),
            git_config: repo.config.clone(),
            hash: repo.hash,
            index_source,
            index: RefCell::new(None),
            snapshot: RefCell::new(None),
            odb: OnceCell::new(),
            lookup: RefCell::new(None),
            skipped_filter: Cell::new(false),
            renormalize: Cell::new(false),
            warned: RefCell::new(std::collections::HashSet::new()),
        })
    }

    /// For files in the work tree (adding, comparing): `.gitattributes`
    /// from the work tree, else from the index file.
    ///
    /// # Errors
    /// Invalid configuration.
    pub fn worktree(repo: &Repo) -> Result<Converter, Die> {
        Converter::base(
            repo,
            Order::WorkTreeFirst,
            IndexSource::File(
                repo.setup
                    .cwd
                    .join(rgit_index::index_path(&repo.setup.git_dir)),
            ),
        )
    }

    /// For checking out the entries of `index` while switching trees
    /// (`read-tree -u`, `switch`, `reset --hard`, `clone`):
    /// `.gitattributes` from `index` first.
    ///
    /// # Errors
    /// Invalid configuration.
    pub fn checkout(repo: &Repo, index: &Index) -> Result<Converter, Die> {
        Converter::with_index(repo, index, Order::IndexFirst)
    }

    /// For checking out single paths (`checkout-index`, `checkout --`,
    /// `restore`): `.gitattributes` from the work tree, `index` as the
    /// fall-back (checked with t0020 "in-tree .gitattributes" and t0028).
    ///
    /// # Errors
    /// Invalid configuration.
    pub fn paths(repo: &Repo, index: &Index) -> Result<Converter, Die> {
        Converter::with_index(repo, index, Order::WorkTreeFirst)
    }

    fn with_index(repo: &Repo, index: &Index, order: Order) -> Result<Converter, Die> {
        let c = Converter::base(repo, order, IndexSource::Given)?;
        *c.snapshot.borrow_mut() = Some(rgit_attr::snapshot(index, &repo.odb));
        *c.index.borrow_mut() = Some(None);
        Ok(c)
    }

    /// Takes `index` as the index from now on (after a command changed
    /// the index and the work tree): `.gitattributes` and the
    /// CR LF check are read afresh.
    pub fn use_index(&self, index: &Index) {
        *self.index.borrow_mut() = Some(Some(Rc::new(index.clone())));
        *self.snapshot.borrow_mut() = None;
        *self.lookup.borrow_mut() = None;
    }

    /// The configuration in use.
    pub fn config(&self) -> &Config {
        &self.cfg
    }

    fn odb(&self) -> Option<&ObjectDatabase> {
        self.odb
            .get_or_init(|| {
                let mut warnings = Vec::new();
                ObjectDatabase::open_for_repo(&self.setup, &self.git_config, &mut warnings).ok()
            })
            .as_ref()
    }

    fn index(&self) -> Option<Rc<Index>> {
        if let Some(i) = self.index.borrow().as_ref() {
            return i.clone();
        }
        let loaded = match &self.index_source {
            IndexSource::File(p) => Index::read_file(p, self.hash, &ReadOptions::default())
                .ok()
                .map(Rc::new),
            IndexSource::Given => None,
        };
        *self.index.borrow_mut() = Some(loaded.clone());
        loaded
    }

    fn snapshot(&self) -> HashMap<Vec<u8>, Vec<u8>> {
        if let Some(s) = self.snapshot.borrow().as_ref() {
            return s.clone();
        }
        let s = match (self.index(), self.odb()) {
            (Some(i), Some(o)) => rgit_attr::snapshot(&i, o),
            _ => HashMap::new(),
        };
        *self.snapshot.borrow_mut() = Some(s.clone());
        s
    }

    /// The conversion attributes of `path`.
    pub fn attrs(&self, path: &[u8]) -> rgit_convert::Attrs {
        if self.lookup.borrow().is_none() {
            let snapshot = self.snapshot();
            let source = Source::Snapshot {
                root: self.root.clone(),
                index: snapshot,
                order: self.order,
            };
            let mut attrs = Attributes::new(&self.attr_opts, source);
            let ids = rgit_convert::ATTRIBUTES.map(|n| attrs.id(n));
            *self.lookup.borrow_mut() = Some(Lookup { attrs, ids });
        }
        let mut slot = self.lookup.borrow_mut();
        let l = slot.as_mut().expect("created above");
        let out = l.attrs.lookup(path, false);
        for w in l.attrs.take_warnings() {
            eprintln!("{w}");
        }
        rgit_convert::Attrs::from_states(l.ids.map(|id| out.get(id)))
    }

    /// The conversion of `path`.
    pub fn conversion(&self, path: &[u8]) -> Conversion {
        Conversion::new(self.attrs(path), &self.cfg)
    }

    /// Whether `path` is converted at all.
    pub fn converts(&self, path: &[u8]) -> bool {
        !self.conversion(path).is_identity()
    }

    /// Whether `conv` names a filter driver that is configured (and that
    /// rgit does not run yet, #112). Remembered for
    /// [`Converter::skipped_filter`].
    pub fn unapplied_filter(&self, conv: &Conversion) -> bool {
        let Some(name) = &conv.attrs.filter else {
            return false;
        };
        let name = String::from_utf8_lossy(name);
        let configured = ["clean", "smudge", "process"]
            .iter()
            .any(|k| self.git_config.get(format!("filter.{name}.{k}")).is_some());
        if configured {
            self.skipped_filter.set(true);
        }
        configured
    }

    /// Whether a configured filter driver was skipped so far.
    pub fn skipped_filter(&self) -> bool {
        self.skipped_filter.get()
    }

    /// Normalizes line endings even of files whose index blob has CR LF,
    /// without `core.safecrlf` checks (`add --renormalize`).
    pub fn set_renormalize(&self) {
        self.renormalize.set(true);
    }

    /// Prints `n`, a message about `path`, unless a warning about `path`
    /// was printed already.
    fn notice(&self, path: &[u8], n: Notice) {
        if matches!(n, Notice::Warning(_)) && !self.warned.borrow_mut().insert(path.to_vec()) {
            return;
        }
        print_notice(n);
    }

    /// Whether the stage-0 index entry of `path` is text with CR LF.
    fn index_has_crlf(&self, path: &[u8]) -> bool {
        if self.renormalize.get() {
            return false;
        }
        let Some(index) = self.index() else {
            return false;
        };
        let Some(e) = index.get(path, Stage::MERGED) else {
            return false;
        };
        if !e.mode.is_file() {
            return false;
        }
        match self.odb().and_then(|o| o.read(&e.id).ok().flatten()) {
            Some(raw) if raw.kind == ObjectKind::Blob => Stats::new(&raw.data).is_text_with_crlf(),
            _ => false,
        }
    }

    /// The blob form of the work tree content `data` of `path`.
    ///
    /// # Errors
    /// A fatal conversion problem (its hint is printed first).
    pub fn to_git(&self, path: &[u8], data: Vec<u8>, opts: Options) -> Result<Vec<u8>, Die> {
        let conv = self.conversion(path);
        self.to_git_with(&conv, path, data, opts)
    }

    /// [`Converter::to_git`] with a conversion already looked up.
    ///
    /// # Errors
    /// A fatal conversion problem (its hint is printed first).
    pub fn to_git_with(
        &self,
        conv: &Conversion,
        path: &[u8],
        data: Vec<u8>,
        opts: Options,
    ) -> Result<Vec<u8>, Die> {
        if conv.is_identity() {
            return Ok(data);
        }
        let mut opts = opts;
        if self.renormalize.get() {
            opts.safecrlf = SafeCrlf::False;
        }
        let shown = String::from_utf8_lossy(path);
        rgit_convert::to_git(
            &shown,
            data,
            conv,
            &self.cfg,
            opts,
            || self.index_has_crlf(path),
            &mut |n| self.notice(path, n),
        )
        .map_err(|f| {
            if let Some(h) = f.hint {
                eprintln!("hint: {h}");
            }
            Die::Fatal(f.message)
        })
    }

    /// The work tree form of the blob `id` (content `data`) of `path`.
    pub fn to_worktree(&self, path: &[u8], id: &ObjectId, data: Vec<u8>) -> Vec<u8> {
        let conv = self.conversion(path);
        self.unapplied_filter(&conv);
        if conv.is_identity() {
            return data;
        }
        let shown = String::from_utf8_lossy(path);
        rgit_convert::to_worktree(&shown, data, &conv, &id.to_hex(), &mut print_notice)
    }
}

impl rgit_worktree::stat::ContentFilter for Converter {
    fn to_git(&self, path: &[u8], data: Vec<u8>) -> Vec<u8> {
        let conv = self.conversion(path);
        // Quietly: problems are reported instead of failing.
        self.to_git_with(&conv, path, data, Options::QUIET)
            .unwrap_or_default()
    }

    fn converts(&self, path: &[u8]) -> bool {
        Converter::converts(self, path)
    }
}

impl rgit_unpack::Filter for Converter {
    fn to_worktree(&self, path: &[u8], id: &ObjectId, data: Vec<u8>) -> Vec<u8> {
        Converter::to_worktree(self, path, id, data)
    }
}
