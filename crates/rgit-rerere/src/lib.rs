// SPDX-License-Identifier: MIT
//! Reuse recorded resolution of conflicted merges (`git rerere`).
//!
//! Written from Documentation/git-rerere.adoc,
//! Documentation/technical/rerere.adoc, Documentation/rerere-options.adoc
//! and Documentation/config/rerere.adoc; the details the documentation
//! leaves open (variants, the order of the steps, which files are written
//! and removed when) were taken from the behaviour of the `git` binary, so
//! that `rr-cache` directories of git and rgit are interchangeable.
//!
//! - [`normalize()`]: the normalised conflict (`preimage`) and its id;
//! - [`Cache`], [`parse_merge_rr`], [`format_merge_rr`]: the `rr-cache`
//!   directory and `MERGE_RR`;
//! - [`rerere()`]: one pass after a conflicted merge or before a commit:
//!   records preimages of new conflicts, replays recorded resolutions and
//!   records the resolutions of resolved conflicts;
//! - [`forget()`], [`clear()`], [`gc()`], [`diff()`], [`remaining()`]: the
//!   subcommands of `git rerere`.
//!
//! The library does not know the index or the work tree: the caller
//! provides them through [`Host`].

mod cache;
pub mod messages;
mod normalize;

pub use cache::{Cache, CorruptMergeRr, Image, RrEntry, format_merge_rr, parse_merge_rr};
pub use normalize::{Normalized, ParseError, normalize};

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use rgit_hash::HashKind;

/// A conflicted path of the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflicted {
    pub path: Vec<u8>,
    /// Both sides (stages 2 and 3) are regular files: rerere can record
    /// the conflict. Other conflicts (deleted on one side, symbolic links,
    /// submodules) are left to the user.
    pub trackable: bool,
}

/// What rerere needs from the repository.
pub trait Host {
    /// The conflicted paths of the index, sorted.
    fn conflicts(&mut self) -> Vec<Conflicted>;
    /// The work tree file at `path`.
    fn read(&mut self, path: &[u8]) -> std::io::Result<Vec<u8>>;
    /// Replaces the work tree file at `path`.
    fn write(&mut self, path: &[u8], data: &[u8]) -> std::io::Result<()>;
    /// The length of the conflict markers of `path`
    /// (`conflict-marker-size`).
    fn marker_size(&mut self, path: &[u8]) -> usize;
    /// Adds the work tree files at `paths` to the index (`rerere.autoUpdate`).
    fn stage(&mut self, paths: &[Vec<u8>]) -> Result<(), String>;
    /// Prints one line of output (standard error).
    fn report(&mut self, line: &str);
}

enum Parsed {
    Conflict(Normalized),
    Clean(Vec<u8>),
    Failed,
}

fn parse(host: &mut dyn Host, path: &[u8], kind: HashKind) -> Parsed {
    let data = match host.read(path) {
        Ok(d) => d,
        Err(e) => {
            host.report(&messages::could_not_open(path, &strerror(&e)));
            return Parsed::Failed;
        }
    };
    let size = host.marker_size(path);
    match normalize(&data, size, kind) {
        Ok(n) if n.hunks > 0 => Parsed::Conflict(n),
        Ok(_) => Parsed::Clean(data),
        Err(ParseError) => {
            host.report(&messages::could_not_parse(path));
            Parsed::Failed
        }
    }
}

/// The C library's text for an I/O error (without the " (os error N)").
fn strerror(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error") {
        Some(i) => s[..i].to_owned(),
        None => s,
    }
}

/// Merges the recorded resolution of a variant into the current conflict;
/// `None` when the variant does not apply cleanly.
fn replay(cache: &Cache, id: &str, variant: u32, current: &[u8]) -> Option<Vec<u8>> {
    let _ = cache.write(id, variant, Image::This, current);
    let pre = cache.read(id, variant, Image::Pre).ok()?;
    let post = cache.read(id, variant, Image::Post).ok()?;
    let opts = rgit_diff::merge::MergeOptions {
        diff: rgit_diff::merge::merge_diff_options(),
        ..Default::default()
    };
    let r = rgit_diff::merge::merge(&pre, current, &post, &opts).ok()?;
    r.is_clean().then_some(r.content)
}

/// The variants of `id` with a recorded resolution, in order.
fn resolved_variants(cache: &Cache, id: &str) -> Vec<u32> {
    cache
        .variants(id)
        .into_iter()
        .filter(|&v| cache.has(id, v, Image::Pre) && cache.has(id, v, Image::Post))
        .collect()
}

/// One rerere pass (`git rerere` without a subcommand, and what merge
/// commands run after a conflict and `commit` before committing).
///
/// For each path, in path order:
/// - a path of `MERGE_RR` whose file has no conflict any more gets its
///   resolution recorded (`postimage`);
/// - a path of `MERGE_RR` that is still conflicted loses its unresolved
///   variant and is handled like a new conflict;
/// - a new conflict is resolved with the first recorded resolution of its
///   id that applies cleanly, else its preimage is recorded in the first
///   free variant and the path goes to `MERGE_RR`.
///
/// With `autoupdate` the files resolved from recorded resolutions are
/// added to the index.
///
/// # Errors
/// Writing to the `rr-cache` failed.
pub fn rerere(
    cache: &Cache,
    rr: &mut Vec<RrEntry>,
    host: &mut dyn Host,
    kind: HashKind,
    autoupdate: bool,
) -> std::io::Result<()> {
    let trackable: BTreeSet<Vec<u8>> = host
        .conflicts()
        .into_iter()
        .filter(|c| c.trackable)
        .map(|c| c.path)
        .collect();
    let mut old: BTreeMap<Vec<u8>, RrEntry> = rr.drain(..).map(|e| (e.path.clone(), e)).collect();
    let paths: BTreeSet<Vec<u8>> = trackable.iter().chain(old.keys()).cloned().collect();

    let mut parsed: BTreeMap<Vec<u8>, Parsed> = BTreeMap::new();
    for p in &paths {
        parsed.insert(p.clone(), parse(host, p, kind));
    }

    let mut kept: Vec<RrEntry> = Vec::new();
    let mut staged: Vec<Vec<u8>> = Vec::new();
    for p in &paths {
        let state = parsed.remove(p).unwrap_or(Parsed::Failed);
        if let Some(e) = old.remove(p) {
            if let Parsed::Clean(data) = &state {
                cache.write(&e.id, e.variant, Image::Post, data)?;
                host.report(&messages::recorded_resolution(p));
                continue;
            }
            if !cache.has(&e.id, e.variant, Image::Post) {
                cache.remove(&e.id, e.variant, Image::Pre);
            }
        }
        if !trackable.contains(p) {
            continue;
        }
        let Parsed::Conflict(n) = state else {
            continue;
        };
        let Some(id) = n.id.as_ref().map(|i| i.to_hex()) else {
            continue;
        };
        let mut done = false;
        for v in resolved_variants(cache, &id) {
            if let Some(result) = replay(cache, &id, v, &n.image) {
                host.write(p, &result)?;
                cache.touch(&id, v, Image::Post);
                if autoupdate {
                    staged.push(p.clone());
                    host.report(&messages::staged(p));
                } else {
                    host.report(&messages::resolved(p));
                }
                done = true;
                break;
            }
        }
        if done {
            continue;
        }
        let v = cache.free_variant(&id);
        cache.write(&id, v, Image::Pre, &n.image)?;
        host.report(&messages::recorded_preimage(p));
        kept.push(RrEntry {
            path: p.clone(),
            id,
            variant: v,
        });
    }
    if !staged.is_empty() {
        host.stage(&staged).map_err(std::io::Error::other)?;
    }
    *rr = kept;
    Ok(())
}

/// `git rerere forget`: for each `(path, conflict)`, the conflict as the
/// merge of its index stages left it, drops the recorded resolution that
/// applies to it and records the conflict again (the path goes to
/// `MERGE_RR`, so that the next resolution is recorded).
///
/// # Errors
/// Writing to the `rr-cache` failed.
pub fn forget(
    cache: &Cache,
    rr: &mut Vec<RrEntry>,
    host: &mut dyn Host,
    kind: HashKind,
    conflicts: &[(Vec<u8>, Vec<u8>)],
) -> std::io::Result<()> {
    for (path, text) in conflicts {
        let size = host.marker_size(path);
        let Ok(n) = normalize(text, size, kind) else {
            host.report(&messages::could_not_parse(path));
            continue;
        };
        let Some(id) = n.id.as_ref().map(|i| i.to_hex()) else {
            continue;
        };
        let found = resolved_variants(cache, &id)
            .into_iter()
            .find(|&v| replay(cache, &id, v, &n.image).is_some());
        let Some(v) = found else {
            host.report(&messages::no_remembered(path));
            continue;
        };
        cache.remove(&id, v, Image::Post);
        cache.write(&id, v, Image::Pre, &n.image)?;
        host.report(&messages::updated_preimage(path));
        host.report(&messages::forgot(path));
        rr.retain(|e| &e.path != path);
        rr.push(RrEntry {
            path: path.clone(),
            id,
            variant: v,
        });
    }
    rr.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(())
}

/// `git rerere clear`: drops the unresolved variants of `MERGE_RR`.
pub fn clear(cache: &Cache, rr: &[RrEntry]) {
    for e in rr {
        if !cache.has(&e.id, e.variant, Image::Post) {
            cache.remove_variant(&e.id, e.variant);
        }
    }
}

/// `git rerere status`: the paths whose resolution will be recorded.
pub fn status(rr: &[RrEntry]) -> Vec<Vec<u8>> {
    rr.iter().map(|e| e.path.clone()).collect()
}

/// `git rerere remaining`: the paths of `MERGE_RR` and the conflicts
/// rerere cannot handle, in path order.
pub fn remaining(rr: &[RrEntry], conflicts: &[Conflicted]) -> Vec<Vec<u8>> {
    let mut out: BTreeSet<Vec<u8>> = rr.iter().map(|e| e.path.clone()).collect();
    out.extend(
        conflicts
            .iter()
            .filter(|c| !c.trackable)
            .map(|c| c.path.clone()),
    );
    out.into_iter().collect()
}

/// `git rerere diff`: the changes from each recorded preimage to the work
/// tree file, as `--- a/<path>`, `+++ b/<path>` and unified hunks.
pub fn diff(cache: &Cache, rr: &[RrEntry], host: &mut dyn Host) -> Vec<u8> {
    let mut out = Vec::new();
    let opts = rgit_diff::DiffOptions {
        indent_heuristic: false,
        funcname: Some(rgit_diff::funcname::FuncName::new(Vec::new())),
        ..Default::default()
    };
    for e in rr {
        let Ok(pre) = cache.read(&e.id, e.variant, Image::Pre) else {
            continue;
        };
        let cur = host.read(&e.path).unwrap_or_default();
        out.extend_from_slice(b"--- a/");
        out.extend_from_slice(&e.path);
        out.extend_from_slice(b"\n+++ b/");
        out.extend_from_slice(&e.path);
        out.push(b'\n');
        out.extend_from_slice(&rgit_diff::diff_text(&pre, &cur, &opts).hunks);
    }
    out
}

/// `git rerere gc`: removes the variants whose resolution is older than
/// `resolved` or which are unresolved and older than `unresolved`
/// (seconds since the epoch). Directories that are not conflict ids of
/// `kind` are left alone.
pub fn gc(cache: &Cache, kind: HashKind, resolved: u64, unresolved: u64) {
    let Ok(rd) = std::fs::read_dir(cache.root()) else {
        return;
    };
    let mut ids: Vec<String> = rd
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .filter(|n| n.len() == kind.hex_len() && n.bytes().all(|b| b.is_ascii_hexdigit()))
        .collect();
    ids.sort();
    let mtime = |p: std::path::PathBuf| -> Option<u64> {
        let t = std::fs::metadata(p).ok()?.modified().ok()?;
        Some(
            t.duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        )
    };
    for id in ids {
        for v in cache.variants(&id) {
            let (time, cutoff) = match mtime(cache.file(&id, v, Image::Post)) {
                Some(t) => (t, resolved),
                None => match mtime(cache.file(&id, v, Image::Pre)) {
                    Some(t) => (t, unresolved),
                    None => continue,
                },
            };
            if time < cutoff {
                for image in [Image::Pre, Image::Post, Image::This] {
                    cache.remove(&id, v, image);
                }
            }
        }
        let _ = std::fs::remove_dir(cache.dir(&id));
    }
}

#[cfg(test)]
mod tests;
