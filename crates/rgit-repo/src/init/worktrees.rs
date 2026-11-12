// SPDX-License-Identifier: MIT
//! Keeping linked work trees connected when `init --separate-git-dir`
//! moves a repository (`gitrepository-layout(5)`, `worktrees/<id>/gitdir`;
//! `git-worktree(1)`, `worktree.useRelativePaths`).

use std::fs;
use std::path::{Component, Path, PathBuf};

use super::InitError;
use crate::error::io_text;
use crate::layout::bytes_to_path;
use crate::{messages, path};

/// After the common directory moved from `old` to `new`, point every
/// linked work tree's `.git` file at `new/worktrees/<id>` and update the
/// `gitdir` files. `relative` writes relative paths.
pub(super) fn repair_after_move(old: &Path, new: &Path, relative: bool) -> Result<(), InitError> {
    let Ok(entries) = fs::read_dir(new.join("worktrees")) else {
        return Ok(());
    };
    for entry in entries.filter_map(Result::ok) {
        let admin = entry.path();
        if !admin.is_dir() {
            continue;
        }
        let Ok(data) = fs::read(admin.join("gitdir")) else {
            continue;
        };
        let text = trim(&data);
        if text.is_empty() {
            continue;
        }
        // A relative entry was relative to the old location.
        let old_admin = old.join("worktrees").join(entry.file_name());
        let dot_git = path::normalize(&path::absolute(&bytes_to_path(text), &old_admin));
        let Some(wt) = dot_git.parent() else {
            continue;
        };
        let wt = path::real_path_lossy(wt, Path::new("/"));
        let dot_git = wt.join(dot_git.file_name().unwrap_or_default());
        let admin = path::real_path_lossy(&admin, Path::new("/"));
        let (link, back) = if relative {
            (relative_path(&admin, &wt), relative_path(&dot_git, &admin))
        } else {
            (admin.clone(), dot_git.clone())
        };
        write(&dot_git, format!("gitdir: {}\n", link.display()))?;
        write(&admin.join("gitdir"), format!("{}\n", back.display()))?;
    }
    Ok(())
}

fn write(path: &Path, text: String) -> Result<(), InitError> {
    fs::write(path, text)
        .map_err(|e| InitError::Fatal(messages::cannot_write(path.display(), io_text(&e))))
}

fn trim(b: &[u8]) -> &[u8] {
    let end = b
        .iter()
        .rposition(|c| !c.is_ascii_whitespace())
        .map_or(0, |i| i + 1);
    &b[..end]
}

/// `target` relative to the directory `base` (both absolute, normalised).
pub(super) fn relative_path(target: &Path, base: &Path) -> PathBuf {
    let t: Vec<Component> = target.components().collect();
    let b: Vec<Component> = base.components().collect();
    let common = t.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut out = PathBuf::new();
    for _ in common..b.len() {
        out.push("..");
    }
    for c in &t[common..] {
        out.push(c);
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        assert_eq!(
            relative_path(Path::new("/w/sep/worktrees/l"), Path::new("/w/l")),
            Path::new("../sep/worktrees/l")
        );
        assert_eq!(
            relative_path(Path::new("/w/l/.git"), Path::new("/w/sep/worktrees/l")),
            Path::new("../../../l/.git")
        );
        assert_eq!(
            relative_path(Path::new("/a"), Path::new("/a")),
            Path::new(".")
        );
    }
}
