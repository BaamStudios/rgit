// SPDX-License-Identifier: MIT
//! The legacy graft file (`$GIT_DIR/info/grafts`, `$GIT_GRAFT_FILE`,
//! `gitrepository-layout(5)`): one line per commit, `<commit> <parent>...`,
//! that overrides the parents commands see. Superseded by
//! `git replace --graft`; reading it prints a deprecation hint
//! (`advice.graftFileDeprecated`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use rgit_config::ConfigSet;
use rgit_hash::{HashKind, ObjectId};
use rgit_messages::advice::{Advice, Advisor};

use crate::messages::grafts as msg;

/// The file was read (and reported on) by this process already.
static REPORTED: AtomicBool = AtomicBool::new(false);
/// Reading the file must not print the deprecation hint
/// (`replace --convert-graft-file`).
static QUIET_HINT: AtomicBool = AtomicBool::new(false);

/// Grafted parents by commit.
pub type Grafts = HashMap<ObjectId, Vec<ObjectId>>;

/// The graft file of a repository: `$GIT_GRAFT_FILE` if set and not empty,
/// else `<common dir>/info/grafts`.
pub fn file(common_dir: &Path) -> PathBuf {
    match std::env::var_os("GIT_GRAFT_FILE") {
        Some(f) if !f.is_empty() => PathBuf::from(f),
        _ => common_dir.join("info").join("grafts"),
    }
}

/// One line of the graft file: `None` for blank lines and comments,
/// `Some(Err(()))` for a malformed one.
pub fn parse_line(hash: HashKind, line: &str) -> Option<Result<(ObjectId, Vec<ObjectId>), ()>> {
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    // `<id>( <id>)*`, separated by single spaces.
    let mut ids = Vec::new();
    for word in line.split(' ') {
        if word.len() != hash.hex_len() {
            return Some(Err(()));
        }
        match ObjectId::from_hex(hash, word.as_bytes()) {
            Ok(id) => ids.push(id),
            Err(_) => return Some(Err(())),
        }
    }
    let commit = ids.remove(0);
    Some(Ok((commit, ids)))
}

/// Read the graft file `path`; `None` if it cannot be opened. Malformed
/// lines are reported (`error: bad graft data: <line>`) the first time
/// the process reads the file, together with the deprecation hint when
/// `advice` allows it.
pub fn read(path: &Path, hash: HashKind, advice: Option<&Advisor>) -> Option<Grafts> {
    let data = std::fs::read(path).ok()?;
    let report = !REPORTED.swap(true, Ordering::Relaxed);
    if report && !QUIET_HINT.load(Ordering::Relaxed) {
        if let Some(a) = advice {
            let _ = a.advise_without_footer(Advice::GraftFileDeprecated, msg::HINT_DEPRECATED);
        }
    }
    let mut grafts = Grafts::new();
    for line in String::from_utf8_lossy(&data).lines() {
        match parse_line(hash, line.trim_end()) {
            None => {}
            Some(Ok((commit, parents))) => {
                grafts.insert(commit, parents);
            }
            Some(Err(())) if report => eprintln!("error: {}", msg::bad_graft_data(line)),
            Some(Err(())) => {}
        }
    }
    Some(grafts)
}

/// The advisor for the deprecation hint.
pub fn advisor(config: &ConfigSet) -> Advisor {
    Advisor::new(config, &rgit_config::SystemEnv::default(), false)
        .unwrap_or_else(|_| Advisor::silent())
}

/// Do not print the deprecation hint in this process.
pub fn quiet_hint() {
    QUIET_HINT.store(true, Ordering::Relaxed);
}

/// The graft file as the revision machinery uses it: located when the
/// repository is opened, read when the first parents are asked for.
#[derive(Default)]
pub struct Lazy {
    source: Option<(PathBuf, HashKind, Advisor)>,
    grafts: std::cell::OnceCell<Grafts>,
}

impl Lazy {
    /// The graft file of the repository at `common_dir`, if there is one.
    pub fn new(common_dir: &Path, hash: HashKind, config: &ConfigSet) -> Lazy {
        let path = file(common_dir);
        let source = path.exists().then(|| (path, hash, advisor(config)));
        Lazy {
            source,
            grafts: std::cell::OnceCell::new(),
        }
    }

    /// The grafted parents of `commit`, if it has a graft entry.
    pub fn parents(&self, commit: &ObjectId) -> Option<&[ObjectId]> {
        let (path, hash, advice) = self.source.as_ref()?;
        self.grafts
            .get_or_init(|| read(path, *hash, Some(advice)).unwrap_or_default())
            .get(commit)
            .map(Vec::as_slice)
    }

    /// Read the file now (prints the hint), as git does when a command
    /// parses its first commit.
    pub fn touch(&self) {
        if let Some((_, hash, _)) = &self.source {
            let _ = self.parents(&ObjectId::null(*hash));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graft_lines() {
        let h = HashKind::Sha1;
        let a = "1".repeat(40);
        let b = "2".repeat(40);
        let id = |s: &str| ObjectId::from_hex(h, s.as_bytes()).unwrap();
        assert_eq!(parse_line(h, ""), None);
        assert_eq!(parse_line(h, "# comment"), None);
        assert_eq!(parse_line(h, &a), Some(Ok((id(&a), vec![]))));
        assert_eq!(
            parse_line(h, &format!("{a} {b}")),
            Some(Ok((id(&a), vec![id(&b)])))
        );
        assert_eq!(parse_line(h, "junk"), Some(Err(())));
        assert_eq!(parse_line(h, &format!("{a}  {b}")), Some(Err(())));
        assert_eq!(parse_line(h, &format!("{a} zz")), Some(Err(())));
    }
}
