// SPDX-License-Identifier: MIT
//! Variable names given by the user (`-c`, `GIT_CONFIG_KEY_<n>`, lookups).

use bstr::{BStr, BString, ByteSlice};

use crate::messages;

/// A validated variable name split into its parts.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key {
    /// Lower-cased section name.
    pub section: BString,
    /// Subsection, case preserved.
    pub subsection: Option<BString>,
    /// Lower-cased variable name.
    pub name: BString,
}

/// Why a variable name was rejected. `Display` gives git's message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// No dot in the name.
    #[error("{}", messages::key_no_section(.0.as_bstr()))]
    NoSection(BString),
    /// Nothing after the last dot.
    #[error("{}", messages::key_no_variable(.0.as_bstr()))]
    NoVariable(BString),
    /// Invalid character in section or variable name.
    #[error("{}", messages::invalid_key(.0.as_bstr()))]
    Invalid(BString),
    /// Newline in the subsection.
    #[error("{}", messages::invalid_key_newline(.0.as_bstr()))]
    Newline(BString),
}

impl Key {
    /// Parse and validate a name like `remote.origin.url`: the section is
    /// everything before the first dot, the variable everything after the
    /// last dot, and the subsection (if any) what lies between.
    pub fn parse(input: impl AsRef<[u8]>) -> Result<Key, KeyError> {
        let input = input.as_ref();
        let last = match input.rfind_byte(b'.') {
            Some(i) if i > 0 => i,
            _ => return Err(KeyError::NoSection(input.into())),
        };
        if last + 1 == input.len() {
            return Err(KeyError::NoVariable(input.into()));
        }
        let first = input.find_byte(b'.').expect("has a dot");
        let section = &input[..first];
        let name = &input[last + 1..];
        let invalid = || KeyError::Invalid(input.into());
        if !section
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b'-')
        {
            return Err(invalid());
        }
        if !name[0].is_ascii_alphabetic()
            || !name.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
        {
            return Err(invalid());
        }
        let subsection = if first < last {
            let sub = &input[first + 1..last];
            if sub.contains(&b'\n') {
                return Err(KeyError::Newline(input.into()));
            }
            Some(BString::from(sub))
        } else {
            None
        };
        Ok(Key {
            section: section.to_ascii_lowercase().into(),
            subsection,
            name: name.to_ascii_lowercase().into(),
        })
    }

    /// Canonical form: `section[.subsection].name`.
    pub fn canonical(&self) -> BString {
        let mut out = self.section.clone();
        if let Some(sub) = &self.subsection {
            out.push(b'.');
            out.extend_from_slice(sub);
        }
        out.push(b'.');
        out.extend_from_slice(&self.name);
        out
    }
}

/// Canonicalize a name for lookup: `Key::parse(..).canonical()`.
pub fn canonicalize(input: impl AsRef<[u8]>) -> Result<BString, KeyError> {
    Key::parse(input).map(|k| k.canonical())
}

/// Split a canonical name into (section, subsection, variable) without
/// validation: section up to the first dot, variable after the last dot.
pub fn split_canonical(name: &BStr) -> (&BStr, Option<&BStr>, &BStr) {
    match (name.find_byte(b'.'), name.rfind_byte(b'.')) {
        (Some(f), Some(l)) if f < l => (
            name[..f].as_bstr(),
            Some(name[f + 1..l].as_bstr()),
            name[l + 1..].as_bstr(),
        ),
        (Some(f), Some(_)) => (name[..f].as_bstr(), None, name[f + 1..].as_bstr()),
        _ => (b"".as_bstr(), None, name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid() {
        assert_eq!(canonicalize("S.Ab.K").unwrap(), "s.Ab.k");
        assert_eq!(canonicalize("s.a b.K").unwrap(), "s.a b.k");
        assert_eq!(canonicalize("1s.k").unwrap(), "1s.k");
        assert_eq!(canonicalize("a.x=y.k").unwrap(), "a.x=y.k");
        assert_eq!(canonicalize("a.b.c.d").unwrap(), "a.b.c.d");
        assert_eq!(canonicalize("a..b").unwrap(), "a..b");
    }

    #[test]
    fn invalid() {
        for (input, msg) in [
            ("s", "key does not contain a section: s"),
            (".k", "key does not contain a section: .k"),
            ("s.k.", "key does not contain variable name: s.k."),
            ("a.", "key does not contain variable name: a."),
            ("s.1k", "invalid key: s.1k"),
            ("s_x.k", "invalid key: s_x.k"),
            ("s\nx.k", "invalid key: s\nx.k"),
            ("s.x\ny.k", "invalid key (newline): s.x\ny.k"),
            ("a.\"b c\".0d", "invalid key: a.\"b c\".0d"),
            ("", "key does not contain a section: "),
        ] {
            assert_eq!(Key::parse(input).unwrap_err().to_string(), msg, "{input}");
        }
    }
}
