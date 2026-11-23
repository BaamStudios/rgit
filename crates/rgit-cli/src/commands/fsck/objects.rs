// SPDX-License-Identifier: MIT
//! Reading and checking every object of the repository for `fsck`.

use std::io::Write;
use std::path::{Path, PathBuf};

use rgit_hash::ObjectId;
use rgit_object::fsck::{self, Control, MsgId, Severity, SpecialFile};
use rgit_object::{ObjectKind, TreeIter};
use rgit_odb::{LooseOptions, LooseStore};
use rgit_pack::{DeltaBaseCache, PackSet};

use super::{ERROR_OBJECT, ERROR_PACK, Fsck, Link};
use crate::messages::fsck as msg;
use crate::repo_objects::Repo;

/// Largest `.gitattributes` blob git parses (100 MiB).
const MAX_ATTRIBUTES_SIZE: usize = 100 << 20;
/// Longest `.gitattributes` line git parses.
const MAX_ATTRIBUTES_LINE: usize = 2048;

/// The loose object files below `dir`, sorted by id.
fn loose_ids(dir: &Path, hash: rgit_hash::HashKind) -> Vec<(ObjectId, PathBuf)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut subdirs: Vec<(String, PathBuf)> = rd
        .filter_map(Result::ok)
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .filter(|(n, _)| n.len() == 2 && n.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
        .collect();
    subdirs.sort();
    for (prefix, sub) in subdirs {
        let Ok(rd) = std::fs::read_dir(&sub) else {
            continue;
        };
        let mut names: Vec<(String, PathBuf)> = rd
            .filter_map(Result::ok)
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .collect();
        names.sort();
        for (name, path) in names {
            if name.len() + 2 != hash.hex_len() {
                continue;
            }
            if let Ok(oid) = ObjectId::from_hex(hash, format!("{prefix}{name}").as_bytes()) {
                out.push((oid, path));
            }
        }
    }
    out
}

/// The object directories to check: the repository's own, then (with
/// `--full`) its alternates.
fn object_dirs(f: &Fsck, repo: &Repo) -> Vec<PathBuf> {
    let mut dirs = vec![crate::repo_objects::objects_dir(&repo.setup)];
    if f.opts.full {
        dirs.extend(repo.alternate_dirs());
    }
    dirs
}

/// Read, verify and check every object.
pub(crate) fn scan(f: &mut Fsck, repo: &Repo) {
    let dirs = object_dirs(f, repo);
    if f.opts.verbose {
        eprintln!(
            "{}",
            if dirs.len() > 1 {
                msg::CHECKING_OBJECT_DIRECTORIES
            } else {
                msg::CHECKING_OBJECT_DIRECTORY
            }
        );
    }
    // Like git, the meters follow the checks of object contents: none
    // with --connectivity-only.
    let progress = f.opts.progress && !f.opts.connectivity_only;
    for dir in &dirs {
        let store = LooseStore::new(dir, f.hash, LooseOptions::default());
        // One step per fan-out directory, shown as its objects are read.
        let mut meter = progress
            .then(|| rgit_progress::Progress::start(msg::CHECKING_OBJECT_DIRECTORIES, 256, true));
        for (oid, path) in loose_ids(dir, f.hash) {
            if let Some(m) = &mut meter {
                m.set(u64::from(oid.as_bytes()[0]) + 1);
            }
            scan_loose(f, &store, &oid, &path);
        }
        if let Some(mut m) = meter {
            m.set(256);
            m.stop();
        }
    }
    let packs: Vec<_> = dirs
        .iter()
        .flat_map(|dir| PackSet::open(dir.join("pack"), f.hash).packs())
        .collect();
    let total: u64 = packs.iter().map(|p| p.index().len() as u64).sum();
    // The packed objects, counted pack by pack.
    let mut meter = (progress && f.opts.full && total > 0)
        .then(|| rgit_progress::Progress::start(msg::CHECKING_OBJECTS, total, true));
    let mut done = 0u64;
    if let Some(m) = &mut meter {
        m.set(0);
    }
    for pack in &packs {
        scan_pack(f, pack);
        done += pack.index().len() as u64;
        if let Some(m) = &mut meter {
            m.set(done);
        }
    }
    if let Some(m) = meter {
        m.stop();
    }
    check_special_blobs(f, repo);
}

fn scan_loose(f: &mut Fsck, store: &LooseStore, oid: &ObjectId, path: &Path) {
    let shown = path.display().to_string();
    if f.opts.connectivity_only {
        // Only the type, and the links of commits, trees and tags.
        match store.header(oid) {
            Ok(Some(h)) if h.kind == ObjectKind::Blob => {
                note_object(f, oid, ObjectKind::Blob, None);
            }
            Ok(Some(_)) => {
                if let Ok(Some(o)) = store.read(oid) {
                    note_object(f, oid, o.kind, Some(&o.data));
                }
            }
            _ => {}
        }
        return;
    }
    match store.verify(oid) {
        Ok(o) => {
            if f.opts.verbose {
                eprintln!("{}", msg::checking(&format!("{} {oid}", o.kind.name())));
            }
            note_object(f, oid, o.kind, Some(&o.data));
            check_object(f, oid, o.kind, &o.data);
        }
        Err(e) => {
            f.errors |= ERROR_OBJECT;
            let mut err = std::io::stderr().lock();
            match &e {
                rgit_odb::Error::HashMismatch { actual, .. } => {
                    let _ = writeln!(err, "error: {}", rgit_odb::messages::hash_path_mismatch(actual, path));
                    return;
                }
                rgit_odb::Error::InflateHeader { zlib, .. } => {
                    let _ = writeln!(err, "error: {}", rgit_odb::messages::inflate_data_error(zlib));
                    let _ = writeln!(err, "error: {}", msg::unable_to_unpack_header(&shown));
                }
                rgit_odb::Error::InvalidType { found, .. } => {
                    let _ = writeln!(
                        err,
                        "error: {}",
                        msg::unable_to_parse_type(&String::from_utf8_lossy(found), &shown)
                    );
                }
                rgit_odb::Error::HeaderTooLong { .. } | rgit_odb::Error::BadHeader { .. } => {
                    let _ = writeln!(err, "error: {e}");
                    let _ = writeln!(err, "error: {}", msg::unable_to_unpack_header(&shown));
                }
                _ => {
                    for l in e.git_lines() {
                        let _ = writeln!(err, "error: {l}");
                    }
                    let _ = writeln!(err, "error: {}", msg::unable_to_unpack_contents(&shown));
                }
            }
            let _ = writeln!(err, "error: {}", msg::object_corrupt(&oid.to_string(), &shown));
        }
    }
}

fn scan_pack(f: &mut Fsck, pack: &rgit_pack::Pack) {
    let index = pack.index();
    let data = match pack.data() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            f.errors |= ERROR_PACK;
            return;
        }
    };
    if !f.opts.connectivity_only {
        if let Err(e) = index.verify_checksum().and_then(|()| data.verify_checksum()) {
            eprintln!("error: {e}");
            f.errors |= ERROR_PACK;
        }
    }
    let mut entries: Vec<(u64, u32)> = (0..index.len())
        .filter_map(|pos| index.offset_at(pos).ok().map(|off| (off, pos)))
        .collect();
    entries.sort_unstable();
    let mut cache = DeltaBaseCache::new(rgit_pack::DEFAULT_DELTA_BASE_CACHE_LIMIT);
    let promisor = pack.is_promisor();
    for (offset, pos) in entries {
        let oid = index.id_at(pos);
        if promisor {
            f.promisor.insert(oid);
            if let Ok(o) = pack.read_at(offset, &mut cache, &mut |_| None) {
                if o.kind != ObjectKind::Blob {
                    let mut tmp = Vec::new();
                    links_of(f.hash, o.kind, &o.data, &mut tmp);
                    f.promisor.extend(tmp);
                }
            }
        }
        if f.opts.connectivity_only {
            match pack.object_info(offset) {
                Ok((ObjectKind::Blob, _)) => note_object(f, &oid, ObjectKind::Blob, None),
                Ok(_) => {
                    if let Ok(o) = pack.read_at(offset, &mut cache, &mut |_| None) {
                        note_object(f, &oid, o.kind, Some(&o.data));
                    }
                }
                Err(_) => {}
            }
            continue;
        }
        match pack.read_at(offset, &mut cache, &mut |_| None) {
            Ok(o) => {
                let actual = rgit_hash::hash_object(f.hash, o.kind.name(), &o.data);
                if actual.as_ref().ok() != Some(&oid) {
                    eprintln!(
                        "error: {}",
                        msg::packed_hash_mismatch(&oid.to_string(), &pack.path().display().to_string(), offset)
                    );
                    f.errors |= ERROR_OBJECT;
                    continue;
                }
                if f.opts.verbose {
                    eprintln!("{}", msg::checking(&format!("{} {oid}", o.kind.name())));
                }
                note_object(f, &oid, o.kind, Some(&o.data));
                check_object(f, &oid, o.kind, &o.data);
            }
            Err(e) => {
                for l in e.chain() {
                    eprintln!("error: {l}");
                }
                f.errors |= ERROR_OBJECT;
            }
        }
    }
}

/// Record that `oid` exists with type `kind`, and its links.
pub(crate) fn note_object(f: &mut Fsck, oid: &ObjectId, kind: ObjectKind, data: Option<&[u8]>) {
    let hash = f.hash;
    let mut links = Vec::new();
    let mut broken: Option<String> = None;
    if let Some(data) = data {
        match kind {
            ObjectKind::Tree => {
                for e in TreeIter::new(hash, data) {
                    match e {
                        Ok(e) => {
                            let (kind, slash) = match e.mode_bits & 0o170000 {
                                0o040000 => (ObjectKind::Tree, "/"),
                                // Submodule commits live elsewhere.
                                0o160000 => continue,
                                _ => (ObjectKind::Blob, ""),
                            };
                            links.push(Link {
                                oid: e.oid,
                                kind,
                                path: Some(format!("{}{slash}", String::from_utf8_lossy(e.name))),
                            });
                            if e.mode_bits & 0o170000 != 0o120000 {
                                if let Some(s @ (SpecialFile::Gitattributes | SpecialFile::Gitmodules)) =
                                    fsck::special_file(e.name)
                                {
                                    f.special.push((e.oid, s));
                                }
                            }
                        }
                        Err(err) => {
                            broken = Some(err.to_string());
                            break;
                        }
                    }
                }
            }
            ObjectKind::Commit => {
                if let Ok(c) = rgit_object::Commit::parse(hash, data) {
                    if let Ok(t) = c.tree() {
                        links.push(Link {
                            oid: t,
                            kind: ObjectKind::Tree,
                            path: None,
                        });
                    }
                    for p in c.parents().unwrap_or_default() {
                        links.push(Link {
                            oid: p,
                            kind: ObjectKind::Commit,
                            path: None,
                        });
                    }
                    if f.opts.root && c.parents().is_ok_and(|p| p.is_empty()) {
                        println!("{}", msg::root(&oid.to_string()));
                    }
                }
            }
            ObjectKind::Tag => {
                if let Ok(t) = rgit_object::Tag::parse(hash, data) {
                    if let (Ok(target), Ok(k)) = (t.object(), t.target_kind()) {
                        links.push(Link {
                            oid: target,
                            kind: k,
                            path: None,
                        });
                        if f.opts.tags {
                            let name = t.name().map(|n| String::from_utf8_lossy(n).into_owned());
                            println!(
                                "{}",
                                msg::tagged(k.name(), &target.to_string(), name.as_deref().unwrap_or(""), &oid.to_string())
                            );
                        }
                    }
                }
            }
            ObjectKind::Blob => {}
        }
    }
    if let Some(cause) = &broken {
        eprintln!("error: {cause}");
        if !f.opts.connectivity_only {
            eprintln!("{}", msg::broken_links(kind.name(), &oid.to_string()));
            f.errors |= ERROR_OBJECT;
        }
    }
    // An object is known with one type; a link that wants another one
    // is broken.
    let mut mismatch = false;
    for l in &links {
        let t = f.obj(&l.oid);
        t.used = true;
        match t.kind {
            None => t.kind = Some(l.kind),
            Some(k) if k != l.kind => {
                eprintln!("error: {}", msg::type_mismatch(&l.oid.to_string(), k.name(), l.kind.name()));
                mismatch = true;
            }
            Some(_) => {}
        }
    }
    if mismatch && broken.is_none() {
        eprintln!("{}", msg::broken_links(kind.name(), &oid.to_string()));
        f.errors |= ERROR_OBJECT;
    }
    let o = f.obj(oid);
    if o.has {
        return;
    }
    if let Some(expected) = o.kind.filter(|&k| k != kind) {
        eprintln!("error: {}", msg::type_mismatch(&oid.to_string(), expected.name(), kind.name()));
        f.errors |= ERROR_OBJECT;
    }
    let o = f.obj(oid);
    o.has = true;
    o.kind = Some(kind);
    o.links = links;
}

/// Run the object checks and print the findings.
fn check_object(f: &mut Fsck, oid: &ObjectId, kind: ObjectKind, data: &[u8]) {
    if f.skip.contains(oid) {
        return;
    }
    let mut lines = Vec::new();
    let mut failed = false;
    let options = fsck::Options {
        max_pathname_len: f.max_pathname_len,
        allow_group_writable: !f.opts.strict,
    };
    // In a tag, the check of the header after `tagger` runs after an
    // error too, and its verdict replaces the earlier ones (as in git).
    let mut extra_header: Option<bool> = None;
    let is_tag = kind == ObjectKind::Tag;
    fsck::check_object(kind, f.hash, data, &options, &mut |finding| {
        let level = f.severity(finding.id);
        if finding.id == MsgId::ExtraHeaderEntry {
            extra_header = Some(matches!(level, Severity::Error | Severity::Fatal));
        }
        match level {
            Severity::Ignore => return Control::Continue,
            Severity::Error | Severity::Fatal => {
                if let Some(c) = &finding.cause {
                    lines.push(format!("error: {c}"));
                }
                lines.push(msg::finding("error", kind.name(), &oid.to_string(), &finding.to_string()));
                failed = true;
                return if is_tag { Control::Continue } else { Control::Stop };
            }
            _ => lines.push(msg::finding("warning", kind.name(), &oid.to_string(), &finding.to_string())),
        }
        Control::Continue
    });
    if let Some(v) = extra_header {
        failed = v;
    }
    let mut err = std::io::stderr().lock();
    for l in lines {
        let _ = writeln!(err, "{l}");
    }
    if failed {
        f.errors |= ERROR_OBJECT;
    }
}

fn report_on(f: &mut Fsck, oid: &ObjectId, kind: &str, id: MsgId, text: &str) {
    if f.skip.contains(oid) {
        return;
    }
    let level = match f.severity(id) {
        Severity::Ignore => return,
        Severity::Error | Severity::Fatal => {
            f.errors |= ERROR_OBJECT;
            "error"
        }
        _ => "warning",
    };
    eprintln!("{}", msg::finding(level, kind, &oid.to_string(), &format!("{}: {text}", id.name())));
}

/// Checks of `.gitattributes` and `.gitmodules` blobs named by trees.
fn check_special_blobs(f: &mut Fsck, repo: &Repo) {
    let special = std::mem::take(&mut f.special);
    let mut seen = std::collections::HashSet::new();
    for (oid, which) in special {
        if !seen.insert((oid, which)) || f.opts.connectivity_only {
            continue;
        }
        let Ok(Some(o)) = repo.odb.read(&oid) else {
            continue;
        };
        if o.kind != ObjectKind::Blob {
            let (id, text) = match which {
                SpecialFile::Gitmodules => (MsgId::GitmodulesBlob, msg::GITMODULES_BLOB),
                _ => (MsgId::GitattributesBlob, msg::GITATTRIBUTES_BLOB),
            };
            report_on(f, &oid, o.kind.name(), id, text);
            continue;
        }
        if which == SpecialFile::Gitmodules {
            for (id, text) in super::gitmodules::check(&o.data) {
                report_on(f, &oid, "blob", id, &text);
            }
            continue;
        }
        if o.data.len() > MAX_ATTRIBUTES_SIZE {
            report_on(f, &oid, "blob", MsgId::GitattributesLarge, msg::GITATTRIBUTES_LARGE);
        } else if o.data.split(|&b| b == b'\n').any(|l| l.len() >= MAX_ATTRIBUTES_LINE) {
            report_on(f, &oid, "blob", MsgId::GitattributesLineLength, msg::GITATTRIBUTES_LINE_LENGTH);
        }
    }
}

/// The ids an object refers to (for promisor objects).
fn links_of(hash: rgit_hash::HashKind, kind: ObjectKind, data: &[u8], out: &mut Vec<ObjectId>) {
    match kind {
        ObjectKind::Tree => {
            for e in TreeIter::new(hash, data) {
                let Ok(e) = e else { break };
                if e.mode_bits & 0o170000 != 0o160000 {
                    out.push(e.oid);
                }
            }
        }
        ObjectKind::Commit => {
            if let Ok(c) = rgit_object::Commit::parse(hash, data) {
                out.extend(c.tree());
                out.extend(c.parents().unwrap_or_default());
            }
        }
        ObjectKind::Tag => {
            if let Ok(t) = rgit_object::Tag::parse(hash, data) {
                out.extend(t.object());
            }
        }
        ObjectKind::Blob => {}
    }
}
