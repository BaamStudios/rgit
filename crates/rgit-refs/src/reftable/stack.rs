// SPDX-License-Identifier: MIT
//! The stack of tables in a `reftable/` directory: `tables.list` names the
//! tables from oldest to newest. Writers take `tables.list.lock`, add a
//! table and replace the list; compaction merges neighbouring tables into
//! one while holding a `<table>.lock` for each of them.

use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use rgit_fs::{FsyncMethod, LockError, LockFile, LockOptions, SharedRepository, TempFile, Timeout};
use rgit_hash::HashKind;

use super::FormatError;
use super::merged;
use super::table::Table;
use super::writer::{
    DEFAULT_BLOCK_SIZE, DEFAULT_RESTART_INTERVAL, WriteError, WriteOptions, Writer,
};

/// The `reftable.*` settings (`Documentation/config/reftable.adoc`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReftableOptions {
    /// `reftable.blockSize`.
    pub block_size: u32,
    /// `reftable.restartInterval`.
    pub restart_interval: u16,
    /// `reftable.indexObjects`.
    pub index_objects: bool,
    /// `reftable.geometricFactor`.
    pub geometric_factor: u8,
    /// `reftable.lockTimeout`.
    pub lock_timeout: Timeout,
    /// Compact after every write (switched off by
    /// `GIT_TEST_REFTABLE_AUTOCOMPACTION=false`).
    pub auto_compact: bool,
}

impl Default for ReftableOptions {
    fn default() -> Self {
        ReftableOptions {
            block_size: DEFAULT_BLOCK_SIZE,
            restart_interval: DEFAULT_RESTART_INTERVAL,
            index_objects: true,
            geometric_factor: 2,
            lock_timeout: Timeout::After(std::time::Duration::from_millis(100)),
            auto_compact: true,
        }
    }
}

fn global_options() -> &'static RwLock<ReftableOptions> {
    static OPTIONS: OnceLock<RwLock<ReftableOptions>> = OnceLock::new();
    OPTIONS.get_or_init(|| {
        let mut o = ReftableOptions::default();
        // The switch of git's test suite; anything but a false value
        // leaves compaction on.
        if let Some(v) = std::env::var_os("GIT_TEST_REFTABLE_AUTOCOMPACTION") {
            let v = v.to_string_lossy().to_ascii_lowercase();
            if matches!(v.as_str(), "false" | "no" | "off" | "0") {
                o.auto_compact = false;
            }
        }
        RwLock::new(o)
    })
}

/// The settings used by every stack of this process.
pub fn options() -> ReftableOptions {
    global_options().read().map(|o| *o).unwrap_or_default()
}

/// Replaces the settings used by every stack of this process (the caller
/// reads them from the configuration).
pub fn set_options(options: ReftableOptions) {
    if let Ok(mut o) = global_options().write() {
        *o = options;
    }
}

/// Errors of stack operations.
#[derive(Debug, thiserror::Error)]
pub enum StackError {
    /// `tables.list.lock` or a table lock is held by someone else.
    #[error("data is locked")]
    Locked,
    /// A file could not be read or written.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// The error.
        #[source]
        source: io::Error,
    },
    /// A table is corrupt.
    #[error("{}: {source}", path.display())]
    Format {
        /// The table file.
        path: PathBuf,
        /// What is wrong.
        #[source]
        source: FormatError,
    },
    /// A table listed in `tables.list` has another object format than the
    /// repository.
    #[error("{}: object format mismatch", .0.display())]
    HashMismatch(PathBuf),
    /// Writing a table failed.
    #[error("{0}")]
    Write(#[from] WriteError),
}

impl From<FormatError> for StackError {
    fn from(source: FormatError) -> Self {
        StackError::Format {
            path: PathBuf::new(),
            source,
        }
    }
}

/// What identifies one version of `tables.list` on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    ino: u64,
}

impl Stamp {
    fn of(meta: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt as _;
        Stamp {
            len: meta.len(),
            modified: meta.modified().ok(),
            #[cfg(unix)]
            ino: meta.ino(),
        }
    }
}

#[derive(Debug, Default)]
struct Cache {
    stamp: Option<Stamp>,
    tables: Vec<Arc<Table>>,
}

/// The tables of one `reftable/` directory.
#[derive(Debug)]
pub struct Stack {
    dir: PathBuf,
    kind: HashKind,
    cache: Mutex<Cache>,
}

/// The stacks opened by this process, by directory and object format.
type Registry = Mutex<HashMap<(PathBuf, HashKind), Arc<Stack>>>;

fn registry() -> &'static Registry {
    static STACKS: OnceLock<Registry> = OnceLock::new();
    STACKS.get_or_init(Mutex::default)
}

/// A random table name suffix.
fn random_suffix() -> u32 {
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u32(std::process::id());
    if let Ok(d) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        h.write_u128(d.as_nanos());
    }
    h.finish() as u32
}

/// The file name of a table covering the update indices `min..=max`.
fn table_name(min: u64, max: u64) -> String {
    format!("0x{min:012x}-0x{max:012x}-{:08x}.ref", random_suffix())
}

impl Stack {
    /// The stack in `dir` (a `reftable/` directory). Stacks are shared by
    /// everything in the process that opens the same directory.
    pub fn open(dir: impl Into<PathBuf>, kind: HashKind) -> Arc<Stack> {
        let dir = dir.into();
        let key = (dir.clone(), kind);
        let mut reg = match registry().lock() {
            Ok(r) => r,
            Err(e) => e.into_inner(),
        };
        Arc::clone(reg.entry(key).or_insert_with(|| {
            Arc::new(Stack {
                dir,
                kind,
                cache: Mutex::default(),
            })
        }))
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The object format.
    pub fn hash_kind(&self) -> HashKind {
        self.kind
    }

    fn list_path(&self) -> PathBuf {
        self.dir.join("tables.list")
    }

    fn io(&self, path: &Path, source: io::Error) -> StackError {
        StackError::Io {
            path: path.to_owned(),
            source,
        }
    }

    /// Reads `tables.list`: the names, and the stamp of the file.
    fn read_list(&self) -> Result<(Vec<String>, Option<Stamp>), StackError> {
        let path = self.list_path();
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((Vec::new(), None)),
            Err(e) => return Err(self.io(&path, e)),
        };
        let stamp = std::fs::metadata(&path).ok().map(|m| Stamp::of(&m));
        let text = String::from_utf8_lossy(&data);
        let names = text
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect();
        Ok((names, stamp))
    }

    /// Loads the tables `names`, reusing those already in `have`.
    fn load(&self, names: &[String], have: &[Arc<Table>]) -> Result<Vec<Arc<Table>>, StackError> {
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            if let Some(t) = have.iter().find(|t| t.name() == name) {
                out.push(Arc::clone(t));
                continue;
            }
            let path = self.dir.join(name);
            let data = std::fs::read(&path).map_err(|e| self.io(&path, e))?;
            let table = Table::parse(name.clone(), data).map_err(|source| StackError::Format {
                path: path.clone(),
                source,
            })?;
            if table.hash_kind() != self.kind {
                return Err(StackError::HashMismatch(path));
            }
            out.push(Arc::new(table));
        }
        Ok(out)
    }

    /// The current tables, oldest first.
    pub fn tables(&self) -> Result<Vec<Arc<Table>>, StackError> {
        let path = self.list_path();
        let stamp = std::fs::metadata(&path).ok().map(|m| Stamp::of(&m));
        let cached = {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            if stamp.is_some() && cache.stamp == stamp {
                return Ok(cache.tables.clone());
            }
            cache.tables.clone()
        };
        self.reload(&cached)
    }

    /// Reads the list and the tables it names. A table that vanished
    /// between reading the list and opening it was compacted away: the
    /// list is read again.
    fn reload(&self, have: &[Arc<Table>]) -> Result<Vec<Arc<Table>>, StackError> {
        let mut attempts = 0;
        loop {
            let (names, stamp) = self.read_list()?;
            match self.load(&names, have) {
                Ok(tables) => {
                    let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
                    cache.stamp = stamp;
                    cache.tables = tables.clone();
                    return Ok(tables);
                }
                Err(StackError::Io { source, .. })
                    if source.kind() == io::ErrorKind::NotFound && attempts < 50 =>
                {
                    let (again, _) = self.read_list()?;
                    if again == names {
                        // The list still names the missing table.
                        let path = self.list_path();
                        return Err(self.io(&path, source));
                    }
                    attempts += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Takes `tables.list.lock` and reads the stack as it is then.
    pub fn lock(self: &Arc<Self>, shared: SharedRepository) -> Result<LockedStack, StackError> {
        let opts = options();
        if let Err(e) = std::fs::create_dir_all(&self.dir) {
            return Err(self.io(&self.dir, e));
        }
        let lock_opts = LockOptions::new().timeout(opts.lock_timeout).shared(shared);
        let lock = match LockFile::acquire(self.list_path(), &lock_opts) {
            Ok(l) => l,
            Err(LockError::Exists { .. }) => return Err(StackError::Locked),
            Err(LockError::Io { lock_path, source }) => {
                return Err(StackError::Io {
                    path: lock_path,
                    source,
                });
            }
        };
        let have = self
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tables
            .clone();
        let tables = self.reload(&have)?;
        Ok(LockedStack {
            stack: Arc::clone(self),
            lock: Some(lock),
            tables,
            shared,
            fsync: None,
            opts,
        })
    }

    /// Compacts as the geometric heuristic asks for. Tables locked by
    /// another process are left alone, and so is everything when the list
    /// is locked.
    pub fn auto_compact(self: &Arc<Self>, shared: SharedRepository) -> Result<(), StackError> {
        let locked = match self.lock(shared) {
            Ok(l) => l,
            Err(StackError::Locked) => return Ok(()),
            Err(e) => return Err(e),
        };
        locked.auto_compact()
    }

    /// Merges all tables into one and removes table files that the list
    /// does not name (`git pack-refs`).
    pub fn compact_all(self: &Arc<Self>, shared: SharedRepository) -> Result<(), StackError> {
        let locked = self.lock(shared)?;
        let n = locked.tables.len();
        locked.compact(0, n, false)?;
        self.remove_stale_tables();
        Ok(())
    }

    /// Removes `*.ref` files that are not part of the stack.
    fn remove_stale_tables(&self) {
        let Ok((names, _)) = self.read_list() else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for e in entries.flatten() {
            let Ok(name) = e.file_name().into_string() else {
                continue;
            };
            if name.ends_with(".ref") && !names.contains(&name) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// A stack whose `tables.list` is locked. Dropping it releases the lock
/// without changing anything.
pub struct LockedStack {
    stack: Arc<Stack>,
    lock: Option<LockFile>,
    tables: Vec<Arc<Table>>,
    shared: SharedRepository,
    fsync: Option<FsyncMethod>,
    opts: ReftableOptions,
}

impl LockedStack {
    /// The tables, oldest first.
    pub fn tables(&self) -> &[Arc<Table>] {
        &self.tables
    }

    /// The stack.
    pub fn stack(&self) -> &Arc<Stack> {
        &self.stack
    }

    /// Sync new files to disk with `method` (`core.fsync=reference`).
    pub fn fsync(mut self, method: Option<FsyncMethod>) -> Self {
        self.fsync = method;
        self
    }

    /// The update index of the next transaction.
    pub fn next_update_index(&self) -> u64 {
        self.tables
            .last()
            .map_or(1, |t| t.max_update_index().saturating_add(1))
    }

    /// The options for writing a table of this stack.
    pub fn write_options(&self) -> WriteOptions {
        WriteOptions {
            block_size: self.opts.block_size,
            restart_interval: self.opts.restart_interval,
            index_objects: self.opts.index_objects,
            hash: self.stack.kind,
        }
    }

    /// Writes `data` as a new table file and returns its name.
    fn write_table(&self, data: &[u8], min: u64, max: u64) -> Result<String, StackError> {
        let dir = &self.stack.dir;
        let mut tmp = TempFile::with_mode(dir, "tmp_", 0o666).map_err(|e| self.stack.io(dir, e))?;
        let path = tmp.path().to_owned();
        tmp.write_all(data).map_err(|e| self.stack.io(&path, e))?;
        if let Some(method) = self.fsync {
            rgit_fs::harden(tmp.file_mut(), method).map_err(|e| self.stack.io(&path, e))?;
        }
        let _ = self.shared.apply(&path);
        let name = table_name(min, max);
        let dest = dir.join(&name);
        tmp.persist(&dest).map_err(|e| self.stack.io(&dest, e))?;
        Ok(name)
    }

    /// Replaces `tables.list` with `names` and releases the lock.
    fn commit_list(&mut self, names: &[String]) -> Result<(), StackError> {
        let Some(mut lock) = self.lock.take() else {
            return Err(StackError::Locked);
        };
        let path = self.stack.list_path();
        let mut text = String::new();
        for n in names {
            text.push_str(n);
            text.push('\n');
        }
        lock.write_all(text.as_bytes())
            .map_err(|e| self.stack.io(&path, e))?;
        if let Some(method) = self.fsync {
            rgit_fs::harden(lock.file_mut(), method).map_err(|e| self.stack.io(&path, e))?;
        }
        lock.commit().map_err(|e| self.stack.io(&path, e))?;
        // Forget the cached list; the next reader loads the new one.
        let mut cache = self.stack.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.stamp = None;
        Ok(())
    }

    /// Appends a table (the bytes of a finished [`Writer`]) covering the
    /// update indices `min..=max` and releases the lock.
    pub fn add_table(mut self, data: &[u8], min: u64, max: u64) -> Result<(), StackError> {
        let name = self.write_table(data, min, max)?;
        let mut names: Vec<String> = self.tables.iter().map(|t| t.name().to_owned()).collect();
        names.push(name.clone());
        if let Err(e) = self.commit_list(&names) {
            let _ = std::fs::remove_file(self.stack.dir.join(&name));
            return Err(e);
        }
        Ok(())
    }

    /// Makes sure `tables.list` exists (an empty stack still has the
    /// file) and releases the lock.
    pub fn touch(mut self) -> Result<(), StackError> {
        let names: Vec<String> = self.tables.iter().map(|t| t.name().to_owned()).collect();
        self.commit_list(&names)
    }

    /// The range of tables the geometric heuristic wants merged.
    fn auto_segment(&self) -> Option<(usize, usize)> {
        let sizes: Vec<u64> = self.tables.iter().map(|t| table_size(t)).collect();
        suggest_compaction(&sizes, u64::from(self.opts.geometric_factor))
    }

    /// Compacts what the heuristic asks for, as far as the tables are not
    /// locked by someone else.
    pub fn auto_compact(self) -> Result<(), StackError> {
        match self.auto_segment() {
            Some((start, end)) => self.compact(start, end, true),
            None => Ok(()),
        }
    }

    /// Merges the tables `start..end` into one. With `lenient`, a table
    /// that someone else holds a lock on shrinks the range to the tables
    /// newer than it; when fewer than two are left, that is
    /// [`StackError::Locked`] as well.
    pub fn compact(mut self, start: usize, end: usize, lenient: bool) -> Result<(), StackError> {
        let mut start = start;
        if end > self.tables.len() || start >= end {
            return Ok(());
        }
        // Lock the tables from the newest down; with `lenient`, keep what
        // could be locked above a table someone else holds.
        if end - start < 2 {
            // A single table is left as it is.
            return Ok(());
        }
        let mut locks = Vec::new();
        for i in (start..end).rev() {
            let path = self.stack.dir.join(self.tables[i].name());
            match LockFile::acquire(&path, &LockOptions::new()) {
                Ok(l) => locks.push(l),
                Err(_) if lenient => {
                    start = i + 1;
                    break;
                }
                Err(_) => return Err(StackError::Locked),
            }
        }
        if end - start < 2 {
            return Err(StackError::Locked);
        }
        let segment = &self.tables[start..end];
        let min = segment
            .iter()
            .map(|t| t.min_update_index())
            .min()
            .unwrap_or(1);
        let max = segment
            .iter()
            .map(|t| t.max_update_index())
            .max()
            .unwrap_or(1);
        let mut writer = Writer::new(self.write_options(), min, max);
        // Deletions only have to survive while older tables remain.
        let keep_deletions = start > 0;
        let mut refs = merged::merged_refs(segment, b"")?;
        while let Some(rec) = refs.next_record()? {
            if rec.is_deletion() && !keep_deletions {
                continue;
            }
            writer.add_ref(&rec)?;
        }
        let mut logs = merged::merged_logs(segment, b"")?;
        while let Some(mut rec) = logs.next_record()? {
            match &mut rec.update {
                None if !keep_deletions => continue,
                None => {}
                // As in the tables git compacts, every message ends with
                // a line feed, also the empty one of a log's marker.
                Some(u) if u.message.last() != Some(&b'\n') => u.message.push(b'\n'),
                Some(_) => {}
            }
            writer.add_log(&rec)?;
        }
        let mut names: Vec<String> = self.tables[..start]
            .iter()
            .map(|t| t.name().to_owned())
            .collect();
        let mut new_name = None;
        if !writer.is_empty() {
            let data = writer.finish()?;
            let name = self.write_table(&data, min, max)?;
            names.push(name.clone());
            new_name = Some(name);
        }
        names.extend(self.tables[end..].iter().map(|t| t.name().to_owned()));
        if let Err(e) = self.commit_list(&names) {
            if let Some(n) = new_name {
                let _ = std::fs::remove_file(self.stack.dir.join(n));
            }
            return Err(e);
        }
        for t in &self.tables[start..end] {
            let _ = std::fs::remove_file(self.stack.dir.join(t.name()));
        }
        drop(locks);
        Ok(())
    }
}

/// The size of a table for the compaction heuristic: the file without its
/// header and footer.
fn table_size(table: &Table) -> u64 {
    let overhead = if table.version() == 2 {
        28 + 72
    } else {
        24 + 68
    };
    table.file_len().saturating_sub(overhead - 1)
}

/// Picks the tables to merge so that the sizes form a geometric sequence
/// again: every table at least `factor` times as large as all newer
/// tables together. Returns the range `start..end` or `None` when the
/// sequence already holds.
pub fn suggest_compaction(sizes: &[u64], factor: u64) -> Option<(usize, usize)> {
    let factor = if factor == 0 { 2 } else { factor };
    // The newest table that is too large for its older neighbour ends the
    // range.
    let end = (1..sizes.len())
        .rev()
        .find(|&i| sizes[i - 1] < sizes[i].saturating_mul(factor))?;
    let mut start = end;
    let mut bytes = sizes[end];
    // Walk towards the oldest table; every table smaller than `factor`
    // times what lies above it joins the range.
    for i in (1..=end).rev() {
        let above = bytes;
        bytes = bytes.saturating_add(sizes[i - 1]);
        if sizes[i - 1] < above.saturating_mul(factor) {
            start = i - 1;
        }
    }
    (start < end).then_some((start, end + 1))
}

#[cfg(test)]
mod tests {
    use super::suggest_compaction;

    #[test]
    fn geometric_sequences_are_left_alone() {
        assert_eq!(suggest_compaction(&[], 2), None);
        assert_eq!(suggest_compaction(&[100], 2), None);
        assert_eq!(suggest_compaction(&[64, 32, 16, 8, 4, 2, 1], 2), None);
    }

    #[test]
    fn small_tables_are_merged() {
        assert_eq!(suggest_compaction(&[100, 1, 1], 2), Some((1, 3)));
        assert_eq!(suggest_compaction(&[1, 1, 1, 1], 2), Some((0, 4)));
        assert_eq!(
            suggest_compaction(&[512, 64, 17, 16, 9, 9, 9, 16, 2, 16], 2),
            Some((1, 10))
        );
    }
}
