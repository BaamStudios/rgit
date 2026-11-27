// SPDX-License-Identifier: MIT
//! Walking the archived tree and writing the entries (`git archive`).
//!
//! Behaviour determined with the reference `git`:
//!
//! - entries come in tree order; a directory entry is written just before
//!   the first file (or submodule) below it that the pathspec selects,
//!   whether or not that file is then dropped by `export-ignore`, so empty
//!   trees and trees with only ignored subdirectories leave no trace while
//!   directories whose files are all ignored do;
//! - the `--prefix` directory itself is written first when the prefix
//!   ends with `/`;
//! - submodules become empty directories;
//! - `--add-file` and `--add-virtual-file` entries follow the tree, in
//!   command line order;
//! - run in a subdirectory, only that subdirectory is archived, with paths
//!   relative to it.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

use rgit_archive::gzip::GzipWriter;
use rgit_archive::tar::{self, TarWriter};
use rgit_archive::zip::{self, ZipWriter};
use rgit_attr::{AttrId, Attributes, State};
use rgit_hash::ObjectId;
use rgit_object::{EntryMode, ObjectKind, TreeIter};
use rgit_pathspec::{DirInterest, Pathspec};

use crate::convert::Converter;
use crate::messages::archive as msg;
use crate::prelude::*;
use crate::repository::Repo;

/// The internal gzip's command name (`tar.<format>.command`).
pub const INTERNAL_GZIP: &str = "git archive gzip";

/// How the archive is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    Tar,
    Zip,
    /// A tar piped through a command (`tar.<format>.command`).
    Filter(String),
}

/// A file added besides the tree.
#[derive(Debug, Clone)]
pub enum Extra {
    /// `--add-file`: the path as given, where it is and the prefix in
    /// effect.
    File {
        prefix: Vec<u8>,
        path: String,
        on_disk: String,
    },
    /// `--add-virtual-file`.
    Virtual { path: Vec<u8>, content: Vec<u8> },
}

/// Destination of the archive bytes.
pub enum Out {
    Plain(Box<dyn Write>),
    Gzip(GzipWriter<Box<dyn Write>>),
    Filter {
        child: Child,
        stdin: Option<ChildStdin>,
        command: String,
    },
}

impl Write for Out {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Out::Plain(w) => w.write(buf),
            Out::Gzip(g) => g.write(buf),
            Out::Filter { stdin, .. } => match stdin {
                Some(s) => s.write(buf),
                None => Err(io::Error::from(io::ErrorKind::BrokenPipe)),
            },
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Out::Plain(w) => w.flush(),
            Out::Gzip(g) => g.flush(),
            Out::Filter { stdin, .. } => stdin.as_mut().map_or(Ok(()), |s| s.flush()),
        }
    }
}

impl Out {
    /// Wrap `dest` for `backend` at compression `level`.
    pub fn new(dest: Dest, backend: &Backend, level: Option<&str>) -> Result<Out, Die> {
        let Backend::Filter(command) = backend else {
            return Ok(Out::Plain(dest.into_writer()));
        };
        if command == INTERNAL_GZIP {
            let level = match level {
                None => 6,
                Some(l) => match l.parse::<u32>() {
                    Ok(n) if n <= rgit_archive::gzip::MAX_LEVEL => n,
                    _ => return Err(Die::Fatal(msg::deflate_level(l))),
                },
            };
            let g = GzipWriter::new(dest.into_writer(), level).map_err(write_error)?;
            return Ok(Out::Gzip(g));
        }
        if command.is_empty() {
            return Err(Die::Fatal(msg::unable_to_start_filter(
                command,
                "No such file or directory",
            )));
        }
        let mut full = command.clone();
        if let Some(l) = level {
            full.push_str(" -");
            full.push_str(l);
        }
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&full).stdin(Stdio::piped());
        // The filter writes where the archive would go.
        cmd.stdout(match dest {
            Dest::File(f) => Stdio::from(f),
            Dest::Stdout => Stdio::inherit(),
        });
        let mut child = cmd.spawn().map_err(|e| {
            Die::Fatal(msg::unable_to_start_filter(
                command,
                &crate::global::strerror(&e),
            ))
        })?;
        let stdin = child.stdin.take();
        Ok(Out::Filter {
            child,
            stdin,
            command: command.clone(),
        })
    }

    /// Flush and close: the gzip trailer, the end of the filter.
    pub fn finish(self) -> Result<(), Die> {
        match self {
            Out::Plain(mut w) => w.flush().map_err(write_error),
            Out::Gzip(g) => g.finish().map(drop).map_err(write_error),
            Out::Filter {
                mut child,
                stdin,
                command,
            } => {
                drop(stdin);
                let status = child.wait().map_err(|e| Die::Fatal(e.to_string()))?;
                if status.success() {
                    Ok(())
                } else {
                    Err(Die::Fatal(msg::filter_failed(&command)))
                }
            }
        }
    }
}

/// Where the archive goes: the output file or standard output.
pub enum Dest {
    Stdout,
    File(std::fs::File),
}

impl Dest {
    /// Open `-o <file>`, or standard output.
    pub fn open(output: Option<&str>) -> Result<Dest, Die> {
        match output {
            None => Ok(Dest::Stdout),
            Some(path) => std::fs::File::create(path).map(Dest::File).map_err(|e| {
                Die::Fatal(msg::could_not_create(path, &crate::global::strerror(&e)))
            }),
        }
    }

    /// The destination as a buffered writer.
    pub fn into_writer(self) -> Box<dyn Write> {
        match self {
            // Not through std's line-buffered `Stdout`, which would split a
            // record at its last newline: `upload-archive` relays what
            // it reads from this output packet by packet, and git's
            // archiver writes a record at once.
            Dest::Stdout => Box::new(io::BufWriter::with_capacity(
                1 << 16,
                crate::commands::receive_pack::RawStdout,
            )),
            Dest::File(f) => Box::new(io::BufWriter::with_capacity(1 << 16, f)),
        }
    }
}

/// A failed write of the archive.
pub fn write_error(e: io::Error) -> Die {
    Die::Fatal(crate::global::strerror(&e))
}

/// What one entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Dir,
    File { executable: bool },
    Symlink,
}

/// The format writer behind the walk.
enum Writer {
    Tar(TarWriter<Out>),
    Zip(ZipWriter<Out>),
}

enum Body<'a> {
    Mem(&'a [u8]),
    Stream(&'a mut dyn Read, u64),
}

impl Writer {
    fn wants_text(&self) -> bool {
        matches!(self, Writer::Zip(_))
    }

    fn write(
        &mut self,
        path: &[u8],
        kind: EntryKind,
        oid: &str,
        body: Body<'_>,
        text: bool,
    ) -> io::Result<()> {
        match self {
            Writer::Tar(t) => {
                let k = match kind {
                    EntryKind::Dir => tar::Kind::Directory,
                    EntryKind::File { executable } => tar::Kind::File { executable },
                    EntryKind::Symlink => tar::Kind::Symlink,
                };
                match body {
                    Body::Mem(data) => t.add(path, k, oid, data),
                    Body::Stream(r, size) => {
                        t.entry(path, k, oid, size, &[])?;
                        t.copy(r, size)?;
                        t.end_entry()
                    }
                }
            }
            Writer::Zip(z) => {
                let k = match kind {
                    EntryKind::Dir => zip::Kind::Directory,
                    EntryKind::File { executable } => zip::Kind::File { executable },
                    EntryKind::Symlink => zip::Kind::Symlink,
                };
                let utf8 = match zip::needs_utf8_flag(path) {
                    Some(u) => u,
                    None => {
                        eprintln!(
                            "warning: {}",
                            msg::not_valid_utf8(&String::from_utf8_lossy(path))
                        );
                        false
                    }
                };
                match body {
                    Body::Mem(data) => z.add(path, k, data, utf8, text),
                    Body::Stream(r, size) => z.add_stream(path, k, size, r, utf8, text),
                }
            }
        }
    }

    fn finish(self, commit: Option<&str>) -> io::Result<Out> {
        match self {
            Writer::Tar(t) => t.finish(),
            Writer::Zip(z) => z.finish(commit.unwrap_or("").as_bytes()),
        }
    }
}

/// Everything that decides the archive.
pub struct Job<'a> {
    pub repo: &'a Repo,
    pub ctx: &'a Context,
    /// The tree archived and the commit it came from.
    pub tree: ObjectId,
    pub commit: Option<ObjectId>,
    pub mtime: u64,
    /// The archived directory relative to the top (`""` or ending in `/`).
    pub base: Vec<u8>,
    pub prefix: Vec<u8>,
    pub pathspec: Pathspec,
    pub backend: Backend,
    pub level: Option<String>,
    pub verbose: bool,
    pub worktree_attributes: bool,
    pub extras: Vec<Extra>,
    pub umask: u32,
    pub big_file_threshold: u64,
    pub output: Option<String>,
}

/// The tree below `base` in `tree`, if it exists.
fn subtree(repo: &Repo, tree: &ObjectId, base: &[u8]) -> Option<ObjectId> {
    let mut cur = *tree;
    for comp in base.split(|&b| b == b'/').filter(|c| !c.is_empty()) {
        let data = repo.read_tree_data(&cur)?;
        let mut next = None;
        for e in TreeIter::new(repo.hash, &data) {
            let e = e.ok()?;
            if e.name == comp && e.is_tree() {
                next = Some(e.oid);
                break;
            }
        }
        cur = next?;
    }
    Some(cur)
}

/// `.gitattributes` blobs of `tree` by path, for the converter.
fn attribute_files(repo: &Repo, tree: &ObjectId, dir: &[u8], out: &mut HashMap<Vec<u8>, Vec<u8>>) {
    let Some(data) = repo.read_tree_data(tree) else {
        return;
    };
    for e in TreeIter::new(repo.hash, &data) {
        let Ok(e) = e else { return };
        let mut path = dir.to_vec();
        path.extend_from_slice(e.name);
        if e.is_tree() {
            path.push(b'/');
            attribute_files(repo, &e.oid, &path, out);
        } else if e.name == b".gitattributes" && e.mode_bits & 0o170000 == 0o100000 {
            if let Ok(Some(raw)) = repo.odb().read(&e.oid) {
                out.insert(path, raw.data);
            }
        }
    }
}

/// Whether `data` looks like text (no NUL in the first 8000 bytes).
fn looks_text(data: &[u8]) -> bool {
    !data[..data.len().min(8000)].contains(&0)
}

struct Walk<'a, 'r> {
    job: &'a Job<'r>,
    attrs: Attributes<'r>,
    ignore: AttrId,
    subst: AttrId,
    diff: AttrId,
    converter: Converter,
    writer: Writer,
    /// Directories entered: archive path, tree id and whether written.
    pending: Vec<(Vec<u8>, String, bool)>,
    describe_used: bool,
    commit_hex: Option<String>,
}

impl Walk<'_, '_> {
    fn emit(&mut self, path: &[u8], kind: EntryKind, oid: &str, body: Body<'_>, text: bool) -> Result<(), Die> {
        if self.job.verbose {
            let mut line = path.to_vec();
            line.push(b'\n');
            let _ = io::stderr().write_all(&line);
        }
        self.writer
            .write(path, kind, oid, body, text)
            .map_err(write_error)
    }

    fn flush_pending(&mut self) -> Result<(), Die> {
        for i in 0..self.pending.len() {
            if !self.pending[i].2 {
                self.pending[i].2 = true;
                let (path, oid) = (self.pending[i].0.clone(), self.pending[i].1.clone());
                self.emit(&path, EntryKind::Dir, &oid, Body::Mem(&[]), false)?;
            }
        }
        Ok(())
    }

    fn is_text(&mut self, repo_path: &[u8], data: &[u8]) -> bool {
        let out = self.attrs.lookup(repo_path, false);
        match out.get(self.diff) {
            State::Unset => false,
            State::Set => true,
            State::Value(driver) => {
                let key = format!("diff.{}.binary", String::from_utf8_lossy(driver));
                match self.job.repo.config.bool(&key) {
                    Ok(Some(true)) => false,
                    _ => looks_text(data),
                }
            }
            _ => looks_text(data),
        }
    }

    fn walk(&mut self, tree: &ObjectId, repo_dir: &[u8], archive_dir: &[u8]) -> Result<(), Die> {
        let repo = self.job.repo;
        let data = repo
            .read_tree_data(tree)
            .ok_or_else(|| Die::Fatal(msg::could_not_read(&tree.to_hex())))?;
        for e in TreeIter::new(repo.hash, &data) {
            let e = e.map_err(|_| Die::Fatal(msg::could_not_read(&tree.to_hex())))?;
            let mut full = repo_dir.to_vec();
            full.extend_from_slice(e.name);
            let mut apath = archive_dir.to_vec();
            apath.extend_from_slice(e.name);
            let is_tree = e.is_tree();
            let selected = {
                let attrs = &mut self.attrs;
                self.job
                    .pathspec
                    .match_path_with(&full, None, &mut |p, spec| attrs.satisfies(p, false, spec))
                    .is_some()
            };
            if is_tree {
                // Directories are written lazily, before the first file
                // below them; an ignored one is skipped with everything
                // in it.
                if !selected && self.job.pathspec.dir_interest(&full) == DirInterest::None {
                    continue;
                }
                if self.attrs.lookup(&full, true).get(self.ignore).is_set() {
                    continue;
                }
                apath.push(b'/');
                full.push(b'/');
                self.pending.push((apath.clone(), e.oid.to_hex(), false));
                self.walk(&e.oid, &full, &apath)?;
                self.pending.pop();
                continue;
            }
            if !selected {
                continue;
            }
            self.flush_pending()?;
            let out = self.attrs.lookup(&full, false);
            if out.get(self.ignore).is_set() {
                continue;
            }
            let subst = out.get(self.subst).is_set();
            let mode = EntryMode::from_bits(e.mode_bits).canonical();
            if mode == EntryMode::COMMIT {
                apath.push(b'/');
                self.emit(&apath, EntryKind::Dir, &e.oid.to_hex(), Body::Mem(&[]), false)?;
                continue;
            }
            let kind = if mode == EntryMode::LINK {
                EntryKind::Symlink
            } else {
                EntryKind::File {
                    executable: mode == EntryMode::BLOB_EXECUTABLE,
                }
            };
            self.blob(&full, &apath, kind, &e.oid, subst)?;
        }
        Ok(())
    }

    fn blob(
        &mut self,
        full: &[u8],
        apath: &[u8],
        kind: EntryKind,
        oid: &ObjectId,
        subst: bool,
    ) -> Result<(), Die> {
        let repo = self.job.repo;
        let hex = oid.to_hex();
        let bad = || Die::Fatal(msg::could_not_read(&hex));
        let converts = kind != EntryKind::Symlink
            && (self.converter.converts(full) || (subst && self.commit_hex.is_some()));
        if !converts && kind != EntryKind::Symlink {
            let size = repo
                .odb()
                .header(oid)
                .ok()
                .flatten()
                .map(|h| h.size)
                .ok_or_else(bad)?;
            if size > self.job.big_file_threshold {
                // Loose objects and whole pack entries are inflated while
                // being written (deltas are resolved in memory).
                let mut reader = repo.odb().stream(oid).ok().flatten().ok_or_else(bad)?;
                if reader.kind() != ObjectKind::Blob {
                    return Err(bad());
                }
                let mut head = vec![0u8; size.min(8000) as usize];
                reader.read_exact(&mut head).map_err(|_| bad())?;
                let text = self.writer.wants_text() && self.is_text(full, &head);
                let mut chained = (&head[..]).chain(reader);
                return self.emit(apath, kind, &hex, Body::Stream(&mut chained, size), text);
            }
        }
        let raw = repo.odb().read(oid).ok().flatten().ok_or_else(bad)?;
        if raw.kind != ObjectKind::Blob {
            return Err(bad());
        }
        let mut data = raw.data;
        if kind != EntryKind::Symlink {
            if subst {
                if let Some(c) = self.commit_hex.clone() {
                    data = self.substitute(&c, data)?;
                }
            }
            data = self.converter.to_worktree(full, oid, data);
        }
        let text = self.writer.wants_text() && self.is_text(full, &data);
        self.emit(apath, kind, &hex, Body::Mem(&data), text)
    }

    /// Expand `$Format:...$` (export-subst); only the first `%(describe)`
    /// of the archive is expanded.
    fn substitute(&mut self, commit: &str, data: Vec<u8>) -> Result<Vec<u8>, Die> {
        const START: &[u8] = b"$Format:";
        let mut out = Vec::with_capacity(data.len());
        let mut rest = &data[..];
        loop {
            let Some(i) = rest.windows(START.len()).position(|w| w == START) else {
                out.extend_from_slice(rest);
                break;
            };
            let after = &rest[i + START.len()..];
            let Some(end) = after.iter().position(|&b| b == b'$') else {
                out.extend_from_slice(rest);
                break;
            };
            out.extend_from_slice(&rest[..i]);
            let fmt = &after[..end];
            out.extend_from_slice(&self.expand(commit, fmt)?);
            rest = &after[end + 1..];
        }
        Ok(out)
    }

    fn expand(&mut self, commit: &str, fmt: &[u8]) -> Result<Vec<u8>, Die> {
        const DESCRIBE: &[u8] = b"%(describe";
        let mut f = Vec::with_capacity(fmt.len());
        let mut i = 0;
        while i < fmt.len() {
            if fmt[i..].starts_with(b"%%") {
                f.extend_from_slice(b"%%");
                i += 2;
                continue;
            }
            if fmt[i..].starts_with(DESCRIBE) {
                if self.describe_used {
                    f.push(b'%');
                } else {
                    self.describe_used = true;
                }
            }
            f.push(fmt[i]);
            i += 1;
        }
        let args = vec!["-1".to_string(), commit.to_string()];
        let (items, _) = crate::history::walk_formatted(self.job.ctx, &args, &[f])?;
        Ok(items
            .into_iter()
            .next()
            .and_then(|i| i.expanded.into_iter().next())
            .unwrap_or_default())
    }
}

/// The id git shows for files that are not in the object database.
fn extra_oid(hex_len: usize) -> String {
    let mut s = "0".repeat(15);
    s.push('1');
    s.push_str(&"0".repeat(hex_len.saturating_sub(16)));
    s
}

/// Check that every pathspec item matches something below `base`.
pub fn check_pathspec(job: &Job<'_>, attrs: &mut Attributes<'_>, root: Option<&ObjectId>) -> Result<(), Die> {
    if job.pathspec.is_empty() {
        return Ok(());
    }
    let mut seen = vec![false; job.pathspec.len()];
    fn walk(
        repo: &Repo,
        ps: &Pathspec,
        attrs: &mut Attributes<'_>,
        seen: &mut [bool],
        tree: &ObjectId,
        dir: &[u8],
    ) {
        let Some(data) = repo.read_tree_data(tree) else {
            return;
        };
        for e in TreeIter::new(repo.hash, &data) {
            let Ok(e) = e else { return };
            let mut full = dir.to_vec();
            full.extend_from_slice(e.name);
            let _ = ps.match_path_with(&full, Some(seen), &mut |p, spec| {
                attrs.satisfies(p, false, spec)
            });
            if e.is_tree() && ps.dir_interest(&full) != DirInterest::None {
                full.push(b'/');
                walk(repo, ps, attrs, seen, &e.oid, &full);
            }
        }
    }
    if let Some(root) = root {
        walk(job.repo, &job.pathspec, attrs, &mut seen, root, &job.base);
    }
    if let Some(item) = job.pathspec.unmatched(&seen).next() {
        return Err(Die::Fatal(msg::pathspec_did_not_match(
            &String::from_utf8_lossy(item.original()),
        )));
    }
    Ok(())
}

/// Open the attributes the archive uses.
fn attributes<'r>(job: &Job<'r>, index: Option<&'r rgit_index::Index>, tree_source: Option<Option<ObjectId>>) -> Attributes<'r> {
    use rgit_attr::{Order, Source};
    let repo = job.repo;
    let odb = repo.odb();
    let source = if job.worktree_attributes {
        if let Some(t) = tree_source {
            Source::Tree(t, odb)
        } else if let Some(root) = &repo.setup.work_tree {
            Source::WorkTree {
                root: root.clone(),
                index: index.map(|i| (i, odb as &dyn rgit_attr::ObjectReader)),
                order: Order::WorkTreeFirst,
            }
        } else {
            Source::None
        }
    } else {
        Source::Tree(Some(job.tree), odb)
    };
    Attributes::new(&crate::attributes::options(repo), source)
}

/// Write the archive.
pub fn run(job: &Job<'_>, index: Option<&rgit_index::Index>, tree_source: Option<Option<ObjectId>>) -> Result<(), Die> {
    let repo = job.repo;
    let mut attrs = attributes(job, index, tree_source);
    let root = subtree(repo, &job.tree, &job.base);
    check_pathspec(job, &mut attrs, root.as_ref())?;
    for w in attrs.take_warnings() {
        eprintln!("{w}");
    }

    let converter = if job.worktree_attributes {
        Converter::worktree(repo)?
    } else {
        let mut files = HashMap::new();
        attribute_files(repo, &job.tree, b"", &mut files);
        Converter::with_attribute_files(repo, files)?
    };

    // Untracked files must exist before anything is written.
    for extra in &job.extras {
        if let Extra::File { path, on_disk, .. } = extra {
            let meta =
                std::fs::metadata(on_disk).map_err(|_| Die::Fatal(msg::file_not_found(path)))?;
            if !meta.is_file() {
                return Err(Die::Fatal(msg::not_a_regular_file(path)));
            }
        }
    }
    let dest = Dest::open(job.output.as_deref())?;
    let out = Out::new(dest, &job.backend, job.level.as_deref())?;
    let commit_hex = job.commit.map(|c| c.to_hex());
    let writer = match job.backend {
        Backend::Zip => {
            let level = job
                .level
                .as_deref()
                .and_then(|l| l.parse::<u32>().ok())
                .unwrap_or(6);
            let date = rgit_date::DateContext::from_env();
            let mtime = i64::try_from(job.mtime).unwrap_or(i64::MAX);
            let tz = date.local_tz(mtime);
            let local = mtime + rgit_date::tz_to_minutes(tz) * 60;
            Writer::Zip(ZipWriter::new(out, level, job.mtime, local))
        }
        _ => {
            let mut t = TarWriter::new(out, job.umask, job.mtime);
            t.begin(commit_hex.as_deref()).map_err(write_error)?;
            Writer::Tar(t)
        }
    };
    let ignore = attrs.id(b"export-ignore");
    let subst = attrs.id(b"export-subst");
    let diff = attrs.id(b"diff");
    let mut walk = Walk {
        job,
        attrs,
        ignore,
        subst,
        diff,
        converter,
        writer,
        pending: Vec::new(),
        describe_used: false,
        commit_hex: commit_hex.clone(),
    };
    if job.prefix.ends_with(b"/") {
        let p = job.prefix.clone();
        walk.emit(&p, EntryKind::Dir, "", Body::Mem(&[]), false)?;
    }
    if let Some(root) = root {
        let base = job.base.clone();
        let prefix = job.prefix.clone();
        walk.walk(&root, &base, &prefix)?;
    }
    let fake = extra_oid(repo.hash.hex_len());
    for extra in &job.extras {
        match extra {
            Extra::File {
                prefix,
                path,
                on_disk,
            } => {
                let meta = std::fs::metadata(on_disk)
                    .map_err(|_| Die::Fatal(msg::file_not_found(path)))?;
                if !meta.is_file() {
                    return Err(Die::Fatal(msg::not_a_regular_file(path)));
                }
                let data =
                    std::fs::read(on_disk).map_err(|_| Die::Fatal(msg::file_not_found(path)))?;
                let base = std::path::Path::new(path)
                    .file_name()
                    .map(|n| n.as_encoded_bytes().to_vec())
                    .unwrap_or_default();
                let mut apath = prefix.clone();
                apath.extend_from_slice(&base);
                #[cfg(unix)]
                let executable = {
                    use std::os::unix::fs::PermissionsExt;
                    meta.permissions().mode() & 0o100 != 0
                };
                #[cfg(not(unix))]
                let executable = false;
                let text = looks_text(&data);
                walk.emit(&apath, EntryKind::File { executable }, &fake, Body::Mem(&data), text)?;
            }
            Extra::Virtual { path, content } => {
                let text = looks_text(content);
                walk.emit(path, EntryKind::File { executable: false }, &fake, Body::Mem(content), text)?;
            }
        }
    }
    let out = walk
        .writer
        .finish(commit_hex.as_deref())
        .map_err(write_error)?;
    out.finish()
}
