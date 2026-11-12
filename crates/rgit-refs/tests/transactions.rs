// SPDX-License-Identifier: MIT
//! Differential tests: `git update-ref --stdin` versus `Transaction`.
//!
//! Each case runs the same commands on two copies of a repository and
//! compares success, the error message and the resulting `refs/` tree,
//! `packed-refs` and root reference files byte for byte.

mod common;

use common::{Cmd, Repo, Step, compare_steps, run_git};
use rgit_hash::ObjectId;
use rgit_refs::files::FilesStore;
use rgit_refs::{OldValue, RefStore};

fn s(cmd: Cmd) -> Step {
    Step {
        cmd,
        no_deref: false,
    }
}

fn nd(cmd: Cmd) -> Step {
    Step {
        cmd,
        no_deref: true,
    }
}

fn r(name: &str) -> String {
    name.to_owned()
}

/// Runs `setup` (git commands) on both twins, then compares `steps`.
fn case(repo: &Repo, label: &str, setup: &[&[&str]], steps: &[Step]) {
    let (a, b) = repo.twins(label);
    for args in setup {
        for dir in [&a, &b] {
            let out = run_git(repo, dir, args, b"");
            assert!(out.ok, "setup {args:?} failed: {}", out.stderr);
        }
    }
    compare_steps(repo, &a, &b, steps);
}

#[test]
fn update_create_delete_verify() {
    let Some(repo) = Repo::new() else { return };
    let [c, d, e] = [repo.c[0], repo.c[1], repo.c[2]];
    let z = ObjectId::null(c.kind());
    let cs = c.to_hex();
    let cs = cs.as_str();
    let ds = d.to_hex();
    let ds = ds.as_str();
    let set_foo: &[&str] = &["update-ref", "refs/p/foo", cs];
    let sym: &[&str] = &["symbolic-ref", "refs/p/symref", "refs/p/foo"];
    let pack: &[&str] = &["pack-refs", "--all"];

    case(
        &repo,
        "plain",
        &[],
        &[s(Cmd::Update(r("refs/heads/x"), c, None))],
    );
    case(
        &repo,
        "create-nested",
        &[],
        &[s(Cmd::Create(r("refs/a/b/c/d"), c))],
    );
    case(
        &repo,
        "missing-old",
        &[],
        &[s(Cmd::Update(r("refs/p/foo"), e, Some(d)))],
    );
    case(
        &repo,
        "wrong-old",
        &[set_foo],
        &[s(Cmd::Update(r("refs/p/foo"), e, Some(d)))],
    );
    case(
        &repo,
        "exists",
        &[set_foo],
        &[s(Cmd::Create(r("refs/p/foo"), e))],
    );
    case(
        &repo,
        "wrong-old-delete",
        &[set_foo],
        &[s(Cmd::Delete(r("refs/p/foo"), Some(d)))],
    );
    case(
        &repo,
        "delete-loose",
        &[set_foo],
        &[s(Cmd::Delete(r("refs/p/foo"), Some(c)))],
    );
    case(
        &repo,
        "delete-packed",
        &[set_foo, pack],
        &[s(Cmd::Delete(r("refs/p/foo"), None))],
    );
    case(
        &repo,
        "delete-both",
        &[set_foo, pack, &["update-ref", "refs/p/foo", ds]],
        &[s(Cmd::Delete(r("refs/p/foo"), Some(d)))],
    );
    case(
        &repo,
        "update-packed",
        &[set_foo, pack],
        &[s(Cmd::Update(r("refs/p/foo"), c, None))],
    );
    case(
        &repo,
        "delete-missing",
        &[],
        &[s(Cmd::Delete(r("refs/p/nope"), None))],
    );
    case(
        &repo,
        "delete-missing-old",
        &[],
        &[s(Cmd::Delete(r("refs/p/nope"), Some(c)))],
    );
    case(
        &repo,
        "verify-ok",
        &[set_foo],
        &[s(Cmd::Verify(r("refs/p/foo"), Some(c)))],
    );
    case(
        &repo,
        "verify-absent",
        &[set_foo],
        &[s(Cmd::Verify(r("refs/p/foo"), None))],
    );
    case(
        &repo,
        "verify-missing",
        &[],
        &[s(Cmd::Verify(r("refs/p/x"), Some(c)))],
    );
    case(
        &repo,
        "update-zero-old",
        &[set_foo],
        &[s(Cmd::Update(r("refs/p/foo"), d, Some(z)))],
    );
    case(
        &repo,
        "dup",
        &[],
        &[
            s(Cmd::Update(r("refs/heads/a"), c, None)),
            s(Cmd::Update(r("refs/heads/a"), d, None)),
        ],
    );
    case(
        &repo,
        "dup-via-head",
        &[],
        &[
            s(Cmd::Update(r("HEAD"), c, None)),
            s(Cmd::Update(r("refs/heads/main"), d, None)),
        ],
    );
    case(
        &repo,
        "dup-via-referent",
        &[],
        &[
            s(Cmd::Update(r("refs/heads/main"), c, None)),
            s(Cmd::Update(r("HEAD"), d, None)),
        ],
    );
    case(
        &repo,
        "head-detach",
        &[],
        &[nd(Cmd::Update(r("HEAD"), c, None))],
    );
    case(
        &repo,
        "head-through",
        &[],
        &[s(Cmd::Update(r("HEAD"), c, Some(e)))],
    );
    case(
        &repo,
        "root-ref",
        &[],
        &[s(Cmd::Update(r("ORIG_HEAD"), c, None))],
    );
    case(&repo, "onelevel", &[], &[s(Cmd::Update(r("foo"), c, None))]);
    case(
        &repo,
        "head-slash",
        &[],
        &[s(Cmd::Create(r("HEAD/foo"), c))],
    );
    case(
        &repo,
        "atomic",
        &[set_foo],
        &[
            s(Cmd::Create(r("refs/p/new"), c)),
            s(Cmd::Update(r("refs/p/foo"), e, Some(d))),
        ],
    );

    // Symbolic references (t1404 "indirect").
    case(
        &repo,
        "ind-missing",
        &[sym],
        &[s(Cmd::Update(r("refs/p/symref"), e, Some(d)))],
    );
    case(
        &repo,
        "ind-wrong",
        &[sym, set_foo],
        &[s(Cmd::Update(r("refs/p/symref"), e, Some(d)))],
    );
    case(
        &repo,
        "ind-exists",
        &[sym, set_foo],
        &[s(Cmd::Create(r("refs/p/symref"), e))],
    );
    case(
        &repo,
        "ind-delete",
        &[sym, set_foo],
        &[s(Cmd::Delete(r("refs/p/symref"), Some(d)))],
    );
    case(
        &repo,
        "nd-missing",
        &[sym],
        &[nd(Cmd::Update(r("refs/p/symref"), e, Some(d)))],
    );
    case(
        &repo,
        "nd-wrong",
        &[sym, set_foo],
        &[nd(Cmd::Update(r("refs/p/symref"), e, Some(d)))],
    );
    case(
        &repo,
        "nd-exists",
        &[sym, set_foo],
        &[nd(Cmd::Create(r("refs/p/symref"), e))],
    );
    case(
        &repo,
        "nd-delete",
        &[sym, set_foo],
        &[nd(Cmd::Delete(r("refs/p/symref"), Some(c)))],
    );
    case(
        &repo,
        "nd-dangling-create",
        &[sym],
        &[nd(Cmd::Create(r("refs/p/symref"), e))],
    );
    case(
        &repo,
        "nd-dangling-update",
        &[sym],
        &[nd(Cmd::Update(r("refs/p/symref"), e, None))],
    );
    case(
        &repo,
        "dangling-create",
        &[sym],
        &[s(Cmd::Create(r("refs/p/symref"), e))],
    );
    case(
        &repo,
        "dangling-delete",
        &[sym],
        &[s(Cmd::Delete(r("refs/p/symref"), None))],
    );
    case(
        &repo,
        "symref-loop",
        &[
            &["symbolic-ref", "refs/p/s1", "refs/p/s2"],
            &["symbolic-ref", "refs/p/s2", "refs/p/s1"],
        ],
        &[s(Cmd::Update(r("refs/p/s1"), c, None))],
    );
}

#[test]
fn symref_commands() {
    let Some(repo) = Repo::new() else { return };
    let c = repo.c[0];
    let sym: &[&str] = &["symbolic-ref", "refs/p/symref", "refs/heads/main"];
    let dangling: &[&str] = &["symbolic-ref", "refs/p/symref", "refs/p/none"];
    let main = || r("refs/heads/main");
    let symref = || r("refs/p/symref");

    case(
        &repo,
        "su-create",
        &[],
        &[nd(Cmd::SymrefUpdate(symref(), main(), None))],
    );
    case(
        &repo,
        "su-create-deref",
        &[],
        &[s(Cmd::SymrefUpdate(symref(), main(), None))],
    );
    case(
        &repo,
        "su-deref-existing",
        &[sym],
        &[s(Cmd::SymrefUpdate(symref(), r("refs/p/x"), None))],
    );
    case(
        &repo,
        "su-right-old",
        &[sym],
        &[nd(Cmd::SymrefUpdate(
            symref(),
            r("refs/p/x"),
            Some(OldValue::Symbolic(main())),
        ))],
    );
    case(
        &repo,
        "su-wrong-old",
        &[sym],
        &[nd(Cmd::SymrefUpdate(
            symref(),
            r("refs/p/x"),
            Some(OldValue::Symbolic(r("refs/p/y"))),
        ))],
    );
    case(
        &repo,
        "su-zero-oid",
        &[sym],
        &[s(Cmd::SymrefUpdate(
            symref(),
            r("refs/p/x"),
            Some(OldValue::Object(ObjectId::null(c.kind()))),
        ))],
    );
    case(
        &repo,
        "su-regular-oid",
        &[&["update-ref", "refs/p/reg", &c.to_hex()]],
        &[s(Cmd::SymrefUpdate(
            r("refs/p/reg"),
            main(),
            Some(OldValue::Object(c)),
        ))],
    );
    case(
        &repo,
        "su-regular-wrong-oid",
        &[&["update-ref", "refs/p/reg", &c.to_hex()]],
        &[s(Cmd::SymrefUpdate(
            r("refs/p/reg"),
            main(),
            Some(OldValue::Object(repo.c[1])),
        ))],
    );
    case(
        &repo,
        "su-dangling",
        &[dangling],
        &[nd(Cmd::SymrefUpdate(symref(), main(), None))],
    );
    case(
        &repo,
        "su-dangling-old",
        &[dangling],
        &[nd(Cmd::SymrefUpdate(
            symref(),
            main(),
            Some(OldValue::Symbolic(r("refs/p/none"))),
        ))],
    );
    case(
        &repo,
        "su-dangling-wrong-old",
        &[dangling],
        &[nd(Cmd::SymrefUpdate(
            symref(),
            main(),
            Some(OldValue::Symbolic(r("refs/p/other"))),
        ))],
    );
    case(
        &repo,
        "sc-new",
        &[],
        &[nd(Cmd::SymrefCreate(symref(), main()))],
    );
    case(
        &repo,
        "sc-onelevel",
        &[],
        &[nd(Cmd::SymrefCreate(symref(), r("bogus")))],
    );
    case(
        &repo,
        "sc-exists",
        &[sym],
        &[nd(Cmd::SymrefCreate(symref(), r("refs/p/x")))],
    );
    case(
        &repo,
        "sc-regular",
        &[],
        &[nd(Cmd::SymrefCreate(main(), r("refs/p/x")))],
    );
    case(
        &repo,
        "sd-right",
        &[sym],
        &[nd(Cmd::SymrefDelete(symref(), Some(main())))],
    );
    case(
        &repo,
        "sd-wrong",
        &[sym],
        &[nd(Cmd::SymrefDelete(symref(), Some(r("refs/p/x"))))],
    );
    case(
        &repo,
        "sd-regular",
        &[],
        &[nd(Cmd::SymrefDelete(main(), Some(r("refs/p/x"))))],
    );
    case(
        &repo,
        "sd-regular-noold",
        &[],
        &[nd(Cmd::SymrefDelete(main(), None))],
    );
    case(
        &repo,
        "sd-missing",
        &[],
        &[nd(Cmd::SymrefDelete(r("refs/p/nope"), None))],
    );
    case(
        &repo,
        "sv-right",
        &[sym],
        &[nd(Cmd::SymrefVerify(symref(), Some(main())))],
    );
    case(
        &repo,
        "sv-wrong",
        &[sym],
        &[nd(Cmd::SymrefVerify(symref(), Some(r("refs/p/x"))))],
    );
    case(
        &repo,
        "sv-absent",
        &[sym],
        &[nd(Cmd::SymrefVerify(symref(), None))],
    );
    case(
        &repo,
        "sv-missing",
        &[],
        &[nd(Cmd::SymrefVerify(r("refs/p/missing"), Some(main())))],
    );
    case(
        &repo,
        "sv-missing-absent",
        &[],
        &[nd(Cmd::SymrefVerify(r("refs/p/missing"), None))],
    );
    case(
        &repo,
        "sv-dangling",
        &[dangling],
        &[nd(Cmd::SymrefVerify(symref(), Some(r("refs/p/none"))))],
    );
    case(
        &repo,
        "sv-regular",
        &[],
        &[nd(Cmd::SymrefVerify(main(), None))],
    );
    case(
        &repo,
        "sv-regular-target",
        &[],
        &[nd(Cmd::SymrefVerify(main(), Some(r("refs/p/x"))))],
    );
    case(
        &repo,
        "head-symref",
        &[],
        &[nd(Cmd::SymrefUpdate(
            r("HEAD"),
            r("refs/heads/other"),
            None,
        ))],
    );
}

/// D/F conflicts (t1404).
#[test]
fn directory_file_conflicts() {
    let Some(repo) = Repo::new() else { return };
    let c = repo.c[0];
    let d = repo.c[1];
    let cs = c.to_hex();
    let pack: &[&str] = &["pack-refs", "--all"];
    for packed in [false, true] {
        let p = if packed { "p-" } else { "l-" };
        let mk = |name: &str| -> Vec<String> { vec!["update-ref".into(), name.into(), cs.clone()] };
        let combos: &[(&str, &[&str])] = &[
            ("prefix-of-new", &["refs/df/c"]),
            ("new-is-prefix", &["refs/df/c/x"]),
            ("deep", &["refs/df/c/x/y"]),
        ];
        for (label, before) in combos {
            for new in ["refs/df/c", "refs/df/c/x", "refs/df/c/x/y", "refs/df/c/z"] {
                let setup: Vec<Vec<String>> = before.iter().map(|n| mk(n)).collect();
                let mut setup_refs: Vec<Vec<&str>> = setup
                    .iter()
                    .map(|v| v.iter().map(String::as_str).collect())
                    .collect();
                if packed {
                    setup_refs.push(pack.to_vec());
                }
                let setup_slices: Vec<&[&str]> = setup_refs.iter().map(Vec::as_slice).collect();
                let tag = format!("{p}{label}-{}", new.replace('/', "_"));
                case(&repo, &tag, &setup_slices, &[s(Cmd::Create(r(new), d))]);
            }
        }
        // Add and delete conflicting names in one transaction.
        for (del, add) in [
            ("refs/df/foo", "refs/df/foo/bar"),
            ("refs/df/foo/bar", "refs/df/foo"),
        ] {
            let setup = mk(del);
            let mut setup_refs: Vec<&[&str]> = Vec::new();
            let setup_slice: Vec<&str> = setup.iter().map(String::as_str).collect();
            setup_refs.push(&setup_slice);
            if packed {
                setup_refs.push(pack);
            }
            let tag = format!("{p}del-add-{}", del.replace('/', "_"));
            case(
                &repo,
                &tag,
                &setup_refs,
                &[s(Cmd::Delete(r(del), None)), s(Cmd::Create(r(add), d))],
            );
            let tag = format!("{p}add-del-{}", del.replace('/', "_"));
            case(
                &repo,
                &tag,
                &setup_refs,
                &[s(Cmd::Create(r(add), d)), s(Cmd::Delete(r(del), None))],
            );
        }
    }
    // Two new names of one transaction.
    case(
        &repo,
        "txn-short-long",
        &[],
        &[
            s(Cmd::Create(r("refs/q/a"), c)),
            s(Cmd::Create(r("refs/q/a/b"), c)),
        ],
    );
    case(
        &repo,
        "txn-long-short",
        &[],
        &[
            s(Cmd::Create(r("refs/q/a/b"), c)),
            s(Cmd::Create(r("refs/q/a"), c)),
        ],
    );
    // Empty directories in the way are removed, others block.
    let (a, b) = repo.twins("empty-dir");
    for dir in [&a, &b] {
        std::fs::create_dir_all(dir.join("refs/heads/emp/a/b")).unwrap();
    }
    compare_steps(&repo, &a, &b, &[s(Cmd::Create(r("refs/heads/emp"), c))]);
    let (a, b) = repo.twins("blocking-dir");
    for dir in [&a, &b] {
        std::fs::create_dir_all(dir.join("refs/heads/full/a")).unwrap();
        std::fs::write(dir.join("refs/heads/full/a/junk.lock"), "").unwrap();
    }
    compare_steps(&repo, &a, &b, &[s(Cmd::Create(r("refs/heads/full"), c))]);
    // Pruning of empty parent directories after deletion.
    case(
        &repo,
        "prune",
        &[
            &["update-ref", "refs/heads/deep/a/b/c", &cs],
            &["update-ref", "refs/top/x", &cs],
        ],
        &[
            s(Cmd::Delete(r("refs/heads/deep/a/b/c"), None)),
            s(Cmd::Delete(r("refs/top/x"), None)),
        ],
    );
}

#[test]
fn lock_conflicts_leave_nothing_behind() {
    let Some(repo) = Repo::new() else { return };
    let c = repo.c[0];
    let cs = c.to_hex();
    let (a, b) = repo.twins("ref-lock");
    for dir in [&a, &b] {
        std::fs::write(dir.join("refs/heads/main.lock"), "").unwrap();
    }
    compare_steps(
        &repo,
        &a,
        &b,
        &[
            s(Cmd::Create(r("refs/heads/new/x"), c)),
            s(Cmd::Update(r("refs/heads/main"), c, None)),
        ],
    );
    let (a, b) = repo.twins("head-lock");
    for dir in [&a, &b] {
        std::fs::write(dir.join("HEAD.lock"), "").unwrap();
    }
    compare_steps(
        &repo,
        &a,
        &b,
        &[s(Cmd::Update(r("refs/heads/main"), c, None))],
    );
    let (a, b) = repo.twins("packed-lock");
    for dir in [&a, &b] {
        assert!(run_git(&repo, dir, &["update-ref", "refs/t/x", &cs], b"").ok);
        assert!(run_git(&repo, dir, &["pack-refs", "--all"], b"").ok);
        std::fs::write(dir.join("packed-refs.lock"), "").unwrap();
    }
    compare_steps(&repo, &a, &b, &[s(Cmd::Delete(r("refs/t/x"), None))]);
}

/// Deterministic xorshift generator (no external crates).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// "Done when": 200 random transactions, state compared after each.
#[test]
fn random_transactions() {
    let Some(repo) = Repo::new() else { return };
    let seed = std::env::var("RGIT_REFS_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x5eed_1234_u64);
    let count: usize = std::env::var("RGIT_REFS_TXNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);
    let mut rng = Rng(seed);
    // HEAD must stay a valid symbolic reference for git to accept the
    // repository: it is only updated through (deref), never replaced.
    let symrefs = ["ORIG_HEAD", "refs/heads/b", "refs/s/sym", "refs/s/sym2"];
    let names = [
        "ORIG_HEAD",
        "refs/heads/main",
        "refs/heads/a",
        "refs/heads/a/b",
        "refs/heads/a/b/c",
        "refs/heads/b",
        "refs/heads/x/y/z",
        "refs/tags/t1",
        "refs/tags/t1/v",
        "refs/remotes/o/main",
        "refs/s/sym",
        "refs/s/sym2",
    ];
    let mut oids = repo.c.clone();
    oids.push(repo.tag);
    let null = ObjectId::null(oids[0].kind());
    let (a, b) = repo.twins("random");
    let store = FilesStore::new(&b, oids[0].kind());
    let mut ok = 0;
    for i in 0..count {
        if rng.below(10) == 0 {
            let args: &[&str] = if rng.below(2) == 0 {
                &["pack-refs", "--all"]
            } else {
                &["pack-refs", "--all", "--prune"]
            };
            // pack-refs is out of scope (#103): git packs both twins.
            let ra = run_git(&repo, &a, args, b"");
            let rb = run_git(&repo, &b, args, b"");
            assert_eq!(ra.ok, rb.ok);
        }
        let n = 1 + rng.below(2);
        let mut steps = Vec::new();
        for _ in 0..n {
            let name = r(rng.pick(&names));
            // Symbolic references are only created at `symrefs` and only
            // point at the other names, so chains have one level. git's
            // handling of longer chains and cycles depends on the order of
            // the names involved and is not reproduced (see the PR).
            let sym_name = r(rng.pick(&symrefs));
            let regular: Vec<&str> = names
                .iter()
                .copied()
                .filter(|n| !symrefs.contains(n))
                .collect();
            let later = regular.as_slice();
            // The tag (a non-commit) mostly goes to tags; branches reject it.
            let oid = if name.starts_with("refs/tags/") || rng.below(8) == 0 {
                *rng.pick(&oids)
            } else {
                *rng.pick(&repo.c)
            };
            // Mostly the right old value, so that many transactions succeed.
            let current = store.resolve_oid(&name).ok().flatten().unwrap_or(null);
            let maybe_old = |rng: &mut Rng| -> Option<ObjectId> {
                match rng.below(6) {
                    0 | 1 => None,
                    2 | 3 => Some(current),
                    4 => Some(null),
                    _ => Some(*rng.pick(&oids)),
                }
            };
            let kind = rng.below(12);
            let cmd = match kind {
                0..=3 => Cmd::Update(name, oid, maybe_old(&mut rng)),
                4 => Cmd::Create(name, oid),
                5 | 6 => {
                    let old = maybe_old(&mut rng).filter(|o| !o.is_null());
                    Cmd::Delete(name, old)
                }
                7 => Cmd::Verify(name, maybe_old(&mut rng)),
                8 | 9 => {
                    let target = r(rng.pick(later));
                    let old = match rng.below(3) {
                        0 => None,
                        1 => Some(OldValue::Symbolic(r(rng.pick(later)))),
                        _ => maybe_old(&mut rng).map(OldValue::Object),
                    };
                    Cmd::SymrefUpdate(sym_name, target, old)
                }
                10 => Cmd::SymrefCreate(sym_name, r(rng.pick(later))),
                _ => Cmd::SymrefDelete(sym_name, (rng.below(2) == 0).then(|| r(rng.pick(&names)))),
            };
            let no_deref = matches!(
                cmd,
                Cmd::SymrefDelete(..) | Cmd::SymrefUpdate(..) | Cmd::SymrefCreate(..)
            ) || rng.below(3) == 0;
            steps.push(Step { cmd, no_deref });
            if rng.below(8) == 0 {
                let cmd = match rng.below(2) {
                    0 => Cmd::Update(r("HEAD"), *rng.pick(&oids), None),
                    _ => Cmd::Verify(r("HEAD"), Some(*rng.pick(&oids))),
                };
                steps.push(Step {
                    cmd,
                    no_deref: false,
                });
            }
        }
        if compare_steps(&repo, &a, &b, &steps) {
            ok += 1;
        }
        let _ = i;
    }
    // Make sure the generator exercises successful transactions too.
    assert!(
        ok * 5 > count,
        "only {ok} of {count} transactions succeeded"
    );
    eprintln!("{ok} of {count} random transactions succeeded");
}
