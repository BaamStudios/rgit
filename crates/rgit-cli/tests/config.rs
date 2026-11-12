// SPDX-License-Identifier: MIT
//! Differential tests of `rgit config` against the reference git: output,
//! exit code and the resulting files.

use rgit_difftest::{Check, Fixture};

const CHECKS: [Check; 5] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Worktree,
    Check::GitFiles,
];

fn fixture() -> Fixture {
    Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"))
}

const SAMPLE: &str = "# comment\n[core]\n\tbare = false\n[s]\n\tk = 1\n\tk = 2\n\tb = yes\n\tn = 1k\n\tv\n\te =\n[t \"Sub\"]\n\tx = a b ; c\n";

/// Run every argument list in its own copy of the fixture state and report
/// all mismatches together.
#[track_caller]
fn all_same(fx: &mut Fixture, cases: &[&[&str]]) {
    let mut failures = Vec::new();
    for args in cases {
        if let Err(m) = fx.compare(*args).checks(CHECKS).try_run() {
            failures.push(m.to_string());
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Like [`all_same`], but restores `file` with `content` before each case.
#[track_caller]
fn each_on(file: &str, content: &str, cases: &[&[&str]]) {
    let mut failures = Vec::new();
    for args in cases {
        let mut fx = fixture().write(file, content);
        if let Err(m) = fx.compare(*args).checks(CHECKS).try_run() {
            failures.push(m.to_string());
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn legacy_reads() {
    let mut fx = fixture().write("c", SAMPLE);
    all_same(
        &mut fx,
        &[
            &["config", "-f", "c", "-l"],
            &["config", "-f", "c", "--list", "-z"],
            &["config", "-f", "c", "--list", "--name-only"],
            &[
                "config",
                "-f",
                "c",
                "--list",
                "--show-origin",
                "--show-scope",
            ],
            &["config", "-f", "c", "-l", "--type=bool"],
            &["config", "-f", "c", "-l", "--int"],
            &["config", "-f", "c", "-l", "--bool-or-int"],
            &["config", "-f", "c", "-l", "--bool-or-str"],
            &["config", "-f", "c", "s.k"],
            &["config", "-f", "c", "--get", "s.k"],
            &["config", "-f", "c", "--get", "s.k", "1"],
            &["config", "-f", "c", "--get", "s.k", "!1"],
            &["config", "-f", "c", "--get", "s.k", "["],
            &["config", "-f", "c", "--get", "--fixed-value", "s.k", "1"],
            &["config", "-f", "c", "--get-all", "s.k"],
            &["config", "-f", "c", "--get", "s.v"],
            &["config", "-f", "c", "--get", "--bool", "s.v"],
            &["config", "-f", "c", "--get", "--bool", "s.b"],
            &["config", "-f", "c", "--get", "--int", "s.n"],
            &["config", "-f", "c", "--get", "--int", "s.b"],
            &["config", "-f", "c", "--get", "s.e"],
            &["config", "-f", "c", "--get", "s.nope"],
            &["config", "-f", "c", "--get", "--default=x", "s.nope"],
            &[
                "config",
                "-f",
                "c",
                "--get",
                "--default=2k",
                "--int",
                "s.nope",
            ],
            &[
                "config",
                "-f",
                "c",
                "--get",
                "--default=zz",
                "--int",
                "s.nope",
            ],
            &["config", "-f", "c", "--get", "t.Sub.x"],
            &["config", "-f", "c", "--get", "t.sub.x"],
            &["config", "-f", "c", "--get", "S.K"],
            &["config", "-f", "c", "--get", "nosection"],
            &["config", "-f", "c", "--get-regexp", "k$"],
            &["config", "-f", "c", "--get-regexp", "^s", "[12]"],
            &["config", "-f", "c", "--get-regexp", "--name-only", "^s"],
            &["config", "-f", "c", "--get-regexp", "-z", "s"],
            &["config", "-f", "c", "--get-regexp", "["],
            &["config", "-f", "c", "--get", "--show-origin", "s.k"],
            &[
                "config",
                "-f",
                "c",
                "--get-all",
                "--show-scope",
                "-z",
                "s.k",
            ],
            &["config", "-f", "missing", "-l"],
            &["config", "-f", "missing", "--get", "a.b"],
            &["config"],
            &["config", "-f", "c", "a", "b", "c", "d"],
            &["config", "-f", "c", "--get"],
            &["config", "-f", "c", "--get", "a", "b", "c"],
            &["config", "-f", "c", "--list", "x"],
            &["config", "-f", "c", "--get", "--unset", "s.k"],
            &["config", "-f", "c", "--global", "--get", "s.k"],
            &["config", "-f", "c", "--name-only", "--get", "s.k"],
            &["config", "-f", "c", "--show-origin", "s.k", "v"],
            &["config", "-f", "c", "--default", "x", "-l"],
            &["config", "-f", "c", "--type=foo", "-l"],
            &["config", "-f", "c", "--bool", "--int", "-l"],
            &[
                "config",
                "-f",
                "c",
                "--bool",
                "--no-type",
                "--int",
                "--get",
                "s.n",
            ],
            &["config", "-h"],
        ],
    );
}

#[test]
fn subcommand_reads() {
    let mut fx = fixture().write("c", SAMPLE);
    all_same(
        &mut fx,
        &[
            &["config", "list", "-f", "c"],
            &["config", "list", "-f", "c", "-z", "--show-origin"],
            &["config", "list", "-f", "c", "--type=bool"],
            &["config", "list", "-f", "c", "x"],
            &["config", "get", "-f", "c", "s.k"],
            &["config", "get", "-f", "c", "--all", "s.k"],
            &["config", "get", "-f", "c", "--show-names", "s.k"],
            &[
                "config",
                "get",
                "-f",
                "c",
                "--show-names",
                "-z",
                "--all",
                "s.k",
            ],
            &["config", "get", "-f", "c", "--show-names", "s.v"],
            &["config", "get", "-f", "c", "--value=2", "s.k"],
            &["config", "get", "-f", "c", "--value=[", "s.k"],
            &["config", "get", "-f", "c", "--fixed-value", "s.k"],
            &["config", "get", "-f", "c", "--regexp", "k$"],
            &["config", "get", "-f", "c", "--regexp", "--all", "k$"],
            &[
                "config",
                "get",
                "-f",
                "c",
                "--regexp",
                "--all",
                "--show-names",
                "k$",
            ],
            &["config", "get", "-f", "c", "--default=x", "s.nope"],
            &["config", "get", "-f", "c", "--default=x", "--all", "s.nope"],
            &["config", "get", "-f", "c", "s.nope"],
            &["config", "get", "-f", "c"],
            &["config", "get", "-f", "c", "--type=bool-or-int", "s.n"],
        ],
    );
}

#[test]
fn legacy_writes() {
    let content = "[s]\n\tk = 1\n\tk = 2\n# keep\n[t] x = 1\n";
    each_on(
        "c",
        content,
        &[
            &["config", "-f", "c", "s.new", "v"],
            &["config", "-f", "c", "S.New", " lead"],
            &["config", "-f", "c", "s.k", "v"],
            &["config", "-f", "c", "s.k", "v", "1"],
            &["config", "-f", "c", "s.k", "v", "["],
            &["config", "-f", "c", "--fixed-value", "s.k", "v", "2"],
            &["config", "-f", "c", "--fixed-value", "s.k", "v"],
            &["config", "-f", "c", "--add", "s.k", "3"],
            &["config", "-f", "c", "--replace-all", "s.k", "3"],
            &["config", "-f", "c", "--replace-all", "s.k", "3", "2"],
            &["config", "-f", "c", "--unset", "s.k"],
            &["config", "-f", "c", "--unset", "s.k", "2"],
            &["config", "-f", "c", "--unset", "s.nope"],
            &["config", "-f", "c", "--unset-all", "s.k"],
            &["config", "-f", "c", "--unset", "t.x"],
            &["config", "-f", "c", "--rename-section", "t", "u.v"],
            &["config", "-f", "c", "--rename-section", "t", "bad name"],
            &["config", "-f", "c", "--rename-section", "nope", "x"],
            &["config", "-f", "c", "--remove-section", "s"],
            &["config", "-f", "c", "--remove-section", "nope"],
            &["config", "-f", "c", "--comment", "why", "s.c", "1"],
            &["config", "-f", "c", "--comment", "a\nb", "s.c", "1"],
            &["config", "-f", "c", "--comment", "x", "--unset", "s.k", "1"],
            &["config", "-f", "c", "--bool", "s.b", "yes"],
            &["config", "-f", "c", "--bool", "s.b", "maybe"],
            &["config", "-f", "c", "--int", "s.i", "1k"],
            &["config", "-f", "c", "--int", "s.i", "1x"],
            &["config", "-f", "c", "--bool-or-int", "s.i", "on"],
            &["config", "-f", "c", "s.1x", "v"],
            &["config", "-f", "c", "nosection", "v"],
            &["config", "-f", "c", "s.", "v"],
            &["config", "-f", "c", "--add", "s.k"],
            &["config", "-f", "c", "--rename-section", "s"],
            &["config", "-f", "dir/missing", "a.b", "c"],
            &["config", "-f", "new", "a.b", "c"],
        ],
    );
}

#[test]
fn subcommand_writes() {
    let content = "[s]\n\tk = 1\n\tk = 2\n";
    each_on(
        "c",
        content,
        &[
            &["config", "set", "-f", "c", "s.k", "v"],
            &["config", "set", "-f", "c", "--all", "s.k", "v"],
            &["config", "set", "-f", "c", "--value=1", "s.k", "v"],
            &[
                "config",
                "set",
                "-f",
                "c",
                "--value=1",
                "--fixed-value",
                "s.k",
                "v",
            ],
            &["config", "set", "-f", "c", "--fixed-value", "s.k", "v"],
            &["config", "set", "-f", "c", "--append", "s.k", "v"],
            &[
                "config",
                "set",
                "-f",
                "c",
                "--append",
                "--value=1",
                "s.k",
                "v",
            ],
            &["config", "set", "-f", "c", "--comment=hi", "s.c", "v"],
            &["config", "set", "-f", "c", "--type=bool", "s.c", "off"],
            &["config", "set", "-f", "c", "s.c"],
            &["config", "set", "-f", "c", "sc"],
            &["config", "set", "-f", "c"],
            &["config", "unset", "-f", "c", "s.k"],
            &["config", "unset", "-f", "c", "--all", "s.k"],
            &["config", "unset", "-f", "c", "--value=2", "s.k"],
            &["config", "unset", "-f", "c", "s.nope"],
            &["config", "unset", "-f", "c"],
            &["config", "rename-section", "-f", "c", "s", "t"],
            &["config", "rename-section", "-f", "c", "s"],
            &["config", "remove-section", "-f", "c", "s"],
            &["config", "remove-section", "-f", "c", "x"],
            &["config", "edit", "-f", "c", "x"],
        ],
    );
}

#[test]
fn repository_files() {
    let mut fx = fixture().git(["init", "-q", "r"]).write("r/sub/.keep", "");
    fx = fx.same(["-C", "r", "config", "set", "a.b", "c"]);
    fx = fx.same(["-C", "r/sub", "config", "set", "--local", "a.d", "e"]);
    fx = fx.same(["-C", "r", "config", "--add", "a.b", "f"]);
    fx = fx.same(["-C", "r", "config", "--worktree", "a.w", "1"]);
    all_same(
        &mut fx,
        &[
            &["-C", "r", "config", "list", "--show-origin", "--show-scope"],
            &["-C", "r/sub", "config", "list", "--local", "--show-origin"],
            &["-C", "r", "config", "get", "--all", "a.b"],
            &["-C", "r", "config", "list", "--worktree"],
            &[
                "-C",
                "r",
                "-c",
                "x.y=z",
                "config",
                "get",
                "--show-scope",
                "x.y",
            ],
            &["-C", "r/sub", "config", "-f", "rel", "a.b", "c"],
            &[
                "--git-dir=r/.git",
                "config",
                "list",
                "--local",
                "--show-origin",
            ],
            &["config", "list", "--local"],
            &["config", "set", "a.b", "c"],
            &["config", "--worktree", "-l"],
        ],
    );
}

#[test]
fn global_file() {
    let mut fx = fixture();
    fx = fx.same(["config", "--global", "a.b", "c"]);
    fx = fx.same(["config", "set", "--global", "--append", "a.b", "d"]);
    all_same(
        &mut fx,
        &[
            &["config", "--global", "--get-all", "a.b"],
            &["config", "list", "--global", "--show-scope"],
        ],
    );
}

#[test]
fn edit_with_editor() {
    let mut fx = fixture().git(["init", "-q", "r"]);
    all_same(
        &mut fx,
        &[&["-C", "r", "config", "-e"], &["-C", "r", "config", "edit"]],
    );
    let mut fx = fixture().git(["init", "-q", "r"]);
    let _ = fx
        .compare(["-C", "r", "config", "edit"])
        .env("GIT_EDITOR", "cat")
        .checks(CHECKS)
        .run();
    let _ = fx
        .compare(["-C", "r", "config", "edit"])
        .env("GIT_EDITOR", "false")
        .checks(CHECKS)
        .run();
}

#[test]
fn lock_held() {
    let mut fx = fixture().write("c", "[s]\n\tk = 1\n").write("c.lock", "");
    all_same(
        &mut fx,
        &[
            &[
                "config",
                "-c",
                "core.configLockTimeout=0",
                "-f",
                "c",
                "s.k",
                "2",
            ],
            &["config", "-f", "c", "--rename-section", "s", "t"],
        ],
    );
}

#[test]
fn url_match() {
    let content = "[http]\n\tsslVerify\n[http \"https://*.example.com\"]\n\tsslVerify = false\n\tcookieFile = /c\n[http \"https://example.com/sub\"]\n\tcookieFile = /s\n[http \"https://u@example.com\"]\n\tcookieFile = /u\n";
    let mut fx = fixture().write("c", content);
    let url = "--get-urlmatch";
    all_same(
        &mut fx,
        &[
            &["config", "-f", "c", url, "http", "https://a.example.com"],
            &[
                "config",
                "-f",
                "c",
                url,
                "--bool",
                "http.sslverify",
                "https://a.example.com",
            ],
            &[
                "config",
                "-f",
                "c",
                url,
                "--bool",
                "http.sslverify",
                "https://example.com",
            ],
            &[
                "config",
                "-f",
                "c",
                url,
                "http.cookiefile",
                "https://u@example.com/sub/x",
            ],
            &[
                "config",
                "-f",
                "c",
                url,
                "http.cookiefile",
                "https://u@example.com/",
            ],
            &[
                "config",
                "-f",
                "c",
                url,
                "http.nope",
                "https://example.com/",
            ],
            &[
                "config",
                "get",
                "-f",
                "c",
                "--url=https://a.example.com",
                "--show-scope",
                "http",
            ],
            &[
                "config",
                "get",
                "-f",
                "c",
                "--url=https://a.example.com",
                "--show-names",
                "http.cookiefile",
            ],
        ],
    );
}

#[test]
fn colors_and_dates() {
    let content = "[c]\n\ta = bold red blue\n\tb = #f1b\n\tbad = foo\n\tui = auto\n[d]\n\tx = 2017/11/11 11:11:11PM\n\tn = never\n\tbad = abc\n";
    let mut fx = fixture().write("c", content);
    let f = ["config", "-f", "c"];
    let cases: Vec<Vec<&str>> = vec![
        vec!["--get", "--type=color", "c.a"],
        vec!["--get", "--type=color", "c.bad"],
        vec!["-l", "--type=color"],
        vec!["--get-color", "c.b"],
        vec!["--get-color", "c.nope", "reverse ul 196"],
        vec!["--get-color", "c.nope", "bogus"],
        vec!["--type=color", "c.new", "bogus"],
        vec!["--type=color", "c.new", "nobold green"],
        vec!["--get-colorbool", "c.ui", "true"],
        vec!["--get-colorbool", "c.ui", "false"],
        vec!["--get-colorbool", "c.nope"],
        vec!["--get", "--expiry-date", "d.x"],
        vec!["--get", "--expiry-date", "d.n"],
        vec!["--get", "--expiry-date", "d.bad"],
        vec!["-l", "--type=expiry-date"],
    ];
    let full: Vec<Vec<&str>> = cases
        .iter()
        .map(|c| f.iter().chain(c.iter()).copied().collect())
        .collect();
    let refs: Vec<&[&str]> = full.iter().map(|c| c.as_slice()).collect();
    all_same(&mut fx, &refs);
}

#[test]
fn unusable_repository_is_no_repository() {
    // Unsafe owner: `config` does not check ownership, it just runs
    // without a repository.
    let mut fx = fixture()
        .git(["init", "-q", "r"])
        .env("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1");
    all_same(
        &mut fx,
        &[
            &["-C", "r", "config", "--global", "a.b", "c"],
            &["-C", "r", "config", "core.bare"],
            &["-C", "r", "config", "--local", "core.bare"],
            &["-C", "r", "config", "x.y", "z"],
        ],
    );
    // Unsupported format: warning, then no repository.
    let mut fx = fixture().git(["init", "-q", "r"]).git([
        "config",
        "-f",
        "r/.git/config",
        "core.repositoryformatversion",
        "99",
    ]);
    all_same(
        &mut fx,
        &[
            &["-C", "r", "config", "core.repositoryformatversion"],
            &["-C", "r", "config", "--local", "core.bare"],
        ],
    );
    // A bare repository that safe.bareRepository forbids.
    let mut fx = fixture().git(["init", "-q", "--bare", "b"]);
    all_same(
        &mut fx,
        &[&[
            "-C",
            "b",
            "-c",
            "safe.bareRepository=explicit",
            "config",
            "core.bare",
        ]],
    );
}

/// With both `~/.gitconfig` and the XDG file present, `--global` reads only
/// `~/.gitconfig` (t1306); with only the XDG file it reads that one.
#[test]
fn global_reads_one_file() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let xdg = fx.home_dir().join(".config/git");
    std::fs::create_dir_all(&xdg).unwrap();
    std::fs::write(
        xdg.join("config"),
        "[user]\n\tname = xdg\n[x]\n\tonly = xdg\n",
    )
    .unwrap();
    let mut fx = fx.same(["config", "--global", "user.name", "user"]);
    let reads: &[&[&str]] = &[
        &["config", "--global", "--list"],
        &["config", "list", "--global", "--show-origin"],
        &["config", "--global", "--get", "x.only"],
        &["config", "--global", "--get-all", "user.name"],
        &["config", "get", "--global", "--all", "user.name"],
    ];
    all_same(&mut fx, reads);
    std::fs::remove_file(fx.home_dir().join(".gitconfig")).unwrap();
    all_same(&mut fx, reads);
}
