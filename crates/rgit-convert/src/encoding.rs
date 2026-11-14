// SPDX-License-Identifier: MIT

//! The `working-tree-encoding` attribute (gitattributes(5)): the work
//! tree file is in the named encoding, the blob in UTF-8.
//!
//! UTF-16 and UTF-32 are handled here; other encodings go through
//! `encoding_rs` (WHATWG encodings), except ISO-8859-1, which is mapped
//! byte for byte. Names are matched case-insensitively.
//!
//! Byte order marks (behaviour of the `git` binary):
//!
//! - `UTF-16` / `UTF-32` require a BOM in the work tree file; checkout
//!   writes a little-endian BOM and little-endian content (what iconv
//!   writes on the platforms git is tested on).
//! - `UTF-16BE`, `UTF-16LE`, `UTF-32BE`, `UTF-32LE` forbid one.
//! - `UTF-16LE-BOM` etc. are the endian forms with a BOM written on
//!   checkout.

use crate::messages;

/// Byte order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Endian {
    Little,
    Big,
}

#[derive(Debug, Clone, Copy)]
enum Kind {
    Utf8,
    /// UTF-16 (`width` 2) or UTF-32 (`width` 4). `endian` `None`: a BOM is
    /// required (and written as little endian); `bom` writes one with the
    /// given endianness.
    Unicode {
        width: usize,
        endian: Option<Endian>,
        bom: bool,
    },
    Latin1,
    Other(&'static encoding_rs::Encoding),
    Unknown,
}

/// A `working-tree-encoding` value.
#[derive(Debug, Clone)]
pub struct Encoding {
    name: String,
    kind: Kind,
}

/// Why content could not be converted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// A `hint: ` line printed before the message, if any (without the
    /// prefix).
    pub hint: Option<String>,
    /// The message (without `fatal: ` / `error: `).
    pub message: String,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

fn kind(name: &str) -> Kind {
    let upper = name.to_ascii_uppercase();
    let unicode = |width, endian, bom| Kind::Unicode { width, endian, bom };
    let (base, bom) = match upper.strip_suffix("-BOM") {
        Some(b) => (b, true),
        None => (upper.as_str(), false),
    };
    let width_endian = match base {
        "UTF-16" | "UTF16" => Some((2, None)),
        "UTF-16LE" | "UTF16LE" => Some((2, Some(Endian::Little))),
        "UTF-16BE" | "UTF16BE" => Some((2, Some(Endian::Big))),
        "UTF-32" | "UTF32" => Some((4, None)),
        "UTF-32LE" | "UTF32LE" => Some((4, Some(Endian::Little))),
        "UTF-32BE" | "UTF32BE" => Some((4, Some(Endian::Big))),
        _ => None,
    };
    if let Some((w, e)) = width_endian {
        if bom && e.is_none() {
            return Kind::Unknown;
        }
        return unicode(w, e, bom);
    }
    if bom {
        return Kind::Unknown;
    }
    match upper.as_str() {
        "UTF-8" | "UTF8" => Kind::Utf8,
        "ISO-8859-1" | "ISO8859-1" | "ISO_8859-1" | "LATIN1" | "L1" | "ISO-IR-100" | "CP819"
        | "IBM819" => Kind::Latin1,
        _ => encoding_rs::Encoding::for_label(upper.as_bytes()).map_or(Kind::Unknown, Kind::Other),
    }
}

const BOM16_BE: &[u8] = b"\xfe\xff";
const BOM16_LE: &[u8] = b"\xff\xfe";
const BOM32_BE: &[u8] = b"\x00\x00\xfe\xff";
const BOM32_LE: &[u8] = b"\xff\xfe\x00\x00";

impl Encoding {
    /// The encoding named `name` (an unknown name fails on use).
    #[must_use]
    pub fn new(name: &str) -> Encoding {
        Encoding {
            name: name.to_string(),
            kind: kind(name),
        }
    }

    /// The name as given.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether the name is UTF-8 (no conversion).
    #[must_use]
    pub fn is_utf8(&self) -> bool {
        matches!(self.kind, Kind::Utf8)
    }

    /// Whether `name` is in the `core.checkRoundtripEncoding` list.
    #[must_use]
    pub fn in_list(&self, list: &[String]) -> bool {
        list.iter().any(|n| n.eq_ignore_ascii_case(&self.name))
    }

    fn fail_in(&self, path: &str) -> Error {
        Error {
            hint: None,
            message: messages::failed_to_encode(path, &self.name, "UTF-8"),
        }
    }

    /// The UTF-8 form of the work tree content `data` of `path`. With
    /// `roundtrip`, the result is encoded again and must give `data` back.
    ///
    /// # Errors
    /// BOM rules, content that is not valid in the encoding, an unknown
    /// encoding, a failed round trip.
    pub fn decode(&self, path: &str, data: &[u8], roundtrip: bool) -> Result<Vec<u8>, Error> {
        if let Kind::Unicode { width, endian, bom } = self.kind {
            self.check_bom(path, data, width, endian, bom)?;
        }
        let out = match self.kind {
            Kind::Utf8 => return Ok(data.to_vec()),
            Kind::Unknown => return Err(self.fail_in(path)),
            Kind::Latin1 => {
                let s: String = data.iter().map(|&b| char::from(b)).collect();
                s.into_bytes()
            }
            Kind::Other(enc) => {
                let (s, had_errors) = enc.decode_without_bom_handling(data);
                if had_errors {
                    return Err(self.fail_in(path));
                }
                s.into_owned().into_bytes()
            }
            Kind::Unicode { width, endian, .. } => {
                let (body, e) = match endian {
                    Some(e) => (data, e),
                    None => strip_bom(data, width).unwrap_or((data, Endian::Little)),
                };
                let (body, e) = if endian.is_some() {
                    // An explicit endianness with BOM (`-BOM` names) may
                    // have one: drop it.
                    strip_bom(body, width)
                        .filter(|(_, be)| *be == e)
                        .unwrap_or((body, e))
                } else {
                    (body, e)
                };
                decode_unicode(body, width, e).ok_or_else(|| self.fail_in(path))?
            }
        };
        if roundtrip {
            let back = self.encode_raw(&out);
            if back.as_deref() != Some(data) {
                return Err(Error {
                    hint: None,
                    message: messages::roundtrip_failed(path, &self.name),
                });
            }
        }
        Ok(out)
    }

    fn check_bom(
        &self,
        path: &str,
        data: &[u8],
        width: usize,
        endian: Option<Endian>,
        bom: bool,
    ) -> Result<(), Error> {
        let has_bom = strip_bom(data, width).is_some();
        let bits = if width == 2 { "16" } else { "32" };
        if endian.is_some() && !bom && has_bom {
            return Err(Error {
                hint: Some(messages::bom_prohibited_hint(path, bits)),
                message: messages::bom_prohibited(path, &self.name),
            });
        }
        if endian.is_none() && !has_bom {
            return Err(Error {
                hint: Some(messages::bom_required_hint(path, bits)),
                message: messages::bom_required(path, &self.name),
            });
        }
        Ok(())
    }

    /// The work tree form of the UTF-8 blob `data` of `path`.
    ///
    /// # Errors
    /// `data` is not valid UTF-8 or cannot be expressed in the encoding.
    pub fn encode(&self, path: &str, data: &[u8]) -> Result<Vec<u8>, Error> {
        self.encode_raw(data).ok_or_else(|| Error {
            hint: None,
            message: messages::failed_to_encode(path, "UTF-8", &self.name),
        })
    }

    fn encode_raw(&self, data: &[u8]) -> Option<Vec<u8>> {
        match self.kind {
            Kind::Utf8 => Some(data.to_vec()),
            Kind::Unknown => None,
            Kind::Latin1 => {
                let s = std::str::from_utf8(data).ok()?;
                s.chars().map(|c| u8::try_from(u32::from(c)).ok()).collect()
            }
            Kind::Other(enc) => {
                let s = std::str::from_utf8(data).ok()?;
                let (out, _, had_errors) = enc.output_encoding().encode(s);
                if had_errors || enc.output_encoding() != enc {
                    return None;
                }
                Some(out.into_owned())
            }
            Kind::Unicode { width, endian, bom } => {
                let s = std::str::from_utf8(data).ok()?;
                let e = endian.unwrap_or(Endian::Little);
                let mut out = Vec::with_capacity(data.len() * width + 4);
                if endian.is_none() || bom {
                    out.extend_from_slice(match (width, e) {
                        (2, Endian::Little) => BOM16_LE,
                        (2, Endian::Big) => BOM16_BE,
                        (_, Endian::Little) => BOM32_LE,
                        (_, Endian::Big) => BOM32_BE,
                    });
                }
                encode_unicode(s, width, e, &mut out);
                Some(out)
            }
        }
    }
}

/// The content after a byte order mark of the given width, and the byte
/// order it announces.
fn strip_bom(data: &[u8], width: usize) -> Option<(&[u8], Endian)> {
    let (be, le) = if width == 2 {
        (BOM16_BE, BOM16_LE)
    } else {
        (BOM32_BE, BOM32_LE)
    };
    if let Some(rest) = data.strip_prefix(be) {
        return Some((rest, Endian::Big));
    }
    data.strip_prefix(le).map(|rest| (rest, Endian::Little))
}

fn decode_unicode(data: &[u8], width: usize, e: Endian) -> Option<Vec<u8>> {
    if data.len() % width != 0 {
        return None;
    }
    let mut s = String::with_capacity(data.len());
    if width == 2 {
        let units = data.as_chunks::<2>().0.iter().map(|&c| match e {
            Endian::Little => u16::from_le_bytes(c),
            Endian::Big => u16::from_be_bytes(c),
        });
        for c in char::decode_utf16(units) {
            s.push(c.ok()?);
        }
    } else {
        for &b in data.as_chunks::<4>().0 {
            let v = match e {
                Endian::Little => u32::from_le_bytes(b),
                Endian::Big => u32::from_be_bytes(b),
            };
            s.push(char::from_u32(v)?);
        }
    }
    Some(s.into_bytes())
}

fn encode_unicode(s: &str, width: usize, e: Endian, out: &mut Vec<u8>) {
    if width == 2 {
        for u in s.encode_utf16() {
            out.extend_from_slice(&match e {
                Endian::Little => u.to_le_bytes(),
                Endian::Big => u.to_be_bytes(),
            });
        }
    } else {
        for c in s.chars() {
            let v = u32::from(c);
            out.extend_from_slice(&match e {
                Endian::Little => v.to_le_bytes(),
                Endian::Big => v.to_be_bytes(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16() {
        let e = Encoding::new("utf-16");
        assert_eq!(e.decode("p", b"\xff\xfea\0b\0", false).unwrap(), b"ab");
        assert_eq!(e.decode("p", b"\xfe\xff\0a\0b", false).unwrap(), b"ab");
        let err = e.decode("p", b"\0a\0b", false).unwrap_err();
        assert!(err.hint.is_some());
        assert_eq!(e.encode("p", b"ab").unwrap(), b"\xff\xfea\0b\0");
        let be = Encoding::new("UTF-16BE");
        assert_eq!(be.encode("p", b"ab").unwrap(), b"\0a\0b");
        assert!(be.decode("p", b"\xfe\xff\0a", false).is_err());
        let lebom = Encoding::new("UTF-16LE-BOM");
        assert_eq!(lebom.encode("p", b"a").unwrap(), b"\xff\xfea\0");
        assert_eq!(lebom.decode("p", b"\xff\xfea\0", false).unwrap(), b"a");
    }

    #[test]
    fn utf32() {
        let e = Encoding::new("UTF-32");
        assert_eq!(
            e.encode("p", "\u{422}".as_bytes()).unwrap(),
            b"\xff\xfe\0\0\x22\x04\0\0"
        );
        assert_eq!(e.decode("p", b"\0\0\xfe\xff\0\0\0a", false).unwrap(), b"a");
    }

    #[test]
    fn others() {
        let l = Encoding::new("latin1");
        assert_eq!(l.decode("p", b"a\xe4", true).unwrap(), "a\u{e4}".as_bytes());
        assert_eq!(l.encode("p", "a\u{e4}".as_bytes()).unwrap(), b"a\xe4");
        assert!(Encoding::new("garbage").decode("p", b"x", false).is_err());
        let sj = Encoding::new("SHIFT-JIS");
        let data = sj.encode("p", "\u{3042}".as_bytes()).unwrap();
        assert_eq!(sj.decode("p", &data, true).unwrap(), "\u{3042}".as_bytes());
        assert!(Encoding::new("UTF-16LE").encode("p", b"\xff").is_err());
    }
}
