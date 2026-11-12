// SPDX-License-Identifier: MIT

//! `:(attr:...)` requirements. Parsed here; evaluating them needs the
//! attributes machinery and is done by the caller (see
//! [`crate::Pathspec::match_path_with`]).

use crate::PathspecError;

/// One requirement of an `attr:` magic word.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AttrRequirement {
    /// `NAME`: the attribute must be set.
    Set(Vec<u8>),
    /// `-NAME`: the attribute must be unset.
    Unset(Vec<u8>),
    /// `!NAME`: the attribute must be unspecified.
    Unspecified(Vec<u8>),
    /// `NAME=VALUE`: the attribute must have this value.
    Value(Vec<u8>, Vec<u8>),
}

impl AttrRequirement {
    /// The attribute name.
    #[must_use]
    pub fn name(&self) -> &[u8] {
        match self {
            Self::Set(n) | Self::Unset(n) | Self::Unspecified(n) | Self::Value(n, _) => n,
        }
    }
}

/// The `attr:` part of a pathspec: the raw text and its requirements.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AttrSpec {
    /// Text after `attr:` as written (escapes not removed).
    pub raw: Vec<u8>,
    /// The parsed requirements, all of which must hold.
    pub requirements: Vec<AttrRequirement>,
}

impl AttrSpec {
    /// Parses the text after `attr:`.
    ///
    /// # Errors
    /// [`PathspecError::EmptyAttr`], [`PathspecError::InvalidAttrName`],
    /// [`PathspecError::InvalidAttrValue`].
    pub fn parse(raw: &[u8]) -> Result<Self, PathspecError> {
        let mut requirements = Vec::new();
        for token in raw.split(u8::is_ascii_whitespace).filter(|t| !t.is_empty()) {
            let req = match token[0] {
                b'-' => AttrRequirement::Unset(valid_name(&token[1..])?),
                b'!' => AttrRequirement::Unspecified(valid_name(&token[1..])?),
                _ => match token.iter().position(|&b| b == b'=') {
                    Some(eq) => {
                        let name = valid_name(&token[..eq])?;
                        AttrRequirement::Value(name, value(&token[eq + 1..])?)
                    }
                    None => AttrRequirement::Set(valid_name(token)?),
                },
            };
            requirements.push(req);
        }
        if requirements.is_empty() {
            return Err(PathspecError::EmptyAttr);
        }
        Ok(Self {
            raw: raw.to_vec(),
            requirements,
        })
    }
}

/// Attribute names: ASCII letters, digits, `-`, `_`, `.`, not starting
/// with `-` (gitattributes(5)).
fn valid_name(name: &[u8]) -> Result<Vec<u8>, PathspecError> {
    let ok = !name.is_empty()
        && name[0] != b'-'
        && name
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    if ok {
        Ok(name.to_vec())
    } else {
        Err(PathspecError::InvalidAttrName {
            name: name.to_vec(),
        })
    }
}

/// Removes backslash escapes from a value and rejects characters that
/// cannot be matched.
fn value(raw: &[u8]) -> Result<Vec<u8>, PathspecError> {
    let mut out = Vec::with_capacity(raw.len());
    let mut it = raw.iter();
    while let Some(&b) = it.next() {
        let c = if b == b'\\' {
            it.next().copied().unwrap_or(b'\\')
        } else {
            b
        };
        if matches!(c, b'=' | b')') {
            return Err(PathspecError::InvalidAttrValue { ch: c });
        }
        out.push(c);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requirements() {
        let s = AttrSpec::parse(b" a  -b !c d=e\\,f ").unwrap();
        assert_eq!(
            s.requirements,
            vec![
                AttrRequirement::Set(b"a".to_vec()),
                AttrRequirement::Unset(b"b".to_vec()),
                AttrRequirement::Unspecified(b"c".to_vec()),
                AttrRequirement::Value(b"d".to_vec(), b"e,f".to_vec()),
            ]
        );
    }

    #[test]
    fn errors() {
        assert_eq!(AttrSpec::parse(b""), Err(PathspecError::EmptyAttr));
        assert_eq!(
            AttrSpec::parse(b"-").unwrap_err().to_string(),
            "invalid attribute name "
        );
        assert_eq!(
            AttrSpec::parse(b"$$").unwrap_err().to_string(),
            "invalid attribute name $$"
        );
        assert_eq!(
            AttrSpec::parse(b"a=b=c").unwrap_err().to_string(),
            "cannot use '=' for value matching"
        );
        assert_eq!(
            AttrSpec::parse(b"a=x\\)y").unwrap_err().to_string(),
            "cannot use ')' for value matching"
        );
        assert_eq!(
            AttrSpec::parse(b"-a=b").unwrap_err().to_string(),
            "invalid attribute name a=b"
        );
    }
}
