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
        if stem.contains("__") {
            // `credential_cache__daemon` for `credential-cache--daemon`.
            out.push_str("#[allow(non_snake_case)]\n");
        }
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

    config_variables(Path::new(&manifest));
}

/// The configuration variable names for `help -c` (decision on rgit #413):
/// read from git's documentation (`Documentation/config.adoc` and
/// `Documentation/config/*.adoc`, every `<name>::` heading with a dot) and
/// the fsck message ids (`Documentation/fsck-msgids.adoc`). The
/// documentation is found through `$RGIT_GIT_DOCUMENTATION` or in
/// `upstream/git/Documentation` (`scripts/fetch-upstream.sh`); without it
/// the committed `data/config-variables.txt` is used.
/// `RGIT_UPDATE_CONFIG_LIST=1` rewrites that file from the documentation.
fn config_variables(manifest: &Path) {
    println!("cargo:rerun-if-env-changed=RGIT_GIT_DOCUMENTATION");
    println!("cargo:rerun-if-env-changed=RGIT_UPDATE_CONFIG_LIST");
    let snapshot = manifest.join("data").join("config-variables.txt");
    println!("cargo:rerun-if-changed={}", snapshot.display());
    let docs = std::env::var_os("RGIT_GIT_DOCUMENTATION")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| manifest.join("../../upstream/git/Documentation"));
    let text = match extract_config_variables(&docs) {
        Some(text) => {
            if std::env::var_os("RGIT_UPDATE_CONFIG_LIST").is_some() {
                std::fs::write(&snapshot, &text).expect("write data/config-variables.txt");
            }
            text
        }
        None => std::fs::read_to_string(&snapshot).expect("read data/config-variables.txt"),
    };
    let dest = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("config-variables.txt");
    std::fs::write(dest, text).expect("write config-variables.txt");
}

/// One name per line: the variables, then `#fsck <id>` lines with the
/// message ids. `None` when the documentation is not there.
fn extract_config_variables(docs: &Path) -> Option<String> {
    let dir = docs.join("config");
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "adoc"))
        .collect();
    files.sort();
    files.push(docs.join("config.adoc"));
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut names = std::collections::BTreeSet::new();
    for f in &files {
        println!("cargo:rerun-if-changed={}", f.display());
        let text = std::fs::read_to_string(f).ok()?;
        for line in text.lines() {
            let line = line.replace('`', "");
            let Some(name) = line.trim_end().strip_suffix("::") else {
                continue;
            };
            if name.starts_with(|c: char| c.is_ascii_alphabetic())
                && name.contains('.')
                && !name.contains([' ', '\t'])
            {
                names.insert(name.to_owned());
            }
        }
    }
    let ids_file = docs.join("fsck-msgids.adoc");
    println!("cargo:rerun-if-changed={}", ids_file.display());
    let mut ids = std::collections::BTreeSet::new();
    for line in std::fs::read_to_string(ids_file).ok()?.lines() {
        let line = line.replace('`', "");
        if let Some(id) = line.trim_end().strip_suffix("::") {
            if !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric()) {
                ids.insert(id.to_owned());
            }
        }
    }
    let mut out = String::new();
    for n in names {
        out.push_str(&n);
        out.push('\n');
    }
    for id in ids {
        out.push_str("#fsck ");
        out.push_str(&id);
        out.push('\n');
    }
    Some(out)
}

fn command_name(stem: &str) -> String {
    stem.replace('_', "-")
}
