// SPDX-License-Identifier: MIT
//! The reftable writer against the reference git: git writes a table,
//! this crate reads it and writes the same records again with the same
//! options. The result has to be the same file.
//!
//! Log blocks are deflated, and the bytes a deflater produces depend on
//! the zlib implementation git was built with. Where the first log block
//! shows that git's zlib and ours agree, the files are compared byte for
//! byte; otherwise everything in front of the log section and the inflated
//! log blocks are compared.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use rgit_difftest::Fixture;
use rgit_refs::reftable::writer::{WriteOptions, Writer};
use rgit_refs::reftable::{BlockInfo, Table};

struct Repo {
    fx: Fixture,
}

impl Repo {
    /// A reftable repository with one commit; `None` without a reference
    /// git that knows the format.
    fn new(logs: bool) -> Option<Repo> {
        let fx = Fixture::new();
        if fx.is_skipped() || !right_version(&fx) {
            return None;
        }
        let out = fx.git_output(["init", "-q", "--ref-format=reftable", "."]);
        if !out.status.success() {
            eprintln!("skipped: the reference git has no reftable support");
            return None;
        }
        let fx = fx
            .env("GIT_TEST_REFTABLE_AUTOCOMPACTION", "false")
            .git([
                "config",
                "core.logAllRefUpdates",
                if logs { "true" } else { "false" },
            ])
            .git(["commit", "-q", "--allow-empty", "-m", "one"]);
        Some(Repo { fx })
    }

    fn git_stdin(&self, args: &[&str], stdin: &[u8]) -> String {
        let work = self.fx.work_dir();
        let mut child = Command::new(self.fx.reference_git())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.fx.home_dir())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .env("GIT_TEST_REFTABLE_AUTOCOMPACTION", "false")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_COMMITTER_DATE", "1700000000 +0130")
            .args(args)
            .current_dir(&work)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn git");
        child.stdin.take().unwrap().write_all(stdin).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn reftable_dir(&self) -> PathBuf {
        self.fx.work_dir().join(".git/reftable")
    }

    /// The tables of the stack, oldest first.
    fn tables(&self) -> Vec<Arc<Table>> {
        let dir = self.reftable_dir();
        let list = std::fs::read_to_string(dir.join("tables.list")).unwrap();
        list.lines()
            .map(|name| read_table(&dir.join(name)))
            .collect()
    }
}

/// The layout choices compared here are those of git 2.56.
fn right_version(fx: &Fixture) -> bool {
    let version = String::from_utf8_lossy(&fx.git_output(["version"]).stdout).into_owned();
    let right = version.contains(" 2.56.");
    if !right {
        eprintln!(
            "skipped: the comparison needs git 2.56, found {}",
            version.trim()
        );
    }
    right
}

fn read_table(path: &Path) -> Arc<Table> {
    let data = std::fs::read(path).unwrap();
    Arc::new(Table::parse(path.file_name().unwrap().to_string_lossy(), data).expect("parse"))
}

/// Writes the records of `table` again.
fn rewrite(table: &Arc<Table>, restart_interval: u16, index_objects: bool) -> Vec<u8> {
    let mut opts = WriteOptions::new(table.hash_kind());
    opts.block_size = table.block_size() as u32;
    opts.restart_interval = restart_interval;
    opts.index_objects = index_objects;
    let mut w = Writer::new(opts, table.min_update_index(), table.max_update_index());
    let mut refs = table.refs();
    while let Some(r) = refs.next_record().unwrap() {
        w.add_ref(&r).unwrap();
    }
    let mut logs = table.logs();
    while let Some(l) = logs.next_record().unwrap() {
        w.add_log(&l).unwrap();
    }
    w.finish().unwrap()
}

fn describe(blocks: &[BlockInfo]) -> String {
    blocks
        .iter()
        .map(|b| format!("{} len={} restarts={}\n", b.typ as char, b.len, b.restarts))
        .collect()
}

/// The inflated log records and the log index keys of a table.
fn log_content(table: &Arc<Table>) -> Vec<String> {
    let mut out = Vec::new();
    let mut logs = table.logs();
    while let Some(l) = logs.next_record().unwrap() {
        out.push(format!("{l:?}"));
    }
    out
}

fn assert_same_table(label: &str, git: &Arc<Table>, ours: &[u8]) {
    if git.bytes() == ours {
        return;
    }
    let mine = Arc::new(Table::parse("rgit", ours.to_vec()).expect("own table parses"));
    let (a, b) = (git.blocks().unwrap(), mine.blocks().unwrap());
    assert_eq!(describe(&a), describe(&b), "{label}: block layout");
    // Up to the first log block the files do not depend on zlib.
    let first_log = a.iter().position(|x| x.typ == b'g');
    let Some(first_log) = first_log else {
        panic!("{label}: tables without log blocks differ");
    };
    let split = if first_log == 0 {
        git.bytes().len().min(24)
    } else {
        a[first_log].offset as usize
    };
    assert_eq!(
        git.bytes()[..split],
        ours[..split],
        "{label}: bytes in front of the log section"
    );
    assert_eq!(log_content(git), log_content(&mine), "{label}: log records");
    // Same deflater: then every byte has to match.
    let end = |t: &Arc<Table>, blocks: &[BlockInfo]| match blocks.get(first_log + 1) {
        Some(next) => next.offset as usize,
        None => t.bytes().len() - if t.version() == 2 { 72 } else { 68 },
    };
    let git_first = &git.bytes()[split.max(a[first_log].offset as usize)..end(git, &a)];
    let our_first = &ours[split.max(b[first_log].offset as usize)..end(&mine, &b)];
    assert_ne!(
        git_first, our_first,
        "{label}: same deflate output for the first log block, but the files differ"
    );
}

fn check(repo: &Repo, label: &str, restart_interval: u16, index_objects: bool) {
    for table in repo.tables() {
        let ours = rewrite(&table, restart_interval, index_objects);
        assert_same_table(&format!("{label} {}", table.name()), &table, &ours);
    }
}

fn branches(repo: &Repo, n: usize) {
    let input: String = (1..=n)
        .map(|i| format!("update refs/heads/branch-{i} HEAD\n"))
        .collect();
    repo.git_stdin(&["update-ref", "--stdin"], input.as_bytes());
}

/// `n` references to distinct blobs plus an annotated tag.
fn blobs(repo: &Repo, n: usize) {
    let mut input = String::new();
    for i in 1..=n {
        let id = repo.git_stdin(
            &["hash-object", "-w", "--stdin"],
            format!("blob {i}\n").as_bytes(),
        );
        input.push_str(&format!("update refs/blobs/b-{i} {}\n", id.trim()));
    }
    repo.git_stdin(&["update-ref", "--stdin"], input.as_bytes());
    repo.git_stdin(&["tag", "-a", "-m", "annotated", "atag"], b"");
}

fn pack(repo: &Repo, config: &[&str]) {
    let mut args: Vec<&str> = Vec::new();
    for c in config {
        args.push("-c");
        args.push(c);
    }
    args.push("pack-refs");
    repo.git_stdin(&args, b"");
}

#[test]
fn single_transactions() {
    let Some(repo) = Repo::new(true) else { return };
    repo.git_stdin(&["branch", "topic"], b"");
    repo.git_stdin(&["tag", "-a", "-m", "msg", "v1"], b"");
    repo.git_stdin(&["tag", "light"], b"");
    repo.git_stdin(
        &["update-ref", "-m", "a message", "refs/heads/other", "HEAD"],
        b"",
    );
    repo.git_stdin(&["symbolic-ref", "refs/heads/sym", "refs/heads/other"], b"");
    repo.git_stdin(&["branch", "-m", "other", "renamed"], b"");
    repo.git_stdin(
        &["reflog", "expire", "--expire=all", "refs/heads/topic"],
        b"",
    );
    repo.git_stdin(&["update-ref", "-d", "refs/heads/topic"], b"");
    repo.git_stdin(&["checkout", "-q", "--detach"], b"");
    assert!(repo.tables().len() > 5);
    check(&repo, "single", 16, true);
}

#[test]
fn compacted_with_logs() {
    for (n, block_size) in [
        (1, 4096),
        (200, 4096),
        (3, 300),
        (6, 300),
        (12, 300),
        (100, 400),
        (300, 300),
    ] {
        let Some(repo) = Repo::new(true) else { return };
        branches(&repo, n);
        pack(&repo, &[&format!("reftable.blockSize={block_size}")]);
        assert_eq!(repo.tables().len(), 1);
        check(&repo, &format!("logs n={n} bs={block_size}"), 16, true);
    }
}

#[test]
fn compacted_without_logs() {
    for (n, block_size) in [
        (2, 100),
        (3, 100),
        (5, 100),
        (40, 100),
        (150, 100),
        (500, 100),
        (1000, 4096),
    ] {
        let Some(repo) = Repo::new(false) else { return };
        branches(&repo, n);
        pack(&repo, &[&format!("reftable.blockSize={block_size}")]);
        check(&repo, &format!("refs n={n} bs={block_size}"), 16, true);
    }
}

#[test]
fn object_blocks() {
    for (n, block_size) in [(30, 200), (600, 200), (300, 4096)] {
        let Some(repo) = Repo::new(false) else { return };
        blobs(&repo, n);
        pack(&repo, &[&format!("reftable.blockSize={block_size}")]);
        check(&repo, &format!("objs n={n} bs={block_size}"), 16, true);
    }
}

#[test]
fn write_options() {
    let Some(repo) = Repo::new(true) else { return };
    branches(&repo, 10);
    pack(&repo, &["reftable.restartInterval=1"]);
    check(&repo, "restart interval 1", 1, true);

    let Some(repo) = Repo::new(false) else { return };
    branches(&repo, 5);
    pack(
        &repo,
        &["reftable.blockSize=100", "reftable.indexObjects=false"],
    );
    check(&repo, "no object index", 16, false);

    let Some(repo) = Repo::new(true) else { return };
    blobs(&repo, 100);
    pack(
        &repo,
        &["reftable.blockSize=1000", "reftable.restartInterval=3"],
    );
    check(&repo, "restart interval 3", 3, true);
}

#[test]
fn sha256_tables() {
    let fx = Fixture::new();
    if fx.is_skipped() || !right_version(&fx) {
        return;
    }
    let out = fx.git_output([
        "init",
        "-q",
        "--ref-format=reftable",
        "--object-format=sha256",
        ".",
    ]);
    if !out.status.success() {
        return;
    }
    let fx = fx
        .env("GIT_TEST_REFTABLE_AUTOCOMPACTION", "false")
        .git(["commit", "-q", "--allow-empty", "-m", "one"])
        .git(["tag", "-a", "-m", "t", "v1"])
        .git(["branch", "topic"]);
    let repo = Repo { fx };
    check(&repo, "sha256", 16, true);
    pack(&repo, &[]);
    check(&repo, "sha256 packed", 16, true);
}
