// SPDX-License-Identifier: MIT
//! `bugreport` and `diagnose` (#184): messages, usage and errors compared
//! with git; the report sections, the enabled hooks and the archive
//! contents checked directly (their system information differs by
//! design).

use std::path::Path;
use std::process::Command;

use rgit_difftest::{Check, Fixture};

const OUT: [Check; 3] = [Check::ExitCode, Check::Stdout, Check::Stderr];
const ERR: [Check; 2] = [Check::ExitCode, Check::Stderr];

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main", "r"])
        .git(["-C", "r", "commit", "-q", "--allow-empty", "-m", "one"])
        .write("r/git-bugreport-dup.txt", "")
        .write("r/file", "")
        .tick()
}

#[test]
fn messages_same_as_git() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let cases: &[(&[&str], &[Check])] = &[
        (&["bugreport", "-h"], &OUT),
        (&["bugreport", "--false"], &OUT),
        (&["bugreport", "false"], &OUT),
        (&["bugreport", "--diagnose=bad"], &OUT),
        (&["bugreport", "--suffix", "dup"], &OUT),
        (&["bugreport", "-s", "x"], &ERR),
        (&["bugreport", "--no-suffix", "-o", "a/b"], &ERR),
        (&["diagnose", "-h"], &OUT),
        (&["diagnose", "--mode=foo"], &OUT),
        (&["diagnose", "-o", "file", "-s", "q"], &ERR),
    ];
    for (argv, checks) in cases {
        fx.compare(*argv)
            .in_dir("r")
            .checks(checks.iter().copied())
            .run();
    }
}

fn rgit(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", dir.parent().unwrap())
        .output()
        .unwrap()
}

/// The entry names of a zip archive (central directory).
fn zip_names(data: &[u8]) -> Vec<String> {
    let eocd = &data[data.len() - 22..];
    assert_eq!(&eocd[..4], b"PK\x05\x06");
    let mut pos = u32::from_le_bytes(eocd[16..20].try_into().unwrap()) as usize;
    let count = u16::from_le_bytes([eocd[10], eocd[11]]);
    let mut names = Vec::new();
    for _ in 0..count {
        let h = &data[pos..];
        assert_eq!(&h[..4], b"PK\x01\x02");
        let n = u16::from_le_bytes([h[28], h[29]]) as usize;
        let extra = u16::from_le_bytes([h[30], h[31]]) as usize;
        let comment = u16::from_le_bytes([h[32], h[33]]) as usize;
        names.push(String::from_utf8_lossy(&h[46..46 + n]).into_owned());
        pos += 46 + n + extra + comment;
    }
    names
}

#[test]
fn report_sections_and_archive() {
    let tmp = tempfile::tempdir().unwrap();
    let top = tmp.path().join("top");
    std::fs::create_dir(&top).unwrap();
    let repo = top.join("r");
    assert!(rgit(&top, &["init", "-q", "r"]).status.success());
    let hooks = repo.join(".git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    for name in ["applypatch-msg", "unknown-hook"] {
        let p = hooks.join(name);
        std::fs::write(&p, "#!/bin/sh\ntrue\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let o = rgit(
        &repo,
        &["bugreport", "-s", "t", "--diagnose=all", "-o", "rep"],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(stdout.starts_with("Collecting diagnostic info\n\n"));
    assert!(stdout.contains("Available space on '"));
    let report = std::fs::read_to_string(repo.join("rep/git-bugreport-t.txt")).unwrap();
    assert!(report.starts_with("Thank you for filling out a Git bug report!\n"));
    assert!(report.contains("\n[System Info]\ngit version:\ngit version "));
    assert!(report.contains("\nshell-path: /bin/sh\n"));
    assert!(report.contains("https://github.com/BaamStudios/rgit/issues"));
    assert!(report.ends_with("\n[Enabled Hooks]\napplypatch-msg\n"));

    let zip = std::fs::read(repo.join("rep/git-diagnostics-t.zip")).unwrap();
    let names = zip_names(&zip);
    assert_eq!(
        names[..3],
        ["diagnostics.log", "packs-local.txt", "objects-local.txt"]
    );
    assert!(names.iter().any(|n| n == ".git/HEAD"));
    assert!(names.iter().any(|n| n == ".git/hooks/applypatch-msg"));

    let o = rgit(&repo, &["diagnose", "-s", "s"]);
    assert!(o.status.success());
    let names = zip_names(&std::fs::read(repo.join("git-diagnostics-s.zip")).unwrap());
    assert_eq!(
        names,
        ["diagnostics.log", "packs-local.txt", "objects-local.txt"]
    );

    // Outside a repository: no hooks, leading directories created.
    let o = rgit(&top, &["bugreport", "-s", "n", "-o", "a/b"]);
    assert!(o.status.success());
    let report = std::fs::read_to_string(top.join("a/b/git-bugreport-n.txt")).unwrap();
    assert!(
        report.ends_with("\n[Enabled Hooks]\nnot run from a git repository - no hooks to show\n")
    );
}
