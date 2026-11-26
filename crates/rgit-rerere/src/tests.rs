// SPDX-License-Identifier: MIT
//! Scenarios of the rerere pass on an in-memory work tree; the expected
//! states were observed with the `git` binary.

use std::collections::BTreeMap;

use super::*;

#[derive(Default)]
struct Fake {
    files: BTreeMap<Vec<u8>, Vec<u8>>,
    conflicts: Vec<Conflicted>,
    out: Vec<String>,
    staged: Vec<Vec<u8>>,
}

impl Host for Fake {
    fn conflicts(&mut self) -> Vec<Conflicted> {
        self.conflicts.clone()
    }
    fn read(&mut self, path: &[u8]) -> std::io::Result<Vec<u8>> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
    }
    fn write(&mut self, path: &[u8], data: &[u8]) -> std::io::Result<()> {
        self.files.insert(path.to_vec(), data.to_vec());
        Ok(())
    }
    fn marker_size(&mut self, _: &[u8]) -> usize {
        7
    }
    fn stage(&mut self, paths: &[Vec<u8>]) -> Result<(), String> {
        self.staged.extend_from_slice(paths);
        Ok(())
    }
    fn report(&mut self, line: &str) {
        self.out.push(line.to_owned());
    }
}

const CONFLICT: &[u8] = b"1\n2\n3\n<<<<<<< HEAD\nM\n=======\nX\n>>>>>>> x\n5\n";
const RESOLVED: &[u8] = b"1\n2\n3\nM\nX\n5\n";
const ID: &str = "7d6565ce286284f27e5f9357c14f612faa07c542";

fn setup() -> (tempfile::TempDir, Cache, Fake) {
    let t = tempfile::tempdir().unwrap();
    let cache = Cache::new(t.path().join("rr-cache"));
    let mut f = Fake::default();
    for p in ["f1", "f2"] {
        f.files.insert(p.into(), CONFLICT.to_vec());
        f.conflicts.push(Conflicted {
            path: p.into(),
            trackable: true,
        });
    }
    (t, cache, f)
}

fn files(cache: &Cache) -> Vec<String> {
    let mut out = Vec::new();
    for d in std::fs::read_dir(cache.root()).unwrap().flatten() {
        for f in std::fs::read_dir(d.path()).unwrap().flatten() {
            out.push(f.file_name().to_string_lossy().into_owned());
        }
    }
    out.sort();
    out
}

#[test]
fn record_then_replay_from_new_resolution() {
    let (_t, cache, mut f) = setup();
    let mut rr = Vec::new();
    rerere(&cache, &mut rr, &mut f, HashKind::Sha1, false).unwrap();
    assert_eq!(
        f.out,
        ["Recorded preimage for 'f1'", "Recorded preimage for 'f2'"]
    );
    // The conflict id is the one git computes for this conflict (the sides
    // of the hunk, each followed by a NUL).
    assert_eq!(rr[0].id, ID);
    assert_eq!((rr[0].variant, rr[1].variant), (0, 1));

    // f1 resolved by hand: its resolution is recorded and replayed on f2,
    // whose own unresolved variant goes away.
    f.out.clear();
    f.files.insert(b"f1".to_vec(), RESOLVED.to_vec());
    rerere(&cache, &mut rr, &mut f, HashKind::Sha1, false).unwrap();
    assert_eq!(
        f.out,
        [
            "Recorded resolution for 'f1'.",
            "Resolved 'f2' using previous resolution."
        ]
    );
    assert!(rr.is_empty());
    assert_eq!(f.files[&b"f2"[..]], RESOLVED);
    assert_eq!(files(&cache), ["postimage", "preimage", "thisimage"]);
}

#[test]
fn still_conflicted_is_recorded_again_and_autoupdate_stages() {
    let (_t, cache, mut f) = setup();
    let mut rr = Vec::new();
    rerere(&cache, &mut rr, &mut f, HashKind::Sha1, false).unwrap();
    f.out.clear();
    rerere(&cache, &mut rr, &mut f, HashKind::Sha1, false).unwrap();
    assert_eq!(
        f.out,
        ["Recorded preimage for 'f1'", "Recorded preimage for 'f2'"]
    );
    f.files.insert(b"f1".to_vec(), RESOLVED.to_vec());
    f.files.insert(b"f2".to_vec(), RESOLVED.to_vec());
    rerere(&cache, &mut rr, &mut f, HashKind::Sha1, false).unwrap();

    // A new merge: both resolved from variant 0 and staged.
    let (_t2, _, mut g) = setup();
    g.out.clear();
    let mut rr = Vec::new();
    rerere(&cache, &mut rr, &mut g, HashKind::Sha1, true).unwrap();
    assert_eq!(
        g.out,
        [
            "Staged 'f1' using previous resolution.",
            "Staged 'f2' using previous resolution."
        ]
    );
    assert_eq!(g.staged.len(), 2);

    // forget f2: variant 0 loses its resolution, f2 is recorded again.
    let mut rr = Vec::new();
    forget(
        &cache,
        &mut rr,
        &mut g,
        HashKind::Sha1,
        &[(b"f2".to_vec(), CONFLICT.to_vec())],
    )
    .unwrap();
    assert_eq!(rr.len(), 1);
    assert_eq!(rr[0].variant, 0);
    assert!(!cache.has(ID, 0, Image::Post));
    assert!(cache.has(ID, 1, Image::Post));

    // clear drops the unresolved variant 0.
    clear(&cache, &rr);
    assert!(!cache.has(ID, 0, Image::Pre));
    assert!(cache.has(ID, 1, Image::Pre));
}

#[test]
fn parse_errors_and_untrackable_paths() {
    let (_t, cache, mut f) = setup();
    f.files.insert(b"f2".to_vec(), b"<<<<<<< a\nfoo\n".to_vec());
    f.conflicts.push(Conflicted {
        path: b"sl".to_vec(),
        trackable: false,
    });
    let mut rr = Vec::new();
    rerere(&cache, &mut rr, &mut f, HashKind::Sha1, false).unwrap();
    assert_eq!(
        f.out,
        [
            "error: could not parse conflict hunks in 'f2'",
            "Recorded preimage for 'f1'"
        ]
    );
    let conflicts = f.conflicts.clone();
    assert_eq!(remaining(&rr, &conflicts), [b"f1".to_vec(), b"sl".to_vec()]);
    assert_eq!(status(&rr), [b"f1".to_vec()]);
    let d = diff(&cache, &rr, &mut f);
    assert!(d.starts_with(b"--- a/f1\n+++ b/f1\n@@ -1,9 +1,9 @@\n 1\n"));
}

#[test]
fn gc_by_age() {
    let t = tempfile::tempdir().unwrap();
    let cache = Cache::new(t.path());
    let a = "1".repeat(40);
    let b = "2".repeat(40);
    cache.write(&a, 0, Image::Pre, b"x").unwrap();
    cache.write(&a, 0, Image::Post, b"x").unwrap();
    cache.write(&a, 1, Image::Pre, b"x").unwrap();
    cache.write(&a, 1, Image::This, b"x").unwrap();
    cache.write(&b, 0, Image::Pre, b"x").unwrap();
    std::fs::create_dir(t.path().join("notanid")).unwrap();
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    // Resolved ones are young enough, unresolved ones are not.
    gc(&cache, HashKind::Sha1, now - 1000, now + 1000);
    assert!(cache.has(&a, 0, Image::Post));
    assert!(!cache.has(&a, 1, Image::Pre));
    assert!(!cache.has(&a, 1, Image::This));
    assert!(!cache.dir(&b).exists());
    assert!(t.path().join("notanid").exists());
}
