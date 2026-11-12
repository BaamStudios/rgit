// SPDX-License-Identifier: MIT

//! `config` against the reference `test-tool`. Repositories are written by
//! hand (the fixture's "git" is the reference test-tool, not git).

mod common;

use common::{Fixture, fixture};

/// Writes a minimal repository with `.git/config` = `config` at `dir`.
fn repo(fx: Fixture, dir: &str, config: &str) -> Fixture {
    let p = |rel: &str| {
        if dir.is_empty() {
            rel.to_string()
        } else {
            format!("{dir}/{rel}")
        }
    };
    fx.write(p(".git/HEAD"), "ref: refs/heads/main\n")
        .write(p(".git/objects/info/.keep"), "")
        .write(p(".git/refs/heads/.keep"), "")
        .write(p(".git/config"), config)
}

const CONFIG: &str = "\
[core]
\trepositoryformatversion = 0
[fruit]
\tapple = red and green
\tKiwi = Hairy
\tempty =
\tbare
\tlist = one
[Fruit]
\tlist = two
[fruit \"Sub Sect\"]
\tname = mixed
[fruit \"sub sect\"]
\tname = lower
[fruit]
\tlist = three
[num]
\tsmall = 42
\tword = many
\tbig = 5g
\tkilo = 2k
[flag]
\tyes = on
\tno = off
\tbare
\tbad = sometimes
[only]
\tnovalue
";

fn compare(fx: &mut Fixture, args: &[&str]) {
    fx.compare(args).output_only().run();
}

fn compare_env(fx: &mut Fixture, args: &[&str], env: &[(&str, &str)]) {
    let mut c = fx.compare(args).output_only();
    for (k, v) in env {
        c = c.env(k, v);
    }
    c.run();
}

#[test]
fn lookups() {
    let Some(fx) = fixture() else { return };
    let mut fx = repo(fx, "", CONFIG)
        .write(
            "other.cfg",
            "[fruit]\n\tlist = zero\n[extra]\n\tkey = val\n",
        )
        .write("broken.cfg", "[\n")
        .write("dir.cfg/.keep", "");
    let mut calls: Vec<Vec<&str>> = Vec::new();
    for op in [
        "get_value",
        "get_value_multi",
        "get_int",
        "get_bool",
        "get_string",
        "get",
        "git_config_int",
    ] {
        for key in [
            "fruit.apple",
            "FRUIT.Kiwi",
            "fruit.kiwi",
            "fruit.empty",
            "fruit.bare",
            "fruit.list",
            "fruit.Sub Sect.name",
            "fruit.sub sect.name",
            "fruit.SUB SECT.name",
            "num.small",
            "NUM.Small",
            "num.word",
            "num.big",
            "num.kilo",
            "flag.yes",
            "flag.no",
            "flag.bare",
            "flag.bad",
            "only.novalue",
            "missing.key",
            "nosection",
            "trailing.",
            ".leading",
            "bad.1name",
            "bad_section.name",
            "a.b c",
            "a..b",
        ] {
            calls.push(vec![op, key]);
        }
    }
    for c in &calls {
        let mut args = vec!["config"];
        args.extend_from_slice(c);
        compare(&mut fx, &args);
    }
    for args in [
        &["config"][..],
        &["config", "nosuch"],
        &["config", "nosuch", "a.b"],
        &["config", "get_value"],
        &["config", "get_value", "a.b", "extra"],
        &["config", "-h"],
        &["config", "read_early_config"],
        &["config", "read_early_config", "a.b", "c"],
        &["config", "iterate"],
        &["config", "iterate", "ignored"],
        &["config", "configset_get_value", "fruit.list"],
        &["config", "configset_get_value", "fruit.list", "other.cfg"],
        &[
            "config",
            "configset_get_value",
            "fruit.list",
            "other.cfg",
            ".git/config",
        ],
        &[
            "config",
            "configset_get_value_multi",
            "fruit.list",
            "other.cfg",
            ".git/config",
        ],
        &[
            "config",
            "configset_get_value_multi",
            "fruit.list",
            ".git/config",
            "other.cfg",
        ],
        &["config", "configset_get_value", "fruit.bare", ".git/config"],
        &["config", "configset_get_value", "extra.key", "other.cfg"],
        &["config", "configset_get_value", "nosection", "other.cfg"],
        &["config", "configset_get_value", "a.b", "missing.cfg"],
        &[
            "config",
            "configset_get_value",
            "a.b",
            "other.cfg",
            "missing.cfg",
        ],
        &["config", "configset_get_value", "a.b", "dir.cfg"],
        &["config", "configset_get_value", "a.b", "broken.cfg"],
        &["config", "read_early_config", "fruit.list"],
        &["config", "read_early_config", "FRUIT.LIST"],
        &["config", "read_early_config", "fruit.Sub Sect.name"],
        &["config", "read_early_config", "missing.key"],
        &["config", "read_early_config", "nosection"],
    ] {
        compare(&mut fx, args);
    }
}

#[test]
fn command_line_and_global() {
    let Some(fx) = fixture() else { return };
    let mut fx = repo(fx, "", CONFIG);
    let params = "'fruit.list=cmd' 'num.cmd=7' 'num.cmd=x' 'only.cmd' 'Mixed.Sub.KEY=v'";
    for args in [
        &["config", "iterate"][..],
        &["config", "get_value_multi", "fruit.list"],
        &["config", "get_value", "only.cmd"],
        &["config", "get_string", "only.cmd"],
        &["config", "get_int", "num.cmd"],
        &["config", "git_config_int", "num.cmd"],
        &["config", "get_bool", "num.cmd"],
        &["config", "read_early_config", "fruit.list"],
        &["config", "read_early_config", "num.cmd"],
    ] {
        compare_env(&mut fx, args, &[("GIT_CONFIG_PARAMETERS", params)]);
    }
    compare_env(
        &mut fx,
        &["config", "iterate"],
        &[
            ("GIT_CONFIG_COUNT", "2"),
            ("GIT_CONFIG_KEY_0", "count.a"),
            ("GIT_CONFIG_VALUE_0", "zero"),
            ("GIT_CONFIG_KEY_1", "count.b"),
            ("GIT_CONFIG_VALUE_1", "one"),
        ],
    );
    compare_env(
        &mut fx,
        &["config", "get_value", "a.b"],
        &[("GIT_CONFIG_PARAMETERS", "'unterminated")],
    );
}

#[test]
fn discovery() {
    let Some(fx) = fixture() else { return };
    let mut fx = repo(
        fx,
        "top",
        "[core]\n\trepositoryformatversion = 0\n[a]\n\tb = top\n",
    )
    .write("top/sub/deeper/.keep", "")
    .write("plain/.keep", "");
    let work = fx.work_dir();
    let abs_git = work.join("top/.git");
    let abs_git = abs_git.to_str().unwrap();
    for dir in ["top", "top/sub", "top/sub/deeper"] {
        fx.compare(["config", "iterate"])
            .in_dir(dir)
            .output_only()
            .run();
        fx.compare(["config", "read_early_config", "a.b"])
            .in_dir(dir)
            .output_only()
            .run();
    }
    fx.compare(["config", "iterate"])
        .in_dir("top/sub/deeper")
        .env("GIT_DIR", "../../.git")
        .output_only()
        .run();
    fx.compare(["config", "iterate"])
        .in_dir("top/sub")
        .env("GIT_DIR", abs_git)
        .output_only()
        .run();
    // Outside any repository (the fixture sets GIT_CEILING_DIRECTORIES).
    for args in [
        &["config"][..],
        &["config", "get_value", "a.b"],
        &["config", "read_early_config", "a.b"],
    ] {
        fx.compare(args).in_dir("plain").output_only().run();
    }
    // A ceiling at the top hides the repository from below.
    let top = work.join("top");
    fx.compare(["config", "read_early_config", "a.b"])
        .in_dir("top/sub")
        .env("GIT_CEILING_DIRECTORIES", top.to_str().unwrap())
        .output_only()
        .run();
}

#[test]
fn gitfile_and_bare() {
    let Some(fx) = fixture() else { return };
    let fx = repo(fx, "sep", "[a]\n\tb = separate\n");
    let target = fx.work_dir().join("sep/.git");
    let mut fx = fx
        .write("wt/.git", format!("gitdir: {}\n", target.display()))
        .write("wt/s/.keep", "")
        .write("bare.git/HEAD", "ref: refs/heads/main\n")
        .write("bare.git/objects/.keep", "")
        .write("bare.git/refs/.keep", "")
        .write(
            "bare.git/config",
            "[core]\n\tbare = true\n[a]\n\tb = bare\n",
        );
    for dir in ["wt", "wt/s", "bare.git", "bare.git/refs"] {
        fx.compare(["config", "iterate"])
            .in_dir(dir)
            .output_only()
            .run();
        fx.compare(["config", "get_value", "a.b"])
            .in_dir(dir)
            .output_only()
            .run();
    }
}

#[test]
fn repository_format() {
    let Some(fx) = fixture() else { return };
    let mut fx = repo(
        fx,
        "new",
        "[core]\n\trepositoryformatversion = 999999\n[a]\n\tb = new\n",
    );
    fx = repo(
        fx,
        "ext",
        "[core]\n\trepositoryformatversion = 1\n[extensions]\n\tfrobnicate = yes\n\tnoop = 1\n[a]\n\tb = ext\n",
    );
    fx = repo(
        fx,
        "v1",
        "[core]\n\trepositoryformatversion = 1\n[extensions]\n\tworktreeConfig = true\n[a]\n\tb = v1\n",
    );
    let mut fx = repo(fx, "syntax", "[core]\n\trepositoryformatversion = 0\n[\n");
    for dir in ["new", "ext", "v1", "syntax"] {
        for args in [
            &["config", "read_early_config", "a.b"][..],
            &["config", "get_value", "a.b"],
            &["config", "iterate"],
        ] {
            fx.compare(args).in_dir(dir).output_only().run();
        }
    }
}

#[test]
fn includes() {
    let Some(fx) = fixture() else { return };
    let mut fx = repo(
        fx,
        "r",
        "[core]\n\trepositoryformatversion = 0\n\
         [include]\n\tpath = inc\n\
         [includeIf \"gitdir:r/\"]\n\tpath = by-gitdir\n\
         [includeIf \"onbranch:main\"]\n\tpath = by-branch\n\
         [includeIf \"onbranch:other\"]\n\tpath = ../broken\n",
    )
    .write("r/.git/inc", "[t]\n\tv = included\n")
    .write("r/.git/by-gitdir", "[t]\n\tv = gitdir\n")
    .write("r/.git/by-branch", "[t]\n\tv = branch\n")
    .write("r/broken", "[broken\n")
    .write("r/sub/.keep", "");
    for dir in ["r", "r/sub"] {
        for args in [
            &["config", "read_early_config", "t.v"][..],
            &["config", "get_value_multi", "t.v"],
            &["config", "iterate"],
        ] {
            fx.compare(args).in_dir(dir).output_only().run();
        }
    }
    let mut fx = fx.write("r/.git/HEAD", "ref: refs/heads/other\n");
    for args in [
        &["config", "read_early_config", "t.v"][..],
        &["config", "get_value_multi", "t.v"],
    ] {
        fx.compare(args).in_dir("r").output_only().run();
    }
}

#[test]
fn xdg_and_home() {
    let Some(fx) = fixture() else { return };
    let fx =
        repo(fx, "r", "[s]\n\tsource = repo\n").write("xdg/git/config", "[s]\n\tsource = xdg\n");
    let xdg = fx.work_dir().join("xdg");
    let mut fx = fx;
    for args in [
        &["config", "read_early_config", "s.source"][..],
        &["config", "get_value_multi", "s.source"],
        &["config", "iterate"],
    ] {
        fx.compare(args)
            .in_dir("r")
            .env("XDG_CONFIG_HOME", xdg.to_str().unwrap())
            .env("GIT_CONFIG_PARAMETERS", "'s.source=cmdline'")
            .output_only()
            .run();
    }
}
