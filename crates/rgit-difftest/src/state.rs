// SPDX-License-Identifier: MIT
//! Collecting a comparable text snapshot of a repository and its worktree.
//!
//! All inspection commands run with the *reference* git, on both sides, so a
//! difference always means the repositories differ, not that rgit's
//! inspection commands do.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use regex::Regex;

use crate::env::BaseEnv;

/// One aspect that a comparison checks.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Check {
    /// Exit code of the command.
    ExitCode,
    /// Standard output, byte for byte (after normalisers).
    Stdout,
    /// Standard error, byte for byte (after normalisers).
    Stderr,
    /// Refs (`for-each-ref`) and the target of `HEAD`.
    Refs,
    /// Raw reflog files under `logs/`.
    Reflogs,
    /// The set of objects (`cat-file --batch-all-objects --batch-check`).
    Objects,
    /// `fsck --full --strict` result (catches corrupt or unreachable-but-broken objects).
    Fsck,
    /// Index version, entry count and `ls-files --stage --debug` (volatile stat fields masked).
    Index,
    /// Every file, symlink and directory below the fixture's work directory,
    /// outside `.git` directories, with mode and content.
    Worktree,
    /// Selected files in the git directory (`HEAD`, `config`, `FETCH_HEAD`, ...).
    GitFiles,
}

impl Check {
    /// Every check.
    pub const ALL: [Check; 10] = [
        Check::ExitCode,
        Check::Stdout,
        Check::Stderr,
        Check::Refs,
        Check::Reflogs,
        Check::Objects,
        Check::Fsck,
        Check::Index,
        Check::Worktree,
        Check::GitFiles,
    ];

    /// Exit code, stdout and stderr.
    pub const OUTPUT: [Check; 3] = [Check::ExitCode, Check::Stdout, Check::Stderr];

    /// Label used in mismatch reports.
    pub fn label(self) -> &'static str {
        match self {
            Check::ExitCode => "exit code",
            Check::Stdout => "stdout",
            Check::Stderr => "stderr",
            Check::Refs => "refs",
            Check::Reflogs => "reflogs",
            Check::Objects => "objects",
            Check::Fsck => "fsck",
            Check::Index => "index",
            Check::Worktree => "worktree",
            Check::GitFiles => "git files",
        }
    }

    pub(crate) fn is_state(self) -> bool {
        !Check::OUTPUT.contains(&self)
    }
}

/// Files in the git directory compared by [`Check::GitFiles`] by default.
pub const DEFAULT_GIT_FILES: &[&str] = &[
    "HEAD",
    "config",
    "FETCH_HEAD",
    "ORIG_HEAD",
    "MERGE_HEAD",
    "MERGE_MSG",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
];

/// Locates the git directory of the repository at `repo`, without running git.
pub(crate) fn find_git_dir(repo: &Path) -> Option<PathBuf> {
    let dotgit = repo.join(".git");
    if dotgit.is_dir() {
        return Some(dotgit);
    }
    if dotgit.is_file() {
        let content = fs::read_to_string(&dotgit).ok()?;
        let target = content.strip_prefix("gitdir:")?.trim();
        return Some(repo.join(target));
    }
    if repo.join("HEAD").is_file() && repo.join("objects").is_dir() {
        return Some(repo.to_path_buf());
    }
    None
}

pub(crate) struct Inspector<'a> {
    pub reference: &'a Path,
    pub env: &'a BaseEnv,
    pub bin_dir: &'a Path,
}

impl Inspector<'_> {
    fn git(&self, cwd: &Path, args: &[&str]) -> (String, String, Option<i32>) {
        let mut cmd = Command::new(self.reference);
        self.env.apply(&mut cmd, Some(self.bin_dir));
        cmd.env("GIT_OPTIONAL_LOCKS", "0");
        cmd.args(args).current_dir(cwd).stdin(Stdio::null());
        match cmd.output() {
            Ok(out) => (
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
                out.status.code(),
            ),
            Err(e) => (
                String::new(),
                format!("failed to run reference git: {e}\n"),
                None,
            ),
        }
    }

    /// stdout on success, otherwise a description of the failure (still comparable).
    fn git_text(&self, cwd: &Path, args: &[&str]) -> String {
        let (out, err, code) = self.git(cwd, args);
        if code == Some(0) {
            out
        } else {
            format!("`git {}` failed ({code:?})\n{out}{err}", args.join(" "))
        }
    }

    /// Collects the text of `check` for the repository at `repo` within `work`.
    pub fn section(&self, check: Check, work: &Path, repo: &Path, git_files: &[String]) -> String {
        if check == Check::Worktree {
            return worktree(work);
        }
        let Some(git_dir) = find_git_dir(repo) else {
            return "(no repository)\n".to_string();
        };
        match check {
            Check::Refs => {
                let head = fs::read_to_string(git_dir.join("HEAD"))
                    .map(|s| s.trim_end().to_string())
                    .unwrap_or_else(|_| "(missing)".into());
                let refs = self.git_text(
                    repo,
                    &[
                        "for-each-ref",
                        "--format=%(refname) %(objectname)%(if)%(symref)%(then) -> %(symref)%(end)",
                    ],
                );
                format!("HEAD: {head}\n{refs}")
            }
            Check::Reflogs => {
                let mut out = String::new();
                let logs = git_dir.join("logs");
                for (rel, path) in walk_files(&logs) {
                    let content = fs::read(&path).unwrap_or_default();
                    let _ = writeln!(out, "== logs/{rel} ==");
                    out.push_str(&render_content(&content));
                }
                out
            }
            Check::Objects => {
                self.git_text(repo, &["cat-file", "--batch-all-objects", "--batch-check"])
            }
            Check::Fsck => {
                let (out, err, code) = self.git(
                    repo,
                    &[
                        "fsck",
                        "--full",
                        "--strict",
                        "--no-dangling",
                        "--no-progress",
                    ],
                );
                format!("exit {code:?}\n{out}{err}")
            }
            Check::Index => {
                let index = git_dir.join("index");
                let Ok(bytes) = fs::read(&index) else {
                    return "(no index)\n".to_string();
                };
                let header = if bytes.len() >= 12 && &bytes[..4] == b"DIRC" {
                    let version = u32::from_be_bytes(bytes[4..8].try_into().expect("4 bytes"));
                    let count = u32::from_be_bytes(bytes[8..12].try_into().expect("4 bytes"));
                    format!("index version {version}, {count} entries\n")
                } else {
                    "index header invalid\n".to_string()
                };
                let listing = self.git_text(repo, &["ls-files", "--stage", "--debug"]);
                header + &mask_stat(&listing)
            }
            Check::GitFiles => {
                let mut out = String::new();
                for name in git_files {
                    match fs::read(git_dir.join(name)) {
                        Ok(content) => {
                            let _ = writeln!(out, "== {name} ==");
                            out.push_str(&render_content(&content));
                        }
                        Err(_) => {
                            let _ = writeln!(out, "== {name} (missing) ==");
                        }
                    }
                }
                out
            }
            Check::ExitCode | Check::Stdout | Check::Stderr | Check::Worktree => {
                unreachable!("not a repository check")
            }
        }
    }
}

/// Masks the stat fields of `ls-files --debug` that differ between two
/// copies of the same worktree (times, device, inode).
fn mask_stat(listing: &str) -> String {
    let times = Regex::new(r"(?m)^(\s+[cm]time: )\S+$").expect("valid regex");
    let dev = Regex::new(r"(?m)^(\s+dev: )\S+(\s+ino: )\S+$").expect("valid regex");
    let s = times.replace_all(listing, "${1}<TIME>");
    dev.replace_all(&s, "${1}<DEV>${2}<INO>").into_owned()
}

/// Renders file content for a text diff. Non-UTF-8 content is summarised.
fn render_content(content: &[u8]) -> String {
    match std::str::from_utf8(content) {
        Ok(s) if s.is_empty() || s.ends_with('\n') => s.to_string(),
        Ok(s) => format!("{s}\n\\ (no newline at end)\n"),
        Err(_) => format!(
            "<binary, {} bytes, fnv1a {:016x}>\n",
            content.len(),
            fnv1a(content)
        ),
    }
}

fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// All regular files below `root` (recursively), as (relative path, path), sorted.
fn walk_files(root: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(t) if t.is_dir() => stack.push(path),
                Ok(_) => {
                    let rel = rel_path(root, &path);
                    out.push((rel, path));
                }
                Err(_) => {}
            }
        }
    }
    out.sort();
    out
}

fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Text form of the worktree below `work`, skipping `.git` entries.
fn worktree(work: &Path) -> String {
    let mut entries: Vec<(String, String)> = Vec::new();
    let mut stack = vec![work.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            if entry.file_name() == ".git" {
                continue;
            }
            let path = entry.path();
            let rel = rel_path(work, &path);
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            let ft = meta.file_type();
            if ft.is_symlink() {
                let target = fs::read_link(&path)
                    .map(|t| t.to_string_lossy().into_owned())
                    .unwrap_or_default();
                entries.push((rel.clone(), format!("== 120000 {rel} -> {target}\n")));
            } else if ft.is_dir() {
                entries.push((rel.clone(), format!("== 040000 {rel}/\n")));
                stack.push(path);
            } else {
                let content = fs::read(&path).unwrap_or_default();
                let mode = file_mode(&meta);
                entries.push((
                    rel.clone(),
                    format!("== {mode} {rel}\n{}", render_content(&content)),
                ));
            }
        }
    }
    entries.sort();
    entries.into_iter().map(|(_, text)| text).collect()
}

#[cfg(unix)]
fn file_mode(meta: &fs::Metadata) -> &'static str {
    use std::os::unix::fs::PermissionsExt;
    if meta.permissions().mode() & 0o100 != 0 {
        "100755"
    } else {
        "100644"
    }
}

#[cfg(not(unix))]
fn file_mode(_meta: &fs::Metadata) -> &'static str {
    "100644"
}

#[cfg(test)]
mod tests {
    use super::mask_stat;

    #[test]
    fn masks_volatile_stat_fields() {
        let s = "100644 abc 0\ta\n  ctime: 1:2\n  mtime: 3:4\n  dev: 5\tino: 6\n  uid: 7\tgid: 8\n";
        assert_eq!(
            mask_stat(s),
            "100644 abc 0\ta\n  ctime: <TIME>\n  mtime: <TIME>\n  dev: <DEV>\tino: <INO>\n  uid: 7\tgid: 8\n"
        );
    }
}
