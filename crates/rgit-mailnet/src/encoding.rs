// SPDX-License-Identifier: MIT
//! Content transfer encodings of RFC 2045: base64 (section 6.8) and
//! quoted-printable (section 6.7), plus the SASL helpers built on base64.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Base64 of `data` on one line, without a line break.
pub fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    // Feed bits in, take six at a time out.
    let mut acc: u32 = 0;
    let mut have = 0u32;
    for &byte in data {
        acc = (acc << 8) | u32::from(byte);
        have += 8;
        while have >= 6 {
            have -= 6;
            out.push(char::from(ALPHABET[((acc >> have) & 0x3f) as usize]));
        }
        acc &= (1 << have) - 1;
    }
    if have > 0 {
        out.push(char::from(ALPHABET[((acc << (6 - have)) & 0x3f) as usize]));
    }
    while out.len() % 4 != 0 {
        out.push('=');
    }
    out
}

/// Base64 of `data` as a message body: lines of at most 76 characters,
/// each ended by `\n`.
pub fn base64_body(data: &[u8]) -> Vec<u8> {
    let text = base64(data);
    let mut out = Vec::with_capacity(text.len() + text.len() / 76 + 1);
    for line in text.as_bytes().chunks(76) {
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    out
}

/// Decode base64, ignoring line breaks and other characters outside the
/// alphabet; `None` when the data is truncated in the middle of a group.
pub fn base64_decode(text: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for &c in text {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => continue,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    if bits >= 6 {
        return None;
    }
    Some(out)
}

fn hex_digit(n: u8) -> u8 {
    b"0123456789ABCDEF"[usize::from(n & 15)]
}

/// Quoted-printable encoding of a message body.
///
/// Line breaks (`\n`) stay hard line breaks; every other control byte
/// (including a carriage return), `=` and bytes above 126 become `=XX`;
/// a space or tab at the end of a line is encoded so it survives
/// transport; lines longer than 76 characters get soft breaks (`=\n`).
pub fn quoted_printable(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 8);
    let mut lines = data.split(|&b| b == b'\n').peekable();
    while let Some(line) = lines.next() {
        let last = lines.peek().is_none();
        if last && line.is_empty() {
            break;
        }
        let mut col = 0;
        for (i, &b) in line.iter().enumerate() {
            let at_end = i + 1 == line.len();
            let plain = match b {
                b' ' | b'\t' => !at_end,
                b'=' => false,
                33..=126 => true,
                _ => false,
            };
            let width = if plain { 1 } else { 3 };
            // Leave room for the soft break marker.
            if col + width > 75 && !(at_end && col + width <= 76) {
                out.extend_from_slice(b"=\n");
                col = 0;
            }
            if plain {
                out.push(b);
            } else {
                out.extend_from_slice(&[b'=', hex_digit(b >> 4), hex_digit(b)]);
            }
            col += width;
        }
        if !last {
            out.push(b'\n');
        } else {
            // The data did not end with a line break: a soft break keeps
            // the decoded text without one.
            out.extend_from_slice(b"=\n");
        }
    }
    out
}

fn hex_value(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'A'..=b'F' => Some(c - b'A' + 10),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

/// Decode a quoted-printable body. Invalid escapes are kept as they are.
pub fn quoted_printable_decode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut lines = data.split(|&b| b == b'\n').peekable();
    while let Some(raw) = lines.next() {
        let last = lines.peek().is_none();
        let line = raw.strip_suffix(b"\r").unwrap_or(raw);
        // Trailing whitespace is padding added in transport.
        let line = line.trim_ascii_end();
        let mut soft = false;
        let mut i = 0;
        while i < line.len() {
            let b = line[i];
            if b == b'=' {
                if i + 1 == line.len() {
                    soft = true;
                    break;
                }
                if let (Some(h), Some(l)) = (
                    line.get(i + 1).copied().and_then(hex_value),
                    line.get(i + 2).copied().and_then(hex_value),
                ) {
                    out.push(h << 4 | l);
                    i += 3;
                    continue;
                }
            }
            out.push(b);
            i += 1;
        }
        if !soft && !last {
            out.push(b'\n');
        }
    }
    out
}

/// The SASL PLAIN initial response (RFC 4616): `\0user\0password`, base64.
pub fn sasl_plain(user: &str, pass: &str) -> String {
    base64(format!("\0{user}\0{pass}").as_bytes())
}

/// The XOAUTH2 initial response: `user=<u>^Aauth=Bearer <token>^A^A`.
pub fn sasl_xoauth2(user: &str, token: &str) -> String {
    base64(format!("user={user}\x01auth=Bearer {token}\x01\x01").as_bytes())
}

/// The OAUTHBEARER initial response of RFC 7628.
pub fn sasl_oauthbearer(user: &str, token: &str) -> String {
    base64(format!("n,a={user},\x01auth=Bearer {token}\x01\x01").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        for s in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar"] {
            let e = base64(s);
            assert_eq!(base64_decode(e.as_bytes()).unwrap(), s);
        }
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(b"fo"), "Zm8=");
    }

    #[test]
    fn base64_body_wraps_at_76() {
        let body = base64_body(&[b'a'; 100]);
        let lines: Vec<_> = body.split(|&b| b == b'\n').collect();
        assert_eq!(lines[0].len(), 76);
        assert!(body.ends_with(b"\n"));
    }

    #[test]
    fn qp_encodes_specials_and_cr() {
        assert_eq!(quoted_printable(b"a = b\r\n"), b"a =3D b=0D\n".to_vec());
        assert_eq!(quoted_printable("ä\n".as_bytes()), b"=C3=A4\n".to_vec());
        assert_eq!(quoted_printable(b"trail \n"), b"trail=20\n".to_vec());
    }

    #[test]
    fn qp_soft_breaks_long_lines() {
        let line = vec![b'x'; 200];
        let mut data = line.clone();
        data.push(b'\n');
        let enc = quoted_printable(&data);
        for l in enc.split(|&b| b == b'\n') {
            assert!(l.len() <= 76, "{}", l.len());
        }
        assert_eq!(quoted_printable_decode(&enc), data);
    }

    #[test]
    fn qp_round_trip() {
        let data = "Grüße = viele\r\n\tend \nlast".as_bytes();
        assert_eq!(quoted_printable_decode(&quoted_printable(data)), data);
    }

    #[test]
    fn sasl_plain_layout() {
        assert_eq!(
            base64_decode(sasl_plain("u", "p").as_bytes()).unwrap(),
            b"\0u\0p"
        );
    }
}
