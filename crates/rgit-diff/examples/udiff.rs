// SPDX-License-Identifier: MIT

//! Prints the unified hunks between two files: `udiff [--no-indent-heuristic] A B`.

use rgit_diff::{Algorithm, DiffOptions, diff_text};
use std::io::Write;

fn main() {
    let mut opts = DiffOptions::default();
    let mut files = Vec::new();
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--no-indent-heuristic" => opts.indent_heuristic = false,
            "--minimal" => opts.minimal = true,
            "--patience" => opts.algorithm = Algorithm::Patience,
            "--histogram" => opts.algorithm = Algorithm::Histogram,
            _ => match arg.strip_prefix("--anchored=") {
                Some(t) => {
                    opts.anchors.push(t.as_bytes().to_vec());
                    opts.algorithm = Algorithm::Patience;
                }
                None => files.push(arg),
            },
        }
    }
    if files.len() != 2 {
        eprintln!(
            "usage: udiff [--no-indent-heuristic] [--minimal|--patience|--histogram|--anchored=<text>] A B"
        );
        std::process::exit(129);
    }
    let a = std::fs::read(&files[0]).expect("read A");
    let b = std::fs::read(&files[1]).expect("read B");
    let d = diff_text(&a, &b, &opts);
    std::io::stdout().write_all(&d.hunks).unwrap();
}
