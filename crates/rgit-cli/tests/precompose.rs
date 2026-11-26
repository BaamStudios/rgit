// SPDX-License-Identifier: MIT
//! Tests of `core.precomposeUnicode` (#183).
//!
//! git implements the setting on macOS only. There the cases compare rgit
//! with the reference git on the real file system. On other systems git
//! (and rgit) ignore the setting, which is compared as well; rgit's
//! implementation is then exercised through its test switch
//! `RGIT_TEST_PRECOMPOSE_UNICODE=1` without a reference.

use std::path::Path;
use std::process::{Command, Output};

use rgit_difftest::{Check, Fixture};

/// `f.Ä` with a combining diaeresis (NFD), as macOS hands out names.
const DECOMPOSED: &str = "f.A\u{308}";
/// The same name precomposed (NFC).
const PRECOMPOSED: &str = "f.\u{c4}";

const STATE: [Check; 4] = [Check::ExitCode, Check::Stdout, Check::Stderr, Check::Index];

#[test]
fn same_as_git_on_this_platform() {
    // macOS: names are precomposed. Elsewhere: the setting does nothing.
    for setting in ["true", "false"] {
        let fx = Fixture::new()
            .rgit(env!("CARGO_BIN_EXE_rgit"))
            .git(["init", "-q"])
            .git(["config", "core.precomposeunicode", setting])
            .write(DECOMPOSED, "decomposed\n")
            .write(format!("d.O\u{308}/{DECOMPOSED}"), "below\n")
            .write("plain", "plain\n");
        if fx.is_skipped() {
            return;
        }
        let mut fx = fx;
        for args in [
            &["status", "--porcelain", "-uall"][..],
            &["ls-files", "-o"],
            &["add", "-v", DECOMPOSED],
            &["add", "."],
            &["ls-files"],
            &["ls-files", "--", DECOMPOSED],
            &["ls-files", "--", PRECOMPOSED],
            &["status", "--porcelain"],
            &["rm", "--cached", "-q", "--", PRECOMPOSED],
            &["ls-files"],
        ] {
            fx.compare(args)
                .env("GIT_CONFIG_PARAMETERS", "'core.quotepath=false'")
                .checks(STATE)
                .run();
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn init_probes_the_file_system_like_git() {
    let mut fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"));
    if fx.is_skipped() {
        return;
    }
    fx.compare(["init", "-q", "probe"])
        .checks([Check::ExitCode])
        .run();
    fx.compare(["-C", "probe", "config", "--get", "core.precomposeunicode"])
        .run();
}

fn rgit(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", home)
        .env("RGIT_TEST_PRECOMPOSE_UNICODE", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("run rgit")
}

fn stdout(out: &Output) -> String {
    assert!(
        out.status.success(),
        "rgit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The implementation itself, on any platform: names read from the work
/// tree and names on the command line end up precomposed in the index.
#[test]
fn test_switch_precomposes_names() {
    let tmp = tempfile::tempdir().expect("temporary directory");
    let home = tmp.path().join("home");
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(work.join("d.O\u{308}")).unwrap();
    stdout(&rgit(&work, &home, &["init", "-q"]));
    // The probe wrote the setting; whether it is true depends on the file
    // system (true where both forms name one file).
    let probed = stdout(&rgit(
        &work,
        &home,
        &["config", "--get", "core.precomposeunicode"],
    ));
    assert!(matches!(probed.trim(), "true" | "false"), "{probed}");
    stdout(&rgit(
        &work,
        &home,
        &["config", "core.precomposeunicode", "true"],
    ));
    stdout(&rgit(&work, &home, &["config", "core.quotepath", "false"]));
    std::fs::write(work.join(DECOMPOSED), "decomposed\n").unwrap();
    std::fs::write(work.join("d.O\u{308}").join(DECOMPOSED), "below\n").unwrap();

    // On a file system that tells the two forms apart the precomposed
    // names are no files: only the names read from the top directory show.
    let normalising = work.join(PRECOMPOSED).exists();
    let untracked = stdout(&rgit(&work, &home, &["ls-files", "-o"]));
    if normalising {
        assert_eq!(
            untracked,
            format!("d.\u{d6}/{PRECOMPOSED}\n{PRECOMPOSED}\n")
        );
    } else {
        assert_eq!(untracked, format!("{PRECOMPOSED}\n"));
    }
    if normalising {
        stdout(&rgit(&work, &home, &["add", "."]));
        let tracked = stdout(&rgit(&work, &home, &["ls-files"]));
        assert_eq!(tracked, format!("d.\u{d6}/{PRECOMPOSED}\n{PRECOMPOSED}\n"));
        // A decomposed argument names the precomposed entry.
        let named = stdout(&rgit(&work, &home, &["ls-files", "--", DECOMPOSED]));
        assert_eq!(named, format!("{PRECOMPOSED}\n"));
        // Nothing is untracked or missing: the work tree is read precomposed.
        assert_eq!(
            stdout(&rgit(&work, &home, &["status", "--porcelain"])),
            format!("A  d.\u{d6}/{PRECOMPOSED}\nA  {PRECOMPOSED}\n")
        );
    }

    // Switched off, names are taken as they are.
    stdout(&rgit(
        &work,
        &home,
        &["config", "core.precomposeunicode", "false"],
    ));
    let raw = stdout(&rgit(&work, &home, &["ls-files", "-o"]));
    if !normalising {
        assert_eq!(raw, format!("d.O\u{308}/{DECOMPOSED}\n{DECOMPOSED}\n"));
    }
}
