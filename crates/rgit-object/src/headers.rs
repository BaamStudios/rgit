// SPDX-License-Identifier: MIT

//! The header block shared by commits and tags: `name SP value LF` lines,
//! where lines starting with a space continue the previous value, followed
//! by an optional empty line and the message.

use crate::Error;

/// One header of a commit or tag, such as `tree <id>` or a multi-line
/// `gpgsig`.
///
/// `value` holds the text after the first space of the first line; each
/// continuation line adds `\n` plus the line without its leading space.
/// A first line without any space (only possible in odd objects) has
/// `has_space == false` and an empty value before any continuation.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Header {
    /// The header name, e.g. `b"parent"`.
    pub name: Vec<u8>,
    /// The value with continuation lines joined by `\n`.
    pub value: Vec<u8>,
    /// Whether the first line had a space after the name.
    pub has_space: bool,
}

impl Header {
    /// A header `name SP value`; a `\n` in `value` becomes a continuation line.
    pub fn new(name: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> Header {
        Header {
            name: name.into(),
            value: value.into(),
            has_space: true,
        }
    }

    /// Append the serialised header (with trailing LF) to `out`.
    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.name);
        if self.has_space {
            out.push(b' ');
        }
        for (i, line) in self.value.split(|&b| b == b'\n').enumerate() {
            if i > 0 {
                out.extend_from_slice(b"\n ");
            }
            out.extend_from_slice(line);
        }
        out.push(b'\n');
    }
}

/// The headers of a commit or tag together with the message.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct HeaderBlock {
    /// The headers in stored order.
    pub headers: Vec<Header>,
    /// The bytes after the empty line that ends the headers; `None` when the
    /// object has no empty line at all (headers only).
    pub message: Option<Vec<u8>>,
}

impl HeaderBlock {
    /// Split `data` into headers and message.
    ///
    /// This is tolerant: any header names, order and NUL bytes are accepted,
    /// so that every object round-trips. The only error is a header block
    /// whose last line lacks its LF ([`Error::UnterminatedHeader`]), which
    /// cannot be represented.
    pub fn parse(data: &[u8]) -> Result<HeaderBlock, Error> {
        let mut headers: Vec<Header> = Vec::new();
        let mut pos = 0;
        let message = loop {
            if pos == data.len() {
                break None;
            }
            if data[pos] == b'\n' {
                break Some(data[pos + 1..].to_vec());
            }
            let nl = data[pos..]
                .iter()
                .position(|&b| b == b'\n')
                .ok_or(Error::UnterminatedHeader)?;
            let line = &data[pos..pos + nl];
            pos += nl + 1;
            match headers.last_mut() {
                Some(last) if line[0] == b' ' => {
                    last.value.push(b'\n');
                    last.value.extend_from_slice(&line[1..]);
                }
                _ => headers.push(match line.iter().position(|&b| b == b' ') {
                    Some(sp) => Header {
                        name: line[..sp].to_vec(),
                        value: line[sp + 1..].to_vec(),
                        has_space: true,
                    },
                    None => Header {
                        name: line.to_vec(),
                        value: Vec::new(),
                        has_space: false,
                    },
                }),
            }
        };
        Ok(HeaderBlock { headers, message })
    }

    /// Append the serialised form to `out`.
    pub fn write_to(&self, out: &mut Vec<u8>) {
        for h in &self.headers {
            h.write_to(out);
        }
        if let Some(m) = &self.message {
            out.push(b'\n');
            out.extend_from_slice(m);
        }
    }

    /// The value of the first header called `name`.
    pub fn get(&self, name: &[u8]) -> Option<&[u8]> {
        self.headers
            .iter()
            .find(|h| h.name == name)
            .map(|h| h.value.as_slice())
    }

    /// The values of all headers called `name`, in order.
    pub fn get_all<'a>(&'a self, name: &'a [u8]) -> impl Iterator<Item = &'a [u8]> + 'a {
        self.headers
            .iter()
            .filter(move |h| h.name == name)
            .map(|h| h.value.as_slice())
    }

    /// The message, empty when there is none.
    pub fn message(&self) -> &[u8] {
        self.message.as_deref().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(data: &[u8]) -> HeaderBlock {
        let b = HeaderBlock::parse(data).unwrap();
        let mut out = Vec::new();
        b.write_to(&mut out);
        assert_eq!(out, data, "{:?}", String::from_utf8_lossy(data));
        b
    }

    #[test]
    fn round_trips() {
        rt(b"");
        rt(b"\n");
        rt(b"\n\n");
        rt(b"a b\n");
        rt(b"a b\n\nmsg");
        rt(b"a\n b\n\n");
        rt(b" x\n y\n\nm\n");
        rt(b"a \n  \n \n\nm\0x");
        rt(b"a b\0c\n");
        let b = rt(b"gpgsig -----BEGIN\n \n line\n -----END\nx y\n\nmsg\n");
        assert_eq!(b.get(b"gpgsig").unwrap(), b"-----BEGIN\n\nline\n-----END");
        assert_eq!(b.get(b"x").unwrap(), b"y");
        assert_eq!(b.message(), b"msg\n");
    }

    #[test]
    fn unterminated() {
        assert_eq!(HeaderBlock::parse(b"a b"), Err(Error::UnterminatedHeader));
        assert_eq!(
            HeaderBlock::parse(b"a b\n c"),
            Err(Error::UnterminatedHeader)
        );
    }
}
