// SPDX-License-Identifier: MIT

//! The variable-width integer used for index v4 path compression (the same
//! encoding as `OFS_DELTA` offsets in packs, see gitformat-pack): big-endian
//! groups of 7 bits, high bit set on all but the last byte, and each
//! continuation adds one to the value so that every number has exactly
//! one encoding.

/// Decodes a number from the start of `data`; returns it and the number of
/// bytes used, or `None` if the data ends early or the value overflows.
pub(crate) fn decode(data: &[u8]) -> Option<(usize, usize)> {
    let mut iter = data.iter();
    let mut c = *iter.next()?;
    let mut value = usize::from(c & 0x7f);
    let mut used = 1;
    while c & 0x80 != 0 {
        c = *iter.next()?;
        used += 1;
        value = value.checked_add(1)?.checked_mul(128)? | usize::from(c & 0x7f);
    }
    Some((value, used))
}

/// Appends the encoding of `value` to `out`.
pub(crate) fn encode(mut value: usize, out: &mut Vec<u8>) {
    let mut buf = [0u8; 16];
    let mut pos = buf.len() - 1;
    buf[pos] = (value & 0x7f) as u8;
    value >>= 7;
    while value != 0 {
        value -= 1;
        pos -= 1;
        buf[pos] = 0x80 | (value & 0x7f) as u8;
        value >>= 7;
    }
    out.extend_from_slice(&buf[pos..]);
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
