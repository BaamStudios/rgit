// SPDX-License-Identifier: MIT

//! Differential tests for the pack writer: every pack rgit writes must be
//! accepted by `git index-pack --strict` and `git verify-pack`, hold the
//! same objects as `git rev-list --objects`, and give back identical
//! objects through `git cat-file`.
//!
//! Tests are skipped (pass trivially) when no `git` binary is available.
//!
//! `upstream_random_pairs` (ignored by default) runs the same checks for 50
//! random (wants, haves) pairs in a full clone of git's own repository:
//! `RGIT_PACK_WRITE_REPO=/path/to/git.git cargo test -p rgit-pack --test
//! write -- --ignored`.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use rgit_hash::{HashKind, ObjectId};
use rgit_pack::write::{
    ListedObject, PackSetSource, Tip, WriteOptions, enumerate, pack_order, write_pack,
};
use rgit_pack::{Pack, PackSet};

fn have_git() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_git(dir: &Path, args: &[&str], input: &[u8], date: u64) -> (bool, Vec<u8>, Vec<u8>) {
    let date = format!("{date} +0000");
    let mut child = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
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
    (out.status.success(), out.stdout, out.stderr)
}

fn git_in(dir: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let (ok, out, err) = run_git(dir, args, input, 1_112_911_993);
    assert!(ok, "git {args:?} failed: {}", String::from_utf8_lossy(&err));
    out
}

struct Repo {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    kind: HashKind,
    time: Cell<u64>,
}

impl Repo {
    fn new(kind: HashKind) -> Repo {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("r");
        std::fs::create_dir(&dir).unwrap();
        git_in(
            &dir,
            &[
                "init",
                "-q",
                "-b",
                "main",
                &format!("--object-format={}", kind.name()),
            ],
            b"",
        );
        Repo {
            _tmp: tmp,
            dir,
            kind,
            time: Cell::new(1_112_911_993),
        }
    }

    fn git(&self, args: &[&str]) -> Vec<u8> {
        self.git_input(args, b"")
    }

    fn git_input(&self, args: &[&str], input: &[u8]) -> Vec<u8> {
        self.time.set(self.time.get() + 60);
        let (ok, out, err) = run_git(&self.dir, args, input, self.time.get());
        assert!(ok, "git {args:?} failed: {}", String::from_utf8_lossy(&err));
        out
    }

    fn text(&self, args: &[&str]) -> String {
        String::from_utf8(self.git(args)).unwrap()
    }

    fn write(&self, name: &str, content: &[u8]) {
        let p = self.dir.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    fn commit(&self, msg: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", msg]);
    }

    /// Branches with merges, slowly changing files (delta candidates),
    /// nested trees, an executable, a symlink, a gitlink, annotated tags
    /// on a commit, a tree, a blob and another tag.
    fn populate(&self) {
        let mut text = String::new();
        for i in 0..150 {
            text.push_str(&format!("line {i} of a text that changes slowly\n"));
        }
        for c in 0..12 {
            text.insert_str((c * 41) % text.len(), &format!("main edit {c}\n"));
            self.write("a.txt", text.as_bytes());
            self.write(&format!("dir/sub{}/f.txt", c % 3), &text.as_bytes()[c..]);
            self.commit(&format!("main {c}"));
        }
        self.write("run.sh", b"#!/bin/sh\necho hi\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = self.dir.join("run.sh");
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        self.git(&["add", "run.sh"]);
        #[cfg(unix)]
        std::os::unix::fs::symlink("a.txt", self.dir.join("link")).unwrap();
        let fake = self.text(&["rev-parse", "HEAD"]);
        self.git(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{},sub", fake.trim()),
        ]);
        self.git(&["commit", "-q", "-m", "exec, link, gitlink"]);
        for b in 0..3 {
            self.git(&["checkout", "-q", "-b", &format!("topic{b}"), "main~4"]);
            for c in 0..5 {
                let mut t = text.clone();
                t.push_str(&format!("topic {b} change {c}\n"));
                self.write(&format!("topic{b}/file.txt"), t.as_bytes());
                self.commit(&format!("topic {b} {c}"));
            }
            self.git(&["checkout", "-q", "main"]);
            if b != 2 {
                self.git(&["merge", "-q", "--no-edit", &format!("topic{b}")]);
            }
            self.write("a.txt", format!("{text}after merge {b}\n").as_bytes());
            self.commit(&format!("after {b}"));
        }
        self.git(&["tag", "-a", "-m", "on commit", "v1", "main~2"]);
        self.git(&["tag", "-a", "-m", "on tag", "v1-again", "v1"]);
        self.git(&["tag", "-a", "-m", "on tree", "t-tree", "main^{tree}"]);
        self.git(&["tag", "-a", "-m", "on blob", "t-blob", "main:a.txt"]);
        self.git(&["tag", "light", "topic2"]);
    }

    fn pack_dir(&self) -> PathBuf {
        self.dir.join(".git/objects/pack")
    }

    /// Repack everything into one pack with deltas, drop loose objects.
    fn repack(&self) {
        self.git(&["repack", "-adfq", "--depth=20", "--window=30"]);
        self.git(&["prune-packed"]);
    }

    /// Add a second pack holding some of the same objects again (stored
    /// differently), so reuse sees several candidate packs.
    fn add_overlapping_pack(&self) {
        let pack = self.git_input(&["pack-objects", "--revs", "--stdout", "-q"], b"topic1\n");
        let tmp = self.dir.join("extra.pack");
        std::fs::write(&tmp, &pack).unwrap();
        let pack_dir = self.pack_dir();
        let name = String::from_utf8(self.git(&["index-pack", tmp.to_str().unwrap()])).unwrap();
        let name = name.trim();
        std::fs::rename(&tmp, pack_dir.join(format!("pack-{name}.pack"))).unwrap();
        std::fs::rename(
            self.dir.join("extra.idx"),
            pack_dir.join(format!("pack-{name}.idx")),
        )
        .unwrap();
    }

    fn rev_list(&self, args: &[&str]) -> BTreeSet<ObjectId> {
        let mut a = vec!["rev-list", "--objects"];
        a.extend_from_slice(args);
        ids_of(self.kind, &self.text(&a))
    }

    fn resolve(&self, rev: &str) -> ObjectId {
        let hex = self.text(&["rev-parse", rev]);
        ObjectId::from_hex(self.kind, hex.trim().as_bytes()).unwrap()
    }

    fn all_tips(&self) -> Vec<ObjectId> {
        let text = self.text(&["for-each-ref", "--format=%(objectname)"]);
        text.lines()
            .map(|l| ObjectId::from_hex(self.kind, l.as_bytes()).unwrap())
            .collect()
    }
}

/// Compare object sets, showing only the differences.
fn assert_same_set(got: &BTreeSet<ObjectId>, expect: &BTreeSet<ObjectId>, what: &str) {
    let extra: Vec<_> = got.difference(expect).collect();
    let missing: Vec<_> = expect.difference(got).collect();
    assert!(
        extra.is_empty() && missing.is_empty(),
        "{what}: not in rev-list: {extra:?}; missing: {missing:?}"
    );
}

/// The ids at the start of each line.
fn ids_of(kind: HashKind, text: &str) -> BTreeSet<ObjectId> {
    text.lines()
        .filter(|l| !l.is_empty())
        .map(|l| ObjectId::from_hex(kind, &l.as_bytes()[..kind.hex_len()]).unwrap())
        .collect()
}

/// `cat-file --batch` output for the given ids, keyed by id.
fn cat_batch(dir: &Path, ids: &BTreeSet<ObjectId>) -> BTreeMap<ObjectId, Vec<u8>> {
    let mut input = String::new();
    for id in ids {
        input.push_str(&id.to_hex());
        input.push('\n');
    }
    let mut out = &git_in(dir, &["cat-file", "--batch"], input.as_bytes())[..];
    let mut map = BTreeMap::new();
    for id in ids {
        let nl = out.iter().position(|&b| b == b'\n').unwrap();
        let line = std::str::from_utf8(&out[..nl]).unwrap().to_string();
        let size: usize = line.rsplit(' ').next().unwrap().parse().unwrap();
        let mut entry = line.into_bytes();
        entry.extend_from_slice(&out[nl + 1..nl + 1 + size]);
        out = &out[nl + 2 + size..];
        map.insert(*id, entry);
    }
    map
}

/// Index `pack` with `git index-pack --strict` inside `target` (which
/// must hold everything the pack's objects link to), check it with
/// `verify-pack`, and return the ids it contains.
fn index_in(target: &Path, kind: HashKind, pack: &[u8], name: &str) -> BTreeSet<ObjectId> {
    let path = target.join(format!("{name}.pack"));
    std::fs::write(&path, pack).unwrap();
    let p = path.to_str().unwrap();
    let (ok, _, err) = run_git(target, &["index-pack", "--strict", p], b"", 0);
    assert!(
        ok,
        "index-pack rejected {name}: {}",
        String::from_utf8_lossy(&err)
    );
    let idx = path.with_extension("idx");
    let i = idx.to_str().unwrap();
    let (ok, _, err) = run_git(target, &["verify-pack", "-v", i], b"", 0);
    assert!(
        ok,
        "verify-pack rejected {name}: {}",
        String::from_utf8_lossy(&err)
    );
    let idx_bytes = std::fs::read(&idx).unwrap();
    let shown = git_in(
        target,
        &["show-index", &format!("--object-format={}", kind.name())],
        &idx_bytes,
    );
    String::from_utf8(shown)
        .unwrap()
        .lines()
        .map(|l| {
            let hex = l.split(' ').nth(1).unwrap();
            ObjectId::from_hex(kind, hex.as_bytes()).unwrap()
        })
        .collect()
}

fn write(
    set: &PackSet,
    kind: HashKind,
    objects: &[ListedObject],
    packs: &[Arc<Pack>],
    opts: &WriteOptions,
) -> Vec<u8> {
    let source = PackSetSource::new(set);
    let (summary, out) = write_pack(Vec::new(), kind, objects, &source, packs, opts).unwrap();
    assert_eq!(summary.entries.len(), objects.len());
    assert_eq!(
        &out[out.len() - kind.byte_len()..],
        summary.checksum.as_bytes()
    );
    out
}

fn option_sets() -> Vec<WriteOptions> {
    let mut v = Vec::new();
    for ofs_delta in [false, true] {
        for (reuse_object, reuse_delta) in [(true, true), (true, false), (false, false)] {
            v.push(WriteOptions {
                ofs_delta,
                reuse_delta,
                reuse_object,
                compression: if reuse_object { None } else { Some(1) },
                max_depth: 50,
            });
        }
    }
    v.push(WriteOptions {
        ofs_delta: true,
        max_depth: 2,
        ..WriteOptions::default()
    });
    v
}

fn full_round_trip(kind: HashKind) {
    if !have_git() {
        return;
    }
    let repo = Repo::new(kind);
    repo.populate();
    repo.repack();
    repo.add_overlapping_pack();
    let set = PackSet::open(repo.pack_dir(), kind);
    let source = PackSetSource::new(&set);
    let tips: Vec<Tip> = repo
        .all_tips()
        .into_iter()
        .map(|id| Tip { id, exclude: false })
        .collect();
    let listed = enumerate(&source, kind, &tips).unwrap();
    let expect = repo.rev_list(&["--all"]);
    let got: BTreeSet<ObjectId> = listed.iter().map(|o| o.id).collect();
    assert_same_set(&got, &expect, "enumerate --all");
    let ordered = pack_order(&listed);
    assert_eq!(ordered.len(), expect.len());

    let originals = cat_batch(&repo.dir, &expect);
    let packs = set.packs();
    assert!(packs.len() >= 2);
    for (n, opts) in option_sets().iter().enumerate() {
        let pack = write(&set, kind, &ordered, &packs, opts);
        let target = tempfile::tempdir().unwrap();
        git_in(
            target.path(),
            &[
                "init",
                "-q",
                "--bare",
                &format!("--object-format={}", kind.name()),
            ],
            b"",
        );
        let ids = index_in(target.path(), kind, &pack, &format!("p{n}"));
        assert_eq!(ids, expect, "pack {n} ({opts:?}) holds other objects");
        // Move the pack into the repository and read everything back.
        let objects_pack = target.path().join("objects/pack");
        for ext in ["pack", "idx"] {
            std::fs::rename(
                target.path().join(format!("p{n}.{ext}")),
                objects_pack.join(format!("pack-p{n}.{ext}")),
            )
            .unwrap();
        }
        assert_eq!(cat_batch(target.path(), &expect), originals, "pack {n}");
    }
}

#[test]
fn full_round_trip_sha1() {
    full_round_trip(HashKind::Sha1);
}

#[test]
fn full_round_trip_sha256() {
    full_round_trip(HashKind::Sha256);
}

/// A small deterministic generator (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Check one (wants, haves) pair: the listed objects equal `rev-list
/// --objects`, and the pack is accepted by `index-pack --strict` in a
/// repository that holds exactly the haves.
fn check_pair(repo: &Repo, set: &PackSet, wants: &[String], haves: &[String], opts: &WriteOptions) {
    let kind = repo.kind;
    let source = PackSetSource::new(set);
    let mut tips: Vec<Tip> = wants
        .iter()
        .map(|w| Tip {
            id: repo.resolve(w),
            exclude: false,
        })
        .collect();
    tips.extend(haves.iter().map(|h| Tip {
        id: repo.resolve(h),
        exclude: true,
    }));
    let listed = enumerate(&source, kind, &tips).unwrap();
    let mut args: Vec<&str> = wants.iter().map(String::as_str).collect();
    args.push("--not");
    args.extend(haves.iter().map(String::as_str));
    let expect = repo.rev_list(&args);
    let got: BTreeSet<ObjectId> = listed.iter().map(|o| o.id).collect();
    assert_same_set(&got, &expect, &format!("wants {wants:?} haves {haves:?}"));

    let pack = write(set, kind, &pack_order(&listed), &set.packs(), opts);
    let target = tempfile::tempdir().unwrap();
    git_in(
        target.path(),
        &[
            "init",
            "-q",
            "--bare",
            &format!("--object-format={}", kind.name()),
        ],
        b"",
    );
    if !haves.is_empty() {
        let input = haves.join("\n") + "\n";
        let have_pack = repo.git_input(
            &["pack-objects", "--revs", "--stdout", "-q"],
            input.as_bytes(),
        );
        git_in(target.path(), &["index-pack", "--stdin"], &have_pack);
    }
    let ids = index_in(target.path(), kind, &pack, "new");
    assert_eq!(ids, expect, "wants {wants:?} haves {haves:?}");
}

#[test]
fn random_pairs() {
    if !have_git() {
        return;
    }
    let kind = HashKind::Sha1;
    let repo = Repo::new(kind);
    repo.populate();
    repo.repack();
    let set = PackSet::open(repo.pack_dir(), kind);
    let mut revs: Vec<String> = repo
        .text(&["rev-list", "--all"])
        .lines()
        .map(str::to_string)
        .collect();
    revs.extend(["v1", "v1-again", "t-tree", "t-blob", "light"].map(String::from));
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for round in 0..50 {
        let wants: Vec<String> = (0..1 + rng.below(2))
            .map(|_| revs[rng.below(revs.len())].clone())
            .collect();
        let haves: Vec<String> = (0..rng.below(3))
            .map(|_| revs[rng.below(revs.len())].clone())
            .collect();
        let opts = WriteOptions {
            ofs_delta: round % 2 == 0,
            ..WriteOptions::default()
        };
        check_pair(&repo, &set, &wants, &haves, &opts);
    }
}

#[test]
fn empty_pack() {
    if !have_git() {
        return;
    }
    let repo = Repo::new(HashKind::Sha1);
    let set = PackSet::open(repo.pack_dir(), HashKind::Sha1);
    let pack = write(&set, HashKind::Sha1, &[], &[], &WriteOptions::default());
    assert_eq!(pack.len(), 12 + 20);
    let target = tempfile::tempdir().unwrap();
    git_in(target.path(), &["init", "-q", "--bare"], b"");
    assert!(index_in(target.path(), HashKind::Sha1, &pack, "empty").is_empty());
}

/// The pack order: commits newest first with their tags, then trees, then
/// blobs.
#[test]
fn order_groups_types() {
    if !have_git() {
        return;
    }
    let kind = HashKind::Sha1;
    let repo = Repo::new(kind);
    repo.populate();
    repo.repack();
    let set = PackSet::open(repo.pack_dir(), kind);
    let source = PackSetSource::new(&set);
    let tips = vec![Tip {
        id: repo.resolve("v1-again"),
        exclude: false,
    }];
    let ordered = pack_order(&enumerate(&source, kind, &tips).unwrap());
    let kinds: Vec<&str> = ordered.iter().map(|o| o.kind.name()).collect();
    let first_tree = kinds.iter().position(|k| *k == "tree").unwrap();
    let first_blob = kinds.iter().position(|k| *k == "blob").unwrap();
    assert!(
        kinds[..first_tree]
            .iter()
            .all(|k| *k == "commit" || *k == "tag")
    );
    assert!(kinds[first_tree..first_blob].iter().all(|k| *k == "tree"));
    assert!(kinds[first_blob..].iter().all(|k| *k == "blob"));
    // v1 points at main~2: the commit comes first, then both tags.
    assert_eq!(ordered[0].id, repo.resolve("main~2"));
    assert_eq!(ordered[1].id, repo.resolve("v1"));
    assert_eq!(ordered[2].id, repo.resolve("v1-again"));
}

/// 50 random (wants, haves) pairs in a full clone of git's repository.
#[test]
#[ignore = "needs RGIT_PACK_WRITE_REPO (a full clone of git.git)"]
fn upstream_random_pairs() {
    let Some(dir) = std::env::var_os("RGIT_PACK_WRITE_REPO") else {
        eprintln!("RGIT_PACK_WRITE_REPO not set, skipping");
        return;
    };
    let dir = PathBuf::from(dir);
    let kind = HashKind::Sha1;
    let set = PackSet::open(dir.join("objects/pack"), kind);
    let source = PackSetSource::new(&set);
    let text = String::from_utf8(git_in(
        &dir,
        &["rev-list", "--max-count=30000", "HEAD"],
        b"",
    ))
    .unwrap();
    let commits: Vec<&str> = text.lines().collect();
    let tags = String::from_utf8(git_in(&dir, &["tag", "--list", "v2.4*"], b"")).unwrap();
    let tags: Vec<&str> = tags.lines().collect();
    let mut rng = Rng(0x1234_5678_9abc_def1);
    for round in 0..50 {
        let i = rng.below(commits.len() - 400);
        let mut wants = vec![commits[i].to_string()];
        if rng.below(4) == 0 {
            wants.push(commits[i + rng.below(50)].to_string());
        }
        if rng.below(5) == 0 && !tags.is_empty() {
            wants.push(tags[rng.below(tags.len())].to_string());
        }
        let mut haves = vec![commits[i + 1 + rng.below(300)].to_string()];
        if rng.below(3) == 0 {
            haves.push(commits[i + 1 + rng.below(300)].to_string());
        }
        let resolve = |r: &str| {
            let hex = String::from_utf8(git_in(&dir, &["rev-parse", r], b"")).unwrap();
            ObjectId::from_hex(kind, hex.trim().as_bytes()).unwrap()
        };
        let mut tips: Vec<Tip> = wants
            .iter()
            .map(|w| Tip {
                id: resolve(w),
                exclude: false,
            })
            .collect();
        tips.extend(haves.iter().map(|h| Tip {
            id: resolve(h),
            exclude: true,
        }));
        let listed = enumerate(&source, kind, &tips).unwrap();
        let mut args = vec!["rev-list", "--objects"];
        args.extend(wants.iter().map(String::as_str));
        args.push("--not");
        args.extend(haves.iter().map(String::as_str));
        let expect = ids_of(kind, &String::from_utf8(git_in(&dir, &args, b"")).unwrap());
        let got: BTreeSet<ObjectId> = listed.iter().map(|o| o.id).collect();
        assert_same_set(
            &got,
            &expect,
            &format!("round {round}: wants {wants:?} haves {haves:?}"),
        );
        let opts = WriteOptions {
            ofs_delta: round % 2 == 0,
            ..WriteOptions::default()
        };
        let pack = write(&set, kind, &pack_order(&listed), &set.packs(), &opts);
        // The target borrows the source's objects as an alternate, so
        // `--strict` can check links; `index-pack` without `--fix-thin`
        // still rejects any delta whose base is not in the pack.
        let target = tempfile::tempdir().unwrap();
        git_in(target.path(), &["init", "-q", "--bare"], b"");
        std::fs::write(
            target.path().join("objects/info/alternates"),
            format!("{}\n", dir.join("objects").display()),
        )
        .unwrap();
        let ids = index_in(target.path(), kind, &pack, "new");
        assert_eq!(ids, expect, "round {round}");
        eprintln!("round {round}: {} objects, {} bytes", ids.len(), pack.len());
    }
}
