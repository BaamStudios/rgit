// SPDX-License-Identifier: MIT

//! Parallel checkout: regular files written by several worker threads
//! (`checkout.workers`, `checkout.thresholdForParallelism`, see
//! git-config(1) and `Documentation/technical/parallel-checkout.adoc`).
//!
//! The caller walks the entries to write in index order as usual and
//! offers each one to [`ParallelCheckout::enqueue`]. An eligible entry (a
//! regular file whose conversion runs in-process, see
//! [`Filter::parallel_conversion`]) gets its leading directories created
//! and whatever is at its path removed right away, in sequence, and is then
//! queued; every other entry is written by the caller at once. Then
//! [`ParallelCheckout::run`] writes the queue: with enough entries (at
//! least the threshold, and at least two workers' worth) the queue is cut
//! into contiguous chunks, one per worker thread, otherwise it is written
//! in sequence.
//!
//! A worker never removes anything it did not create. When a leading
//! directory of its entry is not a real directory any more, or the file
//! already exists, another entry got there first (two paths naming the
//! same file, as on a case-insensitive file system): the entry is marked
//! as collided and, after all workers are done, written once more in
//! sequence, so the last colliding entry wins as in a sequential
//! checkout. Every entry, collided or not, gets the `lstat` data of the
//! file it wrote.
//!
//! git runs the workers as `checkout--worker` processes; rgit uses threads
//! but reports each worker as a trace2 child (`child_start` /
//! `child_exit` with `git checkout--worker`), and the collisions as trace2
//! data in category `pcheckout` (`collision/basename`,
//! `collision/dirname`).

use std::fs::{self, Metadata};
use std::io::{self, Write};
use std::path::Path;

use rgit_hash::ObjectId;
use rgit_index::Entry;
use rgit_object::ObjectKind;

use crate::checkout::{self, CheckoutError, Filter, join, lossy, strerror};
use crate::objects::Objects;

/// The in-process conversion of one blob for the work tree, run on a
/// worker thread.
pub type Conversion = Box<dyn FnOnce(Vec<u8>) -> Vec<u8> + Send>;

/// `checkout.workers` and `checkout.thresholdForParallelism`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParallelConfig {
    /// Number of workers; 1 means sequential checkout.
    pub workers: usize,
    /// Minimum number of eligible entries for a parallel checkout.
    pub threshold: usize,
}

impl Default for ParallelConfig {
    fn default() -> Self {
        ParallelConfig {
            workers: 1,
            threshold: 100,
        }
    }
}

impl ParallelConfig {
    /// The configuration from the raw values: `workers` below one means
    /// one worker per logical CPU, a negative threshold counts as zero.
    #[must_use]
    pub fn new(workers: i64, threshold: i64) -> ParallelConfig {
        let workers = if workers < 1 {
            std::thread::available_parallelism().map_or(1, std::num::NonZero::get)
        } else {
            usize::try_from(workers).unwrap_or(usize::MAX)
        };
        ParallelConfig {
            workers,
            threshold: usize::try_from(threshold.max(0)).unwrap_or(usize::MAX),
        }
    }

    /// Whether entries are queued at all.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.workers > 1
    }
}

/// A queued entry.
struct Item {
    key: usize,
    rel: Vec<u8>,
    entry: Entry,
    conv: Option<Conversion>,
}

/// Why a worker did not write its entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Collision {
    /// A leading directory is not a directory.
    Dirname,
    /// The file already exists.
    Basename,
}

/// What became of one queued entry.
#[derive(Debug)]
pub struct Outcome {
    /// The key it was queued with.
    pub key: usize,
    /// The path it was written to (relative to the root).
    pub rel: Vec<u8>,
    /// The `lstat` data of the new file, or why it could not be written.
    pub result: Result<Metadata, CheckoutError>,
    /// Another entry had already written a file at the same place; this
    /// one was written again afterwards, replacing it.
    pub collided: bool,
}

/// The queue of entries to be written by workers.
pub struct ParallelCheckout<'a> {
    config: ParallelConfig,
    root: &'a Path,
    odb: Option<&'a (dyn Objects + Sync)>,
    queue: Vec<Item>,
    collided_on_enqueue: Vec<usize>,
    detect_existing: bool,
}

impl<'a> ParallelCheckout<'a> {
    /// An empty queue for writing below `root` with blobs from `odb`.
    /// Nothing is queued when `config` asks for one worker or `odb`
    /// cannot be shared between threads.
    pub fn new(config: ParallelConfig, root: &'a Path, odb: &'a dyn Objects) -> Self {
        let odb = if config.enabled() {
            odb.as_sync()
        } else {
            None
        };
        ParallelCheckout {
            config,
            root,
            odb,
            queue: Vec::new(),
            collided_on_enqueue: Vec::new(),
            detect_existing: false,
        }
    }

    /// Treat a file found at an entry's path while queueing it as a
    /// collision (an initial checkout into an empty work tree, `clone`).
    #[must_use]
    pub fn detect_existing(mut self, yes: bool) -> Self {
        self.detect_existing = yes;
        self
    }

    /// Whether entries are queued at all.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.odb.is_some()
    }

    /// Queues `entry`, to be written at `rel` (the entry's path or the path
    /// with a prefix), when a worker can write it; `key` identifies it in
    /// the [`Outcome`]. The leading directories are created and whatever
    /// is at `rel` is removed now. Returns `Ok(false)` without doing
    /// anything when the entry is not eligible: the caller writes it.
    ///
    /// # Errors
    /// When a leading directory cannot be created or the old file cannot
    /// be removed (the entry is not queued).
    pub fn enqueue(
        &mut self,
        key: usize,
        rel: &[u8],
        entry: &Entry,
        filter: &dyn Filter,
    ) -> Result<bool, CheckoutError> {
        if self.odb.is_none() || !entry.mode.is_file() {
            return Ok(false);
        }
        let Some(conv) = filter.parallel_conversion(entry.path(), &entry.id) else {
            return Ok(false);
        };
        checkout::create_leading_dirs(self.root, rel, true)?;
        let full = join(self.root, rel);
        if self.detect_existing && fs::symlink_metadata(&full).is_ok() {
            self.collided_on_enqueue.push(key);
        }
        checkout::remove_any(&full).map_err(|e| CheckoutError::CreateFile {
            path: lossy(rel),
            error: strerror(&e),
        })?;
        self.queue.push(Item {
            key,
            rel: rel.to_vec(),
            entry: entry.clone(),
            conv: Some(conv),
        });
        Ok(true)
    }

    /// Number of queued entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Whether nothing is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Writes the queued entries, in parallel when there are enough of
    /// them, and returns what became of each, in queue order.
    #[must_use]
    pub fn run(self) -> Vec<Outcome> {
        let ParallelCheckout {
            config,
            root,
            odb,
            mut queue,
            collided_on_enqueue,
            ..
        } = self;
        let Some(odb) = odb else {
            return Vec::new();
        };
        let n = queue.len();
        let workers = config.workers.min(n);
        let parallel = n > 0 && n >= config.threshold && workers > 1;
        let mut results: Vec<Option<ItemResult>> = if parallel {
            run_workers(root, odb, &mut queue, workers)
        } else {
            queue
                .iter_mut()
                .map(|item| Some(write_item(root, odb, item)))
                .collect()
        };
        let mut out = Vec::with_capacity(n);
        for (item, res) in queue.iter_mut().zip(results.iter_mut()) {
            let (result, collision) = res.take().unwrap_or_else(|| {
                (
                    Err(CheckoutError::CreateFile {
                        path: lossy(&item.rel),
                        error: "worker failed".into(),
                    }),
                    None,
                )
            });
            if parallel && let Some(c) = collision {
                let key = match c {
                    Collision::Dirname => "collision/dirname",
                    Collision::Basename => "collision/basename",
                };
                rgit_trace::trace2::data("pcheckout", key, &lossy(&item.rel), false);
            }
            out.push((item, result, collision.is_some()));
        }
        // Collided entries are written once more, one after the other,
        // replacing what the entry they collided with left behind.
        let mut outcomes = Vec::with_capacity(n);
        for (item, mut result, collided) in out {
            if collided {
                result = rewrite_collided(root, odb, item);
            }
            outcomes.push(Outcome {
                key: item.key,
                rel: std::mem::take(&mut item.rel),
                result,
                collided: collided || collided_on_enqueue.contains(&item.key),
            });
        }
        outcomes
    }
}

type ItemResult = (Result<Metadata, CheckoutError>, Option<Collision>);

/// Spreads `queue` over `workers` threads in contiguous chunks.
fn run_workers(
    root: &Path,
    odb: &(dyn Objects + Sync),
    queue: &mut [Item],
    workers: usize,
) -> Vec<Option<ItemResult>> {
    let n = queue.len();
    let base = n / workers;
    let extra = n % workers;
    let mut chunks = Vec::with_capacity(workers);
    let mut rest = queue;
    for w in 0..workers {
        let len = base + usize::from(w < extra);
        let (chunk, tail) = rest.split_at_mut(len);
        chunks.push(chunk);
        rest = tail;
    }
    let argv = ["git", "checkout--worker"];
    let mut results: Vec<Option<ItemResult>> = Vec::with_capacity(n);
    std::thread::scope(|scope| {
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                let trace = rgit_trace::trace2::child_start(
                    rgit_trace::trace2::ChildInfo::default(),
                    &argv,
                );
                let handle = scope.spawn(move || {
                    chunk
                        .iter_mut()
                        .map(|item| write_item(root, odb, item))
                        .collect::<Vec<_>>()
                });
                (trace, handle)
            })
            .collect();
        for (trace, handle) in handles {
            match handle.join() {
                Ok(r) => {
                    results.extend(r.into_iter().map(Some));
                    if let Some(t) = trace {
                        t.exit(i64::from(std::process::id()), 0);
                    }
                }
                Err(_) => {
                    if let Some(t) = trace {
                        t.exit(i64::from(std::process::id()), 1);
                    }
                }
            }
        }
    });
    results.resize_with(n, || None);
    results
}

/// The worker's part for one entry: checks for a collision, creates the
/// file, reads and converts the blob and writes it. A file it created is
/// removed again when the blob cannot be read or written.
fn write_item(root: &Path, odb: &(dyn Objects + Sync), item: &mut Item) -> ItemResult {
    let rel = &item.rel;
    let create_err = |e: &io::Error| CheckoutError::CreateFile {
        path: lossy(rel),
        error: strerror(e),
    };
    let dirs_ok = rel.iter().enumerate().all(|(i, &b)| {
        b != b'/' || fs::symlink_metadata(join(root, &rel[..i])).is_ok_and(|m| m.is_dir())
    });
    if !dirs_ok {
        return (
            Err(CheckoutError::CreateFile {
                path: lossy(rel),
                error: "File exists".into(),
            }),
            Some(Collision::Dirname),
        );
    }
    let full = join(root, rel);
    let mut f = match open_new(&full, &item.entry) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            return (Err(create_err(&e)), Some(Collision::Basename));
        }
        Err(e) => return (Err(create_err(&e)), None),
    };
    let data = match read_blob(odb, &item.entry.id) {
        Some(d) => d,
        None => {
            drop(f);
            let _ = fs::remove_file(&full);
            return (
                Err(CheckoutError::ReadObject {
                    id: item.entry.id,
                    path: lossy(rel),
                }),
                None,
            );
        }
    };
    let data = match item.conv.take() {
        Some(conv) => conv(data),
        None => data,
    };
    if let Err(e) = f.write_all(&data) {
        drop(f);
        let _ = fs::remove_file(&full);
        return (Err(create_err(&e)), None);
    }
    drop(f);
    (
        fs::symlink_metadata(&full).map_err(|e| create_err(&e)),
        None,
    )
}

/// Writes a collided entry in sequence, replacing whatever is there.
fn rewrite_collided(
    root: &Path,
    odb: &(dyn Objects + Sync),
    item: &mut Item,
) -> Result<Metadata, CheckoutError> {
    let Some(data) = read_blob(odb, &item.entry.id) else {
        return Err(CheckoutError::ReadObject {
            id: item.entry.id,
            path: lossy(&item.rel),
        });
    };
    // A worker stops at a collision before it converts anything.
    let data = match item.conv.take() {
        Some(conv) => conv(data),
        None => data,
    };
    checkout::write_entry(root, &item.rel, &item.entry, &data, true)
        .map(|m| m.expect("a regular file has stat data"))
}

fn read_blob(odb: &(dyn Objects + Sync), id: &ObjectId) -> Option<Vec<u8>> {
    match odb.read_raw(id) {
        Ok(Some(raw)) if raw.kind == ObjectKind::Blob => Some(raw.data),
        _ => None,
    }
}

fn open_new(full: &Path, entry: &Entry) -> io::Result<fs::File> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(if entry.mode.0 & 0o111 != 0 {
            0o777
        } else {
            0o666
        });
    }
    #[cfg(not(unix))]
    let _ = entry;
    opts.open(full)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rgit_hash::HashKind;
    use rgit_index::{Mode, Stage};
    use rgit_object::RawObject;

    use super::*;
    use crate::checkout::Identity;

    struct Mem(HashMap<ObjectId, Vec<u8>>);

    impl Objects for Mem {
        fn hash_kind(&self) -> HashKind {
            HashKind::Sha1
        }
        fn read_raw(&self, id: &ObjectId) -> Result<Option<RawObject>, String> {
            Ok(self.0.get(id).map(|d| RawObject {
                kind: ObjectKind::Blob,
                data: d.clone(),
            }))
        }
        fn has(&self, id: &ObjectId) -> bool {
            self.0.contains_key(id)
        }
        fn as_sync(&self) -> Option<&(dyn Objects + Sync)> {
            Some(self)
        }
    }

    fn blob(odb: &mut Mem, data: &[u8]) -> ObjectId {
        let id = rgit_hash::hash_object(HashKind::Sha1, "blob", data).unwrap();
        odb.0.insert(id, data.to_vec());
        id
    }

    fn entry(path: &str, id: ObjectId) -> Entry {
        Entry::new(path, Stage::MERGED, Mode::FILE, id)
    }

    const TWO: ParallelConfig = ParallelConfig {
        workers: 2,
        threshold: 0,
    };

    #[test]
    fn writes_everything_and_reports_in_queue_order() {
        let dir = tempfile::tempdir().unwrap();
        let mut odb = Mem(HashMap::new());
        let ids: Vec<_> = (0..7)
            .map(|i| blob(&mut odb, format!("{i}\n").as_bytes()))
            .collect();
        let mut pc = ParallelCheckout::new(TWO, dir.path(), &odb);
        for (i, id) in ids.iter().enumerate() {
            let path = format!("d{}/f{i}", i % 3);
            assert!(
                pc.enqueue(i, path.as_bytes(), &entry(&path, *id), &Identity)
                    .unwrap()
            );
        }
        let out = pc.run();
        let keys: Vec<usize> = out.iter().map(|o| o.key).collect();
        assert_eq!(keys, (0..7).collect::<Vec<_>>());
        for (i, o) in out.iter().enumerate() {
            assert!(o.result.is_ok() && !o.collided);
            let path = dir.path().join(format!("d{}/f{i}", i % 3));
            assert_eq!(fs::read(path).unwrap(), format!("{i}\n").into_bytes());
        }
    }

    #[test]
    fn one_worker_queues_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut odb = Mem(HashMap::new());
        let id = blob(&mut odb, b"x");
        let mut pc = ParallelCheckout::new(ParallelConfig::default(), dir.path(), &odb);
        assert!(!pc.enqueue(0, b"f", &entry("f", id), &Identity).unwrap());
    }

    #[test]
    fn basename_collision_is_written_again() {
        // "f" and "./f" name the same file, as "F" and "f" do on a
        // case-insensitive file system: one worker finds the file of the
        // other, and that entry is written again afterwards.
        let dir = tempfile::tempdir().unwrap();
        let mut odb = Mem(HashMap::new());
        let a = blob(&mut odb, b"a\n");
        let b = blob(&mut odb, b"b\n");
        let mut pc = ParallelCheckout::new(TWO, dir.path(), &odb);
        pc.enqueue(0, b"f", &entry("f", a), &Identity).unwrap();
        pc.enqueue(1, b"./f", &entry("f", b), &Identity).unwrap();
        let out = pc.run();
        assert_eq!(out.iter().filter(|o| o.collided).count(), 1);
        assert!(out.iter().all(|o| o.result.is_ok()));
        let winner = out.iter().find(|o| o.collided).unwrap();
        let expect: &[u8] = if winner.key == 0 { b"a\n" } else { b"b\n" };
        assert_eq!(fs::read(dir.path().join("f")).unwrap(), expect);
    }

    #[test]
    fn dirname_collision_replaces_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut odb = Mem(HashMap::new());
        let a = blob(&mut odb, b"a\n");
        let mut pc = ParallelCheckout::new(TWO, dir.path(), &odb);
        pc.enqueue(0, b"d/x", &entry("d/x", a), &Identity).unwrap();
        pc.enqueue(1, b"e", &entry("e", a), &Identity).unwrap();
        // Something else turned the leading directory into a file.
        fs::remove_dir(dir.path().join("d")).unwrap();
        fs::write(dir.path().join("d"), b"in the way").unwrap();
        let out = pc.run();
        assert!(out[0].collided && !out[1].collided);
        assert!(out[0].result.is_ok());
        assert_eq!(fs::read(dir.path().join("d/x")).unwrap(), b"a\n");
    }

    #[test]
    fn missing_blob_leaves_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut odb = Mem(HashMap::new());
        let a = blob(&mut odb, b"a\n");
        let gone = rgit_hash::hash_object(HashKind::Sha1, "blob", b"gone").unwrap();
        let mut pc = ParallelCheckout::new(TWO, dir.path(), &odb);
        pc.enqueue(0, b"a", &entry("a", a), &Identity).unwrap();
        pc.enqueue(1, b"g", &entry("g", gone), &Identity).unwrap();
        let out = pc.run();
        assert!(out[0].result.is_ok());
        assert_eq!(
            out[1].result.as_ref().unwrap_err().to_string(),
            format!("cannot read object {} 'g'", gone.to_hex())
        );
        assert!(!dir.path().join("g").exists());
    }
}
