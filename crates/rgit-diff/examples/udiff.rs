// SPDX-License-Identifier: MIT

//! Prints the unified hunks between two files: `udiff [--no-indent-heuristic] A B`.

use rgit_diff::{DiffOptions, diff_text};
use std::io::Write;

fn main() {
    let mut opts = DiffOptions::default();
    let mut files = Vec::new();
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--no-indent-heuristic" => opts.indent_heuristic = false,
            "--minimal" => opts.minimal = true,
            _ => files.push(arg),
        }
    }
    if files.len() != 2 {
        eprintln!("usage: udiff [--no-indent-heuristic] A B");
        std::process::exit(129);
    }
    let a = std::fs::read(&files[0]).expect("read A");
    let b = std::fs::read(&files[1]).expect("read B");
    let d = diff_text(&a, &b, &opts);
    std::io::stdout().write_all(&d.hunks).unwrap();
}
