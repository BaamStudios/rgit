// SPDX-License-Identifier: MIT

//! Encoding of entry headers (`Documentation/gitformat-pack.adoc`,
//! "Object entries").

/// Pack type number of an `OFS_DELTA` entry.
pub(crate) const TYPE_OFS_DELTA: u8 = 6;
/// Pack type number of a `REF_DELTA` entry.
pub(crate) const TYPE_REF_DELTA: u8 = 7;

/// Append the type-and-size header of an entry.
///
/// The first byte holds a continuation bit, the 3-bit type and the low four
/// bits of the size; every following byte holds seven more bits of the size,
/// least significant group first.
pub(crate) fn push_entry_header(out: &mut Vec<u8>, pack_type: u8, size: u64) {
    let more = |rest: u64| if rest != 0 { 0x80 } else { 0 };
    let mut rest = size >> 4;
    out.push(more(rest) | (pack_type & 7) << 4 | (size & 0x0f) as u8);
    while rest != 0 {
        let low = (rest & 0x7f) as u8;
        rest >>= 7;
        out.push(more(rest) | low);
    }
}

/// Append the distance from a delta entry back to its base, as stored after
/// the header of an `OFS_DELTA` entry.
///
/// The format is big-endian groups of seven bits, where each group except
/// the last one is stored minus one; this makes every value have exactly
/// one encoding. `distance` must be positive.
pub(crate) fn push_base_distance(out: &mut Vec<u8>, distance: u64) {
    debug_assert!(distance > 0);
    // Collect groups from the least significant one, then reverse.
    let mut groups = [0u8; 10];
    let mut n = 0;
    let mut v = distance;
    groups[n] = (v & 0x7f) as u8;
    n += 1;
    v >>= 7;
    while v > 0 {
        v -= 1;
        groups[n] = 0x80 | (v & 0x7f) as u8;
        n += 1;
        v >>= 7;
    }
    out.extend(groups[..n].iter().rev());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decoder written from the format description, independent of the
    /// reader in `data.rs`.
    fn decode_distance(b: &[u8]) -> (u64, usize) {
        let mut v = (b[0] & 0x7f) as u64;
        let mut i = 1;
        let mut c = b[0];
        while c & 0x80 != 0 {
            c = b[i];
            i += 1;
            v = ((v + 1) << 7) | (c & 0x7f) as u64;
        }
        (v, i)
    }

    #[test]
    fn distance_round_trip() {
        for d in [
            1u64,
            2,
            127,
            128,
            129,
            16383,
            16384,
            16511,
            16512,
            1 << 20,
            (1 << 35) + 12345,
            u64::MAX >> 1,
        ] {
            let mut out = Vec::new();
            push_base_distance(&mut out, d);
            let (back, used) = decode_distance(&out);
            assert_eq!(back, d, "{out:02x?}");
            assert_eq!(used, out.len());
        }
    }

    #[test]
    fn header_examples() {
        let mut out = Vec::new();
        push_entry_header(&mut out, 3, 5);
        assert_eq!(out, [0x35]);
        out.clear();
        push_entry_header(&mut out, 1, 233);
        // 233 = 0b1110_1001: low nibble 9, then 14.
        assert_eq!(out, [0x80 | 0x10 | 9, 14]);
        out.clear();
        push_entry_header(&mut out, 2, 0);
        assert_eq!(out, [0x20]);
    }
}
