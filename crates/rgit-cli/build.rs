// SPDX-License-Identifier: MIT
//! Generates the command registry from the files in `src/commands/`.
//!
//! Every `src/commands/<name>.rs` (except `mod.rs`) becomes the command
//! `<name>`, with `_` in the file name turned into `-` (`cat_file.rs` is
//! `cat-file`, `fsmonitor__daemon.rs` is `fsmonitor--daemon`). The file must
//! define `pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult`.
//! Adding a command therefore touches no shared file.

use std::fmt::Write as _;
use std::path::Path;

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let dir = Path::new(&manifest).join("src").join("commands");
    println!("cargo:rerun-if-changed={}", dir.display());

    let mut files: Vec<(String, String)> = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("read src/commands") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
        if stem == "mod" {
            continue;
        }
        println!("cargo:rerun-if-changed={}", path.display());
        files.push((stem, path.display().to_string()));
    }
    files.sort_by_key(|a| command_name(&a.0));

    let mut out = String::new();
    for (stem, path) in &files {
        writeln!(out, "#[path = {path:?}]\npub mod {stem};").unwrap();
    }
    out.push_str(
        "\n/// All built-in commands, sorted by name.\npub static COMMANDS: &[Command] = &[\n",
    );
    for (stem, _) in &files {
        writeln!(
            out,
            "    Command {{ name: {:?}, run: {stem}::run }},",
            command_name(stem)
        )
        .unwrap();
    }
    out.push_str("];\n");

    let dest = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("commands.rs");
    std::fs::write(dest, out).expect("write commands.rs");
}

fn command_name(stem: &str) -> String {
    stem.replace('_', "-")
}
