// SPDX-License-Identifier: MIT

//! Shared helpers for differential tests against the `git` binary.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// Small deterministic PRNG (xorshift64*).
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `0..n` (n > 0).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    pub fn chance(&mut self, num: usize, den: usize) -> bool {
        self.below(den) < num
    }
}

/// Returns true if a usable `git` binary is on `PATH`.
pub fn have_git() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Runs `git diff --no-index` on two files and returns stdout.
pub fn git_diff(dir: &Path, a: &Path, b: &Path, extra: &[&str]) -> Vec<u8> {
    let out = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir.join(".config"))
        .env("LC_ALL", "C")
        .args([
            "-c",
            "core.quotePath=true",
            "diff",
            "--no-index",
            "--no-color",
        ])
        .args(extra)
        .arg("--")
        .arg(a)
        .arg(b)
        .output()
        .expect("run git");
    assert!(
        out.status.code() == Some(0) || out.status.code() == Some(1),
        "git failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// Strips the patch header (everything before the first hunk) from git's
/// output for a single file pair.
pub fn hunks_of(git_out: &[u8]) -> Vec<u8> {
    let mut pos = 0;
    while pos < git_out.len() {
        if git_out[pos..].starts_with(b"@@ ") || git_out[pos..].starts_with(b"Binary files ") {
            return git_out[pos..].to_vec();
        }
        match git_out[pos..].iter().position(|&c| c == b'\n') {
            Some(p) => pos += p + 1,
            None => break,
        }
    }
    Vec::new()
}

/// A scratch directory under the target directory, removed on drop.
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(name: &str) -> Self {
        let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
        let dir = base.join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Kinds of generated inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gen {
    /// Short files over a tiny alphabet: many equally short edit scripts.
    Tiny,
    /// A random base file and an edited copy.
    Edit,
    /// Code-like text with indentation and blank lines (indent heuristic).
    Code,
    /// Large inputs with many differences (cost limits of the search).
    Big,
    /// Two unrelated large random files.
    BigRand,
    /// Edits near the start, long common tail (context 0 trimming).
    Tail,
}

impl Gen {
    pub fn parse(s: &str) -> Gen {
        match s {
            "tiny" => Gen::Tiny,
            "edit" => Gen::Edit,
            "code" => Gen::Code,
            "big" => Gen::Big,
            "bigrand" => Gen::BigRand,
            "tail" => Gen::Tail,
            _ => panic!("unknown generator {s}"),
        }
    }
}

fn tiny_file(rng: &mut Rng, alpha: usize, max: usize) -> Vec<Vec<u8>> {
    let n = rng.below(max + 1);
    (0..n)
        .map(|_| vec![b'a' + rng.below(alpha) as u8, b'\n'])
        .collect()
}

fn code_line(rng: &mut Rng) -> Vec<u8> {
    if rng.chance(1, 6) {
        // blank line, sometimes with trailing whitespace
        return if rng.chance(1, 5) {
            b"  \n".to_vec()
        } else {
            b"\n".to_vec()
        };
    }
    let depth = rng.below(4);
    let mut l = Vec::new();
    let tab = rng.chance(1, 4);
    for _ in 0..depth {
        if tab {
            l.push(b'\t');
        } else {
            l.extend_from_slice(b"  ");
        }
    }
    const WORDS: &[&str] = &[
        "}",
        "{",
        "fn f() {",
        "if x {",
        "return y;",
        "x = 1;",
        "foo();",
        "end",
        "bar",
        "# c",
        "int main(void)",
        "*/",
        "/*",
        "else",
        "$v",
        "_p:",
    ];
    l.extend_from_slice(WORDS[rng.below(WORDS.len())].as_bytes());
    l.push(b'\n');
    l
}

fn edit_copy(
    rng: &mut Rng,
    base: &[Vec<u8>],
    fresh: &mut dyn FnMut(&mut Rng) -> Vec<u8>,
) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < base.len() {
        match rng.below(12) {
            0 => i += 1 + rng.below(3), // delete a few
            1 => {
                for _ in 0..1 + rng.below(4) {
                    out.push(fresh(rng));
                }
            }
            2 => {
                // repeat the preceding block (creates sliders)
                let len = 1 + rng.below(4);
                let s = i.saturating_sub(len);
                for l in &base[s..i] {
                    out.push(l.clone());
                }
            }
            _ => {
                out.push(base[i].clone());
                i += 1;
            }
        }
    }
    if rng.chance(1, 6) {
        out.push(fresh(rng));
    }
    out
}

fn join(lines: &[Vec<u8>], rng: &mut Rng) -> Vec<u8> {
    let mut v: Vec<u8> = lines.concat();
    if !v.is_empty() && rng.chance(1, 8) {
        v.pop();
    }
    v
}

/// Generates one pair of file contents.
pub fn generate(kind: Gen, rng: &mut Rng) -> (Vec<u8>, Vec<u8>) {
    match kind {
        Gen::Tiny => {
            let alpha = 2 + rng.below(5);
            let max = 4 + rng.below(20);
            let a = tiny_file(rng, alpha, max);
            let b = if rng.chance(1, 2) {
                tiny_file(rng, alpha, max)
            } else {
                edit_copy(rng, &a, &mut |r| vec![b'a' + r.below(alpha) as u8, b'\n'])
            };
            (join(&a, rng), join(&b, rng))
        }
        Gen::Edit => {
            let alpha = 3 + rng.below(40);
            let n = rng.below(200);
            let a: Vec<Vec<u8>> = (0..n)
                .map(|_| format!("line {}\n", rng.below(alpha)).into_bytes())
                .collect();
            let b = edit_copy(rng, &a, &mut |r| {
                format!("line {}\n", r.below(alpha + 5)).into_bytes()
            });
            (join(&a, rng), join(&b, rng))
        }
        Gen::Big => {
            let alpha = 5 + rng.below(3000);
            let n = 300 + rng.below(4000);
            let a: Vec<Vec<u8>> = (0..n)
                .map(|_| format!("{}\n", rng.below(alpha)).into_bytes())
                .collect();
            let mut b = a.clone();
            for _ in 0..1 + rng.below(4) {
                b = edit_copy(rng, &b, &mut |r| {
                    format!("{}\n", r.below(alpha)).into_bytes()
                });
            }
            (join(&a, rng), join(&b, rng))
        }
        Gen::BigRand => {
            let alpha = 2 + rng.below(400);
            let na = rng.below(3000);
            let nb = rng.below(3000);
            let a: Vec<Vec<u8>> = (0..na)
                .map(|_| format!("{}\n", rng.below(alpha)).into_bytes())
                .collect();
            let b: Vec<Vec<u8>> = (0..nb)
                .map(|_| format!("{}\n", rng.below(alpha)).into_bytes())
                .collect();
            (join(&a, rng), join(&b, rng))
        }
        Gen::Tail => {
            let alpha = 2 + rng.below(6);
            let n = 100 + rng.below(800);
            let a: Vec<Vec<u8>> = (0..n)
                .map(|_| format!("l{}\n", rng.below(alpha)).into_bytes())
                .collect();
            let cut = rng.below(n / 3 + 1);
            let mut b = edit_copy(rng, &a[..cut], &mut |r| {
                format!("l{}\n", r.below(alpha)).into_bytes()
            });
            b.extend_from_slice(&a[cut..]);
            (join(&a, rng), join(&b, rng))
        }
        Gen::Code => {
            let n = rng.below(80);
            let a: Vec<Vec<u8>> = (0..n).map(|_| code_line(rng)).collect();
            let b = edit_copy(rng, &a, &mut code_line);
            (join(&a, rng), join(&b, rng))
        }
    }
}

/// Outcome of a differential run.
pub struct Report {
    pub cases: usize,
    pub mismatches: Vec<(u64, Vec<u8>, Vec<u8>)>,
}

/// Runs `count` generated cases starting at `seed` and compares rgit's hunks
/// with git's. `extra` are additional git options matching `opts`.
pub fn run_cases(
    name: &str,
    kind: Gen,
    seed: u64,
    count: usize,
    opts: &rgit_diff::DiffOptions,
    extra: &[&str],
    keep_dir: Option<&Path>,
) -> Report {
    let scratch = Scratch::new(name);
    let pa = scratch.0.join("a");
    let pb = scratch.0.join("b");
    let mut report = Report {
        cases: 0,
        mismatches: Vec::new(),
    };
    for s in seed..seed + count as u64 {
        let mut rng = Rng::new(s);
        let (a, b) = generate(kind, &mut rng);
        std::fs::write(&pa, &a).unwrap();
        std::fs::write(&pb, &b).unwrap();
        let want = hunks_of(&git_diff(&scratch.0, Path::new("a"), Path::new("b"), extra));
        let got = rgit_diff::diff_text(&a, &b, opts).hunks;
        report.cases += 1;
        if got != want {
            if let Some(k) = keep_dir {
                std::fs::create_dir_all(k).unwrap();
                std::fs::write(k.join(format!("{s}.a")), &a).unwrap();
                std::fs::write(k.join(format!("{s}.b")), &b).unwrap();
                std::fs::write(k.join(format!("{s}.git")), &want).unwrap();
                std::fs::write(k.join(format!("{s}.rgit")), &got).unwrap();
            }
            report.mismatches.push((s, want, got));
        }
    }
    report
}

/// Generates a merge triple (base, ours, theirs): two independent edits of
/// one base. Some cases use CRLF line ends.
pub fn generate_triple(kind: Gen, rng: &mut Rng) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let alpha = 2 + rng.below(6);
    type Fresh = Box<dyn Fn(&mut Rng) -> Vec<u8>>;
    let (base, fresh): (Vec<Vec<u8>>, Fresh) = match kind {
        Gen::Tiny => {
            let max = 4 + rng.below(20);
            (
                tiny_file(rng, alpha, max),
                Box::new(move |r: &mut Rng| vec![b'a' + r.below(alpha + 2) as u8, b'\n']),
            )
        }
        Gen::Code => {
            let n = rng.below(80);
            (
                (0..n).map(|_| code_line(rng)).collect(),
                Box::new(code_line),
            )
        }
        _ => {
            let alpha = 3 + rng.below(40);
            let n = rng.below(200);
            (
                (0..n)
                    .map(|_| format!("line {}\n", rng.below(alpha)).into_bytes())
                    .collect(),
                Box::new(move |r: &mut Rng| format!("line {}\n", r.below(alpha + 5)).into_bytes()),
            )
        }
    };
    let mut fresh = |r: &mut Rng| fresh(r);
    let mut ours = edit_copy(rng, &base, &mut fresh);
    let mut theirs = if rng.chance(1, 4) {
        // Both sides start from the same edit (identical changes).
        let t = ours.clone();
        if rng.chance(1, 2) {
            edit_copy(rng, &t, &mut fresh)
        } else {
            t
        }
    } else {
        edit_copy(rng, &base, &mut fresh)
    };
    let mut base = base;
    if rng.chance(1, 6) {
        for f in [&mut base, &mut ours, &mut theirs] {
            for l in f.iter_mut() {
                if rng.chance(3, 4) && l.ends_with(b"\n") && !l.ends_with(b"\r\n") {
                    l.insert(l.len() - 1, b'\r');
                }
            }
        }
    }
    (join(&base, rng), join(&ours, rng), join(&theirs, rng))
}
