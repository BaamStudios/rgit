// SPDX-License-Identifier: MIT

//! The untracked cache (`UNTR` extension, gitformat-index(5) "Untracked
//! cache"): per directory of the work tree the untracked names found the
//! last time it was read, with the directory's stat data and the hash of
//! its per-directory exclude file, so that unchanged directories need not
//! be read again.

use rgit_hash::{HashKind, ObjectId};

use crate::{ewah, varint};

/// The `stat(2)` fields the untracked cache keeps (ctime, mtime, dev, ino,
/// uid, gid, size; 32 bits each).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStat(pub [u32; 9]);

impl CacheStat {
    const LEN: usize = 36;

    fn parse(data: &[u8]) -> Option<CacheStat> {
        let mut s = [0u32; 9];
        for (i, v) in s.iter_mut().enumerate() {
            *v = u32::from_be_bytes(data.get(i * 4..i * 4 + 4)?.try_into().ok()?);
        }
        Some(CacheStat(s))
    }

    fn write(&self, out: &mut Vec<u8>) {
        for v in self.0 {
            out.extend_from_slice(&v.to_be_bytes());
        }
    }
}

/// A global exclude file (`info/exclude`, `core.excludesFile`): its stat
/// data and the hash of its content (null when the file does not exist).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExcludeFile {
    pub stat: CacheStat,
    pub id: ObjectId,
}

/// One directory of the cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheDir {
    /// The directory name (one component; empty for the top).
    pub name: Vec<u8>,
    /// The untracked names found in it (directories with a trailing `/`).
    pub untracked: Vec<Vec<u8>>,
    /// Subdirectories, sorted by name.
    pub dirs: Vec<CacheDir>,
    /// The record can be used (`untracked` and `stat` are current).
    pub valid: bool,
    /// The directory was only read far enough to tell whether it holds
    /// something untracked.
    pub check_only: bool,
    /// Stat data of the directory when it was read (kept for valid ones).
    pub stat: CacheStat,
    /// Hash of the per-directory exclude file, `None` when it is missing.
    pub exclude_id: Option<ObjectId>,
}

impl CacheDir {
    /// A new, invalid directory record.
    pub fn new(name: impl Into<Vec<u8>>) -> CacheDir {
        CacheDir {
            name: name.into(),
            untracked: Vec::new(),
            dirs: Vec::new(),
            valid: false,
            check_only: false,
            stat: CacheStat::default(),
            exclude_id: None,
        }
    }

    /// The subdirectory `name`, created (invalid) when missing.
    pub fn child_mut(&mut self, name: &[u8]) -> &mut CacheDir {
        let i = match self.dirs.binary_search_by(|d| d.name.as_slice().cmp(name)) {
            Ok(i) => i,
            Err(i) => {
                self.dirs.insert(i, CacheDir::new(name));
                i
            }
        };
        &mut self.dirs[i]
    }

    /// The subdirectory `name`.
    pub fn child(&self, name: &[u8]) -> Option<&CacheDir> {
        self.dirs
            .binary_search_by(|d| d.name.as_slice().cmp(name))
            .ok()
            .map(|i| &self.dirs[i])
    }

    /// Forgets what was read: not valid, no untracked names.
    pub fn invalidate(&mut self) {
        self.valid = false;
        self.check_only = false;
        self.untracked.clear();
    }

    /// Invalidates this directory and everything below it.
    pub fn invalidate_all(&mut self) {
        self.invalidate();
        for d in &mut self.dirs {
            d.invalidate_all();
        }
    }

    fn count(&self) -> usize {
        1 + self.dirs.iter().map(CacheDir::count).sum::<usize>()
    }
}

/// The untracked cache of an index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UntrackedCache {
    /// Where the cache can be used (`Location <work tree>, system
    /// <name>`), NUL-terminated strings.
    pub ident: Vec<u8>,
    /// `$GIT_COMMON_DIR/info/exclude`.
    pub info_exclude: ExcludeFile,
    /// `core.excludesFile`.
    pub excludes_file: ExcludeFile,
    /// The directory walk flags the cache was made for.
    pub dir_flags: u32,
    /// The per-directory exclude file name, usually `.gitignore`.
    pub exclude_per_dir: Vec<u8>,
    /// The top directory, once something was cached.
    pub root: Option<CacheDir>,
}

impl UntrackedCache {
    /// An empty cache for `ident` (see [`UntrackedCache::ident`]).
    pub fn new(kind: HashKind, ident: Vec<u8>, dir_flags: u32) -> UntrackedCache {
        let none = ExcludeFile {
            stat: CacheStat::default(),
            id: kind.null(),
        };
        UntrackedCache {
            ident,
            info_exclude: none.clone(),
            excludes_file: none,
            dir_flags,
            exclude_per_dir: b".gitignore".to_vec(),
            root: None,
        }
    }

    /// Invalidates the directories on the way to `path` (the top and every
    /// leading directory that has a record), as done when a path is added
    /// to or removed from the index.
    pub fn invalidate_path(&mut self, path: &[u8]) {
        let Some(mut dir) = self.root.as_mut() else {
            return;
        };
        dir.invalidate();
        let Some(slash) = path.iter().rposition(|&b| b == b'/') else {
            return;
        };
        for comp in path[..slash].split(|&b| b == b'/') {
            let Ok(i) = dir.dirs.binary_search_by(|d| d.name.as_slice().cmp(comp)) else {
                return;
            };
            dir = &mut dir.dirs[i];
            dir.invalidate();
        }
    }

    /// Parses the extension data; `None` if it is damaged (git then drops
    /// the cache).
    pub fn parse(data: &[u8], kind: HashKind) -> Option<UntrackedCache> {
        let hl = kind.byte_len();
        let mut pos = 0;
        let (n, used) = varint::decode(data)?;
        pos += used;
        let ident = data.get(pos..pos + n)?.to_vec();
        pos += n;
        let file = |pos: &mut usize| -> Option<CacheStat> {
            let s = CacheStat::parse(data.get(*pos..)?)?;
            *pos += CacheStat::LEN;
            Some(s)
        };
        let ex_stat = file(&mut pos)?;
        let ef_stat = file(&mut pos)?;
        let dir_flags = u32::from_be_bytes(data.get(pos..pos + 4)?.try_into().ok()?);
        pos += 4;
        let id = |pos: &mut usize| -> Option<ObjectId> {
            let id = ObjectId::from_bytes(kind, data.get(*pos..*pos + hl)?).ok()?;
            *pos += hl;
            Some(id)
        };
        let ex_id = id(&mut pos)?;
        let ef_id = id(&mut pos)?;
        let cstr = |pos: &mut usize| -> Option<Vec<u8>> {
            let nul = data.get(*pos..)?.iter().position(|&b| b == 0)?;
            let s = data[*pos..*pos + nul].to_vec();
            *pos += nul + 1;
            Some(s)
        };
        let exclude_per_dir = cstr(&mut pos)?;
        let mut uc = UntrackedCache {
            ident,
            info_exclude: ExcludeFile {
                stat: ex_stat,
                id: ex_id,
            },
            excludes_file: ExcludeFile {
                stat: ef_stat,
                id: ef_id,
            },
            dir_flags,
            exclude_per_dir,
            root: None,
        };
        let (count, used) = varint::decode(data.get(pos..)?)?;
        pos += used;
        if count == 0 {
            return Some(uc);
        }
        // Directory blocks, depth first.
        fn block(
            data: &[u8],
            pos: &mut usize,
            left: &mut usize,
            cstr: &dyn Fn(&mut usize) -> Option<Vec<u8>>,
        ) -> Option<CacheDir> {
            *left = left.checked_sub(1)?;
            let (n_untracked, used) = varint::decode(data.get(*pos..)?)?;
            *pos += used;
            let (n_dirs, used) = varint::decode(data.get(*pos..)?)?;
            *pos += used;
            let mut dir = CacheDir::new(cstr(pos)?);
            for _ in 0..n_untracked {
                dir.untracked.push(cstr(pos)?);
            }
            for _ in 0..n_dirs {
                dir.dirs.push(block(data, pos, left, cstr)?);
            }
            Some(dir)
        }
        let mut left = count;
        let mut root = block(data, &mut pos, &mut left, &cstr)?;
        if left != 0 {
            return None;
        }
        let bitmap = |pos: &mut usize| -> Option<Vec<usize>> {
            let (set, used) = ewah::decode(data.get(*pos..)?)?;
            *pos += used;
            Some(set)
        };
        let valid = bitmap(&mut pos)?;
        let check_only = bitmap(&mut pos)?;
        let with_id = bitmap(&mut pos)?;
        // Assign the bits in depth-first order.
        let mut order: Vec<Vec<usize>> = Vec::with_capacity(count);
        fn paths(d: &CacheDir, at: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
            out.push(at.clone());
            for (i, c) in d.dirs.iter().enumerate() {
                at.push(i);
                paths(c, at, out);
                at.pop();
            }
        }
        paths(&root, &mut Vec::new(), &mut order);
        let nth = |root: &mut CacheDir, n: usize, f: &mut dyn FnMut(&mut CacheDir)| -> Option<()> {
            let at = order.get(n)?;
            let mut d = root;
            for &i in at {
                d = &mut d.dirs[i];
            }
            f(d);
            Some(())
        };
        for &n in &valid {
            let s = CacheStat::parse(data.get(pos..)?)?;
            pos += CacheStat::LEN;
            nth(&mut root, n, &mut |d| {
                d.valid = true;
                d.stat = s;
            })?;
        }
        for &n in &check_only {
            nth(&mut root, n, &mut |d| d.check_only = true)?;
        }
        for &n in &with_id {
            let i = id(&mut pos)?;
            nth(&mut root, n, &mut |d| d.exclude_id = Some(i))?;
        }
        if data.get(pos) != Some(&0) {
            return None;
        }
        uc.root = Some(root);
        Some(uc)
    }

    /// Serializes the extension data.
    pub fn write(&self, out: &mut Vec<u8>) {
        varint::encode(self.ident.len(), out);
        out.extend_from_slice(&self.ident);
        self.info_exclude.stat.write(out);
        self.excludes_file.stat.write(out);
        out.extend_from_slice(&self.dir_flags.to_be_bytes());
        out.extend_from_slice(self.info_exclude.id.as_bytes());
        out.extend_from_slice(self.excludes_file.id.as_bytes());
        out.extend_from_slice(&self.exclude_per_dir);
        out.push(0);
        let Some(root) = &self.root else {
            varint::encode(0, out);
            return;
        };
        varint::encode(root.count(), out);
        let mut all: Vec<&CacheDir> = Vec::new();
        fn block<'a>(d: &'a CacheDir, out: &mut Vec<u8>, all: &mut Vec<&'a CacheDir>) {
            all.push(d);
            varint::encode(d.untracked.len(), out);
            varint::encode(d.dirs.len(), out);
            out.extend_from_slice(&d.name);
            out.push(0);
            for u in &d.untracked {
                out.extend_from_slice(u);
                out.push(0);
            }
            for c in &d.dirs {
                block(c, out, all);
            }
        }
        block(root, out, &mut all);
        let bits = |f: &dyn Fn(&CacheDir) -> bool| -> Vec<usize> {
            all.iter()
                .enumerate()
                .filter(|(_, d)| f(d))
                .map(|(i, _)| i)
                .collect()
        };
        ewah::encode(&bits(&|d| d.valid), out);
        ewah::encode(&bits(&|d| d.check_only), out);
        ewah::encode(&bits(&|d| d.exclude_id.is_some()), out);
        for d in all.iter().filter(|d| d.valid) {
            d.stat.write(out);
        }
        for d in &all {
            if let Some(id) = &d.exclude_id {
                out.extend_from_slice(id.as_bytes());
            }
        }
        out.push(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_invalidation() {
        let kind = HashKind::Sha1;
        let mut uc = UntrackedCache::new(kind, b"Location /w, system X\0".to_vec(), 6);
        let mut empty = Vec::new();
        uc.write(&mut empty);
        assert_eq!(UntrackedCache::parse(&empty, kind), Some(uc.clone()));

        let mut root = CacheDir::new(Vec::new());
        root.valid = true;
        root.stat = CacheStat([1, 2, 3, 4, 5, 6, 7, 8, 9]);
        root.untracked = vec![b"x/".to_vec(), b"f".to_vec()];
        root.exclude_id = Some(ObjectId::empty_blob(kind));
        let a = root.child_mut(b"a");
        a.valid = true;
        a.child_mut(b"b").check_only = true;
        root.child_mut(b"x").untracked.push(b"y".to_vec());
        uc.root = Some(root);
        let mut data = Vec::new();
        uc.write(&mut data);
        let back = UntrackedCache::parse(&data, kind).unwrap();
        assert_eq!(back, uc);

        // A path below a/b invalidates the top, a and a/b, not x.
        let mut uc = back;
        uc.invalidate_path(b"a/b/c/file");
        let root = uc.root.as_ref().unwrap();
        assert!(!root.valid && root.untracked.is_empty());
        assert!(!root.child(b"a").unwrap().valid);
        assert!(!root.child(b"a").unwrap().child(b"b").unwrap().check_only);
        assert_eq!(root.child(b"x").unwrap().untracked, [b"y".to_vec()]);
    }
}
