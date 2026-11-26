// SPDX-License-Identifier: MIT
//! HPACK header compression for HTTP/2 (RFC 7541).
//!
//! The [`Encoder`] sends every field as a literal without indexing (or
//! "never indexed" for credentials) and without Huffman coding: it keeps no
//! dynamic table, so the peer's `SETTINGS_HEADER_TABLE_SIZE` does not
//! matter. The [`Decoder`] implements the whole format: indexed fields,
//! the three literal forms, dynamic table size updates, the dynamic table
//! with eviction, and Huffman-coded strings.

use std::collections::VecDeque;
use std::sync::OnceLock;

use crate::hpack_huffman::CODES;

/// The static table (RFC 7541, Appendix A); index 1 is the first entry.
const STATIC: [(&str, &str); 61] = [
    (":authority", ""),
    (":method", "GET"),
    (":method", "POST"),
    (":path", "/"),
    (":path", "/index.html"),
    (":scheme", "http"),
    (":scheme", "https"),
    (":status", "200"),
    (":status", "204"),
    (":status", "206"),
    (":status", "304"),
    (":status", "400"),
    (":status", "404"),
    (":status", "500"),
    ("accept-charset", ""),
    ("accept-encoding", "gzip, deflate"),
    ("accept-language", ""),
    ("accept-ranges", ""),
    ("accept", ""),
    ("access-control-allow-origin", ""),
    ("age", ""),
    ("allow", ""),
    ("authorization", ""),
    ("cache-control", ""),
    ("content-disposition", ""),
    ("content-encoding", ""),
    ("content-language", ""),
    ("content-length", ""),
    ("content-location", ""),
    ("content-range", ""),
    ("content-type", ""),
    ("cookie", ""),
    ("date", ""),
    ("etag", ""),
    ("expect", ""),
    ("expires", ""),
    ("from", ""),
    ("host", ""),
    ("if-match", ""),
    ("if-modified-since", ""),
    ("if-none-match", ""),
    ("if-range", ""),
    ("if-unmodified-since", ""),
    ("last-modified", ""),
    ("link", ""),
    ("location", ""),
    ("max-forwards", ""),
    ("proxy-authenticate", ""),
    ("proxy-authorization", ""),
    ("range", ""),
    ("referer", ""),
    ("refresh", ""),
    ("retry-after", ""),
    ("server", ""),
    ("set-cookie", ""),
    ("strict-transport-security", ""),
    ("transfer-encoding", ""),
    ("user-agent", ""),
    ("vary", ""),
    ("via", ""),
    ("www-authenticate", ""),
];

/// A decoded field: name and value.
pub(crate) type Field = (Vec<u8>, Vec<u8>);

/// A decoding error (a `COMPRESSION_ERROR` of the connection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HpackError(pub &'static str);

/// Append `value` as an HPACK integer with an `n`-bit prefix; `first`
/// carries the bits above the prefix.
fn put_int(out: &mut Vec<u8>, first: u8, n: u32, mut value: usize) {
    let max = (1usize << n) - 1;
    if value < max {
        out.push(first | value as u8);
        return;
    }
    out.push(first | max as u8);
    value -= max;
    while value >= 128 {
        out.push((value % 128) as u8 | 0x80);
        value /= 128;
    }
    out.push(value as u8);
}

/// A string literal without Huffman coding.
fn put_str(out: &mut Vec<u8>, s: &[u8]) {
    put_int(out, 0, 7, s.len());
    out.extend_from_slice(s);
}

/// The encoder: literal fields only, no dynamic table.
#[derive(Debug, Default)]
pub(crate) struct Encoder;

impl Encoder {
    /// Encode `fields` (names in lower case) into one header block.
    pub(crate) fn encode(&mut self, fields: &[(String, String)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, value) in fields {
            let sensitive = matches!(
                name.as_str(),
                "authorization" | "proxy-authorization" | "cookie"
            );
            // Literal without indexing (0000) or never indexed (0001),
            // with an indexed name where the static table has it.
            let first = if sensitive { 0x10 } else { 0x00 };
            match STATIC.iter().position(|(n, _)| *n == name) {
                Some(i) => put_int(&mut out, first, 4, i + 1),
                None => {
                    out.push(first);
                    put_str(&mut out, name.as_bytes());
                }
            }
            put_str(&mut out, value.as_bytes());
        }
        out
    }
}

/// The decoder with its dynamic table.
#[derive(Debug)]
pub(crate) struct Decoder {
    table: VecDeque<(Vec<u8>, Vec<u8>)>,
    size: usize,
    /// The size the table may currently use (dynamic table size update).
    max: usize,
    /// The limit announced in `SETTINGS_HEADER_TABLE_SIZE`.
    limit: usize,
}

impl Decoder {
    pub(crate) fn new(limit: usize) -> Decoder {
        Decoder {
            table: VecDeque::new(),
            size: 0,
            max: limit,
            limit,
        }
    }

    fn evict(&mut self) {
        while self.size > self.max {
            match self.table.pop_back() {
                Some((n, v)) => self.size -= n.len() + v.len() + 32,
                None => break,
            }
        }
    }

    fn insert(&mut self, name: Vec<u8>, value: Vec<u8>) {
        let s = name.len() + value.len() + 32;
        if s > self.max {
            self.table.clear();
            self.size = 0;
            return;
        }
        self.size += s;
        self.evict();
        self.table.push_front((name, value));
    }

    fn get(&self, index: usize) -> Result<(Vec<u8>, Vec<u8>), HpackError> {
        if index == 0 {
            return Err(HpackError("index 0"));
        }
        if index <= STATIC.len() {
            let (n, v) = STATIC[index - 1];
            return Ok((n.as_bytes().to_vec(), v.as_bytes().to_vec()));
        }
        self.table
            .get(index - STATIC.len() - 1)
            .cloned()
            .ok_or(HpackError("index out of range"))
    }

    /// Decode one complete header block.
    pub(crate) fn decode(&mut self, mut buf: &[u8]) -> Result<Vec<Field>, HpackError> {
        let mut out = Vec::new();
        let mut fields_seen = false;
        while let Some(&b) = buf.first() {
            if b & 0x80 != 0 {
                let i = get_int(&mut buf, 7)?;
                out.push(self.get(i)?);
                fields_seen = true;
            } else if b & 0xc0 == 0x40 {
                let i = get_int(&mut buf, 6)?;
                let (name, value) = self.literal(&mut buf, i)?;
                self.insert(name.clone(), value.clone());
                out.push((name, value));
                fields_seen = true;
            } else if b & 0xe0 == 0x20 {
                // A size update is only allowed at the start of a block.
                if fields_seen {
                    return Err(HpackError("late dynamic table size update"));
                }
                let n = get_int(&mut buf, 5)?;
                if n > self.limit {
                    return Err(HpackError("dynamic table size above the limit"));
                }
                self.max = n;
                self.evict();
            } else {
                // 0000xxxx (without indexing) and 0001xxxx (never indexed).
                let i = get_int(&mut buf, 4)?;
                out.push(self.literal(&mut buf, i)?);
                fields_seen = true;
            }
        }
        Ok(out)
    }

    fn literal(&self, buf: &mut &[u8], index: usize) -> Result<(Vec<u8>, Vec<u8>), HpackError> {
        let name = if index == 0 {
            get_str(buf)?
        } else {
            self.get(index)?.0
        };
        let value = get_str(buf)?;
        Ok((name, value))
    }
}

fn get_int(buf: &mut &[u8], n: u32) -> Result<usize, HpackError> {
    let max = (1usize << n) - 1;
    let (&first, rest) = buf.split_first().ok_or(HpackError("truncated integer"))?;
    *buf = rest;
    let mut v = (first as usize) & max;
    if v < max {
        return Ok(v);
    }
    let mut shift = 0u32;
    loop {
        let (&b, rest) = buf.split_first().ok_or(HpackError("truncated integer"))?;
        *buf = rest;
        if shift > 28 {
            return Err(HpackError("integer overflow"));
        }
        v += ((b & 0x7f) as usize) << shift;
        shift += 7;
        if b & 0x80 == 0 {
            return Ok(v);
        }
    }
}

fn get_str(buf: &mut &[u8]) -> Result<Vec<u8>, HpackError> {
    let huffman = buf.first().is_some_and(|b| b & 0x80 != 0);
    let len = get_int(buf, 7)?;
    if buf.len() < len {
        return Err(HpackError("truncated string"));
    }
    let (s, rest) = buf.split_at(len);
    *buf = rest;
    if huffman {
        huffman_decode(s)
    } else {
        Ok(s.to_vec())
    }
}

/// A node of the Huffman decoding tree: children for bit 0 and bit 1,
/// leaves are `symbol + LEAF`.
const LEAF: u16 = 0x8000;

fn tree() -> &'static Vec<[u16; 2]> {
    static TREE: OnceLock<Vec<[u16; 2]>> = OnceLock::new();
    TREE.get_or_init(|| {
        let mut nodes: Vec<[u16; 2]> = vec![[0, 0]];
        for (sym, &(code, len)) in CODES.iter().enumerate() {
            let mut node = 0usize;
            for i in (0..len).rev() {
                let bit = ((code >> i) & 1) as usize;
                if i == 0 {
                    nodes[node][bit] = LEAF | sym as u16;
                } else {
                    if nodes[node][bit] == 0 {
                        nodes.push([0, 0]);
                        nodes[node][bit] = (nodes.len() - 1) as u16;
                    }
                    node = nodes[node][bit] as usize;
                }
            }
        }
        nodes
    })
}

/// Decode a Huffman-coded string (RFC 7541, 5.2).
pub(crate) fn huffman_decode(data: &[u8]) -> Result<Vec<u8>, HpackError> {
    let t = tree();
    let mut out = Vec::with_capacity(data.len() * 8 / 5);
    let mut node = 0usize;
    // Bits since the last complete symbol, and whether all were 1.
    let mut pending = 0u32;
    let mut all_ones = true;
    for &byte in data {
        for i in (0..8).rev() {
            let bit = ((byte >> i) & 1) as usize;
            pending += 1;
            all_ones &= bit == 1;
            let next = t[node][bit];
            if next == 0 {
                return Err(HpackError("invalid Huffman code"));
            }
            if next & LEAF != 0 {
                let sym = next & !LEAF;
                if sym == 256 {
                    return Err(HpackError("EOS in Huffman string"));
                }
                out.push(sym as u8);
                node = 0;
                pending = 0;
                all_ones = true;
            } else {
                node = next as usize;
            }
        }
    }
    // Padding: at most 7 bits, all of them 1 (a prefix of EOS).
    if pending > 7 || !all_ones {
        return Err(HpackError("invalid Huffman padding"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn pairs(v: &[(Vec<u8>, Vec<u8>)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(n, v)| {
                (
                    String::from_utf8(n.clone()).unwrap(),
                    String::from_utf8(v.clone()).unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn integers() {
        // RFC 7541, C.1.
        let mut v = Vec::new();
        put_int(&mut v, 0, 5, 10);
        assert_eq!(v, [10]);
        v.clear();
        put_int(&mut v, 0, 5, 1337);
        assert_eq!(v, [31, 154, 10]);
        let mut b: &[u8] = &[31, 154, 10];
        assert_eq!(get_int(&mut b, 5).unwrap(), 1337);
        let mut b: &[u8] = &[42];
        assert_eq!(get_int(&mut b, 8).unwrap(), 42);
    }

    #[test]
    fn requests_with_huffman() {
        // RFC 7541, C.4: three requests sharing a dynamic table.
        let mut d = Decoder::new(4096);
        let r1 = d
            .decode(&hex("8286 8441 8cf1 e3c2 e5f2 3a6b a0ab 90f4 ff"))
            .unwrap();
        assert_eq!(
            pairs(&r1),
            [
                (":method", "GET"),
                (":scheme", "http"),
                (":path", "/"),
                (":authority", "www.example.com"),
            ]
            .map(|(a, b)| (a.to_string(), b.to_string()))
        );
        let r2 = d.decode(&hex("8286 84be 5886 a8eb 1064 9cbf")).unwrap();
        assert_eq!(pairs(&r2)[4], ("cache-control".into(), "no-cache".into()));
        assert_eq!(
            pairs(&r2)[3],
            (":authority".into(), "www.example.com".into())
        );
        let r3 = d
            .decode(&hex(
                "8287 85bf 4088 25a8 49e9 5ba9 7d7f 8925 a849 e95b b8e8 b4bf",
            ))
            .unwrap();
        assert_eq!(pairs(&r3)[4], ("custom-key".into(), "custom-value".into()));
        assert_eq!(d.size, 164);
    }

    #[test]
    fn responses_with_eviction() {
        // RFC 7541, C.5: a 256-byte table evicts entries.
        let mut d = Decoder::new(256);
        let r1 = d
            .decode(&hex("4803 3330 3258 0770 7269 7661 7465 611d \
                 4d6f 6e2c 2032 3120 4f63 7420 3230 3133 \
                 2032 303a 3133 3a32 3120 474d 546e 1768 \
                 7474 7073 3a2f 2f77 7777 2e65 7861 6d70 \
                 6c65 2e63 6f6d"))
            .unwrap();
        assert_eq!(pairs(&r1)[0], (":status".into(), "302".into()));
        assert_eq!(d.size, 222);
        let r2 = d.decode(&hex("4803 3330 37c1 c0bf")).unwrap();
        assert_eq!(pairs(&r2)[0], (":status".into(), "307".into()));
        assert_eq!(pairs(&r2)[1], ("cache-control".into(), "private".into()));
        assert_eq!(d.size, 222);
        assert_eq!(d.table.len(), 4);
    }

    #[test]
    fn encoder_roundtrip() {
        let fields: Vec<(String, String)> = [
            (":method", "POST"),
            (":path", "/x/git-upload-pack"),
            ("authorization", "Basic abc"),
            ("git-protocol", "version=2"),
            ("x-long", &"y".repeat(300)),
        ]
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
        let block = Encoder.encode(&fields);
        let mut d = Decoder::new(4096);
        assert_eq!(pairs(&d.decode(&block).unwrap()), fields);
        assert!(d.table.is_empty());
    }

    #[test]
    fn huffman_errors() {
        // "www.example.com" decodes; a trailing zero bit is bad padding.
        assert_eq!(
            huffman_decode(&hex("f1e3 c2e5 f23a 6ba0 ab90 f4ff")).unwrap(),
            b"www.example.com"
        );
        assert!(huffman_decode(&hex("f1e3 c2e5 f23a 6ba0 ab90 f4fe")).is_err());
        // A whole byte of padding is too long.
        assert!(huffman_decode(&hex("ff")).is_err());
        assert_eq!(huffman_decode(&[]).unwrap(), b"");
    }
}
