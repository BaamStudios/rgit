// SPDX-License-Identifier: MIT

//! Differential tests: for a pack written by `git repack -adb`, the bitmap
//! rgit builds for the same pack must equal git's byte for byte, and
//! rgit must read git's bitmaps.
//!
//! Tests are skipped (pass trivially) when no `git` binary is available.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use rgit_hash::{HashKind, ObjectId};
use rgit_pack::bitmap::BitmapIndex;
use rgit_pack::bitmap::write::{Options, write_pack_bitmap};
use rgit_pack::write::PackSetSource;
use rgit_pack::{Pack, PackSet};

fn have_git() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn git_input(dir: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn git");
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let out = child.wait_with_output().expect("git output");
    writer.join().unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn git(dir: &Path, args: &[&str]) -> Vec<u8> {
    git_input(dir, args, b"")
}

/// A fast-import stream: `branches` lines of history with merges and
/// files in subdirectories; `same_date` gives every commit one date.
fn history(commits: usize, same_date: bool) -> Vec<u8> {
    let mut s = String::new();
    let mut mark = 0;
    let mut last_side = 0;
    for i in 1..=commits {
        let branch = if i % 7 == 3 { "side" } else { "main" };
        mark += 1;
        let date = if same_date {
            1_112_912_000
        } else {
            1_112_912_000 + (i as i64 * 37) % 500 + i as i64 * 10
        };
        s.push_str(&format!("commit refs/heads/{branch}\nmark :{mark}\n"));
        s.push_str(&format!("committer C <c@x> {date} +0000\n"));
        s.push_str(&format!("data <<EOF\nc{i}\nEOF\n"));
        if i % 7 == 3 && last_side == 0 && i > 1 {
            s.push_str(&format!("from :{}\n", mark - 1));
        }
        if branch == "main" && i % 11 == 0 && last_side > 0 {
            s.push_str(&format!("merge :{last_side}\n"));
        }
        if branch == "side" {
            last_side = mark;
        }
        s.push_str(&format!("M 644 inline f{}\ndata <<EOF\nx{i}\nEOF\n", i % 5));
        s.push_str(&format!(
            "M 644 inline dir/sub{}/file name {}.txt\ndata <<EOF\ny{}\nEOF\n",
            i % 3,
            i % 4,
            i / 2
        ));
        if i % 13 == 0 {
            s.push_str("M 644 inline same\ndata <<EOF\nshared\nEOF\n");
            s.push_str("M 644 inline dir/same\ndata <<EOF\nshared\nEOF\n");
        }
        s.push('\n');
        if i % 17 == 0 {
            s.push_str(&format!(
                "tag v{i}\nfrom :{mark}\ntagger T <t@x> 1112912000 +0000\ndata <<EOF\nt\nEOF\n\n"
            ));
        }
    }
    s.into_bytes()
}

struct Repo {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
}

impl Repo {
    fn new(commits: usize, same_date: bool) -> Repo {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("r");
        std::fs::create_dir(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        git_input(
            &dir,
            &["fast-import", "--quiet"],
            &history(commits, same_date),
        );
        Repo { _tmp: tmp, dir }
    }

    fn pack_dir(&self) -> PathBuf {
        self.dir.join(".git/objects/pack")
    }

    /// The single pack after `git repack -adb` with `config`.
    fn repack(&self, config: &[&str]) -> (Arc<Pack>, Vec<u8>) {
        let mut args: Vec<&str> = Vec::new();
        for c in config {
            args.push("-c");
            args.push(c);
        }
        args.extend(["repack", "-adbq"]);
        git(&self.dir, &args);
        let set = PackSet::open(self.pack_dir(), HashKind::Sha1);
        let packs = set.packs();
        assert_eq!(packs.len(), 1);
        let bitmap = std::fs::read(packs[0].index().path().with_extension("bitmap")).unwrap();
        (packs[0].clone(), bitmap)
    }
}

fn compare(repo: &Repo, config: &[&str], opts: Options) {
    let (pack, expected) = repo.repack(config);
    let set = PackSet::open(repo.pack_dir(), HashKind::Sha1);
    let source = PackSetSource::new(&set);
    let prefs: Vec<ObjectId> = Vec::new();
    // The walk order of `git rev-list --all` (what pack-objects walks).
    let order: Vec<ObjectId> = String::from_utf8(git(&repo.dir, &["rev-list", "--all"]))
        .unwrap()
        .lines()
        .map(|l| ObjectId::from_hex(HashKind::Sha1, l.as_bytes()).unwrap())
        .collect();
    let written =
        write_pack_bitmap(&pack, &source, Some(&order), &|id| prefs.contains(id), opts).unwrap();
    assert!(
        written.bytes == expected,
        "bitmap differs ({} vs {} bytes, first difference at {:?})",
        written.bytes.len(),
        expected.len(),
        written
            .bytes
            .iter()
            .zip(&expected)
            .position(|(a, b)| a != b)
    );
    // rgit reads git's bitmap: every commit bitmap matches the rebuilt one.
    let index = BitmapIndex::open_pack(pack).unwrap().unwrap();
    assert!(index.num_entries() > 0);
    for c in index.commits().collect::<Vec<_>>() {
        assert!(index.commit_bitmap(&c).unwrap().is_some());
    }
}

#[test]
fn small_history_identical() {
    if !have_git() {
        return;
    }
    let repo = Repo::new(40, false);
    compare(&repo, &[], Options::default());
}

#[test]
fn large_history_identical() {
    if !have_git() {
        return;
    }
    let repo = Repo::new(400, false);
    compare(&repo, &[], Options::default());
    compare(
        &repo,
        &["pack.writeBitmapLookupTable=true"],
        Options {
            hash_cache: true,
            lookup_table: true,
        },
    );
    compare(
        &repo,
        &["pack.writeBitmapHashCache=false"],
        Options {
            hash_cache: false,
            lookup_table: false,
        },
    );
}

#[test]
fn equal_dates_identical() {
    if !have_git() {
        return;
    }
    let repo = Repo::new(300, true);
    compare(&repo, &[], Options::default());
}

#[test]
fn preferred_tips_identical() {
    if !have_git() {
        return;
    }
    let repo = Repo::new(250, false);
    // Prefer some commits that fall into the windows after the first 100.
    let ids: Vec<String> = String::from_utf8(git(&repo.dir, &["rev-list", "--all"]))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    let mut prefs = Vec::new();
    for (n, i) in [103usize, 121, 150, 199].iter().enumerate() {
        git(
            &repo.dir,
            &["update-ref", &format!("refs/pref/p{n}"), &ids[*i]],
        );
        prefs.push(ObjectId::from_hex(HashKind::Sha1, ids[*i].as_bytes()).unwrap());
    }
    let (pack, expected) = repo.repack(&["pack.preferBitmapTips=refs/pref"]);
    let set = PackSet::open(repo.pack_dir(), HashKind::Sha1);
    let source = PackSetSource::new(&set);
    // The walk order with the new tips (dates are not monotonic here).
    let order: Vec<ObjectId> = String::from_utf8(git(&repo.dir, &["rev-list", "--all"]))
        .unwrap()
        .lines()
        .map(|l| ObjectId::from_hex(HashKind::Sha1, l.as_bytes()).unwrap())
        .collect();
    let written = write_pack_bitmap(
        &pack,
        &source,
        Some(&order),
        &|id| prefs.contains(id),
        Options::default(),
    )
    .unwrap();
    assert!(
        written.bytes == expected,
        "bitmap with preferred tips differs"
    );
}
