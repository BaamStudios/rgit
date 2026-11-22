// SPDX-License-Identifier: MIT
//! History simplification on the example history of the "History
//! Simplification" section of Documentation/rev-list-options.adoc.

use std::collections::HashMap;

use rgit_hash::{HashKind, ObjectId};

use crate::walk::{CommitInfo, CommitSource, Walk, WalkOptions};

/// Commits with the content of the one path of interest (`None`: the
/// path does not exist).
#[derive(Default)]
struct Graph {
    commits: HashMap<ObjectId, (CommitInfo, Option<&'static str>)>,
    names: HashMap<ObjectId, &'static str>,
}

fn id(n: u32) -> ObjectId {
    let mut b = [0u8; 20];
    b[16..].copy_from_slice(&n.to_be_bytes());
    ObjectId::from_bytes(HashKind::Sha1, &b).unwrap()
}

impl Graph {
    fn add(&mut self, n: u32, name: &'static str, parents: &[u32], content: Option<&'static str>) {
        self.commits.insert(
            id(n),
            (
                CommitInfo {
                    parents: parents.iter().map(|&p| id(p)).collect(),
                    time: i64::from(n) * 100,
                },
                content,
            ),
        );
        self.names.insert(id(n), name);
    }

    fn name(&self, id: &ObjectId) -> &'static str {
        self.names[id]
    }
}

impl CommitSource for Graph {
    type Error = ObjectId;
    fn load(&mut self, id: &ObjectId) -> Result<CommitInfo, ObjectId> {
        self.commits.get(id).map(|c| c.0.clone()).ok_or(*id)
    }
    fn treesame(&mut self, id: &ObjectId, parent: Option<&ObjectId>) -> Result<bool, ObjectId> {
        let mine = self.commits[id].1;
        let theirs = parent.and_then(|p| self.commits[p].1);
        Ok(mine == theirs)
    }
    fn only_added(&mut self, id: &ObjectId, parent: &ObjectId) -> Result<bool, ObjectId> {
        Ok(self.commits[id].1.is_some() && self.commits[parent].1.is_none())
    }
}

/// ```text
///   .-A---M---N---O---P---Q
///  /     /   /   /   /   /
/// I     B   C   D   E   Y
///  \   /   /   /   /   /
///   `-------------'   X
/// ```
fn example() -> Graph {
    let mut g = Graph::default();
    g.add(1, "I", &[], Some("asdf"));
    g.add(2, "A", &[1], Some("foo"));
    g.add(3, "B", &[1], Some("foo"));
    g.add(4, "M", &[2, 3], Some("foo"));
    g.add(5, "C", &[1], Some("asdf"));
    g.add(6, "N", &[4, 5], Some("foobar"));
    g.add(7, "D", &[1], Some("baz"));
    g.add(8, "O", &[6, 7], Some("foobarbaz"));
    g.add(9, "E", &[1], Some("asdf"));
    g.add(10, "P", &[8, 9], Some("foobarbaz"));
    g.add(11, "X", &[], None);
    g.add(12, "Y", &[11], None);
    g.add(13, "Q", &[10, 12], Some("foobarbaz"));
    g
}

/// The shown commits as `name(parents)`, or plain names.
fn walk(g: &mut Graph, opts: WalkOptions, with_parents: bool) -> String {
    let mut w = Walk::new(&mut *g, opts);
    w.push(&id(13), false).unwrap();
    let shown = w.collect_all().unwrap();
    let parents: Vec<Vec<ObjectId>> = shown
        .iter()
        .map(|c| w.parents(c).unwrap().to_vec())
        .collect();
    drop(w);
    let mut out = Vec::new();
    for (c, ps) in shown.iter().zip(parents) {
        if with_parents {
            let ps: Vec<&str> = ps.iter().map(|p| g.name(p)).collect();
            out.push(format!("{}({})", g.name(c), ps.join("")));
        } else {
            out.push(g.name(c).to_string());
        }
    }
    out.join(" ")
}

fn prune() -> WalkOptions {
    WalkOptions {
        prune: true,
        ..Default::default()
    }
}

#[test]
fn default_mode_follows_a_treesame_parent() {
    let mut g = example();
    assert_eq!(walk(&mut g, prune(), false), "O D N A I");
    let o = WalkOptions {
        rewrite_parents: true,
        ..prune()
    };
    assert_eq!(walk(&mut g, o, true), "O(ND) D(I) N(AI) A(I) I()");
}

#[test]
fn full_history_without_rewriting() {
    let mut g = example();
    let o = WalkOptions {
        full_history: true,
        ..prune()
    };
    assert_eq!(walk(&mut g, o, false), "Q P O D N B A I");
}

#[test]
fn full_history_with_rewriting() {
    let mut g = example();
    let o = WalkOptions {
        full_history: true,
        rewrite_parents: true,
        ..prune()
    };
    assert_eq!(
        walk(&mut g, o, true),
        "Q(P) P(OI) O(ND) D(I) N(MI) M(AB) B(I) A(I) I()"
    );
}

#[test]
fn sparse_shows_every_walked_commit() {
    let mut g = example();
    let o = WalkOptions {
        sparse: true,
        ..prune()
    };
    assert_eq!(walk(&mut g, o, false), "Q P O D N C M A I");
}

#[test]
fn simplify_merges() {
    let mut g = example();
    let o = WalkOptions {
        simplify_merges: true,
        ..prune()
    };
    assert_eq!(walk(&mut g, o, true), "O(ND) D(I) N(M) M(AB) B(I) A(I) I()");
}

#[test]
fn remove_empty_stops_where_the_path_appears() {
    let mut g = Graph::default();
    g.add(1, "R", &[], None);
    g.add(2, "S", &[1], Some("a"));
    g.add(3, "T", &[2], Some("b"));
    let o = WalkOptions {
        remove_empty: true,
        rewrite_parents: true,
        ..prune()
    };
    let mut w = Walk::new(&mut g, o);
    w.push(&id(3), false).unwrap();
    assert_eq!(w.collect_all().unwrap(), [id(3), id(2)]);
    assert_eq!(w.parents(&id(2)).unwrap(), &[] as &[ObjectId]);
}
