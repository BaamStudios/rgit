// SPDX-License-Identifier: MIT
//! `git fast-export` (Documentation/git-fast-export.adoc): write the
//! selected history as a `fast-import` stream.
//!
//! The commits come from the revision walk in `--topo-order --reverse`
//! order. Each commit is written on the ref it was reached from: the
//! starting points name the commits they point to, and while walking from
//! the newest commit back in time, parents take the name of the first
//! child that reaches them. Blobs are written before the first commit
//! that needs them; refs that name already written commits get a `reset`,
//! annotated tags follow at the end.

use std::collections::{BinaryHeap, HashMap, HashSet};
use std::io::Write;

use rgit_hash::{HashKind, ObjectId};
use rgit_object::{Commit, ObjectKind, Tag};

use crate::diff::collect::{Pair, TreeDiff};
use crate::diff::options::{DiffOpts, Formats};
use crate::diff::output::Output;
use crate::messages::fast_export as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signed {
    Verbatim,
    WarnVerbatim,
    WarnStrip,
    Strip,
    Abort,
}

impl Signed {
    fn parse(s: &str) -> Option<Signed> {
        Some(match s {
            "verbatim" => Signed::Verbatim,
            "warn-verbatim" | "warn" => Signed::WarnVerbatim,
            "warn-strip" => Signed::WarnStrip,
            "strip" => Signed::Strip,
            "abort" => Signed::Abort,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Filtered {
    Abort,
    Drop,
    Rewrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reencode {
    Yes,
    No,
    Abort,
}

/// `--anonymize`: replacement names, the same for the same input.
#[derive(Default)]
struct Anonymizer {
    /// `--anonymize-map` tokens.
    user: HashMap<Vec<u8>, Vec<u8>>,
    paths: HashMap<Vec<u8>, Vec<u8>>,
    refs: HashMap<Vec<u8>, Vec<u8>>,
    idents: HashMap<Vec<u8>, Vec<u8>>,
    blobs: HashMap<ObjectId, usize>,
    gitlinks: HashMap<ObjectId, ObjectId>,
    commits: usize,
    tag_messages: HashMap<Vec<u8>, usize>,
}

impl Anonymizer {
    fn token(map: &mut HashMap<Vec<u8>, Vec<u8>>, user: &HashMap<Vec<u8>, Vec<u8>>, prefix: &str, t: &[u8]) -> Vec<u8> {
        if let Some(u) = user.get(t) {
            return u.clone();
        }
        let n = map.len();
        map.entry(t.to_vec())
            .or_insert_with(|| format!("{prefix}{n}").into_bytes())
            .clone()
    }

    fn path(&mut self, p: &[u8]) -> Vec<u8> {
        let parts: Vec<Vec<u8>> = p
            .split(|&c| c == b'/')
            .map(|c| Self::token(&mut self.paths, &self.user, "path", c))
            .collect();
        parts.join(&b'/')
    }

    fn refname(&mut self, name: &str) -> String {
        let b = name.as_bytes();
        // `refs/<kind>/` stays as it is.
        let (keep, rest) = match b.strip_prefix(b"refs/") {
            Some(r) => match r.iter().position(|&c| c == b'/') {
                Some(i) => (&b[..5 + i + 1], &r[i + 1..]),
                None => (&b[..0], b),
            },
            None => (&b[..0], b),
        };
        let mut out = keep.to_vec();
        let parts: Vec<Vec<u8>> = rest
            .split(|&c| c == b'/')
            .map(|c| Self::token(&mut self.refs, &self.user, "ref", c))
            .collect();
        out.extend_from_slice(&parts.join(&b'/'));
        String::from_utf8_lossy(&out).into_owned()
    }

    /// `Name <email> <date>` with name and email replaced.
    fn ident(&mut self, id: &[u8]) -> Vec<u8> {
        let end = id.iter().rposition(|&c| c == b'>').map_or(id.len(), |p| p + 1);
        let (who, when) = id.split_at(end);
        let n = self.idents.len();
        let mut out = self
            .idents
            .entry(who.to_vec())
            .or_insert_with(|| format!("User {n} <user{n}@example.com>").into_bytes())
            .clone();
        out.extend_from_slice(when);
        out
    }

    fn blob(&mut self, oid: &ObjectId) -> Vec<u8> {
        let n = self.blobs.len();
        let n = *self.blobs.entry(*oid).or_insert(n);
        format!("anonymous blob {n}").into_bytes()
    }

    fn gitlink(&mut self, oid: &ObjectId) -> ObjectId {
        let n = self.gitlinks.len() + 1;
        *self.gitlinks.entry(*oid).or_insert_with(|| {
            let hex = format!("{n:0width$x}", width = oid.kind().hex_len());
            ObjectId::from_hex(oid.kind(), hex.as_bytes()).expect("hex")
        })
    }
}

struct Exporter {
    repo: Repo,
    hash: HashKind,
    out: std::io::BufWriter<std::io::Stdout>,
    marks: HashMap<ObjectId, u64>,
    /// Marked objects in the order they got their marks.
    marked: Vec<ObjectId>,
    last_mark: u64,
    /// Commits written by this run (or known from imported marks).
    shown: HashSet<ObjectId>,
    /// The names commits were written on.
    written_names: HashSet<String>,
    signed_tags: Signed,
    signed_commits: Signed,
    filtered: Filtered,
    reencode: Reencode,
    fake_missing_tagger: bool,
    full_tree: bool,
    no_data: bool,
    reference_excluded_parents: bool,
    show_original_ids: bool,
    mark_tags: bool,
    progress: Option<u64>,
    counter: u64,
    refspecs: Vec<rgit_remote::refspec::Refspec>,
    anon: Option<Anonymizer>,
    diff_opts: DiffOpts,
    marks_changed: bool,
    /// The paths limiting the export.
    pathspec: Option<rgit_pathspec::Pathspec>,
}

fn w(e: std::io::Error) -> Die {
    Die::Fatal(crate::global::strerror(&e))
}

/// Order of the changes of a commit: by path, a path before the paths it
/// is a prefix of, renames after other changes of the same path.
fn depth_first(a: &Pair, b: &Pair) -> std::cmp::Ordering {
    let pa = if a.old.is_some() { &a.old_path } else { &a.path };
    let pb = if b.old.is_some() { &b.old_path } else { &b.path };
    let len = pa.len().min(pb.len());
    pa[..len]
        .cmp(&pb[..len])
        .then_with(|| pb.len().cmp(&pa.len()))
        .then_with(|| (a.renamed == Some(b'R')).cmp(&(b.renamed == Some(b'R'))))
}

/// A path for the stream: quoted when it needs quoting or has a space.
fn stream_path(out: &mut Vec<u8>, path: &[u8]) {
    let needs = path
        .iter()
        .any(|&c| c < 0x20 || c == b'"' || c == b'\\' || c == 0x7f || c >= 0x80 || c == b' ');
    if !needs {
        out.extend_from_slice(path);
        return;
    }
    let mut q = Vec::new();
    crate::quote::write_quoted(&mut q, path, true);
    if q.first() == Some(&b'"') {
        out.extend_from_slice(&q);
    } else {
        out.push(b'"');
        out.extend_from_slice(&q);
        out.push(b'"');
    }
}

/// The signature format of a `gpgsig` value.
fn signature_format(sig: &[u8]) -> &'static str {
    if sig.starts_with(b"-----BEGIN PGP SIGNATURE-----") {
        "openpgp"
    } else if sig.starts_with(b"-----BEGIN SIGNED MESSAGE-----") {
        "x509"
    } else if sig.starts_with(b"-----BEGIN SSH SIGNATURE-----") {
        "ssh"
    } else {
        "unknown"
    }
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let progress: RefCell<Option<String>> = RefCell::new(None);
    let signed_tags = Cell::new(Signed::Abort);
    let signed_commits = Cell::new(Signed::Strip);
    let filtered = Cell::new(Filtered::Abort);
    let reencode = Cell::new(Reencode::Abort);
    let export_marks: RefCell<Option<String>> = RefCell::new(None);
    let import_marks: RefCell<Option<(String, bool)>> = RefCell::new(None);
    let fake_tagger = Cell::new(false);
    let full_tree = Cell::new(false);
    let done = Cell::new(false);
    let no_data = Cell::new(false);
    let refspecs: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let anonymize = Cell::new(false);
    let anon_map: RefCell<Vec<(Vec<u8>, Vec<u8>)>> = RefCell::new(Vec::new());
    let ref_excluded = Cell::new(false);
    let original_ids = Cell::new(false);
    let mark_tags = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::string(None, "progress", &progress, "n", msg::HELP_PROGRESS),
            Opt::callback(None, "signed-tags", Some("mode"), msg::HELP_SIGNED_TAGS, |v, unset| {
                let v = v.unwrap_or("");
                if unset {
                    signed_tags.set(Signed::Abort);
                    return Ok(());
                }
                signed_tags.set(
                    Signed::parse(v)
                        .ok_or_else(|| CallbackError::Error(msg::unknown_signed_tags_mode(v)))?,
                );
                Ok(())
            }),
            Opt::callback(None, "signed-commits", Some("mode"), msg::HELP_SIGNED_COMMITS, |v, unset| {
                let v = v.unwrap_or("");
                if unset {
                    signed_commits.set(Signed::Strip);
                    return Ok(());
                }
                signed_commits.set(
                    Signed::parse(v)
                        .ok_or_else(|| CallbackError::Error(msg::unknown_signed_commits_mode(v)))?,
                );
                Ok(())
            }),
            Opt::callback(
                None,
                "tag-of-filtered-object",
                Some("mode"),
                msg::HELP_TAG_OF_FILTERED,
                |v, unset| {
                    let v = v.unwrap_or("");
                    filtered.set(match (unset, v) {
                        (true, _) => Filtered::Abort,
                        (_, "abort") => Filtered::Abort,
                        (_, "drop") => Filtered::Drop,
                        (_, "rewrite") => Filtered::Rewrite,
                        _ => return Err(CallbackError::Error(msg::unknown_filtered_mode(v))),
                    });
                    Ok(())
                },
            ),
            Opt::callback(None, "reencode", Some("mode"), msg::HELP_REENCODE, |v, unset| {
                let v = v.unwrap_or("");
                reencode.set(match (unset, v) {
                    (true, _) => Reencode::Abort,
                    (_, "yes" | "true" | "on") => Reencode::Yes,
                    (_, "no" | "false" | "off") => Reencode::No,
                    (_, "abort") => Reencode::Abort,
                    _ => return Err(CallbackError::Error(msg::unknown_reencode_mode(v))),
                });
                Ok(())
            }),
            Opt::string(None, "export-marks", &export_marks, "file", msg::HELP_EXPORT_MARKS),
            Opt::callback(None, "import-marks", Some("file"), msg::HELP_IMPORT_MARKS, |v, unset| {
                *import_marks.borrow_mut() = if unset {
                    None
                } else {
                    Some((v.unwrap_or("").to_string(), false))
                };
                Ok(())
            }),
            Opt::callback(
                None,
                "import-marks-if-exists",
                Some("file"),
                msg::HELP_IMPORT_MARKS_IF_EXISTS,
                |v, unset| {
                    *import_marks.borrow_mut() = if unset {
                        None
                    } else {
                        Some((v.unwrap_or("").to_string(), true))
                    };
                    Ok(())
                },
            ),
            Opt::flag(None, "fake-missing-tagger", &fake_tagger, msg::HELP_FAKE_TAGGER),
            Opt::flag(None, "full-tree", &full_tree, msg::HELP_FULL_TREE),
            Opt::flag(None, "use-done-feature", &done, msg::HELP_DONE),
            Opt::callback(None, "no-data", None, msg::HELP_NO_DATA, |_, _| {
                no_data.set(true);
                Ok(())
            })
            .noarg()
            .noneg(),
            Opt::callback(None, "data", None, msg::HELP_DATA, |_, _| {
                no_data.set(false);
                Ok(())
            })
            .noarg()
            .noneg(),
            Opt::string_list(None, "refspec", &refspecs, "refspec", msg::HELP_REFSPEC),
            Opt::flag(None, "anonymize", &anonymize, msg::HELP_ANONYMIZE),
            Opt::callback(None, "anonymize-map", Some("from:to"), msg::HELP_ANONYMIZE_MAP, |v, _| {
                let v = v.unwrap_or("");
                let (from, to) = v.split_once(':').unwrap_or((v, v));
                if from.is_empty() || to.is_empty() {
                    return Err(CallbackError::Error(msg::EMPTY_MAP_TOKEN.into()));
                }
                anon_map
                    .borrow_mut()
                    .push((from.as_bytes().to_vec(), to.as_bytes().to_vec()));
                Ok(())
            })
            .noneg(),
            Opt::flag(None, "reference-excluded-parents", &ref_excluded, msg::HELP_REFERENCE_EXCLUDED),
            Opt::flag(None, "show-original-ids", &original_ids, msg::HELP_ORIGINAL_IDS),
            Opt::flag(None, "mark-tags", &mark_tags, msg::HELP_MARK_TAGS),
        ],
    )
    .flags(ParseFlags {
        keep_unknown_opt: true,
        ..ParseFlags::default()
    });
    if argv.len() == 1 {
        return Err(parser.usage_error().into());
    }
    let parsed = parser.parse(argv)?;
    drop(parser);
    if !anon_map.borrow().is_empty() && !anonymize.get() {
        return Err(Die::Fatal(msg::MAP_NEEDS_ANONYMIZE.into()));
    }
    // Rename and copy detection options go to the diff, everything else
    // to the revision walk.
    let mut diff_opts = DiffOpts::default();
    let mut rev_args = Vec::new();
    // Arguments after `--end-of-options` are revisions, whatever they look
    // like; the option parser has taken the marker away.
    let after_eoo = argv
        .iter()
        .take_while(|a| *a != "--")
        .position(|a| a == "--end-of-options")
        .map_or(0, |i| argv[i + 1..].iter().filter(|a| *a != "--").count());
    let (opts_part, literal) = parsed.args.split_at(parsed.args.len() - after_eoo.min(parsed.args.len()));
    for a in opts_part {
        let rename_opt = a == "-M"
            || a == "-C"
            || a.starts_with("-M") && a[2..].bytes().all(|c| c.is_ascii_digit() || c == b'%')
            || a.starts_with("-C") && a[2..].bytes().all(|c| c.is_ascii_digit() || c == b'%')
            || a.starts_with("--find-renames")
            || a.starts_with("--find-copies")
            || a == "--no-renames"
            || a.starts_with("-l") && a.len() > 2;
        if rename_opt && diff_opts.parse_arg(a)? {
            continue;
        }
        if a == "--reverse" || a == "--topo-order" || a == "--date-order" {
            continue;
        }
        rev_args.push(a.clone());
    }
    if !literal.is_empty() {
        rev_args.push("--end-of-options".to_string());
        rev_args.extend(literal.iter().cloned());
    }

    let mut repo = Repo::open(ctx, Mode::Required)?;
    let hash = repo.hash;
    let mut specs = Vec::new();
    for s in refspecs.borrow().iter() {
        let spec = rgit_remote::refspec::Refspec::parse_with_hash(
            s.as_bytes(),
            rgit_remote::refspec::Direction::Push,
            hash,
        )
        .map_err(|_| Die::Fatal(msg::invalid_refspec(s)))?;
        specs.push(spec);
    }
    let _ = &mut repo;
    let mut ex = Exporter {
        repo,
        hash,
        out: std::io::BufWriter::new(std::io::stdout()),
        marks: HashMap::new(),
        marked: Vec::new(),
        last_mark: 0,
        shown: HashSet::new(),
        written_names: HashSet::new(),
        signed_tags: signed_tags.get(),
        signed_commits: signed_commits.get(),
        filtered: filtered.get(),
        reencode: reencode.get(),
        fake_missing_tagger: fake_tagger.get(),
        full_tree: full_tree.get(),
        no_data: no_data.get(),
        reference_excluded_parents: ref_excluded.get(),
        show_original_ids: original_ids.get(),
        mark_tags: mark_tags.get(),
        progress: progress
            .borrow()
            .as_deref()
            .and_then(|p| p.parse::<u64>().ok())
            .filter(|&n| n > 0),
        counter: 0,
        refspecs: specs,
        anon: anonymize.get().then(|| Anonymizer {
            user: anon_map.borrow().iter().cloned().collect(),
            ..Anonymizer::default()
        }),
        diff_opts,
        marks_changed: false,
        pathspec: None,
    };
    if let Some((file, if_exists)) = import_marks.borrow().clone() {
        ex.import_marks(&file, if_exists)?;
    }
    let mut ex = ex;
    if done.get() {
        ex.out.write_all(b"feature done\n").map_err(w)?;
    }
    let walk = if rev_args.is_empty() && !ex.refspecs.is_empty() {
        None
    } else {
        Some(crate::history::walk_export(ctx, &rev_args)?)
    };
    if let Some(walk) = &walk {
        // With imported marks a path-limited export cannot rely on the
        // files of earlier commits: every commit lists its whole tree.
        if import_marks.borrow().is_some() && !walk.paths.is_empty() {
            ex.full_tree = true;
        }
        ex.export(ctx, walk)?;
    }
    ex.refspec_deletions()?;
    if done.get() {
        ex.out.write_all(b"done\n").map_err(w)?;
    }
    ex.out.flush().map_err(w)?;
    if let Some(file) = export_marks.borrow().as_deref() {
        ex.export_marks(file)?;
    }
    Ok(0)
}

impl Exporter {
    fn import_marks(&mut self, file: &str, if_exists: bool) -> Result<(), Die> {
        let text = match std::fs::read(file) {
            Ok(t) => t,
            Err(e) if if_exists && e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                return Err(Die::Fatal(msg::cannot_read(file, &crate::global::strerror(&e))));
            }
        };
        for line in text.split(|&c| c == b'\n') {
            if line.is_empty() {
                continue;
            }
            let bad = || Die::Fatal(msg::not_a_mark(&String::from_utf8_lossy(line)));
            let rest = line.strip_prefix(b":").ok_or_else(bad)?;
            let sp = rest.iter().position(|&c| c == b' ').ok_or_else(bad)?;
            let mark: u64 = std::str::from_utf8(&rest[..sp])
                .ok()
                .and_then(|s| s.parse().ok())
                .ok_or_else(bad)?;
            let oid = ObjectId::from_hex(self.hash, &rest[sp + 1..]).map_err(|_| bad())?;
            self.last_mark = self.last_mark.max(mark);
            // Only commits keep their marks (blobs are written again).
            if self.repo.odb.header(&oid).ok().flatten().map(|h| h.kind) != Some(ObjectKind::Commit) {
                continue;
            }
            if self.marks.insert(oid, mark).is_none() {
                self.marked.push(oid);
            }
            self.shown.insert(oid);
        }
        Ok(())
    }

    fn export_marks(&self, file: &str) -> Result<(), Die> {
        if !self.marks_changed {
            return Ok(());
        }
        // The marks are kept in a hash table keyed by the first four bytes
        // of the object id (little endian), with linear probing; it grows
        // to (size + 1000) * 3 / 2 when two thirds are used. Commits are
        // written in table order.
        let mut size = 0usize;
        let mut slots: Vec<Option<ObjectId>> = Vec::new();
        let place = |slots: &mut Vec<Option<ObjectId>>, oid: ObjectId| {
            let b = oid.as_bytes();
            let h = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize;
            let n = slots.len();
            let mut i = h % n;
            while slots[i].is_some() {
                i = (i + 1) % n;
            }
            slots[i] = Some(oid);
        };
        for (used, oid) in self.marked.iter().enumerate() {
            if used >= size * 2 / 3 {
                size = (size + 1000) * 3 / 2;
                let old: Vec<ObjectId> = slots.iter().flatten().copied().collect();
                slots = vec![None; size];
                for o in old {
                    place(&mut slots, o);
                }
            }
            place(&mut slots, *oid);
        }
        let list: Vec<(u64, ObjectId)> = slots
            .into_iter()
            .flatten()
            .filter(|oid| {
                self.repo.odb.header(oid).ok().flatten().map(|h| h.kind) == Some(ObjectKind::Commit)
            })
            .map(|o| (self.marks[&o], o))
            .collect();
        let mut out = Vec::new();
        for (m, o) in list {
            out.extend_from_slice(format!(":{m} {o}\n").as_bytes());
        }
        std::fs::write(file, out)
            .map_err(|e| Die::Fatal(msg::unable_to_write(file, &crate::global::strerror(&e))))
    }

    fn next_mark(&mut self, oid: ObjectId) -> u64 {
        self.last_mark += 1;
        if self.marks.insert(oid, self.last_mark).is_none() {
            self.marked.push(oid);
        }
        self.marks_changed = true;
        self.last_mark
    }

    fn read(&self, oid: &ObjectId) -> Result<(ObjectKind, Vec<u8>), Die> {
        match self.repo.odb.read(oid) {
            Ok(Some(o)) => Ok((o.kind, o.data)),
            _ => Err(Die::Fatal(msg::could_not_read(&oid.to_hex()))),
        }
    }

    /// The name a ref is written as: mapped by `--refspec`, anonymized.
    fn ref_name(&mut self, name: &str) -> String {
        let mut name = name.to_string();
        for spec in &self.refspecs {
            if spec.src.is_empty() {
                continue;
            }
            if let Some(dst) = spec.map_src(name.as_bytes()) {
                name = String::from_utf8_lossy(&dst).into_owned();
                break;
            }
        }
        match &mut self.anon {
            Some(a) => a.refname(&name),
            None => name,
        }
    }

    fn progress(&mut self) -> Result<(), Die> {
        self.counter += 1;
        if let Some(n) = self.progress {
            if self.counter % n == 0 {
                let line = format!("progress {} objects\n", self.counter);
                self.out.write_all(line.as_bytes()).map_err(w)?;
            }
        }
        Ok(())
    }

    fn write_blob(&mut self, oid: &ObjectId) -> Result<(), Die> {
        if self.no_data || self.marks.contains_key(oid) {
            return Ok(());
        }
        let (_, data) = self.read(oid)?;
        let data = match &mut self.anon {
            Some(a) => a.blob(oid),
            None => data,
        };
        let mark = self.next_mark(*oid);
        let mut b = format!("blob\nmark :{mark}\n").into_bytes();
        if self.show_original_ids {
            b.extend_from_slice(format!("original-oid {oid}\n").as_bytes());
        }
        b.extend_from_slice(format!("data {}\n", data.len()).as_bytes());
        b.extend_from_slice(&data);
        b.push(b'\n');
        self.out.write_all(&b).map_err(w)?;
        self.progress()
    }

    /// The names of the starting points: full ref names where the
    /// argument names a ref.
    fn tip_names(&self, ctx: &Context, walk: &crate::history::ExportWalk) -> Result<Vec<String>, Die> {
        let revs = crate::refs_support::Revs::new(ctx, &self.repo.setup)?;
        Ok(walk
            .tips
            .iter()
            .map(|(_, _, src)| {
                if src.starts_with("refs/") || src == "HEAD" && !src.is_empty() {
                    if let Ok(d) = revs.with_resolver(|r| r.dwim_ref(src)) {
                        if d.count > 0 {
                            return d.full_name;
                        }
                    }
                    return src.clone();
                }
                match revs.with_resolver(|r| r.dwim_ref(src)) {
                    Ok(d) if d.count > 0 => d.full_name,
                    _ => src.clone(),
                }
            })
            .collect())
    }

    /// Committer time and parents of a commit (cached).
    fn commit_info(
        &self,
        c: &ObjectId,
        cache: &mut HashMap<ObjectId, (i64, Vec<ObjectId>)>,
    ) -> (i64, Vec<ObjectId>) {
        if let Some(v) = cache.get(c) {
            return v.clone();
        }
        let v = self
            .read(c)
            .ok()
            .and_then(|(_, data)| Commit::parse(self.hash, &data).ok())
            .map(|commit| {
                let t = commit
                    .committer()
                    .and_then(|s| s.time().map(|t| t.seconds))
                    .unwrap_or(0);
                (t, commit.parents().unwrap_or_default())
            })
            .unwrap_or((0, Vec::new()));
        cache.insert(*c, v.clone());
        v
    }

    fn peel_commit(&self, mut oid: ObjectId) -> Option<ObjectId> {
        for _ in 0..64 {
            let (kind, data) = self.read(&oid).ok()?;
            match kind {
                ObjectKind::Commit => return Some(oid),
                ObjectKind::Tag => oid = Tag::parse(self.hash, &data).ok()?.object().ok()?,
                _ => return None,
            }
        }
        None
    }

    fn export(&mut self, ctx: &Context, walk: &crate::history::ExportWalk) -> Result<(), Die> {
        let names = self.tip_names(ctx, walk)?;
        // Blobs tagged directly are written first; refs to other
        // non-commits are skipped.
        for (oid, negative, source) in &walk.tips {
            if *negative {
                continue;
            }
            let Ok((kind, data)) = self.read(oid) else {
                continue;
            };
            match kind {
                ObjectKind::Tag => {
                    let mut cur = (kind, data);
                    for _ in 0..64 {
                        if cur.0 != ObjectKind::Tag {
                            break;
                        }
                        let Ok(target) = Tag::parse(self.hash, &cur.1).and_then(|t| t.object())
                        else {
                            break;
                        };
                        let Ok(next) = self.read(&target) else {
                            break;
                        };
                        if next.0 == ObjectKind::Blob {
                            self.write_blob(&target)?;
                        }
                        if next.0 == ObjectKind::Tree {
                            eprintln!("warning: {}", msg::tag_unexpected_type("tree"));
                        }
                        cur = next;
                    }
                }
                ObjectKind::Blob | ObjectKind::Tree => {
                    eprintln!("warning: {}", msg::unexpected_type(source, kind.name()));
                }
                ObjectKind::Commit => {}
            }
        }

        // Names of the commits: the starting points, then parents from the
        // first child reaching them, newest commits first (the whole
        // history counts, also commits that are not written).
        let in_walk: HashSet<ObjectId> = walk.commits.iter().copied().collect();
        let mut unnamed = in_walk.len();
        let mut label: HashMap<ObjectId, String> = HashMap::new();
        let mut heap: BinaryHeap<(i64, std::cmp::Reverse<u64>, ObjectId)> = BinaryHeap::new();
        let mut seq = 0u64;
        let mut queued: HashSet<ObjectId> = HashSet::new();
        let mut parents_of: HashMap<ObjectId, (i64, Vec<ObjectId>)> = HashMap::new();
        for ((oid, negative, _), name) in walk.tips.iter().zip(&names) {
            if *negative {
                continue;
            }
            let Some(c) = self.peel_commit(*oid) else {
                continue;
            };
            if let std::collections::hash_map::Entry::Vacant(e) = label.entry(c) {
                e.insert(name.clone());
                if in_walk.contains(&c) {
                    unnamed -= 1;
                }
            }
            if queued.insert(c) {
                let t = self.commit_info(&c, &mut parents_of).0;
                heap.push((t, std::cmp::Reverse(seq), c));
                seq += 1;
            }
        }
        while unnamed > 0 {
            let Some((_, _, c)) = heap.pop() else {
                break;
            };
            let Some(name) = label.get(&c).cloned() else {
                continue;
            };
            let parents = self.commit_info(&c, &mut parents_of).1;
            for p in parents {
                if let std::collections::hash_map::Entry::Vacant(e) = label.entry(p) {
                    e.insert(name.clone());
                    if in_walk.contains(&p) {
                        unnamed -= 1;
                    }
                }
                if queued.insert(p) {
                    let t = self.commit_info(&p, &mut parents_of).0;
                    heap.push((t, std::cmp::Reverse(seq), p));
                    seq += 1;
                }
            }
        }

        let pathspec = crate::diff::pathspec(ctx, &self.repo, &walk.paths)?;
        self.pathspec = Some(pathspec.clone());
        for c in &walk.commits {
            if self.shown.contains(c) {
                continue;
            }
            let name = label.get(c).cloned().unwrap_or_default();
            let parents = walk.parents.get(c).cloned().unwrap_or_default();
            self.write_commit(c, &name, &parents, &pathspec)?;
        }

        // Refs whose commit was written under another name, then tags;
        // each by name, last name first.
        let mut extra: std::collections::BTreeMap<String, ObjectId> = Default::default();
        let mut tags: std::collections::BTreeMap<String, ObjectId> = Default::default();
        for ((oid, negative, _), name) in walk.tips.iter().zip(&names) {
            if *negative {
                continue;
            }
            let Ok((kind, _)) = self.read(oid) else {
                continue;
            };
            match kind {
                ObjectKind::Tag => {
                    tags.entry(name.clone()).or_insert(*oid);
                }
                ObjectKind::Commit => {
                    extra.entry(name.clone()).or_insert(*oid);
                }
                _ => {}
            }
        }
        for (name, oid) in extra.iter().rev() {
            // A commit that is not written stands for its nearest written
            // ancestor.
            // A ref that commits were written on needs no reset.
            if self.written_names.contains(name) {
                continue;
            }
            let target = self.rewritten(oid, walk);
            let from = match target.and_then(|t| self.marks.get(&t)) {
                Some(m) => format!(":{m}"),
                None => ObjectId::null(self.hash).to_hex(),
            };
            let shown = self.ref_name(name);
            let s = format!("reset {shown}\nfrom {from}\n\n");
            self.out.write_all(s.as_bytes()).map_err(w)?;
        }
        for (name, oid) in tags.iter().rev() {
            self.write_tag(oid, name, walk)?;
        }
        Ok(())
    }

    fn diff(
        &mut self,
        old: Option<&ObjectId>,
        new: &ObjectId,
        pathspec: &rgit_pathspec::Pathspec,
    ) -> Result<Vec<Pair>, Die> {
        let mut t = TreeDiff {
            repo: &self.repo,
            pathspec,
            recursive: true,
            show_trees: false,
            max_depth: None,
            pairs: Vec::new(),
        };
        t.walk(old, Some(new), b"")?;
        let mut pairs = t.pairs;
        if self.diff_opts.copies_harder {
            if let Some(old) = old {
                let files = crate::diff::collect::tree_files(&self.repo, old, pathspec)?;
                pairs = crate::diff::collect::with_unmodified(pairs, files);
            }
        }
        let out = Output::new(Some(&self.repo), self.hash, &self.diff_opts, Formats::default(), false)?;
        let pairs = out.diffcore(pairs)?;
        // Unmodified copy sources are not changes.
        Ok(pairs
            .into_iter()
            .filter(|p| {
                p.renamed.is_some()
                    || match (&p.old, &p.new) {
                        (Some(o), Some(n)) => o.oid != n.oid || o.mode != n.mode,
                        _ => true,
                    }
            })
            .collect())
    }

    fn tree_of(&self, commit: &ObjectId) -> Result<ObjectId, Die> {
        let (_, data) = self.read(commit)?;
        Commit::parse(self.hash, &data)
            .ok()
            .and_then(|c| c.tree().ok())
            .ok_or_else(|| Die::Fatal(msg::could_not_read(&commit.to_hex())))
    }

    fn write_commit(
        &mut self,
        oid: &ObjectId,
        name: &str,
        parents: &[ObjectId],
        pathspec: &rgit_pathspec::Pathspec,
    ) -> Result<(), Die> {
        let (_, data) = self.read(oid)?;
        let commit = Commit::parse(self.hash, &data)
            .map_err(|_| Die::Fatal(msg::could_not_read(&oid.to_hex())))?;
        let author = commit
            .author_raw()
            .ok_or_else(|| Die::Fatal(msg::no_author(&oid.to_hex())))?
            .to_vec();
        let committer = commit
            .committer_raw()
            .ok_or_else(|| Die::Fatal(msg::no_committer(&oid.to_hex())))?
            .to_vec();
        let tree = commit
            .tree()
            .map_err(|_| Die::Fatal(msg::could_not_read(&oid.to_hex())))?;
        let first_available = parents
            .first()
            .filter(|p| self.marks.contains_key(p) || self.reference_excluded_parents);
        let base = match first_available {
            Some(p) => Some(self.tree_of(p)?),
            None => None,
        };
        let mut pairs = if self.full_tree {
            self.diff(None, &tree, pathspec)?
        } else {
            self.diff(base.as_ref(), &tree, pathspec)?
        };
        // Blobs first, in diff order.
        for p in &pairs {
            if let Some(n) = &p.new {
                if n.mode & 0o170000 != 0o160000 {
                    self.write_blob(&n.oid)?;
                }
            }
        }
        let mut message = commit.message().to_vec();
        let mut encoding = commit.encoding().map(<[u8]>::to_vec);
        if let Some(enc) = &encoding {
            let enc_s = String::from_utf8_lossy(enc).into_owned();
            if !rgit_convert::encoding::same_encoding(&enc_s, "UTF-8") {
                match self.reencode {
                    Reencode::Abort => {
                        return Err(Die::Fatal(msg::encoding_abort(&enc_s, &oid.to_hex())));
                    }
                    Reencode::Yes => {
                        if let Some(m) = rgit_convert::encoding::transcode(&message, &enc_s, "UTF-8")
                        {
                            message = m;
                            encoding = None;
                        }
                    }
                    Reencode::No => {}
                }
            }
        }
        let shown_name = self.ref_name(name);
        let mark = self.next_mark(*oid);
        self.shown.insert(*oid);
        self.written_names.insert(name.to_string());
        let mut b = Vec::new();
        if parents.is_empty() {
            b.extend_from_slice(format!("reset {shown_name}\n").as_bytes());
        }
        b.extend_from_slice(format!("commit {shown_name}\nmark :{mark}\n").as_bytes());
        if self.show_original_ids {
            b.extend_from_slice(format!("original-oid {oid}\n").as_bytes());
        }
        let (author, committer) = match &mut self.anon {
            Some(a) => {
                let c = a.ident(&committer);
                (a.ident(&author), c)
            }
            None => (author, committer),
        };
        b.extend_from_slice(b"author ");
        b.extend_from_slice(&author);
        b.extend_from_slice(b"\ncommitter ");
        b.extend_from_slice(&committer);
        b.push(b'\n');
        let sigs: Vec<(&str, Vec<u8>)> = [
            ("sha1", commit.gpgsig().map(<[u8]>::to_vec)),
            ("sha256", commit.gpgsig_sha256().map(<[u8]>::to_vec)),
        ]
        .into_iter()
        .filter_map(|(a, s)| s.map(|s| (a, s)))
        .collect();
        if !sigs.is_empty() {
            match self.signed_commits {
                Signed::Abort => {
                    return Err(Die::Fatal(msg::signed_commit_abort(&oid.to_hex())));
                }
                Signed::WarnStrip => {
                    eprintln!("warning: {}", msg::stripping_commit_signature(&oid.to_hex()));
                }
                Signed::Strip => {}
                Signed::Verbatim | Signed::WarnVerbatim => {
                    if self.signed_commits == Signed::WarnVerbatim {
                        eprintln!("warning: {}", msg::exporting_signed_commit(&oid.to_hex()));
                    }
                    // The signature over the repository's own format
                    // first.
                    let mut ordered = sigs.clone();
                    if self.hash == HashKind::Sha256 {
                        ordered.reverse();
                    }
                    for (algo, sig) in ordered {
                        let algo = if algo == "sha1" {
                            if self.hash == HashKind::Sha1 { "sha1" } else { "sha256" }
                        } else if self.hash == HashKind::Sha1 {
                            "sha256"
                        } else {
                            "sha1"
                        };
                        b.extend_from_slice(
                            format!("gpgsig {algo} {}\ndata {}\n", signature_format(&sig), sig.len())
                                .as_bytes(),
                        );
                        b.extend_from_slice(&sig);
                        b.push(b'\n');
                    }
                }
            }
        }
        if let Some(enc) = &encoding {
            b.extend_from_slice(b"encoding ");
            b.extend_from_slice(enc);
            b.push(b'\n');
        }
        if let Some(a) = &mut self.anon {
            a.commits += 1;
            message = format!("subject {}\n\nbody\n", a.commits - 1).into_bytes();
        }
        b.extend_from_slice(format!("data {}\n", message.len()).as_bytes());
        b.extend_from_slice(&message);
        let mut first = true;
        for p in parents {
            let r = match self.marks.get(p) {
                Some(m) => format!(":{m}"),
                None if self.reference_excluded_parents => p.to_hex(),
                None => continue,
            };
            let kw = if first { "from" } else { "merge" };
            first = false;
            b.extend_from_slice(format!("{kw} {r}\n").as_bytes());
        }
        if self.full_tree {
            b.extend_from_slice(b"deleteall\n");
        }
        pairs.sort_by(depth_first);
        let modified: HashSet<Vec<u8>> = pairs
            .iter()
            .filter(|p| p.renamed.is_none() && p.old.is_some() && p.new.is_some())
            .map(|p| p.path.clone())
            .collect();
        for p in &pairs {
            self.write_change(&mut b, p, &modified)?;
        }
        b.push(b'\n');
        self.out.write_all(&b).map_err(w)?;
        self.progress()
    }

    fn path(&mut self, p: &[u8]) -> Vec<u8> {
        match &mut self.anon {
            Some(a) => a.path(p),
            None => p.to_vec(),
        }
    }

    fn write_change(
        &mut self,
        b: &mut Vec<u8>,
        p: &Pair,
        modified: &HashSet<Vec<u8>>,
    ) -> Result<(), Die> {
        match (p.renamed, &p.old, &p.new) {
            (Some(kind), Some(o), Some(n)) => {
                let same = o.oid == n.oid && o.mode == n.mode;
                if kind == b'C' && modified.contains(&p.old_path) {
                    return self.write_modify(b, &p.path, n.mode, &n.oid);
                }
                let src = self.path(&p.old_path);
                let dst = self.path(&p.path);
                b.push(kind);
                b.push(b' ');
                stream_path(b, &src);
                b.push(b' ');
                stream_path(b, &dst);
                b.push(b'\n');
                if !same {
                    self.write_modify(b, &p.path, n.mode, &n.oid)?;
                }
            }
            (_, Some(_), None) => {
                let path = self.path(&p.old_path);
                b.extend_from_slice(b"D ");
                stream_path(b, &path);
                b.push(b'\n');
            }
            (_, _, Some(n)) => {
                if let Some(o) = &p.old {
                    if o.mode & 0o170000 != n.mode & 0o170000 && p.old_path != p.path {
                        let path = self.path(&p.old_path);
                        b.extend_from_slice(b"D ");
                        stream_path(b, &path);
                        b.push(b'\n');
                    }
                }
                self.write_modify(b, &p.path, n.mode, &n.oid)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn write_modify(&mut self, b: &mut Vec<u8>, path: &[u8], mode: u32, oid: &ObjectId) -> Result<(), Die> {
        let dataref = if mode & 0o170000 == 0o160000 {
            match &mut self.anon {
                Some(a) => a.gitlink(oid).to_hex(),
                None => oid.to_hex(),
            }
        } else if self.no_data {
            oid.to_hex()
        } else {
            match self.marks.get(oid) {
                Some(m) => format!(":{m}"),
                None => oid.to_hex(),
            }
        };
        let path = self.path(path);
        b.extend_from_slice(format!("M {mode:06o} {dataref} ").as_bytes());
        stream_path(b, &path);
        b.push(b'\n');
        Ok(())
    }

    /// The commit a ref or tag of a commit that is not written stands
    /// for: the nearest written ancestor, following a parent the commit
    /// does not differ from in the limiting paths (else the first one).
    fn rewritten(&mut self, commit: &ObjectId, walk: &crate::history::ExportWalk) -> Option<ObjectId> {
        let pathspec = self.pathspec.take().filter(|_| !walk.paths.is_empty());
        let mut seen = HashSet::new();
        let mut c = *commit;
        let found = loop {
            if !seen.insert(c) {
                break None;
            }
            let Some(parents) = self
                .read(&c)
                .ok()
                .and_then(|(_, data)| Commit::parse(self.hash, &data).ok())
                .and_then(|commit| commit.parents().ok())
            else {
                break None;
            };
            // With paths, a commit that does not change them stands for
            // the parent it equals.
            let mut same_as = None;
            if let Some(ps) = &pathspec {
                let tree = self.tree_of(&c).ok();
                let empty = rgit_hash::hash_object(self.hash, "tree", b"").ok();
                let bases: Vec<Option<ObjectId>> = if parents.is_empty() {
                    vec![None]
                } else {
                    parents.iter().map(|p| Some(*p)).collect()
                };
                for p in bases {
                    let pt = match p {
                        Some(p) => self.tree_of(&p).ok(),
                        None => empty,
                    };
                    let (Some(t), Some(pt)) = (tree, pt) else {
                        continue;
                    };
                    let mut d = TreeDiff {
                        repo: &self.repo,
                        pathspec: ps,
                        recursive: true,
                        show_trees: false,
                        max_depth: None,
                        pairs: Vec::new(),
                    };
                    if d.walk(Some(&pt), Some(&t), b"").is_ok() && d.pairs.is_empty() {
                        same_as = Some(p);
                        break;
                    }
                }
            }
            if same_as.is_none() && self.marks.contains_key(&c) {
                break Some(c);
            }
            let next = match same_as {
                Some(Some(p)) => p,
                Some(None) => break None,
                None => match parents.first() {
                    Some(p) => *p,
                    None => break None,
                },
            };
            c = next;
        };
        self.pathspec = pathspec.or_else(|| self.pathspec.take());
        found
    }

    fn write_tag(&mut self, oid: &ObjectId, name: &str, walk: &crate::history::ExportWalk) -> Result<(), Die> {
        let (_, data) = self.read(oid)?;
        let tag = Tag::parse(self.hash, &data).map_err(|_| Die::Fatal(msg::could_not_read(&oid.to_hex())))?;
        let target = tag
            .object()
            .map_err(|_| Die::Fatal(msg::could_not_read(&oid.to_hex())))?;
        let (target_kind, _) = self.read(&target)?;
        if target_kind == ObjectKind::Tag {
            // A chain of tags ending at a tree is left out.
            let mut cur = target;
            for _ in 0..64 {
                let Ok((k, d)) = self.read(&cur) else { break };
                match k {
                    ObjectKind::Tag => match Tag::parse(self.hash, &d).and_then(|t| t.object()) {
                        Ok(next) => cur = next,
                        Err(_) => break,
                    },
                    ObjectKind::Tree => {
                        eprintln!("warning: {}", msg::omitting_tree_tag(&oid.to_hex()));
                        return Ok(());
                    }
                    _ => break,
                }
            }
        }
        let mut from = None;
        let mut wrote_inner = false;
        match target_kind {
            ObjectKind::Tree => {
                eprintln!("warning: {}", msg::omitting_tree_tag(&oid.to_hex()));
                return Ok(());
            }
            ObjectKind::Tag => {
                // The tagged tag goes first, under the same name.
                if !self.marks.contains_key(&target) {
                    self.write_tag(&target, name, walk)?;
                    wrote_inner = true;
                }
                if let Some(m) = self.marks.get(&target) {
                    from = Some(format!(":{m}"));
                }
            }
            _ => {
                if let Some(m) = self.marks.get(&target) {
                    from = Some(format!(":{m}"));
                }
            }
        }
        let from = match from {
            Some(f) => f,
            None => match self.filtered {
                Filtered::Abort => return Err(Die::Fatal(msg::tags_unexported(&oid.to_hex()))),
                Filtered::Drop => return Ok(()),
                Filtered::Rewrite => {
                    if target_kind != ObjectKind::Commit {
                        return Err(Die::Fatal(msg::tags_unexported(&oid.to_hex())));
                    }
                    if walk.paths.is_empty() {
                        // Excluded by the revisions (not by paths): name
                        // it by id.
                        target.to_hex()
                    } else {
                        match self.rewritten(&target, walk) {
                            Some(c) => format!(":{}", self.marks[&c]),
                            None => {
                                // Nothing to tag: the tag is deleted.
                                let short = name.strip_prefix("refs/tags/").unwrap_or(name);
                                let shown = self.ref_name(&format!("refs/tags/{short}"));
                                let zero = ObjectId::null(self.hash);
                                let s = format!("reset {shown}\nfrom {zero}\n\n");
                                self.out.write_all(s.as_bytes()).map_err(w)?;
                                return Ok(());
                            }
                        }
                    }
                }
            },
        };
        if wrote_inner {
            // The name was set to the inner tag on the way.
            let shown = self.ref_name(name);
            let zero = ObjectId::null(self.hash);
            let s = format!("reset {shown}\nfrom {zero}\n\n");
            self.out.write_all(s.as_bytes()).map_err(w)?;
        }
        let mut message = tag.message().to_vec();
        let (body, sig) = rgit_object::split_signature(&message);
        if sig.is_some() {
            let body_len = body.len();
            match self.signed_tags {
                Signed::Abort => return Err(Die::Fatal(msg::signed_tag_abort(&oid.to_hex()))),
                Signed::WarnVerbatim => {
                    eprintln!("warning: {}", msg::exporting_signed_tag(&oid.to_hex()));
                }
                Signed::Verbatim => {}
                Signed::WarnStrip => {
                    eprintln!("warning: {}", msg::stripping_tag_signature(&oid.to_hex()));
                    message.truncate(body_len);
                }
                Signed::Strip => message.truncate(body_len),
            }
        }
        let short = name.strip_prefix("refs/tags/").unwrap_or(name);
        let shown = self.ref_name(&format!("refs/tags/{short}"));
        let shown = shown.strip_prefix("refs/tags/").unwrap_or(&shown).to_string();
        let mut b = format!("tag {shown}\n").into_bytes();
        if self.mark_tags {
            let m = self.next_mark(*oid);
            b.extend_from_slice(format!("mark :{m}\n").as_bytes());
        }
        b.extend_from_slice(format!("from {from}\n").as_bytes());
        if self.show_original_ids {
            b.extend_from_slice(format!("original-oid {oid}\n").as_bytes());
        }
        match tag.tagger_raw() {
            Some(t) => {
                let t = match &mut self.anon {
                    Some(a) => a.ident(t),
                    None => t.to_vec(),
                };
                b.extend_from_slice(b"tagger ");
                b.extend_from_slice(&t);
                b.push(b'\n');
            }
            None if self.fake_missing_tagger => {
                b.extend_from_slice(format!("tagger {}\n", msg::FAKE_TAGGER).as_bytes());
            }
            None => {}
        }
        if let Some(a) = &mut self.anon {
            let n = a.tag_messages.len();
            let n = *a.tag_messages.entry(message.clone()).or_insert(n);
            message = format!("tag message {n}").into_bytes();
        }
        b.extend_from_slice(format!("data {}\n", message.len()).as_bytes());
        b.extend_from_slice(&message);
        b.push(b'\n');
        self.out.write_all(&b).map_err(w)?;
        Ok(())
    }

    /// `--refspec :<ref>`: delete the ref.
    fn refspec_deletions(&mut self) -> Result<(), Die> {
        let zero = ObjectId::null(self.hash);
        let dels: Vec<String> = self
            .refspecs
            .iter()
            .filter(|s| s.src.is_empty())
            .filter_map(|s| s.dst.as_ref().map(|d| String::from_utf8_lossy(d).into_owned()))
            .collect();
        for d in dels {
            let s = format!("reset {d}\nfrom {zero}\n\n");
            self.out.write_all(s.as_bytes()).map_err(w)?;
        }
        Ok(())
    }
}
