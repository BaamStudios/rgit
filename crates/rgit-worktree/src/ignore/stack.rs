// SPDX-License-Identifier: MIT

//! All exclude sources of a work tree and git's precedence between them.

use std::collections::HashMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use super::pattern::{Pattern, PatternList};
use crate::messages;
use crate::os;

/// In-tree pattern files larger than this are ignored with a warning.
pub const MAX_PATTERN_FILE: u64 = 100 * 1024 * 1024;

/// The pattern that decided about a path, and the list it belongs to.
#[derive(Debug, Clone, Copy)]
pub struct Match<'a> {
    /// The source of the pattern.
    pub list: &'a PatternList,
    /// The pattern.
    pub pattern: &'a Pattern,
}

impl Match<'_> {
    /// Whether the path is excluded (the pattern is not negated).
    #[must_use]
    pub fn is_excluded(&self) -> bool {
        !self.pattern.is_negative()
    }
}

/// The exclude sources of one work tree, highest precedence first:
///
/// 1. patterns given on the command line (`ls-files -x`),
/// 2. per-directory files (`.gitignore`), deeper directories first,
/// 3. pattern files (`ls-files -X`, `$GIT_COMMON_DIR/info/exclude`,
///    `core.excludesFile`), the file added last first.
///
/// Within one source the last matching pattern decides. Per-directory files
/// are read lazily, without following a symbolic link at the file itself,
/// and cached.
#[derive(Debug)]
pub struct Excludes {
    root: PathBuf,
    icase: bool,
    cmdline: PatternList,
    per_dir_name: Option<Vec<u8>>,
    per_dir: HashMap<Vec<u8>, Option<PatternList>>,
    per_dir_override: HashMap<Vec<u8>, Vec<u8>>,
    files: Vec<PatternList>,
    warnings: Vec<String>,
}

impl Excludes {
    /// No sources yet, for the work tree whose top is `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Excludes {
        Excludes {
            root: root.into(),
            icase: false,
            cmdline: PatternList::new(None, Vec::new()),
            per_dir_name: None,
            per_dir: HashMap::new(),
            per_dir_override: HashMap::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// Compare case-insensitively (`core.ignoreCase`).
    pub fn set_ignore_case(&mut self, icase: bool) {
        self.icase = icase;
    }

    /// Adds a command-line pattern (`ls-files -x <pattern>`), relative to
    /// the top of the work tree.
    pub fn add_pattern(&mut self, pattern: &[u8]) {
        let n = self.cmdline.patterns().len() + 1;
        self.cmdline.add_line(pattern, n);
    }

    /// Reads a per-directory file of this name in every directory
    /// (`--exclude-per-directory`, `.gitignore` for the standard rules).
    pub fn set_per_directory(&mut self, name: impl Into<Vec<u8>>) {
        self.per_dir_name = Some(name.into());
        self.per_dir.clear();
    }

    /// Uses `data` as the per-directory file of `dir` (empty or ending in
    /// `/`) instead of reading it from the work tree, for a file that is
    /// marked skip-worktree in the index.
    pub fn set_per_directory_contents(&mut self, dir: impl Into<Vec<u8>>, data: Vec<u8>) {
        let dir = dir.into();
        self.per_dir.remove(&dir);
        self.per_dir_override.insert(dir, data);
    }

    /// Adds already parsed patterns with file precedence (group 3).
    pub fn add_list(&mut self, list: PatternList) {
        self.files.push(list);
    }

    /// Reads the pattern file `path` (following symbolic links) into group
    /// 3; `source` is how `check-ignore -v` names it. Returns whether the
    /// file existed. A missing file is not an error.
    ///
    /// # Errors
    /// If the file exists but cannot be read.
    pub fn add_file(&mut self, path: &Path, source: impl Into<Vec<u8>>) -> io::Result<bool> {
        let data = match std::fs::read(path) {
            Ok(data) => Some(data),
            Err(e) if is_quiet_error(&e) => None,
            Err(e) => return Err(e),
        };
        match data {
            Some(data) => {
                self.files.push(PatternList::from_bytes(
                    &data,
                    Some(source.into()),
                    Vec::new(),
                ));
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// The standard sources of git's porcelain (`--exclude-standard`):
    /// `.gitignore` per directory, then `excludes_file` (the value of
    /// `core.excludesFile`, or its default
    /// [`default_excludes_file`](super::default_excludes_file)) and
    /// `<common_dir>/info/exclude`, which takes precedence over it. Each is
    /// given as (path to read, name to show). Unreadable files produce a
    /// warning.
    pub fn add_standard(
        &mut self,
        excludes_file: Option<(&Path, Vec<u8>)>,
        info_exclude: (&Path, Vec<u8>),
    ) {
        self.set_per_directory(b".gitignore".to_vec());
        if let Some((path, source)) = excludes_file {
            self.add_file_or_warn(path, source);
        }
        self.add_file_or_warn(info_exclude.0, info_exclude.1);
    }

    fn add_file_or_warn(&mut self, path: &Path, source: Vec<u8>) {
        if let Err(e) = self.add_file(path, source) {
            if !is_quiet_error(&e) {
                let shown = path.display().to_string();
                self.warnings
                    .push(messages::unable_to_access(&shown, &os::strerror(&e)));
            }
        }
    }

    /// Warnings collected while reading pattern files (without
    /// `warning: `), emptied by this call.
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }

    /// Reads the per-directory file of `dir` (empty or ending in `/`) if
    /// that has not been done yet.
    fn load_dir(&mut self, dir: &[u8]) {
        let Some(name) = &self.per_dir_name else {
            return;
        };
        if self.per_dir.contains_key(dir) {
            return;
        }
        let mut rel = dir.to_vec();
        rel.extend_from_slice(name);
        let path = os::join(&self.root, &rel);
        let shown = String::from_utf8_lossy(&rel).into_owned();
        let read = match self.per_dir_override.get(dir) {
            Some(data) => Ok(Some(data.clone())),
            None => read_pattern_file(&path, &shown),
        };
        let list = match read {
            Ok(Some(data)) => Some(PatternList::from_bytes(&data, Some(rel), dir.to_vec())),
            Ok(None) => None,
            Err(warning) => {
                self.warnings.extend(warning);
                None
            }
        };
        self.per_dir.insert(dir.to_vec(), list);
    }

    /// Reads the per-directory files of all directories that contain
    /// `path`.
    fn load_dirs_for(&mut self, path: &[u8]) {
        self.load_dir(b"");
        for (i, &b) in path.iter().enumerate() {
            if b == b'/' {
                self.load_dir(&path[..=i]);
            }
        }
    }

    /// The pattern deciding about the work tree path `path` (no trailing
    /// `/`), considering only the path itself, not its leading directories.
    /// Per-directory files of all containing directories are read.
    pub fn last_match(&mut self, path: &[u8], is_dir: bool) -> Option<Match<'_>> {
        self.load_dirs_for(path);
        self.find(path, is_dir)
    }

    /// Whether `path` is excluded by [`Excludes::last_match`].
    pub fn is_excluded(&mut self, path: &[u8], is_dir: bool) -> bool {
        self.last_match(path, is_dir)
            .is_some_and(|m| m.is_excluded())
    }

    /// The rule of `git check-ignore`: each leading directory of `path` is
    /// checked first, top down; the first one that is excluded decides
    /// (files inside an excluded directory cannot be re-included).
    /// Otherwise the pattern deciding about `path` itself is returned, which
    /// may be a negated one. Per-directory files below an excluded directory
    /// are not read. `path` may end in `/`.
    pub fn match_with_leading_dirs(&mut self, path: &[u8], is_dir: bool) -> Option<Match<'_>> {
        self.load_dir(b"");
        let mut hit = None;
        for (i, &b) in path.iter().enumerate() {
            if b != b'/' || i == 0 {
                continue;
            }
            if self.find(&path[..i], true).is_some_and(|m| m.is_excluded()) {
                hit = Some(i);
                break;
            }
            self.load_dir(&path[..=i]);
        }
        match hit {
            Some(i) => self.find(&path[..i], true),
            None => self.find(path, is_dir),
        }
    }

    fn find(&self, path: &[u8], is_dir: bool) -> Option<Match<'_>> {
        let icase = self.icase;
        if let Some(p) = self.cmdline.last_match(path, is_dir, icase) {
            return Some(Match {
                list: &self.cmdline,
                pattern: p,
            });
        }
        let mut end = path.len();
        loop {
            let dir_end = match path[..end].iter().rposition(|&b| b == b'/') {
                Some(i) => i + 1,
                None => 0,
            };
            if let Some(Some(list)) = self.per_dir.get(&path[..dir_end]) {
                if let Some(p) = list.last_match(path, is_dir, icase) {
                    return Some(Match { list, pattern: p });
                }
            }
            if dir_end == 0 {
                break;
            }
            end = dir_end - 1;
        }
        for list in self.files.iter().rev() {
            if let Some(p) = list.last_match(path, is_dir, icase) {
                return Some(Match { list, pattern: p });
            }
        }
        None
    }
}

/// Errors that need no warning: the file is missing or is a directory.
fn is_quiet_error(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory | io::ErrorKind::IsADirectory
    )
}

/// Reads an in-tree pattern file without following a symbolic link at
/// the file itself. `Ok(None)`: missing or a directory. Files above
/// [`MAX_PATTERN_FILE`] and unreadable files are skipped; the error is the
/// warning to print, if any.
fn read_pattern_file(path: &Path, shown: &str) -> Result<Option<Vec<u8>>, Option<String>> {
    let fail = |e: io::Error| -> Option<String> {
        if is_quiet_error(&e) {
            None
        } else {
            Some(messages::unable_to_access(shown, &os::strerror(&e)))
        }
    };
    let mut file = match os::open_read(path, true) {
        Ok(f) => f,
        Err(e) if is_quiet_error(&e) => return Ok(None),
        Err(e) => return Err(fail(e)),
    };
    let meta = file.metadata().map_err(fail)?;
    if meta.is_dir() {
        return Ok(None);
    }
    if meta.len() > MAX_PATTERN_FILE {
        return Err(Some(messages::too_large_pattern_file(shown)));
    }
    let mut data = Vec::with_capacity(meta.len() as usize);
    file.read_to_end(&mut data).map_err(fail)?;
    Ok(Some(data))
}
