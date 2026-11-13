// SPDX-License-Identifier: MIT
//! `git <cmd> -h` and `--help-all` work outside a repository for every
//! builtin rgit implements: usage on stdout, exit 0 (t1517, #259).

use std::process::Command;

#[test]
fn help_outside_a_repository() {
    let rgit = env!("CARGO_BIN_EXE_rgit");
    let dir = std::env::temp_dir().join(format!("rgit-help-outside-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = Command::new(rgit)
        .arg("--list-cmds=builtins")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", &dir)
        .env("XDG_CONFIG_HOME", dir.join(".config"))
        .output()
        .expect("run rgit");
    let list = String::from_utf8(out.stdout).expect("utf-8");
    let mut failures = Vec::new();
    for cmd in list.split_whitespace() {
        for opt in ["-h", "--help-all"] {
            let out = Command::new(rgit)
                .args([cmd, opt])
                .current_dir(&dir)
                .env("GIT_CEILING_DIRECTORIES", &dir)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("HOME", &dir)
                .env("XDG_CONFIG_HOME", dir.join(".config"))
                .output()
                .expect("run rgit");
            let stdout = String::from_utf8_lossy(&out.stdout);
            // `init-db` is a synonym and prints the usage of `init`, like git
            // (a known breakage in t1517).
            let shown = if cmd == "init-db" { "init" } else { cmd };
            let usage = format!("usage: git {shown}");
            if out.status.code() != Some(0)
                || !stdout
                    .lines()
                    .any(|l| l.trim_start().to_lowercase().starts_with(&usage))
            {
                failures.push(format!(
                    "git {cmd} {opt}: exit {:?}\n{stdout}{}",
                    out.status.code(),
                    String::from_utf8_lossy(&out.stderr)
                ));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
