// SPDX-License-Identifier: MIT
//! `git remote` against the reference git: output, exit code, refs,
//! reflogs and `.git/config` after every subcommand. Remotes are local
//! repositories in the fixture; nothing contacts a server.

use rgit_difftest::Fixture;

mod common;
use common::servers::server_path;

/// Compare `args` run in the repository `dir`, with all checks on it. The
/// reference git comes first in `PATH`, so it serves the fetches.
fn same_in(fx: &mut Fixture, dir: &str, args: &[&str]) {
    let path = server_path(fx);
    fx.compare(args)
        .in_dir(dir)
        .repo(dir)
        .env("PATH", path)
        .run();
}

/// Two source repositories `one` (branches `main`, `side`) and `two`
/// (`main`, `side`, `another`), and a clone `c` of `one`.
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main", "one"])
        .git(["-C", "one", "commit", "-q", "--allow-empty", "-m", "a"])
        .git(["-C", "one", "branch", "side"])
        .git(["init", "-q", "-b", "main", "two"])
        .git(["-C", "two", "commit", "-q", "--allow-empty", "-m", "b"])
        .git(["-C", "two", "branch", "side"])
        .git(["-C", "two", "branch", "another"])
        .git(["clone", "-q", "one", "c"])
}

#[test]
fn usage_and_listing() {
    let mut fx = fixture()
        .git(["-C", "c", "config", "remote.m.url", "u1"])
        .git(["-C", "c", "config", "--add", "remote.m.url", "u2"])
        .git(["-C", "c", "config", "--add", "remote.m.pushurl", "p1"])
        .git(["-C", "c", "config", "--add", "remote.m.pushurl", "p2"])
        .git([
            "-C",
            "c",
            "config",
            "remote.m.partialclonefilter",
            "blob:none",
        ])
        .git(["-C", "c", "config", "remote.k.url", ""])
        .git([
            "-C",
            "c",
            "config",
            "remote.f.fetch",
            "refs/heads/*:refs/remotes/f/*",
        ])
        .git(["-C", "c", "config", "remote.p.url", ""])
        .git(["-C", "c", "config", "remote.p.pushurl", "pp"])
        .git(["-C", "c", "config", "url.REWRITTEN/.insteadOf", "u"])
        .git(["-C", "c", "config", "url.PUSH/.pushInsteadOf", "u"]);
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["remote"],
        &["remote", "-v"],
        &["remote", "--verbose"],
        &["remote", "show"],
        &["remote", "-v", "show"],
        &["remote", "-h"],
        &["remote", "origin"],
        &["remote", "-v", "bogus"],
        &["remote", "--bogus"],
        &["remote", "add", "-h"],
        &["remote", "rename", "-h"],
        &["remote", "rm", "-h"],
        &["remote", "set-head", "-h"],
        &["remote", "set-branches", "-h"],
        &["remote", "get-url", "-h"],
        &["remote", "set-url", "-h"],
        &["remote", "show", "-h"],
        &["remote", "prune", "-h"],
        &["remote", "update", "-h"],
        &["remote", "add", "x"],
        &["remote", "add", "x", "u", "extra"],
        &["remote", "rename", "origin"],
        &["remote", "remove"],
        &["remote", "remove", "a", "b"],
        &["remote", "set-head", "origin"],
        &["remote", "set-head", "origin", "-a", "-d"],
        &["remote", "set-head", "origin", "-a", "main"],
        &["remote", "set-branches"],
        &["remote", "set-branches", "--add"],
        &["remote", "get-url"],
        &["remote", "get-url", "origin", "extra"],
        &["remote", "set-url", "origin"],
        &["remote", "set-url", "--add", "origin", "a", "b"],
        &["remote", "set-url", "--delete", "origin"],
        &["remote", "set-url", "--add", "--delete", "origin", "x"],
        &["remote", "prune"],
        &["remote", "show", "-v"],
    ];
    for args in cases {
        same_in(&mut fx, "c", args);
    }
    for name in ["m", "k", "f", "p", "origin", "nosuch"] {
        for opts in [&[][..], &["--all"], &["--push"], &["--push", "--all"]] {
            let mut args = vec!["remote", "get-url"];
            args.extend_from_slice(opts);
            args.push(name);
            same_in(&mut fx, "c", &args);
        }
    }
    same_in(
        &mut fx,
        "c",
        &["remote", "show", "-n", "m", "k", "f", "p", "nosuch"],
    );
}

#[test]
fn add_variants() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["remote", "add", "a", "../one"],
        &[
            "remote", "add", "-t", "main", "-t", "side", "-m", "side", "b", "../one",
        ],
        &["remote", "add", "--mirror", "c", "../one"],
        &["remote", "add", "--mirror=fetch", "d", "../one"],
        &["remote", "add", "--mirror=push", "e", "../one"],
        &["remote", "add", "--mirror=bogus", "f", "../one"],
        &["remote", "add", "--tags", "g", "../one"],
        &["remote", "add", "--no-tags", "h", "../one"],
        &["remote", "add", "--no-no-tags", "h2", "../one"],
        &["remote", "add", "--mirror=push", "-t", "x", "i", "../one"],
        &[
            "remote",
            "add",
            "--mirror=fetch",
            "-t",
            "x",
            "-m",
            "y",
            "j",
            "../one",
        ],
        &[
            "remote",
            "add",
            "--mirror=push",
            "-t",
            "x",
            "-m",
            "y",
            "j",
            "../one",
        ],
        &[
            "remote",
            "add",
            "--mirror",
            "--no-tags",
            "-t",
            "heads/x",
            "mm",
            "u",
        ],
        &["remote", "add", "--mirror", "--no-mirror", "nm", "u"],
        &["remote", "add", "a", "../one"],
        &["remote", "add", "a/b", "../one"],
        &["remote", "add", "bad name", "../one"],
        &["remote", "add", "x..y", "../one"],
        &["remote", "add", "some:url", "x"],
        &["remote", "add", "", "../one"],
        &["remote", "add", "-x", "u"],
        &["remote", "add", "a*b", "u"],
        &["remote", "add", "q.lock", "u"],
        &["remote", "add", ".q", "u"],
        &["remote", "add", "q/", "u"],
        &["remote", "add", "@", "u"],
        &["remote", "add", "q@{1}", "u"],
        &["remote", "add", "HEAD", "u"],
        &["remote", "add", "k", ""],
        &["remote", "add", "outer/inner", "u"],
        &["remote", "add", "outer", "u"],
        &["remote", "add", "-m", "nonexistent", "hx", "../one"],
        &["remote"],
        &["remote", "-v"],
    ];
    for args in cases {
        same_in(&mut fx, "c", args);
    }
    // Existing in the repository configuration only: global remotes and
    // `-c` do not count.
    let mut fx = fx
        .git(["config", "--global", "remote.gl.url", "gurl"])
        .git(["-C", "c", "config", "remote.vcs.vcs", "bar"]);
    let cases: &[&[&str]] = &[
        &["remote", "add", "vcs", "u"],
        &["remote", "rename", "origin", "vcs"],
        &["remote", "get-url", "gl"],
        &["remote", "add", "gl", "x"],
        &["-c", "remote.cl.url=x", "remote", "remove", "cl"],
        &["remote", "-v"],
    ];
    for args in cases {
        same_in(&mut fx, "c", args);
    }
}

#[test]
fn add_with_fetch() {
    let mut fx = fixture().git(["init", "-q", "-b", "main", "e"]);
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["remote", "add", "-f", "x", "../two"],
        &["remote", "add", "-f", "-m", "side", "y", "../two"],
        &["remote", "add", "-f", "--tags", "z", "../one"],
        &["remote", "add", "-f", "bad", "../nosuch"],
    ];
    for args in cases {
        same_in(&mut fx, "e", args);
    }
}

#[test]
fn urls_and_branches() {
    let mut fx = fixture()
        .git(["-C", "c", "remote", "add", "o", "a1"])
        .git(["-C", "c", "config", "remote.o.tagOpt", "--tags"])
        .git(["-C", "c", "config", "remote.k.fetch", "x"]);
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["remote", "set-url", "nosuch", "x"],
        &["remote", "set-url", "--add", "o", "a2"],
        &["remote", "set-url", "--add", "o", "b3"],
        &["remote", "set-url", "o", "X"],
        &["remote", "set-url", "o", "Y", "a"],
        &["remote", "set-url", "o", "Z", "b"],
        &["remote", "set-url", "o", "W", "nomatch"],
        &["remote", "set-url", "o", "n(ew", "(bad"],
        &["remote", "set-url", "--delete", "o", "nomatch"],
        &["remote", "set-url", "--delete", "o", ".*"],
        &["remote", "set-url", "--delete", "o", "a2"],
        &["remote", "set-url", "--push", "o", "p1", "nomatch"],
        &["remote", "set-url", "--push", "o", "p1"],
        &["remote", "set-url", "--push", "o", "q1"],
        &["remote", "set-url", "--push", "--add", "o", "p2"],
        &["remote", "set-url", "--push", "o", "S"],
        &["remote", "set-url", "--push", "o", "foo", "p+2"],
        &["remote", "get-url", "--push", "--all", "o"],
        &["remote", "set-url", "--push", "--delete", "o", "foo"],
        &["remote", "set-url", "--push", "--delete", "o", ".*"],
        &["remote", "set-url", "--push", "--delete", "o", "x"],
        &["remote", "set-url", "k", "u"],
        &["remote", "set-url", "--delete", "k", "u"],
        &["remote", "set-url", "--add", "k", "v"],
        &["remote", "set-branches", "nosuch", "a"],
        &["remote", "set-branches", "o", "a", "b"],
        &["remote", "set-branches", "--add", "o", "c"],
        &["remote", "set-branches", "o"],
        &["remote", "set-branches", "--add", "o"],
        &["remote", "set-branches", "o", "a", "b"],
        &["remote", "set-branches", "--add", "o", "d"],
        &[
            "-c",
            "core.configLockTimeout=0",
            "remote",
            "set-url",
            "o",
            "x",
        ],
    ];
    for args in cases {
        same_in(&mut fx, "c", args);
    }
    let mut fx = fx.git(["-C", "c", "config", "remote.o.mirror", "true"]);
    same_in(&mut fx, "c", &["remote", "set-branches", "o", "heads/x"]);
    same_in(&mut fx, "c", &["remote", "-v"]);
    same_in(&mut fx, "c", &["remote", "show", "-n", "o", "k"]);
}

#[test]
fn rename_and_remove() {
    let mut fx = fixture()
        .git(["-C", "c", "remote", "add", "other", "../two"])
        .git(["-C", "c", "fetch", "-q", "other"])
        .git(["-C", "c", "config", "core.logAllRefUpdates", "always"])
        .git([
            "-C",
            "c",
            "update-ref",
            "refs/remotes/origin/deep/x",
            "HEAD",
        ])
        .git(["-C", "c", "branch", "foo"])
        .git(["-C", "c", "config", "branch.main.pushRemote", "origin"])
        .git(["-C", "c", "config", "branch.main.rebase", "true"])
        .git(["-C", "c", "config", "branch.y.remote", "other"])
        .git(["-C", "c", "config", "branch.y.pushremote", "origin"])
        .git(["-C", "c", "config", "remote.pushdefault", "origin"])
        .git([
            "-C",
            "c",
            "config",
            "--add",
            "remote.origin.fetch",
            "+refs/tags/*:refs/remotes/origin/tags/*",
        ])
        .git([
            "-C",
            "c",
            "config",
            "--add",
            "remote.origin.fetch",
            "refs/heads/main:refs/heads/foo",
        ])
        .git([
            "-C",
            "c",
            "config",
            "--add",
            "remote.origin.fetch",
            "^refs/heads/no",
        ])
        .git([
            "-C",
            "c",
            "config",
            "--add",
            "remote.origin.push",
            "refs/heads/*:refs/remotes/origin/*",
        ])
        .git(["config", "--global", "remote.pushDefault", "origin"]);
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["remote", "rename", "nosuch", "x"],
        &["remote", "rename", "origin", "other"],
        &["remote", "rename", "origin", "invalid...name"],
        &["remote", "rename", "origin", "origin"],
        &["remote", "rename", "--progress", "origin", "up"],
        &["remote", "rename", "--no-progress", "up", "up/nested"],
        &["remote", "rename", "up/nested", "up"],
        &["remote", "remove", "nosuch"],
        &["remote", "rm", "up"],
        &["remote", "remove", "other"],
    ];
    for args in cases {
        same_in(&mut fx, "c", args);
    }
}

#[test]
fn rename_conflicts_and_legacy() {
    let mut fx = fixture()
        .git(["-C", "c", "remote", "add", "parent", "u"])
        .git(["-C", "c", "config", "branch.x.remote", "parent"])
        .git(["-C", "c", "update-ref", "refs/remotes/parent/child", "HEAD"])
        .git(["-C", "c", "remote", "add", "unborn", "u"])
        .git(["-C", "c", "symbolic-ref", "refs/remotes/unborn/HEAD", "refs/remotes/unborn/nonexistent"])
        .write("c/.git/remotes/leg", "URL: ../one\nPush: refs/heads/a:refs/heads/b\nPull: refs/heads/main:refs/heads/origin\n")
        .write("c/.git/branches/br", "../one#side\n")
        .git(["-C", "c", "config", "remote.oops.fetch", "+refs/*:refs/*"])
        .git(["-C", "c", "tag", "footag"])
        .git(["-C", "c", "config", "remote.nd.url", "u"])
        .git(["-C", "c", "config", "remote.nd.fetch", "+refs/heads/*:refs/heads/nd/*"])
        .git(["-C", "c", "update-ref", "refs/remotes/nd/x", "HEAD"]);
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["remote", "rename", "nd", "nd2"],
        &["remote", "rename", "parent", "parent/child"],
        &["remote", "rename", "unborn", "unborn-renamed"],
        &["remote", "add", "leg", "x"],
        &["remote", "rename", "leg", "leg"],
        &["remote", "rename", "br", "br"],
        &["remote", "remove", "oops"],
    ];
    for args in cases {
        same_in(&mut fx, "c", args);
    }
}

#[test]
fn set_head() {
    let mut fx = fixture()
        .git(["init", "-q", "-b", "main", "empty"])
        .git(["-C", "c", "remote", "add", "e", "../empty"])
        .git(["-C", "c", "remote", "add", "two", "../two"])
        .git(["-C", "c", "fetch", "-q", "two"])
        .git([
            "-C",
            "c",
            "config",
            "remote.origin.followRemoteHEAD",
            "always",
        ]);
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["remote", "set-head", "origin", "-d"],
        &["remote", "set-head", "origin", "-d"],
        &["remote", "set-head", "nosuch", "-d"],
        &["remote", "set-head", "origin", "--auto"],
        &["remote", "set-head", "origin", "-a"],
        &["remote", "set-head", "origin", "side"],
        &["remote", "set-head", "origin", "nosuch"],
        &["remote", "set-head", "origin", "a..b"],
        &["remote", "set-head", "origin", "-a"],
        &["remote", "set-head", "two", "-a"],
        &["remote", "set-head", "e", "-a"],
        &["remote", "set-head", "nosuch", "-a"],
    ];
    for args in cases {
        same_in(&mut fx, "c", args);
    }
    let mut fx = fx.git([
        "-C",
        "c",
        "update-ref",
        "--no-deref",
        "refs/remotes/origin/HEAD",
        "HEAD",
    ]);
    same_in(&mut fx, "c", &["remote", "set-head", "origin", "-a"]);
    let mut fx = fx.git([
        "-C",
        "c",
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/heads/main",
    ]);
    same_in(&mut fx, "c", &["remote", "set-head", "origin", "-a"]);
    // A detached remote HEAD with two branches at its commit.
    let mut fx = fx.git(["-C", "one", "checkout", "-q", "--detach"]);
    same_in(&mut fx, "c", &["remote", "set-head", "origin", "-a"]);
    same_in(&mut fx, "c", &["remote", "show", "origin"]);
    let mut fx = fx.write("c/.git/refs/remotes/origin/HEAD.lock", "");
    let normalize =
        rgit_difftest::Normalizer::regex(r"Unable to create '[^']*'", "Unable to create 'X'");
    for args in [
        &["remote", "set-head", "origin", "main"][..],
        &["remote", "set-head", "origin", "-d"],
    ] {
        let path = server_path(&fx);
        fx.compare(args)
            .in_dir("c")
            .repo("c")
            .env("PATH", path)
            .normalize_stderr(normalize.clone())
            .run();
    }
}

#[test]
fn show_and_prune() {
    let mut fx = fixture()
        .git(["-C", "c", "remote", "add", "two", "../two"])
        .git([
            "-C",
            "c",
            "config",
            "--add",
            "remote.two.pushurl",
            "../three",
        ])
        .git([
            "-C",
            "c",
            "config",
            "--add",
            "remote.origin.fetch",
            "refs/heads/main:refs/heads/upstream",
        ])
        .git(["-C", "c", "fetch", "-q", "origin"])
        .git(["-C", "c", "fetch", "-q", "two"])
        .git(["-C", "c", "checkout", "-q", "-b", "ahead", "origin/main"])
        .git(["-C", "c", "commit", "-q", "--allow-empty", "-m", "ahead"])
        .git(["-C", "c", "checkout", "-q", "main"])
        .git([
            "-C",
            "c",
            "branch",
            "-q",
            "--track",
            "octopus",
            "origin/main",
        ])
        .git([
            "-C",
            "c",
            "branch",
            "-q",
            "--track",
            "rebase",
            "origin/main",
        ])
        .git(["-C", "c", "branch", "-q", "--track", "inter", "origin/main"])
        .git(["-C", "c", "branch", "-d", "-r", "origin/main"])
        .git(["-C", "c", "config", "branch.rebase.rebase", "true"])
        .git(["-C", "c", "config", "branch.inter.rebase", "merges"])
        .git([
            "-C",
            "c",
            "config",
            "branch.octopus.merge",
            "topic-a topic-b topic-c",
        ])
        .git(["-C", "one", "commit", "-q", "--allow-empty", "-m", "update"])
        .git(["-C", "one", "branch", "new"])
        .git(["-C", "c", "config", "--add", "remote.origin.push", ":"])
        .git([
            "-C",
            "c",
            "config",
            "--add",
            "remote.origin.push",
            "refs/heads/main:refs/heads/upstream",
        ])
        .git([
            "-C",
            "c",
            "config",
            "--add",
            "remote.origin.push",
            "+refs/tags/lastbackup",
        ])
        .git([
            "-C",
            "c",
            "config",
            "--add",
            "remote.two.push",
            "+refs/heads/ahead:refs/heads/main",
        ])
        .git([
            "-C",
            "c",
            "config",
            "--add",
            "remote.two.push",
            "refs/heads/main:refs/heads/another",
        ])
        .git(["-C", "c", "update-ref", "refs/remotes/origin/gone", "HEAD"]);
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["remote", "show", "origin", "two"],
        &["remote", "-v", "show", "-n", "origin", "two"],
        &["remote", "show", "nosuch"],
        &["remote", "show", "-n", "nosuch"],
        &["remote", "show", "../one"],
        &["remote", "prune", "-n", "origin"],
        &["remote", "prune", "--dry-run", "origin", "two"],
        &["remote", "prune", "origin"],
        &["remote", "prune", "origin"],
        &["remote", "prune", "nosuch"],
    ];
    for args in cases {
        same_in(&mut fx, "c", args);
    }
    let mut fx = fx
        .git([
            "-C",
            "c",
            "config",
            "--add",
            "remote.origin.fetch",
            "^refs/heads/main",
        ])
        .git(["-C", "c", "config", "branch.ahead.rebase", "interactive"]);
    same_in(&mut fx, "c", &["remote", "show", "origin"]);
    same_in(&mut fx, "c", &["remote", "show", "-n", "origin"]);
    let mut fx = fx.git(["-C", "c", "config", "remote.two.mirror", "true"]);
    same_in(&mut fx, "c", &["remote", "show", "two"]);
    // Pruning leaves a dangling `HEAD`.
    let mut fx = fx
        .git(["-C", "c", "remote", "set-head", "origin", "side"])
        .git(["-C", "one", "checkout", "-q", "new"])
        .git(["-C", "one", "branch", "-D", "side"]);
    same_in(&mut fx, "c", &["remote", "prune", "origin"]);
}

#[test]
fn update_groups() {
    let mut fx = fixture()
        .git(["-C", "c", "remote", "add", "two", "../two"])
        .git(["-C", "c", "remote", "add", "x", "../one"])
        .git(["-C", "c", "config", "remote.x.skipDefaultUpdate", "true"]);
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["remote", "update", "nosuch"],
        &["remote", "update"],
        &["remote", "update", "default"],
        &["remote", "update", "two", "origin", "two"],
        &["remote", "-v", "update", "-p"],
    ];
    for args in cases {
        same_in(&mut fx, "c", args);
    }
    let mut fx = fx.git(["-C", "c", "config", "remotes.g", "x nosuch"]).git([
        "-C",
        "c",
        "config",
        "remotes.default",
        "\t two  \n",
    ]);
    same_in(&mut fx, "c", &["remote", "update", "g"]);
    same_in(&mut fx, "c", &["remote", "update"]);
    // A single remote is fetched directly.
    let mut fx = fx
        .git(["init", "-q", "-b", "main", "lone"])
        .git(["-C", "lone", "remote", "add", "only", "../one"]);
    same_in(&mut fx, "lone", &["remote", "update"]);
}
