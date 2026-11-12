// SPDX-License-Identifier: MIT
//! Differential tests for reading: iteration against `git for-each-ref` and
//! `git show-ref`, resolution against `git symbolic-ref` / `git rev-parse`,
//! name rules against `git check-ref-format`.

mod common;

use common::{Repo, run_git};
use rgit_difftest::assert_same_text;
use rgit_hash::HashKind;
use rgit_refs::files::FilesStore;
use rgit_refs::name::{RefNameFlags, check_branch_name, check_refname_format, normalize_refname};
use rgit_refs::{IterItem, IterOptions, Peeled, RefStore, Target};

fn for_each_ref(store: &FilesStore, opts: &IterOptions) -> String {
    let mut out = String::new();
    for item in store.iter(opts).unwrap() {
        if let IterItem::Ref(r) = item {
            if let Some(oid) = r.oid {
                out.push_str(&format!("{oid} {}\n", r.name));
            }
        }
    }
    out
}

/// `show-ref -d` for a store whose tags are all packed (peeled known).
fn show_ref_d(store: &FilesStore) -> String {
    let mut out = String::new();
    for item in store.iter(&IterOptions::default()).unwrap() {
        if let IterItem::Ref(r) = item {
            let Some(oid) = r.oid else { continue };
            out.push_str(&format!("{oid} {}\n", r.name));
            if let Peeled::Tag(p) = r.peeled {
                out.push_str(&format!("{p} {}^{{}}\n", r.name));
            }
        }
    }
    out
}

fn setup(repo: &Repo, dir: &std::path::Path) {
    let c = repo.c.iter().map(|c| c.to_hex()).collect::<Vec<_>>();
    let cmds: Vec<Vec<&str>> = vec![
        vec!["update-ref", "refs/heads/aa", &c[0]],
        vec!["update-ref", "refs/heads/a-b", &c[1]],
        vec!["update-ref", "refs/heads/a.b", &c[1]],
        vec!["update-ref", "refs/heads/a/b", &c[2]],
        vec!["update-ref", "refs/heads/z", &c[0]],
        vec!["tag", "light", &c[1]],
        vec!["tag", "-a", "-m", "m", "heavy", &c[0]],
        vec!["update-ref", "refs/remotes/o/main", &c[2]],
        vec!["symbolic-ref", "refs/remotes/o/HEAD", "refs/remotes/o/main"],
        vec!["symbolic-ref", "refs/heads/dangling", "refs/heads/nothing"],
        vec!["update-ref", "ORIG_HEAD", &c[1]],
    ];
    for cmd in cmds {
        let out = run_git(repo, dir, &cmd, b"");
        assert!(out.ok, "{cmd:?}: {}", out.stderr);
    }
}

#[test]
fn iteration_matches_for_each_ref() {
    let Some(repo) = Repo::new() else { return };
    let (a, _) = repo.twins("iter");
    setup(&repo, &a);
    let store = FilesStore::new(&a, HashKind::Sha1);
    let check = |label: &str| {
        for prefix in [
            "",
            "refs/heads/",
            "refs/heads/a",
            "refs/tags/",
            "refs/remotes/o/",
        ] {
            let mut args = vec!["for-each-ref", "--format=%(objectname) %(refname)"];
            if !prefix.is_empty() {
                args.push(prefix);
            }
            let git = run_git(&repo, &a, &args, b"");
            assert!(git.ok);
            // for-each-ref matches path components, our prefix is plain.
            let ours = for_each_ref(&store, &IterOptions::prefix(prefix));
            let ours: String = ours
                .lines()
                .filter(|l| {
                    let name = l.split(' ').nth(1).unwrap_or("");
                    prefix.is_empty()
                        || prefix.ends_with('/')
                        || name == prefix
                        || name.starts_with(&format!("{prefix}/"))
                })
                .map(|l| format!("{l}\n"))
                .collect();
            assert_same_text(
                &format!("{label} for-each-ref {prefix}"),
                &git.stdout,
                &ours,
            );
        }
        let git = run_git(
            &repo,
            &a,
            &[
                "for-each-ref",
                "--include-root-refs",
                "--format=%(objectname) %(refname)",
            ],
            b"",
        );
        let opts = IterOptions {
            prefix: String::new(),
            include_root_refs: true,
        };
        assert_same_text(
            &format!("{label} root refs"),
            &git.stdout,
            &for_each_ref(&store, &opts),
        );
    };
    check("loose");
    assert!(run_git(&repo, &a, &["pack-refs", "--all"], b"").ok);
    check("packed");
    // Loose values shadow packed ones.
    assert!(
        run_git(
            &repo,
            &a,
            &["update-ref", "refs/heads/aa", &repo.c[2].to_hex()],
            b""
        )
        .ok
    );
    check("shadowed");
    // show-ref -d with peeled lines from packed-refs.
    let git = run_git(&repo, &a, &["show-ref", "-d"], b"");
    assert_same_text("show-ref -d", &git.stdout, &show_ref_d(&store));
}

#[test]
fn broken_refs_are_reported() {
    let Some(repo) = Repo::new() else { return };
    let (a, _) = repo.twins("broken");
    std::fs::write(a.join("refs/heads/junk"), "junk\n").unwrap();
    std::fs::write(a.join("refs/heads/bad..name"), format!("{}\n", repo.c[0])).unwrap();
    let store = FilesStore::new(&a, HashKind::Sha1);
    let items = store.iter(&IterOptions::default()).unwrap();
    assert!(items.contains(&IterItem::Broken("refs/heads/junk".into())));
    assert!(items.contains(&IterItem::BadName("refs/heads/bad..name".into())));
    let git = run_git(&repo, &a, &["for-each-ref", "--format=%(refname)"], b"");
    assert!(
        git.stderr
            .contains("warning: ignoring broken ref refs/heads/junk")
    );
    assert!(!git.stdout.contains("junk"));
}

#[test]
fn resolution_matches_git() {
    let Some(repo) = Repo::new() else { return };
    let (a, _) = repo.twins("resolve");
    setup(&repo, &a);
    let store = FilesStore::new(&a, HashKind::Sha1);
    for name in [
        "HEAD",
        "ORIG_HEAD",
        "refs/heads/main",
        "refs/remotes/o/HEAD",
        "refs/heads/dangling",
        "refs/heads/missing",
    ] {
        let git = run_git(&repo, &a, &["rev-parse", "--verify", "-q", name], b"");
        let ours = store.resolve_oid(name).unwrap();
        assert_eq!(
            git.ok.then(|| git.stdout.trim().to_owned()),
            ours.map(|o| o.to_hex()),
            "{name}"
        );
        let git = run_git(
            &repo,
            &a,
            &["symbolic-ref", "-q", "--no-recurse", name],
            b"",
        );
        let ours = match store.read_raw(name).unwrap() {
            Some(Target::Symbolic(t)) => Some(t),
            _ => None,
        };
        assert_eq!(git.ok.then(|| git.stdout.trim().to_owned()), ours, "{name}");
    }
    // Unborn branch: HEAD resolves to nothing but names its branch.
    assert!(
        run_git(
            &repo,
            &a,
            &["symbolic-ref", "HEAD", "refs/heads/unborn"],
            b""
        )
        .ok
    );
    let r = store.resolve("HEAD").unwrap().unwrap();
    assert_eq!(r.chain, ["HEAD", "refs/heads/unborn"]);
    assert_eq!(r.oid, None);
}

#[test]
fn packed_refs_round_trip_byte_identical() {
    let Some(repo) = Repo::new() else { return };
    let (a, _) = repo.twins("packed-rt");
    setup(&repo, &a);
    assert!(run_git(&repo, &a, &["pack-refs", "--all"], b"").ok);
    let data = std::fs::read(a.join("packed-refs")).unwrap();
    let parsed = rgit_refs::packed::PackedRefs::parse(&data, HashKind::Sha1).unwrap();
    assert_eq!(parsed.serialize(), data);
    let heavy = parsed.find("refs/tags/heavy").unwrap();
    assert_eq!(heavy.peeled, Some(repo.c[0]));
}

#[test]
fn check_ref_format_matches_git() {
    let Some(repo) = Repo::new() else { return };
    let names = [
        "refs/heads/main",
        "heads/foo",
        "foo",
        "foo/bar/baz",
        "refs///heads/foo",
        "/heads/foo",
        "heads/foo/",
        "./foo",
        "foo/.bar",
        "foo/bar.",
        "foo..bar",
        "foo/bar.lock",
        "foo.lock/bar",
        "foo/@{bar",
        "foo/@bar",
        "@",
        "@/foo",
        "foo/a b",
        "foo/a~b",
        "foo/a^b",
        "foo/a:b",
        "foo/a?b",
        "foo/a[b",
        "foo/a\\b",
        "foo/*",
        "foo/*/bar",
        "foo/b*r",
        "*/foo",
        "foo/*/*",
        "*",
        "foo/\u{e4}",
        "foo/a\u{7f}b",
        "foo/a\tb",
        "",
        "-foo",
        "refs/heads/-foo",
        "HEAD",
        ".",
        "foo/.",
        "foo//bar",
        "//foo/bar",
    ];
    let flag_sets: &[(&[&str], RefNameFlags)] = &[
        (&[], RefNameFlags::STRICT),
        (&["--allow-onelevel"], RefNameFlags::ONELEVEL),
        (
            &["--refspec-pattern"],
            RefNameFlags {
                allow_onelevel: false,
                refspec_pattern: true,
            },
        ),
        (
            &["--allow-onelevel", "--refspec-pattern"],
            RefNameFlags {
                allow_onelevel: true,
                refspec_pattern: true,
            },
        ),
    ];
    let dir = repo.fx.work_dir().join(".git");
    for name in names {
        // A leading dash would be taken as an option.
        for (args, flags) in flag_sets.iter().filter(|_| !name.starts_with('-')) {
            let mut cmd = vec!["check-ref-format"];
            cmd.extend_from_slice(args);
            cmd.push(name);
            let git = run_git(&repo, &dir, &cmd, b"");
            let ours = check_refname_format(name.as_bytes(), *flags).is_ok();
            assert_eq!(git.ok, ours, "check-ref-format {args:?} {name:?}");

            let mut cmd = vec!["check-ref-format", "--normalize"];
            cmd.extend_from_slice(args);
            cmd.push(name);
            let git = run_git(&repo, &dir, &cmd, b"");
            let ours = normalize_refname(name.as_bytes(), *flags)
                .map(|n| format!("{}\n", String::from_utf8(n).unwrap()));
            assert_eq!(
                git.ok.then(|| git.stdout.clone()),
                ours.ok(),
                "check-ref-format --normalize {args:?} {name:?}"
            );
        }
        if !name.starts_with("@{") {
            let git = run_git(&repo, &dir, &["check-ref-format", "--branch", name], b"");
            let ours = check_branch_name(name.as_bytes()).is_ok();
            assert_eq!(git.ok, ours, "check-ref-format --branch {name:?}");
        }
    }
}
