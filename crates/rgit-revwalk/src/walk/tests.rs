// SPDX-License-Identifier: MIT
use std::collections::HashMap;

use rgit_hash::{HashKind, ObjectId};

use super::*;

/// In-memory commit graph: commit `n` has id `n` (big-endian in the last bytes).
#[derive(Default)]
struct Fake {
    commits: HashMap<ObjectId, CommitInfo>,
    loads: usize,
}

fn id(n: u32) -> ObjectId {
    let mut b = [0u8; 20];
    b[16..].copy_from_slice(&n.to_be_bytes());
    ObjectId::from_bytes(HashKind::Sha1, &b).unwrap()
}

fn num(id: &ObjectId) -> u32 {
    let b = id.as_bytes();
    u32::from_be_bytes([b[16], b[17], b[18], b[19]])
}

impl Fake {
    fn add(&mut self, n: u32, time: i64, parents: &[u32]) -> &mut Self {
        self.commits.insert(
            id(n),
            CommitInfo {
                parents: parents.iter().map(|&p| id(p)).collect(),
                time,
            },
        );
        self
    }
}

impl CommitSource for Fake {
    type Error = ObjectId;
    fn load(&mut self, id: &ObjectId) -> Result<CommitInfo, ObjectId> {
        self.loads += 1;
        self.commits.get(id).cloned().ok_or(*id)
    }
}

fn run(fake: &mut Fake, opts: WalkOptions, tips: &[i64]) -> Vec<u32> {
    let mut w = Walk::new(fake, opts);
    for &t in tips {
        w.push(&id(t.unsigned_abs() as u32), t < 0).unwrap();
    }
    w.collect_all().unwrap().iter().map(num).collect()
}

/// A(1) - B(2) - C(5) - M(6) - D(7)
///   \- S1(3) - S2(4) --/
fn merge_graph() -> Fake {
    let mut f = Fake::default();
    f.add(1, 100, &[])
        .add(2, 200, &[1])
        .add(3, 150, &[1])
        .add(4, 250, &[3])
        .add(5, 300, &[2])
        .add(6, 400, &[5, 4])
        .add(7, 500, &[6]);
    f
}

#[test]
fn default_order_is_by_date() {
    let mut f = merge_graph();
    assert_eq!(
        run(&mut f, WalkOptions::default(), &[7]),
        [7, 6, 5, 4, 2, 3, 1]
    );
}

#[test]
fn topo_and_date_order() {
    let mut f = merge_graph();
    let topo = WalkOptions {
        order: Order::Topo,
        ..Default::default()
    };
    assert_eq!(run(&mut f, topo, &[7]), [7, 6, 4, 3, 5, 2, 1]);
    let date = WalkOptions {
        order: Order::Date,
        ..Default::default()
    };
    assert_eq!(run(&mut f, date, &[7]), [7, 6, 5, 4, 2, 3, 1]);
    let topo = WalkOptions {
        order: Order::Topo,
        ..Default::default()
    };
    assert_eq!(run(&mut f, topo.clone(), &[4, 5]), [5, 2, 4, 3, 1]);
    assert_eq!(run(&mut f, topo, &[5, 4]), [5, 2, 4, 3, 1]);
}

#[test]
fn exclusion() {
    let mut f = merge_graph();
    assert_eq!(run(&mut f, WalkOptions::default(), &[7, -5]), [7, 6, 4, 3]);
    assert_eq!(run(&mut f, WalkOptions::default(), &[7, -4]), [7, 6, 5, 2]);
    assert_eq!(
        run(&mut f, WalkOptions::default(), &[5, -7]),
        Vec::<u32>::new()
    );
}

#[test]
fn first_parent_skip_count_reverse() {
    let mut f = merge_graph();
    let fp = WalkOptions {
        first_parent: true,
        ..Default::default()
    };
    assert_eq!(run(&mut f, fp, &[7]), [7, 6, 5, 2, 1]);
    let o = WalkOptions {
        skip: 1,
        max_count: Some(3),
        reverse: true,
        ..Default::default()
    };
    assert_eq!(run(&mut f, o, &[7]), [4, 5, 6]);
    let o = WalkOptions {
        skip: 2,
        max_count: Some(2),
        ..Default::default()
    };
    assert_eq!(run(&mut f, o, &[7]), [5, 4]);
}

#[test]
fn equal_dates_keep_insertion_order() {
    let mut f = Fake::default();
    f.add(1, 10, &[])
        .add(2, 10, &[1])
        .add(3, 10, &[1])
        .add(4, 10, &[2, 3]);
    assert_eq!(run(&mut f, WalkOptions::default(), &[4]), [4, 2, 3, 1]);
}

#[test]
fn clock_skew_within_slop() {
    // 4 is excluded and reaches 1 only through 3, whose date is far in the past.
    let mut f = Fake::default();
    f.add(1, 100, &[])
        .add(2, 200, &[1])
        .add(3, 50, &[1])
        .add(4, 300, &[3]);
    assert_eq!(run(&mut f, WalkOptions::default(), &[2, -4]), [2]);
}

#[test]
fn lazy_without_exclusions() {
    let mut f = Fake::default();
    f.add(1, 1, &[]);
    for n in 2..1000 {
        f.add(n, i64::from(n), &[n - 1]);
    }
    let o = WalkOptions {
        max_count: Some(3),
        ..Default::default()
    };
    assert_eq!(run(&mut f, o, &[999]), [999, 998, 997]);
    assert!(f.loads < 10, "{} loads", f.loads);
}

#[test]
fn missing_commit_is_an_error() {
    let mut f = Fake::default();
    f.add(2, 2, &[1]);
    let mut w = Walk::new(&mut f, WalkOptions::default());
    w.push(&id(2), false).unwrap();
    // Parents are read when a commit is taken out of the queue.
    assert_eq!(w.next_commit(), Err(id(1)));
}
