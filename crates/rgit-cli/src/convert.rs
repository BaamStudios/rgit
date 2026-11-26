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
//! Filter drivers (`filter=`, rgit_convert::filter) run before the other
//! conversions on the way in and after them on the way out, as
//! gitattributes(5) "Interaction between checkin/checkout attributes"
//! describes. A failing `required` filter is fatal.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use rgit_attr::{AttrId, Attributes, Order, Source};
use rgit_config::ConfigSet;
use rgit_convert::filter::{self, Direction, Driver, Meta, Outcome};
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
        // Unlike the `eol` attribute, `core.eol` ignores case.
        match v.to_ascii_lowercase().as_slice() {
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

/// What `process` filters are told about the commit being checked out.
static CHECKOUT_META: std::sync::Mutex<Meta> = std::sync::Mutex::new(Meta {
    refname: None,
    treeish: None,
    blob: None,
});

/// Tells `process` filters which ref (`ref=`) and commit or tree
/// (`treeish=`) the following checkout writes.
pub fn set_checkout_meta(refname: Option<String>, treeish: Option<&ObjectId>) {
    let mut m = CHECKOUT_META.lock().unwrap_or_else(|e| e.into_inner());
    m.refname = refname;
    m.treeish = treeish.map(ObjectId::to_hex);
}

/// Files converted with a filter driver in this process: path, digest of
/// the work tree content, blob.
type Converted = HashMap<Vec<u8>, (u64, ObjectId)>;

static WRITTEN: std::sync::LazyLock<std::sync::Mutex<Converted>> =
    std::sync::LazyLock::new(Default::default);

fn content_digest(data: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut h);
    h.finish()
}

fn remember(path: &[u8], digest: u64, id: ObjectId) {
    WRITTEN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(path.to_vec(), (digest, id));
}

/// Printed after the `fatal:` line of [`die_now`] (clone's "checkout
/// failed" warning).
static DIE_EPILOGUE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Sets (or clears) what [`die_now`] prints after its message.
pub fn set_die_epilogue(text: Option<String>) {
    *DIE_EPILOGUE.lock().unwrap_or_else(|e| e.into_inner()) = text;
}

/// Dies at once with `fatal: <message>` from inside a conversion that
/// cannot return an error (checkout, comparing with the index): removes
/// lock files and ends the filter processes first.
pub fn die_now(message: &str) -> ! {
    eprintln!("fatal: {message}");
    if let Some(text) = DIE_EPILOGUE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    {
        eprint!("{text}");
    }
    rgit_fs::remove_registered_files();
    filter::finish_all();
    std::process::exit(rgit_messages::EXIT_FATAL);
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

/// A blob a `process` filter delayed.
struct Delayed {
    path: Vec<u8>,
    driver: Driver,
    meta: Meta,
    id: Option<ObjectId>,
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
    drivers: RefCell<HashMap<Vec<u8>, Option<Driver>>>,
    delayed: RefCell<Vec<Delayed>>,
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
            drivers: RefCell::new(HashMap::new()),
            delayed: RefCell::new(Vec::new()),
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

    /// For commands that never read the index (`log`, `show`, `diff
    /// <commit> <commit>`): `.gitattributes` from the work tree only.
    ///
    /// # Errors
    /// Invalid configuration.
    pub fn without_index(repo: &Repo) -> Result<Converter, Die> {
        let c = Converter::base(repo, Order::WorkTreeFirst, IndexSource::Given)?;
        *c.snapshot.borrow_mut() = Some(HashMap::new());
        *c.index.borrow_mut() = Some(None);
        Ok(c)
    }

    /// For `archive`: `.gitattributes` from `files` (the contents of the
    /// archived tree's `.gitattributes` by path) only.
    ///
    /// # Errors
    /// Invalid configuration.
    pub fn with_attribute_files(
        repo: &Repo,
        files: HashMap<Vec<u8>, Vec<u8>>,
    ) -> Result<Converter, Die> {
        let mut c = Converter::base(repo, Order::IndexFirst, IndexSource::Given)?;
        c.root = None;
        *c.snapshot.borrow_mut() = Some(files);
        *c.index.borrow_mut() = Some(None);
        Ok(c)
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

    /// The state of attribute `name` for `path`, from the same files as
    /// the conversion attributes (e.g. `diff`).
    pub fn attr(&self, path: &[u8], name: &[u8]) -> rgit_attr::State {
        self.attrs(path);
        let mut slot = self.lookup.borrow_mut();
        let l = slot.as_mut().expect("created by attrs");
        let id = l.attrs.id(name);
        l.attrs.lookup(path, false).get(id).clone()
    }

    /// The conversion of `path`.
    pub fn conversion(&self, path: &[u8]) -> Conversion {
        Conversion::new(self.attrs(path), &self.cfg)
    }

    /// Whether `path` is converted at all.
    pub fn converts(&self, path: &[u8]) -> bool {
        let conv = self.conversion(path);
        !conv.is_identity() || self.driver(&conv).is_some()
    }

    /// The filter driver `conv` names, if it is configured.
    pub fn driver(&self, conv: &Conversion) -> Option<Driver> {
        let name = conv.attrs.filter.as_ref()?;
        if let Some(d) = self.drivers.borrow().get(name) {
            return d.clone();
        }
        let shown = String::from_utf8_lossy(name).into_owned();
        let cfg = &self.git_config;
        let d = filter::driver(
            &shown,
            |key| {
                cfg.get(format!("filter.{shown}.{key}"))
                    .map(|e| e.value.as_ref().map(|v| v.to_vec()))
            },
            cfg.bool(format!("filter.{shown}.required")).ok().flatten(),
        );
        self.drivers.borrow_mut().insert(name.clone(), d.clone());
        d
    }

    /// Where filters run: the top of the work tree, with `GIT_PREFIX` and
    /// absolute `GIT_DIR` / `GIT_WORK_TREE` when they were given.
    fn filter_context(&self) -> filter::Context {
        let mut env: Vec<(std::ffi::OsString, std::ffi::OsString)> = vec![(
            "GIT_PREFIX".into(),
            self.setup.prefix.clone().unwrap_or_default(),
        )];
        if self.setup.explicit {
            env.push(("GIT_DIR".into(), self.setup.absolute_git_dir().into()));
            if let Some(r) = &self.root {
                env.push(("GIT_WORK_TREE".into(), r.clone().into()));
            }
        }
        filter::Context {
            cwd: self.root.clone(),
            env,
        }
    }

    /// Runs the filter driver of `conv` over `data`. `Err` is the fatal
    /// message of a `required` driver that did not filter.
    fn run_filter(
        &self,
        conv: &Conversion,
        dir: Direction,
        path: &[u8],
        data: Vec<u8>,
        blob: Option<&ObjectId>,
    ) -> Result<Vec<u8>, String> {
        self.run_filter_delayable(conv, dir, path, data, blob, false)
            .map(|d| d.unwrap_or_default())
    }

    /// [`Converter::run_filter`]; with `can_delay` a `process` filter may
    /// delay the blob (`Ok(None)`, kept for [`Converter::finish_delayed`]).
    fn run_filter_delayable(
        &self,
        conv: &Conversion,
        dir: Direction,
        path: &[u8],
        data: Vec<u8>,
        blob: Option<&ObjectId>,
        can_delay: bool,
    ) -> Result<Option<Vec<u8>>, String> {
        let Some(driver) = self.driver(conv) else {
            return Ok(Some(data));
        };
        let mut meta = CHECKOUT_META
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        meta.blob = blob.map(ObjectId::to_hex);
        let ctx = self.filter_context();
        match filter::apply(&driver, dir, path, &data, &meta, &ctx, can_delay) {
            Outcome::Filtered(d) => Ok(Some(d)),
            Outcome::Delayed => {
                self.delayed.borrow_mut().push(Delayed {
                    path: path.to_vec(),
                    driver,
                    meta,
                    id: blob.copied(),
                });
                Ok(None)
            }
            Outcome::NotRun | Outcome::Failed if driver.required => {
                let shown = String::from_utf8_lossy(path);
                Err(match dir {
                    Direction::Clean => {
                        rgit_convert::messages::clean_filter_failed(&shown, &driver.name)
                    }
                    Direction::Smudge => {
                        rgit_convert::messages::smudge_filter_failed(&shown, &driver.name)
                    }
                })
            }
            Outcome::NotRun | Outcome::Failed => Ok(Some(data)),
        }
    }

    /// Fetches the blobs `process` filters delayed and hands each to
    /// `ready`; returns error messages for those that never came.
    pub fn finish_delayed(&self, ready: &mut dyn FnMut(&[u8], Vec<u8>)) -> Vec<String> {
        let mut pending = std::mem::take(&mut *self.delayed.borrow_mut());
        let mut errors = Vec::new();
        let ctx = self.filter_context();
        // Each filter is asked until it answers with an empty list.
        let mut active: Vec<String> = Vec::new();
        for d in &pending {
            let cmd = d.driver.process.clone().unwrap_or_default();
            if !active.contains(&cmd) {
                active.push(cmd);
            }
        }
        while !active.is_empty() {
            let mut still = Vec::new();
            for cmd in active {
                let of_cmd = |d: &Delayed| d.driver.process.as_deref() == Some(cmd.as_str());
                let list = match filter::list_available(&cmd) {
                    Ok(l) if !l.is_empty() => l,
                    _ => continue,
                };
                let mut fetched = false;
                let mut bad = false;
                for p in list {
                    let Some(i) = pending.iter().position(|d| of_cmd(d) && d.path == p) else {
                        errors.push(rgit_convert::messages::not_delayed_earlier(
                            &cmd,
                            &String::from_utf8_lossy(&p),
                        ));
                        bad = true;
                        continue;
                    };
                    fetched = true;
                    let d = pending.remove(i);
                    let out = filter::apply(
                        &d.driver,
                        Direction::Smudge,
                        &d.path,
                        &[],
                        &d.meta,
                        &ctx,
                        false,
                    );
                    match out {
                        Outcome::Filtered(c) => {
                            if let Some(id) = d.id {
                                remember(&d.path, content_digest(&c), id);
                            }
                            ready(&d.path, c);
                        }
                        _ => errors.push(rgit_convert::messages::not_filtered_properly(
                            &String::from_utf8_lossy(&d.path),
                        )),
                    }
                }
                // A filter that names a blob it did not delay is not asked
                // again.
                if !bad && (fetched || pending.iter().any(of_cmd)) {
                    still.push(cmd);
                }
            }
            active = still;
        }
        for d in pending {
            errors.push(rgit_convert::messages::not_filtered_properly(
                &String::from_utf8_lossy(&d.path),
            ));
        }
        errors
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
        if self.driver(conv).is_none() {
            return self.convert_to_git(conv, path, data, opts);
        }
        let digest = content_digest(&data);
        let data = self
            .run_filter(conv, Direction::Clean, path, data, None)
            .map_err(Die::Fatal)?;
        let out = self.convert_to_git(conv, path, data, opts)?;
        if let Ok(id) = rgit_hash::hash_object(self.hash, "blob", &out) {
            remember(path, digest, id);
        }
        Ok(out)
    }

    /// [`Converter::to_git_with`] after the filter driver.
    fn convert_to_git(
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
        self.to_worktree_delayable(path, id, data, false)
            .unwrap_or_default()
    }

    /// [`Converter::to_worktree`]; with `can_delay` `None` when a
    /// `process` filter delayed the blob ([`Converter::finish_delayed`]).
    pub fn to_worktree_delayable(
        &self,
        path: &[u8],
        id: &ObjectId,
        data: Vec<u8>,
        can_delay: bool,
    ) -> Option<Vec<u8>> {
        let conv = self.conversion(path);
        let data = if conv.is_identity() {
            data
        } else {
            let shown = String::from_utf8_lossy(path);
            rgit_convert::to_worktree(&shown, data, &conv, &id.to_hex(), &mut print_notice)
        };
        if self.driver(&conv).is_none() {
            return Some(data);
        }
        match self.run_filter_delayable(&conv, Direction::Smudge, path, data, Some(id), can_delay) {
            Ok(Some(d)) => {
                remember(path, content_digest(&d), *id);
                Some(d)
            }
            Ok(None) => None,
            Err(m) => die_now(&m),
        }
    }

    /// The blob just checked out to or added from `path` if `data` is
    /// exactly what was converted (git does not clean such files again).
    fn just_written(&self, path: &[u8], data: &[u8]) -> Option<Vec<u8>> {
        let id = {
            let w = WRITTEN.lock().unwrap_or_else(|e| e.into_inner());
            let (digest, id) = w.get(path)?;
            if *digest != content_digest(data) {
                return None;
            }
            *id
        };
        let raw = self.odb()?.read(&id).ok().flatten()?;
        Some(raw.data)
    }
}

impl rgit_worktree::stat::ContentFilter for Converter {
    fn to_git(&self, path: &[u8], data: Vec<u8>) -> Vec<u8> {
        let conv = self.conversion(path);
        if self.driver(&conv).is_some() {
            if let Some(blob) = self.just_written(path, &data) {
                return blob;
            }
        }
        // Quietly: problems are reported instead of failing; only a
        // required filter driver is fatal.
        match self.to_git_with(&conv, path, data, Options::QUIET) {
            Ok(d) => d,
            Err(Die::Fatal(m)) => die_now(&m),
            Err(_) => Vec::new(),
        }
    }

    fn converts(&self, path: &[u8]) -> bool {
        Converter::converts(self, path)
    }
}

impl rgit_unpack::Filter for Converter {
    fn passes_unchanged(&self, path: &[u8]) -> bool {
        !Converter::converts(self, path)
    }

    fn to_worktree(&self, path: &[u8], id: &ObjectId, data: Vec<u8>) -> Vec<u8> {
        Converter::to_worktree(self, path, id, data)
    }

    fn to_worktree_delayable(&self, path: &[u8], id: &ObjectId, data: Vec<u8>) -> Option<Vec<u8>> {
        Converter::to_worktree_delayable(self, path, id, data, true)
    }

    fn finish_delayed(&self, ready: &mut dyn FnMut(&[u8], Vec<u8>)) -> Vec<String> {
        Converter::finish_delayed(self, ready)
    }
}
