// SPDX-License-Identifier: MIT
//! The revision resolver against an in-memory repository.

use std::collections::{BTreeMap, HashMap};

use rgit_hash::{HashKind, ObjectId, Prefix, hash_object};
use rgit_object::ObjectKind;
use rgit_revwalk::revision::{
    ConfigLookup, Error, Flags, Hint, IndexLookup, Level, LookupError, ObjectLookup, Options,
    RefLookup, RefValue, ReflogEntry, Repo, Resolver,
};

#[derive(Default)]
struct Mem {
    objects: HashMap<ObjectId, (ObjectKind, Vec<u8>)>,
    refs: BTreeMap<String, String>,
    logs: HashMap<String, Vec<ReflogEntry>>,
    config: HashMap<String, Vec<String>>,
    index: HashMap<(Vec<u8>, u8), ObjectId>,
}

impl Mem {
    fn put(&mut self, kind: ObjectKind, data: Vec<u8>) -> ObjectId {
        let id = hash_object(HashKind::Sha1, kind.name(), &data).unwrap();
        self.objects.insert(id, (kind, data));
        id
    }

    fn blob(&mut self, text: &str) -> ObjectId {
        self.put(ObjectKind::Blob, text.as_bytes().to_vec())
    }

    fn tree(&mut self, entries: &[(&str, bool, ObjectId)]) -> ObjectId {
        let mut sorted: Vec<_> = entries.to_vec();
        sorted.sort_by_key(|e| {
            let mut k = e.0.as_bytes().to_vec();
            if e.1 {
                k.push(b'/');
            }
            k
        });
        let mut data = Vec::new();
        for (name, is_tree, id) in sorted {
            data.extend_from_slice(if is_tree { b"40000 " } else { b"100644 " });
            data.extend_from_slice(name.as_bytes());
            data.push(0);
            data.extend_from_slice(id.as_bytes());
        }
        self.put(ObjectKind::Tree, data)
    }

    fn commit(&mut self, tree: ObjectId, parents: &[ObjectId], time: u64, msg: &str) -> ObjectId {
        let mut s = format!("tree {}\n", tree.to_hex());
        for p in parents {
            s.push_str(&format!("parent {}\n", p.to_hex()));
        }
        s.push_str(&format!("author A <a@x> {time} +0000\n"));
        s.push_str(&format!("committer C <c@x> {time} +0000\n\n{msg}\n"));
        self.put(ObjectKind::Commit, s.into_bytes())
    }

    fn tag(&mut self, target: ObjectId, kind: &str, name: &str) -> ObjectId {
        let s = format!(
            "object {}\ntype {kind}\ntag {name}\ntagger T <t@x> 100 +0000\n\nmsg\n",
            target.to_hex()
        );
        self.put(ObjectKind::Tag, s.into_bytes())
    }

    fn set_ref(&mut self, name: &str, value: &str) {
        self.refs.insert(name.to_string(), value.to_string());
    }
}

impl ObjectLookup for Mem {
    fn hash_kind(&self) -> HashKind {
        HashKind::Sha1
    }

    fn read(&self, oid: &ObjectId) -> Result<Option<(ObjectKind, Vec<u8>)>, LookupError> {
        Ok(self.objects.get(oid).cloned())
    }

    fn prefix_matches(&self, prefix: &Prefix) -> Result<Vec<ObjectId>, LookupError> {
        Ok(self
            .objects
            .keys()
            .filter(|id| id.starts_with(prefix))
            .copied()
            .collect())
    }

    fn approximate_count(&self) -> u64 {
        self.objects.len() as u64
    }
}

impl RefLookup for Mem {
    fn resolve(&self, name: &str) -> Result<Option<RefValue>, LookupError> {
        let mut cur = name.to_string();
        for _ in 0..5 {
            match self.refs.get(&cur) {
                None if cur == name => return Ok(None),
                None => {
                    return Ok(Some(RefValue {
                        name: cur,
                        oid: None,
                    }));
                }
                Some(v) => match v.strip_prefix("ref: ") {
                    Some(t) => cur = t.to_string(),
                    None => {
                        let oid = ObjectId::from_hex(HashKind::Sha1, v.as_bytes()).ok();
                        return Ok(Some(RefValue { name: cur, oid }));
                    }
                },
            }
        }
        Ok(None)
    }

    fn for_each_ref(
        &self,
        prefix: &str,
        f: &mut dyn FnMut(&str, &ObjectId),
    ) -> Result<(), LookupError> {
        for name in self.refs.keys().filter(|n| n.starts_with(prefix)) {
            if let Some(RefValue { oid: Some(o), .. }) = self.resolve(name)? {
                f(name, &o);
            }
        }
        Ok(())
    }

    fn reflog(&self, name: &str) -> Result<Option<Vec<ReflogEntry>>, LookupError> {
        Ok(self.logs.get(name).cloned())
    }
}

impl ConfigLookup for Mem {
    fn get(&self, key: &str) -> Option<String> {
        self.config.get(key).and_then(|v| v.last().cloned())
    }

    fn get_all(&self, key: &str) -> Vec<String> {
        self.config.get(key).cloned().unwrap_or_default()
    }
}

impl IndexLookup for Mem {
    fn entry(&self, path: &[u8], stage: u8) -> Option<ObjectId> {
        self.index.get(&(path.to_vec(), stage)).copied()
    }
}

struct Fixture {
    mem: Mem,
    c1: ObjectId,
    c2: ObjectId,
    side: ObjectId,
    merge: ObjectId,
    tree1: ObjectId,
    blob_a: ObjectId,
    tag: ObjectId,
}

/// c1 -- c2 ------ merge (main, HEAD)
///   \            /
///    `-- side --'
fn fixture() -> Fixture {
    let mut m = Mem::default();
    let blob_a = m.blob("a\n");
    let blob_b = m.blob("b\n");
    let sub = m.tree(&[("b", false, blob_b)]);
    let tree1 = m.tree(&[("a", false, blob_a), ("d", true, sub)]);
    let c1 = m.commit(tree1, &[], 100, "one");
    let blob_a2 = m.blob("a2\n");
    let tree2 = m.tree(&[("a", false, blob_a2), ("d", true, sub)]);
    let c2 = m.commit(tree2, &[c1], 200, "two");
    let side = m.commit(tree1, &[c1], 150, "side work");
    let merge = m.commit(tree2, &[c2, side], 300, "merge side");
    let tag = m.tag(c1, "commit", "v1");
    m.set_ref("HEAD", "ref: refs/heads/main");
    m.set_ref("refs/heads/main", &merge.to_hex());
    m.set_ref("refs/heads/side", &side.to_hex());
    m.set_ref("refs/tags/v1", &tag.to_hex());
    m.set_ref("refs/remotes/origin/main", &c2.to_hex());
    m.set_ref("refs/remotes/origin/HEAD", "ref: refs/remotes/origin/main");
    m.config
        .insert("branch.main.remote".into(), vec!["origin".into()]);
    m.config
        .insert("branch.main.merge".into(), vec!["refs/heads/main".into()]);
    m.config.insert(
        "remote.origin.fetch".into(),
        vec!["+refs/heads/*:refs/remotes/origin/*".into()],
    );
    m.logs.insert(
        "refs/heads/main".into(),
        vec![
            ReflogEntry {
                old: HashKind::Sha1.null(),
                new: c1,
                time: 100,
                tz: 0,
                message: "commit (initial): one".into(),
            },
            ReflogEntry {
                old: c1,
                new: merge,
                time: 300,
                tz: 0,
                message: "merge".into(),
            },
        ],
    );
    m.logs.insert(
        "HEAD".into(),
        vec![
            ReflogEntry {
                old: HashKind::Sha1.null(),
                new: c1,
                time: 100,
                tz: 0,
                message: "commit (initial): one".into(),
            },
            ReflogEntry {
                old: c1,
                new: side,
                time: 150,
                tz: 0,
                message: "checkout: moving from main to side".into(),
            },
            ReflogEntry {
                old: side,
                new: merge,
                time: 300,
                tz: 0,
                message: "checkout: moving from side to main".into(),
            },
        ],
    );
    m.index.insert((b"a".to_vec(), 0), blob_a);
    Fixture {
        mem: m,
        c1,
        c2,
        side,
        merge,
        tree1,
        blob_a,
        tag,
    }
}

fn resolver(m: &Mem) -> Resolver<'_> {
    let repo = Repo {
        objects: m,
        refs: m,
        config: m,
        index: Some(m),
        work_tree: None,
        work_tree_info: None,
    };
    Resolver::new(
        repo,
        Options {
            date: rgit_date::DateContext::utc(1000),
            ..Options::default()
        },
    )
}

#[test]
fn names_and_suffixes() {
    let f = fixture();
    let r = resolver(&f.mem);
    let ok = |n: &str| r.resolve(n).unwrap_or_else(|e| panic!("{n}: {e:?}"));
    assert_eq!(ok("HEAD"), f.merge);
    assert_eq!(ok("@"), f.merge);
    assert_eq!(ok("main"), f.merge);
    assert_eq!(ok("heads/main"), f.merge);
    assert_eq!(ok("refs/heads/main"), f.merge);
    assert_eq!(ok("origin"), f.c2);
    assert_eq!(ok("v1"), f.tag);
    assert_eq!(ok("v1^{}"), f.c1);
    assert_eq!(ok("v1^0"), f.c1);
    assert_eq!(ok("v1^{tree}"), f.tree1);
    assert_eq!(ok("HEAD^"), f.c2);
    assert_eq!(ok("HEAD^2"), f.side);
    assert_eq!(ok("HEAD~2"), f.c1);
    assert_eq!(ok("HEAD^2~1"), f.c1);
    assert_eq!(ok("@^^"), f.c1);
    assert_eq!(ok("v1:a"), f.blob_a);
    assert_eq!(ok(":a"), f.blob_a);
    assert_eq!(ok(":0:a"), f.blob_a);
    assert_eq!(ok("HEAD^{/^side}"), f.side);
    assert_eq!(ok(":/^two"), f.c2);
    assert_eq!(ok(&f.c1.to_hex()), f.c1);
    assert_eq!(ok(&f.c1.to_hex()[..8]), f.c1);
    assert_eq!(ok("HEAD@{1}"), f.side);
    assert_eq!(ok("main@{1}"), f.c1);
    assert_eq!(ok("@{-1}"), f.side);
    assert_eq!(ok("@{u}"), f.c2);
    assert_eq!(ok("main@{upstream}"), f.c2);
    for bad in [
        "HEAD^3",
        "HEAD~9",
        "nope",
        "HEAD:nope",
        "v1^{blob}",
        "@{-5}",
        ":nope",
    ] {
        assert_eq!(r.resolve(bad), Err(Error::NotFound), "{bad}");
    }
    assert_eq!(
        r.resolve("main@{5}"),
        Err(Error::Fatal("log for 'main' only has 2 entries".into()))
    );
    assert_eq!(
        r.resolve("side@{u}"),
        Err(Error::Fatal(
            "no upstream configured for branch 'side'".into()
        ))
    );
    assert_eq!(
        r.resolve("nope@{u}"),
        Err(Error::Fatal("no such branch: 'nope'".into()))
    );
}

#[test]
fn diagnostics() {
    let f = fixture();
    let r = resolver(&f.mem);
    assert_eq!(r.resolve("v1^{blob}"), Err(Error::NotFound));
    let d = r.take_diagnostics();
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].level, Level::Error);
    assert_eq!(
        d[0].text,
        "v1^{blob}: expected blob type, but the object dereferences to tree type"
    );
    assert_eq!(
        r.diagnose("HEAD:nope").as_deref(),
        Some("path 'nope' does not exist in 'HEAD'")
    );
    assert_eq!(
        r.diagnose("nope:a").as_deref(),
        Some("invalid object name 'nope'.")
    );
    assert_eq!(
        r.diagnose(":1:a").as_deref(),
        Some("path 'a' is in the index, but not at stage 1")
    );
}

#[test]
fn ranges_and_shorthands() {
    let f = fixture();
    let r = resolver(&f.mem);
    let items = r
        .try_range("side..main", Flags::default())
        .unwrap()
        .unwrap();
    assert_eq!(
        items.iter().map(|i| (i.oid, i.negated)).collect::<Vec<_>>(),
        vec![(f.merge, false), (f.side, true)]
    );
    let items = r.try_range("c..", Flags::default()).unwrap();
    assert!(items.is_none());
    let items = r
        .try_range("side...origin", Flags::default())
        .unwrap()
        .unwrap();
    assert_eq!(
        items.iter().map(|i| (i.oid, i.negated)).collect::<Vec<_>>(),
        vec![(f.c2, false), (f.side, false), (f.c1, true)]
    );
    let items = r
        .try_parent_shorthand("HEAD^@", Flags::default())
        .unwrap()
        .unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[1].name.as_deref(), Some("HEAD^2"));
    let items = r
        .try_parent_shorthand("HEAD^-2", Flags::default())
        .unwrap()
        .unwrap();
    assert_eq!(
        items.iter().map(|i| (i.oid, i.negated)).collect::<Vec<_>>(),
        vec![(f.merge, false), (f.side, true)]
    );
    assert_eq!(r.merge_bases(&f.c2, &f.side).unwrap(), vec![f.c1]);
}

#[test]
fn ref_names() {
    let f = fixture();
    let r = resolver(&f.mem);
    let d = r.dwim_ref("HEAD").unwrap();
    assert_eq!((d.count, d.full_name.as_str()), (1, "refs/heads/main"));
    assert_eq!(
        r.dwim_ref("@{u}").unwrap().full_name,
        "refs/remotes/origin/main"
    );
    assert_eq!(r.dwim_ref("@{-1}").unwrap().full_name, "refs/heads/side");
    assert_eq!(r.shorten_ref("refs/heads/main", true).unwrap(), "main");
    assert_eq!(
        r.shorten_ref("refs/remotes/origin/HEAD", true).unwrap(),
        "origin"
    );
    assert_eq!(r.shorten_ref("refs/tags/v1", false).unwrap(), "v1");
}

#[test]
fn ambiguity_and_abbreviation() {
    let mut m = Mem::default();
    // Find two blobs whose ids share four hex digits.
    let mut seen: HashMap<String, ObjectId> = HashMap::new();
    let mut pair = None;
    for i in 0..5000 {
        let id = m.blob(&format!("blob {i}\n"));
        let p = id.to_hex()[..4].to_string();
        if let Some(other) = seen.insert(p.clone(), id) {
            pair = Some((p, other, id));
            break;
        }
    }
    let (prefix, a, b) = pair.expect("collision");
    let r = resolver(&m);
    assert_eq!(r.resolve(&prefix), Err(Error::NotFound));
    let d = r.take_diagnostics();
    assert_eq!(d[0].text, format!("short object ID {prefix} is ambiguous"));
    assert_eq!(d[1].level, Level::Hint);
    assert!(d[1].text.starts_with("The candidates are:\n  "));
    let quiet = Flags {
        quiet: true,
        ..Flags::default()
    };
    assert_eq!(r.resolve_with(&prefix, quiet), Err(Error::NotFound));
    assert!(r.take_diagnostics().is_empty());
    let blob_hint = Flags {
        hint: Hint::Blob,
        ..Flags::default()
    };
    assert_eq!(r.resolve_with(&prefix, blob_hint), Err(Error::NotFound));
    let la = r.unique_abbrev(&a, 4).unwrap();
    let lb = r.unique_abbrev(&b, 4).unwrap();
    assert!(la.len() > 4 && lb.len() > 4);
    assert_eq!(r.resolve(&la).unwrap(), a);
    assert_eq!(r.default_abbrev(), 7);
}
