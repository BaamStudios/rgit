// SPDX-License-Identifier: MIT
//! Differential tests of the user formats of `rgit log` and `rgit
//! rev-list` (`--format`, `--pretty=format:`, aliases, decorations,
//! padding, wrapping, trailers) against the reference git.

use rgit_difftest::{Check, Fixture};

fn commit(fx: Fixture, msg: &str) -> Fixture {
    fx.git(["commit", "-q", "--allow-empty", "-m", msg]).tick()
}

/// Branches, a merge, tags, a body with a tab, trailers, a long subject
/// and a note.
fn history() -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"]);
    fx = commit(fx, "root");
    fx = commit(
        fx,
        "second: with a subject that is quite a bit longer than forty columns\n\n\
         body line one\n\tindented with a tab\n\n\
         Signed-off-by: A U Thor <author@example.com>\n\
         Acked-by: Someone\n  folded <s@example.com>\n\
         [ not a trailer ]\nCloses: #12",
    );
    fx = fx.git(["branch", "side"]);
    fx = commit(fx, "main one\ncontinued subject");
    fx = fx.git(["checkout", "-q", "side"]);
    fx = commit(fx, "Side: fix the (bug)! ...");
    fx = fx
        .git(["checkout", "-q", "main"])
        .git(["merge", "-q", "--no-ff", "-m", "merge side", "side"])
        .tick()
        .git(["tag", "-a", "-m", "tag", "v1", "HEAD~1"])
        .git(["tag", "light", "side"])
        .git(["update-ref", "refs/remotes/origin/main", "HEAD~1"])
        .git(["notes", "add", "-m", "a note", "HEAD~1"]);
    commit(fx, "after")
}

fn same(fx: &mut Fixture, args: &[&str]) {
    fx.compare(args).checks(Check::OUTPUT).run();
}

#[test]
fn placeholders() {
    let mut fx = history();
    for format in [
        "%H %h %T %t %P %p",
        "%an|%ae|%al|%aN|%aE|%aL|%cn|%ce|%cl|%cN|%cE|%cL",
        "%ad|%aD|%at|%ai|%aI|%as|%ah|%cd|%cD|%ct|%ci|%cI|%cs|%ch",
        "%d|%D|%(decorate)|%(decorate:prefix=[,suffix=],separator=%x3B,tag=,pointer=>)",
        "%s|%f|%e",
        "%b|",
        "%B|",
        "%N|",
        "%m%G?%gd%gs%S",
        "%n%%%x41%x4g%q %(foo) %",
        "[%+s][%-b][% s][%+b]x%n%-b",
        "%(trailers)|%(trailers:only)|%(trailers:unfold)",
        "%(trailers:key=acked-by:,valueonly)|%(trailers:keyonly,separator=%x2C )",
        "%(trailers:key_value_separator=%x3D,unfold,separator=;)|%(trailers:key)",
        "%(trailers:only=no,key=Closes)|%(trailers:only=bogus)",
    ] {
        same(&mut fx, &["log", &format!("--format={format}")]);
    }
    for args in [
        &["log", "--format=%ad", "--date=iso"][..],
        &["log", "--format=%ad", "--date=iso-strict"],
        &["log", "--format=%ad", "--date=rfc"],
        &["log", "--format=%ad", "--date=short"],
        &["log", "--format=%ad", "--date=raw"],
        &["log", "--format=%ad", "--date=unix"],
        &["log", "--format=%ad", "--date=format:%Y/%m/%d %H"],
        &["log", "--format=%h|%p|%t", "--abbrev=10"],
        &["log", "--format=%h|%p", "--no-abbrev"],
        &["log", "--format=%h", "--abbrev=0"],
        &["log", "--format=%P", "--first-parent"],
        &["log", "--format=%m %s", "--left-right", "main...side"],
        &["log", "--format=%m %s", "--boundary", "side..main"],
        &["log", "--format=%S %s", "main", "side"],
        &["log", "--format=%S %s", "--all"],
        &["log", "--format=[%N]", "--no-notes"],
        &["log", "--format=%aN %aE"],
    ] {
        same(&mut fx, args);
    }
}

#[test]
fn layout() {
    let mut fx = history();
    for format in [
        "[%<(10)%s][%>(10)%s][%><(10)%s]",
        "[%<(12,trunc)%s][%<(12,ltrunc)%s][%<(12,mtrunc)%s][%<(1,trunc)%s]",
        "%h %<|(40)%s|",
        "%h %>|(-10)%s|",
        "[ab    %>>(10)%s][x %>>(10,trunc)%s]",
        "[%<(10,trunc)%s%>>(10,ltrunc)% an]",
        "[%<(4)%%%an][%<(4)%x41%an][%<(6)%m%an][%<(4)%<(6)%an]",
        "%<(0)x %<(-1)x %<(3,foo)x %<(16385)x %<|(-81)x",
        "%w(20,2,4)%B|",
        "%w(0,1,2)%B%w(0)X",
        "%w(1)%s|",
        "%w(12,0,4)AA %s BB%w(0)CC",
        "%w(x)%s %w(1,2,3,4)%s %+w(2)",
        "%w(10,1,2)%<(15)%an|%>(15,trunc)%s|",
    ] {
        same(&mut fx, &["log", &format!("--format={format}")]);
    }
    fx.compare(["log", "--format=%h %<|(-20)%s|"])
        .env("COLUMNS", "50")
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn colors() {
    let mut fx = history();
    for args in [
        &["log", "--format=%Cred%h%Creset %C(blue bold)%s%C(reset)"][..],
        &[
            "log",
            "--color",
            "--format=%Cred%h%Creset %C(blue bold)%s%C(reset)",
        ],
        &["log", "--format=%C(always,green)%h%C(auto,red)%s"],
        &[
            "log",
            "--color=always",
            "--format=%C(auto)%h%d %H x%C(auto)%h",
        ],
        &["log", "--color=always", "--format=%C(auto)%D|%(decorate)"],
        &["log", "--color=always", "--format=%<(3,trunc)%C(auto)%h|"],
        &["log", "--color=always", "--format=%C(bogus)%h"],
        &["log", "--format=%C(bogus)%h %C(red %Cyellow"],
        &["log", "--color=always", "--oneline", "--decorate"],
        &["rev-list", "--color", "--format=%Cred%h%Creset", "HEAD"],
    ] {
        same(&mut fx, args);
    }
}

#[test]
fn separators_and_names() {
    let mut fx = history()
        .git(["config", "pretty.short-h", "format:<%h>"])
        .git(["config", "pretty.loop1", "loop2"])
        .git(["config", "pretty.loop2", "loop1"])
        .git(["config", "pretty.missing", "nosuchformat"]);
    for args in [
        &["log", "--format="][..],
        &["log", "--format=format:"],
        &["log", "--format=%s"],
        &["log", "--pretty=format:%s"],
        &["log", "--pretty=tformat:%b"],
        &["log", "--pretty=format:%-b"],
        &["log", "-z", "--format=%s"],
        &["log", "-z", "--format=format:%s"],
        &["log", "-z", "--oneline"],
        &["log", "-z", "-2"],
        &["log", "--pretty=SHORT-H"],
        &["log", "--pretty=loop1"],
        &["log", "--pretty=missing"],
        &["log", "--pretty=one"],
        &["log", "--pretty=med", "-1"],
        &["log", "--pretty=reference"],
        &["log", "--pretty=reference", "--date=iso"],
        &["-c", "log.date=rfc", "log", "--pretty=reference"],
        &["rev-list", "--format=%s", "HEAD"],
        &["rev-list", "--format=%b", "HEAD"],
        &["rev-list", "--format=", "HEAD"],
        &["rev-list", "--no-commit-header", "--format=%s", "HEAD"],
        &[
            "rev-list",
            "--abbrev-commit",
            "--format=%s",
            "--parents",
            "HEAD",
        ],
        &["rev-list", "--left-right", "--format=%m%s", "main...side"],
        &["rev-list", "--pretty=reference", "HEAD"],
        &["rev-list", "--format=%S|%N", "-1", "HEAD"],
        &["log", "--decorate", "-2"],
        &["log", "--decorate=full", "--oneline"],
        &[
            "log",
            "--oneline",
            "--decorate",
            "--decorate-refs=refs/tags",
        ],
        &[
            "log",
            "--oneline",
            "--decorate",
            "--decorate-refs-exclude=refs/heads",
        ],
        &["log", "--format=%d", "--clear-decorations"],
        &["log", "--format=%D", "--decorate=full"],
    ] {
        same(&mut fx, args);
    }
    for (key, value, args) in [
        ("format.pretty", "%h %s", &["log"][..]),
        ("format.pretty", "%h %s", &["log", "--pretty"]),
        ("format.pretty", "%h %s", &["rev-list", "HEAD"]),
        ("log.date", "short", &["log", "--format=%ad"]),
        ("log.decorate", "full", &["log", "--oneline"]),
        (
            "log.excludeDecoration",
            "refs/tags",
            &["log", "--format=%d"],
        ),
    ] {
        let mut fx = history().git(["config", key, value]);
        same(&mut fx, args);
    }
}

#[test]
fn latin1_commits() {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .git(["config", "i18n.commitEncoding", "ISO8859-1"])
        .write("msg", b"gr\xfc\xdfe aus K\xf6ln\n\nk\xf6rper\n");
    let mut fx = fx
        .git(["commit", "-q", "--allow-empty", "-F", "msg"])
        .tick();
    fx = fx.git(["config", "--unset", "i18n.commitEncoding"]);
    fx = commit(fx, "utf-8 \u{e4}");
    for args in [
        &["log", "--format=%s|%b|%e|%<(12,trunc)%s"][..],
        &["log", "--oneline"],
        &[
            "-c",
            "i18n.logOutputEncoding=ISO8859-1",
            "log",
            "--format=%s",
        ],
        &["rev-list", "--format=%B", "HEAD"],
        &["log", "--pretty=raw"],
    ] {
        same(&mut fx, args);
    }
}
