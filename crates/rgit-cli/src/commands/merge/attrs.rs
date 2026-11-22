// SPDX-License-Identifier: MIT
//! Merge attributes and drivers of a repository for `rgit-merge`
//! (gitattributes(5) "Performing a three-way merge", config/merge.adoc):
//! the `merge` attribute (`text`, `binary`, `union`, a custom driver, or
//! unset), `merge.default`, `conflict-marker-size`, custom drivers
//! (`merge.<driver>.driver` with `%O %A %B %L %P %S %X %Y`, run by the
//! shell in the top of the work tree on temporary files) and their
//! `merge.<driver>.recursive` driver for inner merges, and the
//! renormalization of `merge.renormalize`.
//!
//! Attributes come from the work tree's `.gitattributes`, with the
//! index's copies as the fall-back.

use std::cell::RefCell;
use std::io::Write as _;
use std::path::PathBuf;
use std::rc::Rc;

use rgit_attr::{AttrId, Attributes, Order, Source, State};
use rgit_config::ConfigSet;
use rgit_hash::ObjectId;
use rgit_merge::{Driver, DriverInput, PathMerge};

use crate::convert::Converter;
use crate::messages::merge as msg;
use crate::repository::Repo;

struct Lookup {
    attrs: Attributes<'static>,
    merge: AttrId,
    marker: AttrId,
}

/// The merge attributes and drivers of a repository.
pub(crate) struct RepoPathMerge {
    lookup: RefCell<Lookup>,
    config: ConfigSet,
    root: PathBuf,
    env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    converter: Option<Converter>,
    hash: rgit_hash::HashKind,
    /// The object databases of checked-out submodules by path.
    submodules: RefCell<std::collections::HashMap<Vec<u8>, Option<Rc<rgit_odb::ObjectDatabase>>>>,
    /// Paths whose invalid marker size was reported.
    warned: RefCell<std::collections::HashSet<Vec<u8>>>,
}

impl std::fmt::Debug for RepoPathMerge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RepoPathMerge")
    }
}

impl RepoPathMerge {
    /// The merge attributes of `repo`; `renormalize` prepares the
    /// conversion of `merge.renormalize`, which also takes the
    /// `.gitattributes` files of `theirs` (a tree) where the index has
    /// none, so that attributes the other side adds apply.
    pub(crate) fn open(
        ctx: &crate::Context,
        repo: &Repo,
        renormalize: bool,
        theirs: Option<&ObjectId>,
    ) -> Result<Rc<dyn PathMerge>, crate::Die> {
        let root = match &repo.setup.work_tree {
            Some(w) => repo.setup.cwd.join(w),
            None => repo.setup.cwd.join(&repo.setup.git_dir),
        };
        let (mut index, _) = repo.read_index()?;
        if renormalize {
            if let Some(tree) = theirs {
                add_attribute_files(repo, &mut index, tree);
            }
        }
        let snapshot = rgit_attr::snapshot(&index, repo.odb());
        let source = Source::Snapshot {
            root: repo.setup.work_tree.as_ref().map(|w| repo.setup.cwd.join(w)),
            index: snapshot,
            order: Order::WorkTreeFirst,
        };
        let mut attrs = Attributes::new(&crate::attributes::options(repo), source);
        let merge = attrs.id(b"merge");
        let marker = attrs.id(b"conflict-marker-size");
        let converter = if renormalize {
            let c = Converter::checkout(repo, &index)?;
            c.set_renormalize();
            Some(c)
        } else {
            None
        };
        Ok(Rc::new(RepoPathMerge {
            lookup: RefCell::new(Lookup {
                attrs,
                merge,
                marker,
            }),
            config: repo.config.clone(),
            root,
            env: ctx.child_env(),
            converter,
            hash: repo.hash,
            submodules: RefCell::new(std::collections::HashMap::new()),
            warned: RefCell::new(std::collections::HashSet::new()),
        }))
    }

    fn config_value(&self, key: &str) -> Option<Vec<u8>> {
        self.config
            .get(key)
            .and_then(|e| e.value.as_ref())
            .map(|v| v.to_vec())
    }

    /// The driver called `name`: a built-in one or a configured custom
    /// one (an unknown name merges as text).
    fn named(&self, name: &[u8], inner: bool) -> Driver {
        match name {
            b"text" => return Driver::Text,
            b"binary" => return Driver::Binary,
            b"union" => return Driver::Union,
            _ => {}
        }
        let n = String::from_utf8_lossy(name);
        if inner {
            if let Some(r) = self.config_value(&format!("merge.{n}.recursive")) {
                if r != name {
                    return self.named(&r, false);
                }
            }
        }
        if self.config_value(&format!("merge.{n}.driver")).is_some() {
            Driver::Custom(name.to_vec())
        } else {
            Driver::Text
        }
    }
}

/// Adds the `.gitattributes` files of `tree` that `index` lacks.
fn add_attribute_files(repo: &Repo, index: &mut rgit_index::Index, tree: &ObjectId) {
    let Ok(entries) = rgit_unpack::tree_entries(repo.odb(), tree) else {
        return;
    };
    let add = rgit_index::AddOptions {
        ok_to_replace: false,
        skip_df_check: true,
        skip_path_check: true,
    };
    for e in entries {
        let p = e.path();
        let name = p.rsplit(|&b| b == b'/').next().unwrap_or(p);
        if name == b".gitattributes" && index.get(p, rgit_index::Stage::MERGED).is_none() {
            let _ = index.add(e, add);
        }
    }
}

/// `s` quoted for the shell.
fn sq(s: &[u8]) -> String {
    crate::context::sq_quote(&String::from_utf8_lossy(s))
}

fn seed() -> u64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1);
    (t ^ (u64::from(std::process::id()) << 32)) | 1
}

/// A new temporary file `.merge_file_XXXXXX` in `dir` holding `data`.
fn temp_file(dir: &std::path::Path, data: &[u8], seed: &mut u64) -> std::io::Result<String> {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    loop {
        let mut name = String::from(".merge_file_");
        for _ in 0..6 {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 7;
            *seed ^= *seed << 17;
            name.push(CHARS[(*seed % CHARS.len() as u64) as usize] as char);
        }
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join(&name))
        {
            Ok(mut f) => {
                f.write_all(data)?;
                return Ok(name);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

impl RepoPathMerge {
    /// The object database of the submodule checked out at `path`.
    fn submodule_odb(&self, path: &[u8]) -> Option<Rc<rgit_odb::ObjectDatabase>> {
        if let Some(o) = self.submodules.borrow().get(path) {
            return o.clone();
        }
        let dir = self.root.join(String::from_utf8_lossy(path).as_ref());
        let odb = rgit_submodule::gitdir::git_dir_of(&dir).and_then(|g| {
            let objects = g.join("objects");
            objects.is_dir().then(|| {
                Rc::new(rgit_odb::ObjectDatabase::open_with_packs(
                    objects,
                    self.hash,
                    rgit_odb::LooseOptions::default(),
                ))
            })
        });
        self.submodules
            .borrow_mut()
            .insert(path.to_vec(), odb.clone());
        odb
    }
}

impl PathMerge for RepoPathMerge {
    fn driver(&self, path: &[u8], inner: bool) -> (Driver, Option<usize>) {
        let (merge, marker) = {
            let mut l = self.lookup.borrow_mut();
            let out = l.attrs.lookup(path, false);
            let r = (out.get(l.merge).clone(), out.get(l.marker).clone());
            for w in l.attrs.take_warnings() {
                eprintln!("{w}");
            }
            r
        };
        let driver = match merge {
            State::Set => Driver::Text,
            State::Unset => Driver::Binary,
            State::Value(v) => self.named(&v, inner),
            State::Unspecified => match self.config_value("merge.default") {
                Some(v) => self.named(&v, inner),
                None => Driver::Text,
            },
        };
        let size = match marker {
            State::Value(v) => {
                let s = String::from_utf8_lossy(&v).into_owned();
                match s.parse::<i64>() {
                    Ok(n) if n > 0 => usize::try_from(n).ok(),
                    Ok(_) => None,
                    Err(_) => {
                        if self.warned.borrow_mut().insert(path.to_vec()) {
                            eprintln!("warning: {}", msg::invalid_marker_size(&s));
                        }
                        None
                    }
                }
            }
            _ => None,
        };
        (driver, size)
    }

    fn run(&self, name: &[u8], input: &DriverInput<'_>) -> Result<(Vec<u8>, bool), String> {
        let n = String::from_utf8_lossy(name).into_owned();
        let cmd = self
            .config_value(&format!("merge.{n}.driver"))
            .unwrap_or_default();
        let fail = || msg::FAILED_INTERNAL_MERGE.to_owned();
        let mut seed = seed();
        let o = temp_file(&self.root, input.base, &mut seed).map_err(|_| fail())?;
        let a = temp_file(&self.root, input.ours, &mut seed).map_err(|_| fail())?;
        let b = temp_file(&self.root, input.theirs, &mut seed).map_err(|_| fail())?;
        let mut line = String::new();
        let cmd = String::from_utf8_lossy(&cmd).into_owned();
        let mut chars = cmd.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                line.push(c);
                continue;
            }
            match chars.next() {
                Some('O') => line.push_str(&o),
                Some('A') => line.push_str(&a),
                Some('B') => line.push_str(&b),
                Some('L') => line.push_str(&input.marker_size.to_string()),
                Some('P') => line.push_str(&sq(input.path)),
                Some('S') => line.push_str(&sq(input.base_label)),
                Some('X') => line.push_str(&sq(input.ours_label)),
                Some('Y') => line.push_str(&sq(input.theirs_label)),
                Some('%') => line.push('%'),
                Some(other) => {
                    line.push('%');
                    line.push(other);
                }
                None => line.push('%'),
            }
        }
        let _ = std::io::stdout().flush();
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(&line)
            .current_dir(&self.root)
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stdin(std::process::Stdio::null())
            .status();
        let result = std::fs::read(self.root.join(&a));
        for f in [&o, &a, &b] {
            let _ = std::fs::remove_file(self.root.join(f));
        }
        let status = status.map_err(|_| fail())?;
        let code = match status.code() {
            Some(c) if c < 128 => c,
            _ => return Err(fail()),
        };
        let data = result.map_err(|_| fail())?;
        Ok((data, code == 0))
    }

    fn submodule_is_ancestor(
        &self,
        path: &[u8],
        ancestor: &ObjectId,
        descendant: &ObjectId,
    ) -> Option<bool> {
        let odb = self.submodule_odb(path)?;
        let has = |id: &ObjectId| matches!(odb.read(id), Ok(Some(_)));
        if !has(ancestor) || !has(descendant) {
            return None;
        }
        if ancestor == descendant {
            return Some(true);
        }
        let commits = rgit_merge::ObjectCommits {
            objects: &*odb,
            shallow: std::collections::HashSet::new(),
            label: Box::new(|id: &ObjectId| id.to_hex().into_bytes()),
        };
        let bases = rgit_merge::merge_bases(&commits, ancestor, descendant).ok()?;
        Some(bases.contains(ancestor))
    }

    fn renormalize(&self, path: &[u8], data: Vec<u8>) -> Vec<u8> {
        let Some(c) = &self.converter else {
            return data;
        };
        let id = ObjectId::null(self.hash);
        let wt = c.to_worktree(path, &id, data);
        c.to_git(path, wt.clone(), rgit_convert::Options::QUIET)
            .unwrap_or(wt)
    }
}
