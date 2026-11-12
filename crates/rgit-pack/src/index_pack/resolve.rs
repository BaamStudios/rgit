// SPDX-License-Identifier: MIT

//! Second pass: resolve deltas, in parallel, with bounded memory.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use rgit_hash::{HashKind, ObjectId, hash_object};

use super::{BadEntry, Base, Checker, Entry, Error, Options, Progress, Slots, effective_threads};
use crate::ObjectKind;
use crate::data::inflate_exact;
use crate::delta::apply_delta;

/// Which entries depend on which.
pub(crate) struct Graph {
    /// (base entry, delta entry), sorted.
    ofs: Vec<(u32, u32)>,
    /// (base id, delta entry), sorted.
    refs: Vec<(ObjectId, u32)>,
    /// One flag per entry: the delta was taken by a thread.
    claimed: Vec<AtomicBool>,
}

impl Graph {
    pub fn new(entries: &[Entry]) -> Graph {
        let mut ofs = Vec::new();
        let mut refs = Vec::new();
        for (i, e) in entries.iter().enumerate() {
            match e.base {
                Base::Ofs(b) => ofs.push((b, i as u32)),
                Base::Ref(id) => refs.push((id, i as u32)),
                Base::None(_) => {}
            }
        }
        ofs.sort_unstable();
        refs.sort_unstable();
        Graph {
            ofs,
            refs,
            claimed: (0..entries.len()).map(|_| AtomicBool::new(false)).collect(),
        }
    }

    /// Make room for appended entries.
    pub fn grow(&mut self, len: usize) {
        while self.claimed.len() < len {
            self.claimed.push(AtomicBool::new(false));
        }
    }

    /// The deltas whose base is entry `idx` with id `id`.
    pub fn children(&self, idx: u32, id: &ObjectId) -> Vec<u32> {
        let mut out = Vec::new();
        let start = self.ofs.partition_point(|&(b, _)| b < idx);
        out.extend(
            self.ofs[start..]
                .iter()
                .take_while(|&&(b, _)| b == idx)
                .map(|&(_, c)| c),
        );
        let start = self.refs.partition_point(|(b, _)| b < id);
        out.extend(
            self.refs[start..]
                .iter()
                .take_while(|(b, _)| b == id)
                .map(|&(_, c)| c),
        );
        out
    }

    pub fn has_children(&self, idx: u32, id: &ObjectId) -> bool {
        let i = self.ofs.partition_point(|&(b, _)| b < idx);
        if self.ofs.get(i).is_some_and(|&(b, _)| b == idx) {
            return true;
        }
        let i = self.refs.partition_point(|(b, _)| b < id);
        self.refs.get(i).is_some_and(|(b, _)| b == id)
    }

    /// Take delta `idx`; false if another thread already did.
    pub fn claim(&self, idx: u32) -> bool {
        !self.claimed[idx as usize].swap(true, Ordering::AcqRel)
    }

    /// `REF_DELTA` entries nobody took yet, in pack order, with their base.
    pub fn unclaimed_refs(&self) -> Vec<(u32, ObjectId)> {
        let mut out: Vec<(u32, ObjectId)> = self
            .refs
            .iter()
            .filter(|&&(_, c)| !self.claimed[c as usize].load(Ordering::Acquire))
            .map(|&(id, c)| (c, id))
            .collect();
        out.sort_unstable_by_key(|&(c, _)| c);
        out
    }
}

/// Everything the workers share.
pub(crate) struct Ctx<'a, 'p> {
    pub pack: &'a [u8],
    pub entries: &'a [Entry],
    pub slots: &'a [OnceLock<(ObjectId, ObjectKind)>],
    pub graph: &'a Graph,
    pub hash: HashKind,
    pub checker: &'a Checker<'a>,
    /// Bytes of delta bases one thread may keep.
    pub budget: usize,
    pub abort: AtomicBool,
    pub error: Mutex<Option<Error>>,
    pub resolved: AtomicU64,
    pub progress: Option<Mutex<&'p mut (dyn Progress + 'static)>>,
}

impl Ctx<'_, '_> {
    fn fail(&self, e: Error) {
        self.abort.store(true, Ordering::Release);
        let mut slot = self.error.lock().unwrap_or_else(|p| p.into_inner());
        if slot.is_none() {
            *slot = Some(e);
        }
    }

    /// Inflate the data of entry `idx` from the pack.
    fn inflate(&self, idx: u32) -> Result<Vec<u8>, Error> {
        let e = &self.entries[idx as usize];
        let bad = |code| Error::BadObject {
            offset: e.offset,
            what: BadEntry::Inflate { code, detail: None },
        };
        let start = usize::try_from(e.data_offset).map_err(|_| bad(-5))?;
        let size = usize::try_from(e.size).map_err(|_| bad(-5))?;
        let input = self.pack.get(start..).ok_or_else(|| bad(-5))?;
        // The first pass checked the stream already.
        inflate_exact(input, size, e.offset)
            .map(|(d, _)| d)
            .map_err(|_| bad(-3))
    }

    /// The object of delta `idx` applied to `base`.
    fn apply(&self, idx: u32, base: &[u8]) -> Result<Vec<u8>, Error> {
        let delta = self.inflate(idx)?;
        apply_delta(base, &delta).map_err(|reason| Error::BadObject {
            offset: self.entries[idx as usize].offset,
            what: BadEntry::BadDelta {
                note: delta_note(&delta, reason),
            },
        })
    }

    fn tick(&self) {
        let n = self.resolved.fetch_add(1, Ordering::AcqRel) + 1;
        if let Some(p) = &self.progress {
            let mut p = p.lock().unwrap_or_else(|p| p.into_inner());
            p.update(n, 0);
        }
    }

    /// Resolve every delta below the object at entry `root` (whose id and
    /// type are known). `data` is its content if already at hand.
    pub fn resolve_tree(&self, root: u32, data: Option<Vec<u8>>) -> Result<(), Error> {
        let (root_id, kind) = *self.slots[root as usize].get().expect("root resolved");
        let children = self.graph.children(root, &root_id);
        if children.is_empty() {
            return Ok(());
        }
        let data = match data {
            Some(d) => d,
            None => self.inflate(root)?,
        };
        let mut stack = vec![Frame {
            idx: root,
            data: Some(data),
            children,
            next: 0,
        }];
        let mut held: usize = stack[0].data.as_ref().map_or(0, Vec::len);
        while let Some(top) = stack.last_mut() {
            if self.abort.load(Ordering::Acquire) {
                return Ok(());
            }
            let Some(&child) = top.children.get(top.next) else {
                if let Some(d) = stack.pop().and_then(|f| f.data) {
                    held -= d.len();
                }
                continue;
            };
            top.next += 1;
            if !self.graph.claim(child) {
                continue;
            }
            if stack.last().is_some_and(|f| f.data.is_none()) {
                held += self.rebuild(&mut stack)?;
            }
            let base = stack
                .last()
                .and_then(|f| f.data.as_deref())
                .expect("rebuilt");
            let out = self.apply(child, base)?;
            let id = hash_object(self.hash, kind.as_str(), &out)
                .map_err(|_| Error::CollisionAttack(self.entries[child as usize].offset))?;
            let _ = self.slots[child as usize].set((id, kind));
            self.checker.check(&id, kind, &out)?;
            self.tick();
            let grandchildren = self.graph.children(child, &id);
            if grandchildren.is_empty() {
                continue;
            }
            held += out.len();
            stack.push(Frame {
                idx: child,
                data: Some(out),
                children: grandchildren,
                next: 0,
            });
            // Over budget: drop the oldest bases (never the root, which
            // may not be re-readable, and never the top).
            let top = stack.len() - 1;
            let mut i = 1;
            while held > self.budget && i < top {
                if let Some(d) = stack[i].data.take() {
                    held -= d.len();
                }
                i += 1;
            }
        }
        Ok(())
    }

    /// Recreate the data of the top frame from the nearest frame below it
    /// that still has its data. Returns the bytes added.
    fn rebuild(&self, stack: &mut [Frame]) -> Result<usize, Error> {
        let top = stack.len() - 1;
        let from = (0..top)
            .rev()
            .find(|&i| stack[i].data.is_some())
            .expect("the root keeps its data");
        let mut cur = stack[from].data.clone().expect("checked");
        for frame in &stack[from + 1..=top] {
            cur = self.apply(frame.idx, &cur)?;
        }
        let n = cur.len();
        stack[top].data = Some(cur);
        Ok(n)
    }
}

struct Frame {
    idx: u32,
    data: Option<Vec<u8>>,
    children: Vec<u32>,
    next: usize,
}

/// The `error:` line git prints before "failed to apply delta": none for
/// deltas shorter than four bytes or with the wrong base size, the opcode
/// message for opcode 0, otherwise "delta replay has gone wild".
fn delta_note(delta: &[u8], reason: crate::DeltaError) -> Option<&'static str> {
    use super::messages as m;
    if delta.len() < 4 || reason == "base size mismatch" || reason == "truncated delta header" {
        None
    } else if reason == "reserved delta opcode 0" {
        Some(m::DELTA_OPCODE_0)
    } else {
        Some(m::DELTA_GONE_WILD)
    }
}

/// Resolve all deltas reachable from the base objects of the pack.
/// Returns the number of resolved deltas.
pub(crate) fn resolve_all(
    pack: &[u8],
    entries: &[Entry],
    slots: &Slots,
    graph: &Graph,
    opts: &Options,
    checker: &Checker<'_>,
    progress: Option<&mut (dyn Progress + 'static)>,
) -> Result<u64, Error> {
    let roots: Vec<u32> = entries
        .iter()
        .enumerate()
        .filter(|(i, e)| {
            !e.is_delta()
                && slots[*i]
                    .get()
                    .is_some_and(|(id, _)| graph.has_children(*i as u32, id))
        })
        .map(|(i, _)| i as u32)
        .collect();
    let threads = effective_threads(opts.threads).min(roots.len()).max(1);
    let ctx = Ctx {
        pack,
        entries,
        slots,
        graph,
        hash: opts.hash,
        checker,
        budget: (opts.memory_limit / threads).max(1 << 20),
        abort: AtomicBool::new(false),
        error: Mutex::new(None),
        resolved: AtomicU64::new(0),
        progress: progress.map(Mutex::new),
    };
    let next = AtomicUsize::new(0);
    let work = || {
        loop {
            if ctx.abort.load(Ordering::Acquire) {
                return;
            }
            let i = next.fetch_add(1, Ordering::AcqRel);
            let Some(&root) = roots.get(i) else { return };
            if let Err(e) = ctx.resolve_tree(root, None) {
                ctx.fail(e);
                return;
            }
        }
    };
    if threads == 1 {
        work();
    } else {
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(work);
            }
        });
    }
    if let Some(e) = ctx.error.into_inner().unwrap_or_else(|p| p.into_inner()) {
        return Err(e);
    }
    Ok(ctx.resolved.into_inner())
}
