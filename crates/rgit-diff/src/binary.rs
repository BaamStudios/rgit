// SPDX-License-Identifier: MIT

//! Binary patches (`--binary`): the `GIT binary patch` block that `git
//! apply` understands.
//!
//! Format as observed in the output of the `git` binary: after the
//! header line come two hunks, the forward one (`literal <size>`: the new
//! contents) and the reverse one (the old contents), each followed by an
//! empty line. The data is zlib-compressed and written in lines of at
//! most 52 bytes; a line starts with its byte count (`A`-`Z` for 1-26,
//! `a`-`z` for 27-52) followed by the bytes in base 85 (RFC 1924
//! alphabet, groups of four bytes as five digits, most significant first,
//! the last group padded with zero bytes). `delta <size>` hunks hold a
//! delta in the pack format instead; rgit writes literal hunks only,
//! which every `git apply` accepts.

use std::io::Write;

use flate2::Compression;
use flate2::write::{ZlibDecoder, ZlibEncoder};

const ALPHABET: &[u8; 85] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz!#$%&()*+-;<=>?@^_`{|}~";

/// Bytes per data line.
const LINE_BYTES: usize = 52;

fn digit(c: u8) -> Option<u32> {
    ALPHABET.iter().position(|&a| a == c).map(|p| p as u32)
}

/// Appends `data` as base-85 data lines.
pub fn encode_lines(out: &mut Vec<u8>, data: &[u8]) {
    for chunk in data.chunks(LINE_BYTES) {
        let n = chunk.len() as u8;
        out.push(if n <= 26 { b'A' + n - 1 } else { b'a' + n - 27 });
        for group in chunk.chunks(4) {
            let mut word = [0u8; 4];
            word[..group.len()].copy_from_slice(group);
            let mut v = u32::from_be_bytes(word);
            let mut digits = [0u8; 5];
            for d in digits.iter_mut().rev() {
                *d = ALPHABET[(v % 85) as usize];
                v /= 85;
            }
            out.extend_from_slice(&digits);
        }
        out.push(b'\n');
    }
}

/// Decodes one data line (without its line feed); `None` if it is not
/// valid.
pub fn decode_line(line: &[u8]) -> Option<Vec<u8>> {
    let (&len, rest) = line.split_first()?;
    let n = match len {
        b'A'..=b'Z' => usize::from(len - b'A') + 1,
        b'a'..=b'z' => usize::from(len - b'a') + 27,
        _ => return None,
    };
    if rest.len() != n.div_ceil(4) * 5 {
        return None;
    }
    let mut out = Vec::with_capacity(rest.len() / 5 * 4);
    for group in rest.chunks(5) {
        let mut v: u64 = 0;
        for &c in group {
            v = v * 85 + u64::from(digit(c)?);
        }
        let v = u32::try_from(v).ok()?;
        out.extend_from_slice(&v.to_be_bytes());
    }
    out.truncate(n);
    Some(out)
}

/// zlib-compresses `data` (fastest level, as git does).
pub fn deflate(data: &[u8]) -> Vec<u8> {
    let mut e = ZlibEncoder::new(Vec::new(), Compression::fast());
    e.write_all(data).expect("in memory");
    e.finish().expect("in memory")
}

/// Inflates zlib data; `None` if it is corrupt.
pub fn inflate(data: &[u8]) -> Option<Vec<u8>> {
    let mut d = ZlibDecoder::new(Vec::new());
    d.write_all(data).ok()?;
    d.finish().ok()
}

fn literal(out: &mut Vec<u8>, data: &[u8]) {
    writeln!(out, "literal {}", data.len()).unwrap();
    encode_lines(out, &deflate(data));
    out.push(b'\n');
}

/// Appends the `GIT binary patch` block that turns `old` into `new`.
pub fn write_binary_patch(out: &mut Vec<u8>, old: &[u8], new: &[u8]) {
    out.extend_from_slice(b"GIT binary patch\n");
    literal(out, new);
    literal(out, old);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base85_round_trip() {
        for len in [0usize, 1, 3, 4, 5, 26, 27, 51, 52, 53, 200] {
            let data: Vec<u8> = (0..len).map(|i| (i * 37 % 256) as u8).collect();
            let mut text = Vec::new();
            encode_lines(&mut text, &data);
            let mut back = Vec::new();
            for line in text.split(|&c| c == b'\n').filter(|l| !l.is_empty()) {
                back.extend(decode_line(line).unwrap());
            }
            assert_eq!(back, data);
        }
        assert_eq!(decode_line(b"Hxx"), None);
    }

    #[test]
    fn empty_literal_matches_git() {
        // What `git diff --binary` writes for empty contents.
        let mut out = Vec::new();
        encode_lines(&mut out, &deflate(b""));
        assert_eq!(out, b"HcmV?d00001\n");
        assert_eq!(inflate(&decode_line(b"HcmV?d00001").unwrap()).unwrap(), b"");
    }
}
