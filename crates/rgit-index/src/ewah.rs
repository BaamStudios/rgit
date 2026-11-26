// SPDX-License-Identifier: MIT

//! EWAH-compressed bitmaps as stored in index extensions (serialization
//! described in `technical/bitmap-format.adoc`, appendix A): bit count,
//! word count, 64-bit words (run length words followed by literal words)
//! and the position of the last run length word, all big-endian.
//!
//! A run length word holds, from the low bits up: the repeated bit, the
//! number of repeated 64-bit words (32 bits) and the number of literal
//! words that follow (31 bits).

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

/// Decodes a bitmap from the start of `data`: the positions of the set
/// bits (below the stored bit count) and the number of bytes used.
pub(crate) fn decode(data: &[u8]) -> Option<(Vec<usize>, usize)> {
    let be32 = |p: usize| -> Option<u32> {
        Some(u32::from_be_bytes(data.get(p..p + 4)?.try_into().ok()?))
    };
    let bits = be32(0)? as usize;
    let words = be32(4)? as usize;
    let body = data.get(8..8 + words.checked_mul(8)?)?;
    let _rlw_pos = be32(8 + words * 8)?;
    let used = 12 + words * 8;
    let word = |i: usize| u64::from_be_bytes(body[i * 8..i * 8 + 8].try_into().expect("8 bytes"));
    let mut set = Vec::new();
    let mut pos = 0usize; // bit position of the next word
    let mut i = 0;
    while i < words {
        let rlw = word(i);
        i += 1;
        let run = usize::try_from(run_len(rlw)).ok()?.checked_mul(64)?;
        if run_bit(rlw) {
            set.extend((pos..pos + run).take_while(|&b| b < bits));
        }
        pos += run;
        for _ in 0..literals(rlw) {
            if i >= words {
                return None;
            }
            let w = word(i);
            i += 1;
            for b in 0..64 {
                if w >> b & 1 != 0 && pos + b < bits {
                    set.push(pos + b);
                }
            }
            pos += 64;
        }
    }
    Some((set, used))
}

/// Encodes the bitmap with the bits `set` (ascending) set, as git builds it
/// bit by bit: zero words become runs, every other word is a literal; the
/// bit count is one past the last set bit.
pub(crate) fn encode(set: &[usize], out: &mut Vec<u8>) {
    let bits = set.last().map_or(0, |&b| b + 1);
    // Literal words with their word index.
    let mut lits: Vec<(usize, u64)> = Vec::new();
    for &b in set {
        let w = b / 64;
        match lits.last_mut() {
            Some((i, v)) if *i == w => *v |= 1 << (b % 64),
            _ => lits.push((w, 1 << (b % 64))),
        }
    }
    let mut words: Vec<u64> = vec![0];
    let mut rlw = 0usize;
    let mut next = 0usize; // index of the next word of the bitmap
    for (w, v) in lits {
        let mut gap = (w - next) as u64;
        while gap > 0 {
            // Zeros extend the current run while it has no literals.
            if literals(words[rlw]) == 0 && run_len(words[rlw]) < MAX_RUN {
                let add = gap.min(MAX_RUN - run_len(words[rlw]));
                words[rlw] += add << 1;
                gap -= add;
            } else {
                rlw = words.len();
                words.push(0);
            }
        }
        if literals(words[rlw]) == MAX_LITERALS {
            rlw = words.len();
            words.push(0);
        }
        words[rlw] += 1 << (1 + RUN_BITS);
        words.push(v);
        next = w + 1;
    }
    out.extend_from_slice(&u32::try_from(bits).unwrap_or(u32::MAX).to_be_bytes());
    out.extend_from_slice(&u32::try_from(words.len()).unwrap_or(u32::MAX).to_be_bytes());
    for w in &words {
        out.extend_from_slice(&w.to_be_bytes());
    }
    out.extend_from_slice(&u32::try_from(rlw).unwrap_or(u32::MAX).to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round(set: &[usize]) -> Vec<u8> {
        let mut out = Vec::new();
        encode(set, &mut out);
        let (back, used) = decode(&out).unwrap();
        assert_eq!(back, set);
        assert_eq!(used, out.len());
        out
    }

    #[test]
    fn layouts() {
        // As git writes them: an empty bitmap is one empty run length word.
        assert_eq!(
            round(&[]),
            [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        let four = round(&[0, 1, 2, 3]);
        assert_eq!(&four[..8], &[0, 0, 0, 4, 0, 0, 0, 2]);
        assert_eq!(&four[8..16], &0x2_0000_0000u64.to_be_bytes());
        assert_eq!(&four[16..24], &0xfu64.to_be_bytes());
        round(&[0, 200, 201, 5000, 5063, 5064]);
        round(&(0..300).collect::<Vec<_>>());
    }
}
