// SPDX-License-Identifier: MIT
//! Large objects (`core.bigFileThreshold`, #89): files above the threshold
//! are stored deflated in a pack when added, streamed to the work tree and
//! to standard output, and shown as binary by diff. Differential tests run
//! with a small threshold; the memory test with a multi-GiB sparse file
//! runs only with `RGIT_TEST_LARGE_FILES=1`.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use rgit_difftest::{Check, Fixture, Normalizer};

const THRESHOLD: &str = "core.bigFileThreshold=1k";

/// Content of `len` bytes that does not compress to nothing.
fn content(seed: u32, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..len)
        .map(|i| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            if i % 61 == 60 {
                b'\n'
            } else {
                b'a' + (x % 26) as u8
            }
        })
        .collect()
}

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .git(["config", "core.bigFileThreshold", "1k"])
        .write("big1", content(1, 200_000))
        .write("big2", content(1, 200_000))
        .write("big3", content(2, 150_000))
        .write("small", "small\n")
}

/// [`fixture`] with two commits that change `big1`.
fn committed() -> Fixture {
    let fx = fixture()
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .tick();
    let mut changed = content(1, 200_000);
    changed.extend_from_slice(b"more\n");
    fx.write("big1", changed)
        .git(["commit", "-q", "-a", "-m", "two"])
        .tick()
}

#[test]
fn add_stores_the_same_objects() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    fx.compare(["add", "."])
        .checks([
            Check::ExitCode,
            Check::Stdout,
            Check::Stderr,
            Check::Objects,
            Check::Fsck,
            Check::Index,
        ])
        .run();
}

#[test]
fn hash_object_streams() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    fx.compare(["hash-object", "big1", "small", "big3"])
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["hash-object", "-w", "big1", "big3"])
        .checks([Check::ExitCode, Check::Stdout, Check::Objects, Check::Fsck])
        .run();
}

#[test]
fn negative_threshold_is_fatal() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    fx.compare(["-c", "core.bigFileThreshold=-1", "hash-object", "small"])
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn checkout_streams_to_the_work_tree() {
    let fx = committed();
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx.remove("big1").remove("big3");
    fx.compare(["checkout", "--", "big1", "big3"])
        .checks([Check::ExitCode, Check::Worktree, Check::Index])
        .run();
    let mut fx = committed();
    fx.compare(["checkout", "-q", "HEAD^"])
        .checks([Check::ExitCode, Check::Worktree, Check::Index])
        .run();
}

#[test]
fn cat_file_and_show_stream() {
    let mut fx = committed();
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["cat-file", "blob", "HEAD:big1"][..],
        &["cat-file", "-p", "HEAD^:big1"],
        &["cat-file", "-s", "HEAD:big1"],
        &["show", "HEAD:big3"],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
    fx.compare(["cat-file", "--batch"])
        .stdin("HEAD:big1\nHEAD:small\n")
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn diff_treats_large_files_as_binary() {
    let mut fx = committed();
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["diff", "HEAD^", "HEAD"][..],
        &["diff", "--stat", "HEAD^", "HEAD"],
        &["diff", "--numstat", "HEAD^", "HEAD"],
        &["diff", "--raw", "HEAD^", "HEAD"],
        &["log", "-p", "--format=%s"],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
    // `--binary` and `--text` read large contents and diff them (#654).
    for args in [
        &["diff", "--binary", "HEAD^", "HEAD"][..],
        &["diff", "-a", "HEAD^", "HEAD"],
        &["diff", "--binary", "--stat", "HEAD^", "HEAD"],
        &["format-patch", "--stdout", "-1", "HEAD"],
    ] {
        fx.compare(args)
            .checks(Check::OUTPUT)
            .normalize(Normalizer::regex(r"(?m)^2\.[0-9]+\.[0-9]+.*$", "VERSION"))
            .run();
    }
    let mut bin = content(4, 3000);
    bin[0] = 0;
    let mut fx = fx
        .write("bin", &bin)
        .git(["add", "bin"])
        .git(["commit", "-q", "-m", "bin"])
        .tick();
    bin[1] = 0;
    fx = fx.write("bin", &bin);
    fx.compare(["diff", "--binary"]).checks(Check::OUTPUT).run();
    let mut fx = committed().write("big3", content(3, 150_000));
    fx.compare(["diff"]).checks(Check::OUTPUT).run();
    fx.compare(["diff", "--stat"]).checks(Check::OUTPUT).run();
}

#[test]
fn fsck_and_repack_with_large_blobs() {
    let mut fx = committed();
    if fx.is_skipped() {
        return;
    }
    fx.compare(["fsck"]).checks(Check::OUTPUT).run();
    fx.compare(["repack", "-a", "-d", "-q"])
        .checks([Check::ExitCode, Check::Objects, Check::Fsck])
        .normalize_state(Normalizer::pack_names())
        .run();
}

// ---------------------------------------------------------------------------
// Pack layout and memory, run with rgit alone.

struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Repo {
    fn new() -> Repo {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("r");
        let r = Repo { _dir: dir, root };
        std::fs::create_dir(&r.root).unwrap();
        r.ok(&["init", "-q"]);
        r
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_rgit"));
        // Never read or write the real user configuration.
        c.env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join(".config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@example.com")
            .args(args)
            .current_dir(&self.root);
        c
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn files(&self, dir: &str, suffix: &str) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![self.root.join(".git/objects").join(dir)];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if p.file_name().is_some_and(|n| n != "pack" && n != "info") {
                        stack.push(p);
                    }
                } else if p.to_string_lossy().ends_with(suffix) {
                    out.push(p);
                }
            }
        }
        out
    }
}

#[test]
fn large_files_go_into_one_pack() {
    let r = Repo::new();
    std::fs::write(r.root.join("a"), content(5, 100_000)).unwrap();
    std::fs::write(r.root.join("b"), content(5, 100_000)).unwrap();
    std::fs::write(r.root.join("c"), content(6, 100_000)).unwrap();
    r.ok(&["-c", THRESHOLD, "add", "a", "b", "c"]);
    // Two distinct objects in one pack, nothing loose.
    let idx = r.files("pack", ".idx");
    assert_eq!(idx.len(), 1, "{idx:?}");
    assert!(r.files("", "").is_empty(), "{:?}", r.files("", ""));
    let shown = r
        .cmd(&["show-index"])
        .stdin(std::fs::File::open(&idx[0]).unwrap())
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&shown.stdout).lines().count(), 2);
    // The same content again: no new pack.
    std::fs::write(r.root.join("d"), content(6, 100_000)).unwrap();
    r.ok(&["-c", THRESHOLD, "add", "d"]);
    assert_eq!(r.files("pack", ".idx").len(), 1);
    // pack.packSizeLimit starts a new pack when the next object does not fit.
    std::fs::write(r.root.join("e"), content(7, 100_000)).unwrap();
    std::fs::write(r.root.join("f"), content(8, 100_000)).unwrap();
    r.ok(&[
        "-c",
        THRESHOLD,
        "-c",
        "pack.packSizeLimit=150k",
        "add",
        "e",
        "f",
    ]);
    assert_eq!(r.files("pack", ".idx").len(), 3);
    assert_eq!(
        r.ok(&["cat-file", "blob", ":f"]).as_bytes(),
        content(8, 100_000)
    );
    r.ok(&["fsck", "--strict"]);
}

#[test]
fn alloc_limit_stops_whole_reads() {
    let r = Repo::new();
    std::fs::write(r.root.join("a"), content(9, 300_000)).unwrap();
    let id = r.ok(&["-c", THRESHOLD, "hash-object", "-w", "a"]);
    let id = id.trim();
    // Streamed: fine under the limit.
    let out = r
        .cmd(&["-c", THRESHOLD, "cat-file", "blob", id])
        .env("GIT_ALLOC_LIMIT", "100k")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(out.stdout, content(9, 300_000));
    // Read whole (above the default threshold): the limit is fatal.
    let out = r
        .cmd(&["cat-file", "-p", id])
        .env("GIT_ALLOC_LIMIT", "100k")
        .stdout(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("attempting to allocate 300001 over limit 102400"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `archive`, a `file://` clone and `index-pack` outside a repository
/// read large packed blobs as streams (t1050 23, 26-28; #654).
#[test]
fn archive_clone_and_index_pack_stream() {
    let r = Repo::new();
    // As in t1050 (the reference upload-pack, which a file:// clone
    // runs, needs some memory too).
    std::fs::write(r.root.join("a"), " ".repeat(2_000_000) + "X").unwrap();
    std::fs::write(r.root.join("b"), "small\n").unwrap();
    r.ok(&["config", "core.bigFileThreshold", "1k"]);
    r.ok(&["add", "a", "b"]);
    r.ok(&["commit", "-q", "-m", "one"]);
    assert_eq!(
        r.files("pack", ".pack").len(),
        1,
        "the large blob is packed"
    );
    let limited = |args: &[&str]| {
        let out = r
            .cmd(args)
            .env("GIT_ALLOC_LIMIT", "1500k")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    };
    for format in [
        &["--format=tar"][..],
        &["--format=zip", "-0"],
        &["--format=zip"],
    ] {
        let mut args = vec!["archive"];
        args.extend_from_slice(format);
        args.push("HEAD");
        let whole = r.cmd(&args).output().unwrap().stdout;
        assert_eq!(limited(&args), whole, "{format:?}");
    }
    let url = format!("file://{}", r.root.join(".git").display());
    let dest = r.root.join("clone");
    limited(&[
        "-c",
        "core.bigFileThreshold=1k",
        "clone",
        "-q",
        &url,
        dest.to_str().unwrap(),
    ]);
    let pack = std::fs::read_dir(dest.join(".git/objects/pack"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "pack"))
        .expect("a pack");
    let out = r
        .cmd(&[
            "-c",
            "core.bigFileThreshold=1k",
            "index-pack",
            "--object-format=sha1",
            "--strict",
            "--verify",
            pack.to_str().unwrap(),
        ])
        .env("GIT_DIR", "non-existent")
        .env("GIT_ALLOC_LIMIT", "1500k")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Peak resident memory (KiB) of a finished child, from `wait4`.
#[cfg(target_os = "linux")]
fn run_measured(mut c: Command) -> (std::process::ExitStatus, i64) {
    use std::os::unix::process::ExitStatusExt;
    let child = c.stdout(Stdio::null()).spawn().unwrap();
    let pid = child.id() as libc::pid_t;
    let mut status = 0;
    // SAFETY: an all-zero `rusage` is a valid value, and `wait4` only
    // writes into the two out-parameters, which outlive the call.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `pid` is our own child, not yet reaped.
    let r = unsafe { libc::wait4(pid, &mut status, 0, &mut usage) };
    assert_eq!(r, pid);
    std::mem::forget(child);
    (std::process::ExitStatus::from_raw(status), usage.ru_maxrss)
}

/// `RGIT_TEST_LARGE_FILES=1`: adding and checking out a sparse file of
/// `RGIT_TEST_LARGE_SIZE` bytes (default 2 GiB + 1) keeps the peak
/// resident memory below 256 MiB.
#[cfg(target_os = "linux")]
#[test]
fn large_file_memory_stays_bounded() {
    if std::env::var_os("RGIT_TEST_LARGE_FILES").is_none_or(|v| v != "1") {
        return;
    }
    const LIMIT_KIB: i64 = 256 << 10;
    let size: u64 = std::env::var("RGIT_TEST_LARGE_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or((2 << 30) + 1);
    let r = Repo::new();
    let path = r.root.join("big");
    let f = std::fs::File::create(&path).unwrap();
    f.set_len(size).unwrap();
    drop(f);
    let check = |what: &str, c: Command| {
        let (status, kib) = run_measured(c);
        assert!(status.success(), "{what} failed");
        eprintln!("{what}: peak RSS {kib} KiB");
        assert!(kib < LIMIT_KIB, "{what}: peak RSS {kib} KiB");
    };
    check("add", r.cmd(&["add", "big"]));
    assert!(r.files("", "").is_empty());
    r.ok(&["commit", "-q", "-m", "big"]);
    std::fs::remove_file(&path).unwrap();
    check("checkout", r.cmd(&["checkout", "--", "big"]));
    assert_eq!(std::fs::metadata(&path).unwrap().len(), size);
    check("cat-file", r.cmd(&["cat-file", "blob", "HEAD:big"]));
    check("fsck", r.cmd(&["fsck"]));
}
