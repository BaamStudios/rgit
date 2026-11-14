// SPDX-License-Identifier: MIT
//! Aliases, unknown commands, autocorrection and `git help` (#179),
//! compared against the reference git.
//!
//! Suggestions depend on the set of commands, which differs between git and
//! rgit; the autocorrect cases use made-up names far from every command.

use rgit_difftest::{Fixture, Normalizer};

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("sub/dir/f", "x\n")
}

fn same(fx: &mut Fixture, args: &[&str]) {
    fx.compare(args).run();
}

/// The `trace:` lines of GIT_TRACE without time and source location.
fn trace_lines() -> Normalizer {
    Normalizer::regex(r"(?m)^\d\d:\d\d:\d\d\.\d+ +\S+ +", "")
}

#[test]
fn simple_and_nested_aliases() {
    let mut fx = fixture()
        .git(["config", "alias.gd", "rev-parse --git-dir"])
        .git(["config", "alias.n1", "n2"])
        .git(["config", "alias.n2", "gd"])
        .git(["config", "alias.LegacyCase", "rev-parse --show-toplevel"])
        .git([
            "config",
            "alias.Sub.command",
            "rev-parse --is-inside-work-tree",
        ])
        .git([
            "config",
            "alias.simple.dotted",
            "rev-parse --is-bare-repository",
        ])
        .git([
            "config",
            "alias.q",
            "config --default 'a b'\"c\\\"d\"e\\ f''g --get no.such",
        ])
        .git(["config", "alias.opts", "-c x.y=z config x.y"]);
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["gd"][..],
        &["n1"],
        &["legacycase"],
        &["LEGACYCASE"],
        &["Sub"],
        &["sub"],
        &["simple.dotted"],
        &["q"],
        &["opts"],
        &["-C", "sub/dir", "gd"],
    ] {
        same(&mut fx, args);
    }
}

#[test]
fn alias_errors() {
    let mut fx = fixture()
        .git(["config", "alias.l1", "l2"])
        .git(["config", "alias.l2", "l3"])
        .git(["config", "alias.l3", "l2"])
        .git(["config", "alias.self", "self -v"])
        .git(["config", "alias.unclosed", "log 'x"])
        .git(["config", "alias.backslash", "log x\\"])
        .git(["config", "alias.empty", ""])
        .git(["config", "alias.optonly", "-p"])
        .git(["config", "alias.envy", "-C sub status"])
        .git(["config", "alias.gitdir", "--git-dir=.git status"])
        .git(["config", "alias.unknown", "no-such-thing x"])
        .git(["config", "alias.u1", "u2"])
        .git(["config", "alias.u2", "no-such-thing"])
        .git(["config", "alias.badopt", "--bogus status"]);
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["l1"][..],
        &["self"],
        &["unclosed"],
        &["backslash"],
        &["empty"],
        &["optonly"],
        &["envy"],
        &["gitdir"],
        &["unknown"],
        &["u1"],
        &["badopt"],
        &["-c", "alias.l2.command=l1", "l1", "-h"],
    ] {
        same(&mut fx, args);
    }
}

#[test]
fn alias_without_value() {
    let mut fx = fixture()
        .write(".git/config-extra", "[alias]\n\tnoval\n")
        .git(["config", "include.path", "config-extra"]);
    if fx.is_skipped() {
        return;
    }
    same(&mut fx, &["noval"]);
}

#[test]
fn alias_help_note() {
    let mut fx =
        fixture()
            .git(["config", "alias.a1", "a2"])
            .git(["config", "alias.a2", "rev-parse"]);
    if fx.is_skipped() {
        return;
    }
    same(&mut fx, &["a1", "-h"]);
    same(&mut fx, &["a1", "--git-dir"]);
}

#[test]
fn shell_aliases() {
    let mut fx = fixture()
        .git([
            "config",
            "alias.where",
            "!pwd; echo \"prefix=[$GIT_PREFIX]\"; echo \"dir=[$GIT_DIR]\"",
        ])
        .git([
            "config",
            "alias.args",
            "!f() { printf '<%s>' \"$@\"; echo; }; f",
        ])
        .git(["config", "alias.plain", "!echo"])
        .git(["config", "alias.code", "!exit 3"])
        .git(["config", "alias.missing", "!no-such-program-xyz"])
        .git(["config", "alias.nested", "!git gd"])
        .git(["config", "alias.gd", "rev-parse --git-dir"])
        .git(["config", "alias.term", "!kill -TERM $$"]);
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["where"][..],
        &["-C", "sub/dir", "where"],
        &["-C", ".git", "where"],
        &["args", "a b", "", "c"],
        &["plain", "x", "y"],
        &["plain", "-h"],
        &["code"],
        &["missing"],
        &["nested"],
        &["term"],
    ] {
        same(&mut fx, args);
    }
}

#[test]
fn shell_alias_trace() {
    let mut fx =
        fixture()
            .git(["config", "alias.echo", "!echo $*"])
            .git(["config", "alias.plain", "!true"]);
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["echo", "arg"][..],
        &["plain"],
        &["frotz", "a", "", "b", " ", "c"],
    ] {
        fx.compare(args)
            .env("GIT_TRACE", "1")
            .normalize_stderr(trace_lines())
            // Suggestions for "frotz" depend on the commands each side has.
            .normalize_stderr(Normalizer::regex(r"(?s)\n\nThe most similar.*", "\n"))
            .run();
    }
}

#[test]
fn external_command_comes_before_alias() {
    let mut fx = fixture()
        .write_executable("bin/git-ext", "#!/bin/sh\necho \"external: $*\"\n")
        .git(["config", "alias.ext", "rev-parse --git-dir"]);
    if fx.is_skipped() {
        return;
    }
    let mut dirs = vec![fx.work_dir().join("bin")];
    dirs.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(dirs).unwrap();
    fx.compare(["ext", "a"]).env("PATH", &path).run();
    // Only the fixture's programs: the system's git-* programs are in git's
    // exec path, but "others" for rgit.
    let bin = fx.work_dir().join("bin");
    fx.compare(["--list-cmds=others"]).env("PATH", &bin).run();
    fx.compare(["--list-cmds=alias"]).run();
}

#[test]
fn autocorrect() {
    let mut fx = fixture()
        .git(["config", "alias.qzxjqk", "rev-parse --git-dir"])
        .git(["config", "alias.qzxjqkab", "rev-parse --git-dir"])
        .git(["config", "alias.qzxjqkwa", "rev-parse --git-dir"]);
    if fx.is_skipped() {
        return;
    }
    for value in [
        None,
        Some("show"),
        Some("0"),
        Some("false"),
        Some("never"),
        Some("immediate"),
        Some("1"),
        Some("-1"),
        Some("2"),
        Some("prompt"),
        Some("bogus"),
    ] {
        let mut args: Vec<String> = Vec::new();
        if let Some(v) = value {
            args.extend(["-c".into(), format!("help.autocorrect={v}")]);
        }
        // One similar command (a swap), several, none.
        for typed in ["qzxjkq", "qzxjqkw", "zzzzzzzzzz"] {
            let mut a = args.clone();
            a.push(typed.into());
            fx.compare(&a).run();
        }
    }
}

#[test]
fn help_usage_errors() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["help", "-h"][..],
        &["help", "-a", "add"],
        &["help", "-g", "add"],
        &["help", "-a", "-g"],
        &["help", "-g", "-c"],
        &["help", "-a", "-i"],
        &["help", "-g", "-w"],
        &["help", "--user-interfaces", "add"],
        &["help", "--developer-interfaces", "x"],
        &["help", "-g", "--no-external-commands"],
        &["help", "-g", "--aliases"],
    ] {
        same(&mut fx, args);
    }
}

#[test]
fn help_pages() {
    let mut fx = fixture()
        .write_executable("browser", "#!/bin/sh\necho \"$*\" >>browser.log\n")
        .write("html/git-status.html", "")
        .git(["config", "alias.st", "status -s"])
        .git(["config", "alias.sh", "!echo"])
        .git(["config", "help.browser", "test"])
        .git(["config", "browser.test.cmd", "./browser"])
        .git(["config", "man.viewer", "mine"])
        .git(["config", "man.mine.cmd", "echo man:"]);
    if fx.is_skipped() {
        return;
    }
    let web = ["-c", "help.format=html", "-c", "help.htmlpath=test://h"];
    let with = |extra: &[&str]| -> Vec<String> {
        web.iter().chain(extra).map(|s| s.to_string()).collect()
    };
    // Cases that open a page in the configured browser. git starts the
    // browser through its `git-web--browse` script, which a reference git
    // run from its build tree (not installed) cannot always execute; then
    // these cases say nothing about rgit and are left out.
    let browser_cases = [
        with(&["help", "status"]),
        with(&["help", "revisions"]),
        with(&["help", "git"]),
        with(&["help", "no-such-page"]),
        with(&["st", "--help"]),
        with(&["status", "--help"]),
        with(&["-c", "help.htmlpath=html", "help", "status"]),
    ];
    let probe = fx.git_output(with(&["help", "status"]));
    let browser_works = probe.status.success() && fx.work_dir().join("browser.log").exists();
    if browser_works {
        fx = fx.remove("browser.log");
        for args in &browser_cases {
            fx.compare(args).run();
        }
    } else {
        eprintln!(
            "help_pages: the reference git cannot start a browser ({}), browser cases skipped",
            String::from_utf8_lossy(&probe.stderr).trim()
        );
    }
    for args in [
        with(&["help", "st"]),
        with(&["help", "sh"]),
        with(&["revisions", "--help"]),
        with(&["help", "--exclude-guides", "revisions"]),
        with(&["-c", "help.htmlpath=html", "help", "log"]),
        vec!["help".into(), "-w".into(), "-c".into()],
        vec![
            "-c".into(),
            "help.format=bogus".into(),
            "help".into(),
            "log".into(),
        ],
        vec!["help".into(), "log".into()],
        vec!["help".into(), "-m".into(), "revisions".into()],
    ] {
        fx.compare(&args).run();
    }
}

#[test]
fn dashed_name_of_a_non_builtin() {
    let rgit = env!("CARGO_BIN_EXE_rgit");
    let dir = std::env::temp_dir().join(format!("rgit-dashed-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let link = dir.join("git-no-such-builtin");
    let _ = std::fs::remove_file(&link);
    #[cfg(unix)]
    std::os::unix::fs::symlink(rgit, &link).unwrap();
    #[cfg(not(unix))]
    std::fs::copy(rgit, &link).unwrap();
    let out = std::process::Command::new(&link)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", &dir)
        .env("XDG_CONFIG_HOME", dir.join(".config"))
        .env("PATH", &dir)
        .output()
        .unwrap();
    // Such links are not commands of their own.
    let main = std::process::Command::new(rgit)
        .arg(format!("--exec-path={}", dir.display()))
        .arg("--list-cmds=main")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", &dir)
        .env("XDG_CONFIG_HOME", dir.join(".config"))
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let main = String::from_utf8_lossy(&main.stdout);
    assert!(main.lines().any(|l| l == "help"), "{main}");
    assert!(!main.lines().any(|l| l == "no-such-builtin"), "{main}");
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: cannot handle no-such-builtin as a builtin\n"
    );
}
