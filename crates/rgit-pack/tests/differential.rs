// SPDX-License-Identifier: MIT

//! Differential tests: packs written by the `git` binary must read back
//! exactly as `git cat-file --batch` shows them.
//!
//! Tests are skipped (pass trivially) when no `git` binary is available.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rgit_hash::{HashKind, ObjectId, Prefix};
use rgit_pack::{DeltaBaseCache, Error, Object, ObjectKind, Pack, PackSet};

fn have_git() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn git(dir: &Path, args: &[&str]) -> Vec<u8> {
    git_input(dir, args, b"")
}

fn git_input(dir: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1112911993 -0700")
        .env("GIT_COMMITTER_DATE", "1112911993 -0700")
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

struct Repo {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    kind: HashKind,
}

impl Repo {
    fn new(kind: HashKind) -> Repo {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("r");
        std::fs::create_dir(&dir).unwrap();
        git(
            &dir,
            &["init", "-q", &format!("--object-format={}", kind.name())],
        );
        Repo {
            _tmp: tmp,
            dir,
            kind,
        }
    }

    fn git(&self, args: &[&str]) -> Vec<u8> {
        git(&self.dir, args)
    }

    fn pack_dir(&self) -> PathBuf {
        self.dir.join(".git/objects/pack")
    }

    fn write(&self, name: &str, content: &[u8]) {
        let p = self.dir.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    /// History with many similar file versions (delta candidates), nested
    /// trees, binary data and an annotated tag.
    fn populate(&self, commits: usize) {
        let mut text = String::new();
        for i in 0..200 {
            text.push_str(&format!("line {i} of a text that changes slowly\n"));
        }
        for c in 0..commits {
            text.insert_str((c * 37) % text.len(), &format!("edit {c}\n"));
            self.write("a.txt", text.as_bytes());
            self.write(&format!("dir/sub{}/f.txt", c % 3), &text.as_bytes()[c..]);
            let bin: Vec<u8> = (0..3000u32)
                .map(|i| (i.wrapping_mul(2654435761).wrapping_add(c as u32) >> 13) as u8)
                .collect();
            self.write("bin.dat", &bin);
            self.git(&["add", "-A"]);
            self.git(&["commit", "-q", "-m", &format!("commit {c}")]);
        }
        self.git(&["tag", "-f", "-a", "-m", "a tag", "v1"]);
        self.write("empty", b"");
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", "empty file"]);
    }

    /// Every object as git sees it.
    fn git_objects(&self) -> BTreeMap<ObjectId, Object> {
        let out = self.git(&["cat-file", "--batch-all-objects", "--batch"]);
        parse_batch(self.kind, &out)
    }

    fn packs(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(self.pack_dir())
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "idx"))
            .collect();
        v.sort();
        v
    }

    fn remove_loose(&self) {
        let objects = self.dir.join(".git/objects");
        for e in std::fs::read_dir(&objects).unwrap().flatten() {
            let n = e.file_name();
            let n = n.to_string_lossy();
            if n.len() == 2 && n.chars().all(|c| c.is_ascii_hexdigit()) {
                std::fs::remove_dir_all(e.path()).unwrap();
            }
        }
    }
}

fn parse_kind(s: &str) -> ObjectKind {
    match s {
        "commit" => ObjectKind::Commit,
        "tree" => ObjectKind::Tree,
        "blob" => ObjectKind::Blob,
        "tag" => ObjectKind::Tag,
        _ => panic!("bad type {s}"),
    }
}

fn parse_batch(kind: HashKind, mut out: &[u8]) -> BTreeMap<ObjectId, Object> {
    let mut map = BTreeMap::new();
    while !out.is_empty() {
        let nl = out.iter().position(|&b| b == b'\n').unwrap();
        let line = std::str::from_utf8(&out[..nl]).unwrap();
        let mut parts = line.split(' ');
        let id = ObjectId::from_hex(kind, parts.next().unwrap().as_bytes()).unwrap();
        let ty = parse_kind(parts.next().unwrap());
        let size: usize = parts.next().unwrap().parse().unwrap();
        let data = out[nl + 1..nl + 1 + size].to_vec();
        assert_eq!(out[nl + 1 + size], b'\n');
        out = &out[nl + 2 + size..];
        map.insert(id, Object { kind: ty, data });
    }
    map
}

/// Read every object of `set` and compare with git's view.
fn assert_same(repo: &Repo, set: &PackSet) {
    let expect = repo.git_objects();
    assert!(!expect.is_empty());
    let ids = set.all_ids();
    assert_eq!(ids, expect.keys().copied().collect::<Vec<_>>());
    let mut cache = DeltaBaseCache::default();
    for (id, obj) in &expect {
        let got = set
            .read(id, &mut cache, &mut |e| panic!("{e}"))
            .unwrap()
            .unwrap_or_else(|| panic!("{id} missing"));
        assert_eq!(&got, obj, "object {id}");
    }
    // Reading again in reverse order exercises the cache differently.
    let mut cache = DeltaBaseCache::new(4096);
    for (id, obj) in expect.iter().rev() {
        let got = set
            .read(id, &mut cache, &mut |e| panic!("{e}"))
            .unwrap()
            .unwrap();
        assert_eq!(&got, obj);
    }
}

fn deltas_in(repo: &Repo) -> (usize, usize) {
    // (number of deltas, maximum chain depth) from verify-pack.
    let mut deltas = 0;
    let mut depth = 0;
    for idx in repo.packs() {
        let out = repo.git(&["verify-pack", "-v", idx.to_str().unwrap()]);
        for line in String::from_utf8(out).unwrap().lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() == 7 {
                deltas += 1;
                depth = depth.max(f[5].parse().unwrap());
            }
        }
    }
    (deltas, depth)
}

#[test]
fn gc_packs_match_git_sha1() {
    if !have_git() {
        return;
    }
    let repo = Repo::new(HashKind::Sha1);
    repo.populate(30);
    repo.git(&["gc", "-q", "--aggressive"]);
    repo.remove_loose();
    let (deltas, depth) = deltas_in(&repo);
    assert!(deltas > 20 && depth > 2, "deltas {deltas} depth {depth}");
    let set = PackSet::open(repo.pack_dir(), HashKind::Sha1);
    assert!(set.scan_errors().is_empty());
    assert_same(&repo, &set);
    let mut cache = DeltaBaseCache::default();
    for p in set.packs() {
        p.verify(&mut cache).unwrap();
        assert!(!p.is_keep() && !p.is_promisor());
        assert_eq!(p.index().version(), 2);
    }
}

#[test]
fn sha256_repository() {
    if !have_git() {
        return;
    }
    let repo = Repo::new(HashKind::Sha256);
    repo.populate(15);
    repo.git(&["repack", "-adfq"]);
    repo.remove_loose();
    assert!(deltas_in(&repo).0 > 5);
    let set = PackSet::open(repo.pack_dir(), HashKind::Sha256);
    assert_same(&repo, &set);
    let mut cache = DeltaBaseCache::default();
    set.packs()[0].verify(&mut cache).unwrap();
    // The same files read as SHA-1 do not parse.
    assert!(Pack::open(&repo.packs()[0], HashKind::Sha1).is_err());
}

#[test]
fn ref_deltas_and_index_v1() {
    if !have_git() {
        return;
    }
    let repo = Repo::new(HashKind::Sha1);
    repo.populate(20);
    repo.git(&[
        "-c",
        "repack.useDeltaBaseOffset=false",
        "-c",
        "pack.indexVersion=1",
        "repack",
        "-adfq",
    ]);
    repo.remove_loose();
    let set = PackSet::open(repo.pack_dir(), HashKind::Sha1);
    let pack = &set.packs()[0];
    assert_eq!(pack.index().version(), 1);
    assert_eq!(pack.index().crc32_at(0), None);
    // All deltas are REF_DELTA.
    let data = pack.data().unwrap();
    let mut refs = 0;
    for pos in 0..pack.index().len() {
        let h = data
            .entry_header(pack.index().offset_at(pos).unwrap())
            .unwrap();
        assert!(!matches!(h.kind, rgit_pack::EntryKind::OfsDelta { .. }));
        refs += matches!(h.kind, rgit_pack::EntryKind::RefDelta { .. }) as usize;
    }
    assert!(refs > 10);
    assert_same(&repo, &set);
    pack.verify(&mut DeltaBaseCache::default()).unwrap();
}

#[test]
fn large_offset_table() {
    if !have_git() {
        return;
    }
    let repo = Repo::new(HashKind::Sha1);
    repo.populate(10);
    // Offsets above 256 go to the 64-bit table.
    let ids = repo.git(&["rev-list", "--objects", "--all"]);
    let ids: Vec<u8> = String::from_utf8(ids)
        .unwrap()
        .lines()
        .map(|l| format!("{}\n", &l[..40]))
        .collect::<String>()
        .into_bytes();
    git_input(
        &repo.dir,
        &[
            "pack-objects",
            "-q",
            "--index-version=2,256",
            ".git/objects/pack/pack",
        ],
        &ids,
    );
    repo.git(&["prune-packed"]);
    repo.remove_loose();
    let set = PackSet::open(repo.pack_dir(), HashKind::Sha1);
    let pack = &set.packs()[0];
    let idx_len = std::fs::metadata(pack.index().path()).unwrap().len();
    let n = pack.index().len() as u64;
    assert!(
        idx_len > 8 + 1024 + n * 28 + 40,
        "no 64-bit offsets written"
    );
    assert_same(&repo, &set);
    pack.verify(&mut DeltaBaseCache::default()).unwrap();
}

#[test]
fn object_info_and_delta_base_match_batch_check() {
    if !have_git() {
        return;
    }
    let repo = Repo::new(HashKind::Sha1);
    repo.populate(12);
    repo.git(&["repack", "-adfq"]);
    repo.remove_loose();
    let out = repo.git(&[
        "cat-file",
        "--batch-all-objects",
        "--batch-check=%(objectname) %(objecttype) %(objectsize) %(deltabase) %(objectsize:disk)",
    ]);
    let set = PackSet::open(repo.pack_dir(), HashKind::Sha1);
    let pack = &set.packs()[0];
    for line in String::from_utf8(out).unwrap().lines() {
        let f: Vec<&str> = line.split(' ').collect();
        let id: ObjectId = f[0].parse().unwrap();
        let offset = pack
            .index()
            .offset_at(pack.index().lookup(&id).unwrap())
            .unwrap();
        let (kind, size) = pack.object_info(offset).unwrap();
        assert_eq!(kind.name(), f[1]);
        assert_eq!(size.to_string(), f[2]);
        let base = pack.delta_base(offset).unwrap();
        let null = HashKind::Sha1.null();
        assert_eq!(base.unwrap_or(null).to_string(), f[3]);
        let disk = pack.entry_end(offset).unwrap() - offset;
        assert_eq!(disk.to_string(), f[4]);
    }
}

#[test]
fn prefix_lookup() {
    if !have_git() {
        return;
    }
    let repo = Repo::new(HashKind::Sha1);
    repo.populate(10);
    repo.git(&["repack", "-adq"]);
    let set = PackSet::open(repo.pack_dir(), HashKind::Sha1);
    let ids = set.all_ids();
    for id in &ids {
        for len in [1usize, 2, 4, 7, 40] {
            let p = Prefix::new(*id, len).unwrap();
            let expect: Vec<ObjectId> = ids.iter().filter(|o| o.starts_with(&p)).copied().collect();
            assert_eq!(set.lookup_prefix(&p, usize::MAX), expect, "{p}");
        }
    }
    let none = Prefix::from_hex(HashKind::Sha1, b"0000000000").unwrap();
    assert!(
        set.lookup_prefix(&none, 10)
            .iter()
            .all(|o| o.starts_with(&none))
    );
}

#[test]
fn rescan_finds_new_pack_and_drops_removed() {
    if !have_git() {
        return;
    }
    let repo = Repo::new(HashKind::Sha1);
    repo.populate(3);
    repo.git(&["repack", "-adq"]);
    let set = PackSet::open(repo.pack_dir(), HashKind::Sha1);
    let old = set.packs();
    assert_eq!(old.len(), 1);
    repo.populate(2);
    repo.git(&["repack", "-adfq"]);
    repo.remove_loose();
    let head = String::from_utf8(repo.git(&["rev-parse", "HEAD"])).unwrap();
    let head: ObjectId = head.trim().parse().unwrap();
    // The old pack was replaced; the miss triggers a rescan.
    let mut cache = DeltaBaseCache::default();
    let obj = set
        .read(&head, &mut cache, &mut |e| panic!("{e}"))
        .unwrap()
        .unwrap();
    assert_eq!(obj.kind, ObjectKind::Commit);
    assert_eq!(set.packs().len(), 1);
    assert!(!std::sync::Arc::ptr_eq(&set.packs()[0], &old[0]));
}

#[test]
fn keep_and_promisor_flags() {
    if !have_git() {
        return;
    }
    let repo = Repo::new(HashKind::Sha1);
    repo.populate(1);
    repo.git(&["repack", "-adq"]);
    let idx = repo.packs().remove(0);
    std::fs::write(idx.with_extension("keep"), b"").unwrap();
    std::fs::write(idx.with_extension("promisor"), b"").unwrap();
    let set = PackSet::open(repo.pack_dir(), HashKind::Sha1);
    assert!(set.packs()[0].is_keep());
    assert!(set.packs()[0].is_promisor());
}

fn pack_entry_header(ty: u8, size: usize) -> Vec<u8> {
    let mut out = vec![(ty << 4) | (size & 15) as u8];
    let mut rest = size >> 4;
    while rest > 0 {
        *out.last_mut().unwrap() |= 0x80;
        out.push((rest & 127) as u8);
        rest >>= 7;
    }
    out
}

fn size_varint(mut n: usize, out: &mut Vec<u8>) {
    loop {
        let b = (n & 127) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn ofs_encoding(mut n: u64) -> Vec<u8> {
    let mut buf = vec![(n & 127) as u8];
    n >>= 7;
    while n > 0 {
        n -= 1;
        buf.push(0x80 | (n & 127) as u8);
        n >>= 7;
    }
    buf.reverse();
    buf
}

fn deflate(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn copy_op(offset: usize, size: usize, out: &mut Vec<u8>) {
    if size == 0 {
        return;
    }
    out.push(0xff);
    out.extend_from_slice(&(offset as u32).to_le_bytes());
    out.extend_from_slice(&(size as u32).to_le_bytes()[..3]);
}

/// Write a pack (and its v2 index) holding one blob in `revisions`
/// versions; version `i` is an `OFS_DELTA` against version `i - 1`, and a
/// whole object every `max_depth + 1` versions. Returns the ids in
/// revision order. The tests check the result with `git verify-pack`.
fn write_chain_pack(dir: &Path, revisions: usize, max_depth: usize) -> Vec<ObjectId> {
    let mut pack = b"PACK".to_vec();
    pack.extend_from_slice(&2u32.to_be_bytes());
    pack.extend_from_slice(&(revisions as u32).to_be_bytes());
    let mut entries: Vec<(ObjectId, u32, u64)> = Vec::new();
    let mut content: Vec<u8> = Vec::new();
    let mut prev_offset = 0u64;
    for i in 0..revisions {
        let line = format!("revision {i}\n");
        // Insert at a line boundary somewhere inside.
        let at = if content.is_empty() {
            0
        } else {
            let mut p = (i * 7919) % content.len();
            while p > 0 && content[p - 1] != b'\n' {
                p -= 1;
            }
            p
        };
        let base_len = content.len();
        let mut next = content[..at].to_vec();
        next.extend_from_slice(line.as_bytes());
        next.extend_from_slice(&content[at..]);
        let offset = pack.len() as u64;
        let start = pack.len();
        if i % (max_depth + 1) == 0 {
            pack.extend(pack_entry_header(3, next.len()));
            pack.extend(deflate(&next));
        } else {
            let mut delta = Vec::new();
            size_varint(base_len, &mut delta);
            size_varint(next.len(), &mut delta);
            copy_op(0, at, &mut delta);
            delta.push(line.len() as u8);
            delta.extend_from_slice(line.as_bytes());
            copy_op(at, base_len - at, &mut delta);
            pack.extend(pack_entry_header(6, delta.len()));
            pack.extend(ofs_encoding(offset - prev_offset));
            pack.extend(deflate(&delta));
        }
        let crc = crc32fast::hash(&pack[start..]);
        let id = rgit_hash::hash_object(HashKind::Sha1, "blob", &next).unwrap();
        entries.push((id, crc, offset));
        prev_offset = offset;
        content = next;
    }
    let trailer = rgit_hash::hash(HashKind::Sha1, &pack).unwrap();
    pack.extend_from_slice(trailer.as_bytes());
    let ids: Vec<ObjectId> = entries.iter().map(|e| e.0).collect();

    entries.sort();
    let mut idx = vec![0xff, b't', b'O', b'c', 0, 0, 0, 2];
    for b in 0..256usize {
        let n = entries
            .iter()
            .filter(|e| (e.0.as_bytes()[0] as usize) <= b)
            .count();
        idx.extend_from_slice(&(n as u32).to_be_bytes());
    }
    for e in &entries {
        idx.extend_from_slice(e.0.as_bytes());
    }
    for e in &entries {
        idx.extend_from_slice(&e.1.to_be_bytes());
    }
    for e in &entries {
        idx.extend_from_slice(&(e.2 as u32).to_be_bytes());
    }
    idx.extend_from_slice(trailer.as_bytes());
    let idx_sum = rgit_hash::hash(HashKind::Sha1, &idx).unwrap();
    idx.extend_from_slice(idx_sum.as_bytes());

    let name = dir.join(format!("pack-{trailer}"));
    std::fs::write(name.with_extension("pack"), pack).unwrap();
    std::fs::write(name.with_extension("idx"), idx).unwrap();
    ids
}

/// Long delta chains are resolved without recursion.
fn deep_chain(revisions: usize, reads: usize) {
    let repo = Repo::new(HashKind::Sha1);
    let ids = write_chain_pack(&repo.pack_dir(), revisions, 4095);
    let (_, depth) = deltas_in(&repo);
    assert_eq!(depth, (revisions - 1).min(4095));
    let set = PackSet::open(repo.pack_dir(), HashKind::Sha1);
    let pack = set.packs()[0].clone();
    // git agrees on the newest version.
    let last = ids.last().unwrap();
    let expect = repo.git(&["cat-file", "blob", &last.to_string()]);
    let got = pack.read(last, &mut DeltaBaseCache::default()).unwrap();
    assert_eq!(got.unwrap().data, expect);
    // Run on a thread with a small stack: recursion would overflow it.
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || {
            let mut cache = DeltaBaseCache::default();
            let mut state = 12345u64;
            for _ in 0..reads {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let i = (state >> 33) as usize % ids.len();
                let obj = pack.read(&ids[i], &mut cache).unwrap().unwrap();
                let h = rgit_hash::hash_object(HashKind::Sha1, obj.kind.name(), &obj.data);
                assert_eq!(h.unwrap(), ids[i]);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn deep_delta_chain() {
    if !have_git() {
        return;
    }
    deep_chain(600, 2000);
}

/// Done-when check of issue #17: chain depth 4095, 100k random reads.
/// Slow; run with `cargo test --release -- --ignored`.
#[test]
#[ignore]
fn deep_delta_chain_4095() {
    if !have_git() {
        return;
    }
    deep_chain(4200, 100_000);
}

/// Compare every object of an existing repository, for example the
/// upstream git clone: `RGIT_PACK_TEST_REPO=/path/to/repo cargo test
/// --release -- --ignored existing_repository`.
#[test]
#[ignore]
fn existing_repository() {
    let Some(dir) = std::env::var_os("RGIT_PACK_TEST_REPO") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let git_dir = PathBuf::from(
        String::from_utf8(git(&dir, &["rev-parse", "--absolute-git-dir"]))
            .unwrap()
            .trim(),
    );
    let fmt = String::from_utf8(git(&dir, &["rev-parse", "--show-object-format"])).unwrap();
    let kind = HashKind::from_name(fmt.trim()).unwrap();
    let set = PackSet::open(git_dir.join("objects/pack"), kind);
    let out = git(&dir, &["cat-file", "--batch-all-objects", "--batch"]);
    let expect = parse_batch(kind, &out);
    let mut cache = DeltaBaseCache::default();
    let mut found = 0;
    for (id, obj) in &expect {
        if let Some(got) = set.read(id, &mut cache, &mut |e| panic!("{e}")).unwrap() {
            assert_eq!(&got, obj, "{id}");
            found += 1;
        }
    }
    eprintln!("{found} of {} objects read from packs", expect.len());
}

// --- corruption (t5303 / t5313 behaviour) ---

fn munge(path: &Path, at: u64, bytes: &[u8]) {
    make_writable(path);
    let mut d = std::fs::read(path).unwrap();
    d[at as usize..at as usize + bytes.len()].copy_from_slice(bytes);
    std::fs::write(path, d).unwrap();
}

fn make_writable(path: &Path) {
    let mut perm = std::fs::metadata(path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(false);
    std::fs::set_permissions(path, perm).unwrap();
}

struct Corrupt {
    repo: Repo,
    blobs: Vec<ObjectId>,
}

/// Three similar blobs in one pack: one whole object, two deltas against
/// it (or a chain).
fn three_blobs(extra_pack_args: &[&str]) -> Corrupt {
    let repo = Repo::new(HashKind::Sha1);
    let mut text = String::new();
    for i in 0..50 {
        text.push_str(&format!("line {i}\n"));
    }
    let mut blobs = Vec::new();
    for suffix in ["", "x\n", "y\n"] {
        text.push_str(suffix);
        let out = git_input(
            &repo.dir,
            &["hash-object", "-w", "--stdin"],
            text.as_bytes(),
        );
        blobs.push(std::str::from_utf8(&out).unwrap().trim().parse().unwrap());
    }
    let list: String = blobs.iter().map(|b: &ObjectId| format!("{b}\n")).collect();
    let mut args = vec!["pack-objects", "-q"];
    args.extend_from_slice(extra_pack_args);
    args.push(".git/objects/pack/pack");
    git_input(&repo.dir, &args, list.as_bytes());
    repo.remove_loose();
    Corrupt { repo, blobs }
}

impl Corrupt {
    fn pack_files(&self) -> (PathBuf, PathBuf) {
        let idx = self.repo.packs().remove(0);
        (idx.with_extension("pack"), idx)
    }

    fn offset_of(&self, id: &ObjectId) -> u64 {
        let pack = Pack::open(self.pack_files().1, HashKind::Sha1).unwrap();
        let idx = pack.index();
        idx.offset_at(idx.lookup(id).unwrap()).unwrap()
    }

    /// Read every blob from a fresh set: Ok(true) readable, Ok(false)
    /// missing, Err corrupt. Errors reported along the way are collected.
    fn read_all(&self) -> (Vec<Result<bool, Error>>, Vec<String>) {
        let set = PackSet::open(self.repo.pack_dir(), HashKind::Sha1);
        let mut reported = Vec::new();
        let mut cache = DeltaBaseCache::default();
        let res = self
            .blobs
            .iter()
            .map(|id| {
                set.read(id, &mut cache, &mut |e| reported.push(e.to_string()))
                    .map(|o| o.is_some())
            })
            .collect();
        (res, reported)
    }
}

fn readable(r: &[Result<bool, Error>]) -> Vec<bool> {
    r.iter().map(|r| matches!(r, Ok(true))).collect()
}

#[test]
fn corrupt_header_of_base() {
    if !have_git() {
        return;
    }
    let c = three_blobs(&[]);
    let (pack, _) = c.pack_files();
    // The largest blob is the base and stored first.
    let base = c.blobs[2];
    munge(&pack, c.offset_of(&base), b"\0");
    let (res, reported) = c.read_all();
    assert_eq!(readable(&res), [false, false, false]);
    assert!(res.iter().all(|r| matches!(r, Err(Error::Corrupt { .. }))));
    assert!(
        reported
            .iter()
            .any(|m| m.starts_with("unknown object type 0 at offset 12 in "))
    );
    let msg = res[2].as_ref().unwrap_err().to_string();
    assert_eq!(
        msg,
        format!(
            "packed object {base} (stored in {}) is corrupt",
            pack.display()
        )
    );
}

#[test]
fn corrupt_data_of_base_reports_like_git() {
    if !have_git() {
        return;
    }
    let c = three_blobs(&[]);
    let (pack, _) = c.pack_files();
    let base = c.blobs[2];
    let ofs = c.offset_of(&base);
    munge(&pack, ofs + 2, b"\0");
    let set = PackSet::open(c.repo.pack_dir(), HashKind::Sha1);
    let mut cache = DeltaBaseCache::default();
    let mut reported: Vec<Error> = Vec::new();
    let err = set
        .read(&c.blobs[0], &mut cache, &mut |e| reported.push(e.clone()))
        .unwrap_err();
    assert!(matches!(err, Error::Corrupt { .. }));
    let chain = reported[0].chain();
    assert_eq!(
        chain,
        [
            "inflate: data stream error (incorrect header check)".to_string(),
            format!(
                "failed to read delta base object {base} at offset {ofs} from {}",
                pack.display()
            ),
        ]
    );
}

#[test]
fn corrupt_delta_data_keeps_other_objects() {
    if !have_git() {
        return;
    }
    let c = three_blobs(&[]);
    let (pack, _) = c.pack_files();
    munge(&pack, c.offset_of(&c.blobs[1]) + 4, b"\0\0\0\0");
    let (res, _) = c.read_all();
    let r = readable(&res);
    assert!(!r[1] && r[2], "{res:?}");
}

#[test]
fn redundant_pack_allows_recovery() {
    if !have_git() {
        return;
    }
    let c = three_blobs(&[]);
    let (pack, idx) = c.pack_files();
    // A second, intact copy of the same objects under another name.
    let other = c
        .repo
        .pack_dir()
        .join("pack-0000000000000000000000000000000000000000");
    std::fs::copy(&pack, other.with_extension("pack")).unwrap();
    std::fs::copy(&idx, other.with_extension("idx")).unwrap();
    munge(&pack, c.offset_of(&c.blobs[2]), b"\0");
    let (res, _) = c.read_all();
    assert_eq!(readable(&res), [true, true, true], "{res:?}");
}

#[test]
fn bogus_ofs_delta() {
    if !have_git() {
        return;
    }
    let c = three_blobs(&["--delta-base-offset"]);
    let (pack, idx_path) = c.pack_files();
    let saved = std::fs::read(&pack).unwrap();
    let idx = std::fs::read(&idx_path).unwrap();
    let ofs = c.offset_of(&c.blobs[0]);
    let h = Pack::open(c.pack_files().1, HashKind::Sha1)
        .unwrap()
        .data()
        .unwrap()
        .entry_header(ofs)
        .unwrap();
    assert!(matches!(h.kind, rgit_pack::EntryKind::OfsDelta { .. }));
    for bogus in [
        &b"\x7f\xff"[..],
        b"\x01",
        b"\x00",
        b"\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\x7f",
    ] {
        // Replace the offset field (after the type/size bytes) with the
        // bogus encoding.
        let mut field = ofs as usize;
        while saved[field] & 0x80 != 0 {
            field += 1;
        }
        field += 1;
        let mut new = saved[..field].to_vec();
        new.extend_from_slice(bogus);
        new.extend_from_slice(&saved[h.data_offset as usize..]);
        let p = Pack::from_bytes(idx.clone(), new, &pack, HashKind::Sha1).unwrap();
        let mut cache = DeltaBaseCache::default();
        assert!(p.read_at(ofs, &mut cache, &mut |_| None).is_err());
    }
}

#[test]
fn pack_index_count_mismatch_hides_pack() {
    if !have_git() {
        return;
    }
    let c = three_blobs(&[]);
    let (pack, _) = c.pack_files();
    munge(&pack, 8, b"\xff\0\0\0");
    let set = PackSet::open(c.repo.pack_dir(), HashKind::Sha1);
    // Enumeration still uses the index.
    assert_eq!(set.all_ids().len(), 3);
    let mut reported = Vec::new();
    let mut cache = DeltaBaseCache::default();
    let r = set
        .read(&c.blobs[0], &mut cache, &mut |e| {
            reported.push(e.to_string())
        })
        .unwrap();
    assert!(r.is_none());
    assert!(
        reported[0].contains("claims to have 4278190080 objects while index indicates 3 objects")
    );
}

#[test]
fn matched_bogus_count_rejects_index() {
    if !have_git() {
        return;
    }
    let c = three_blobs(&[]);
    let (pack, idx) = c.pack_files();
    munge(&pack, 8, b"\xff\0\0\0");
    munge(&idx, 8 + 255 * 4, b"\xff\0\0\0");
    let set = PackSet::open(c.repo.pack_dir(), HashKind::Sha1);
    assert!(set.all_ids().is_empty());
    assert_eq!(
        set.scan_errors()[0].to_string(),
        format!("wrong index v2 file size in {}", idx.display())
    );
}

#[test]
fn bogus_index_offsets() {
    if !have_git() {
        return;
    }
    // v1: offset of the first entry.
    let c = three_blobs(&["--index-version=1"]);
    let (_, idx) = c.pack_files();
    let first = Pack::open(&idx, HashKind::Sha1).unwrap().index().id_at(0);
    munge(&idx, 4 * 256, b"\xff\0\0\0");
    let p = Pack::open(&idx, HashKind::Sha1).unwrap();
    let err = p.read(&first, &mut DeltaBaseCache::default()).unwrap_err();
    assert_eq!(
        err.to_string(),
        "offset beyond end of packfile (truncated pack?)"
    );

    // v2, no msb.
    let c = three_blobs(&["--index-version=2"]);
    let (_, idx) = c.pack_files();
    let first = Pack::open(&idx, HashKind::Sha1).unwrap().index().id_at(0);
    let ofs_table = 8 + 1024 + 3 * (20 + 4);
    munge(&idx, ofs_table, b"\0\xff\0\0");
    let p = Pack::open(&idx, HashKind::Sha1).unwrap();
    assert!(p.read(&first, &mut DeltaBaseCache::default()).is_err());

    // v2, msb set but no 64-bit table.
    munge(&idx, ofs_table, b"\xff\0\0\0");
    let p = Pack::open(&idx, HashKind::Sha1).unwrap();
    let err = p.read(&first, &mut DeltaBaseCache::default()).unwrap_err();
    assert!(
        err.to_string()
            .starts_with("offset beyond end of pack index for ")
    );
}

#[test]
fn index_header_errors() {
    if !have_git() {
        return;
    }
    let c = three_blobs(&[]);
    let (pack, idx) = c.pack_files();
    let good_idx = std::fs::read(&idx).unwrap();
    let good_pack = std::fs::read(&pack).unwrap();

    let mut d = good_idx.clone();
    d[4..8].copy_from_slice(&7u32.to_be_bytes());
    let e = rgit_pack::PackIndex::from_bytes(d, &idx, HashKind::Sha1).unwrap_err();
    assert!(e.to_string().ends_with(
        "is version 7 and is not supported by this binary (try upgrading GIT to a newer version)"
    ));

    let e = rgit_pack::PackIndex::from_bytes(good_idx[..100].to_vec(), &idx, HashKind::Sha1)
        .unwrap_err();
    assert_eq!(
        e.to_string(),
        format!("index file {} is too small", idx.display())
    );

    let mut d = good_idx.clone();
    d[8 + 4 * 10..8 + 4 * 11].copy_from_slice(&[0xff, 0, 0, 0]);
    let e = rgit_pack::PackIndex::from_bytes(d, &idx, HashKind::Sha1).unwrap_err();
    assert!(e.to_string().starts_with("non-monotonic index "));

    let mut d = good_pack.clone();
    d[..4].copy_from_slice(b"XACK");
    let p = Pack::from_bytes(good_idx.clone(), d, &pack, HashKind::Sha1).unwrap();
    assert_eq!(
        p.data().unwrap_err().to_string(),
        format!("file {} is not a GIT packfile", pack.display())
    );

    let mut d = good_pack.clone();
    d[4..8].copy_from_slice(&5u32.to_be_bytes());
    let p = Pack::from_bytes(good_idx.clone(), d, &pack, HashKind::Sha1).unwrap();
    assert!(
        p.data()
            .unwrap_err()
            .to_string()
            .contains("is version 5 and not supported")
    );

    let mut d = good_pack.clone();
    let n = d.len();
    d[n - 1] ^= 1;
    let p = Pack::from_bytes(good_idx.clone(), d, &pack, HashKind::Sha1).unwrap();
    assert_eq!(
        p.data().unwrap_err().to_string(),
        format!("packfile {} does not match index", pack.display())
    );

    let p = Pack::from_bytes(
        good_idx.clone(),
        good_pack[..20].to_vec(),
        &pack,
        HashKind::Sha1,
    )
    .unwrap();
    assert!(p.data().is_err());

    // Checksums.
    let mut d = good_idx.clone();
    d[8 + 1024] ^= 1;
    let i = rgit_pack::PackIndex::from_bytes(d, &idx, HashKind::Sha1).unwrap();
    assert!(i.verify_checksum().is_err());
    let mut d = good_pack.clone();
    d[20] ^= 1;
    let data = rgit_pack::PackData::from_bytes(d, &pack, HashKind::Sha1).unwrap();
    assert!(data.verify_checksum().is_err());
}

/// Flip bytes all over a pack and its index: reading must never panic.
#[test]
fn random_corruption_never_panics() {
    if !have_git() {
        return;
    }
    let repo = Repo::new(HashKind::Sha1);
    repo.populate(6);
    repo.git(&["repack", "-adfq"]);
    let idx_path = repo.packs().remove(0);
    let good_idx = std::fs::read(&idx_path).unwrap();
    let good_pack = std::fs::read(idx_path.with_extension("pack")).unwrap();
    let ids: Vec<ObjectId> = Pack::open(&idx_path, HashKind::Sha1)
        .unwrap()
        .index()
        .ids()
        .collect();
    let mut state = 99u64;
    let mut next = || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) as usize
    };
    for round in 0..400 {
        let mut idx = good_idx.clone();
        let mut pack = good_pack.clone();
        for _ in 0..1 + next() % 4 {
            let target: &mut Vec<u8> = if round % 4 == 0 { &mut idx } else { &mut pack };
            let at = next() % target.len();
            target[at] = next() as u8;
        }
        if round % 7 == 0 {
            let l = next() % pack.len();
            pack.truncate(l);
        }
        let Ok(p) = Pack::from_bytes(idx, pack, "fuzz.pack", HashKind::Sha1) else {
            continue;
        };
        let mut cache = DeltaBaseCache::new(1 << 16);
        for id in &ids {
            let _ = p.read(id, &mut cache);
            if let Some(pos) = p.index().lookup(id) {
                if let Ok(o) = p.index().offset_at(pos) {
                    let _ = p.object_info(o);
                    let _ = p.delta_base(o);
                }
            }
        }
        let _ = p.verify(&mut cache);
    }
}
