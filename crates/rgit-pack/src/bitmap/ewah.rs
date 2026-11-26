// SPDX-License-Identifier: MIT

//! Uncompressed bitmaps and their EWAH serialization
//! (`Documentation/technical/bitmap-format.adoc`, appendix A).
//!
//! On disk a bitmap is: the number of bits (4 bytes), the number of 64-bit
//! words (4 bytes), the words, and the index of the last run length word
//! (4 bytes), all big-endian. A run length word holds, from the low bits
//! up: the repeated bit, the number of repeated words (32 bits) and the
//! number of literal words that follow it (31 bits).
//!
//! In memory, [`Bitmap`] is a plain vector of 64-bit words; bit `n` is bit
//! `n % 64` of word `n / 64`.

const RUN_BITS: u32 = 32;
const LITERAL_BITS: u32 = 31;
const MAX_RUN: u64 = (1 << RUN_BITS) - 1;
const MAX_LITERALS: u64 = (1 << LITERAL_BITS) - 1;

fn run_bit(rlw: u64) -> bool {
    rlw & 1 != 0
}

fn run_len(rlw: u64) -> u64 {
    (rlw >> 1) & MAX_RUN
}

fn literals(rlw: u64) -> u64 {
    rlw >> (1 + RUN_BITS)
}

/// An uncompressed bitmap that grows as bits are set.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Bitmap {
    words: Vec<u64>,
}

impl std::fmt::Debug for Bitmap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.ones()).finish()
    }
}

impl Bitmap {
    /// An empty bitmap.
    pub fn new() -> Bitmap {
        Bitmap::default()
    }

    /// An empty bitmap with room for `bits` bits (all words allocated).
    pub fn with_bits(bits: usize) -> Bitmap {
        Bitmap {
            words: vec![0; bits.div_ceil(64)],
        }
    }

    /// A bitmap from its words.
    pub fn from_words(words: Vec<u64>) -> Bitmap {
        Bitmap { words }
    }

    /// The words (the last ones may be zero).
    pub fn words(&self) -> &[u64] {
        &self.words
    }

    /// Set bit `n`.
    pub fn set(&mut self, n: usize) {
        let w = n / 64;
        if w >= self.words.len() {
            self.words.resize(w + 1, 0);
        }
        self.words[w] |= 1 << (n % 64);
    }

    /// Clear bit `n`.
    pub fn unset(&mut self, n: usize) {
        if let Some(w) = self.words.get_mut(n / 64) {
            *w &= !(1 << (n % 64));
        }
    }

    /// Whether bit `n` is set.
    pub fn get(&self, n: usize) -> bool {
        self.words
            .get(n / 64)
            .is_some_and(|w| w >> (n % 64) & 1 != 0)
    }

    /// `self |= other`.
    pub fn or(&mut self, other: &Bitmap) {
        if other.words.len() > self.words.len() {
            self.words.resize(other.words.len(), 0);
        }
        for (a, b) in self.words.iter_mut().zip(&other.words) {
            *a |= b;
        }
    }

    /// `self &= !other`.
    pub fn and_not(&mut self, other: &Bitmap) {
        for (a, b) in self.words.iter_mut().zip(&other.words) {
            *a &= !b;
        }
    }

    /// `self &= other`.
    pub fn and(&mut self, other: &Bitmap) {
        for (i, a) in self.words.iter_mut().enumerate() {
            *a &= other.words.get(i).copied().unwrap_or(0);
        }
    }

    /// `self ^= other`.
    pub fn xor(&mut self, other: &Bitmap) {
        if other.words.len() > self.words.len() {
            self.words.resize(other.words.len(), 0);
        }
        for (a, b) in self.words.iter_mut().zip(&other.words) {
            *a ^= b;
        }
    }

    /// Drop trailing zero words.
    pub fn trim(&mut self) {
        let used = self.words.iter().rposition(|&w| w != 0).map_or(0, |i| i + 1);
        self.words.truncate(used);
    }

    /// Number of set bits.
    pub fn count(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// Whether no bit is set.
    pub fn is_empty(&self) -> bool {
        self.words.iter().all(|&w| w == 0)
    }

    /// Whether every bit set in `other` is set here.
    pub fn contains_all(&self, other: &Bitmap) -> bool {
        other
            .words
            .iter()
            .enumerate()
            .all(|(i, &b)| b & !self.words.get(i).copied().unwrap_or(0) == 0)
    }

    /// The positions of the set bits, ascending.
    pub fn ones(&self) -> impl Iterator<Item = usize> + '_ {
        self.words.iter().enumerate().flat_map(|(i, &w)| {
            let mut w = w;
            std::iter::from_fn(move || {
                if w == 0 {
                    return None;
                }
                let b = w.trailing_zeros() as usize;
                w &= w - 1;
                Some(i * 64 + b)
            })
        })
    }

    /// Serialize with the `bits` field set to the number of words times 64
    /// (all words are written, trailing zero words as a run), the way
    /// commit bitmaps are written.
    pub fn encode_full(&self, out: &mut Vec<u8>) {
        encode_words(&self.words, self.words.len() * 64, out);
    }

    /// Serialize only up to the last set bit (`bits` = one past it), the
    /// way the type bitmaps are written.
    pub fn encode_trimmed(&self, out: &mut Vec<u8>) {
        let used = self
            .words
            .iter()
            .rposition(|&w| w != 0)
            .map_or(0, |i| i + 1);
        let bits = match used {
            0 => 0,
            n => (n - 1) * 64 + (64 - self.words[n - 1].leading_zeros() as usize),
        };
        encode_words(&self.words[..used], bits, out);
    }

    /// The number of 64-bit words the EWAH form of this bitmap takes (as
    /// [`encode_full`](Bitmap::encode_full) writes it).
    pub fn compressed_words(&self) -> usize {
        compress(&self.words).0.len()
    }
}

/// Compress `words`: zero and all-ones words become runs, every other word
/// is a literal. Returns the words and the index of the last run length
/// word.
fn compress(words: &[u64]) -> (Vec<u64>, usize) {
    let mut out: Vec<u64> = vec![0];
    let mut rlw = 0usize;
    for &w in words {
        if w == 0 || w == u64::MAX {
            let bit = u64::from(w == u64::MAX);
            let cur = out[rlw];
            if literals(cur) == 0
                && (run_len(cur) == 0 || u64::from(run_bit(cur)) == bit)
                && run_len(cur) < MAX_RUN
            {
                out[rlw] = ((cur & !1) | bit) + 2;
            } else {
                rlw = out.len();
                out.push(bit | 2);
            }
        } else {
            if literals(out[rlw]) == MAX_LITERALS {
                rlw = out.len();
                out.push(0);
            }
            out[rlw] += 1 << (1 + RUN_BITS);
            out.push(w);
        }
    }
    (out, rlw)
}

fn encode_words(words: &[u64], bits: usize, out: &mut Vec<u8>) {
    let (body, rlw) = compress(words);
    out.extend_from_slice(&u32::try_from(bits).unwrap_or(u32::MAX).to_be_bytes());
    out.extend_from_slice(&u32::try_from(body.len()).unwrap_or(u32::MAX).to_be_bytes());
    for w in &body {
        out.extend_from_slice(&w.to_be_bytes());
    }
    out.extend_from_slice(&u32::try_from(rlw).unwrap_or(u32::MAX).to_be_bytes());
}

/// A bitmap read from `data`: the bitmap (bits at or past the stored bit
/// count are dropped) and the number of bytes it took. `None`: the data is
/// truncated or inconsistent.
pub fn decode(data: &[u8]) -> Option<(Bitmap, usize)> {
    let be32 = |p: usize| -> Option<u32> {
        Some(u32::from_be_bytes(data.get(p..p.checked_add(4)?)?.try_into().ok()?))
    };
    let bits = be32(0)? as usize;
    let nwords = be32(4)? as usize;
    let body_len = nwords.checked_mul(8)?;
    let body = data.get(8..8usize.checked_add(body_len)?)?;
    let _rlw = be32(8 + body_len)?;
    let used = 12 + body_len;
    let word = |i: usize| u64::from_be_bytes(body[i * 8..i * 8 + 8].try_into().expect("8 bytes"));
    let total_words = bits.div_ceil(64);
    let mut words: Vec<u64> = Vec::with_capacity(total_words);
    let mut i = 0;
    while i < nwords {
        let rlw = word(i);
        i += 1;
        let run = usize::try_from(run_len(rlw)).ok()?;
        if words.len().checked_add(run)? > total_words {
            return None;
        }
        let fill = if run_bit(rlw) { u64::MAX } else { 0 };
        words.resize(words.len() + run, fill);
        let lits = usize::try_from(literals(rlw)).ok()?;
        if i.checked_add(lits)? > nwords || words.len() + lits > total_words {
            return None;
        }
        for k in 0..lits {
            words.push(word(i + k));
        }
        i += lits;
    }
    // Drop bits past the stored count.
    if bits % 64 != 0
        && let Some(last) = words.get_mut(bits / 64)
    {
        *last &= (1u64 << (bits % 64)) - 1;
    }
    words.truncate(total_words);
    Some((Bitmap { words }, used))
}

/// The size in bytes of the bitmap at the start of `data`, without decoding
/// it.
pub fn encoded_len(data: &[u8]) -> Option<usize> {
    let n = u32::from_be_bytes(data.get(4..8)?.try_into().ok()?) as usize;
    let len = 12usize.checked_add(n.checked_mul(8)?)?;
    (data.len() >= len).then_some(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bm(bits: &[usize]) -> Bitmap {
        let mut b = Bitmap::new();
        for &n in bits {
            b.set(n);
        }
        b
    }

    #[test]
    fn empty_layout() {
        let mut out = Vec::new();
        Bitmap::new().encode_trimmed(&mut out);
        assert_eq!(out, [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn ones_become_runs() {
        let b = bm(&(0..1000).collect::<Vec<_>>());
        let mut out = Vec::new();
        b.encode_trimmed(&mut out);
        // 15 full words as a run of ones, then one literal.
        assert_eq!(&out[..8], &[0, 0, 3, 0xe8, 0, 0, 0, 2]);
        assert_eq!(&out[8..16], &((1u64 << 33) | (15 << 1) | 1).to_be_bytes());
        let (back, used) = decode(&out).unwrap();
        assert_eq!(used, out.len());
        assert_eq!(back.count(), 1000);
    }

    #[test]
    fn round_trip_full() {
        let b = bm(&[0, 5, 64, 200, 201, 5000, 5063, 5064]);
        let mut full = Bitmap::with_bits(6000);
        full.or(&b);
        let mut out = Vec::new();
        full.encode_full(&mut out);
        let (back, _) = decode(&out).unwrap();
        assert_eq!(back.ones().collect::<Vec<_>>(), b.ones().collect::<Vec<_>>());
        assert_eq!(back.words().len(), 6000usize.div_ceil(64));
    }

    #[test]
    fn truncated_fails() {
        let mut out = Vec::new();
        bm(&[3, 900]).encode_trimmed(&mut out);
        for n in 0..out.len() {
            assert!(decode(&out[..n]).is_none());
        }
    }

    #[test]
    fn ops() {
        let mut a = bm(&[1, 2, 3, 100]);
        let b = bm(&[2, 100, 300]);
        a.and_not(&b);
        assert_eq!(a.ones().collect::<Vec<_>>(), [1, 3]);
        a.xor(&b);
        assert_eq!(a.ones().collect::<Vec<_>>(), [1, 2, 3, 100, 300]);
        assert!(a.contains_all(&b));
        a.and(&bm(&[3, 300]));
        assert_eq!(a.ones().collect::<Vec<_>>(), [3, 300]);
    }
}
