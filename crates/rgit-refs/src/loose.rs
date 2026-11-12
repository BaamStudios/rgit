// SPDX-License-Identifier: MIT
//! Content of loose reference files.
//!
//! A loose reference is a file below `$GIT_DIR` that contains either an
//! object id in hex followed by a newline, or `ref: <name>` followed by a
//! newline (`gitrepository-layout`). Readers are lenient in the same way as
//! the reference `git`: the newline may be missing, whitespace may follow
//! the object id, and whitespace may separate `ref:` from the name.

use rgit_hash::{HashKind, ObjectId};

use crate::name::{RefNameFlags, is_valid_refname};
use crate::target::Target;

/// Parses the content of a loose reference file. `None` means the file is
/// broken.
pub fn parse(content: &[u8], kind: HashKind) -> Option<Target> {
    if let Some(rest) = content.strip_prefix(b"ref:") {
        let start = rest
            .iter()
            .position(|b| !is_space(*b))
            .unwrap_or(rest.len());
        let end = rest
            .iter()
            .rposition(|b| !is_space(*b))
            .map_or(start, |p| p + 1);
        let name = rest.get(start..end)?;
        if !is_valid_refname(name, RefNameFlags::ONELEVEL) {
            return None;
        }
        return Some(Target::Symbolic(String::from_utf8(name.to_vec()).ok()?));
    }
    let hex_len = kind.hex_len();
    let hex = content.get(..hex_len)?;
    if let Some(&next) = content.get(hex_len) {
        if !is_space(next) {
            return None;
        }
    }
    ObjectId::from_hex(kind, hex).ok().map(Target::Object)
}

/// Serializes a target as git writes it.
pub fn serialize(target: &Target) -> Vec<u8> {
    match target {
        Target::Object(id) => format!("{}\n", id.to_hex()).into_bytes(),
        Target::Symbolic(name) => format!("ref: {name}\n").into_bytes(),
    }
}

/// C `isspace` in the C locale.
pub(crate) fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "920e1e2790b00f7617fdf62ba3e9e5937050d6f9";

    fn oid() -> Target {
        Target::Object(ObjectId::from_hex(HashKind::Sha1, HEX.as_bytes()).unwrap())
    }

    #[test]
    fn lenient_parsing() {
        let k = HashKind::Sha1;
        assert_eq!(parse(format!("{HEX}\n").as_bytes(), k), Some(oid()));
        assert_eq!(parse(HEX.as_bytes(), k), Some(oid()));
        assert_eq!(parse(format!("{HEX} garbage\n").as_bytes(), k), Some(oid()));
        assert_eq!(parse(format!("{HEX}x\n").as_bytes(), k), None);
        assert_eq!(parse(&HEX.as_bytes()[..39], k), None);
        let sym = Some(Target::Symbolic("refs/heads/main".into()));
        assert_eq!(parse(b"ref: refs/heads/main\n", k), sym);
        assert_eq!(parse(b"ref:refs/heads/main", k), sym);
        assert_eq!(parse(b"ref:   refs/heads/main  \n", k), sym);
        assert_eq!(parse(b"ref: refs/heads/main\textra\n", k), None);
        assert_eq!(parse(b"ref: \n", k), None);
    }

    #[test]
    fn round_trip() {
        assert_eq!(serialize(&oid()), format!("{HEX}\n").into_bytes());
        assert_eq!(
            serialize(&Target::Symbolic("refs/heads/x".into())),
            b"ref: refs/heads/x\n"
        );
    }
}
