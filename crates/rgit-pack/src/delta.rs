// SPDX-License-Identifier: MIT

//! Delta decoding (Documentation/gitformat-pack.adoc, "Deltified
//! representation").
//!
//! A delta starts with the base size and the result size (size encoding),
//! followed by copy instructions (msb set: up to four offset and three size
//! bytes, size 0 meaning 0x10000) and insert instructions (1 to 127 literal
//! bytes). Opcode 0 is reserved and rejected.

/// Why a delta could not be applied.
pub type DeltaError = &'static str;

/// Read a size-encoded integer from `data` at `*pos`.
pub(crate) fn read_size(data: &[u8], pos: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    let mut shift = 0u32;
    loop {
        let byte = *data.get(*pos)?;
        *pos += 1;
        if shift > 63 || (shift > 0 && (byte & 0x7f) as u64 > (u64::MAX >> shift)) {
            return None;
        }
        value |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
    }
}

/// The base and result sizes at the start of `delta`, and where the
/// instructions begin.
pub fn delta_sizes(delta: &[u8]) -> Option<(u64, u64, usize)> {
    let mut pos = 0;
    let base = read_size(delta, &mut pos)?;
    let result = read_size(delta, &mut pos)?;
    Some((base, result, pos))
}

/// Apply `delta` to `base` and return the result.
///
/// Every size and range is checked: a delta whose base size differs from
/// `base.len()`, that copies outside the base, that is truncated, or whose
/// output differs in size from the size it declares is an error.
pub fn apply_delta(base: &[u8], delta: &[u8]) -> Result<Vec<u8>, DeltaError> {
    let (base_size, result_size, mut pos) = delta_sizes(delta).ok_or("truncated delta header")?;
    if base_size != base.len() as u64 {
        return Err("base size mismatch");
    }
    let result_size = usize::try_from(result_size).map_err(|_| "result too large")?;
    // Each instruction byte yields at most 0xffffff bytes (copy) or 127
    // (insert), so a short delta cannot claim a huge result; cap the
    // up-front allocation anyway and let the vector grow if needed.
    let mut out = Vec::with_capacity(result_size.min(1 << 26));
    while pos < delta.len() {
        let op = delta[pos];
        pos += 1;
        if op & 0x80 != 0 {
            let mut offset = 0usize;
            let mut size = 0usize;
            for i in 0..4 {
                if op & (1 << i) != 0 {
                    let b = *delta.get(pos).ok_or("truncated copy instruction")?;
                    pos += 1;
                    offset |= (b as usize) << (8 * i);
                }
            }
            for i in 0..3 {
                if op & (0x10 << i) != 0 {
                    let b = *delta.get(pos).ok_or("truncated copy instruction")?;
                    pos += 1;
                    size |= (b as usize) << (8 * i);
                }
            }
            if size == 0 {
                size = 0x10000;
            }
            let end = offset.checked_add(size).ok_or("copy out of range")?;
            if end > base.len() {
                return Err("copy out of range");
            }
            if size > result_size - out.len() {
                return Err("result larger than declared");
            }
            out.extend_from_slice(&base[offset..end]);
        } else if op != 0 {
            let size = op as usize;
            let lit = delta
                .get(pos..pos + size)
                .ok_or("truncated insert instruction")?;
            pos += size;
            if size > result_size - out.len() {
                return Err("result larger than declared");
            }
            out.extend_from_slice(lit);
        } else {
            return Err("reserved delta opcode 0");
        }
    }
    if out.len() != result_size {
        return Err("result smaller than declared");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_and_insert() {
        // base 5, result 7: copy 0..3, insert "XY", copy 3..5.
        let delta = [5, 7, 0x91, 0, 3, 2, b'X', b'Y', 0x91, 3, 2];
        assert_eq!(apply_delta(b"hello", &delta).unwrap(), b"helXYlo");
    }

    #[test]
    fn copy_size_zero_means_64k() {
        let base = vec![7u8; 0x10000];
        let delta = [0x80, 0x80, 0x04, 0x80, 0x80, 0x04, 0x80];
        assert_eq!(apply_delta(&base, &delta).unwrap(), base);
    }

    #[test]
    fn rejects_bad_deltas() {
        // Base size mismatch.
        assert!(apply_delta(b"abc", &[4, 1, 1, b'x']).is_err());
        // Too many literal bytes.
        assert!(apply_delta(b"", &[0, 1, 2, b'x', b'y']).is_err());
        // Too few literal bytes.
        assert!(apply_delta(b"", &[0, 3, 2, b'x', b'y']).is_err());
        // Literal truncated.
        assert!(apply_delta(b"", &[0, 2, 2, b'x']).is_err());
        // Copy beyond base.
        assert!(apply_delta(b"ab", &[2, 3, 0x91, 0, 3]).is_err());
        // Truncated copy parameters.
        assert!(apply_delta(b"ab", &[2, 2, 0x91, 0]).is_err());
        // Reserved opcode.
        assert!(apply_delta(b"", &[0, 0, 0]).is_err());
        // Trailing garbage after a complete result.
        assert!(apply_delta(b"", &[0, 1, 1, b'x', 1, b'y']).is_err());
        // Header truncated.
        assert!(apply_delta(b"", &[0x80]).is_err());
        // Size overflow.
        assert!(apply_delta(b"", &[0xff; 12]).is_err());
    }
}
