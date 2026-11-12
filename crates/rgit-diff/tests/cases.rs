// SPDX-License-Identifier: MIT

//! Hand-made inputs that pin down behaviour found by the differential
//! experiments (each compared with `git diff --no-index`).

mod support;

use rgit_diff::{DiffOptions, diff_text};
use std::path::Path;
use support::{Scratch, git_diff, have_git, hunks_of};

fn lines(parts: &[String]) -> Vec<u8> {
    parts
        .iter()
        .flat_map(|l| format!("{l}\n").into_bytes())
        .collect()
}

fn n(prefix: &str, count: usize) -> Vec<String> {
    (0..count).map(|i| format!("{prefix}{i}")).collect()
}

fn rep(line: &str, count: usize) -> Vec<String> {
    vec![line.to_string(); count]
}

fn cat(parts: &[Vec<String>]) -> Vec<String> {
    parts.concat()
}

fn check(name: &str, a: &[u8], b: &[u8], context: usize) {
    let s = Scratch::new(&format!("case-{name}"));
    std::fs::write(s.0.join("a"), a).unwrap();
    std::fs::write(s.0.join("b"), b).unwrap();
    let u = format!("-U{context}");
    let want = hunks_of(&git_diff(&s.0, Path::new("a"), Path::new("b"), &[&u]));
    let opts = DiffOptions {
        context,
        ..DiffOptions::default()
    };
    let got = diff_text(a, b, &opts).hunks;
    assert_eq!(
        String::from_utf8_lossy(&got),
        String::from_utf8_lossy(&want),
        "case {name}"
    );
}

#[test]
fn cases() {
    if !have_git() {
        eprintln!("git not found, skipping");
        return;
    }
    let s = |x: &str| x.to_string();
    // Lines with many partners inside long runs of unmatched lines are
    // dropped before the search; the run is scanned up to 100 lines.
    for q in [70usize, 76, 77, 120] {
        let a = cat(&[
            vec![s("f")],
            rep("g", 40),
            rep("d", 20),
            vec![s("f2"), s("p")],
        ]);
        let b = cat(&[n("x", 1), rep("g", 50), n("z", q), vec![s("d")], n("y", 1)]);
        check(&format!("many-{q}"), &lines(&a), &lines(&b), 3);
    }
    // Partner threshold depends on the length of the line's own file.
    for k in [3usize, 4, 7, 8, 15, 16] {
        let a = cat(&[rep("d", k), vec![s("p"), s("e")]]);
        let b = cat(&[n("x", 12), vec![s("d"), s("y1"), s("y2"), s("e")]]);
        check(&format!("limit-{k}"), &lines(&a), &lines(&b), 3);
    }
    // Indent heuristic: search window of size + 1 lines, at most 100.
    for (g, d) in [
        (1usize, 2usize),
        (1, 3),
        (2, 3),
        (2, 4),
        (120, 99),
        (120, 101),
    ] {
        let unit: Vec<String> = (0..g).map(|i| format!("                l{i}")).collect();
        let run: Vec<String> = (0..g + d).map(|k| unit[k % g].clone()).collect();
        let b = cat(&[
            vec![s("        foo();")],
            run.clone(),
            vec![s("                z"), s("end")],
        ]);
        let a = cat(&[
            vec![s("        foo();")],
            run[g..].to_vec(),
            vec![s("                z"), s("end")],
        ]);
        check(&format!("window-{g}-{d}"), &lines(&a), &lines(&b), 3);
    }
    // Blank lines are counted up to 20 around a split.
    for (k1, k2) in [(16usize, 19usize), (16, 20), (17, 21), (15, 21), (14, 20)] {
        for top in ["A", "  A"] {
            let bottom = if top == "A" { "B" } else { "    B" };
            let b = cat(&[
                vec![s(top)],
                rep("", k1),
                rep("x", 2),
                rep("", k2),
                vec![s(bottom)],
            ]);
            let a = cat(&[
                vec![s(top)],
                rep("", k1),
                rep("x", 1),
                rep("", k2),
                vec![s(bottom)],
            ]);
            check(&format!("blank-{k1}-{k2}-{top}"), &lines(&a), &lines(&b), 3);
        }
    }
    // Without context a long common tail is cut off in 1024-byte blocks.
    for r in [250usize, 255, 260, 300, 1000] {
        let a = cat(&[
            vec![s("top")],
            (0..r).flat_map(|_| [s("x"), s("y")]).collect(),
            vec![s("bottom")],
        ]);
        let b = cat(&[
            vec![s("top")],
            (0..=r).flat_map(|_| [s("x"), s("y")]).collect(),
            vec![s("bottom")],
        ]);
        check(&format!("tail-{r}"), &lines(&a), &lines(&b), 0);
        check(&format!("tail-{r}-u3"), &lines(&a), &lines(&b), 3);
    }
    // Incomplete last lines.
    check("noeol", b"a\nb", b"a\nc", 3);
    check("noeol-added", b"a\nb", b"a\nb\n", 3);
    check("empty-old", b"", b"a\n", 3);
}
