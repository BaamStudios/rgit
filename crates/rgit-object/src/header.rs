// SPDX-License-Identifier: MIT

//! The object header `<type> <size>\0` that precedes the content in loose
//! objects and in the input of the object hash.

use crate::{Error, ObjectKind};

/// The longest header: `commit ` + 20 digits of `u64::MAX` + NUL.
pub const MAX_HEADER_LEN: usize = 6 + 1 + 20 + 1;

/// A parsed object header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectHeader {
    /// The object type.
    pub kind: ObjectKind,
    /// The content size in bytes.
    pub size: u64,
}

impl ObjectHeader {
    /// Parse a header at the start of `data` and return it together with the
    /// header length (including the NUL), so that `&data[len..]` is the content.
    ///
    /// The size must be plain decimal without sign or leading zeros
    /// (except `0` itself); the type must be one of the four object types.
    pub fn parse(data: &[u8]) -> Result<(ObjectHeader, usize), Error> {
        let sp = data
            .iter()
            .take(MAX_HEADER_LEN)
            .position(|&b| b == b' ')
            .ok_or(Error::BadHeader)?;
        let kind = ObjectKind::from_name(&data[..sp])
            .ok_or_else(|| Error::UnknownKind(data[..sp].to_vec()))?;
        let rest = &data[sp + 1..];
        let nul = rest
            .iter()
            .take(MAX_HEADER_LEN)
            .position(|&b| b == 0)
            .ok_or(Error::BadHeader)?;
        let size = parse_size(&rest[..nul]).ok_or(Error::BadHeader)?;
        Ok((ObjectHeader { kind, size }, sp + 1 + nul + 1))
    }

    /// Append `<type> <size>\0` to `out`.
    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(self.kind.name().as_bytes());
        out.push(b' ');
        out.extend_from_slice(self.size.to_string().as_bytes());
        out.push(0);
    }

    /// The header bytes `<type> <size>\0`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(MAX_HEADER_LEN);
        self.write_to(&mut v);
        v
    }
}

/// Parse a canonical decimal size: digits only, no leading zero unless the
/// value is 0, no overflow.
fn parse_size(digits: &[u8]) -> Option<u64> {
    if digits.is_empty() || (digits.len() > 1 && digits[0] == b'0') {
        return None;
    }
    let mut v: u64 = 0;
    for &d in digits {
        if !d.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add(u64::from(d - b'0'))?;
    }
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for kind in ObjectKind::ALL {
            for size in [0, 1, 9, 10, 12345, u64::MAX] {
                let h = ObjectHeader { kind, size };
                let mut b = h.to_bytes();
                let n = b.len();
                b.extend_from_slice(b"content");
                assert_eq!(ObjectHeader::parse(&b).unwrap(), (h, n));
            }
        }
    }

    #[test]
    fn rejects() {
        for bad in [
            &b"blob 12"[..],
            b"blob\x0012",
            b"blob 012\0",
            b"blob -1\0",
            b"blob +1\0",
            b"blob \0",
            b"blob 1 \0",
            b"blob 18446744073709551616\0",
            b"",
        ] {
            assert_eq!(ObjectHeader::parse(bad), Err(Error::BadHeader), "{bad:?}");
        }
        assert!(matches!(
            ObjectHeader::parse(b"bogus 1\0"),
            Err(Error::UnknownKind(_))
        ));
    }
}
