// SPDX-License-Identifier: MIT
//! `git pack-redundant` (Documentation/git-pack-redundant.adoc): the local
//! packs whose objects are all in other packs. Deprecated: it only runs
//! with `--i-still-use-this` (as git 2.56).
//!
//! Packs with an object no other local pack has are kept; then, as long
//! as objects are missing, the pack bringing the most of them (the one
//! with more objects on a tie). Objects named on standard input are
//! ignored, and objects in packs of alternate object directories count as
//! present (for the packs to keep only with `--alt-odb`). The choice was
//! determined from the behaviour of the `git` binary.

use std::collections::{BTreeSet, HashSet};
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use rgit_hash::{HashKind, ObjectId};
use rgit_pack::PackIndex;

use crate::messages::deprecated as dmsg;
use crate::messages::pack_redundant as msg;
use crate::prelude::*;
use crate::repo::{self, Mode};

struct Pack {
    /// The `.pack` file as shown.
    pack_path: PathBuf,
    idx_path: PathBuf,
    objects: BTreeSet<ObjectId>,
    /// Bytes of the pack and its index.
    size: u64,
    mtime: std::time::SystemTime,
}

fn usage_error() -> Die {
    eprint!("{}", msg::USAGE);
    Die::Exit(rgit_options::EXIT_USAGE)
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let args = &argv[1..];
    if args.len() == 1 && matches!(args[0].as_str(), "-h" | "--help-all") {
        print!("{}", msg::USAGE);
        return Ok(0);
    }
    let mut still_use = false;
    let mut all = false;
    let mut verbose = false;
    let mut alt_odb = false;
    let mut files: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--" => {
                i += 1;
                break;
            }
            "--all" => all = true,
            "--verbose" => verbose = true,
            "--alt-odb" => alt_odb = true,
            a if a == dmsg::STILL_USE_THIS => still_use = true,
            a if a.starts_with('-') => {
                if still_use || args.iter().any(|x| x == dmsg::STILL_USE_THIS) {
                    return Err(usage_error());
                }
                break;
            }
            _ => break,
        }
        i += 1;
    }
    files.extend(args[i.min(args.len())..].iter().map(String::as_str));
    if !still_use && !args.iter().any(|a| a == dmsg::STILL_USE_THIS) {
        eprint!("{}", dmsg::removal_notice("pack-redundant", ""));
        return Err(Die::Fatal(dmsg::REFUSING_TO_RUN.to_owned()));
    }
    let setup = repo::setup(ctx, Mode::Required)?;
    let kind = setup.format.object_format;
    let objects_dir = rgit_odb::objects_dir(&setup.common_dir);

    // Objects to ignore, from standard input.
    let mut ignored = HashSet::new();
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        let mut lock = stdin.lock();
        let mut line = String::new();
        loop {
            line.clear();
            match lock.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let hex = line.trim_end_matches('\n');
            match ObjectId::from_hex(kind, hex.as_bytes()) {
                Ok(id) => {
                    ignored.insert(id);
                }
                Err(_) => return Err(Die::Fatal(msg::bad_object_id(&line))),
            }
        }
    }

    let local = load_packs(&objects_dir, kind);
    let mut alt = Vec::new();
    for dir in rgit_odb::resolve_alternates(&objects_dir) {
        alt.extend(load_packs(&dir, kind));
    }

    let local: Vec<Pack> = if all {
        local
    } else {
        let mut chosen: Vec<Pack> = Vec::new();
        let mut local = local;
        for f in &files {
            let stem = f.strip_suffix(".pack").unwrap_or(f);
            let base = f.rsplit('/').next().unwrap_or(f);
            let hexlen = kind.hex_len();
            let valid = base
                .strip_prefix("pack-")
                .is_some_and(|r| r.len() >= hexlen && r.as_bytes()[..hexlen].iter().all(u8::is_ascii_hexdigit));
            if !valid {
                return Err(Die::Fatal(msg::bad_pack_filename(f)));
            }
            let found = local.iter().position(|p| {
                let shown = p.pack_path.to_string_lossy();
                shown.strip_suffix(".pack").unwrap_or(&shown).ends_with(stem)
            });
            match found {
                Some(n) => chosen.push(local.remove(n)),
                None => {
                    if !chosen.iter().any(|p| {
                        let shown = p.pack_path.to_string_lossy();
                        shown.strip_suffix(".pack").unwrap_or(&shown).ends_with(stem)
                    }) {
                        return Err(Die::Fatal(msg::not_in_packed_git(f)));
                    }
                }
            }
        }
        chosen
    };
    if local.is_empty() {
        return Err(Die::Fatal(msg::ZERO_PACKS.to_owned()));
    }

    let alt_objects: HashSet<ObjectId> = alt.iter().flat_map(|p| p.objects.iter().copied()).collect();
    // What each pack holds that counts.
    let base: Vec<BTreeSet<ObjectId>> = local
        .iter()
        .map(|p| {
            p.objects
                .iter()
                .filter(|o| !ignored.contains(*o) && !(alt_odb && alt_objects.contains(*o)))
                .copied()
                .collect()
        })
        .collect();
    // Packs with an object no other pack has.
    let essential: Vec<bool> = (0..local.len())
        .map(|i| {
            base[i]
                .iter()
                .any(|o| !(0..local.len()).any(|j| j != i && base[j].contains(o)))
        })
        .collect();
    let remaining: Vec<BTreeSet<ObjectId>> = base
        .iter()
        .map(|s| s.iter().filter(|o| !alt_objects.contains(*o)).copied().collect())
        .collect();
    let all_objects: BTreeSet<ObjectId> = remaining.iter().flatten().copied().collect();

    // The kept packs, the last chosen first.
    let mut kept: Vec<usize> = Vec::new();
    let mut covered: HashSet<ObjectId> = HashSet::new();
    for (i, &e) in essential.iter().enumerate() {
        if e {
            kept.insert(0, i);
            covered.extend(remaining[i].iter().copied());
        }
    }
    loop {
        let mut best: Option<(usize, usize)> = None;
        for i in 0..local.len() {
            if kept.contains(&i) {
                continue;
            }
            let new = remaining[i].iter().filter(|o| !covered.contains(*o)).count();
            if new == 0 {
                continue;
            }
            let better = match best {
                None => true,
                Some((b, bn)) => {
                    new > bn || (new == bn && local[i].objects.len() > local[b].objects.len())
                }
            };
            if better {
                best = Some((i, new));
            }
        }
        let Some((i, _)) = best else { break };
        kept.insert(0, i);
        covered.extend(remaining[i].iter().copied());
    }

    if verbose {
        let mut err = std::io::stderr().lock();
        let _ = err.write_all(msg::alt_packs(alt.len()).as_bytes());
        let _ = err.write_all(msg::SMALLEST_SET.as_bytes());
        for &i in &kept {
            let _ = writeln!(err, "\t{}", local[i].pack_path.display());
        }
        let in_kept: usize = kept.iter().map(|&i| remaining[i].len()).sum();
        let size: u64 = kept.iter().map(|&i| local[i].size).sum();
        let _ = err.write_all(
            msg::duplicates(in_kept.saturating_sub(all_objects.len()), size / 1024).as_bytes(),
        );
        let _ = err.write_all(msg::unique_considered(all_objects.len()).as_bytes());
        let _ = err.write_all(msg::REDUNDANT_HEADER.as_bytes());
    }
    // The list goes out last, as git's buffered standard output does.
    let mut out = Vec::new();
    let mut redundant_size = 0;
    for (i, p) in local.iter().enumerate() {
        if kept.contains(&i) {
            continue;
        }
        redundant_size += p.size;
        let _ = writeln!(out, "{}", p.idx_path.display());
        let _ = writeln!(out, "{}", p.pack_path.display());
    }
    if verbose {
        eprint!("{}", msg::redundant_total(redundant_size / (1024 * 1024)));
    }
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(&out);
    let _ = o.flush();
    Ok(0)
}

/// The packs of `objects_dir`, the newest first.
fn load_packs(objects_dir: &Path, kind: HashKind) -> Vec<Pack> {
    let dir = objects_dir.join("pack");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("pack-") && n.ends_with(".pack"))
        .collect();
    names.sort();
    let mut packs = Vec::new();
    for name in names {
        let pack_path = dir.join(&name);
        let idx_path = dir.join(format!("{}.idx", &name[..name.len() - ".pack".len()]));
        let Ok(index) = PackIndex::open(&idx_path, kind) else {
            continue;
        };
        let Ok(meta) = std::fs::metadata(&pack_path) else {
            continue;
        };
        let idx_size = std::fs::metadata(&idx_path).map_or(0, |m| m.len());
        packs.push(Pack {
            objects: index.ids().collect(),
            size: meta.len() + idx_size,
            mtime: meta.modified().unwrap_or(std::time::UNIX_EPOCH),
            pack_path,
            idx_path,
        });
    }
    packs.sort_by_key(|p| std::cmp::Reverse(p.mtime));
    packs
}
