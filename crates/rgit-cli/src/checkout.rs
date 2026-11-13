// SPDX-License-Identifier: MIT
//! Initial checkout into an empty work tree (used by `clone`): every file
//! of a commit's tree is written, and the index gets one entry per file
//! with its stat data plus a complete cache tree.
//!
//! This is the simple case of a one-way tree merge into an empty index; the
//! general unpack-trees engine is #107. Line-ending and filter attributes
//! are not applied: a warning names the first path they could affect
//! (decision on #107).

use std::io::Write;
use std::path::{Path, PathBuf};

use rgit_config::ConfigSet;
use rgit_hash::ObjectId;
use rgit_index::{
    AddOptions, CacheTree, Entry, Index, Mode as IndexMode, Stage, TreeNode, WriteOptions,
};
use rgit_object::{ObjectKind, Tree};
use rgit_repo::Setup;
use rgit_worktree::stat::stat_from_metadata;

use crate::Die;
use crate::refs_support::Objects;

fn os_path(root: &Path, rel: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        root.join(std::ffi::OsStr::from_bytes(rel))
    }
    #[cfg(not(unix))]
    {
        root.join(String::from_utf8_lossy(rel).as_ref())
    }
}

struct Writer<'a> {
    objects: &'a Objects,
    root: &'a Path,
    symlinks: bool,
    entries: Vec<Entry>,
    attributes_warned: bool,
    autocrlf: bool,
}

impl Writer<'_> {
    fn read(&self, id: &ObjectId, kind: ObjectKind) -> Result<Vec<u8>, Die> {
        match self.objects.read(id) {
            Some((k, data)) if k == kind => Ok(data),
            _ => Err(Die::Fatal(format!(
                "unable to read {} {id}",
                match kind {
                    ObjectKind::Tree => "tree",
                    _ => "sha1 file of",
                }
            ))),
        }
    }

    /// Writes the tree `id` below `base` and returns its cache-tree node.
    fn tree(&mut self, id: &ObjectId, name: &[u8], base: &[u8]) -> Result<TreeNode, Die> {
        let data = self.read(id, ObjectKind::Tree)?;
        let tree = Tree::parse(self.objects.hash(), &data)
            .map_err(|_| Die::Fatal(format!("unable to read tree {id}")))?;
        let start = self.entries.len();
        let mut children = Vec::new();
        for e in &tree.entries {
            let mut path = base.to_vec();
            path.extend_from_slice(&e.name);
            if !rgit_index::verify_path(&path) {
                return Err(Die::Fatal(rgit_index::messages::invalid_path(
                    &String::from_utf8_lossy(&path),
                )));
            }
            let mode = e.mode.canonical();
            if mode.is_tree() {
                std::fs::create_dir(os_path(self.root, &path))
                    .or_else(|err| match err.kind() {
                        std::io::ErrorKind::AlreadyExists => Ok(()),
                        _ => Err(err),
                    })
                    .map_err(|err| {
                        Die::Fatal(format!(
                            "cannot create directory at '{}': {err}",
                            String::from_utf8_lossy(&path)
                        ))
                    })?;
                path.push(b'/');
                children.push(self.tree(&e.oid, &e.name, &path)?);
            } else {
                self.file(&path, mode.bits(), &e.oid)?;
            }
        }
        let count = (self.entries.len() - start) as u32;
        let mut node = TreeNode::new_valid(name.to_vec(), count, *id);
        children.sort_by(|a, b| a.name.len().cmp(&b.name.len()).then(a.name.cmp(&b.name)));
        node.children = children;
        Ok(node)
    }

    fn file(&mut self, path: &[u8], mode: u32, id: &ObjectId) -> Result<(), Die> {
        let full = os_path(self.root, path);
        let shown = String::from_utf8_lossy(path).into_owned();
        let fail = |e: std::io::Error| Die::Fatal(format!("unable to create file {shown}: {e}"));
        match mode & 0o170000 {
            0o160000 => {
                // A submodule: an empty directory.
                std::fs::create_dir(&full)
                    .or_else(|e| match e.kind() {
                        std::io::ErrorKind::AlreadyExists => Ok(()),
                        _ => Err(e),
                    })
                    .map_err(fail)?;
            }
            0o120000 => {
                let target = self.read(id, ObjectKind::Blob)?;
                if self.symlinks {
                    #[cfg(unix)]
                    {
                        use std::os::unix::ffi::OsStrExt;
                        std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(&target), &full)
                            .map_err(fail)?;
                    }
                    #[cfg(not(unix))]
                    write_regular(&full, &target, false).map_err(fail)?;
                } else {
                    write_regular(&full, &target, false).map_err(fail)?;
                }
            }
            _ => {
                let data = self.read(id, ObjectKind::Blob)?;
                if !self.attributes_warned
                    && (self.autocrlf
                        || (path.ends_with(b".gitattributes") && mentions_conversion(&data)))
                {
                    self.attributes_warned = true;
                    eprintln!(
                        "warning: rgit: eol/filter attributes are not applied yet; '{shown}' checked out unconverted"
                    );
                }
                write_regular(&full, &data, mode & 0o111 != 0).map_err(fail)?;
            }
        }
        let meta = std::fs::symlink_metadata(&full)
            .map_err(|e| Die::Fatal(format!("unable to stat just-written file {shown}: {e}")))?;
        let mut entry = Entry::new(path.to_vec(), Stage::MERGED, IndexMode(mode), *id);
        if mode & 0o170000 != 0o160000 {
            entry.stat = stat_from_metadata(&meta);
        }
        self.entries.push(entry);
        Ok(())
    }
}

/// Whether a `.gitattributes` file sets attributes that convert content.
fn mentions_conversion(data: &[u8]) -> bool {
    let text = String::from_utf8_lossy(data);
    text.lines().any(|l| {
        let l = l.trim();
        !l.starts_with('#')
            && l.split_whitespace().skip(1).any(|a| {
                let a = a.trim_start_matches(['-', '!']);
                [
                    "text",
                    "eol",
                    "crlf",
                    "filter",
                    "ident",
                    "working-tree-encoding",
                ]
                .iter()
                .any(|p| a.starts_with(p))
            })
    })
}

fn write_regular(path: &Path, data: &[u8], executable: bool) -> std::io::Result<()> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(if executable { 0o777 } else { 0o666 });
    }
    #[cfg(not(unix))]
    let _ = executable;
    let mut f = o.open(path)?;
    f.write_all(data)
}

/// Checks out `commit` into the empty work tree `root` and writes the
/// index of the repository of `setup`.
pub fn checkout_commit(
    setup: &Setup,
    config: &ConfigSet,
    objects: &Objects,
    root: &Path,
    commit: &ObjectId,
) -> Result<(), Die> {
    let data = match objects.read(commit) {
        Some((ObjectKind::Commit, d)) => d,
        _ => return Err(Die::Fatal(format!("unable to read tree {commit}"))),
    };
    let tree = rgit_object::Commit::parse(objects.hash(), &data)
        .and_then(|c| c.tree())
        .map_err(|_| Die::Fatal(format!("unable to read tree {commit}")))?;
    let bool_of = |k: &str, d: bool| config.bool(k).ok().flatten().unwrap_or(d);
    let autocrlf = config
        .string("core.autocrlf")
        .ok()
        .flatten()
        .is_some_and(|v| {
            let v = String::from_utf8_lossy(v).to_ascii_lowercase();
            ["true", "input", "yes", "on", "1"].contains(&v.as_str())
        });
    let mut w = Writer {
        objects,
        root,
        symlinks: bool_of("core.symlinks", true),
        entries: Vec::new(),
        attributes_warned: false,
        autocrlf,
    };
    let node = w.tree(&tree, b"", b"")?;
    let mut index = Index::new(objects.hash());
    for e in w.entries {
        index
            .add(e, AddOptions::default())
            .map_err(|err| Die::Fatal(err.to_string()))?;
    }
    index.set_cache_tree(Some(CacheTree { root: node }));
    let path = rgit_index::index_path(&setup.absolute_git_dir());
    let skip_hash = bool_of("index.skiphash", false);
    index
        .write_file(
            &path,
            &rgit_fs::LockOptions::default(),
            &WriteOptions { skip_hash },
        )
        .map_err(|e| Die::Fatal(e.to_string()))
}
