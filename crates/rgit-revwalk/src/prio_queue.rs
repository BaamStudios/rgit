// SPDX-License-Identifier: MIT
//! A priority queue with a caller-given order, as revision walks use it
//! for commits (newest first): [`PrioQueue::get`] returns the item that
//! orders first, items that order equal leave in the order they were put
//! in. Without an order ([`PrioQueue::lifo`]) it is a stack, which a walk
//! uses when no ordering is wanted.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

type Compare<T> = Box<dyn Fn(&T, &T) -> Ordering>;

/// See the module documentation.
pub struct PrioQueue<T> {
    cmp: Option<std::rc::Rc<Compare<T>>>,
    heap: BinaryHeap<Slot<T>>,
    stack: Vec<T>,
    seq: u64,
}

struct Slot<T> {
    item: T,
    seq: u64,
    cmp: std::rc::Rc<Compare<T>>,
}

impl<T> PartialEq for Slot<T> {
    fn eq(&self, other: &Self) -> bool {
        Ord::cmp(self, other) == Ordering::Equal
    }
}

impl<T> Eq for Slot<T> {}

impl<T> PartialOrd for Slot<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(Ord::cmp(self, other))
    }
}

impl<T> Ord for Slot<T> {
    /// `BinaryHeap` pops the greatest slot: the one that orders first,
    /// among equal ones the oldest.
    fn cmp(&self, other: &Self) -> Ordering {
        (self.cmp)(&other.item, &self.item).then_with(|| other.seq.cmp(&self.seq))
    }
}

impl<T> PrioQueue<T> {
    /// A queue that returns the item for which `cmp` orders first
    /// (`Ordering::Less`).
    pub fn new(cmp: impl Fn(&T, &T) -> Ordering + 'static) -> Self {
        PrioQueue {
            cmp: Some(std::rc::Rc::new(Box::new(cmp))),
            heap: BinaryHeap::new(),
            stack: Vec::new(),
            seq: 0,
        }
    }

    /// A queue without an order: the item put in last comes out first.
    pub fn lifo() -> Self {
        PrioQueue {
            cmp: None,
            heap: BinaryHeap::new(),
            stack: Vec::new(),
            seq: 0,
        }
    }

    /// Adds `item`.
    pub fn put(&mut self, item: T) {
        match &self.cmp {
            Some(cmp) => {
                self.heap.push(Slot {
                    item,
                    seq: self.seq,
                    cmp: cmp.clone(),
                });
                self.seq += 1;
            }
            None => self.stack.push(item),
        }
    }

    /// Removes and returns the next item.
    pub fn get(&mut self) -> Option<T> {
        match self.cmp {
            Some(_) => self.heap.pop().map(|s| s.item),
            None => self.stack.pop(),
        }
    }

    /// The next item, left in the queue.
    pub fn peek(&self) -> Option<&T> {
        match self.cmp {
            Some(_) => self.heap.peek().map(|s| &s.item),
            None => self.stack.last(),
        }
    }

    /// Removes the next item and puts `item` in instead (cheaper than
    /// [`get`](Self::get) and [`put`](Self::put)); returns the removed
    /// item, or `None` when the queue was empty (then only `item` is put).
    pub fn replace(&mut self, item: T) -> Option<T> {
        let old = self.get();
        self.put(item);
        old
    }

    /// Reverses the order of a [`lifo`](Self::lifo) queue (the item put in
    /// first comes out next). A queue with an order is not changed.
    pub fn reverse(&mut self) {
        if self.cmp.is_none() {
            self.stack.reverse();
        }
    }

    /// The number of items.
    pub fn len(&self) -> usize {
        self.heap.len() + self.stack.len()
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Removes every item.
    pub fn clear(&mut self) {
        self.heap.clear();
        self.stack.clear();
    }
}

impl<T> std::fmt::Debug for PrioQueue<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrioQueue")
            .field("lifo", &self.cmp.is_none())
            .field("len", &self.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain<T>(q: &mut PrioQueue<T>) -> Vec<T> {
        std::iter::from_fn(|| q.get()).collect()
    }

    #[test]
    fn ordered() {
        let mut q = PrioQueue::new(|a: &i32, b: &i32| a.cmp(b));
        for v in [2, 6, 3, 10, 9, 5, 7, 4, 5, 8, 1] {
            q.put(v);
        }
        assert_eq!(q.peek(), Some(&1));
        assert_eq!(q.replace(11), Some(1));
        assert_eq!(drain(&mut q), [2, 3, 4, 5, 5, 6, 7, 8, 9, 10, 11]);
        assert!(q.is_empty());
        assert_eq!(q.replace(1), None);
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn equal_items_leave_in_insertion_order() {
        let mut q = PrioQueue::new(|a: &(i32, char), b: &(i32, char)| a.0.cmp(&b.0));
        for v in [(1, 'a'), (0, 'x'), (1, 'b'), (1, 'c'), (0, 'y')] {
            q.put(v);
        }
        let order: String = drain(&mut q).into_iter().map(|v| v.1).collect();
        assert_eq!(order, "xyabc");
    }

    #[test]
    fn stack() {
        let mut q = PrioQueue::lifo();
        for v in [1, 2, 3] {
            q.put(v);
        }
        assert_eq!(q.peek(), Some(&3));
        q.reverse();
        assert_eq!(drain(&mut q), [1, 2, 3]);
    }
}
