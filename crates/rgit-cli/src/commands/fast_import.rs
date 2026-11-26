// SPDX-License-Identifier: MIT
//! `git fast-import` (Documentation/git-fast-import.adoc): read a stream
//! of `blob`, `commit`, `tag`, `reset`, ... commands from standard input,
//! write the objects to pack files and update branches and tags at
//! `checkpoint` and at the end.
//!
//! On invalid input the command writes a crash report to
//! `$GIT_DIR/fast_import_crash_<pid>`, keeps the objects written so far and
//! exports the marks, but updates no refs.

#[path = "fast_import/ident.rs"]
mod ident;
#[path = "fast_import/input.rs"]
mod input;
#[path = "fast_import/notes.rs"]
mod notes;
#[path = "fast_import/store.rs"]
mod store;
#[path = "fast_import/tree.rs"]
mod tree;

use std::collections::{BTreeMap, HashMap};
use std::io::{StdinLock, Write};
use std::path::{Path, PathBuf};

use rgit_hash::{HashKind, ObjectId};
use rgit_object::ObjectKind;
use rgit_refs::name::{RefNameFlags, check_refname_format};
use rgit_refs::{RefUpdate, Transaction};

use crate::messages::fast_import as msg;
use crate::prelude::*;
use crate::repo::{self, Mode};
use crate::repo_objects::Repo;
use ident::DateFormat;
use input::Input;
use store::{ClosedPack, Store};
use tree::{Found, Loader, MODE_GITLINK, MODE_TREE, Tree, Writer};

/// How signed commits or tags are handled (`--signed-commits`,
/// `--signed-tags`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum SignedMode {
    Verbatim,
    WarnVerbatim,
    Strip,
    WarnStrip,
    Abort,
    StripIfInvalid,
    /// With the key to sign with (else the default key).
    SignIfInvalid(Option<String>),
    AbortIfInvalid,
}

impl SignedMode {
    fn parse(s: &str) -> Option<SignedMode> {
        Some(match s {
            "verbatim" => SignedMode::Verbatim,
            "warn-verbatim" | "warn" => SignedMode::WarnVerbatim,
            "strip" => SignedMode::Strip,
            "warn-strip" => SignedMode::WarnStrip,
            "abort" => SignedMode::Abort,
            "strip-if-invalid" => SignedMode::StripIfInvalid,
            "abort-if-invalid" => SignedMode::AbortIfInvalid,
            "sign-if-invalid" => SignedMode::SignIfInvalid(None),
            s if s.starts_with("sign-if-invalid=") => SignedMode::SignIfInvalid(
                Some(s["sign-if-invalid=".len()..].to_string()).filter(|k| !k.is_empty()),
            ),
            _ => return None,
        })
    }
}

/// Where an option comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    CommandLine,
    Feature,
    Option,
}

/// A branch fast-import knows about.
struct Branch {
    /// The object the ref is set to (`None`: no commit yet).
    tip: Option<ObjectId>,
    /// The tree of the next commit (loaded from `tip` when `None`).
    tree: Option<Tree>,
    /// `reset ... from <null>`: the ref is deleted.
    delete: bool,
    /// The pack the last commit went into.
    pack_id: Option<u32>,
}

impl Branch {
    fn empty() -> Branch {
        Branch {
            tip: None,
            tree: Some(Tree::default()),
            delete: false,
            pack_id: None,
        }
    }
}

/// What a `from`, `merge`, `to` or `N` argument names.
enum Target {
    /// The null object id.
    Null,
    /// An object.
    Object(ObjectId),
    /// A branch of this import (its tip and tree).
    Branch(Option<ObjectId>, Option<Tree>),
}

/// Object access for tree loading and writing.
struct Objects<'a> {
    store: &'a mut Store,
    repo: &'a Repo,
    closed: &'a mut Vec<ClosedPack>,
}

impl Loader for Objects<'_> {
    fn load_tree(&mut self, oid: &ObjectId) -> Result<Vec<u8>, Die> {
        match self.store.read(self.repo, oid)? {
            Some((ObjectKind::Tree, data)) => Ok(data),
            _ => Err(Die::Fatal(msg::object_not_found(&oid.to_hex()))),
        }
    }
}

impl Writer for Objects<'_> {
    fn write_tree(&mut self, data: &[u8]) -> Result<ObjectId, Die> {
        let closed = &mut *self.closed;
        self.store
            .store(self.repo, ObjectKind::Tree, data, &mut |c| {
                closed.push(ClosedPack {
                    path: c.path.clone(),
                    id: c.id,
                })
            })
    }
}

struct Importer<'c> {
    ctx: &'c Context,
    repo: Repo,
    hash: HashKind,
    store: Store,
    input: Input<StdinLock<'static>>,
    marks: BTreeMap<u64, ObjectId>,
    branches: HashMap<String, Branch>,
    branch_order: Vec<String>,
    tags: Vec<(String, ObjectId)>,
    revs: Option<crate::refs_support::Revs>,
    closed: Vec<ClosedPack>,
    out: Box<dyn Write>,
    path_opts: rgit_unpack::PathOptions,

    // Options.
    cmdline: Vec<(String, Option<String>)>,
    date_format: DateFormat,
    force: bool,
    quiet: bool,
    require_done: bool,
    relative_marks: bool,
    allow_unsafe: bool,
    export_marks: Option<PathBuf>,
    import_cmdline: Vec<(PathBuf, bool)>,
    import_feature: Option<(PathBuf, bool)>,
    pack_edges: Option<std::fs::File>,
    signed_commits: SignedMode,
    signed_tags: SignedMode,
    rewrite_from: BTreeMap<String, HashMap<u64, ObjectId>>,
    rewrite_to: BTreeMap<String, HashMap<u64, ObjectId>>,

    // State.
    seen_data: bool,
    /// Options the stream set (the command line overrides them).
    stream_set: std::collections::HashSet<String>,
    marks_failed: bool,
    failed: bool,
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    // `--allow-unsafe-features` counts only when spelled in full before
    // the first argument that is not an option.
    let mut allow_unsafe = false;
    for a in &argv[1..] {
        if !a.starts_with('-') || a == "--" {
            break;
        }
        if a == "--allow-unsafe-features" {
            allow_unsafe = true;
        }
    }
    let collected: RefCell<Vec<(String, Option<String>)>> = RefCell::new(Vec::new());
    let push = |name: &'static str| {
        let c = &collected;
        move |v: Option<&str>, unset: bool| {
            let n = if unset {
                format!("no-{name}")
            } else {
                name.to_string()
            };
            c.borrow_mut().push((n, v.map(str::to_owned)));
            Ok(())
        }
    };
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::group(msg::GROUP_COMMON),
            Opt::callback(None, "date-format", Some("fmt"), msg::HELP_DATE_FORMAT, push("date-format")).noneg(),
            Opt::callback(None, "stats", None, msg::HELP_STATS, push("stats")).noarg().noneg(),
            Opt::callback(None, "quiet", None, msg::HELP_QUIET, push("quiet")).noarg().noneg(),
            Opt::callback(None, "force", None, msg::HELP_FORCE, push("force")).noarg().noneg(),
            Opt::callback(None, "done", None, msg::HELP_DONE, push("done")).noarg().noneg(),
            Opt::callback(None, "max-pack-size", Some("n"), msg::HELP_MAX_PACK_SIZE, push("max-pack-size")).noneg(),
            Opt::callback(None, "big-file-threshold", Some("n"), msg::HELP_BIG_FILE, push("big-file-threshold")).noneg(),
            Opt::callback(None, "depth", Some("n"), msg::HELP_DEPTH, push("depth")).noneg(),
            Opt::callback(None, "active-branches", Some("n"), msg::HELP_ACTIVE_BRANCHES, push("active-branches")).noneg(),
            Opt::group(msg::GROUP_MARKS),
            Opt::callback(None, "import-marks", Some("file"), msg::HELP_IMPORT_MARKS, push("import-marks")).noneg(),
            Opt::callback(None, "import-marks-if-exists", Some("file"), msg::HELP_IMPORT_MARKS_IF_EXISTS, push("import-marks-if-exists")).noneg(),
            Opt::callback(None, "export-marks", Some("file"), msg::HELP_EXPORT_MARKS, push("export-marks")).noneg(),
            Opt::callback(None, "relative-marks", None, msg::HELP_RELATIVE_MARKS, push("relative-marks")).noarg(),
            Opt::group(msg::GROUP_SUBMODULE),
            Opt::callback(None, "rewrite-submodules-from", Some("name:filename"), msg::HELP_REWRITE_FROM, push("rewrite-submodules-from")).noneg(),
            Opt::callback(None, "rewrite-submodules-to", Some("name:filename"), msg::HELP_REWRITE_TO, push("rewrite-submodules-to")).noneg(),
            Opt::group(msg::GROUP_SIGNING),
            Opt::callback(None, "signed-commits", Some("mode"), msg::HELP_SIGNED_COMMITS, push("signed-commits")).noneg(),
            Opt::callback(None, "signed-tags", Some("mode"), msg::HELP_SIGNED_TAGS, push("signed-tags")).noneg(),
            Opt::callback(None, "allow-unsafe-features", None, msg::HELP_UNSAFE, push("allow-unsafe-features")).noarg().noneg().hidden(),
            Opt::callback(None, "cat-blob-fd", Some("fd"), msg::HELP_CAT_BLOB_FD, push("cat-blob-fd")).noneg().hidden(),
            Opt::callback(None, "export-pack-edges", Some("file"), msg::HELP_PACK_EDGES, push("export-pack-edges")).noneg().hidden(),
        ],
    );
    parser.parse(argv)?;
    drop(parser);
    let cmdline = collected.into_inner();
    // Invalid signing modes are usage errors.
    for (name, value) in &cmdline {
        if name == "signed-commits" || name == "signed-tags" {
            let v = value.as_deref().unwrap_or("");
            if SignedMode::parse(v).is_none() {
                eprintln!("usage: {}", msg::unknown_signed_mode(name, v));
                return Err(Die::Exit(129));
            }
        }
    }

    let setup = repo::setup(ctx, Mode::Required)?;
    let repo = Repo::open(ctx, setup)?;
    let hash = repo.hash;
    let config = &repo.config;
    let int = |k: &str| config.int(k).ok().flatten();
    let level = int("pack.compression")
        .or_else(|| int("core.compression"))
        .filter(|&l| (0..=9).contains(&l))
        .map(|l| l as u32);
    let mut store = Store::new(hash, &crate::repo_objects::objects_dir(&repo.setup), level);
    store.unpack_limit = int("fastimport.unpacklimit")
        .or_else(|| int("transfer.unpacklimit"))
        .map_or(100, |v| v.max(0) as u64);
    let path_opts = rgit_unpack::PathOptions {
        ignore_case: false,
        protect_ntfs: config
            .bool("core.protectntfs")
            .ok()
            .flatten()
            .unwrap_or(rgit_unpack::PathOptions::default().protect_ntfs),
        protect_hfs: config
            .bool("core.protecthfs")
            .ok()
            .flatten()
            .unwrap_or(rgit_unpack::PathOptions::default().protect_hfs),
    };
    let stdin: &'static std::io::Stdin = Box::leak(Box::new(std::io::stdin()));
    let mut imp = Importer {
        ctx,
        repo,
        hash,
        store,
        input: Input::new(stdin.lock()),
        marks: BTreeMap::new(),
        branches: HashMap::new(),
        branch_order: Vec::new(),
        tags: Vec::new(),
        revs: None,
        closed: Vec::new(),
        out: Box::new(std::io::stdout()),
        path_opts,
        cmdline,
        date_format: DateFormat::Raw,
        force: false,
        quiet: false,
        require_done: false,
        relative_marks: false,
        allow_unsafe,
        export_marks: None,
        import_cmdline: Vec::new(),
        import_feature: None,
        pack_edges: None,
        signed_commits: SignedMode::Verbatim,
        signed_tags: SignedMode::Verbatim,
        rewrite_from: BTreeMap::new(),
        rewrite_to: BTreeMap::new(),
        seen_data: false,
        stream_set: Default::default(),
        marks_failed: false,
        failed: false,
    };
    match imp.run() {
        Ok(()) => {}
        Err(Die::Fatal(m)) => {
            eprintln!("fatal: {m}");
            imp.crash(&m);
            return Err(Die::Exit(128));
        }
        Err(e) => return Err(e),
    }
    Ok(if imp.failed { 1 } else { 0 })
}

/// Parse a decimal `--<name>` argument.
fn non_negative(name: &str, v: Option<&str>) -> Result<u64, Die> {
    let v = v.unwrap_or("");
    if v.is_empty() || !v.bytes().all(|c| c.is_ascii_digit()) {
        return Err(Die::Fatal(msg::not_non_negative(name)));
    }
    v.parse::<u64>()
        .map_err(|_| Die::Fatal(msg::not_non_negative(name)))
}

/// A size with an optional `k`, `m` or `g` suffix.
fn parse_size(v: &str) -> Option<u64> {
    let (num, mul) = match v.as_bytes().last()? {
        b'k' | b'K' => (&v[..v.len() - 1], 1u64 << 10),
        b'm' | b'M' => (&v[..v.len() - 1], 1 << 20),
        b'g' | b'G' => (&v[..v.len() - 1], 1 << 30),
        _ => (v, 1),
    };
    num.parse::<u64>().ok()?.checked_mul(mul)
}

fn starts(line: &[u8], p: &str) -> bool {
    line.starts_with(p.as_bytes())
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// The digits of a mark at the start of `s` (after `:`): value and rest.
fn mark_number(s: &[u8]) -> (u64, &[u8]) {
    let n = s.iter().take_while(|c| c.is_ascii_digit()).count();
    let mut v: u64 = 0;
    for &d in &s[..n] {
        v = v.saturating_mul(10).saturating_add(u64::from(d - b'0'));
    }
    (v, &s[n..])
}

impl<'c> Importer<'c> {
    fn run(&mut self) -> Result<(), Die> {
        let cmdline = std::mem::take(&mut self.cmdline);
        for (name, value) in &cmdline {
            self.apply_option(name, value.as_deref(), Source::CommandLine)?;
        }
        self.cmdline = cmdline;
        let mut done = false;
        while self.input.next()? {
            let line = self.input.line.clone();
            if !starts(&line, "feature ") && !starts(&line, "option ") {
                self.prepare_data()?;
            }
            if line == b"blob" {
                self.parse_blob()?;
            } else if starts(&line, "commit ") {
                self.parse_commit()?;
            } else if starts(&line, "tag ") {
                self.parse_tag()?;
            } else if starts(&line, "reset ") {
                self.parse_reset()?;
            } else if starts(&line, "ls ") {
                self.parse_ls(None)?;
            } else if starts(&line, "cat-blob ") {
                self.parse_cat_blob()?;
            } else if starts(&line, "get-mark ") {
                self.parse_get_mark()?;
            } else if line == b"checkpoint" {
                self.checkpoint()?;
                self.input.skip_optional_lf()?;
            } else if line == b"done" {
                done = true;
                break;
            } else if line == b"alias" {
                self.parse_alias()?;
            } else if starts(&line, "progress ") {
                let mut out = std::io::stdout();
                let _ = out.write_all(&line);
                let _ = out.write_all(b"\n");
                let _ = out.flush();
                self.input.skip_optional_lf()?;
            } else if let Some(f) = line.strip_prefix(b"feature ") {
                self.parse_feature(&lossy(f))?;
            } else if let Some(o) = line.strip_prefix(b"option ") {
                self.parse_option(&lossy(o))?;
            } else {
                return Err(Die::Fatal(msg::unsupported_command(&lossy(&line))));
            }
            self.flush_closed()?;
        }
        if !done && self.require_done {
            // The marks are imported even then (and so exported again).
            self.prepare_data()?;
            return Err(Die::Fatal(msg::STREAM_ENDS_EARLY.into()));
        }
        self.prepare_data()?;
        self.finish()
    }

    /// Before the first command that is not `feature` or `option`: the
    /// command line overrides the stream, and marks are imported.
    fn prepare_data(&mut self) -> Result<(), Die> {
        if self.seen_data {
            return Ok(());
        }
        self.seen_data = true;
        let cmdline = std::mem::take(&mut self.cmdline);
        for (name, value) in &cmdline {
            let base = name.strip_prefix("no-").unwrap_or(name);
            if !self.stream_set.contains(base) || name.starts_with("import-marks") {
                continue;
            }
            self.apply_option(name, value.as_deref(), Source::CommandLine)?;
        }
        self.cmdline = cmdline;
        let files = if self.import_cmdline.is_empty() {
            self.import_feature.clone().into_iter().collect()
        } else {
            self.import_cmdline.clone()
        };
        for (path, if_exists) in files {
            self.marks_failed = true;
            self.import_marks(&path, if_exists)?;
            self.marks_failed = false;
        }
        Ok(())
    }

    fn marks_path(&self, v: &str, src: Source) -> PathBuf {
        if self.relative_marks {
            self.repo.setup.git_dir.join("info").join("fast-import").join(v)
        } else {
            self.user_path(v, src)
        }
    }

    /// A file named on the command line, relative to the directory the
    /// command started in.
    fn user_path(&self, v: &str, src: Source) -> PathBuf {
        let p = PathBuf::from(v);
        match &self.repo.setup.prefix {
            Some(prefix) if src == Source::CommandLine && p.is_relative() => {
                Path::new(prefix).join(p)
            }
            _ => p,
        }
    }

    /// Apply one option of the command line or the stream. Returns false
    /// when the name is not an option.
    fn apply_option(&mut self, name: &str, value: Option<&str>, src: Source) -> Result<bool, Die> {
        if src != Source::CommandLine {
            self.stream_set
                .insert(name.strip_prefix("no-").unwrap_or(name).to_string());
        }
        let v = value.unwrap_or("");
        match name {
            "date-format" => {
                self.date_format = DateFormat::parse(v)
                    .ok_or_else(|| Die::Fatal(msg::unknown_date_format(v)))?;
            }
            "force" => self.force = true,
            "done" => self.require_done = true,
            "quiet" => self.quiet = true,
            "stats" => self.quiet = false,
            "relative-marks" => self.relative_marks = true,
            "no-relative-marks" => self.relative_marks = false,
            "allow-unsafe-features" => {}
            "import-marks" | "import-marks-if-exists" => {
                let path = self.marks_path(v, src);
                let if_exists = name == "import-marks-if-exists";
                if src == Source::CommandLine {
                    self.import_cmdline.push((path, if_exists));
                } else {
                    if self.import_feature.is_some() {
                        return Err(Die::Fatal(msg::ONE_IMPORT_MARKS.into()));
                    }
                    self.import_feature = Some((path, if_exists));
                }
            }
            "export-marks" => self.export_marks = Some(self.marks_path(v, src)),
            "max-pack-size" => {
                let mut n = parse_size(v).ok_or_else(|| Die::Fatal(msg::not_non_negative(name)))?;
                if n < 8192 {
                    eprintln!("warning: {}", msg::max_pack_size_mb(n));
                    n *= 1 << 20;
                }
                self.store.max_pack_size = n;
            }
            "big-file-threshold" => {
                parse_size(v).ok_or_else(|| Die::Fatal(msg::not_non_negative(name)))?;
            }
            "depth" | "active-branches" => {
                non_negative(name, value)?;
            }
            "cat-blob-fd" => {
                let fd = non_negative(name, value)?;
                if src == Source::CommandLine {
                    self.set_cat_blob_fd(fd)?;
                }
            }
            "export-pack-edges" => {
                let f = std::fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(self.user_path(v, src))
                    .map_err(|e| {
                        Die::Fatal(msg::cannot_read(v, &crate::global::strerror(&e)))
                    })?;
                self.pack_edges = Some(f);
            }
            "signed-commits" | "signed-tags" => {
                let mode = SignedMode::parse(v)
                    .ok_or_else(|| Die::Fatal(msg::unknown_signed_mode(name, v)))?;
                if name == "signed-commits" {
                    self.signed_commits = mode;
                } else {
                    self.signed_tags = mode;
                }
            }
            "rewrite-submodules-from" | "rewrite-submodules-to" => {
                let (sub, file) = v
                    .split_once(':')
                    .ok_or_else(|| Die::Fatal(msg::REWRITE_FORMAT.into()))?;
                let map = read_marks_file(&self.user_path(file, src), file, self.hash)?;
                if name == "rewrite-submodules-from" {
                    self.rewrite_from.insert(sub.to_string(), map);
                } else {
                    self.rewrite_to.insert(sub.to_string(), map);
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn set_cat_blob_fd(&mut self, fd: u64) -> Result<(), Die> {
        let fd = i32::try_from(fd).map_err(|_| Die::Fatal(msg::not_non_negative("cat-blob-fd")))?;
        if fd == 1 {
            self.out = Box::new(std::io::stdout());
            return Ok(());
        }
        // SAFETY: fcntl(F_GETFD) only queries the descriptor.
        if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
            return Err(Die::Fatal(msg::not_non_negative("cat-blob-fd")));
        }
        // SAFETY: the descriptor is open (checked above) and was handed to
        // this process for writing responses; nothing else in the process
        // uses it.
        let file = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(fd) };
        self.out = Box::new(file);
        Ok(())
    }

    fn parse_feature(&mut self, f: &str) -> Result<(), Die> {
        let (name, value) = match f.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (f, None),
        };
        let unsafe_name = match name {
            "import-marks" | "import-marks-if-exists" | "rewrite-submodules-from"
            | "rewrite-submodules-to"
                if value.is_some() =>
            {
                Some(name.to_string())
            }
            "export-marks" if value.is_some() => Some(f.to_string()),
            _ => None,
        };
        if let Some(n) = unsafe_name {
            if !self.allow_unsafe {
                return Err(Die::Fatal(msg::feature_forbidden(&n)));
            }
        }
        if self.seen_data {
            return Err(Die::Fatal(msg::feature_after_data(f)));
        }
        let known = match (name, value) {
            ("date-format", Some(_))
            | ("import-marks", Some(_))
            | ("import-marks-if-exists", Some(_))
            | ("export-marks", Some(_))
            | ("rewrite-submodules-from", Some(_))
            | ("rewrite-submodules-to", Some(_)) => {
                if (name == "import-marks" || name == "import-marks-if-exists")
                    && !self.import_cmdline.is_empty()
                {
                    if self.import_feature.is_some() {
                        return Err(Die::Fatal(msg::ONE_IMPORT_MARKS.into()));
                    }
                    self.import_feature = Some((PathBuf::new(), true));
                    return Ok(());
                }
                self.apply_option(name, value, Source::Feature)?
            }
            ("relative-marks", None)
            | ("no-relative-marks", None)
            | ("force", None)
            | ("done", None) => self.apply_option(name, None, Source::Feature)?,
            ("get-mark", None) | ("cat-blob", None) | ("ls", None) | ("notes", None)
            | ("alias", None) => true,
            _ => false,
        };
        if !known {
            return Err(Die::Fatal(msg::unsupported_feature(f)));
        }
        Ok(())
    }

    fn parse_option(&mut self, o: &str) -> Result<(), Die> {
        let Some(opt) = o.strip_prefix("git ") else {
            return Ok(());
        };
        if self.seen_data {
            return Err(Die::Fatal(msg::option_after_data(opt)));
        }
        let (name, value) = match opt.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (opt, None),
        };
        let allowed = matches!(
            (name, value.is_some()),
            ("max-pack-size", true)
                | ("big-file-threshold", true)
                | ("depth", true)
                | ("active-branches", true)
                | ("export-pack-edges", true)
                | ("cat-blob-fd", true)
                | ("signed-commits", true)
                | ("signed-tags", true)
                | ("quiet", false)
                | ("stats", false)
                | ("allow-unsafe-features", false)
        );
        if !allowed {
            return Err(Die::Fatal(msg::unsupported_option(opt)));
        }
        if name == "export-pack-edges" && !self.allow_unsafe {
            return Err(Die::Fatal(msg::option_forbidden(name)));
        }
        if name == "cat-blob-fd" {
            non_negative(name, value)?;
            return Ok(());
        }
        self.apply_option(name, value, Source::Option)?;
        Ok(())
    }

    // ---------------------------------------------------------------
    // Marks

    fn import_marks(&mut self, path: &Path, if_exists: bool) -> Result<(), Die> {
        if if_exists && !path.exists() {
            return Ok(());
        }
        let text = std::fs::read(path).map_err(|e| {
            Die::Fatal(msg::cannot_read(
                &path.display().to_string(),
                &crate::global::strerror(&e),
            ))
        })?;
        for line in text.split(|&c| c == b'\n') {
            if line.is_empty() {
                continue;
            }
            let (mark, oid) = parse_mark_line(line, self.hash)
                .ok_or_else(|| Die::Fatal(msg::corrupt_mark_line(&lossy(line))))?;
            if self.store.kind_of(&self.repo, &oid).is_none() {
                return Err(Die::Fatal(msg::object_not_found(&oid.to_hex())));
            }
            self.marks.insert(mark, oid);
        }
        Ok(())
    }

    fn export_marks(&self) -> Result<(), Die> {
        let Some(path) = &self.export_marks else {
            return Ok(());
        };
        let mut out = Vec::new();
        for (m, oid) in &self.marks {
            out.extend_from_slice(format!(":{m} {oid}\n").as_bytes());
        }
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(dir);
            }
        }
        let lock = {
            let mut p = path.as_os_str().to_owned();
            p.push(".lock");
            PathBuf::from(p)
        };
        let shown = path.display().to_string();
        std::fs::write(&lock, &out)
            .map_err(|e| Die::Fatal(msg::unable_to_write(&shown, &crate::global::strerror(&e))))?;
        std::fs::rename(&lock, path)
            .map_err(|e| Die::Fatal(msg::unable_to_write(&shown, &crate::global::strerror(&e))))?;
        Ok(())
    }

    fn mark(&self, m: u64) -> Result<ObjectId, Die> {
        self.marks
            .get(&m)
            .copied()
            .ok_or_else(|| Die::Fatal(msg::mark_not_declared(m)))
    }

    /// An optional `mark :<n>` line; reads the next line after it.
    fn parse_mark(&mut self) -> Result<Option<u64>, Die> {
        let Some(rest) = self.input.line.strip_prefix(b"mark :") else {
            return Ok(None);
        };
        let (m, _) = mark_number(rest);
        self.input.next()?;
        Ok(Some(m))
    }

    fn set_mark(&mut self, m: Option<u64>, oid: ObjectId) {
        if let Some(m) = m {
            if m > 0 {
                self.marks.insert(m, oid);
            }
        }
    }

    fn skip_original_oid(&mut self) -> Result<(), Die> {
        if starts(&self.input.line, "original-oid ") {
            self.input.next()?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------
    // Objects

    fn store_object(&mut self, kind: ObjectKind, data: &[u8]) -> Result<ObjectId, Die> {
        let closed = &mut self.closed;
        self.store.store(&self.repo, kind, data, &mut |c| {
            closed.push(ClosedPack {
                path: c.path.clone(),
                id: c.id,
            })
        })
    }

    fn objects(&mut self) -> Objects<'_> {
        Objects {
            store: &mut self.store,
            repo: &self.repo,
            closed: &mut self.closed,
        }
    }

    fn kind_of(&self, oid: &ObjectId) -> Option<ObjectKind> {
        self.store.kind_of(&self.repo, oid)
    }

    fn read_object(&mut self, oid: &ObjectId) -> Result<Option<(ObjectKind, Vec<u8>)>, Die> {
        self.store.read(&self.repo, oid)
    }

    /// Follow tags from `oid` to a non-tag object.
    fn peel(&mut self, oid: ObjectId) -> Result<Option<(ObjectKind, ObjectId, Vec<u8>)>, Die> {
        let mut cur = oid;
        for _ in 0..64 {
            let Some((kind, data)) = self.read_object(&cur)? else {
                return Ok(None);
            };
            if kind != ObjectKind::Tag {
                return Ok(Some((kind, cur, data)));
            }
            let tag = rgit_object::Tag::parse(self.hash, &data)
                .map_err(|_| Die::Fatal(msg::object_not_found(&cur.to_hex())))?;
            cur = tag
                .object()
                .map_err(|_| Die::Fatal(msg::object_not_found(&cur.to_hex())))?;
        }
        Ok(None)
    }

    /// The tree of commit (or tag of a commit) `oid`.
    fn commit_tree(&mut self, oid: ObjectId) -> Result<Option<ObjectId>, Die> {
        match self.peel(oid)? {
            Some((ObjectKind::Commit, _, data)) => Ok(rgit_object::Commit::parse(self.hash, &data)
                .ok()
                .and_then(|c| c.tree().ok())),
            Some((ObjectKind::Tree, id, _)) => Ok(Some(id)),
            _ => Ok(None),
        }
    }

    fn flush_closed(&mut self) -> Result<(), Die> {
        if self.closed.is_empty() {
            return Ok(());
        }
        self.revs = None;
        let closed = std::mem::take(&mut self.closed);
        for c in &closed {
            self.write_pack_edges(c);
        }
        Ok(())
    }

    fn write_pack_edges(&mut self, c: &ClosedPack) {
        let Some(f) = &mut self.pack_edges else {
            return;
        };
        let shown = display_path(&c.path);
        let mut line = format!("{shown}:");
        for name in &self.branch_order {
            if let Some(b) = self.branches.get(name) {
                if b.pack_id == Some(c.id) {
                    if let Some(tip) = b.tip {
                        line.push_str(&format!(" {tip}"));
                    }
                }
            }
        }
        line.push('\n');
        let _ = f.write_all(line.as_bytes());
    }

    fn close_pack(&mut self) -> Result<(), Die> {
        let closed = self.store.close(&self.repo)?;
        if let Some(c) = closed {
            self.closed.push(c);
        }
        self.revs = None;
        self.flush_closed()
    }

    // ---------------------------------------------------------------
    // Resolving commit-ish arguments

    fn revs(&mut self) -> Result<&crate::refs_support::Revs, Die> {
        if self.revs.is_none() {
            self.revs = Some(crate::refs_support::Revs::new(self.ctx, &self.repo.setup)?);
        }
        Ok(self.revs.as_ref().expect("set"))
    }

    /// Resolve the argument of `from`, `merge`, `to` or the commit-ish of
    /// `N`. `line` is the whole command (for messages); `current` the
    /// branch being committed to.
    fn resolve(&mut self, spec: &[u8], line: &[u8], current: Option<&str>) -> Result<Target, Die> {
        let text = lossy(spec);
        if current == Some(text.as_str()) {
            return Err(Die::Fatal(msg::branch_from_itself(&text)));
        }
        if let Some(b) = self.branches.get(&text) {
            return Ok(Target::Branch(b.tip, b.tree.clone()));
        }
        if let Some(rest) = spec.strip_prefix(b":") {
            let (m, rest) = mark_number(rest);
            if !rest.is_empty() {
                return Err(Die::Fatal(msg::garbage_after_mark(&lossy(line))));
            }
            return Ok(Target::Object(self.mark(m)?));
        }
        if spec.len() == self.hash.hex_len() {
            if let Ok(oid) = ObjectId::from_hex(self.hash, spec) {
                if oid.is_null() {
                    return Ok(Target::Null);
                }
                return Ok(Target::Object(oid));
            }
        }
        let resolved = self.revs()?.resolve(&text);
        match resolved {
            Some(oid) => Ok(Target::Object(oid)),
            None => Err(Die::Fatal(msg::invalid_ref_or_sha1(&text))),
        }
    }

    /// `resolve` for a commit: the commit id (tags peeled).
    fn resolve_commit(
        &mut self,
        spec: &[u8],
        line: &[u8],
        current: Option<&str>,
    ) -> Result<Target, Die> {
        let t = self.resolve(spec, line, current)?;
        if let Target::Object(oid) = t {
            return Ok(Target::Object(self.require_commit(oid, spec)?));
        }
        Ok(t)
    }

    fn require_commit(&mut self, oid: ObjectId, spec: &[u8]) -> Result<ObjectId, Die> {
        match self.peel(oid)? {
            Some((ObjectKind::Commit, id, _)) => Ok(id),
            _ => {
                if let Some(rest) = spec.strip_prefix(b":") {
                    let (m, _) = mark_number(rest);
                    return Err(Die::Fatal(msg::mark_not_a_commit(m)));
                }
                Err(Die::Fatal(msg::not_a_valid_commit(&oid.to_hex())))
            }
        }
    }

    // ---------------------------------------------------------------
    // Commands

    fn parse_blob(&mut self) -> Result<(), Die> {
        self.input.next()?;
        let mark = self.parse_mark()?;
        self.skip_original_oid()?;
        let data = self.input.read_data()?;
        let oid = self.store_object(ObjectKind::Blob, &data)?;
        self.set_mark(mark, oid);
        Ok(())
    }

    fn check_branch_name(name: &str) -> Result<(), Die> {
        check_refname_format(name.as_bytes(), RefNameFlags::ONELEVEL)
            .map_err(|_| Die::Fatal(msg::bad_branch_name(name)))
    }

    fn take_branch(&mut self, name: &str) -> Branch {
        match self.branches.remove(name) {
            Some(b) => b,
            None => {
                self.branch_order.push(name.to_string());
                Branch::empty()
            }
        }
    }

    /// The tree of `b`, loaded from its tip when needed.
    fn branch_tree(&mut self, b: &mut Branch) -> Result<(), Die> {
        if b.tree.is_some() {
            return Ok(());
        }
        let tree = match b.tip {
            Some(tip) => match self.commit_tree(tip)? {
                Some(t) => {
                    let hash = self.hash;
                    Tree::load(hash, &t, &mut self.objects())?
                }
                None => Tree::default(),
            },
            None => Tree::default(),
        };
        b.tree = Some(tree);
        Ok(())
    }

    /// Set the branch to what `from` names.
    fn apply_from(&mut self, b: &mut Branch, t: Target) -> Result<(), Die> {
        match t {
            Target::Null => {
                b.tip = None;
                b.tree = Some(Tree::default());
            }
            Target::Object(oid) => {
                b.tip = Some(oid);
                b.tree = None;
            }
            Target::Branch(tip, tree) => {
                b.tip = tip;
                b.tree = tree;
            }
        }
        Ok(())
    }

    fn parse_commit(&mut self) -> Result<(), Die> {
        let name = lossy(&self.input.line["commit ".len()..]);
        Self::check_branch_name(&name)?;
        let mut b = self.take_branch(&name);
        let result = self.parse_commit_body(&name, &mut b);
        self.branches.insert(name, b);
        result
    }

    fn parse_commit_body(&mut self, name: &str, b: &mut Branch) -> Result<(), Die> {
        self.input.next()?;
        let mark = self.parse_mark()?;
        self.skip_original_oid()?;
        let fmt = self.date_format;
        let mut author = None;
        if let Some(a) = self.input.line.strip_prefix(b"author ") {
            author = Some(ident::parse_ident(a, fmt).map_err(Die::Fatal)?);
            self.input.next()?;
        }
        let Some(c) = self.input.line.strip_prefix(b"committer ") else {
            return Err(Die::Fatal(msg::EXPECTED_COMMITTER.into()));
        };
        let committer = ident::parse_ident(c, fmt).map_err(Die::Fatal)?;
        self.input.next()?;
        let mut sig_sha1: Option<Vec<u8>> = None;
        let mut sig_sha256: Option<Vec<u8>> = None;
        while let Some(rest) = self.input.line.strip_prefix(b"gpgsig ") {
            let line = self.input.text();
            let rest = lossy(rest);
            let mut parts = rest.splitn(2, ' ');
            let algo = parts.next().unwrap_or("").to_string();
            if parts.next().is_none_or(str::is_empty) {
                return Err(Die::Fatal(msg::gpgsig_format(&line)));
            }
            if algo != "sha1" && algo != "sha256" {
                return Err(Die::Fatal(msg::gpgsig_unknown_algo(&algo)));
            }
            self.input.next()?;
            let sig = self.input.read_data()?;
            let slot = if algo == "sha1" {
                &mut sig_sha1
            } else {
                &mut sig_sha256
            };
            if slot.is_some() {
                eprintln!(
                    "warning: {}",
                    if algo == "sha1" {
                        msg::MULTIPLE_SHA1_SIGS
                    } else {
                        msg::MULTIPLE_SHA256_SIGS
                    }
                );
            } else {
                *slot = Some(sig);
            }
            self.input.next()?;
        }
        let mut encoding = None;
        if let Some(e) = self.input.line.strip_prefix(b"encoding ") {
            encoding = Some(e.to_vec());
            self.input.next()?;
        }
        let message = self.input.read_data()?;
        self.input.next()?;

        if let Some(spec) = self.input.line.strip_prefix(b"from ") {
            let spec = spec.to_vec();
            let line = self.input.line.clone();
            let t = self.resolve_commit(&spec, &line, Some(name))?;
            self.apply_from(b, t)?;
            self.input.next()?;
        }
        let mut merges = Vec::new();
        while let Some(spec) = self.input.line.strip_prefix(b"merge ") {
            let spec = spec.to_vec();
            let line = self.input.line.clone();
            match self.resolve_commit(&spec, &line, None)? {
                Target::Object(oid) => merges.push(oid),
                Target::Branch(Some(tip), _) => {
                    let c = self.require_commit(tip, &spec)?;
                    merges.push(c);
                }
                Target::Branch(None, _) | Target::Null => {
                    return Err(Die::Fatal(msg::invalid_ref_or_sha1(&lossy(&spec))));
                }
            }
            self.input.next()?;
        }
        self.branch_tree(b)?;
        // The fan-out of the notes when the first `N` command came.
        let mut notes_fanout: Option<usize> = None;
        loop {
            if self.input.eof && self.input.line.is_empty() {
                break;
            }
            let line = self.input.line.clone();
            if starts(&line, "M ") {
                self.file_modify(b, &line)?;
            } else if starts(&line, "D ") {
                self.file_delete(b, &line)?;
            } else if starts(&line, "R ") {
                self.file_copy(b, &line, true)?;
            } else if starts(&line, "C ") {
                self.file_copy(b, &line, false)?;
            } else if starts(&line, "N ") {
                let fanout = match notes_fanout {
                    Some(f) => f,
                    None => {
                        let hash = self.hash;
                        let mut root = b.tree.take().unwrap_or_default();
                        let n = notes::count(hash, &mut root, &mut self.objects());
                        b.tree = Some(root);
                        let f = notes::fanout_for(n?);
                        notes_fanout = Some(f);
                        f
                    }
                };
                self.note_modify(b, &line, name, fanout)?;
            } else if line == b"deleteall" {
                b.tree = Some(Tree::default());
            } else if starts(&line, "ls ") {
                self.parse_ls(Some(b))?;
            } else if starts(&line, "cat-blob ") {
                self.parse_cat_blob()?;
            } else if line.is_empty() {
                break;
            } else {
                self.input.unread();
                break;
            }
            self.input.next()?;
        }
        let hash = self.hash;
        let mut tree = b.tree.take().unwrap_or_default();
        if let Some(before) = notes_fanout {
            let after = notes::fanout_for(notes::count(hash, &mut tree, &mut self.objects())?);
            if after != before {
                notes::relayout(hash, &mut tree, after, &mut self.objects())?;
            }
        }
        let tree_oid = tree::write(&mut tree, &mut self.objects())?;
        b.tree = Some(tree);

        let mut parents = Vec::new();
        if let Some(tip) = b.tip {
            parents.push(tip);
        }
        parents.extend(merges);
        let mut body = Vec::new();
        body.extend_from_slice(format!("tree {tree_oid}\n").as_bytes());
        for p in &parents {
            body.extend_from_slice(format!("parent {p}\n").as_bytes());
        }
        body.extend_from_slice(b"author ");
        body.extend_from_slice(author.as_deref().unwrap_or(&committer));
        body.extend_from_slice(b"\ncommitter ");
        body.extend_from_slice(&committer);
        body.push(b'\n');
        if let Some(e) = &encoding {
            body.extend_from_slice(b"encoding ");
            body.extend_from_slice(e);
            body.push(b'\n');
        }
        let unsigned_len = body.len();
        let signed = sig_sha1.is_some() || sig_sha256.is_some();
        for (header, sig) in [("gpgsig", &sig_sha1), ("gpgsig-sha256", &sig_sha256)] {
            if let Some(sig) = sig {
                write_header(&mut body, header, sig);
            }
        }
        if signed {
            let mut plain = body[..unsigned_len].to_vec();
            plain.push(b'\n');
            plain.extend_from_slice(&message);
            let plain_oid = self.store.object_id(ObjectKind::Commit, &plain)?;
            let count = usize::from(sig_sha1.is_some()) + usize::from(sig_sha256.is_some());
            match self.signed_commits.clone() {
                SignedMode::Verbatim => {}
                SignedMode::WarnVerbatim => {
                    for _ in 0..count {
                        eprintln!("warning: {}", msg::importing_signed_commit(&plain_oid.to_hex()));
                    }
                }
                SignedMode::Strip => body.truncate(unsigned_len),
                SignedMode::WarnStrip => {
                    for _ in 0..count {
                        eprintln!("warning: {}", msg::stripping_signed_commit(&plain_oid.to_hex()));
                    }
                    body.truncate(unsigned_len);
                }
                SignedMode::Abort => {
                    return Err(Die::Fatal(msg::signed_commit_abort(&plain_oid.to_hex())));
                }
                mode => {
                    let mut full = body.clone();
                    full.push(b'\n');
                    full.extend_from_slice(&message);
                    let check = self.check_signature(&full, false)?;
                    if let Some(signer) = check {
                        let signer = signer.unwrap_or_else(|| ident_who(&committer));
                        match mode {
                            SignedMode::StripIfInvalid => {
                                eprintln!("warning: {}", msg::stripping_invalid_commit(&signer));
                                body.truncate(unsigned_len);
                            }
                            SignedMode::SignIfInvalid(key) => {
                                eprintln!("warning: {}", msg::replacing_invalid_commit(&signer));
                                let sig = self.sign(&plain, key, &ident_who(&committer))?;
                                body.truncate(unsigned_len);
                                let header = if self.hash == HashKind::Sha256 {
                                    "gpgsig-sha256"
                                } else {
                                    "gpgsig"
                                };
                                write_header(&mut body, header, sig.strip_suffix(b"\n").unwrap_or(&sig));
                            }
                            _ => return Err(Die::Fatal(msg::ABORT_INVALID_SIGNATURE.into())),
                        }
                    }
                }
            }
        }
        body.push(b'\n');
        body.extend_from_slice(&message);
        let oid = self.store_object(ObjectKind::Commit, &body)?;
        b.tip = Some(oid);
        b.delete = false;
        b.pack_id = Some(self.store.pack_id);
        self.set_mark(mark, oid);
        Ok(())
    }

    /// Verify the signature of the raw commit or tag `full`. `None` when it
    /// is valid; else the signer the signature names, if any.
    fn check_signature(&self, full: &[u8], tag: bool) -> Result<Option<Option<String>>, Die> {
        use crate::signing::verify;
        let parts = if tag {
            verify::tag_signature(full)
        } else {
            verify::commit_signature(self.hash, full)
        };
        let Some((payload, sig)) = parts else {
            return Ok(None);
        };
        let settings = crate::signing::Settings::from_config(&self.repo.config)?;
        let check = verify::check(&settings, &payload, &sig);
        if check.good(crate::signing::Trust::Undefined) {
            return Ok(None);
        }
        Ok(Some(check.signer.clone()))
    }

    /// Sign `payload` with `key` (else the configured key or `who`).
    fn sign(&self, payload: &[u8], key: Option<String>, who: &str) -> Result<Vec<u8>, Die> {
        let settings = crate::signing::Settings::from_config(&self.repo.config)?;
        let sign = match key {
            Some(k) => crate::signing::Sign::Key(k),
            None => crate::signing::Sign::Default,
        };
        let failed = |e: crate::signing::SignError| match e {
            crate::signing::SignError::Die(d) => d,
            crate::signing::SignError::Failed => Die::Fatal(msg::SIGNING_FAILED.into()),
        };
        let key = crate::signing::signing_key(&settings, &sign, who).map_err(failed)?;
        crate::signing::sign_buffer(&settings, payload, &key).map_err(failed)
    }

    /// Check that `path` may be written to the tree.
    fn check_path(&self, path: &[u8], mode: u32) -> Result<(), Die> {
        if path.is_empty() {
            return Ok(());
        }
        let m = rgit_index::Mode(mode);
        if !rgit_unpack::verify_checkout_path(path, m, self.path_opts) {
            return Err(Die::Fatal(msg::invalid_path_quoted(&lossy(path))));
        }
        Ok(())
    }

    fn file_modify(&mut self, b: &mut Branch, line: &[u8]) -> Result<(), Die> {
        let text = lossy(line);
        let rest = &line[2..];
        let sp = rest
            .iter()
            .position(|&c| c == b' ')
            .ok_or_else(|| Die::Fatal(msg::corrupt_mode(&text)))?;
        let digits = &rest[..sp];
        if digits.is_empty() || !digits.iter().all(|c| (b'0'..=b'7').contains(c)) {
            return Err(Die::Fatal(msg::corrupt_mode(&text)));
        }
        let mut raw: u32 = 0;
        for &d in digits {
            raw = raw.saturating_mul(8).saturating_add(u32::from(d - b'0'));
        }
        let mode = match raw {
            0o100644 | 0o644 => 0o100644,
            0o100755 | 0o755 => 0o100755,
            0o120000 => 0o120000,
            0o160000 => MODE_GITLINK,
            0o040000 => MODE_TREE,
            _ => return Err(Die::Fatal(msg::corrupt_mode(&text))),
        };
        let rest = &rest[sp + 1..];
        enum Data {
            Inline,
            Oid(ObjectId, bool),
        }
        let (data, path_part) = if let Some(p) = rest.strip_prefix(b"inline ") {
            (Data::Inline, p)
        } else if let Some(r) = rest.strip_prefix(b":") {
            let (m, after) = mark_number(r);
            let Some(p) = after.strip_prefix(b" ") else {
                return Err(Die::Fatal(msg::missing_space_after_mark(&text)));
            };
            (Data::Oid(self.mark(m)?, true), p)
        } else {
            let hl = self.hash.hex_len();
            match rest.get(..hl).and_then(|h| ObjectId::from_hex(self.hash, h).ok()) {
                Some(oid) => {
                    let Some(p) = rest[hl..].strip_prefix(b" ") else {
                        return Err(Die::Fatal(msg::missing_space_after_sha1(&text)));
                    };
                    (Data::Oid(oid, false), p)
                }
                None => return Err(Die::Fatal(msg::invalid_dataref(&text))),
            }
        };
        let path = parse_path(path_part, line, "path", true)?.0;
        let oid = match data {
            Data::Inline => {
                if mode == MODE_GITLINK {
                    return Err(Die::Fatal(msg::gitlink_inline(&text)));
                }
                if mode == MODE_TREE {
                    return Err(Die::Fatal(msg::dir_inline(&text)));
                }
                self.input.next()?;
                while starts(&self.input.line, "cat-blob ") {
                    self.parse_cat_blob()?;
                    self.input.next()?;
                }
                let data = self.input.read_data()?;
                self.store_object(ObjectKind::Blob, &data)?
            }
            Data::Oid(oid, is_mark) => {
                let want = tree::mode_kind(mode);
                let oid = if mode == MODE_GITLINK && !is_mark {
                    self.rewrite_gitlink(oid)
                } else {
                    oid
                };
                match self.kind_of(&oid) {
                    Some(k) if k != want => {
                        return Err(Die::Fatal(msg::not_a_kind(want.name(), k.name(), &text)));
                    }
                    Some(_) => {}
                    None if mode == MODE_GITLINK => {}
                    None if mode == MODE_TREE => {
                        return Err(Die::Fatal(format!("tree not found: {text}")));
                    }
                    None => return Err(Die::Fatal(msg::blob_not_found(&text))),
                }
                oid
            }
        };
        self.check_path(&path, mode)?;
        let hash = self.hash;
        let mut root = b.tree.take().unwrap_or_default();
        let result = (|| -> Result<(), Die> {
            if path.is_empty() {
                if mode != MODE_TREE {
                    return Err(Die::Fatal(msg::ROOT_NOT_DIR.into()));
                }
                root = Tree::load(hash, &oid, &mut self.objects())?;
                return Ok(());
            }
            let comps: Vec<&[u8]> = path.split(|&c| c == b'/').collect();
            if mode == MODE_TREE && oid == empty_tree(hash) {
                tree::delete(hash, &mut root, &comps, &mut self.objects())?;
                return Ok(());
            }
            tree::set(
                hash,
                &mut root,
                &comps,
                Found {
                    mode,
                    oid,
                    sub: None,
                },
                &mut self.objects(),
            )
        })();
        b.tree = Some(root);
        result
    }

    fn file_delete(&mut self, b: &mut Branch, line: &[u8]) -> Result<(), Die> {
        let path = parse_path(&line[2..], line, "path", true)?.0;
        let hash = self.hash;
        let mut root = b.tree.take().unwrap_or_default();
        let r = if path.is_empty() {
            root = Tree::default();
            Ok(())
        } else {
            let comps: Vec<&[u8]> = path.split(|&c| c == b'/').collect();
            tree::delete(hash, &mut root, &comps, &mut self.objects()).map(|_| ())
        };
        b.tree = Some(root);
        r
    }

    fn file_copy(&mut self, b: &mut Branch, line: &[u8], rename: bool) -> Result<(), Die> {
        let (src, rest) = parse_path(&line[2..], line, "source", false)?;
        let (dest, _) = parse_path(rest, line, "dest", true)?;
        let hash = self.hash;
        let mut root = b.tree.take().unwrap_or_default();
        let result = (|| -> Result<(), Die> {
            let found = if src.is_empty() {
                Some(Found {
                    mode: MODE_TREE,
                    oid: root.oid.unwrap_or_else(|| ObjectId::null(hash)),
                    sub: Some(Box::new(root.clone())),
                })
            } else {
                let comps: Vec<&[u8]> = src.split(|&c| c == b'/').collect();
                if !rename && comps.iter().any(|c| c.is_empty()) {
                    return Err(Die::Fatal(msg::EMPTY_COMPONENT.into()));
                }
                tree::get(hash, &mut root, &comps, &mut self.objects())?
            };
            let Some(found) = found else {
                return Err(Die::Fatal(msg::path_not_in_branch(&lossy(&src))));
            };
            self.check_path(&dest, found.mode)?;
            if rename {
                if src.is_empty() {
                    root = Tree::default();
                } else {
                    let comps: Vec<&[u8]> = src.split(|&c| c == b'/').collect();
                    tree::delete(hash, &mut root, &comps, &mut self.objects())?;
                }
            }
            if dest.is_empty() {
                if found.mode != MODE_TREE {
                    return Err(Die::Fatal(msg::ROOT_NOT_DIR.into()));
                }
                root = match found.sub {
                    Some(sub) => *sub,
                    None => Tree::load(hash, &found.oid, &mut self.objects())?,
                };
                root.oid = None;
                return Ok(());
            }
            let comps: Vec<&[u8]> = dest.split(|&c| c == b'/').collect();
            tree::set(hash, &mut root, &comps, found, &mut self.objects())
        })();
        b.tree = Some(root);
        result
    }

    fn note_modify(
        &mut self,
        b: &mut Branch,
        line: &[u8],
        name: &str,
        fanout: usize,
    ) -> Result<(), Die> {
        let text = lossy(line);
        let rest = &line[2..];
        let (blob, target) = if let Some(t) = rest.strip_prefix(b"inline ") {
            (None, t)
        } else if let Some(r) = rest.strip_prefix(b":") {
            let (m, after) = mark_number(r);
            let Some(t) = after.strip_prefix(b" ") else {
                return Err(Die::Fatal(msg::missing_space_after_mark(&text)));
            };
            (Some(self.mark(m)?), t)
        } else {
            let hl = self.hash.hex_len();
            match rest.get(..hl).and_then(|h| ObjectId::from_hex(self.hash, h).ok()) {
                Some(oid) => {
                    let Some(t) = rest[hl..].strip_prefix(b" ") else {
                        return Err(Die::Fatal(msg::missing_space_after_sha1(&text)));
                    };
                    (Some(oid), t)
                }
                None => return Err(Die::Fatal(msg::invalid_dataref(&text))),
            }
        };
        let target = target.to_vec();
        let annotated = match self.resolve(&target, line, Some(name))? {
            Target::Object(oid) => {
                if target.starts_with(b":") {
                    self.require_commit(oid, &target)?
                } else {
                    oid
                }
            }
            Target::Branch(Some(tip), _) => tip,
            Target::Branch(None, _) | Target::Null => {
                return Err(Die::Fatal(msg::NOTE_EMPTY_BRANCH.into()));
            }
        };
        let blob = match blob {
            None => {
                self.input.next()?;
                let data = self.input.read_data()?;
                Some(self.store_object(ObjectKind::Blob, &data)?)
            }
            Some(oid) if oid.is_null() => None,
            Some(oid) => {
                match self.kind_of(&oid) {
                    Some(ObjectKind::Blob) => {}
                    Some(k) => {
                        return Err(Die::Fatal(msg::not_a_kind("blob", k.name(), &text)));
                    }
                    None => return Err(Die::Fatal(msg::blob_not_found(&text))),
                }
                Some(oid)
            }
        };
        let hash = self.hash;
        let hex = annotated.to_hex();
        let mut root = b.tree.take().unwrap_or_default();
        let r = (|| -> Result<(), Die> {
            notes::remove(hash, &mut root, &hex, fanout, &mut self.objects())?;
            if let Some(blob) = blob {
                notes::add(hash, &mut root, &hex, blob, fanout, &mut self.objects())?;
            }
            Ok(())
        })();
        b.tree = Some(root);
        r
    }

    fn rewrite_gitlink(&self, oid: ObjectId) -> ObjectId {
        for (name, from) in &self.rewrite_from {
            let Some(to) = self.rewrite_to.get(name) else {
                continue;
            };
            if let Some((m, _)) = from.iter().find(|(_, o)| **o == oid) {
                if let Some(new) = to.get(m) {
                    return *new;
                }
            }
        }
        oid
    }

    fn parse_tag(&mut self) -> Result<(), Die> {
        let name = lossy(&self.input.line["tag ".len()..]);
        self.input.next()?;
        let mark = self.parse_mark()?;
        let Some(spec) = self.input.line.strip_prefix(b"from ") else {
            return Err(Die::Fatal(msg::expected_from(&self.input.text())));
        };
        let spec = spec.to_vec();
        let line = self.input.line.clone();
        let oid = match self.resolve(&spec, &line, None)? {
            Target::Object(oid) => oid,
            Target::Branch(Some(tip), _) => tip,
            Target::Branch(None, _) | Target::Null => {
                return Err(Die::Fatal(msg::TAG_EMPTY_BRANCH.into()));
            }
        };
        let kind = self
            .kind_of(&oid)
            .ok_or_else(|| Die::Fatal(msg::not_a_valid_commit(&oid.to_hex())))?;
        self.input.next()?;
        self.skip_original_oid()?;
        let mut tagger = None;
        if let Some(t) = self.input.line.strip_prefix(b"tagger ") {
            tagger = Some(ident::parse_ident(t, self.date_format).map_err(Die::Fatal)?);
            self.input.next()?;
        }
        let mut message = self.input.read_data()?;
        let mut body = Vec::new();
        body.extend_from_slice(format!("object {oid}\ntype {}\ntag {name}\n", kind.name()).as_bytes());
        if let Some(t) = &tagger {
            body.extend_from_slice(b"tagger ");
            body.extend_from_slice(t);
            body.push(b'\n');
        }
        body.push(b'\n');
        let (_, sig) = rgit_object::split_signature(&message);
        if sig.is_some() {
            match self.signed_tags.clone() {
                SignedMode::WarnVerbatim => {
                    eprintln!("warning: {}", msg::importing_signed_tag(&name));
                }
                SignedMode::Strip => {
                    let keep = rgit_object::split_signature(&message).0.len();
                    message.truncate(keep);
                }
                SignedMode::WarnStrip => {
                    eprintln!("warning: {}", msg::stripping_signed_tag(&name));
                    let keep = rgit_object::split_signature(&message).0.len();
                    message.truncate(keep);
                }
                SignedMode::Abort => {
                    return Err(Die::Fatal(msg::signed_tag_abort(&name)));
                }
                SignedMode::Verbatim => {}
                mode => {
                    let mut full = body.clone();
                    full.extend_from_slice(&message);
                    if self.check_signature(&full, true)?.is_some() {
                        let keep = rgit_object::split_signature(&message).0.len();
                        message.truncate(keep);
                        match mode {
                            SignedMode::StripIfInvalid => {}
                            SignedMode::SignIfInvalid(key) => {
                                let mut payload = body.clone();
                                payload.extend_from_slice(&message);
                                let who = tagger.as_deref().map(ident_who).unwrap_or_default();
                                let sig = self.sign(&payload, key, &who)?;
                                message.extend_from_slice(&sig);
                            }
                            _ => return Err(Die::Fatal(msg::ABORT_INVALID_SIGNATURE.into())),
                        }
                    }
                }
            }
        }
        body.extend_from_slice(&message);
        let tag_oid = self.store_object(ObjectKind::Tag, &body)?;
        self.set_mark(mark, tag_oid);
        self.tags.retain(|(n, _)| *n != name);
        self.tags.push((name, tag_oid));
        Ok(())
    }

    fn parse_reset(&mut self) -> Result<(), Die> {
        let name = lossy(&self.input.line["reset ".len()..]);
        Self::check_branch_name(&name)?;
        let mut b = self.take_branch(&name);
        b.tip = None;
        b.tree = Some(Tree::default());
        b.delete = false;
        if let Some(tag) = name.strip_prefix("refs/tags/") {
            self.tags.retain(|(n, _)| n != tag);
        }
        let r = (|| -> Result<(), Die> {
            if !self.input.next()? {
                return Ok(());
            }
            if let Some(spec) = self.input.line.strip_prefix(b"from ") {
                let spec = spec.to_vec();
                let line = self.input.line.clone();
                match self.resolve(&spec, &line, Some(&name))? {
                    Target::Null => b.delete = true,
                    t => self.apply_from(&mut b, t)?,
                }
                if !self.input.next()? {
                    return Ok(());
                }
            }
            if !self.input.line.is_empty() {
                self.input.unread();
            }
            Ok(())
        })();
        self.branches.insert(name, b);
        r
    }

    fn parse_alias(&mut self) -> Result<(), Die> {
        self.input.next()?;
        let Some(m) = self.parse_mark()? else {
            return Err(Die::Fatal(msg::expected_mark(&self.input.text())));
        };
        let Some(spec) = self.input.line.strip_prefix(b"to ") else {
            return Err(Die::Fatal(msg::expected_to(&self.input.text())));
        };
        let spec = spec.to_vec();
        let line = self.input.line.clone();
        let oid = match self.resolve(&spec, &line, None)? {
            Target::Object(oid) => oid,
            Target::Branch(Some(tip), _) => tip,
            _ => return Err(Die::Fatal(msg::invalid_ref_or_sha1(&lossy(&spec)))),
        };
        self.set_mark(Some(m), oid);
        self.input.skip_optional_lf()?;
        Ok(())
    }

    fn respond(&mut self, data: &[u8]) {
        let _ = self.out.write_all(data);
        let _ = self.out.flush();
    }

    fn parse_get_mark(&mut self) -> Result<(), Die> {
        let line = self.input.line.clone();
        let arg = &line["get-mark ".len()..];
        let Some(r) = arg.strip_prefix(b":") else {
            return Err(Die::Fatal(msg::not_a_mark(&lossy(arg))));
        };
        let (m, rest) = mark_number(r);
        if !rest.is_empty() {
            return Err(Die::Fatal(msg::garbage_after_mark(&lossy(&line))));
        }
        let oid = self.mark(m)?;
        self.respond(format!("{oid}\n").as_bytes());
        Ok(())
    }

    fn parse_cat_blob(&mut self) -> Result<(), Die> {
        let line = self.input.line.clone();
        let arg = &line["cat-blob ".len()..];
        let oid = if let Some(r) = arg.strip_prefix(b":") {
            let (m, rest) = mark_number(r);
            if !rest.is_empty() {
                return Err(Die::Fatal(msg::garbage_after_mark(&lossy(&line))));
            }
            self.mark(m)?
        } else {
            let hl = self.hash.hex_len();
            match arg.get(..hl).and_then(|h| ObjectId::from_hex(self.hash, h).ok()) {
                Some(oid) if arg.len() == hl => oid,
                Some(_) => return Err(Die::Fatal(format!("garbage after SHA1: {}", lossy(&line)))),
                None => return Err(Die::Fatal(msg::invalid_dataref(&lossy(&line)))),
            }
        };
        match self.read_object(&oid)? {
            None => self.respond(format!("{oid} missing\n").as_bytes()),
            Some((ObjectKind::Blob, data)) => {
                let mut out = format!("{oid} blob {}\n", data.len()).into_bytes();
                out.extend_from_slice(&data);
                out.push(b'\n');
                self.respond(&out);
            }
            Some((kind, _)) => {
                return Err(Die::Fatal(msg::cat_blob_not_blob(&oid.to_hex(), kind.name())));
            }
        }
        Ok(())
    }

    /// `ls`, at the top level (`b` is `None`) or inside a commit.
    fn parse_ls(&mut self, b: Option<&mut Branch>) -> Result<(), Die> {
        let line = self.input.line.clone();
        let text = lossy(&line);
        let rest = &line[3..];
        let hash = self.hash;
        let (mut root, path) = if rest.first() == Some(&b'"') {
            let Some(b) = b else {
                return Err(Die::Fatal(msg::not_in_commit(&text)));
            };
            let path = parse_path(rest, &line, "path", true)?.0;
            (None, (path, Some(b)))
        } else {
            let (oid, p) = if let Some(r) = rest.strip_prefix(b":") {
                let (m, after) = mark_number(r);
                let Some(p) = after.strip_prefix(b" ") else {
                    return Err(Die::Fatal(msg::missing_space_after_mark(&text)));
                };
                (self.mark(m)?, p)
            } else {
                let hl = hash.hex_len();
                match rest.get(..hl).and_then(|h| ObjectId::from_hex(hash, h).ok()) {
                    Some(oid) => {
                        let Some(p) = rest[hl..].strip_prefix(b" ") else {
                            return Err(Die::Fatal(msg::missing_space_after_tree_ish(&text)));
                        };
                        (oid, p)
                    }
                    None => return Err(Die::Fatal(msg::invalid_dataref(&text))),
                }
            };
            let path = parse_path(p, &line, "path", true)?.0;
            let tree_oid = match self.peel(oid)? {
                None => return Err(Die::Fatal(msg::object_not_found(&oid.to_hex()))),
                Some((ObjectKind::Commit, _, data)) => rgit_object::Commit::parse(hash, &data)
                    .ok()
                    .and_then(|c| c.tree().ok())
                    .ok_or_else(|| Die::Fatal(msg::object_not_found(&oid.to_hex())))?,
                Some((ObjectKind::Tree, id, _)) => id,
                Some(_) => {
                    return Err(Die::Fatal(msg::object_not_found(&oid.to_hex())));
                }
            };
            let t = Tree::load(hash, &tree_oid, &mut self.objects())?;
            (Some(t), (path, None))
        };
        let (path, branch) = path;
        if let Some(b) = branch {
            self.branch_tree(b)?;
            root = b.tree.clone();
        }
        let tree: &mut Tree = root.as_mut().expect("tree");
        let found = if path.is_empty() {
            Some(Found {
                mode: MODE_TREE,
                oid: tree.oid.unwrap_or_else(|| ObjectId::null(hash)),
                sub: Some(Box::new(tree.clone())),
            })
        } else {
            let comps: Vec<&[u8]> = path.split(|&c| c == b'/').collect();
            if comps.iter().any(|c| c.is_empty()) {
                return Err(Die::Fatal(msg::EMPTY_COMPONENT.into()));
            }
            let mut objs = Objects {
                store: &mut self.store,
                repo: &self.repo,
                closed: &mut self.closed,
            };
            tree::get(hash, tree, &comps, &mut objs)?
        };
        let mut out = Vec::new();
        match found {
            None => {
                out.extend_from_slice(b"missing ");
                crate::quote::write_quoted(&mut out, &path, true);
            }
            Some(mut f) => {
                if let Some(sub) = &mut f.sub {
                    let mut objs = Objects {
                        store: &mut self.store,
                        repo: &self.repo,
                        closed: &mut self.closed,
                    };
                    f.oid = tree::write(sub, &mut objs)?;
                }
                out.extend_from_slice(
                    format!("{:06o} {} {}\t", f.mode, tree::mode_kind(f.mode).name(), f.oid)
                        .as_bytes(),
                );
                crate::quote::write_quoted(&mut out, &path, true);
            }
        }
        out.push(b'\n');
        self.respond(&out);
        Ok(())
    }

    // ---------------------------------------------------------------
    // Checkpoints, end and crash

    fn checkpoint(&mut self) -> Result<(), Die> {
        self.close_pack()?;
        self.update_refs()?;
        self.export_marks()?;
        Ok(())
    }

    fn finish(&mut self) -> Result<(), Die> {
        self.close_pack()?;
        self.update_refs()?;
        self.export_marks()?;
        if !self.quiet {
            self.print_stats();
        }
        Ok(())
    }

    fn update_refs(&mut self) -> Result<(), Die> {
        let setup = self.repo.setup.clone();
        let config = &self.repo.config;
        let objects = crate::refs_support::Objects::open(&setup);
        let ref_store = crate::refs_support::ref_store(&setup, config);
        let mut hooks = crate::refs_support::UpdateHooks::new(&setup, config, &objects);
        let revs = crate::refs_support::Revs::new(self.ctx, &setup)?;
        let hash = self.hash;
        let refs = crate::refs::Refs::new(&setup);
        for name in self.branch_order.clone() {
            let Some(b) = self.branches.get(&name) else {
                continue;
            };
            let old = refs.resolve(&name);
            let update = if b.delete {
                if old.is_none() {
                    continue;
                }
                RefUpdate::delete(name.as_str(), hash, None)
            } else {
                let Some(new) = b.tip else {
                    continue;
                };
                if let Some(id) = name.strip_prefix("refs/replace/") {
                    if id == new.to_hex() {
                        if old.is_some() {
                            eprintln!("warning: {}", msg::dropping_replace(&name));
                            let mut tx = Transaction::new(&ref_store);
                            let _ = tx
                                .push(RefUpdate::delete(name.as_str(), hash, None).message("fast-import"))
                                .and_then(|()| tx.commit(&mut hooks));
                        }
                        continue;
                    }
                }
                if let Some(old) = old {
                    if old == new {
                        continue;
                    }
                    let peeled = |o: &ObjectId| objects.peel(o).filter(|p| objects.kind(p) == Some(ObjectKind::Commit));
                    let contained = match (peeled(&old), peeled(&new)) {
                        (Some(o), Some(n)) => revs.is_ancestor(&o, &n),
                        _ => false,
                    };
                    if !self.force && !contained {
                        eprintln!(
                            "warning: {}",
                            msg::not_updating(&name, &new.to_hex(), &old.to_hex())
                        );
                        self.failed = true;
                        continue;
                    }
                }
                RefUpdate::update(name.as_str(), new, None)
            };
            let mut tx = Transaction::new(&ref_store);
            if let Err(e) = tx
                .push(update.message("fast-import"))
                .and_then(|()| tx.commit(&mut hooks))
            {
                eprintln!("error: {e}");
                self.failed = true;
            }
        }
        for (name, oid) in self.tags.clone() {
            let full = format!("refs/tags/{name}");
            let mut tx = Transaction::new(&ref_store);
            if let Err(e) = tx
                .push(RefUpdate::update(full.as_str(), oid, None).message("fast-import"))
                .and_then(|()| tx.commit(&mut hooks))
            {
                eprintln!("error: {e}");
                self.failed = true;
            }
        }
        Ok(())
    }

    fn print_stats(&self) {
        let c = &self.store.counts;
        let kinds: [(u64, u64); 4] = std::array::from_fn(|i| (c[i].stored, c[i].duplicates));
        eprint!(
            "{}",
            msg::statistics(&kinds, self.branches.len(), self.marks.len())
        );
    }

    /// After a fatal error: write the crash report, keep the objects and
    /// the marks; refs are not updated.
    fn crash(&mut self, message: &str) {
        let pid = std::process::id();
        let path = self.repo.setup.git_dir.join(format!("fast_import_crash_{pid}"));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let history: Vec<String> = self.input.history.iter().map(|l| lossy(l)).collect();
        let branches: Vec<(String, String)> = self
            .branch_order
            .iter()
            .filter_map(|name| {
                let b = self.branches.get(name)?;
                let tip = b.tip.unwrap_or_else(|| ObjectId::null(self.hash));
                Some((name.clone(), tip.to_hex()))
            })
            .collect();
        let r = msg::crash_report_text(&msg::Crash {
            pid,
            parent: std::os::unix::process::parent_id(),
            at: rgit_date::format_raw(now, 0),
            message,
            history: &history,
            branches: &branches,
            exported: self.export_marks.as_ref().map(|p| p.display().to_string()),
        });
        if std::fs::write(&path, r).is_ok() {
            eprintln!("{}", msg::crash_report(&path.display().to_string()));
        }
        if self.store.close(&self.repo).is_err() {
            self.store.abandon();
        }
        if !self.marks_failed {
            let _ = self.export_marks();
        }
    }
}

/// `Name <email>` of an identity line.
fn ident_who(ident: &[u8]) -> String {
    let end = ident
        .iter()
        .rposition(|&c| c == b'>')
        .map_or(ident.len(), |p| p + 1);
    lossy(ident[..end].trim_ascii_start())
}

/// Write a header whose value may span lines (continuation lines start
/// with a space).
fn write_header(out: &mut Vec<u8>, name: &str, value: &[u8]) {
    out.extend_from_slice(name.as_bytes());
    for l in value.split(|&c| c == b'\n') {
        out.push(b' ');
        out.extend_from_slice(l);
        out.push(b'\n');
    }
}

fn empty_tree(hash: HashKind) -> ObjectId {
    rgit_hash::hash_object(hash, "tree", b"").expect("empty tree")
}

/// `path` for messages and the pack edges file: relative to the current
/// directory when below it.
fn display_path(path: &Path) -> String {
    if let Ok(cwd) = std::env::current_dir() {
        if let Ok(rel) = path.strip_prefix(&cwd) {
            return rel.display().to_string();
        }
    }
    path.display().to_string()
}

/// `:<mark> <oid>`.
fn parse_mark_line(line: &[u8], hash: HashKind) -> Option<(u64, ObjectId)> {
    let r = line.strip_prefix(b":")?;
    let (m, rest) = mark_number(r);
    let hex = rest.strip_prefix(b" ")?;
    let oid = ObjectId::from_hex(hash, hex).ok()?;
    Some((m, oid))
}

fn read_marks_file(
    path: &Path,
    shown: &str,
    hash: HashKind,
) -> Result<HashMap<u64, ObjectId>, Die> {
    let text = std::fs::read(path).map_err(|e| {
        Die::Fatal(msg::cannot_read(
            shown,
            &crate::global::strerror(&e),
        ))
    })?;
    let mut map = HashMap::new();
    for line in text.split(|&c| c == b'\n') {
        if line.is_empty() {
            continue;
        }
        let (m, oid) = parse_mark_line(line, hash)
            .or_else(|| {
                // Marks of another hash algorithm (submodule rewriting).
                let r = line.strip_prefix(b":")?;
                let (m, rest) = mark_number(r);
                let hex = rest.strip_prefix(b" ")?;
                let other = if hash == HashKind::Sha1 {
                    HashKind::Sha256
                } else {
                    HashKind::Sha1
                };
                Some((m, ObjectId::from_hex(other, hex).ok()?))
            })
            .ok_or_else(|| Die::Fatal(msg::corrupt_mark_line(&lossy(line))))?;
        map.insert(m, oid);
    }
    Ok(map)
}

/// Parse a `<path>` at the start of `s`: quoted (C style) or literal. The
/// last field of a line takes the rest of the line; other fields end at
/// a space. Returns the path and what follows it (after the space).
fn parse_path<'a>(
    s: &'a [u8],
    line: &[u8],
    field: &str,
    last: bool,
) -> Result<(Vec<u8>, &'a [u8]), Die> {
    let text = || lossy(line);
    if s.first() == Some(&b'"') {
        let (path, rest) = crate::quote::c_unquote_prefix(s)
            .ok_or_else(|| Die::Fatal(msg::invalid_field(field, &text())))?;
        if path.contains(&0) {
            return Err(Die::Fatal(msg::nul_in_field(field, &text())));
        }
        if last {
            if !rest.is_empty() {
                return Err(Die::Fatal(msg::garbage_after_field(field, &text())));
            }
            return Ok((path, rest));
        }
        let Some(rest) = rest.strip_prefix(b" ") else {
            return Err(Die::Fatal(msg::missing_space_after_field(field, &text())));
        };
        return Ok((path, rest));
    }
    if last {
        return Ok((s.to_vec(), &s[s.len()..]));
    }
    let Some(sp) = s.iter().position(|&c| c == b' ') else {
        return Err(Die::Fatal(msg::missing_space_after_field(field, &text())));
    };
    Ok((s[..sp].to_vec(), &s[sp + 1..]))
}
