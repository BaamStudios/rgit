// SPDX-License-Identifier: MIT
//! `git unpack-objects` (Documentation/git-unpack-objects.adoc): read a
//! pack from standard input and store its objects as loose objects.
//!
//! The pack is parsed and resolved by `rgit_pack::index_pack` (into a
//! temporary file that is removed afterwards); every object is handed to a
//! hook as soon as its id is known and written unless it already exists.
//! `REF_DELTA` bases may come from the repository (thin packs).
//!
//! `--strict` checks every object (`--strict=<msg-id>=<severity>,...`
//! adjusts severities, `skiplist=<file>` leaves objects unchecked) and its
//! links: an object is only written when
//! everything it refers to is in the pack (and itself writable) or in the
//! repository.
//!
//! `-r` skips entries whose data does not inflate and goes on after
//! them; deltas on a skipped entry stay unresolved.
//!
//! Blobs larger than `core.bigFileThreshold` are not held in memory:
//! they are hashed while the pack is read and then copied from the
//! temporary pack file to loose objects as a stream.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::sync::Mutex;

use rgit_hash::{HashKind, ObjectId};
use rgit_object::fsck::{self, Control, MsgId, Severity};
use rgit_object::ObjectKind;
use rgit_pack::index_pack::{self as ip, BadEntry, Input, ObjectDb, ObjectHook, Options};

use crate::messages::unpack_objects as msg;
use crate::commands::fsck::StrictSettings;
use crate::prelude::*;
use crate::repo::{self, Mode};
use crate::repo_objects::Repo;

#[derive(Default)]
struct Args {
    dry_run: bool,
    quiet: bool,
    recover: bool,
    strict: bool,
    /// `--strict=<list>`: severities and skip lists.
    settings: StrictSettings,
    pack_header: Option<(u32, u32)>,
    max_input_size: Option<u64>,
}

fn usage(code: i32) -> Die {
    let text = format!("usage: {}\n", msg::USAGE);
    Die::Output(rgit_options::Exit {
        code,
        stdout: if code == 0 { text.clone() } else { String::new() },
        stderr: if code == 0 { String::new() } else { text },
    })
}

fn parse_args(argv: &[String]) -> Result<Args, Die> {
    let mut a = Args::default();
    for arg in &argv[1..] {
        let (name, value) = match arg.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (arg.as_str(), None),
        };
        match (name, value) {
            ("-n", None) => a.dry_run = true,
            ("-q", None) => a.quiet = true,
            ("-r", None) => a.recover = true,
            ("-h" | "--help-all", None) => return Err(usage(0)),
            ("--strict", v) => {
                a.strict = true;
                if let Some(v) = v {
                    a.settings.lists.push(v.to_string());
                }
            }
            ("--pack_header", Some(v)) => {
                let (ver, n) = v.split_once(',').ok_or_else(|| Die::Fatal(msg::bad(arg)))?;
                let ver = ver.parse().map_err(|_| Die::Fatal(msg::bad(arg)))?;
                let n = n.parse().map_err(|_| Die::Fatal(msg::bad(arg)))?;
                a.pack_header = Some((ver, n));
            }
            ("--max-input-size", Some(v)) => {
                a.max_input_size = Some(parse_size(v).ok_or_else(|| usage(129))?);
            }
            _ => return Err(usage(129)),
        }
    }
    Ok(a)
}

/// A size with an optional `k`, `m` or `g` suffix.
fn parse_size(s: &str) -> Option<u64> {
    let (num, mult) = match s.char_indices().last() {
        Some((i, 'k' | 'K')) => (&s[..i], 1u64 << 10),
        Some((i, 'm' | 'M')) => (&s[..i], 1 << 20),
        Some((i, 'g' | 'G')) => (&s[..i], 1 << 30),
        _ => (s, 1),
    };
    num.parse::<u64>().ok()?.checked_mul(mult)
}

/// The objects an object refers to, with the type each must have.
fn links(hash: HashKind, kind: ObjectKind, data: &[u8]) -> Vec<(ObjectId, Option<ObjectKind>)> {
    let mut out = Vec::new();
    match kind {
        ObjectKind::Tree => {
            for e in rgit_object::TreeIter::new(hash, data) {
                let Ok(e) = e else { break };
                match e.mode_bits & 0o170000 {
                    0o160000 => {}
                    0o040000 => out.push((e.oid, Some(ObjectKind::Tree))),
                    _ => out.push((e.oid, Some(ObjectKind::Blob))),
                }
            }
        }
        ObjectKind::Commit => {
            if let Ok(c) = rgit_object::Commit::parse(hash, data) {
                if let Ok(t) = c.tree() {
                    out.push((t, Some(ObjectKind::Tree)));
                }
                if let Ok(ps) = c.parents() {
                    out.extend(ps.into_iter().map(|p| (p, Some(ObjectKind::Commit))));
                }
            }
        }
        ObjectKind::Tag => {
            if let Ok(t) = rgit_object::Tag::parse(hash, data) {
                if let Ok(o) = t.object() {
                    out.push((o, t.target_kind().ok()));
                }
            }
        }
        ObjectKind::Blob => {}
    }
    out
}

/// Receives every object of the pack.
struct Sink<'a> {
    repo: &'a Repo,
    args: &'a Args,
    /// `--strict`: objects kept until their links are checked.
    held: Mutex<Vec<(ObjectId, ObjectKind, Vec<u8>)>>,
    /// The first write error.
    write_error: Mutex<Option<String>>,
    /// Large blobs to copy from the pack: id, type, size, zlib stream
    /// offset.
    large: Mutex<Vec<(ObjectId, ObjectKind, u64, u64)>>,
}

impl Sink<'_> {
    fn severity(&self, id: MsgId) -> Severity {
        self.args.settings.severity(id)
    }

    fn write(&self, kind: ObjectKind, data: &[u8]) {
        if let Err(e) = self.repo.odb.write(kind, data) {
            let mut slot = self.write_error.lock().unwrap_or_else(|p| p.into_inner());
            slot.get_or_insert_with(|| e.to_string());
        }
    }
}

impl ObjectHook for Sink<'_> {
    fn object(&self, id: &ObjectId, kind: rgit_pack::ObjectKind, data: &[u8]) -> Result<(), String> {
        let kind = ObjectKind::from_pack_type(kind.pack_type()).expect("object kind");
        if self.args.strict {
            if self.args.settings.skip.contains(id) {
                self.held
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push((*id, kind, data.to_vec()));
                return Ok(());
            }
            let mut failed = false;
            let mut lines = Vec::new();
            fsck::check_object(kind, self.repo.hash, data, &fsck::Options::default(), &mut |f| {
                match self.severity(f.id) {
                    Severity::Ignore => Control::Continue,
                    Severity::Warn | Severity::Info => {
                        lines.push(format!("warning: {}", msg::fsck_finding(id, &f)));
                        Control::Continue
                    }
                    Severity::Error | Severity::Fatal => {
                        if let Some(c) = &f.cause {
                            lines.push(format!("error: {c}"));
                        }
                        lines.push(format!("error: {}", msg::fsck_finding(id, &f)));
                        failed = true;
                        Control::Stop
                    }
                }
            });
            if !lines.is_empty() {
                let mut err = io::stderr().lock();
                for l in lines {
                    let _ = writeln!(err, "{l}");
                }
            }
            if failed {
                return Err(msg::FSCK_ERROR.to_string());
            }
            self.held
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push((*id, kind, data.to_vec()));
            return Ok(());
        }
        if !self.args.dry_run && (*id == self.repo.hash.empty_tree() || !self.repo.odb.contains(id)) {
            self.write(kind, data);
        }
        Ok(())
    }

    fn large_object(
        &self,
        id: &ObjectId,
        kind: rgit_pack::ObjectKind,
        size: u64,
        data_offset: u64,
    ) -> Result<(), String> {
        if !self.args.dry_run && !self.repo.odb.contains(id) {
            let kind = ObjectKind::from_pack_type(kind.pack_type()).expect("object kind");
            self.large
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push((*id, kind, size, data_offset));
        }
        Ok(())
    }

    fn corrupt(&self, error: &ip::Error) {
        let mut err = io::stderr().lock();
        for line in error.errors_before() {
            let _ = writeln!(err, "error: {line}");
        }
        match error {
            ip::Error::BadObject {
                what: BadEntry::UnknownType(t),
                ..
            } => {
                let _ = writeln!(err, "error: {}", msg::bad_object_type(*t));
            }
            ip::Error::BadObject { what, .. } => {
                let _ = writeln!(err, "error: {what}");
            }
            _ => {}
        }
    }
}

/// `--strict`: the `.gitmodules` blobs named by trees of the pack.
fn check_gitmodules(
    sink: &Sink<'_>,
    held: &[(ObjectId, ObjectKind, Vec<u8>)],
    in_pack: &HashMap<ObjectId, usize>,
) -> Result<(), Die> {
    let hash = sink.repo.hash;
    let mut seen = HashSet::new();
    let mut failed = false;
    for (_, kind, data) in held {
        if *kind != ObjectKind::Tree {
            continue;
        }
        for e in rgit_object::TreeIter::new(hash, data) {
            let Ok(e) = e else { break };
            if e.mode_bits & 0o170000 == 0o120000
                || fsck::special_file(e.name) != Some(fsck::SpecialFile::Gitmodules)
                || !seen.insert(e.oid)
            {
                continue;
            }
            let Some(&j) = in_pack.get(&e.oid) else {
                continue;
            };
            if held[j].1 != ObjectKind::Blob || sink.args.settings.skip.contains(&e.oid) {
                continue;
            }
            for (id, text) in crate::commands::fsck::gitmodules::check(&held[j].2) {
                let finding = format!("{}: {text}", id.name());
                match sink.severity(id) {
                    Severity::Ignore => {}
                    Severity::Warn | Severity::Info => {
                        eprintln!("warning: {}", msg::fsck_finding(&e.oid, &finding));
                    }
                    Severity::Error | Severity::Fatal => {
                        eprintln!("error: {}", msg::fsck_finding(&e.oid, &finding));
                        failed = true;
                    }
                }
            }
        }
    }
    if failed {
        return Err(Die::Fatal(msg::FSCK_ERROR_PACK.into()));
    }
    Ok(())
}

/// git's wording for the errors of a pack read by `unpack-objects`.
fn fail(e: ip::Error) -> Die {
    match e {
        ip::Error::EarlyEof => Die::Fatal(msg::EARLY_EOF.into()),
        ip::Error::SignatureMismatch => Die::Fatal(msg::BAD_PACK_FILE.into()),
        ip::Error::Version(v) => Die::Fatal(msg::unknown_version(v)),
        ip::Error::ChecksumMismatch => Die::Fatal(msg::FINAL_SHA1.into()),
        ip::Error::Unresolved(_) => Die::Fatal(msg::UNRESOLVED.into()),
        ip::Error::TooLarge(_) => Die::Fatal(msg::TOO_LARGE.into()),
        ip::Error::Hook(m) => Die::Fatal(m),
        ip::Error::BadObject {
            what: BadEntry::DeltaBaseOutOfBound,
            ..
        } => Die::Fatal(msg::OFS_OUT_OF_BOUND.into()),
        ip::Error::BadObject { ref what, .. } => {
            for line in e.errors_before() {
                eprintln!("error: {line}");
            }
            match what {
                BadEntry::UnknownType(t) => eprintln!("error: {}", msg::bad_object_type(*t)),
                other => eprintln!("error: {other}"),
            }
            Die::Exit(1)
        }
        other => {
            for line in other.errors_before() {
                eprintln!("error: {line}");
            }
            Die::Fatal(other.to_string())
        }
    }
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mut args = parse_args(argv)?;
    let setup = repo::setup(ctx, Mode::Required)?;
    let repo = Repo::open(ctx, setup)?;
    let hash = repo.hash;
    args.settings.resolve(hash)?;

    let mut opts = Options::new(hash);
    // One thread: objects reach the hook in a stable order.
    opts.threads = 1;
    opts.fix_thin = true;
    opts.pack_header = args.pack_header;
    opts.max_input_size = args.max_input_size.filter(|&n| n > 0);
    opts.allow_trailing_data = true;
    opts.recover = args.recover;
    opts.checksum_last = true;
    // A dry run keeps no blob; otherwise large blobs are streamed.
    opts.big_file_threshold = if args.dry_run && !args.strict {
        0
    } else {
        repo.odb.big_file_threshold()
    };
    let sink = Sink {
        repo: &repo,
        args: &args,
        held: Mutex::new(Vec::new()),
        write_error: Mutex::new(None),
        large: Mutex::new(Vec::new()),
    };

    let pack_dir = crate::repo_objects::objects_dir(&repo.setup).join("pack");
    let _ = std::fs::create_dir_all(&pack_dir);
    let mut tmp = rgit_fs::TempFile::with_mode(&pack_dir, rgit_fs::PACK_PREFIX, 0o444)
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let tmp_path = tmp.path().to_path_buf();
    let mut file = tmp.file_mut().try_clone().map_err(|e| Die::Fatal(e.to_string()))?;
    let indexed = {
        let stdin = io::stdin();
        let mut lock = stdin.lock();
        ip::index_pack(
            Input::Stream {
                reader: &mut lock,
                dest: &mut file,
                dest_path: &tmp_path,
            },
            &opts,
            Some(&repo.odb as &dyn ObjectDb),
            Some(&sink as &dyn ObjectHook),
            None,
        )
    };
    drop(file);
    let _ = args.quiet;
    let indexed = indexed.map_err(fail)?;
    // Large blobs: from the pack file to loose objects, as streams.
    let large = std::mem::take(&mut *sink.large.lock().unwrap_or_else(|p| p.into_inner()));
    for (id, kind, size, offset) in large {
        if repo.odb.contains(&id) {
            continue;
        }
        let mut r = rgit_pack::EntryReader::open(&tmp_path, offset, size)
            .map_err(|e| Die::Fatal(e.to_string()))?;
        repo.odb
            .loose()
            .write_stream(kind, size, &mut r)
            .map_err(|e| Die::Fatal(e.to_string()))?;
    }
    drop(tmp);
    if let Some(e) = sink.write_error.lock().unwrap_or_else(|p| p.into_inner()).take() {
        return Err(Die::Fatal(e));
    }

    if args.strict {
        let held = std::mem::take(&mut *sink.held.lock().unwrap_or_else(|p| p.into_inner()));
        // Objects appended to complete a thin pack exist already.
        let local: HashSet<ObjectId> = indexed
            .objects()
            .iter()
            .rev()
            .take(indexed.local_objects() as usize)
            .map(|o| o.id)
            .collect();
        let in_pack: HashMap<ObjectId, usize> =
            held.iter().enumerate().map(|(i, o)| (o.0, i)).collect();
        check_gitmodules(&sink, &held, &in_pack)?;
        // An object is good when every link is in the repository with the
        // right type, or a good object of the pack of the right type.
        let link_lists: Vec<Vec<(ObjectId, Option<ObjectKind>)>> =
            held.iter().map(|(_, k, d)| links(hash, *k, d)).collect();
        let mut good: Vec<bool> = link_lists
            .iter()
            .map(|ls| {
                ls.iter().all(|&(target, want)| match in_pack.get(&target) {
                    Some(&j) => want.is_none_or(|w| w == held[j].1),
                    None => {
                        local.contains(&target)
                            || matches!(repo.odb.header(&target),
                                Ok(Some(h)) if want.is_none_or(|w| w == h.kind))
                    }
                })
            })
            .collect();
        let any_bad = good.iter().any(|g| !g);
        // A bad object makes everything that refers to it bad.
        loop {
            let mut changed = false;
            for i in 0..held.len() {
                if good[i]
                    && link_lists[i]
                        .iter()
                        .any(|(t, _)| in_pack.get(t).is_some_and(|&j| !good[j]))
                {
                    good[i] = false;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        if !args.dry_run {
            for (i, (id, kind, data)) in held.iter().enumerate() {
                if good[i] && (*id == hash.empty_tree() || !repo.odb.contains(id)) {
                    sink.write(*kind, data);
                }
            }
        }
        if any_bad {
            return Err(Die::Fatal(msg::UNEXPECTED_TYPE.into()));
        }
    }
    // Input read past the pack belongs to the caller.
    let mut out = io::stdout().lock();
    let _ = out.write_all(indexed.trailing_data());
    let _ = out.flush();
    Ok(0)
}
