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
    // No automatic maintenance from the setup commands: a detached
    // background run of the reference git would race with the checks.
    let mut fx = fixture()
        .git(["init", "-q"])
        .git(["config", "maintenance.auto", "false"]);
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

/// `repack -a -d` drops the shallow commits it deletes (t5537 ".git/shallow
/// is edited by repack"); `-A` loosens them, so they stay listed.
#[test]
fn repack_edits_the_shallow_file() {
    let fx = repo();
    if fx.is_skipped() {
        return;
    }
    let tree = sh(&fx, "\"$GIT\" rev-parse HEAD^{tree}").trim().to_string();
    let orphan = sh(&fx, &format!("echo hi | \"$GIT\" commit-tree {tree}"))
        .trim()
        .to_string();
    let head = sh(&fx, "\"$GIT\" rev-parse HEAD").trim().to_string();
    // The orphan is packed, so that only deleting the old pack removes it.
    sh(
        &fx,
        &format!("\"$GIT\" repack -adkq && printf '%s\\n' {head} {orphan} >.git/shallow"),
    );
    sh(&fx, "cp -R .git ../loosen.git");
    sh(&fx, "\"$RGIT\" repack -Adq --unpack-unreachable=never");
    sh(&fx, &format!("grep {orphan} .git/shallow"));
    sh(
        &fx,
        "rm -rf .git && mv ../loosen.git .git && \"$RGIT\" repack -adq",
    );
    let shallow = std::fs::read_to_string(fx.work_dir().join(".git/shallow")).unwrap();
    assert_eq!(shallow, format!("{head}\n"));
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
        "mkdir -p .git/hooks && printf '#!/bin/sh\\necho vetoed >&2\\nexit 1\\n' >.git/hooks/pre-auto-gc && chmod +x .git/hooks/pre-auto-gc",
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
    let mut fx = fx.git(["config", "maintenance.auto", "true"]).git([
        "config",
        "maintenance.strategy",
        "gc",
    ]);
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

#[test]
fn fetch_runs_auto_gc_like_git() {
    let mut fx = gc_repo()
        .git(["clone", "-q", "--no-local", ".", "../upstream"])
        .write("../upstream/up.txt", "upstream\n")
        .git(["-C", "../upstream", "add", "up.txt"])
        .git(["-C", "../upstream", "commit", "-q", "-m", "upstream"])
        .git(["remote", "add", "up", "../upstream"])
        .git(["config", "gc.auto", "1"])
        .git(["config", "gc.autoDetach", "false"])
        .git(["config", "maintenance.auto", "true"])
        .git(["config", "maintenance.strategy", "gc"]);
    if fx.is_skipped() {
        return;
    }
    let state = [
        Check::ExitCode,
        Check::Stdout,
        Check::Stderr,
        Check::Refs,
        Check::Objects,
        Check::Fsck,
    ];
    sh(&fx, FILL_17);
    fx.compare(["fetch", "-q", "--no-tags", "up"])
        .checks(state)
        .run();
    sh(&fx, FILL_17);
    fx.compare(["fetch", "-q", "--no-tags", "--no-auto-maintenance", "up"])
        .checks(state)
        .run();
}

#[test]
fn badly_named_refs_stop_destructive_commands() {
    let fx = repo();
    if fx.is_skipped() {
        return;
    }
    // History reachable only from a ref whose name git would not create.
    let bogus = sh(
        &fx,
        "\"$GIT\" commit -q --allow-empty -m bogus && \"$GIT\" rev-parse HEAD && \"$GIT\" -c core.logAllRefUpdates=false reset -q --hard HEAD^",
    )
    .trim()
    .to_string();
    sh(
        &fx,
        &format!(
            "\"$GIT\" reflog expire --expire=now --all && echo {bogus} >'.git/refs/heads/bogus..name'"
        ),
    );
    for cmd in [
        "prune --expire=now",
        "repack -a -d",
        "repack -A -d --unpack-unreachable=now",
        "gc --prune=now",
    ] {
        let err = sh_err(&fx, &format!("\"$RGIT\" {cmd} || echo failed >&2"));
        assert!(err.ends_with("failed\n"), "{cmd}: {err}");
        sh(&fx, &format!("\"$GIT\" cat-file -e {bogus}"));
    }
    // GIT_REF_PARANOIA=0 skips such references.
    sh(&fx, "GIT_REF_PARANOIA=0 \"$RGIT\" repack -a -d -k");
    sh(&fx, &format!("\"$GIT\" cat-file -e {bogus}"));
}

#[test]
fn objects_reachable_from_recent_ones_survive_repack_and_prune() {
    let fx = repo();
    if fx.is_skipped() {
        return;
    }
    // A blob that became unreachable sits in an old pack; a new tree
    // (not yet committed by "another process") refers to it.
    sh(
        &fx,
        "\"$GIT\" -c core.logAllRefUpdates=false checkout -q -b doomed
        echo abandon >abandon && \"$GIT\" add abandon && \"$GIT\" -c core.logAllRefUpdates=false commit -q -m abandon
        \"$GIT\" repack -a -d -q
        \"$GIT\" -c core.logAllRefUpdates=false checkout -q main && \"$GIT\" branch -q -D doomed
        \"$GIT\" reflog expire --expire=now --expire-unreachable=now --all",
    );
    let pack = sh(&fx, "ls .git/objects/pack/*.pack").trim().to_string();
    set_mtime(&fx.work_dir().join(&pack), 86400);
    let tree = sh(
        &fx,
        "GIT_INDEX_FILE=.git/index.tmp; export GIT_INDEX_FILE; \"$GIT\" read-tree HEAD && echo abandon >abandon && \"$GIT\" add abandon && \"$GIT\" write-tree",
    )
    .trim()
    .to_string();
    sh(&fx, "rm -f .git/index.tmp abandon");
    sh(&fx, "\"$RGIT\" gc -q --prune=12.hours.ago");
    sh(
        &fx,
        &format!(
            "\"$GIT\" ls-tree -r {tree} >/dev/null && echo foo | \"$GIT\" commit-tree -p HEAD {tree} >/dev/null"
        ),
    );
    let commit = sh(
        &fx,
        &format!("echo foo | \"$GIT\" commit-tree -p HEAD {tree}"),
    );
    sh(
        &fx,
        &format!("\"$GIT\" update-ref refs/heads/rescued {}", commit.trim()),
    );
    sh(&fx, "\"$GIT\" fsck --full");
    // gc.recentObjectsHook names objects that count as recent.
    let blob = sh(&fx, "echo precious | \"$GIT\" hash-object -w --stdin")
        .trim()
        .to_string();
    set_mtime(&loose_path(&fx, &blob), 86400);
    sh(
        &fx,
        &format!("printf '#!/bin/sh\\necho {blob}\\n' >hook && chmod +x hook"),
    );
    sh(
        &fx,
        "\"$RGIT\" -c gc.recentObjectsHook=./hook prune --expire=now",
    );
    assert!(loose_path(&fx, &blob).exists());
    sh(&fx, "\"$RGIT\" prune --expire=now");
    assert!(!loose_path(&fx, &blob).exists());
}

#[test]
fn pack_garbage_is_removed() {
    let fx = gc_repo();
    if fx.is_skipped() {
        return;
    }
    sh(
        &fx,
        "cd .git/objects/pack && : >foo.keep && : >foo.pack && : >fake.idx && : >fake2.keep && : >fake2.idx && : >fake3.keep",
    );
    sh(&fx, "\"$RGIT\" gc -q");
    let left = sh(
        &fx,
        "cd .git/objects/pack && ls foo* fake* 2>/dev/null | sort",
    );
    assert_eq!(left, "fake3.keep\nfoo.keep\nfoo.pack\n");
}

/// Bitmaps of `repack -a` in a bare repository, run by git and by rgit:
/// a pack written again under its old name loses the `.bitmap` (and the
/// `.rev`) this run does not write; the default bitmaps of a bare
/// repository leave kept packs alone and give up quietly, asked-for ones
/// pack kept objects or warn; packs split by `pack.packSizeLimit` get no
/// bitmap.
#[test]
fn repack_bitmaps_in_bare_repositories_match_git() {
    let fx = repo();
    if fx.is_skipped() {
        return;
    }
    let script = r#"
packs() { ls "$1"/objects/pack | sed 's/pack-[0-9a-f]*/pack-X/' | sort | uniq -c; }
run() {
  X=$1 D=$2
  rm -rf $D && "$GIT" clone -q --bare . $D
  "$X" -C $D repack -ad 2>&1; packs $D
  "$X" -C $D -c repack.writeBitmaps=false repack -ad 2>&1; packs $D
  "$X" -C $D repack -ad 2>&1
  "$X" -C $D -c pack.writeReverseIndex=false -c repack.writeBitmaps=false repack -ad 2>&1; packs $D
  "$X" -C $D repack -adq
  p=$(ls $D/objects/pack/*.pack) && >${p%.pack}.keep
  "$X" -C $D repack -ad 2>&1; packs $D
  "$X" -C $D repack -adb --no-pack-kept-objects 2>&1; packs $D
  "$X" -C $D -c repack.writeBitmaps=true repack -ad 2>&1; packs $D
  rm -f $D/objects/pack/*.keep
  awk 'BEGIN { srand(1); for (i = 0; i < 300000; i++) printf "%08x", rand() * 4294967295 }' >big
  b=$("$GIT" -C $D hash-object -w "$PWD/big") && "$GIT" -C $D update-ref refs/tags/big $b
  "$X" -C $D -c pack.packSizeLimit=1m repack -ad 2>&1; packs $D
  "$X" -C $D repack -adb --max-pack-size=1m 2>&1; packs $D
}
run "$GIT" ../g.git >../g.out
run "$RGIT" ../r.git >../r.out
diff ../g.out ../r.out && cat ../g.out
"#;
    let out = sh(&fx, script);
    // The cases did what they are meant to check.
    assert!(
        out.contains("warning: disabling bitmap writing, as some objects are not being packed")
    );
    assert!(
        out.contains(
            "warning: disabling bitmap writing, packs are split due to pack.packSizeLimit"
        ),
        "{out}"
    );
    assert!(out.contains("Nothing new to pack."));
}

/// `pack-objects --unpack-unreachable` loosens the unreachable objects of
/// the packs it reads, except those an alternate has.
#[test]
fn pack_objects_unpack_unreachable_matches_git() {
    let fx = repo();
    if fx.is_skipped() {
        return;
    }
    let script = r#"
run() {
  X=$1 D=$2
  rm -rf $D $D.alt && "$GIT" clone -q --bare . $D && "$GIT" -C $D repack -adq
  "$GIT" clone -q --bare . $D.alt && "$GIT" -C $D.alt repack -adq
  "$GIT" -C $D update-ref -d refs/heads/main
  "$GIT" -C $D for-each-ref --format='%(refname)' | while read r; do "$GIT" -C $D update-ref -d $r; done
  "$GIT" -C $D commit-tree -m x $("$GIT" -C $D mktree </dev/null) >/dev/null
  n=$(cd $D && "$X" pack-objects --honor-pack-keep --non-empty --all --reflog --unpack-unreachable </dev/null pack)
  "$GIT" -C $D count-objects | cut -d, -f1
  echo "$(cd $D.alt/objects && pwd)" >$D/objects/info/alternates
  rm -f $D/objects/??/*
  n=$(cd $D && "$X" pack-objects --honor-pack-keep --non-empty --all --reflog --unpack-unreachable </dev/null pack)
  "$GIT" -C $D count-objects | cut -d, -f1
}
run "$GIT" ../g.git >../g.out
run "$RGIT" ../r.git >../r.out
diff ../g.out ../r.out && cat ../g.out
"#;
    let out = sh(&fx, script);
    let counts: Vec<&str> = out.lines().collect();
    assert_eq!(counts.len(), 2);
    assert_ne!(counts[0], "0 objects");
    assert_eq!(counts[1], "0 objects");
}

/// `repack --filter`: the reachable objects the filter keeps go into one
/// pack, the others (of the replaced packs) into a second one, here or at
/// `--filter-to`.
#[test]
fn repack_filter_splits_the_pack() {
    let fx = repo();
    if fx.is_skipped() {
        return;
    }
    let out = sh(
        &fx,
        r#"G="$GIT" R="$RGIT"
"$R" -c repack.writeBitmaps=false repack -a -d -q --filter=blob:none
ls .git/objects/pack/*.pack | wc -l
commit=$("$G" rev-parse HEAD)
blob=$("$G" rev-parse HEAD:a.txt)
for p in .git/objects/pack/*.idx; do
  ids=$("$G" show-index <$p | cut -d' ' -f2)
  case "$ids" in *$commit*) echo commit-pack-has-blob=$(echo "$ids" | grep -c $blob || true);; esac
  case "$ids" in *$blob*) echo blob-pack-only-blobs=$(echo "$ids" | while read o; do "$G" cat-file -t $o; done | sort -u | tr '\n' ' ');; esac
done
"$G" fsck --no-dangling 2>&1
"$G" init -q --bare ../filtered.git
"$R" -c repack.writeBitmaps=false repack -a -d -q --filter=blob:none --filter-to=../filtered.git/objects/pack/pack
ls .git/objects/pack/*.pack | wc -l
ls ../filtered.git/objects/pack/*.pack | wc -l
"$G" cat-file -e $blob 2>/dev/null || echo blob-gone
echo "$(cd ../filtered.git/objects && pwd)" >.git/objects/info/alternates
"$G" cat-file -p $blob
"#,
    );
    assert_eq!(
        out,
        "2\ncommit-pack-has-blob=0\nblob-pack-only-blobs=blob\n1\n1\nblob-gone\na 3\n"
    );
}

/// The checks of `--filter-to`, `--drop-filtered` and `--dry-run`.
#[test]
fn repack_drop_filtered_errors_match_git() {
    let mut fx = repo();
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["repack", "--drop-filtered", "--dry-run", "-a"][..],
        &["repack", "--dry-run", "-a"],
        &[
            "repack",
            "--drop-filtered",
            "--filter=blob:limit=1k",
            "--filter-to=x",
        ],
        &["repack", "--drop-filtered", "--filter=blob:limit=1k"],
        &[
            "repack",
            "--drop-filtered",
            "--filter=blob:limit=1k",
            "-a",
            "-b",
        ],
        &["repack", "--drop-filtered", "--filter=blob:none", "-a"],
        &["repack", "--drop-filtered", "--filter=blob:limit=1k", "-a"],
        &["repack", "--filter-to=x", "-a", "-d"],
        &["repack", "-a", "-d", "--filter=bogus"],
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

/// `--drop-filtered` in a partial clone: the large blobs of the promisor
/// packs are listed (`--dry-run`) and dropped; small and local ones stay;
/// an index entry or an operation in progress stops it.
#[test]
fn repack_drop_filtered_drops_large_promisor_blobs() {
    let fx = repo();
    if fx.is_skipped() {
        return;
    }
    let setup = r#"G="$GIT" R="$RGIT"
"$G" config core.repositoryformatversion 1
"$G" config extensions.partialclone origin
"$G" config remote.origin.promisor true
"$G" config remote.origin.url .
pb() {
  head -c $1 /dev/zero | tr '\0' $2 >content
  oid=$("$G" hash-object -w content)
  h=$(echo $oid | "$G" pack-objects .git/objects/pack/pack)
  : >.git/objects/pack/pack-$h.promisor
  rm .git/objects/$(echo $oid | cut -c1-2)/$(echo $oid | cut -c3-)
  echo $oid
}
big=$(pb 3000 b); big2=$(pb 2000 c); small=$(pb 500 s)
head -c 4000 /dev/zero | tr '\0' l >local; loc=$("$G" hash-object -w local)
"#;
    let out = sh(
        &fx,
        &format!(
            r#"{setup}
"$R" -c repack.writeBitmaps=false repack --drop-filtered --filter=blob:limit=1k --dry-run -a | sort >actual
printf '%s\n' $big $big2 | sort >expect
cmp expect actual && echo listed
"$G" cat-file -e $big && echo still-there
"$R" -c repack.writeBitmaps=false repack --drop-filtered --filter=blob:limit=1k -a -q
"$G" cat-file --batch-all-objects --batch-check='%(objectname)' >present 2>/dev/null
grep -c $big present || true
grep -c $small present
grep -c $loc present
"$G" fsck --no-dangling >/dev/null 2>&1 && echo fsck-ok
"#
        ),
    );
    assert_eq!(out, "listed\nstill-there\n0\n1\n1\nfsck-ok\n");

    let fx = repo();
    let out = sh(
        &fx,
        &format!(
            r#"{setup}
head -c 3000 /dev/zero | tr '\0' b >tracked.bin
"$G" add tracked.bin
"$R" repack --drop-filtered --filter=blob:limit=1k --dry-run -a 2>&1 | sed "s/$big/OID/"
"$G" reset -q
: >.git/MERGE_HEAD
"$R" repack --drop-filtered --filter=blob:limit=1k --dry-run -a 2>&1 || echo failed
"#
        ),
    );
    assert_eq!(
        out,
        "fatal: cannot drop 'tracked.bin' (OID): it is referenced by the current index\n\
         fatal: --drop-filtered cannot be used while another operation (merge, rebase, am, cherry-pick, revert, or bisect) is in progress\nfailed\n"
    );
}

/// `objects/info/packs` with several packs lists them in git's order:
/// directory order, and without `-f` the order of the existing file
/// (#711). Both binaries run on the same directory, so they see the same
/// `readdir` order.
#[test]
fn update_server_info_pack_order_matches_git() {
    let fx = fixture()
        .git(["init", "-q"])
        .git(["config", "maintenance.auto", "false"])
        .git(["config", "gc.auto", "0"]);
    if fx.is_skipped() {
        return;
    }
    let script = r#"
mkpack() {
    echo "$1" >"f$1"
    "$GIT" add "f$1"
    "$GIT" commit -q -m "$1"
    "$GIT" rev-parse "HEAD:f$1" | "$GIT" pack-objects -q .git/objects/pack/pack
}
# Move an index to the end of the directory order (on most file systems).
rotate() {
    f=.git/objects/pack/pack-$1.idx
    mv "$f" "$f.x" && mv "$f.x" "$f"
}
# Run update-server-info with both binaries from the same starting file.
both() {
    cp .git/objects/info/packs start 2>/dev/null || rm -f start
    for g in "$GIT" "$RGIT"; do
        if test -f start; then cp start .git/objects/info/packs; else rm -f .git/objects/info/packs; fi
        "$g" update-server-info "$@" 2>err
        echo "== $*"
        cat err .git/objects/info/packs
    done
}
p1=$(mkpack 1)
p2=$(mkpack 2)
"$GIT" update-server-info
mkpack 3 >/dev/null
p4=$(mkpack 4)
mkpack 5 >/dev/null
rotate "$p1"
rotate "$p4"
both
both -f
printf 'X garbage\n' >>.git/objects/info/packs
both
cp start .git/objects/info/packs
printf 'P %s\n' pack-0000000000000000000000000000000000000000.pack >>.git/objects/info/packs
both
printf 'D foo\n' >.git/objects/info/packs
both
cp start .git/objects/info/packs
printf 'P %s.pack\n' "pack-$p1" "pack-$p2" "pack-$p1" >.git/objects/info/packs
both
mkpack 6 >/dev/null
rotate "$p2"
both
both -f
"#;
    let out = sh(&fx, script);
    let runs: Vec<&str> = out.split("== ").skip(1).collect();
    assert_eq!(runs.len(), 16, "{out}");
    for pair in runs.chunks(2) {
        assert_eq!(pair[0], pair[1], "git vs rgit:\n{out}");
    }
}
