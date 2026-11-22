// SPDX-License-Identifier: MIT

//! Creating deltas (`Documentation/gitformat-pack.adoc`, "Deltified
//! representation").
//!
//! The base is cut into blocks of [`BLOCK`] bytes; a table maps the hash
//! of each block to the places it occurs. The target is scanned with a
//! rolling hash over every [`BLOCK`]-byte window. Where a window's hash
//! names base blocks, the candidates are compared byte by byte, the
//! longest match is extended backwards over the literal bytes collected
//! so far and forwards as far as it goes, and emitted as a copy. Bytes
//! that match nothing become insert instructions.

/// Block length for the base table and the rolling hash.
const BLOCK: usize = 16;
/// Positions kept per hash value; repetitive bases would otherwise make
/// the scan quadratic.
const MAX_PER_HASH: usize = 64;
/// Longest copy written as one instruction. The format allows three size
/// bytes, but 64 KiB copies are what every reader handles.
const MAX_COPY: usize = 0x10000;
/// Longest insert instruction.
const MAX_INSERT: usize = 0x7f;
/// Largest base offset a copy instruction can address.
const MAX_OFFSET: usize = u32::MAX as usize;

/// Multiplier of the polynomial rolling hash.
const MUL: u32 = 0x0100_0193;

/// A pseudo-random 32-bit value per byte value, so that the polynomial
/// hash mixes all bits of the input bytes.
const BYTE_MIX: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    let mut i = 0;
    while i < 256 {
        // splitmix64
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        t[i] = z as u32;
        i += 1;
    }
    t
};

/// `MUL` to the power `BLOCK - 1`: the weight of the byte that leaves the
/// window.
const OUT_WEIGHT: u32 = {
    let mut w = 1u32;
    let mut i = 1;
    while i < BLOCK {
        w = w.wrapping_mul(MUL);
        i += 1;
    }
    w
};

fn block_hash(block: &[u8]) -> u32 {
    block.iter().fold(0u32, |h, &b| {
        h.wrapping_mul(MUL).wrapping_add(BYTE_MIX[b as usize])
    })
}

fn roll(h: u32, out: u8, inp: u8) -> u32 {
    h.wrapping_sub(BYTE_MIX[out as usize].wrapping_mul(OUT_WEIGHT))
        .wrapping_mul(MUL)
        .wrapping_add(BYTE_MIX[inp as usize])
}

/// The block table of one base.
#[derive(Debug, Clone)]
pub(crate) struct DeltaIndex {
    /// `bits` high bits of a mixed hash select the bucket.
    bits: u32,
    /// Bucket `b` holds `entries[starts[b]..starts[b + 1]]`.
    starts: Vec<u32>,
    /// (block hash, block offset in the base).
    entries: Vec<(u32, u32)>,
    /// Length of the base the table was built for.
    base_len: usize,
}

impl DeltaIndex {
    /// Index `base`. Bases of 4 GiB or more cannot be addressed by copy
    /// instructions; for them the table stays empty.
    pub(crate) fn new(base: &[u8]) -> DeltaIndex {
        let blocks = if base.len() > MAX_OFFSET {
            0
        } else {
            base.len() / BLOCK
        };
        let bits = (usize::BITS - blocks.max(1).leading_zeros()).clamp(1, 28);
        let buckets = 1usize << bits;
        let mut hashes: Vec<(u32, u32)> = (0..blocks)
            .map(|i| {
                let at = i * BLOCK;
                (block_hash(&base[at..at + BLOCK]), at as u32)
            })
            .collect();
        // Group by bucket, then by hash, latest offsets first so the cap
        // keeps those (later parts of a file change less often in
        // practice, and any choice is valid).
        let bucket_of = |h: u32| bucket(h, bits);
        hashes.sort_unstable_by(|a, b| {
            (bucket_of(a.0), a.0, std::cmp::Reverse(a.1)).cmp(&(
                bucket_of(b.0),
                b.0,
                std::cmp::Reverse(b.1),
            ))
        });
        let mut entries = Vec::with_capacity(hashes.len());
        let mut run = 0usize;
        for (i, e) in hashes.iter().enumerate() {
            if i > 0 && hashes[i - 1].0 == e.0 {
                run += 1;
            } else {
                run = 0;
            }
            if run < MAX_PER_HASH {
                entries.push(*e);
            }
        }
        let mut starts = vec![0u32; buckets + 1];
        for e in &entries {
            starts[bucket_of(e.0) + 1] += 1;
        }
        for b in 0..buckets {
            starts[b + 1] += starts[b];
        }
        DeltaIndex {
            bits,
            starts,
            entries,
            base_len: base.len(),
        }
    }

    /// Approximate memory use in bytes.
    pub(crate) fn memory(&self) -> usize {
        self.starts.len() * 4 + self.entries.len() * 8
    }

    fn candidates(&self, h: u32) -> impl Iterator<Item = usize> + '_ {
        let b = bucket(h, self.bits);
        let (from, to) = (self.starts[b] as usize, self.starts[b + 1] as usize);
        self.entries[from..to]
            .iter()
            .filter(move |e| e.0 == h)
            .map(|e| e.1 as usize)
    }
}

fn bucket(h: u32, bits: u32) -> usize {
    (h.wrapping_mul(0x9e37_79b1) >> (32 - bits)) as usize
}

/// Length of the common prefix of `a` and `b`.
fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    let n = a.len().min(b.len());
    let mut i = 0;
    while i + 8 <= n {
        let x = u64::from_le_bytes(a[i..i + 8].try_into().expect("8 bytes"));
        let y = u64::from_le_bytes(b[i..i + 8].try_into().expect("8 bytes"));
        let d = x ^ y;
        if d != 0 {
            return i + (d.trailing_zeros() / 8) as usize;
        }
        i += 8;
    }
    while i < n && a[i] == b[i] {
        i += 1;
    }
    i
}

fn push_size(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let low = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(low);
            return;
        }
        out.push(0x80 | low);
    }
}

fn push_inserts(out: &mut Vec<u8>, mut data: &[u8]) {
    while !data.is_empty() {
        let n = data.len().min(MAX_INSERT);
        out.push(n as u8);
        out.extend_from_slice(&data[..n]);
        data = &data[n..];
    }
}

fn push_copies(out: &mut Vec<u8>, mut offset: usize, mut len: usize) {
    while len > 0 {
        let n = len.min(MAX_COPY);
        let at = out.len();
        out.push(0x80);
        let mut op = 0x80u8;
        for (bit, byte) in (offset as u32).to_le_bytes().into_iter().enumerate() {
            if byte != 0 {
                op |= 1 << bit;
                out.push(byte);
            }
        }
        // A size of 0x10000 is written as no size bytes at all.
        if n != MAX_COPY {
            for (bit, byte) in (n as u32).to_le_bytes()[..3].iter().enumerate() {
                if *byte != 0 {
                    op |= 0x10 << bit;
                    out.push(*byte);
                }
            }
        }
        out[at] = op;
        offset += n;
        len -= n;
    }
}

/// A run of target bytes found in the base.
#[derive(Debug, Clone, Copy)]
struct Match {
    /// Start in the target.
    start: usize,
    /// Start in the base.
    at: usize,
    len: usize,
}

impl Match {
    fn end(&self) -> usize {
        self.start + self.len
    }
}

/// The longest run through target position `i` (whose window hash is
/// `h`) that starts at an indexed base block, extended backwards to at
/// most `floor`. Runs shorter than a block are not reported.
fn best_match(
    base: &[u8],
    index: &DeltaIndex,
    target: &[u8],
    i: usize,
    h: u32,
    floor: usize,
) -> Option<Match> {
    let mut best: Option<Match> = None;
    for at in index.candidates(h) {
        let fwd = common_prefix(&base[at..], &target[i..]);
        if fwd < BLOCK {
            continue;
        }
        let mut back = 0;
        while i - back > floor && at > back && target[i - back - 1] == base[at - back - 1] {
            back += 1;
        }
        let m = Match {
            start: i - back,
            at: at - back,
            len: back + fwd,
        };
        if best.is_none_or(|b| m.len > b.len) {
            best = Some(m);
            if m.end() == target.len() {
                break;
            }
        }
    }
    best
}

/// Shortest piece of a match that is still written as a copy when a
/// better match takes over the rest; shorter ones become literals.
const MIN_KEPT_COPY: usize = 8;

/// A delta that turns `base` into `target`, using `index` (built from
/// `base`). `None` when the delta would be longer than `max_size` bytes.
///
/// After a match is found, the next [`BLOCK`] positions are tried too: in
/// repetitive text the first match is often a short one elsewhere in the
/// base, while the run that continues for long only starts at the next
/// indexed block.
pub(crate) fn create_delta(
    base: &[u8],
    index: &DeltaIndex,
    target: &[u8],
    max_size: usize,
) -> Option<Vec<u8>> {
    debug_assert_eq!(index.base_len, base.len());
    let mut out = Vec::with_capacity((target.len() / 4).min(max_size) + 16);
    push_size(&mut out, base.len() as u64);
    push_size(&mut out, target.len() as u64);
    // Every pending literal costs at least one byte; give up as soon as
    // the delta cannot fit any more.
    let over = |out: &Vec<u8>, pending: usize| {
        out.len() + pending + pending.div_ceil(MAX_INSERT) > max_size
    };
    let mut literal_from = 0usize;
    let mut i = 0usize;
    let mut h = 0u32;
    let mut hashed = false;
    let mut kept: Vec<Match> = Vec::new();
    while i + BLOCK <= target.len() {
        if !hashed {
            h = block_hash(&target[i..i + BLOCK]);
            hashed = true;
        }
        let Some(mut m) = best_match(base, index, target, i, h, literal_from) else {
            if i + BLOCK < target.len() {
                h = roll(h, target[i], target[i + BLOCK]);
            }
            i += 1;
            if (i - literal_from) % 256 == 0 && over(&out, i - literal_from) {
                return None;
            }
            continue;
        };
        // Look ahead for a run reaching further.
        kept.clear();
        let (mut j, mut hj) = (i, h);
        for _ in 0..BLOCK {
            if j + BLOCK >= target.len() || j + 1 >= m.end() {
                break;
            }
            hj = roll(hj, target[j], target[j + BLOCK]);
            j += 1;
            let Some(n) = best_match(base, index, target, j, hj, literal_from) else {
                continue;
            };
            if n.end() <= m.end() || n.len <= m.len {
                continue;
            }
            // Keep what the old run covers before the new one, if worth it.
            kept.retain(|k| k.start < n.start);
            if let Some(last) = kept.last_mut() {
                last.len = last.len.min(n.start - last.start);
            }
            if n.start > m.start {
                let head = Match {
                    len: n.start - m.start,
                    ..m
                };
                if head.len >= MIN_KEPT_COPY {
                    kept.push(head);
                }
            }
            kept.retain(|k| k.len >= MIN_KEPT_COPY);
            m = n;
        }
        kept.push(m);
        for k in &kept {
            push_inserts(&mut out, &target[literal_from..k.start]);
            push_copies(&mut out, k.at, k.len);
            literal_from = k.end();
        }
        i = literal_from;
        hashed = false;
        if over(&out, 0) {
            return None;
        }
    }
    let rest = target.len() - literal_from;
    if over(&out, rest) {
        return None;
    }
    push_inserts(&mut out, &target[literal_from..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply_delta;

    fn check(base: &[u8], target: &[u8]) -> usize {
        let index = DeltaIndex::new(base);
        let d = create_delta(base, &index, target, usize::MAX).expect("delta");
        assert_eq!(apply_delta(base, &d).expect("applies"), target);
        d.len()
    }

    fn pseudo_random(n: usize, seed: u64) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                s as u8
            })
            .collect()
    }

    #[test]
    fn rolling_hash_matches_block_hash() {
        let data = pseudo_random(200, 7);
        let mut h = block_hash(&data[..BLOCK]);
        for i in 1..data.len() - BLOCK {
            h = roll(h, data[i - 1], data[i + BLOCK - 1]);
            assert_eq!(h, block_hash(&data[i..i + BLOCK]), "at {i}");
        }
    }

    #[test]
    fn edge_cases_round_trip() {
        check(b"", b"");
        check(b"", b"hello");
        check(b"hello", b"");
        check(b"short", b"short");
        let base = pseudo_random(5000, 1);
        check(&base, &base);
        check(&base, &base[100..4000]);
        check(&base, &pseudo_random(5000, 2));
        // Larger than one copy instruction.
        let big = pseudo_random(300_000, 3);
        let n = check(&big, &big);
        assert!(n < 64, "{n}");
    }

    #[test]
    fn edits_give_small_deltas() {
        let base = pseudo_random(20_000, 9);
        let mut target = base.clone();
        target.splice(5000..5000, b"inserted text".iter().copied());
        target.drain(12_000..12_500);
        target[15_000] ^= 0xff;
        let n = check(&base, &target);
        assert!(n < 120, "delta of {n} bytes");
        // Content moved around.
        let mut moved = base[10_000..].to_vec();
        moved.extend_from_slice(&base[..10_000]);
        let n = check(&base, &moved);
        assert!(n < 40, "delta of {n} bytes");
    }

    #[test]
    fn repetitive_data() {
        let base = vec![0u8; 100_000];
        let mut target = vec![0u8; 150_000];
        target[70_000] = 1;
        check(&base, &target);
        let text: Vec<u8> = b"abcabcabd".iter().copied().cycle().take(50_000).collect();
        check(&text, &text[3..]);
    }

    #[test]
    fn copy_offsets_and_sizes_encode() {
        // Offsets with zero bytes in the middle, and sizes at the limits.
        let base = pseudo_random(0x2_0100, 5);
        for (from, len) in [(0, 0x10000), (0x10000, 0x100), (0x100, 0x1_0001), (1, 17)] {
            check(&base, &base[from..from + len]);
        }
    }

    #[test]
    fn size_limit() {
        let base = pseudo_random(4000, 11);
        let target = pseudo_random(4000, 12);
        let index = DeltaIndex::new(&base);
        assert!(create_delta(&base, &index, &target, 1000).is_none());
        let similar = [&base[..2000], b"x", &base[2000..]].concat();
        assert!(create_delta(&base, &index, &similar, 1000).is_some());
    }

    #[test]
    fn random_round_trips() {
        for seed in 0..40u64 {
            let base = pseudo_random(1000 + (seed as usize * 37) % 3000, seed + 100);
            let mut target = Vec::new();
            let mut s = seed + 1;
            for _ in 0..10 {
                s = s
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let from = (s >> 33) as usize % base.len();
                let len = ((s >> 13) as usize % 300).min(base.len() - from);
                target.extend_from_slice(&base[from..from + len]);
                target.extend_from_slice(&pseudo_random((s >> 50) as usize % 20, s));
            }
            check(&base, &target);
        }
    }
}
