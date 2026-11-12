// SPDX-License-Identifier: MIT
//! Path output: C-style quoting as described for `core.quotePath`
//! (git-config(1)) and paths relative to the current directory.

/// Append `path` to `out`, quoted in C style when it contains a control
/// character, `"`, `\`, DEL or (with `quote_high`, i.e. `core.quotePath`)
/// a byte of 0x80 or above.
pub fn write_quoted(out: &mut Vec<u8>, path: &[u8], quote_high: bool) {
    let needs =
        |c: u8| c < 0x20 || c == b'"' || c == b'\\' || c == 0x7f || (quote_high && c >= 0x80);
    if !path.iter().any(|&c| needs(c)) {
        out.extend_from_slice(path);
        return;
    }
    out.push(b'"');
    for &c in path {
        match c {
            0x07 => out.extend_from_slice(b"\\a"),
            0x08 => out.extend_from_slice(b"\\b"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\n' => out.extend_from_slice(b"\\n"),
            0x0b => out.extend_from_slice(b"\\v"),
            0x0c => out.extend_from_slice(b"\\f"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            c if needs(c) => {
                out.push(b'\\');
                out.push(b'0' + (c >> 6));
                out.push(b'0' + ((c >> 3) & 7));
                out.push(b'0' + (c & 7));
            }
            c => out.push(c),
        }
    }
    out.push(b'"');
}

/// `path` (relative to the top of the work tree) as seen from the
/// directory `prefix` (empty, or ending in `/`): `../` for every directory
/// of the prefix that the path is not in, `./` when nothing is left.
pub fn relative_to(path: &[u8], prefix: &[u8]) -> Vec<u8> {
    if prefix.is_empty() {
        return path.to_vec();
    }
    // Longest common leading directory part.
    let mut common = 0;
    let mut i = 0;
    while i < prefix.len() && i < path.len() && prefix[i] == path[i] {
        if prefix[i] == b'/' {
            common = i + 1;
        }
        i += 1;
    }
    // `path` may name a directory of the prefix itself ("a" for "a/").
    if i == path.len() && i < prefix.len() && prefix[i] == b'/' {
        common = i + 1;
    }
    let rest_prefix = &prefix[common..];
    let rest_path = path.get(common..).unwrap_or_default();
    let mut out = Vec::new();
    for _ in rest_prefix.split(|&b| b == b'/').filter(|c| !c.is_empty()) {
        out.extend_from_slice(b"../");
    }
    out.extend_from_slice(rest_path);
    if out.is_empty() {
        out.extend_from_slice(b"./");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(p: &[u8], high: bool) -> String {
        let mut v = Vec::new();
        write_quoted(&mut v, p, high);
        String::from_utf8_lossy(&v).into_owned()
    }

    #[test]
    fn quoting() {
        assert_eq!(q(b"plain", true), "plain");
        assert_eq!(q(b"c/\"q", true), "\"c/\\\"q\"");
        assert_eq!(q("ä\tq".as_bytes(), true), "\"\\303\\244\\tq\"");
        assert_eq!(q("ä\tq".as_bytes(), false), "\"ä\\tq\"");
        assert_eq!(q(b"d\x7f", false), "\"d\\177\"");
        assert_eq!(q("ä".as_bytes(), false), "ä");
    }

    #[test]
    fn relative() {
        assert_eq!(relative_to(b"a/b", b""), b"a/b");
        assert_eq!(relative_to(b"a/b/f", b"a/b/"), b"f");
        assert_eq!(relative_to(b"a/g", b"a/b/"), b"../g");
        assert_eq!(relative_to(b"a", b"a/b/"), b"../");
        assert_eq!(relative_to(b"a/b", b"a/b/"), b"./");
        assert_eq!(relative_to(b"c", b"a/b/"), b"../../c");
        assert_eq!(relative_to(b"ab", b"a/"), b"../ab");
        assert_eq!(relative_to(b"a", b"a/"), b"./");
    }
}
