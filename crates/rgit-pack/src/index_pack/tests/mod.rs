// SPDX-License-Identifier: MIT

//! Unit tests with packs built here (no git binary needed). Differential
//! tests against git are in `rgit-cli/tests/index_pack.rs`.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Mutex;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use rgit_hash::{HashKind, Hasher, ObjectId, hash_object};

use super::*;
use crate::{DeltaBaseCache, Pack, PackIndex};

/// A pack under construction.
struct Builder {
    hash: HashKind,
    entries: Vec<Vec<u8>>,
    offsets: Vec<u64>,
}

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
    z.write_all(data).unwrap();
    z.finish().unwrap()
}

fn header(ty: u8, mut size: u64) -> Vec<u8> {
    let mut out = vec![(ty << 4) | (size & 15) as u8];
    size >>= 4;
    while size != 0 {
        *out.last_mut().unwrap() |= 0x80;
        out.push((size & 0x7f) as u8);
        size >>= 7;
    }
    out
}

fn varint(mut n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

/// A delta that rebuilds `target` from `base`: copy a common prefix, insert
/// the rest.
fn make_delta(base: &[u8], target: &[u8]) -> Vec<u8> {
    let mut d = varint(base.len() as u64);
    d.extend(varint(target.len() as u64));
    let common = base
        .iter()
        .zip(target)
        .take_while(|(a, b)| a == b)
        .count()
        .min(0xffff);
    let mut pos = 0;
    if common > 0 {
        // copy offset 0, size `common` (two size bytes).
        d.extend([
            0x80 | 0x10 | 0x20,
            (common & 0xff) as u8,
            (common >> 8) as u8,
        ]);
        pos = common;
    }
    for chunk in target[pos..].chunks(127) {
        d.push(chunk.len() as u8);
        d.extend_from_slice(chunk);
    }
    d
}

fn blob_id(hash: HashKind, data: &[u8]) -> ObjectId {
    hash_object(hash, "blob", data).unwrap()
}

impl Builder {
    fn new(hash: HashKind) -> Builder {
        Builder {
            hash,
            entries: Vec::new(),
            offsets: Vec::new(),
        }
    }

    fn next_offset(&self) -> u64 {
        12 + self.entries.iter().map(|e| e.len() as u64).sum::<u64>()
    }

    fn push(&mut self, e: Vec<u8>) -> usize {
        self.offsets.push(self.next_offset());
        self.entries.push(e);
        self.entries.len() - 1
    }

    fn object(&mut self, kind: ObjectKind, data: &[u8]) -> usize {
        let mut e = header(kind.pack_type(), data.len() as u64);
        e.extend(zlib(data));
        self.push(e)
    }

    fn blob(&mut self, data: &[u8]) -> usize {
        self.object(ObjectKind::Blob, data)
    }

    fn ref_delta(&mut self, base: &[u8], target: &[u8]) -> usize {
        let d = make_delta(base, target);
        let mut e = header(7, d.len() as u64);
        e.extend_from_slice(blob_id(self.hash, base).as_bytes());
        e.extend(zlib(&d));
        self.push(e)
    }

    fn raw_ref_delta(&mut self, base_id: ObjectId, delta: &[u8]) -> usize {
        let mut e = header(7, delta.len() as u64);
        e.extend_from_slice(base_id.as_bytes());
        e.extend(zlib(delta));
        self.push(e)
    }

    fn ofs_delta(&mut self, base_entry: usize, base: &[u8], target: &[u8]) -> usize {
        let d = make_delta(base, target);
        let mut e = header(6, d.len() as u64);
        let mut rel = self.next_offset() - self.offsets[base_entry];
        let mut enc = vec![(rel & 0x7f) as u8];
        rel >>= 7;
        while rel != 0 {
            rel -= 1;
            enc.insert(0, 0x80 | (rel & 0x7f) as u8);
            rel >>= 7;
        }
        e.extend(enc);
        e.extend(zlib(&d));
        self.push(e)
    }

    fn finish(&self) -> Vec<u8> {
        let mut out = b"PACK".to_vec();
        out.extend(2u32.to_be_bytes());
        out.extend((self.entries.len() as u32).to_be_bytes());
        for e in &self.entries {
            out.extend(e);
        }
        let mut h = Hasher::new(self.hash);
        h.update(&out);
        out.extend_from_slice(h.finalize().unwrap().as_bytes());
        out
    }
}

/// Index `pack` as a file.
fn index_file(pack: &[u8], opts: &Options) -> Result<IndexedPack, Error> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.pack");
    std::fs::write(&path, pack).unwrap();
    index_pack(Input::File(&path), opts, None, None, None)
}

/// Index `pack` as a stream; returns the result and the written file.
fn index_stream(
    pack: &[u8],
    opts: &Options,
    db: Option<&dyn ObjectDb>,
) -> (Result<IndexedPack, Error>, Vec<u8>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.pack");
    let mut dest = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    let mut reader = pack;
    let r = index_pack(
        Input::Stream {
            reader: &mut reader,
            dest: &mut dest,
            dest_path: &path,
        },
        opts,
        db,
        None,
        None,
    );
    drop(dest);
    let mut written = Vec::new();
    std::fs::File::open(&path)
        .unwrap()
        .read_to_end(&mut written)
        .unwrap();
    (r, written)
}

/// Check every object through the existing reader: the written index must
/// open, and every listed object must read back with its id.
fn check_with_reader(pack: &[u8], indexed: &IndexedPack, v: IndexVersion) {
    let idx = indexed.index_bytes(v).unwrap();
    let p = Pack::from_bytes(idx, pack.to_vec(), "t.pack", indexed.hash_kind()).unwrap();
    let mut cache = DeltaBaseCache::new(1 << 20);
    p.verify(&mut cache).unwrap();
    for o in indexed.objects() {
        let obj = p.read(&o.id, &mut cache).unwrap().unwrap();
        assert_eq!(obj.kind, o.kind);
        assert_eq!(
            hash_object(indexed.hash_kind(), o.kind.as_str(), &obj.data).unwrap(),
            o.id
        );
    }
}

fn opts() -> Options {
    Options::new(HashKind::Sha1)
}

#[test]
fn whole_objects_and_deltas() {
    for hash in HashKind::ALL {
        let mut b = Builder::new(hash);
        let base = b"line one\nline two\nline three\n".repeat(20);
        let mut v2 = base.clone();
        v2.extend_from_slice(b"more\n");
        let mut v3 = v2.clone();
        v3.extend_from_slice(b"even more\n");
        let e0 = b.blob(&base);
        let e1 = b.ofs_delta(e0, &base, &v2);
        b.ref_delta(&v2, &v3);
        b.object(ObjectKind::Commit, b"tree 0\n");
        let _ = e1;
        let pack = b.finish();
        let mut o = Options::new(hash);
        o.threads = 2;
        let r = index_file(&pack, &o).unwrap();
        assert_eq!(r.objects().len(), 4);
        assert_eq!(r.deltas(), 2);
        assert_eq!(r.objects()[2].id, blob_id(hash, &v3));
        assert_eq!(
            r.checksum().as_bytes(),
            &pack[pack.len() - hash.byte_len()..]
        );
        check_with_reader(&pack, &r, IndexVersion::default());
        check_with_reader(
            &pack,
            &r,
            IndexVersion {
                version: 1,
                ..IndexVersion::default()
            },
        );
        check_with_reader(
            &pack,
            &r,
            IndexVersion {
                version: 2,
                large_offset_limit: 20,
            },
        );
        // The stream variant writes the same bytes.
        let (s, written) = index_stream(&pack, &o, None);
        assert_eq!(written, pack);
        assert_eq!(s.unwrap().objects(), r.objects());
    }
}

/// Long chains, wide fans and a tiny memory budget, so that bases are
/// dropped and rebuilt; every thread count gives the same result.
#[test]
fn deep_chains_with_eviction() {
    let mut b = Builder::new(HashKind::Sha1);
    let mut versions = Vec::new();
    let mut cur: Vec<u8> = (0..60_000u32).map(|i| (i % 251) as u8).collect();
    let root = b.blob(&cur);
    versions.push(cur.clone());
    let mut prev = root;
    for i in 0..60u32 {
        let mut next = cur.clone();
        next.extend_from_slice(format!("step {i}\n").as_bytes());
        prev = if i % 3 == 0 {
            b.ref_delta(&cur, &next)
        } else {
            b.ofs_delta(prev, &cur, &next)
        };
        cur = next;
        versions.push(cur.clone());
    }
    // A fan below the root.
    for i in 0..30u32 {
        let mut next = versions[0].clone();
        next.extend_from_slice(format!("fan {i}\n").as_bytes());
        b.ofs_delta(root, &versions[0], &next);
    }
    let pack = b.finish();
    let mut reference = None;
    for threads in [1, 2, 7] {
        let mut o = opts();
        o.threads = threads;
        o.memory_limit = 1; // forces the per-thread minimum
        let r = index_file(&pack, &o).unwrap();
        assert_eq!(r.deltas(), 90);
        let idx = r.index_bytes(IndexVersion::default()).unwrap();
        match &reference {
            None => reference = Some(idx),
            Some(x) => assert_eq!(x, &idx),
        }
        check_with_reader(&pack, &r, IndexVersion::default());
    }
}

#[test]
fn unresolved_and_cycles() {
    let mut b = Builder::new(HashKind::Sha1);
    b.ref_delta(b"hello world", b"hello there");
    let pack = b.finish();
    let e = index_file(&pack, &opts()).unwrap_err();
    assert_eq!(e.to_string(), "pack has 1 unresolved delta");

    let mut b = Builder::new(HashKind::Sha1);
    b.ref_delta(b"hello world", b"hello there");
    b.ref_delta(b"hello there", b"hello world");
    let pack = b.finish();
    let e = index_file(&pack, &opts()).unwrap_err();
    assert_eq!(e.to_string(), "pack has 2 unresolved deltas");

    // A whole copy breaks the cycle; each delta is resolved once.
    b.blob(b"hello world");
    let pack = b.finish();
    let r = index_file(&pack, &opts()).unwrap();
    assert_eq!(r.objects().len(), 3);
    assert!(r.check_duplicates().is_err());
}

struct MapDb(HashMap<ObjectId, Object>);

impl ObjectDb for MapDb {
    fn contains(&self, id: &ObjectId) -> bool {
        self.0.contains_key(id)
    }
    fn read(&self, id: &ObjectId) -> Result<Option<Object>, String> {
        Ok(self.0.get(id).cloned())
    }
}

fn db_with(objects: &[&[u8]]) -> MapDb {
    MapDb(
        objects
            .iter()
            .map(|d| {
                (
                    blob_id(HashKind::Sha1, d),
                    Object {
                        kind: ObjectKind::Blob,
                        data: d.to_vec(),
                    },
                )
            })
            .collect(),
    )
}

#[test]
fn fix_thin_appends_bases() {
    let mut b = Builder::new(HashKind::Sha1);
    b.ref_delta(b"hello world", b"hello there");
    b.ref_delta(b"hello there", b"hello again");
    b.ref_delta(b"another base", b"another child");
    let thin = b.finish();
    let db = db_with(&[b"hello world", b"another base"]);
    let mut o = opts();
    o.fix_thin = true;
    let (r, written) = index_stream(&thin, &o, Some(&db));
    let r = r.unwrap();
    assert_eq!(r.local_objects(), 2);
    assert_eq!(r.objects().len(), 5);
    assert_eq!(r.pack_len(), written.len() as u64);
    assert_eq!(&written[8..12], &5u32.to_be_bytes());
    check_with_reader(&written, &r, IndexVersion::default());

    // Without the option, or with a base missing everywhere, it fails.
    let (r, _) = index_stream(&thin, &opts(), Some(&db));
    assert_eq!(r.unwrap_err().to_string(), "pack has 3 unresolved deltas");
    let db = db_with(&[b"hello world"]);
    let (r, _) = index_stream(&thin, &o, Some(&db));
    assert_eq!(r.unwrap_err().to_string(), "pack has 1 unresolved delta");
}

#[test]
fn collision_with_existing_object() {
    let mut b = Builder::new(HashKind::Sha1);
    b.blob(b"a\n");
    let pack = b.finish();
    let id = blob_id(HashKind::Sha1, b"a\n");
    let db = MapDb(HashMap::from([(
        id,
        Object {
            kind: ObjectKind::Blob,
            data: b"b\n".to_vec(),
        },
    )]));
    let (r, _) = index_stream(&pack, &opts(), Some(&db));
    assert_eq!(
        r.unwrap_err().to_string(),
        format!("SHA1 COLLISION FOUND WITH {id} !")
    );
    // Same content is fine.
    let db = db_with(&[b"a\n"]);
    assert!(index_stream(&pack, &opts(), Some(&db)).0.is_ok());
}

#[test]
fn hook_sees_every_object_and_can_abort() {
    struct Count(Mutex<Vec<ObjectId>>, bool);
    impl ObjectHook for Count {
        fn object(&self, id: &ObjectId, _: ObjectKind, _: &[u8]) -> Result<(), String> {
            self.0.lock().unwrap().push(*id);
            if self.1 { Err("nope".into()) } else { Ok(()) }
        }
    }
    let mut b = Builder::new(HashKind::Sha1);
    b.blob(b"hello world");
    b.ref_delta(b"hello world", b"hello there");
    let pack = b.finish();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.pack");
    std::fs::write(&path, &pack).unwrap();
    let hook = Count(Mutex::new(Vec::new()), false);
    index_pack(Input::File(&path), &opts(), None, Some(&hook), None).unwrap();
    assert_eq!(hook.0.lock().unwrap().len(), 2);
    let hook = Count(Mutex::new(Vec::new()), true);
    let e = index_pack(Input::File(&path), &opts(), None, Some(&hook), None).unwrap_err();
    assert_eq!(e.to_string(), "nope");
}

#[test]
fn corrupt_input_messages() {
    let mut b = Builder::new(HashKind::Sha1);
    b.blob(b"hello world");
    b.ref_delta(b"hello world", b"hello there");
    let good = b.finish();
    let msg = |p: &[u8]| format!("fatal: {}", index_file(p, &opts()).unwrap_err());

    assert_eq!(msg(&good[..30]), "fatal: early EOF");
    assert_eq!(msg(&good[..5]), "fatal: early EOF");
    let mut bad = good.clone();
    *bad.last_mut().unwrap() ^= 1;
    assert_eq!(msg(&bad), "fatal: pack is corrupted (SHA1 mismatch)");
    let mut junk = good.clone();
    junk.extend_from_slice(b"junk");
    assert_eq!(msg(&junk), "fatal: pack has junk at the end");
    let mut sig = good.clone();
    sig[0] = b'X';
    assert_eq!(msg(&sig), "fatal: pack signature mismatch");
    let mut ver = good.clone();
    ver[7] = 4;
    assert_eq!(msg(&ver), "fatal: pack version 4 unsupported");
    let mut ty = good.clone();
    ty[12] = (ty[12] & 0x8f) | (5 << 4);
    assert_eq!(
        msg(&ty),
        "fatal: pack has bad object at offset 12: unknown object type 5"
    );

    // Sizes that do not match the zlib stream.
    for (declared, code) in [(5u64, -5), (20, 1)] {
        let mut b = Builder::new(HashKind::Sha1);
        let mut e = header(3, declared);
        e.extend(zlib(b"hello world"));
        b.push(e);
        assert_eq!(
            msg(&b.finish()),
            format!("fatal: pack has bad object at offset 12: inflate returned {code}")
        );
    }

    // Broken deltas and the error line before the fatal one.
    let base_id = blob_id(HashKind::Sha1, b"hello world");
    for (delta, note) in [
        (&[0x0b, 0x05, 0x00][..], None),
        (
            &[0x0b, 0x05, 0x02, 0x01, 0x02][..],
            Some("delta replay has gone wild"),
        ),
        (
            &[0x0b, 0x05, 0x00, 0x00, 0x00][..],
            Some("unexpected delta opcode 0"),
        ),
        (&[0x0c, 0x05, 0x91, 0x00, 0x0a][..], None),
    ] {
        let mut b = Builder::new(HashKind::Sha1);
        b.blob(b"hello world");
        b.raw_ref_delta(base_id, delta);
        let e = index_file(&b.finish(), &opts()).unwrap_err();
        assert_eq!(
            format!("fatal: {e}"),
            "fatal: pack has bad object at offset 32: failed to apply delta"
        );
        assert_eq!(
            e.errors_before(),
            note.map(|n: &str| vec![n.to_string()]).unwrap_or_default()
        );
    }

    let mut o = opts();
    o.max_input_size = Some(good.len() as u64 - 1);
    assert_eq!(
        format!("fatal: {}", index_file(&good, &o).unwrap_err()),
        format!(
            "fatal: pack exceeds maximum allowed size ({} bytes)",
            good.len() - 1
        )
    );
    o.max_input_size = Some(good.len() as u64);
    assert!(index_file(&good, &o).is_ok());
}

#[test]
fn duplicates_and_pack_header() {
    let mut b = Builder::new(HashKind::Sha1);
    b.blob(b"x");
    b.blob(b"y");
    b.blob(b"x");
    let pack = b.finish();
    let r = index_file(&pack, &opts()).unwrap();
    let mut o = opts();
    o.reject_duplicates = true;
    assert_eq!(
        format!("fatal: {}", index_file(&pack, &o).unwrap_err()),
        format!(
            "fatal: The same object {} appears twice in the pack",
            blob_id(HashKind::Sha1, b"x")
        )
    );
    // Duplicates are ordered by offset in the index.
    let idx = PackIndex::from_bytes(
        r.index_bytes(IndexVersion::default()).unwrap(),
        "t",
        HashKind::Sha1,
    )
    .unwrap();
    assert_eq!(idx.len(), 3);

    // --pack_header: the stream starts after the header.
    let mut o = opts();
    o.pack_header = Some((2, 3));
    let (s, written) = index_stream(&pack[12..], &o, None);
    assert_eq!(written, pack);
    assert_eq!(s.unwrap().checksum(), r.checksum());
}

#[test]
fn trailing_data_on_a_stream() {
    let mut b = Builder::new(HashKind::Sha1);
    b.blob(b"hello");
    let pack = b.finish();
    let mut input = pack.clone();
    input.extend_from_slice(b"rest");
    let (r, written) = index_stream(&input, &opts(), None);
    assert_eq!(r.unwrap_err().to_string(), "pack has junk at the end");
    let mut o = opts();
    o.allow_trailing_data = true;
    let (r, written2) = index_stream(&input, &o, None);
    assert_eq!(r.unwrap().trailing_data(), b"rest");
    assert_eq!(written2, pack);
    assert_eq!(written, pack);
}

#[test]
fn rev_index_layout() {
    let mut b = Builder::new(HashKind::Sha1);
    for i in 0..10u8 {
        b.blob(&[i; 3]);
    }
    let pack = b.finish();
    let r = index_file(&pack, &opts()).unwrap();
    let rev = r.rev_bytes().unwrap();
    assert_eq!(&rev[..12], b"RIDX\0\0\0\x01\0\0\0\x01");
    assert_eq!(rev.len(), 12 + 4 * 10 + 40);
    // Entry k of the table is the index position of the k-th object in
    // pack order.
    let mut sorted: Vec<ObjectId> = r.objects().iter().map(|o| o.id).collect();
    sorted.sort();
    for (k, o) in r.objects().iter().enumerate() {
        let pos = u32::from_be_bytes(rev[12 + 4 * k..16 + 4 * k].try_into().unwrap());
        assert_eq!(sorted[pos as usize], o.id);
    }
}

#[test]
fn index_version_of_existing_index() {
    let mut b = Builder::new(HashKind::Sha1);
    for i in 0..10u8 {
        b.blob(&[i; 30]);
    }
    let pack = b.finish();
    let r = index_file(&pack, &opts()).unwrap();
    for v in [
        IndexVersion::default(),
        IndexVersion {
            version: 1,
            ..IndexVersion::default()
        },
        IndexVersion {
            version: 2,
            large_offset_limit: 100,
        },
    ] {
        let idx = r.index_bytes(v).unwrap();
        let back = IndexVersion::of(&idx, HashKind::Sha1).unwrap();
        assert_eq!(r.index_bytes(back).unwrap(), idx);
    }
    assert!(IndexVersion::of(b"short", HashKind::Sha1).is_none());
}

#[test]
fn humanised_sizes() {
    assert_eq!(messages::humanise_bytes(20), "20 bytes");
    assert_eq!(messages::humanise_bytes(2000), "1.95 KiB");
    assert_eq!(messages::humanise_bytes(3_000_000), "2.86 MiB");
}
