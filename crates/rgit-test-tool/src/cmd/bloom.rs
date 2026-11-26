// SPDX-License-Identifier: MIT

//! `bloom` (t0095-bloom.sh): changed-path Bloom filters, computed with
//! rgit-commitgraph.
//!
//! Observed behaviour of the reference binary:
//! - `get_murmur3 <string>` prints `Murmur3 Hash with seed=0:0x<8 hex>`
//!   (murmur3 as published, bytes unsigned); `get_murmur3_seven_highbit`
//!   does the same for the bytes `99 aa bb cc dd ee ff`.
//! - `generate_filter <string>...` prints one `Hashes:` line per string
//!   (the seven bit positions before the modulo, `0x%08x|` each, version 1
//!   hashing) and then a filter sized for one path (two bytes) with all
//!   strings added: `Filter_Length:2` and `Filter_Data:` with `%02x|` per
//!   byte.
//! - `get_filter_for_commit <commit-hex>` prints the filter of the commit's
//!   changes against its first parent (default settings), in the same two
//!   lines; a commit that cannot be read gives `error: Could not read <id>`
//!   and `fatal: unable to read tree (<id>)`, an argument that is not a
//!   full hex id `fatal: cannot parse oid '<arg>'` (exit 128).
//! - Without a subcommand, or with a missing argument, the usage goes to
//!   standard error with exit code 129; an unknown subcommand does nothing.

use std::io::Write;
use std::path::PathBuf;

use rgit_commitgraph::bloom::{self, Key, Settings};
use rgit_commitgraph::{Objects, ParsedCommit};
use rgit_hash::{HashKind, ObjectId};
use rgit_object::{Commit, ObjectKind};
use rgit_odb::{LooseOptions, ObjectDatabase};

const USAGE: &str = "usage: \n  test-tool bloom get_murmur3 <string>\n  test-tool bloom get_murmur3_seven_highbit\n  test-tool bloom generate_filter <string> [<string>...]\n  test-tool bloom get_filter_for_commit <commit-hex>\n";

fn usage() -> i32 {
    let _ = std::io::stderr().write_all(USAGE.as_bytes());
    129
}

fn print_filter(out: &mut impl Write, filter: &[u8]) {
    let _ = writeln!(out, "Filter_Length:{}", filter.len());
    let _ = write!(out, "Filter_Data:");
    for b in filter {
        let _ = write!(out, "{b:02x}|");
    }
    let _ = writeln!(out);
}

pub fn bloom(args: &[String]) -> i32 {
    let Some(sub) = args.first() else {
        return usage();
    };
    let mut out = std::io::stdout().lock();
    match sub.as_str() {
        "get_murmur3" => {
            let Some(s) = args.get(1) else {
                return usage();
            };
            let _ = writeln!(
                out,
                "Murmur3 Hash with seed=0:0x{:08x}",
                bloom::murmur3(s.as_bytes(), 0, false)
            );
            0
        }
        "get_murmur3_seven_highbit" => {
            let data = [0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
            let _ = writeln!(
                out,
                "Murmur3 Hash with seed=0:0x{:08x}",
                bloom::murmur3(&data, 0, false)
            );
            0
        }
        "generate_filter" => {
            if args.len() < 2 {
                return usage();
            }
            let settings = Settings::default();
            let len = settings.bits_per_entry.div_ceil(8) as usize;
            let mut filter = vec![0u8; len];
            for s in &args[1..] {
                let key = Key::new(s.as_bytes(), &settings);
                let _ = write!(out, "Hashes:");
                for h in &key.hashes {
                    let _ = write!(out, "0x{h:08x}|");
                }
                let _ = writeln!(out);
                bloom::add_key(&mut filter, &key);
            }
            print_filter(&mut out, &filter);
            0
        }
        "get_filter_for_commit" => {
            let Some(hex) = args.get(1) else {
                return usage();
            };
            filter_for_commit(&mut out, hex)
        }
        _ => 0,
    }
}

/// The repository's objects.
struct Db(ObjectDatabase);

impl Objects for Db {
    fn commit(&self, id: &ObjectId) -> Result<Option<ParsedCommit>, rgit_commitgraph::Unparsable> {
        let Ok(Some(o)) = self.0.read(id) else {
            return Ok(None);
        };
        if o.kind != ObjectKind::Commit {
            return Ok(None);
        }
        let c =
            Commit::parse(self.0.hash_kind(), &o.data).map_err(|_| rgit_commitgraph::Unparsable)?;
        Ok(Some(ParsedCommit {
            tree: c.tree().map_err(|_| rgit_commitgraph::Unparsable)?,
            parents: c.parents().map_err(|_| rgit_commitgraph::Unparsable)?,
            date: 0,
        }))
    }

    fn tree(&self, id: &ObjectId) -> Option<Vec<u8>> {
        match self.0.read(id) {
            Ok(Some(o)) if o.kind == ObjectKind::Tree => Some(o.data),
            _ => None,
        }
    }
}

fn hash_of(git_dir: &std::path::Path) -> HashKind {
    let config = std::fs::read_to_string(git_dir.join("config")).unwrap_or_default();
    let sha256 = config.lines().any(|l| {
        let l = l.trim().to_ascii_lowercase();
        l.starts_with("objectformat") && l.ends_with("sha256")
    });
    if sha256 {
        HashKind::Sha256
    } else {
        HashKind::Sha1
    }
}

fn filter_for_commit(out: &mut impl Write, hex: &str) -> i32 {
    let repo = match crate::repo::discover(false) {
        Ok(r) => r,
        Err(_) => {
            eprintln!("fatal: {}", crate::messages::NOT_A_REPOSITORY);
            return 128;
        }
    };
    let hash = hash_of(&repo.abs_git_dir);
    let Ok(id) = ObjectId::from_hex(hash, hex.as_bytes()) else {
        eprintln!("fatal: cannot parse oid '{hex}'");
        return 128;
    };
    let objects: PathBuf = std::env::var_os("GIT_OBJECT_DIRECTORY")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo.abs_git_dir.join("objects"));
    let mut odb = ObjectDatabase::open_with_packs(objects, hash, LooseOptions::default());
    odb.quiet();
    let db = Db(odb);
    let commit = match db.commit(&id) {
        Ok(Some(c)) => c,
        _ => {
            eprintln!("error: Could not read {hex}");
            eprintln!("fatal: unable to read tree ({hex})");
            return 128;
        }
    };
    let parent = match commit.parents.first() {
        Some(p) => match db.commit(p) {
            Ok(Some(c)) => Some(c.tree),
            _ => {
                eprintln!("error: Could not read {}", p.to_hex());
                return 128;
            }
        },
        None => None,
    };
    let paths = rgit_commitgraph::write::changed_paths(&db, hash, &commit.tree, parent.as_ref());
    let (filter, _) = bloom::build(&paths, &Settings::default());
    print_filter(out, &filter);
    0
}
