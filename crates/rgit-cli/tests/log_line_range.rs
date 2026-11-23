// SPDX-License-Identifier: MIT
//! Differential tests of `log -L` (line-range history) against the
//! reference git: hand-written histories with functions, renames and
//! merges, and generated histories of random edits.

use rgit_difftest::{Check, Fixture};

fn compare_all(fx: &mut Fixture, cases: &[Vec<String>]) {
    let mut failures = Vec::new();
    for argv in cases {
        if let Err(m) = fx.compare(argv).checks(Check::OUTPUT).try_run() {
            failures.push(format!("{argv:?}\n{}", m.report()));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn args(list: &[&[&str]]) -> Vec<Vec<String>> {
    list.iter()
        .map(|a| a.iter().map(|s| (*s).to_owned()).collect())
        .collect()
}

const C_FILE: &str = "#include <stdio.h>\n\nint f(int x)\n{\n\tint s = 0;\n\twhile (x) {\n\t\tx >>= 1;\n\t\ts++;\n\t}\n\treturn s;\n}\n\n/*\n * A comment.\n */\n\nint main ()\n{\n\tprintf(\"%d\\n\", f(15));\n\treturn 0;\n}\n";

#[test]
fn line_log_functions_renames_and_merges() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a.c", C_FILE)
        .git(["add", "."])
        .git(["commit", "-q", "-m", "initial"])
        .tick()
        .write("a.c", C_FILE.replace("int f(int x)", "long f(long x)"))
        .git(["commit", "-q", "-a", "-m", "long f"])
        .tick()
        .write(
            "a.c",
            C_FILE
                .replace("int f(int x)", "long f(long x)")
                .replace("A comment.", "Only an example!")
                .replace("return 0;\n}\n", "return 0;\n}"),
        )
        .git(["commit", "-q", "-a", "-m", "comment, incomplete line"])
        .tick()
        .git(["checkout", "-q", "-b", "side"])
        .git(["mv", "a.c", "b.c"])
        .git(["commit", "-q", "-m", "rename"])
        .tick()
        .write(
            "b.c",
            C_FILE
                .replace("int f(int x)", "long f(long x)")
                .replace("A comment.", "Only an example!")
                .replace("x >>= 1;", "x /= 2;")
                .replace("return 0;\n}\n", "return 0;\n}"),
        )
        .git(["commit", "-q", "-a", "-m", "divide"])
        .tick()
        .git(["checkout", "-q", "main"])
        .write(
            "a.c",
            C_FILE
                .replace("int f(int x)", "long f(long x)")
                .replace("A comment.", "Only a short example!")
                .replace("return 0;\n}\n", "return 0;\n}\n/* end */\n"),
        )
        .git(["commit", "-q", "-a", "-m", "main change"])
        .tick()
        .git(["merge", "-q", "--no-edit", "side"])
        .tick();
    let cases = args(&[
        &["log", "-L", "3,11:b.c"],
        &["log", "-L", ":f:b.c"],
        &["log", "-L", ":main:b.c"],
        &["log", "-L", "/long f/,/^}/:b.c", "-L", "17,+3:b.c"],
        &["log", "-L1,3:b.c", "-L", "^:main:b.c"],
        &["log", "-L", "13,15:b.c", "--oneline"],
        &["log", "-L", "13,15:b.c", "--oneline", "--no-patch"],
        &["log", "-L", ":f:b.c", "--format=%h %p %s", "-s"],
        &[
            "log",
            "-L",
            ":f:b.c",
            "--format=%h %p %s",
            "-s",
            "--parents",
        ],
        &["log", "-L", ":f:b.c", "--graph", "--oneline"],
        &["log", "-L", ":f:b.c", "--first-parent", "--oneline"],
        &["log", "-L", ":f:b.c", "--raw", "--format="],
        &["log", "-L", ":f:b.c", "--name-status", "--format="],
        &["log", "-L", ":f:b.c", "--no-prefix", "--full-index"],
        &["log", "-L", ":main:a.c", "main~1"],
        &["log", "-L", "20,22:a.c", "main~1"],
        &["log", "-L", "20,22:a.c", "main~1", "-w"],
        &["log", "-L", "1,5:b.c", "-1"],
        &["log", "-L", ",3:b.c"],
        &["log", "-L", "8,$:b.c"],
        &["log", "-L", "b.c"],
        &["log", "-L", "1:"],
        &["log", "-L", "1:nope"],
        &["log", "-L", "100:b.c"],
        &["log", "-L", ":nomatch:b.c"],
        &["log", "-L", "1,1:b.c", "--", "b.c"],
        &["log", "-L", "1,1:b.c", "--stat"],
        &["log", "-L", "1,1:b.c", "main", "side"],
        &["log", "-L"],
    ]);
    compare_all(&mut fx, &cases);
}

/// A small deterministic pseudo-random generator.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) as usize) % n.max(1)
    }
}

fn render(lines: &[String]) -> String {
    let mut s = lines.join("\n");
    s.push('\n');
    s
}

/// One random edit: replace, insert or delete a block of lines.
fn edit(rng: &mut Lcg, lines: &mut Vec<String>, tag: &str) {
    let at = rng.next(lines.len());
    let len = 1 + rng.next(4);
    match rng.next(3) {
        0 => {
            for k in at..(at + len).min(lines.len()) {
                lines[k] = format!("\t{tag} {k}");
            }
        }
        1 => {
            for k in 0..len {
                let text = if k == 0 && rng.next(3) == 0 {
                    format!("fn_{tag}()")
                } else {
                    format!("\t{tag} new {k}")
                };
                lines.insert(at, text);
            }
        }
        _ if lines.len() > 12 => {
            lines.drain(at..(at + len).min(lines.len()));
        }
        _ => lines.push(format!("\t{tag} tail")),
    }
}

#[test]
fn line_log_generated_histories() {
    for seed in [7u64, 42, 1234] {
        let mut rng = Lcg(seed);
        let mut lines: Vec<String> = (0..40)
            .map(|i| {
                if i % 8 == 0 {
                    format!("fn_{i}()")
                } else {
                    format!("\tline {i}")
                }
            })
            .collect();
        let mut fx = Fixture::new()
            .rgit(env!("CARGO_BIN_EXE_rgit"))
            .git(["init", "-q"])
            .write("f.txt", render(&lines))
            .git(["add", "."])
            .git(["commit", "-q", "-m", "c0"]);
        for c in 1..14 {
            if c == 7 {
                // A side branch with its own edits, merged back.
                let mut side = lines.clone();
                fx = fx.git(["checkout", "-q", "-b", "side"]);
                for s in 0..3 {
                    edit(&mut rng, &mut side, &format!("s{s}"));
                    fx = fx
                        .write("f.txt", render(&side))
                        .git(["commit", "-q", "-a", "-m", &format!("side {s}")])
                        .tick();
                }
                fx = fx.git(["checkout", "-q", "main"]);
                let n = lines.len();
                lines[n - 1] = "\tmain end".to_owned();
                fx = fx
                    .write("f.txt", render(&lines))
                    .git(["commit", "-q", "-a", "-m", "main end"])
                    .tick()
                    .git(["merge", "-q", "--no-edit", "-X", "theirs", "side"])
                    .tick();
                lines = fx
                    .git_stdout(["show", "HEAD:f.txt"])
                    .split(|&b| b == b'\n')
                    .map(|l| String::from_utf8_lossy(l).into_owned())
                    .collect();
                lines.pop();
                continue;
            }
            edit(&mut rng, &mut lines, &format!("c{c}"));
            fx = fx
                .write("f.txt", render(&lines))
                .git(["commit", "-q", "-a", "-m", &format!("c{c}")])
                .tick();
        }
        let n = lines.len();
        let mut cases = Vec::new();
        for _ in 0..12 {
            let a = 1 + rng.next(n);
            let b = a + rng.next(8);
            cases.push(vec![
                "log".to_owned(),
                "-L".to_owned(),
                format!("{a},{b}:f.txt"),
            ]);
        }
        let (a, b) = (1 + rng.next(n / 2), n / 2 + 1 + rng.next(n / 2));
        cases.push(vec![
            "log".to_owned(),
            format!("-L{a},+3:f.txt"),
            format!("-L{b},+2:f.txt"),
            "--oneline".to_owned(),
        ]);
        cases.push(vec![
            "log".to_owned(),
            "-L".to_owned(),
            ":fn_:f.txt".to_owned(),
        ]);
        compare_all(&mut fx, &cases);
    }
}
