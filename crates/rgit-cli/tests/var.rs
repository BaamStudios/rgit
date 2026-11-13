// SPDX-License-Identifier: MIT
//! `rgit var` against the reference git, with full control over the
//! environment (the difftest fixture always sets an identity and an editor,
//! which these cases must be able to unset).
//!
//! Time stamps of guessed "now" dates are masked; everything else is
//! compared byte for byte (exit code, stdout, stderr).
//!
//! The cases need a reference git of at least [`MIN_VERSION`] built with
//! the stock defaults (`vi`, `less`); older ones lack variables, and some
//! distributions (Debian) build git with other default programs. Otherwise
//! the tests are skipped.

use std::path::{Path, PathBuf};
use std::process::Command;

const MIN_VERSION: (u32, u32) = (2, 56);

/// `git version 2.56.0...` -> (2, 56).
fn parse_version(out: &[u8]) -> Option<(u32, u32)> {
    let s = String::from_utf8_lossy(out);
    let v = s.trim().strip_prefix("git version ")?;
    let mut it = v.split('.');
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

fn reference_git() -> Option<PathBuf> {
    let git = std::env::var_os("RGIT_REFERENCE_GIT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("git"));
    let version = Command::new(&git)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| parse_version(&o.stdout));
    let defaults = || {
        let out = Command::new(&git)
            .args(["var", "GIT_PAGER"])
            .env_remove("GIT_PAGER")
            .env_remove("PAGER")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output();
        out.is_ok_and(|o| o.stdout == b"less\n")
    };
    if version.is_some_and(|v| v >= MIN_VERSION) && defaults() {
        Some(git)
    } else if version.is_some() {
        eprintln!("skipped: reference git is older than 2.56 or not a stock build");
        None
    } else {
        assert!(
            std::env::var_os("RGIT_REQUIRE_REFERENCE").is_none_or(|v| v != "1"),
            "reference git required but not found"
        );
        None
    }
}

struct Lab {
    _dir: tempfile::TempDir,
    root: PathBuf,
    git: PathBuf,
    /// The reference's system config directory when it is not `/etc`: a
    /// build installed under another prefix (CI's pinned reference, built
    /// with prefix `$HOME`) names `<prefix>/etc/gitconfig`. rgit always
    /// reports `/etc`, so the reference output is mapped back.
    sysconfdir: Option<String>,
}

impl Lab {
    fn new() -> Option<Lab> {
        let git = reference_git()?;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        let mut lab = Lab {
            _dir: dir,
            root,
            git,
            sysconfdir: None,
        };
        let attr = lab
            .cmd(&lab.git, &[], &["var", "GIT_ATTR_SYSTEM"], &lab.root)
            .output()
            .unwrap();
        let attr = String::from_utf8_lossy(&attr.stdout);
        lab.sysconfdir = attr
            .trim_end()
            .strip_suffix("/gitattributes")
            .filter(|d| *d != "/etc")
            .map(|d| format!("{d}/"));
        let out = lab
            .cmd(&lab.git, &[], &["init", "-q", "repo"], &lab.root)
            .output()
            .unwrap();
        assert!(out.status.success());
        Some(lab)
    }

    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    fn cmd(&self, bin: &Path, env: &[(&str, &str)], args: &[&str], cwd: &Path) -> Command {
        let mut c = Command::new(bin);
        c.env_clear();
        if let Some(p) = std::env::var_os("PATH") {
            c.env("PATH", p);
        }
        c.env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("LC_ALL", "C")
            .env("TZ", "UTC");
        for (k, v) in env {
            c.env(k, v);
        }
        c.args(args).current_dir(cwd);
        c
    }

    fn run(&self, bin: &Path, env: &[(&str, &str)], args: &[&str]) -> String {
        let out = self.cmd(bin, env, args, &self.repo()).output().unwrap();
        format!(
            "exit {}\n--- stdout\n{}--- stderr\n{}",
            out.status.code().unwrap_or(-1),
            mask_times(&String::from_utf8_lossy(&out.stdout)),
            String::from_utf8_lossy(&out.stderr)
        )
    }

    fn same(&self, env: &[(&str, &str)], args: &[&str]) {
        // A reference with another sysconfdir would read a different system
        // config file than rgit; point it at the stock one.
        let mut ref_env = env.to_vec();
        let reads_system = env
            .iter()
            .any(|(k, v)| *k == "GIT_CONFIG_NOSYSTEM" && *v == "0")
            && !env.iter().any(|(k, _)| *k == "GIT_CONFIG_SYSTEM");
        if self.sysconfdir.is_some() && reads_system {
            ref_env.push(("GIT_CONFIG_SYSTEM", "/etc/gitconfig"));
        }
        let mut want = self.run(&self.git, &ref_env, args);
        if let Some(dir) = &self.sysconfdir {
            want = want.replace(dir.as_str(), "/etc/");
        }
        let got = self.run(Path::new(env!("CARGO_BIN_EXE_rgit")), env, args);
        assert_eq!(got, want, "rgit {args:?} with {env:?}");
    }
}

/// Replace `<10 digits> <±4 digits>` by `<T>` (dates of "now").
fn mask_times(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        let is_time = b.len() - i >= 16
            && b[i..i + 10].iter().all(u8::is_ascii_digit)
            && b[i + 10] == b' '
            && matches!(b[i + 11], b'+' | b'-')
            && b[i + 12..i + 16].iter().all(u8::is_ascii_digit);
        if is_time {
            out.push_str("<T>");
            i += 16;
        } else {
            let c = s[i..].chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

const ID: &[(&str, &str)] = &[
    ("GIT_AUTHOR_NAME", "A U Thor"),
    ("GIT_AUTHOR_EMAIL", "author@example.com"),
    ("GIT_COMMITTER_NAME", "C O Mitter"),
    ("GIT_COMMITTER_EMAIL", "committer@example.com"),
];

fn with<'a>(base: &[(&'a str, &'a str)], more: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    base.iter().chain(more).copied().collect()
}

#[test]
fn identities() {
    let Some(lab) = Lab::new() else { return };
    lab.same(ID, &["var", "GIT_AUTHOR_IDENT"]);
    lab.same(ID, &["var", "GIT_COMMITTER_IDENT"]);
    let dated = with(ID, &[("GIT_AUTHOR_DATE", "@1234567890 +0200")]);
    lab.same(&dated, &["var", "GIT_AUTHOR_IDENT"]);
    let dated = with(ID, &[("GIT_COMMITTER_DATE", "2005-04-07T22:13:13")]);
    lab.same(&dated, &["var", "GIT_COMMITTER_IDENT"]);
    let bad = with(ID, &[("GIT_AUTHOR_DATE", "garbage")]);
    lab.same(&bad, &["var", "GIT_AUTHOR_IDENT"]);
    lab.same(&bad, &["var", "-l"]);
    let messy = [
        ("GIT_AUTHOR_NAME", " .Pat <Doe> Jr.. "),
        ("GIT_AUTHOR_EMAIL", " <x@y>. "),
    ];
    lab.same(&messy, &["var", "GIT_AUTHOR_IDENT"]);
    let crud = [("GIT_AUTHOR_NAME", " ,;<>"), ("GIT_AUTHOR_EMAIL", "x@y")];
    lab.same(&crud, &["var", "GIT_AUTHOR_IDENT"]);
}

#[test]
fn identity_configuration() {
    let Some(lab) = Lab::new() else { return };
    let cfg = ["-c", "user.name=user", "-c", "user.email=user@example.com"];
    let mut args = cfg.to_vec();
    args.extend([
        "-c",
        "author.name=author",
        "-c",
        "committer.email=c@example.com",
    ]);
    args.extend(["var", "-l"]);
    lab.same(&[], &args);
    let env = [("GIT_AUTHOR_NAME", "env"), ("GIT_COMMITTER_EMAIL", "env@x")];
    lab.same(&env, &args);
    // Empty values.
    lab.same(
        &[("GIT_AUTHOR_EMAIL", "")],
        &[
            "-c",
            "user.name=n",
            "-c",
            "user.email=e",
            "var",
            "GIT_AUTHOR_IDENT",
        ],
    );
    lab.same(
        &[("GIT_AUTHOR_EMAIL", "x@y")],
        &[
            "-c",
            "user.name=n",
            "-c",
            "user.email=",
            "var",
            "GIT_AUTHOR_IDENT",
        ],
    );
    lab.same(
        &[("GIT_AUTHOR_NAME", "n")],
        &[
            "-c",
            "author.email=",
            "-c",
            "user.email=u",
            "var",
            "GIT_AUTHOR_IDENT",
        ],
    );
    lab.same(
        &[("GIT_COMMITTER_NAME", "n")],
        &["-c", "author.email=a@b", "var", "GIT_COMMITTER_IDENT"],
    );
    lab.same(
        &[("EMAIL", " <e@f> ")],
        &["-c", "user.name=n", "var", "GIT_AUTHOR_IDENT"],
    );
    lab.same(
        &[],
        &[
            "-c",
            "user.name=<>",
            "-c",
            "user.email=e",
            "var",
            "GIT_AUTHOR_IDENT",
        ],
    );
}

/// t7518: an empty configured name is not replaced by a guess.
#[test]
fn empty_names() {
    let Some(lab) = Lab::new() else { return };
    let email = [("GIT_AUTHOR_EMAIL", "a@b"), ("GIT_COMMITTER_EMAIL", "c@d")];
    lab.same(&email, &["-c", "user.name=", "var", "GIT_AUTHOR_IDENT"]);
    lab.same(&email, &["-c", "user.name=", "var", "GIT_COMMITTER_IDENT"]);
    lab.same(
        &with(&email, &[("GIT_AUTHOR_NAME", "")]),
        &["var", "GIT_AUTHOR_IDENT"],
    );
    lab.same(&email, &["-c", "user.name=", "var", "-l"]);
    lab.same(&with(&email, &[("GIT_AUTHOR_NAME", "<>")]), &["var", "-l"]);
}

/// t7517: `user.useConfigOnly`.
#[test]
fn use_config_only() {
    let Some(lab) = Lab::new() else { return };
    let only = ["-c", "user.useConfigOnly=true"];
    let run = |env: &[(&str, &str)], more: &[&str]| {
        let mut args = only.to_vec();
        args.extend(more);
        lab.same(env, &args);
    };
    run(&[], &["var", "GIT_AUTHOR_IDENT"]);
    run(&[], &["-c", "user.name=n", "var", "GIT_AUTHOR_IDENT"]);
    run(
        &[("EMAIL", "e@f")],
        &["-c", "user.name=n", "var", "GIT_AUTHOR_IDENT"],
    );
    run(
        &[],
        &[
            "-c",
            "user.name=n",
            "-c",
            "user.email=e",
            "var",
            "GIT_AUTHOR_IDENT",
        ],
    );
    run(&[("GIT_AUTHOR_EMAIL", "e")], &["var", "GIT_AUTHOR_IDENT"]);
    run(
        &[("GIT_COMMITTER_EMAIL", "e")],
        &["-c", "author.name=a", "var", "GIT_COMMITTER_IDENT"],
    );
    run(ID, &["var", "-l"]);
    lab.same(
        &[],
        &["-c", "user.useConfigOnly=bogus", "var", "GIT_EDITOR"],
    );
}

#[test]
fn editors() {
    let Some(lab) = Lab::new() else { return };
    for term in ["dumb", "vt100"] {
        let t = ("TERM", term);
        lab.same(&[t], &["var", "GIT_EDITOR"]);
        lab.same(
            &[t, ("VISUAL", "v"), ("EDITOR", "e")],
            &["var", "GIT_EDITOR"],
        );
        lab.same(&[t, ("VISUAL", "v")], &["var", "GIT_EDITOR"]);
        lab.same(&[t, ("EDITOR", "")], &["var", "GIT_EDITOR"]);
        lab.same(
            &[t, ("EDITOR", "e")],
            &["-c", "core.editor=c", "var", "GIT_EDITOR"],
        );
        lab.same(
            &[t, ("GIT_EDITOR", "g")],
            &["-c", "core.editor=c", "var", "GIT_EDITOR"],
        );
        lab.same(&[t], &["var", "GIT_SEQUENCE_EDITOR"]);
        lab.same(
            &[t, ("GIT_EDITOR", "g")],
            &["-c", "sequence.editor=s", "var", "GIT_SEQUENCE_EDITOR"],
        );
        lab.same(
            &[t, ("GIT_SEQUENCE_EDITOR", "")],
            &["var", "GIT_SEQUENCE_EDITOR"],
        );
        lab.same(&with(ID, &[t]), &["var", "-l"]);
    }
    lab.same(&[], &["var", "GIT_EDITOR"]);
    lab.same(&[], &["-c", "core.editor", "var", "GIT_EDITOR"]);
    let multi = with(ID, &[("GIT_EDITOR", "!f() {\n\techo Hello!\n}; f")]);
    lab.same(&multi, &["var", "-l"]);
}

#[test]
fn pager_and_branch() {
    let Some(lab) = Lab::new() else { return };
    lab.same(&[], &["var", "GIT_PAGER"]);
    lab.same(&[("PAGER", "more")], &["var", "GIT_PAGER"]);
    lab.same(&[("PAGER", "")], &["var", "GIT_PAGER"]);
    lab.same(
        &[("GIT_PAGER", "")],
        &["-c", "core.pager=most", "var", "GIT_PAGER"],
    );
    lab.same(&[], &["-c", "core.pager=most", "var", "GIT_PAGER"]);
    lab.same(&[], &["-c", "core.pager", "var", "GIT_PAGER"]);
    lab.same(&[], &["var", "GIT_DEFAULT_BRANCH"]);
    lab.same(
        &[("GIT_TEST_DEFAULT_INITIAL_BRANCH_NAME", "tb")],
        &["var", "GIT_DEFAULT_BRANCH"],
    );
    for b in [
        "foo", "a..b", "", "HEAD", "-x", "x.lock", "@", "a@{b", "x:y",
    ] {
        let c = format!("init.defaultBranch={b}");
        lab.same(&[], &["-c", &c, "var", "GIT_DEFAULT_BRANCH"]);
    }
    lab.same(&[], &["var", "GIT_SHELL_PATH"]);
}

#[test]
fn paths() {
    let Some(lab) = Lab::new() else { return };
    let home = lab.root.join("home");
    let home = home.to_str().unwrap();
    let xdg = format!("{home}/xdg");
    for v in [
        "GIT_ATTR_SYSTEM",
        "GIT_ATTR_GLOBAL",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_GLOBAL",
    ] {
        lab.same(&[], &["var", v]);
        lab.same(&[("XDG_CONFIG_HOME", &xdg)], &["var", v]);
        lab.same(&[("HOME", "")], &["var", v]);
    }
    lab.same(&[("GIT_ATTR_NOSYSTEM", "1")], &["var", "GIT_ATTR_SYSTEM"]);
    lab.same(
        &[("GIT_ATTR_NOSYSTEM", "bogus")],
        &["var", "GIT_ATTR_SYSTEM"],
    );
    lab.same(
        &[("GIT_CONFIG_NOSYSTEM", "0")],
        &["var", "GIT_CONFIG_SYSTEM"],
    );
    lab.same(
        &[
            ("GIT_CONFIG_NOSYSTEM", "0"),
            ("GIT_CONFIG_SYSTEM", "/dev/null"),
        ],
        &["var", "GIT_CONFIG_SYSTEM"],
    );
    lab.same(
        &[("GIT_CONFIG_GLOBAL", "/dev/null")],
        &["var", "GIT_CONFIG_GLOBAL"],
    );
    lab.same(&[("GIT_CONFIG_GLOBAL", "")], &["var", "GIT_CONFIG_GLOBAL"]);
    lab.same(
        &[],
        &["-c", "core.attributesFile=~/attr", "var", "GIT_ATTR_GLOBAL"],
    );
    lab.same(
        &[],
        &["-c", "core.attributesFile=~nouser/x", "var", "GIT_EDITOR"],
    );
    let all = with(
        ID,
        &[("GIT_CONFIG_NOSYSTEM", "0"), ("XDG_CONFIG_HOME", &xdg)],
    );
    lab.same(&all, &["-c", "foo.bar", "-c", "Foo.BAZ=1", "var", "-l"]);
}

#[test]
fn usage() {
    let Some(lab) = Lab::new() else { return };
    for args in [
        &["var"][..],
        &["var", "-h"],
        &["var", "--help-all"],
        &["var", "-h", "-h"],
        &["var", "GIT_FOO"],
        &["var", "-l", "GIT_EDITOR"],
        &["var", "a", "b"],
        &["var", "--foo"],
    ] {
        lab.same(ID, args);
    }
}

#[test]
fn bad_configuration_files() {
    let Some(lab) = Lab::new() else { return };
    let config = lab.repo().join(".git/config");
    let original = std::fs::read(&config).unwrap();
    for (extra, var) in [
        ("[user]\n\tname\n", "GIT_EDITOR"),
        ("[user]\n\tuseConfigOnly = maybe\n", "GIT_EDITOR"),
        ("[core]\n\tpager\n", "GIT_EDITOR"),
        ("[core]\n\tpager\n", "GIT_PAGER"),
        ("[core]\n\teditor\n", "GIT_PAGER"),
    ] {
        let mut data = original.clone();
        data.extend_from_slice(extra.as_bytes());
        std::fs::write(&config, data).unwrap();
        lab.same(ID, &["var", var]);
    }
}
