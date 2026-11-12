// SPDX-License-Identifier: MIT

//! Minimal repository discovery for the subcommands that need a repository
//! (`config`). It covers what the reference `test-tool` was observed to do:
//! `GIT_DIR`, walking up to a `.git` directory or `.git` file, bare
//! repositories, `GIT_CEILING_DIRECTORIES`, changing to the top of the work
//! tree, and the repository format check.
//!
//! This is a stand-in until rgit's own setup code exists (issue #26); it is
//! not meant to be complete.

use std::path::{Path, PathBuf};

use bstr::{BString, ByteSlice};
use rgit_config::{IntWidth, Origin, Scope, parse_bytes};

use crate::messages;

/// Highest repository format version understood.
const MAX_FORMAT_VERSION: i64 = 1;

/// Valid values of `extensions.objectFormat`.
const HASH_NAMES: &[&str] = &["sha1", "sha256"];

/// Repository extensions understood with format version 1.
const KNOWN_EXTENSIONS: &[&str] = &[
    "noop",
    "noop-v1",
    "preciousobjects",
    "partialclone",
    "worktreeconfig",
    "objectformat",
    "compatobjectformat",
    "refstorage",
    "relativeworktrees",
    "submodulepathconfig",
];

/// A discovered repository.
#[derive(Debug, Clone)]
pub struct Repo {
    /// The git directory as it appears in messages: `.git` after changing
    /// to the top of the work tree, `$GIT_DIR` as given, an absolute path
    /// for `.git` files and bare repositories found further up, and the
    /// empty path for a bare repository that is the current directory.
    pub git_dir: PathBuf,
    /// The git directory, absolute (symbolic links not resolved).
    pub abs_git_dir: PathBuf,
    /// The top of the work tree, absolute; `None` for a bare repository.
    pub worktree: Option<PathBuf>,
}

impl Repo {
    /// The checked-out branch (for `includeIf.onbranch`).
    pub fn current_branch(&self) -> Option<BString> {
        let head = std::fs::read(self.abs_git_dir.join("HEAD")).ok()?;
        let target = head
            .trim_end_with(|c| c.is_ascii_whitespace())
            .strip_prefix(b"ref: ")?;
        target
            .strip_prefix(b"refs/heads/")
            .map(|b| BString::from(b.to_vec()))
    }
}

/// Why no usable repository was found.
#[derive(Debug)]
pub enum SetupError {
    /// No repository up to the root or a ceiling directory.
    NotFound,
    /// No repository up to a file system boundary (the directory on the
    /// other file system is given).
    FilesystemBoundary(PathBuf),
    /// The repository configuration is broken: the lines to print before
    /// exiting with 128, also during early configuration reading.
    Fatal(Vec<String>),
    /// A repository with a format this code does not understand.
    Format {
        /// The git directory as shown in messages.
        git_dir: PathBuf,
        /// The reason, possibly several lines.
        reason: String,
    },
}

fn absolute(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|c| c.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    }
}

/// Whether `dir` looks like a git directory.
fn is_git_dir(dir: &Path) -> bool {
    let head = dir.join("HEAD");
    (head.is_file() || head.is_symlink())
        && dir.join("objects").is_dir()
        && dir.join("refs").is_dir()
}

/// The target of a `.git` file (`gitdir: <path>`), resolved.
fn read_gitfile(file: &Path) -> Option<PathBuf> {
    let data = std::fs::read(file).ok()?;
    let target = data
        .trim_end_with(|c| c.is_ascii_whitespace())
        .strip_prefix(b"gitdir: ")?;
    let target = PathBuf::from(target.to_str().ok()?);
    let dir = file.parent().unwrap_or(Path::new("/"));
    let full = if target.is_absolute() {
        target
    } else {
        dir.join(target)
    };
    full.canonicalize().ok()
}

/// The device a directory is on (`None` if unknown).
fn device(dir: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        dir.metadata().ok().map(|m| m.dev())
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        None
    }
}

/// `GIT_CEILING_DIRECTORIES`: absolute entries, with symbolic links
/// resolved where possible.
fn ceilings() -> Vec<PathBuf> {
    let Some(v) = std::env::var_os("GIT_CEILING_DIRECTORIES") else {
        return Vec::new();
    };
    std::env::split_paths(&v)
        .filter(|p| p.is_absolute())
        .map(|p| p.canonicalize().unwrap_or(p))
        .collect()
}

/// The repository format check. `display` is the git directory as shown
/// in messages.
fn check_format(git_dir: &Path, display: &Path) -> Result<(), SetupError> {
    let Ok(data) = std::fs::read(git_dir.join("config")) else {
        return Ok(());
    };
    let shown_config = if display.as_os_str().is_empty() || display == Path::new(".") {
        PathBuf::from("config")
    } else {
        display.join("config")
    };
    // Syntax errors are reported when the configuration is read.
    let Ok(set) = parse_bytes(&data, Origin::File(shown_config.clone()), Scope::Local) else {
        return Ok(());
    };
    let version = match set.get("core.repositoryformatversion") {
        Some(e) => e
            .int(IntWidth::I32)
            .map_err(|err| SetupError::Fatal(vec![messages::fatal(err)]))?,
        None => 0,
    };
    let fail = |reason: String| {
        Err(SetupError::Format {
            git_dir: display.to_path_buf(),
            reason,
        })
    };
    if version > MAX_FORMAT_VERSION {
        return fail(messages::repo_version(MAX_FORMAT_VERSION, version));
    }
    if version < 1 {
        return Ok(());
    }
    let mut unknown: Vec<String> = Vec::new();
    for e in set.entries() {
        let Some(rest) = e.name.strip_prefix(b"extensions.") else {
            continue;
        };
        let name = rest.to_str_lossy().into_owned();
        if !KNOWN_EXTENSIONS.contains(&name.as_str()) {
            unknown.push(name);
            continue;
        }
        if matches!(name.as_str(), "objectformat" | "compatobjectformat") {
            let value = e.value.clone().unwrap_or_default();
            if !HASH_NAMES.contains(&value.to_str_lossy().as_ref()) {
                return Err(SetupError::Fatal(vec![
                    format!(
                        "error: {}",
                        messages::invalid_value(&e.name.to_str_lossy(), &value.to_str_lossy())
                    ),
                    messages::fatal(messages::bad_config_line(e.line, &shown_config)),
                ]));
            }
        }
    }
    if !unknown.is_empty() {
        return fail(messages::unknown_extensions(&unknown));
    }
    Ok(())
}

/// Finds the repository. With `chdir`, changes to the top of the work tree
/// like git's setup does (and names the git directory relative to it).
pub fn discover(chdir: bool) -> Result<Repo, SetupError> {
    if let Some(dir) = std::env::var_os("GIT_DIR").filter(|v| !v.is_empty()) {
        let git_dir = PathBuf::from(dir);
        let abs_git_dir = absolute(&git_dir);
        check_format(&abs_git_dir, &git_dir)?;
        let worktree = match std::env::var_os("GIT_WORK_TREE") {
            Some(w) => Some(absolute(Path::new(&w))),
            None => std::env::current_dir().ok(),
        };
        return Ok(Repo {
            git_dir,
            abs_git_dir,
            worktree,
        });
    }
    let cwd = std::env::current_dir().map_err(|_| SetupError::NotFound)?;
    let ceilings = ceilings();
    let across_fs = std::env::var_os("GIT_DISCOVERY_ACROSS_FILESYSTEM").is_some_and(|v| {
        rgit_config::value::parse_bool(Some(v.as_encoded_bytes())).unwrap_or(false)
    });
    let mut dir = cwd.clone();
    loop {
        let dotgit = dir.join(".git");
        let found = if dotgit.is_dir() && is_git_dir(&dotgit) {
            let shown = if chdir || dir == cwd {
                PathBuf::from(".git")
            } else {
                dotgit.clone()
            };
            Some((shown, dotgit, Some(dir.clone())))
        } else if dotgit.is_file() {
            read_gitfile(&dotgit).map(|target| (target.clone(), target, Some(dir.clone())))
        } else if is_git_dir(&dir) {
            let shown = if dir == cwd {
                PathBuf::new()
            } else {
                dir.clone()
            };
            Some((shown, dir.clone(), None))
        } else {
            None
        };
        if let Some((git_dir, abs_git_dir, worktree)) = found {
            check_format(
                &abs_git_dir,
                if git_dir.as_os_str().is_empty() {
                    Path::new(".")
                } else {
                    &git_dir
                },
            )?;
            if chdir {
                if let Some(top) = &worktree {
                    if *top != cwd && std::env::set_current_dir(top).is_err() {
                        return Err(SetupError::NotFound);
                    }
                }
            }
            return Ok(Repo {
                git_dir,
                abs_git_dir,
                worktree: worktree.map(|w| w.canonicalize().unwrap_or(w)),
            });
        }
        let Some(parent) = dir.parent() else {
            return Err(SetupError::NotFound);
        };
        if ceilings.iter().any(|c| c == parent) {
            return Err(SetupError::NotFound);
        }
        if !across_fs && device(parent) != device(&cwd) {
            return Err(SetupError::FilesystemBoundary(parent.to_path_buf()));
        }
        dir = parent.to_path_buf();
    }
}
