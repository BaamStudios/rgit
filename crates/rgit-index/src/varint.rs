// SPDX-License-Identifier: MIT

//! The variable-width integer used for index v4 path compression (the same
//! encoding as `OFS_DELTA` offsets in packs, see gitformat-pack): big-endian
//! groups of 7 bits, high bit set on all but the last byte, and each
//! continuation adds one to the value so that every number has exactly
//! one encoding.

/// Decodes a number from the start of `data`; returns it and the number of
/// bytes used, or `None` if the data ends early or the value overflows.
pub(crate) fn decode(data: &[u8]) -> Option<(usize, usize)> {
    let mut acc: Option<usize> = None;
    for (n, &byte) in data.iter().enumerate() {
        let group = usize::from(byte & 0x7f);
        let value = match acc {
            None => group,
            // Every continuation byte stands for "one more, times 128".
            Some(prev) => prev.checked_add(1)?.checked_mul(128)? + group,
        };
        if byte & 0x80 == 0 {
            return Some((value, n + 1));
        }
        acc = Some(value);
    }
    None
}

/// Appends the encoding of `value` to `out`.
pub(crate) fn encode(value: usize, out: &mut Vec<u8>) {
    // Build the 7-bit groups from the least significant end, then emit
    // them most significant first.
    let mut groups = vec![(value % 128) as u8];
    let mut high = value / 128;
    while high > 0 {
        high -= 1;
        groups.push((high % 128) as u8 | 0x80);
        high /= 128;
    }
    out.extend(groups.into_iter().rev());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for v in [
            0,
            1,
            127,
            128,
            129,
            255,
            16383,
            16384,
            16511,
            16512,
            1 << 20,
            usize::MAX >> 1,
        ] {
            let mut out = Vec::new();
            encode(v, &mut out);
            assert_eq!(decode(&out), Some((v, out.len())), "{v}");
        }
    }

    #[test]
    fn known_encodings() {
        let mut out = Vec::new();
        encode(128, &mut out);
        assert_eq!(out, [0x80, 0x00]);
        out.clear();
        encode(16511, &mut out);
        assert_eq!(out, [0xff, 0x7f]);
    }

    #[test]
    fn truncated() {
        assert_eq!(decode(&[]), None);
        assert_eq!(decode(&[0x80]), None);
        assert_eq!(decode(&[0xff; 40]), None);
    }
}
