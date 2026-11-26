// SPDX-License-Identifier: MIT
//! Changed-path Bloom filters (`Documentation/gitformat-commit-graph.adoc`,
//! chunks `BIDX` and `BDAT`).
//!
//! A filter holds the paths a commit changes against its first parent
//! (a root commit: against the empty tree), every leading directory of
//! such a path included. A path is hashed with 32-bit murmur3 under two
//! seeds; the `i`-th of `num_hashes` bit positions is `h0 + i * h1`
//! modulo the number of bits in the filter. Bit `n` is bit `n % 8` of byte
//! `n / 8`. A filter has `ceil(paths * bits_per_entry / 8)` bytes; a
//! commit without changes has the one byte `00`, a commit with more than
//! `max_changed_paths` changes the one byte `ff`.
//!
//! Version 1 hashes the bytes of a path as signed values, which changes
//! the hash of paths with bytes `>= 0x80`; version 2 is murmur3 as
//! published. Both are read and written.

/// Seed of the first hash.
const SEED0: u32 = 0x293a_e76f;
/// Seed of the second hash. The format documentation prints it as
/// `0x7e646e2`, one digit short; this value reproduces the hashes of the
/// reference implementation (`test-tool bloom generate_filter`).
const SEED1: u32 = 0x7e64_6e2c;

/// Default number of hashes per path.
pub const DEFAULT_NUM_HASHES: u32 = 7;
/// Default number of bits per path.
pub const DEFAULT_BITS_PER_ENTRY: u32 = 10;
/// Default limit of changed paths (more give a filter with all bits set).
pub const DEFAULT_MAX_CHANGED_PATHS: u32 = 512;

/// 32-bit murmur3 (MurmurHash3_x86_32) of `data`. With `signed`, every
/// byte is taken as a sign-extended signed value (changed-path filters
/// version 1).
pub fn murmur3(data: &[u8], seed: u32, signed: bool) -> u32 {
    const C1: u32 = 0xcc9e_2d51;
    const C2: u32 = 0x1b87_3593;
    let byte = |b: u8| -> u32 {
        if signed {
            i32::from(b as i8) as u32
        } else {
            u32::from(b)
        }
    };
    let mut h = seed;
    let (chunks, tail) = data.as_chunks::<4>();
    for c in chunks {
        let mut k = byte(c[0]) | (byte(c[1]) << 8) | (byte(c[2]) << 16) | (byte(c[3]) << 24);
        k = k.wrapping_mul(C1);
        k = k.rotate_left(15);
        k = k.wrapping_mul(C2);
        h ^= k;
        h = h.rotate_left(13);
        h = h.wrapping_mul(5).wrapping_add(0xe654_6b64);
    }
    let mut k: u32 = 0;
    if tail.len() >= 3 {
        k ^= byte(tail[2]) << 16;
    }
    if tail.len() >= 2 {
        k ^= byte(tail[1]) << 8;
    }
    if !tail.is_empty() {
        k ^= byte(tail[0]);
        k = k.wrapping_mul(C1);
        k = k.rotate_left(15);
        k = k.wrapping_mul(C2);
        h ^= k;
    }
    h ^= data.len() as u32;
    h ^= h >> 16;
    h = h.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2_ae35);
    h ^= h >> 16;
    h
}

/// The parameters of the filters of a commit-graph file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// 1 or 2 (see the module documentation).
    pub hash_version: u32,
    /// Bit positions per path.
    pub num_hashes: u32,
    /// Minimum bits per path.
    pub bits_per_entry: u32,
    /// More changed paths than this give a filter with all bits set. Not
    /// stored in the file.
    pub max_changed_paths: u32,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            hash_version: 1,
            num_hashes: DEFAULT_NUM_HASHES,
            bits_per_entry: DEFAULT_BITS_PER_ENTRY,
            max_changed_paths: DEFAULT_MAX_CHANGED_PATHS,
        }
    }
}

impl Settings {
    /// The settings stored in a file, without the limit.
    pub fn same_filters(&self, other: &Settings) -> bool {
        self.hash_version == other.hash_version
            && self.num_hashes == other.num_hashes
            && self.bits_per_entry == other.bits_per_entry
    }

    /// The `bloom/settings` trace2 value.
    pub fn json(&self) -> String {
        format!(
            "{{\"hash_version\":{},\"num_hashes\":{},\"bits_per_entry\":{},\"max_changed_paths\":{}}}",
            self.hash_version, self.num_hashes, self.bits_per_entry, self.max_changed_paths
        )
    }
}

/// The bit positions of one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    /// `num_hashes` hash values (positions before the modulo).
    pub hashes: Vec<u32>,
}

impl Key {
    /// Hash `path` (without a trailing slash).
    pub fn new(path: &[u8], settings: &Settings) -> Key {
        let signed = settings.hash_version == 1;
        let h0 = murmur3(path, SEED0, signed);
        let h1 = murmur3(path, SEED1, signed);
        let hashes = (0..settings.num_hashes)
            .map(|i| h0.wrapping_add(i.wrapping_mul(h1)))
            .collect();
        Key { hashes }
    }
}

/// Set the bits of `key` in `filter`.
pub fn add_key(filter: &mut [u8], key: &Key) {
    let bits = filter.len() as u64 * 8;
    if bits == 0 {
        return;
    }
    for &h in &key.hashes {
        let n = u64::from(h) % bits;
        filter[(n / 8) as usize] |= 1 << (n % 8);
    }
}

/// Whether `filter` may contain `key`. An empty filter (not computed)
/// may contain anything.
pub fn contains(filter: &[u8], key: &Key) -> bool {
    let bits = filter.len() as u64 * 8;
    if bits == 0 {
        return true;
    }
    key.hashes.iter().all(|&h| {
        let n = u64::from(h) % bits;
        filter[(n / 8) as usize] & (1 << (n % 8)) != 0
    })
}

/// What [`build`] made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Built {
    /// A filter of the paths.
    Normal,
    /// No changed paths: the single byte `00`.
    Empty,
    /// Too many changed paths: the single byte `ff`.
    Large,
}

/// Build the filter of `paths` (distinct, every leading directory
/// already included).
pub fn build(paths: &[Vec<u8>], settings: &Settings) -> (Vec<u8>, Built) {
    if paths.is_empty() {
        return (vec![0], Built::Empty);
    }
    if paths.len() > settings.max_changed_paths as usize {
        return (vec![0xff], Built::Large);
    }
    let bits = paths.len() as u64 * u64::from(settings.bits_per_entry);
    let len = bits.div_ceil(8).max(1) as usize;
    let mut filter = vec![0u8; len];
    for p in paths {
        add_key(&mut filter, &Key::new(p, settings));
    }
    (filter, Built::Normal)
}

/// The keys to look up for a path given on the command line: the path and
/// each of its leading directories, longest first.
pub fn keys_for_path(path: &[u8], settings: &Settings) -> Vec<Key> {
    let mut out = vec![Key::new(path, settings)];
    for (i, &b) in path.iter().enumerate().rev() {
        if b == b'/' && i > 0 {
            out.push(Key::new(&path[..i], settings));
        }
    }
    out
}

/// Whether some path has a byte `>= 0x80` (version 1 and version 2
/// filters differ only for such paths).
pub fn has_high_bit(paths: &[Vec<u8>]) -> bool {
    paths.iter().any(|p| p.iter().any(|&b| b >= 0x80))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(f: &[u8]) -> String {
        f.iter().map(|b| format!("{b:02x}|")).collect()
    }

    // Expected values from `test-tool bloom` of the reference git build.
    #[test]
    fn murmur3_unseeded() {
        assert_eq!(murmur3(b"", 0, false), 0);
        assert_eq!(murmur3(b"Hello world!", 0, false), 0x627b_0c2c);
        assert_eq!(
            murmur3(b"The quick brown fox jumps over the lazy dog", 0, false),
            0x2e4f_f723
        );
        assert_eq!(murmur3("äöü€x".as_bytes(), 0, false), 0x037e_0d3a);
    }

    #[test]
    fn keys_and_filters() {
        let s = Settings::default();
        let k = Key::new(b"", &s);
        assert_eq!(
            k.hashes,
            [
                0x5615800c, 0x5b966560, 0x61174ab4, 0x66983008, 0x6c19155c, 0x7199fab0, 0x771ae004
            ]
        );
        let (f, _) = build(&[b"".to_vec()], &s);
        assert_eq!(hex(&f), "11|11|");
        let (f, _) = build(&[b" ".to_vec()], &s);
        assert_eq!(hex(&f), "51|55|");
        let (f, _) = build(&[b"Hello world!".to_vec()], &s);
        assert_eq!(hex(&f), "92|6c|");
        let (f, _) = build(&[b"file.txt".to_vec()], &s);
        assert_eq!(hex(&f), "a5|4a|");
        assert!(contains(&f, &Key::new(b"file.txt", &s)));
    }

    #[test]
    fn ten_files_in_a_directory() {
        let s = Settings::default();
        let mut paths: Vec<Vec<u8>> = (0..10)
            .map(|i| format!("smallDir/{i}").into_bytes())
            .collect();
        paths.push(b"smallDir".to_vec());
        let (f, b) = build(&paths, &s);
        assert_eq!(b, Built::Normal);
        assert_eq!(hex(&f), "02|b3|c4|a0|34|e7|fe|eb|cb|47|fe|a0|e8|72|");
    }

    #[test]
    fn empty_and_large() {
        let s = Settings::default();
        assert_eq!(build(&[], &s), (vec![0], Built::Empty));
        let many: Vec<Vec<u8>> = (0..513).map(|i| format!("d/{i}").into_bytes()).collect();
        assert_eq!(build(&many, &s), (vec![0xff], Built::Large));
    }

    #[test]
    fn version_1_hashes_high_bit_bytes_as_signed() {
        let v1 = Settings::default();
        let v2 = Settings {
            hash_version: 2,
            ..Settings::default()
        };
        let cases: [(&str, u32); 5] = [
            ("¢", 0x8095ff0b),
            ("a¢", 0x1404566c),
            ("x¢yz", 0x7615c12b),
            ("äöü€x", 0x7219a937),
            ("¢¢¢¢¢", 0x2bb92e0e),
        ];
        for (s, h0) in cases {
            let k1 = Key::new(s.as_bytes(), &v1).hashes[0];
            let k2 = Key::new(s.as_bytes(), &v2).hashes[0];
            eprintln!("{s}: v1 {k1:08x} v2 {k2:08x} want {h0:08x}");
            assert_eq!(k1, h0, "{s}");
            assert_ne!(k2, h0, "{s}");
        }
    }
}
