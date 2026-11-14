// SPDX-License-Identifier: MIT

//! Differential check on real history: for the last N first-parent commits
//! of a repository, render `diff-tree -p --no-renames --full-index c^ c`
//! with rgit-diff (blobs and the raw tree diff come from the `git` binary)
//! and compare with git's own output.
//!
//! usage: commitcheck REPO COUNT [OUT_DIR]
//! With `DIFF_ALGORITHM=<name>` in the environment both sides use that
//! algorithm (`--diff-algorithm=<name>`).
//! Prints one line per mismatching commit; with OUT_DIR the expected and
//! actual patches are written there.

use rgit_diff::{Algorithm, DiffOptions, MODE_SYMLINK, PatchFile, PatchOptions, write_file_patch};
use rgit_hash::{HashKind, ObjectId};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

fn git(repo: &Path, args: &[&str]) -> Vec<u8> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .args(args)
        .output()
        .expect("run git");
    assert!(out.status.success(), "git {args:?} failed");
    out.stdout
}

struct CatFile {
    child: std::process::Child,
    out: BufReader<std::process::ChildStdout>,
}

impl CatFile {
    fn new(repo: &Path) -> Self {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("cat-file");
        let out = BufReader::new(child.stdout.take().unwrap());
        CatFile { child, out }
    }

    fn get(&mut self, oid: &str) -> Vec<u8> {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{oid}").unwrap();
        stdin.flush().unwrap();
        let mut header = String::new();
        self.out.read_line(&mut header).unwrap();
        let size: usize = header.split_whitespace().nth(2).unwrap().parse().unwrap();
        let mut data = vec![0; size + 1];
        self.out.read_exact(&mut data).unwrap();
        data.pop();
        data
    }
}

/// Removes the function-name text after `@@ ... @@` (for repositories whose
/// attributes select language-specific hunk headers).
fn strip_funcnames(patch: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(patch.len());
    for line in patch.split_inclusive(|&c| c == b'\n') {
        if line.starts_with(b"@@ -") {
            if let Some(p) = line[3..].windows(3).position(|w| w == b" @@") {
                out.extend_from_slice(&line[..3 + p + 3]);
                out.push(b'\n');
                continue;
            }
        }
        out.extend_from_slice(line);
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let repo = Path::new(&args[1]);
    let count: usize = args[2].parse().unwrap();
    let out_dir = args.get(3).map(Path::new);
    let revs = git(
        repo,
        &[
            "rev-list",
            "--first-parent",
            "--max-count",
            &count.to_string(),
            "HEAD",
        ],
    );
    let mut cat = CatFile::new(repo);
    let algorithm = std::env::var("DIFF_ALGORITHM").unwrap_or_else(|_| "myers".into());
    let (alg, minimal) = Algorithm::parse(&algorithm).expect("DIFF_ALGORITHM");
    let alg_arg = format!("--diff-algorithm={algorithm}");
    let opts = PatchOptions {
        abbrev: 40,
        diff: DiffOptions {
            algorithm: alg,
            minimal,
            ..DiffOptions::default()
        },
        ..PatchOptions::default()
    };
    let (mut ok, mut bad) = (0, 0);
    for c in String::from_utf8(revs).unwrap().lines() {
        let parent = format!("{c}^");
        if Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "-q", "--verify", &parent])
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            continue;
        }
        let want = git(
            repo,
            &[
                "diff-tree",
                "-p",
                "--no-renames",
                "--full-index",
                "--no-color",
                &alg_arg,
                &parent,
                c,
            ],
        );
        let raw = git(
            repo,
            &[
                "diff-tree",
                "-r",
                "-z",
                "--no-renames",
                "--no-abbrev",
                &parent,
                c,
            ],
        );
        let mut got = Vec::new();
        let mut fields = raw.split(|&b| b == 0).filter(|f| !f.is_empty());
        while let Some(meta) = fields.next() {
            let path = fields.next().unwrap();
            let meta = String::from_utf8_lossy(meta);
            let parts: Vec<&str> = meta.trim_start_matches(':').split(' ').collect();
            let (om, nm) = (
                u32::from_str_radix(parts[0], 8).unwrap(),
                u32::from_str_radix(parts[1], 8).unwrap(),
            );
            if om == 0o160000 || nm == 0o160000 {
                // Submodules are out of scope here.
                continue;
            }
            let (oo, no) = (parts[2], parts[3]);
            let od = if om != 0 { cat.get(oo) } else { Vec::new() };
            let nd = if nm != 0 { cat.get(no) } else { Vec::new() };
            let oid = |h: &str| ObjectId::from_hex(HashKind::Sha1, h.as_bytes()).unwrap();
            let o = (om != 0).then(|| PatchFile {
                path,
                mode: om,
                oid: oid(oo),
                data: &od,
            });
            let n = (nm != 0).then(|| PatchFile {
                path,
                mode: nm,
                oid: oid(no),
                data: &nd,
            });
            let type_change = om != 0 && nm != 0 && ((om == MODE_SYMLINK) != (nm == MODE_SYMLINK));
            if type_change {
                write_file_patch(&mut got, o.as_ref(), None, &opts);
                write_file_patch(&mut got, None, n.as_ref(), &opts);
            } else {
                write_file_patch(&mut got, o.as_ref(), n.as_ref(), &opts);
            }
        }
        let want_body = match want.windows(11).position(|w| w == b"diff --git ") {
            Some(p) => &want[p..],
            None => &want[want.len()..],
        };
        let strip = std::env::var_os("STRIP_FUNCNAME").is_some();
        let (got, want_body) = if strip {
            (strip_funcnames(&got), strip_funcnames(want_body))
        } else {
            (got, want_body.to_vec())
        };
        if got == want_body {
            ok += 1;
        } else {
            bad += 1;
            println!("MISMATCH {c}");
            if let Some(d) = out_dir {
                std::fs::create_dir_all(d).unwrap();
                std::fs::write(d.join(format!("{c}.git")), &want_body).unwrap();
                std::fs::write(d.join(format!("{c}.rgit")), &got).unwrap();
            }
        }
    }
    println!("RESULT {ok} equal, {bad} different");
}
