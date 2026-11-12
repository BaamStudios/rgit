// SPDX-License-Identifier: MIT

//! Alternate object directories (`objects/info/alternates`,
//! `$GIT_ALTERNATE_OBJECT_DIRECTORIES`), see `gitrepository-layout`.

use std::path::{Path, PathBuf};

/// The environment variable listing extra object directories, separated by
/// `:` (`;` on Windows).
pub const ALTERNATE_OBJECT_DIRECTORIES_ENV: &str = "GIT_ALTERNATE_OBJECT_DIRECTORIES";

/// Alternates are followed this many levels deep, as in git.
const MAX_DEPTH: usize = 5;

#[cfg(windows)]
const ENV_SEPARATOR: char = ';';
#[cfg(not(windows))]
const ENV_SEPARATOR: char = ':';

/// The alternate object directories of `objects_dir`, in lookup order and
/// without duplicates or `objects_dir` itself: first those of
/// `$GIT_ALTERNATE_OBJECT_DIRECTORIES`, then the lines of
/// `objects/info/alternates` (relative paths are relative to the object
/// directory holding the file), each followed by its own alternates.
/// Directories that do not exist are left out.
pub fn resolve(objects_dir: &Path) -> Vec<PathBuf> {
    let mut seen = vec![canonical(objects_dir)];
    let mut out = Vec::new();
    if let Some(env) = std::env::var_os(ALTERNATE_OBJECT_DIRECTORIES_ENV) {
        let env = env.to_string_lossy().into_owned();
        for entry in env.split(ENV_SEPARATOR).filter(|e| !e.is_empty()) {
            add(PathBuf::from(entry), 0, &mut seen, &mut out);
        }
    }
    for entry in read_file(objects_dir) {
        add(entry, 0, &mut seen, &mut out);
    }
    out
}

fn add(dir: PathBuf, depth: usize, seen: &mut Vec<PathBuf>, out: &mut Vec<PathBuf>) {
    if !dir.is_dir() {
        return;
    }
    let key = canonical(&dir);
    if seen.contains(&key) {
        return;
    }
    seen.push(key);
    out.push(dir.clone());
    if depth + 1 > MAX_DEPTH {
        return;
    }
    for entry in read_file(&dir) {
        add(entry, depth + 1, seen, out);
    }
}

/// The entries of `<objects_dir>/info/alternates`, resolved against
/// `objects_dir`.
fn read_file(objects_dir: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read(objects_dir.join("info").join("alternates")) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&text)
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let p = Path::new(l);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                objects_dir.join(p)
            }
        })
        .collect()
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
        std::fs::write(a.join("info/alternates"), format!("{}\n", b.display())).unwrap();
        // A cycle back to the main directory is ignored.
        std::fs::write(b.join("info/alternates"), format!("{}\n", main.display())).unwrap();
        let got: Vec<PathBuf> = read_file(&main);
        assert_eq!(got.len(), 2);
        let mut seen = vec![canonical(&main)];
        let mut out = Vec::new();
        for e in got {
            add(e, 0, &mut seen, &mut out);
        }
        let out: Vec<PathBuf> = out.iter().map(|p| canonical(p)).collect();
        assert_eq!(out, vec![canonical(&a), canonical(&b)]);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
