// SPDX-License-Identifier: MIT
//! Reachability bitmaps against the reference git: MIDX bitmaps written by
//! rgit are byte-identical, git accepts the pack bitmaps rgit writes
//! (`rev-list --test-bitmap`), and `rev-list --use-bitmap-index` answers
//! like git from git's bitmaps.

use std::path::Path;
use std::process::Command;

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"))
}

/// Two branches with a merge and a tag, `n` commits on the main line.
fn history(n: usize) -> Fixture {
    let mut fx =
        fixture()
            .git(["init", "-q", "-b", "main"])
            .git(["config", "maintenance.auto", "false"]);
    for c in 0..n {
        if c == n / 2 {
            fx = fx.git(["checkout", "-q", "-b", "side", "HEAD~3"]);
        }
        if c == n / 2 + 4 {
            fx = fx
                .git(["checkout", "-q", "main"])
                .git(["merge", "-q", "--no-edit", "side"]);
        }
        fx = fx
            .write(format!("d{}/f{}.txt", c % 3, c % 5), format!("line {c}\n"))
            .git(["add", "-A"])
            .git(["commit", "-q", "-m", &format!("c{c}")])
            .tick();
    }
    fx.git(["tag", "-a", "-m", "t", "v1", "HEAD~2"])
}

/// Run `bin` with `args` in `dir` with the fixture's isolated environment.
fn run(fx: &Fixture, bin: &Path, dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin)
        .current_dir(dir)
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", fx.home_dir())
        .env("XDG_CONFIG_HOME", fx.home_dir())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
}

fn copy_dir(from: &Path, to: &Path) {
    let st = Command::new("cp")
        .arg("-a")
        .arg(from)
        .arg(to)
        .status()
        .unwrap();
    assert!(st.success());
}

#[test]
fn rev_list_with_git_bitmap() {
    let mut fx = history(30).git(["repack", "-adbq"]);
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["rev-list", "--use-bitmap-index", "HEAD"][..],
        &["rev-list", "--use-bitmap-index", "--objects", "HEAD"],
        &["rev-list", "--use-bitmap-index", "--count", "HEAD"],
        &[
            "rev-list",
            "--use-bitmap-index",
            "--count",
            "--objects",
            "--all",
        ],
        &[
            "rev-list",
            "--use-bitmap-index",
            "--objects",
            "HEAD~5..HEAD",
        ],
        &["rev-list", "--use-bitmap-index", "--objects", "v1"],
        &[
            "rev-list",
            "--use-bitmap-index",
            "--disk-usage",
            "--objects",
            "HEAD",
        ],
        &["rev-list", "--use-bitmap-index", "--count", "main...side"],
    ] {
        fx.compare(args)
            .checks([Check::ExitCode, Check::Stdout])
            .run();
    }
    // Objects outside the bitmapped pack come after it.
    let mut fx = fx
        .write("new.txt", "new\n")
        .git(["add", "new.txt"])
        .git(["commit", "-q", "-m", "new"]);
    fx.compare(["rev-list", "--use-bitmap-index", "--objects", "HEAD"])
        .checks([Check::ExitCode, Check::Stdout])
        .run();
    fx.compare(["rev-list", "--test-bitmap", "HEAD~1"])
        .checks([Check::ExitCode])
        .run();
}

/// `--filter` with `--use-bitmap-index`: the same objects and order as
/// git, for objects in the bitmapped pack and after it, with the named
/// objects kept unless `--filter-provided-objects` is given.
#[test]
fn rev_list_bitmap_with_filters() {
    let fx = history(12).git(["repack", "-adbq"]);
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx
        .write("big.txt", "a much larger blob\n")
        .write("small", "s\n")
        .git(["add", "-A"])
        .git(["commit", "-q", "-m", "after"])
        .tick()
        .write("d0/f9.txt", "again\n")
        .git(["add", "-A"])
        .git(["commit", "-q", "-m", "after 2"]);
    let filters = [
        "blob:none",
        "blob:limit=5",
        "tree:0",
        "tree:1",
        "object:type=commit",
        "object:type=tree",
        "object:type=blob",
        "object:type=tag",
        "combine:blob:limit=10+object:type=blob",
    ];
    let tips: [&[&str]; 6] = [
        &["HEAD"],
        &["HEAD", "HEAD:big.txt"],
        &["v1", "HEAD^{tree}", "HEAD:big.txt"],
        &["HEAD", "HEAD^{tree}", "HEAD:big.txt"],
        &["HEAD~1", "HEAD~1^{tree}", "HEAD~5^{tree}"],
        &["HEAD", "^HEAD~4"],
    ];
    for f in filters {
        let filter = format!("--filter={f}");
        for t in tips {
            for provided in [false, true] {
                let mut args = vec!["rev-list", "--use-bitmap-index", "--objects", &filter];
                if provided {
                    args.push("--filter-provided-objects");
                }
                args.extend_from_slice(t);
                fx.compare(&args)
                    .checks([Check::ExitCode, Check::Stdout])
                    .run();
            }
        }
    }
    fx.compare([
        "rev-list",
        "--use-bitmap-index",
        "--objects",
        "--filter=blob:none",
        "--count",
        "HEAD",
    ])
    .checks([Check::ExitCode, Check::Stdout])
    .run();
}

#[test]
fn git_accepts_rgit_pack_bitmap() {
    let fx = history(40);
    if fx.is_skipped() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let ours = tmp.path().join("ours");
    copy_dir(&fx.work_dir(), &ours);
    let rgit = Path::new(env!("CARGO_BIN_EXE_rgit"));
    let out = run(&fx, rgit, &ours, &["repack", "-adbq"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bitmaps: Vec<_> = std::fs::read_dir(ours.join(".git/objects/pack"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".bitmap"))
        .collect();
    assert_eq!(bitmaps.len(), 1);
    let out = run(
        &fx,
        fx.reference_git(),
        &ours,
        &["rev-list", "--test-bitmap", "HEAD"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // And rgit reads it back like git.
    let a = run(
        &fx,
        fx.reference_git(),
        &ours,
        &["rev-list", "--use-bitmap-index", "--objects", "--all"],
    );
    let b = run(
        &fx,
        rgit,
        &ours,
        &["rev-list", "--use-bitmap-index", "--objects", "--all"],
    );
    assert_eq!(a.stdout, b.stdout);
}

#[test]
fn midx_bitmap_is_byte_identical() {
    let fx = history(30);
    if fx.is_skipped() {
        return;
    }
    let fx = fx
        .git(["repack", "-dq"])
        .write("x.txt", "x\n")
        .git(["add", "x.txt"])
        .git(["commit", "-q", "-m", "x"])
        .git(["repack", "-dq"]);
    // Distinct whole-second modification times: the oldest pack is the
    // preferred one, ties would depend on the directory order.
    let pack_dir = fx.work_dir().join(".git/objects/pack");
    let mut packs: Vec<_> = std::fs::read_dir(&pack_dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "pack"))
        .collect();
    packs.sort();
    for (i, p) in packs.iter().enumerate() {
        let t =
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000 - 100 * i as u64);
        std::fs::File::open(p).unwrap().set_modified(t).unwrap();
    }
    for config in [&[][..], &["-c", "pack.writeBitmapLookupTable=true"][..]] {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("git"), tmp.path().join("rgit"));
        copy_dir(&fx.work_dir(), &a);
        copy_dir(&fx.work_dir(), &b);
        let mut args: Vec<&str> = config.to_vec();
        args.extend(["multi-pack-index", "write", "--bitmap"]);
        let out = run(&fx, fx.reference_git(), &a, &args);
        assert!(out.status.success());
        let out = run(&fx, Path::new(env!("CARGO_BIN_EXE_rgit")), &b, &args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let files = |d: &Path| {
            let mut v: Vec<(String, Vec<u8>)> = std::fs::read_dir(d.join(".git/objects/pack"))
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("multi-pack-index"))
                .map(|n| {
                    let data = std::fs::read(d.join(".git/objects/pack").join(&n)).unwrap();
                    (n, data)
                })
                .collect();
            v.sort();
            v
        };
        let (fa, fb) = (files(&a), files(&b));
        if fa != fb
            && let Some(d) = std::env::var_os("RGIT_BITMAP_DUMP")
        {
            copy_dir(&a, &Path::new(&d).join("git"));
            copy_dir(&b, &Path::new(&d).join("rgit"));
        }
        assert_eq!(
            fa.iter().map(|f| &f.0).collect::<Vec<_>>(),
            fb.iter().map(|f| &f.0).collect::<Vec<_>>()
        );
        for ((name, x), (_, y)) in fa.iter().zip(&fb) {
            assert!(x == y, "{name} differs");
        }
    }
}

#[test]
fn fsck_checks_reverse_index() {
    let fx = history(10).git(["-c", "pack.writeReverseIndex=true", "repack", "-adq"]);
    if fx.is_skipped() {
        return;
    }
    let pack_dir = fx.work_dir().join(".git/objects/pack");
    let rev = std::fs::read_dir(&pack_dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "rev"))
        .unwrap();
    let mut data = std::fs::read(&rev).unwrap();
    data[11] = 3;
    let mut perm = std::fs::metadata(&rev).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(false);
    std::fs::set_permissions(&rev, perm).unwrap();
    std::fs::write(&rev, &data).unwrap();
    let rgit = Path::new(env!("CARGO_BIN_EXE_rgit"));
    let out = run(&fx, rgit, &fx.work_dir(), &["fsck"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("has unsupported hash id 3"));
    let out = run(&fx, rgit, &fx.work_dir(), &["fsck", "--connectivity-only"]);
    assert!(out.status.success());
}
