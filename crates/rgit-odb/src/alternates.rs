// SPDX-License-Identifier: MIT

//! Alternate object directories (`objects/info/alternates`,
//! `$GIT_ALTERNATE_OBJECT_DIRECTORIES`), see `gitrepository-layout` and
//! `git(1)`.

use std::path::{Path, PathBuf};

/// The environment variable listing extra object directories, separated by
/// `:` (`;` on Windows).
pub const ALTERNATE_OBJECT_DIRECTORIES_ENV: &str = "GIT_ALTERNATE_OBJECT_DIRECTORIES";

/// Alternates are followed this many links deep: the entries of the
/// repository's own file (and of the environment) are at depth 0, their
/// entries at depth 1, and so on up to this depth (as in git).
const MAX_DEPTH: usize = 5;

#[cfg(windows)]
const ENV_SEPARATOR: u8 = b';';
#[cfg(not(windows))]
const ENV_SEPARATOR: u8 = b':';

/// The alternate object directories of `objects_dir`, in lookup order and
/// without duplicates or `objects_dir` itself: first those of
/// `$GIT_ALTERNATE_OBJECT_DIRECTORIES`, then the lines of
/// `objects/info/alternates` (relative paths are relative to the object
/// directory holding the file), each followed by its own alternates.
/// Directories that do not exist are left out.
pub fn resolve(objects_dir: &Path) -> Vec<PathBuf> {
    resolve_reporting(objects_dir, &mut |_| {})
}

/// [`resolve`], passing the errors git prints while reading alternates
/// (nesting too deep) to `report` (print each with `error: `).
pub fn resolve_reporting(objects_dir: &Path, report: &mut dyn FnMut(String)) -> Vec<PathBuf> {
    let mut seen = vec![canonical(objects_dir)];
    let mut out = Vec::new();
    if let Some(env) = std::env::var_os(ALTERNATE_OBJECT_DIRECTORIES_ENV) {
        let env = env.to_string_lossy().into_owned();
        for entry in split_env(env.as_bytes()) {
            add(path_of(entry), 0, &mut seen, &mut out, report);
        }
    }
    for entry in read_file(objects_dir) {
        add(entry, 0, &mut seen, &mut out, report);
    }
    out
}

fn add(
    dir: PathBuf,
    depth: usize,
    seen: &mut Vec<PathBuf>,
    out: &mut Vec<PathBuf>,
    report: &mut dyn FnMut(String),
) {
    if !dir.is_dir() {
        return;
    }
    let key = canonical(&dir);
    if seen.contains(&key) {
        return;
    }
    seen.push(key);
    out.push(dir.clone());
    let entries = read_file(&dir);
    if entries.is_empty() {
        return;
    }
    if depth + 1 > MAX_DEPTH {
        report(crate::messages::alternates_too_deep(&dir));
        return;
    }
    for entry in entries {
        add(entry, depth + 1, seen, out, report);
    }
}

fn path_of(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
}

/// The entries of `$GIT_ALTERNATE_OBJECT_DIRECTORIES`: separated by `:`;
/// an entry starting with `"` is C-quoted (and may contain `:`). Broken
/// quoting falls back to the raw text up to the next separator.
fn split_env(mut s: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while !s.is_empty() {
        if s[0] == b'"' {
            if let Some((path, rest)) = unquote_prefix(s) {
                if rest.is_empty() || rest[0] == ENV_SEPARATOR {
                    if !path.is_empty() {
                        out.push(path);
                    }
                    s = rest.get(1..).unwrap_or_default();
                    continue;
                }
            }
        }
        let end = s
            .iter()
            .position(|&b| b == ENV_SEPARATOR)
            .unwrap_or(s.len());
        if end > 0 {
            out.push(s[..end].to_vec());
        }
        s = s.get(end + 1..).unwrap_or_default();
    }
    out
}

/// The entries of `<objects_dir>/info/alternates`, resolved against
/// `objects_dir`. Lines starting with `#` are comments; a line starting
/// with `"` is C-quoted (left out when the quoting is broken).
fn read_file(objects_dir: &Path) -> Vec<PathBuf> {
    let Ok(data) = std::fs::read(objects_dir.join("info").join("alternates")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in data.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() || line[0] == b'#' {
            continue;
        }
        let path = if line[0] == b'"' {
            match unquote_prefix(line) {
                Some((p, [])) => p,
                _ => continue,
            }
        } else {
            line.to_vec()
        };
        let p = path_of(path);
        out.push(if p.is_absolute() {
            p
        } else {
            objects_dir.join(p)
        });
    }
    out
}

/// Unquote the C-quoted string at the start of `s` (which starts with
/// `"`); returns it and the bytes after the closing quote.
fn unquote_prefix(s: &[u8]) -> Option<(Vec<u8>, &[u8])> {
    let body = s.strip_prefix(b"\"")?;
    let mut out = Vec::new();
    let mut i = 0;
    while i < body.len() {
        let c = body[i];
        i += 1;
        match c {
            b'"' => return Some((out, &body[i..])),
            b'\\' => {
                let e = *body.get(i)?;
                i += 1;
                let b = match e {
                    b'a' => 7,
                    b'b' => 8,
                    b'f' => 12,
                    b'n' => b'\n',
                    b'r' => b'\r',
                    b't' => b'\t',
                    b'v' => 11,
                    b'\\' | b'"' => e,
                    b'0'..=b'3' => {
                        let d2 = *body.get(i)?;
                        let d3 = *body.get(i + 1)?;
                        i += 2;
                        if !(b'0'..=b'7').contains(&d2) || !(b'0'..=b'7').contains(&d3) {
                            return None;
                        }
                        ((e - b'0') << 6) | ((d2 - b'0') << 3) | (d3 - b'0')
                    }
                    _ => return None,
                };
                out.push(b);
            }
            _ => out.push(c),
        }
    }
    None
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_nested_and_duplicate_entries() {
        let tmp = std::env::temp_dir().join(format!("rgit-odb-alt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let main = tmp.join("main/objects");
        let a = tmp.join("a/objects");
        let b = tmp.join("b/objects");
        for d in [&main, &a, &b] {
            std::fs::create_dir_all(d.join("info")).unwrap();
        }
        std::fs::write(
            main.join("info/alternates"),
            "# comment\n../../a/objects\n\n/does/not/exist\n",
        )
        .unwrap();
        std::fs::write(a.join("info/alternates"), format!("\"{}\"\n", b.display())).unwrap();
        // A cycle back to the main directory is ignored.
        std::fs::write(b.join("info/alternates"), format!("{}\n", main.display())).unwrap();
        let got: Vec<PathBuf> = read_file(&main);
        assert_eq!(got.len(), 2);
        let mut seen = vec![canonical(&main)];
        let mut out = Vec::new();
        for e in got {
            add(e, 0, &mut seen, &mut out, &mut |_| {});
        }
        let out: Vec<PathBuf> = out.iter().map(|p| canonical(p)).collect();
        assert_eq!(out, vec![canonical(&a), canonical(&b)]);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn environment_entries_with_quotes() {
        let sep = ENV_SEPARATOR as char;
        let s = format!("\"a\\057b\"{sep}c{sep}{sep}\"d{sep}e\"{sep}\"broken");
        let got: Vec<String> = split_env(s.as_bytes())
            .into_iter()
            .map(|e| String::from_utf8(e).unwrap())
            .collect();
        assert_eq!(got, ["a/b", "c", &format!("d{sep}e"), "\"broken"]);
    }
}
