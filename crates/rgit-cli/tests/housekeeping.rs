// SPDX-License-Identifier: MIT
//! `repack`, `prune` and `prune-packed` against the reference git.
//!
//! The differential cases compare output, exit code, the set of objects
//! and `fsck` (which lists dangling objects, so a wrongly pruned or kept
//! object shows up). Packs need not be byte-identical, so the pack layout
//! is checked separately by running rgit and inspecting the result with
//! git (`count-objects`, `fsck --full`, `verify-pack`).
//!
//! Safety cases: nothing reachable from any worktree's HEAD, index,
//! per-worktree refs, reflog, stash or tag may be pruned.

use std::path::Path;
use std::process::Command;

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"))
}

/// A repository with every kind of starting point and some garbage:
/// branches, tags (also of a blob), a deleted branch still in the HEAD
/// reflog, a stash, a blob only in the index, a linked worktree with its
/// own detached HEAD, index and `refs/bisect` ref, and dangling blobs,
/// trees, commits and tags. Some objects are packed, some loose.
fn repo() -> Fixture {
    let mut fx = fixture().git(["init", "-q"]);
    for c in 0..4 {
        fx = fx
            .write("a.txt", format!("a {c}\n"))
            .write(format!("d/f{c}.txt"), format!("f {c}\n"))
            .git(["add", "-A"])
            .git(["commit", "-q", "-m", &format!("c{c}")])
            .tick();
    }
    fx = fx
        .git(["tag", "-a", "-m", "tag", "v1"])
        .git(["repack", "-adq"])
        // A branch that is deleted, but stays in the HEAD reflog.
        .git(["checkout", "-q", "-b", "gone", "HEAD~1"])
        .write("gone.txt", "gone\n")
        .git(["add", "gone.txt"])
        .git(["commit", "-q", "-m", "gone"])
        .tick()
        .git(["checkout", "-q", "main"])
        .git(["branch", "-q", "-D", "gone"])
        // A branch deleted without any reflog left: unreachable.
        .git([
            "-c",
            "core.logAllRefUpdates=false",
            "branch",
            "lost",
            "HEAD~2",
        ])
        .git([
            "-c",
            "core.logAllRefUpdates=false",
            "checkout",
            "-q",
            "lost",
        ])
        .write("lost.txt", "lost\n")
        .git(["add", "lost.txt"])
        .git([
            "-c",
            "core.logAllRefUpdates=false",
            "commit",
            "-q",
            "-m",
            "lost",
        ])
        .tick()
        .git([
            "-c",
            "core.logAllRefUpdates=false",
            "checkout",
            "-q",
            "main",
        ])
        .git([
            "-c",
            "core.logAllRefUpdates=false",
            "branch",
            "-q",
            "-D",
            "lost",
        ])
        .git(["reflog", "expire", "--expire-unreachable=now", "--all"])
        // A stash.
        .write("a.txt", "stashed\n")
        .git(["stash", "-q"])
        .tick()
        // A blob only in the index.
        .write("staged.txt", "only staged\n")
        .git(["add", "staged.txt"])
        // A linked worktree with a detached commit and a staged blob.
        .git(["worktree", "add", "-q", "--detach", "wt"])
        .write("wt/wt.txt", "worktree commit\n")
        .git(["-C", "wt", "add", "wt.txt"])
        .git(["-C", "wt", "commit", "-q", "-m", "wt"])
        .tick()
        .write("wt/wt-staged.txt", "worktree staged\n")
        .git(["-C", "wt", "add", "wt-staged.txt"])
        .git(["-C", "wt", "update-ref", "refs/bisect/bad", "HEAD~1"])
        .git(["tag", "blobtag", "HEAD:a.txt"]);
    // Garbage: a dangling blob, tree, commit and annotated tag.
    fx = fx
        .write("x", "dangling blob\n")
        .git(["hash-object", "-w", "x"])
        .remove("x");
    let tree = String::from_utf8(fx.git_stdout(["write-tree"])).unwrap();
    let tree = tree.trim().to_string();
    let commit = String::from_utf8(fx.git_stdout(["commit-tree", &tree, "-m", "dangling"]))
        .unwrap()
        .trim()
        .to_string();
    fx = fx
        .git(["tag", "-a", "-m", "dangling tag", "dtag", &commit])
        .git(["update-ref", "-d", "refs/tags/dtag"]);
    fx
}

/// Run `args` with both binaries; stdout must match after sorting lines
/// (git lists loose objects in directory order).
fn compare_sorted(fx: &mut Fixture, args: &[&str], checks: &[Check]) {
    let Some(out) = fx
        .compare(args)
        .checks(
            checks
                .iter()
                .copied()
                .chain([Check::ExitCode, Check::Stderr]),
        )
        .run()
    else {
        return;
    };
    let sorted = |b: &[u8]| {
        let mut l: Vec<String> = String::from_utf8_lossy(b)
            .lines()
            .map(str::to_string)
            .collect();
        l.sort();
        l
    };
    assert_eq!(
        sorted(&out.rgit.stdout),
        sorted(&out.git.stdout),
        "stdout of {args:?}"
    );
}

const STATE: &[Check] = &[Check::Refs, Check::Objects, Check::Fsck, Check::Index];

#[test]
fn prune_matches_git() {
    let mut fx = repo();
    if fx.is_skipped() {
        return;
    }
    compare_sorted(&mut fx, &["prune", "-n"], &[]);
    compare_sorted(&mut fx, &["prune", "--dry-run", "--expire=never"], &[]);
    compare_sorted(&mut fx, &["prune", "-v"], STATE);
    compare_sorted(&mut fx, &["prune"], STATE);
}

#[test]
fn prune_after_repack_matches_git() {
    let mut fx = repo().git(["repack", "-adq"]);
    if fx.is_skipped() {
        return;
    }
    compare_sorted(&mut fx, &["prune", "-n"], &[]);
    compare_sorted(&mut fx, &["prune-packed", "-n"], &[]);
    compare_sorted(&mut fx, &["prune-packed"], STATE);
}

#[test]
fn prune_keeps_heads_given_as_arguments() {
    let fx = repo();
    if fx.is_skipped() {
        return;
    }
    let head = String::from_utf8(fx.git_stdout(["rev-parse", "HEAD"])).unwrap();
    let mut fx = fx.write("n.txt", "new\n").git(["add", "n.txt"]).git([
        "-c",
        "core.logAllRefUpdates=false",
        "commit",
        "-q",
        "-m",
        "tmp",
    ]);
    let tmp = String::from_utf8(fx.git_stdout(["rev-parse", "HEAD"])).unwrap();
    fx = fx
        .git([
            "-c",
            "core.logAllRefUpdates=false",
            "reset",
            "-q",
            "--hard",
            head.trim(),
        ])
        .git(["reflog", "expire", "--expire=now", "--all"]);
    compare_sorted(&mut fx, &["prune", "-n", "--", tmp.trim()], &[]);
    compare_sorted(&mut fx, &["prune", tmp.trim()], STATE);
}

#[test]
fn prune_errors_match_git() {
    let mut fx = repo();
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["prune", "garbage"][..],
        &["prune", "---"],
        &["prune", "--expire"],
        &["prune", "--expire=nyah"],
        &["prune-packed", "extra"],
        &["prune", "-h"],
        &["prune-packed", "-h"],
    ] {
        fx.compare(args)
            .checks([Check::ExitCode, Check::Stdout, Check::Stderr])
            .run();
    }
    let mut fx = fx
        .git(["config", "core.repositoryformatversion", "1"])
        .git(["config", "extensions.preciousObjects", "true"]);
    for args in [
        &["prune"][..],
        &["repack", "-a", "-d"],
        &["repack", "-a", "-k", "--cruft"],
    ] {
        fx.compare(args)
            .checks([
                Check::ExitCode,
                Check::Stdout,
                Check::Stderr,
                Check::Objects,
            ])
            .run();
    }
}

#[test]
fn broken_refs_stop_prune_and_repack() {
    let mut fx = repo().write(".git/refs/heads/garbage", "not a ref\n");
    if fx.is_skipped() {
        return;
    }
    for args in [&["prune"][..], &["prune", "-n"], &["repack", "-a", "-d"]] {
        fx.compare(args)
            .checks([
                Check::ExitCode,
                Check::Stdout,
                Check::Stderr,
                Check::Objects,
            ])
            .run();
    }
    // A reference to an object that does not exist.
    let mut fx = repo().write(
        ".git/refs/heads/missing",
        "1111111111111111111111111111111111111111\n",
    );
    for args in [&["prune"][..], &["repack", "-a", "-d"]] {
        fx.compare(args)
            .checks([
                Check::ExitCode,
                Check::Stdout,
                Check::Stderr,
                Check::Objects,
            ])
            .run();
    }
}

#[test]
fn repack_matches_git() {
    for args in [
        &["repack"][..],
        &["repack", "-d"],
        &["repack", "-a"],
        &["repack", "-a", "-d"],
        &["repack", "-A", "-d"],
        &["repack", "-a", "-d", "-k"],
        &["repack", "-A", "-d", "--unpack-unreachable=now"],
        &[
            "repack",
            "-adlfFq",
            "-n",
            "--depth=5",
            "--window=3",
            "--threads=1",
        ],
    ] {
        let mut fx = repo();
        if fx.is_skipped() {
            return;
        }
        compare_sorted(&mut fx, args, STATE);
        // What is left after pruning must agree as well.
        compare_sorted(&mut fx, &["prune"], STATE);
    }
}

#[test]
fn repack_nothing_to_pack() {
    let mut fx = fixture().git(["init", "-q"]);
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["repack"][..],
        &["repack", "-a", "-d"],
        &["repack", "-q"],
        &["prune"],
    ] {
        fx.compare(args).run();
    }
}

/// Run a shell script in the work directory with `$RGIT` (the binary
/// under test) and `$GIT` (the reference git), isolated from the user's
/// configuration. Returns stdout; panics on failure.
fn sh(fx: &Fixture, script: &str) -> String {
    let work = fx.work_dir();
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!("set -e\n{script}"))
        .current_dir(&work)
        .env("RGIT", env!("CARGO_BIN_EXE_rgit"))
        .env("GIT", fx.reference_git())
        .env("HOME", fx.home_dir())
        .env("XDG_CONFIG_HOME", fx.home_dir().join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .output()
        .expect("sh");
    assert!(
        out.status.success(),
        "script failed:\n{script}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn count_objects(fx: &Fixture) -> (u32, u32) {
    let out = sh(fx, "\"$GIT\" count-objects -v");
    let field = |name: &str| -> u32 {
        out.lines()
            .find_map(|l| l.strip_prefix(&format!("{name}: ")))
            .and_then(|v| v.parse().ok())
            .unwrap()
    };
    (field("count"), field("packs"))
}

#[test]
fn repack_all_leaves_one_pack_and_a_sound_repository() {
    let fx = repo();
    if fx.is_skipped() {
        return;
    }
    let reachable_before = sh(
        &fx,
        "\"$GIT\" rev-list --objects --all --reflog --indexed-objects | cut -c1-40 | sort",
    );
    sh(&fx, "\"$RGIT\" repack -a -d -q");
    let (loose, packs) = count_objects(&fx);
    assert_eq!(packs, 1);
    // Only the unreachable objects, which were loose, are left loose.
    let unreachable = sh(&fx, "\"$GIT\" prune -n | wc -l");
    assert_eq!(loose, unreachable.trim().parse::<u32>().unwrap());
    assert!(loose > 0);
    sh(&fx, "\"$GIT\" fsck --full");
    sh(
        &fx,
        "for i in .git/objects/pack/*.idx; do \"$GIT\" verify-pack $i; done",
    );
    let reachable_after = sh(
        &fx,
        "\"$GIT\" rev-list --objects --all --reflog --indexed-objects | cut -c1-40 | sort",
    );
    assert_eq!(reachable_before, reachable_after);
    // Prune and repack again: still sound, nothing loose.
    sh(&fx, "\"$RGIT\" prune && \"$RGIT\" repack -a -d -q");
    assert_eq!(count_objects(&fx), (0, 1));
    sh(&fx, "\"$GIT\" fsck --full");
    sh(
        &fx,
        "test -f .git/info/refs && grep -q '^P pack-' .git/objects/info/packs",
    );
    // The worktree still works.
    sh(
        &fx,
        "\"$GIT\" -C wt show HEAD:wt.txt && \"$GIT\" -C wt show :wt-staged.txt && \"$GIT\" stash show -p",
    );
}

#[test]
fn repack_honours_keep_packs() {
    let fx = repo();
    if fx.is_skipped() {
        return;
    }
    sh(
        &fx,
        "\"$GIT\" repack -a -d -q
        for p in .git/objects/pack/*.pack; do touch ${p%.pack}.keep; done
        echo new >new.txt && \"$GIT\" add new.txt && \"$GIT\" commit -q -m new",
    );
    sh(&fx, "\"$RGIT\" repack -a -d -q");
    let unreachable: u32 = sh(&fx, "\"$GIT\" prune -n | wc -l").trim().parse().unwrap();
    assert_eq!(count_objects(&fx), (unreachable, 2));
    // The objects of the kept pack were not copied into the new one.
    let dup = sh(
        &fx,
        "for i in .git/objects/pack/*.idx; do \"$GIT\" show-index <$i | cut -d' ' -f2; done | sort | uniq -d",
    );
    assert_eq!(dup, "");
    // --keep-pack works like a .keep file.
    sh(
        &fx,
        "rm .git/objects/pack/*.keep
        echo more >more.txt && \"$GIT\" add more.txt && \"$GIT\" commit -q -m more",
    );
    let kept = sh(&fx, "ls .git/objects/pack | grep pack$ | head -1");
    sh(
        &fx,
        &format!("\"$RGIT\" repack -a -d -q --keep-pack={}", kept.trim()),
    );
    sh(&fx, &format!("test -f .git/objects/pack/{}", kept.trim()));
    sh(&fx, "\"$GIT\" fsck --full");
}

fn set_mtime(path: &Path, secs_ago: u64) {
    let t = std::time::SystemTime::now() - std::time::Duration::from_secs(secs_ago);
    let f = std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap_or_else(|_| std::fs::File::open(path).unwrap());
    f.set_modified(t).unwrap();
}

fn loose_path(fx: &Fixture, id: &str) -> std::path::PathBuf {
    fx.work_dir()
        .join(".git/objects")
        .join(&id[..2])
        .join(&id[2..])
}

#[test]
fn prune_expire_keeps_recent_objects_and_what_they_reach() {
    let fx = repo();
    if fx.is_skipped() {
        return;
    }
    sh(&fx, "\"$GIT\" repack -a -d -q && \"$GIT\" prune");
    let old = sh(&fx, "echo old | \"$GIT\" hash-object -w --stdin")
        .trim()
        .to_string();
    let saved = sh(&fx, "echo saved | \"$GIT\" hash-object -w --stdin")
        .trim()
        .to_string();
    let tree = sh(
        &fx,
        &format!("printf '100644 blob {saved}\\tf\\n' | \"$GIT\" mktree"),
    )
    .trim()
    .to_string();
    for id in [&old, &saved] {
        set_mtime(&loose_path(&fx, id), 86400);
    }
    let stale = fx.work_dir().join(".git/objects/tmp_obj_stale");
    std::fs::write(&stale, "x").unwrap();
    set_mtime(&stale, 86400);
    let fresh = fx.work_dir().join(".git/objects/tmp_obj_fresh");
    std::fs::write(&fresh, "x").unwrap();
    let out = sh(&fx, "\"$RGIT\" prune -n --expire=12.hours.ago");
    assert!(out.contains(&format!("{old} blob")), "{out}");
    assert!(!out.contains(&saved) && !out.contains(&tree), "{out}");
    assert!(
        out.contains("Removing stale temporary file .git/objects/tmp_obj_stale"),
        "{out}"
    );
    assert!(!out.contains("tmp_obj_fresh"), "{out}");
    sh(&fx, "\"$RGIT\" prune --expire=12.hours.ago");
    assert!(!loose_path(&fx, &old).exists());
    assert!(loose_path(&fx, &saved).exists());
    assert!(loose_path(&fx, &tree).exists());
    assert!(!stale.exists() && fresh.exists());
    sh(&fx, "\"$RGIT\" prune --expire=never");
    assert!(loose_path(&fx, &saved).exists());
    sh(&fx, "\"$RGIT\" prune --expire=now");
    assert!(!loose_path(&fx, &saved).exists() && !loose_path(&fx, &tree).exists());
    sh(&fx, "\"$GIT\" fsck --full");
}

#[test]
fn repack_loosens_unreachable_objects_with_the_pack_time() {
    let fx = repo();
    if fx.is_skipped() {
        return;
    }
    sh(
        &fx,
        "\"$GIT\" repack -a -d -q && \"$GIT\" prune
        \"$GIT\" checkout -q -b doomed && echo doomed >doomed.txt && \"$GIT\" add doomed.txt
        \"$GIT\" commit -q -m doomed && \"$GIT\" checkout -q main
        \"$GIT\" repack -a -d -q
        \"$GIT\" branch -q -D doomed && \"$GIT\" reflog expire --expire=now --expire-unreachable=now --all",
    );
    let doomed = sh(&fx, "echo doomed | \"$GIT\" hash-object --stdin")
        .trim()
        .to_string();
    let pack = sh(&fx, "ls .git/objects/pack/*.pack").trim().to_string();
    set_mtime(&fx.work_dir().join(&pack), 3 * 86400);
    sh(
        &fx,
        "\"$RGIT\" repack -A -d -q --unpack-unreachable=4.days.ago",
    );
    let path = loose_path(&fx, &doomed);
    assert!(path.exists());
    let age = std::fs::metadata(&path)
        .unwrap()
        .modified()
        .unwrap()
        .elapsed()
        .unwrap();
    assert!(
        age.as_secs() > 2 * 86400,
        "loosened object has the pack's time"
    );
    sh(&fx, "\"$GIT\" fsck --full");
    // Old enough: not loosened at all.
    sh(
        &fx,
        "\"$RGIT\" repack -a -d -q -k && \"$RGIT\" prune --expire=now",
    );
    let pack = sh(&fx, "ls .git/objects/pack/*.pack").trim().to_string();
    set_mtime(&fx.work_dir().join(&pack), 30 * 86400);
    sh(
        &fx,
        "\"$RGIT\" repack -A -d -q --unpack-unreachable=2.weeks.ago",
    );
    assert!(!loose_path(&fx, &doomed).exists());
    sh(&fx, "\"$GIT\" fsck --full");
}

#[test]
fn local_repack_skips_alternates() {
    let fx = fixture()
        .git(["init", "-q", "base"])
        .write("base/f", "base\n")
        .git(["-C", "base", "add", "f"])
        .git(["-C", "base", "commit", "-q", "-m", "base"])
        .git(["-C", "base", "repack", "-adq"])
        .git(["clone", "-q", "--shared", "base", "clone"])
        .write("clone/g", "clone\n")
        .git(["-C", "clone", "add", "g"])
        .git(["-C", "clone", "commit", "-q", "-m", "clone"]);
    if fx.is_skipped() {
        return;
    }
    sh(&fx, "cd clone && \"$RGIT\" repack -a -d -l -q");
    let n = sh(
        &fx,
        "cd clone && \"$GIT\" show-index <$(ls .git/objects/pack/*.idx) | wc -l",
    );
    assert_eq!(n.trim(), "3", "only the clone's own commit, tree and blob");
    sh(
        &fx,
        "cd clone && \"$GIT\" fsck --full && \"$RGIT\" prune && \"$GIT\" fsck --full",
    );
    // Without -l the borrowed objects are copied.
    sh(&fx, "cd clone && \"$RGIT\" repack -a -d -q");
    let n = sh(
        &fx,
        "cd clone && \"$GIT\" show-index <$(ls .git/objects/pack/*.idx) | wc -l",
    );
    assert_eq!(n.trim(), "6");
}

#[test]
fn shallow_file_is_pruned() {
    let fx = repo();
    if fx.is_skipped() {
        return;
    }
    let tree = sh(&fx, "\"$GIT\" rev-parse HEAD^{tree}").trim().to_string();
    let orphan = sh(&fx, &format!("echo hi | \"$GIT\" commit-tree {tree}"))
        .trim()
        .to_string();
    sh(&fx, &format!("echo {orphan} >.git/shallow"));
    let out = sh(&fx, "\"$RGIT\" prune --dry-run");
    assert!(out.contains(&orphan));
    sh(&fx, &format!("grep {orphan} .git/shallow"));
    sh(&fx, "\"$RGIT\" prune");
    assert!(!fx.work_dir().join(".git/shallow").exists());
    // A shallow commit that is reachable stays listed.
    let head = sh(&fx, "\"$GIT\" rev-parse HEAD").trim().to_string();
    sh(&fx, &format!("echo {head} >.git/shallow"));
    sh(
        &fx,
        "\"$RGIT\" prune -n >/dev/null && \"$RGIT\" prune --expire=never",
    );
    sh(&fx, &format!("grep {head} .git/shallow"));
}

// ---------------------------------------------------------------- gc

/// [`repo`] with reflog expiry off, so that git's `reflog expire` step
/// (not in rgit yet, #101) changes nothing, and without cruft packs
/// (#96).
fn gc_repo() -> Fixture {
    repo()
        .git(["config", "gc.reflogExpire", "never"])
        .git(["config", "gc.reflogExpireUnreachable", "never"])
        .git(["config", "gc.cruftPacks", "false"])
        .write("loose.txt", "loose\n")
        .git(["add", "loose.txt"])
        .git(["commit", "-q", "-m", "loose"])
}

#[test]
fn gc_matches_git() {
    for args in [
        &["gc"][..],
        &["gc", "--prune=now"],
        &["gc", "--no-prune"],
        &["gc", "--aggressive", "-q"],
        &["gc", "--keep-largest-pack"],
    ] {
        let mut fx = gc_repo();
        if fx.is_skipped() {
            return;
        }
        compare_sorted(&mut fx, args, STATE);
    }
}

#[test]
fn gc_errors_match_git() {
    let mut fx = gc_repo();
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["gc", "--gobbledegook"][..],
        &["gc", "extra"],
        &["gc", "-h"],
    ] {
        fx.compare(args)
            .checks([Check::ExitCode, Check::Stdout, Check::Stderr])
            .run();
    }
    let mut fx = fx.git(["config", "gc.pruneExpire", "invalid"]);
    fx.compare(["gc"])
        .checks([
            Check::ExitCode,
            Check::Stdout,
            Check::Stderr,
            Check::Objects,
        ])
        .run();
}

#[test]
fn gc_leaves_one_pack() {
    let fx = gc_repo();
    if fx.is_skipped() {
        return;
    }
    // Several packs and loose objects.
    sh(
        &fx,
        "for i in 1 2 3; do echo $i >p$i && \"$GIT\" add p$i && \"$GIT\" commit -q -m p$i && \"$GIT\" repack -q; done
        echo extra >extra.txt && \"$GIT\" add extra.txt && \"$GIT\" commit -q -m extra",
    );
    sh(&fx, "\"$RGIT\" gc -q --prune=now");
    assert_eq!(count_objects(&fx), (0, 1));
    sh(&fx, "\"$GIT\" fsck --full && test ! -f .git/gc.pid");
    sh(
        &fx,
        "\"$GIT\" -C wt show :wt-staged.txt && \"$GIT\" stash show -p",
    );
}

/// Write loose objects until `objects/17` holds three of them.
const FILL_17: &str = "i=0; while [ $(ls .git/objects/17 2>/dev/null | wc -l) -lt 3 ]; do i=$((i+1)); echo \"fill $i\" | \"$GIT\" hash-object -w --stdin >/dev/null; done";

/// Run `script`, returning its standard error.
fn sh_err(fx: &Fixture, script: &str) -> String {
    sh(fx, &format!("{{ {script} ; }} 2>&1 >/dev/null"))
}

#[test]
fn gc_auto() {
    let fx = gc_repo();
    if fx.is_skipped() {
        return;
    }
    sh(
        &fx,
        &format!("\"$GIT\" config gc.auto 1 && \"$GIT\" config gc.autoDetach false && {FILL_17}"),
    );
    // Nothing to do with the default threshold, or when disabled.
    let err = sh_err(
        &fx,
        "\"$RGIT\" -c gc.auto=6700 gc --auto && \"$RGIT\" -c gc.auto=0 gc --auto",
    );
    assert_eq!(err, "");
    // The hook can veto.
    sh(
        &fx,
        "printf '#!/bin/sh\\necho vetoed >&2\\nexit 1\\n' >.git/hooks/pre-auto-gc && chmod +x .git/hooks/pre-auto-gc",
    );
    assert_eq!(sh_err(&fx, "\"$RGIT\" gc --auto"), "vetoed\n");
    sh(&fx, "rm .git/hooks/pre-auto-gc");
    let err = sh_err(&fx, "\"$RGIT\" gc --auto");
    let too_many =
        "warning: There are too many unreachable loose objects; run 'git prune' to remove them.\n";
    assert_eq!(
        err,
        format!(
            "Auto packing the repository for optimum performance.\n\
             See \"git help gc\" for manual housekeeping.\n{too_many}"
        )
    );
    let (_, packs) = count_objects(&fx);
    assert!(packs >= 1);
    sh(&fx, "\"$GIT\" fsck --full");
    assert_eq!(sh_err(&fx, "\"$RGIT\" gc --auto --quiet"), too_many);
}

#[test]
fn gc_log_stops_background_runs() {
    let fx = gc_repo();
    if fx.is_skipped() {
        return;
    }
    sh(
        &fx,
        &format!("\"$GIT\" config gc.auto 1 && {FILL_17} && echo 'old trouble' >.git/gc.log"),
    );
    assert_eq!(
        sh_err(&fx, "\"$RGIT\" gc --auto"),
        "Auto packing the repository in background for optimum performance.\n\
         See \"git help gc\" for manual housekeeping.\n\
         warning: The last gc run reported the following. Please correct the root cause\n\
         and remove .git/gc.log\n\
         Automatic cleanup will not be performed until the file is removed.\n\
         \n\
         old trouble\n\n"
    );
    // An old log is removed and the run goes to the background.
    sh(&fx, "touch -d '3 days ago' .git/gc.log");
    sh(&fx, "\"$RGIT\" gc --auto --quiet");
    sh(
        &fx,
        "i=0; while [ -f .git/gc.pid ] || [ -f .git/gc.log.lock ] || ! ls .git/objects/pack/*.pack >/dev/null 2>&1; do i=$((i+1)); [ $i -lt 600 ]; sleep 0.1; done",
    );
    // The background run wrote its warning to gc.log.
    let log = sh(&fx, "cat .git/gc.log");
    assert!(log.contains("too many unreachable loose objects"), "{log}");
    sh(&fx, "\"$GIT\" fsck --full");
}

#[test]
fn gc_pid_lock() {
    let fx = gc_repo();
    if fx.is_skipped() {
        return;
    }
    // A live process on this host holds the lock.
    let err = sh_err(
        &fx,
        "echo \"$$ $(hostname)\" >.git/gc.pid; \"$RGIT\" gc -q || echo rc=$? >&2",
    );
    assert!(
        err.starts_with("fatal: gc is already running on machine"),
        "{err}"
    );
    assert!(err.ends_with("rc=128\n"), "{err}");
    // --force runs anyway and removes the lock afterwards.
    sh(&fx, "\"$RGIT\" gc -q --force && test ! -f .git/gc.pid");
    // The lock of a process that is gone is taken over.
    sh(
        &fx,
        "echo \"999999 $(hostname)\" >.git/gc.pid && \"$RGIT\" gc -q && test ! -f .git/gc.pid",
    );
}

#[test]
fn commit_and_merge_run_auto_gc_like_git() {
    let mut fx =
        gc_repo()
            .git(["config", "gc.auto", "1"])
            .git(["config", "gc.autoDetach", "false"]);
    if fx.is_skipped() {
        return;
    }
    sh(&fx, FILL_17);
    // git's default maintenance strategy does not run gc after a commit.
    fx.compare(["commit", "-q", "--allow-empty", "-m", "no gc"])
        .checks([
            Check::ExitCode,
            Check::Stdout,
            Check::Stderr,
            Check::Objects,
        ])
        .run();
    let mut fx = fx.git(["config", "maintenance.strategy", "gc"]);
    let state = [
        Check::ExitCode,
        Check::Stdout,
        Check::Stderr,
        Check::Objects,
        Check::Fsck,
    ];
    fx.compare(["commit", "--allow-empty", "-m", "gc"])
        .checks(state)
        .run();
    sh(&fx, FILL_17);
    fx.compare(["merge", "-q", "--no-edit", "--no-ff", "HEAD~2"])
        .checks(state)
        .run();
    let mut fx = fx.git(["config", "maintenance.auto", "false"]);
    sh(&fx, FILL_17);
    fx.compare(["commit", "--allow-empty", "-m", "off"])
        .checks([
            Check::ExitCode,
            Check::Stdout,
            Check::Stderr,
            Check::Objects,
        ])
        .run();
}
