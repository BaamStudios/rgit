// SPDX-License-Identifier: MIT
//! Differential tests of the advice framework (`advice.*`, `GIT_ADVICE`,
//! `--no-advice`, `color.advice`) against the reference git, for every
//! advice an rgit command shows. Each advice is switched off by its own
//! key, set to true (no "Disable this message" line), switched off
//! globally, and given invalid values (fatal).

use rgit_difftest::Fixture;

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write(".gitignore", "ig\n")
        .write("ig", "")
        .write("f", "1\n")
        .write("g", "1\n")
        .git(["add", "f", "g"])
        .git(["commit", "-q", "-m", "c"])
        .write("f", "2\n")
        .write("g", "3\n")
        .git(["add", "g"])
        .write("g", "4\n")
}

/// Runs `cmd` with each way of configuring `advice.<name>` and compares
/// stdout, stderr and the exit code.
fn variants(fx: &mut Fixture, name: &str, cmd: &[&str]) {
    variants_in(fx, "", name, cmd);
}

/// [`variants`] in the subdirectory `dir`.
fn variants_in(fx: &mut Fixture, dir: &str, name: &str, cmd: &[&str]) {
    let key = |v: &str| format!("advice.{name}={v}");
    let configs: Vec<Vec<String>> = vec![
        vec![],
        vec!["-c".into(), key("true")],
        vec!["-c".into(), key("false")],
        vec!["-c".into(), key("")],
        vec!["-c".into(), key("bogus")],
        vec!["--no-advice".into()],
        vec!["-c".into(), "color.advice=always".into()],
        vec![
            "-c".into(),
            "color.ui=always".into(),
            "-c".into(),
            "color.advice.hint=red bold".into(),
        ],
    ];
    for pre in &configs {
        let mut argv: Vec<String> = pre.clone();
        argv.extend(cmd.iter().map(|s| s.to_string()));
        fx.compare(&argv).in_dir(dir).output_only().run();
    }
    for env in ["0", "false", "", "yes", "bogus"] {
        fx.compare(cmd)
            .in_dir(dir)
            .env("GIT_ADVICE", env)
            .output_only()
            .run();
    }
    // --no-advice wins over an invalid GIT_ADVICE.
    let mut argv = vec!["--no-advice"];
    argv.extend_from_slice(cmd);
    fx.compare(&argv)
        .in_dir(dir)
        .env("GIT_ADVICE", "bogus")
        .output_only()
        .run();
}

#[test]
fn add_empty_pathspec() {
    variants(&mut fixture(), "addEmptyPathspec", &["add"]);
}

#[test]
fn add_ignored_file() {
    variants(&mut fixture(), "addIgnoredFile", &["add", "ig"]);
}

#[test]
fn branch_ref_syntax() {
    variants(&mut fixture(), "refSyntax", &["branch", "a..b"]);
}

#[test]
fn branch_force_delete_and_upstream() {
    let mut fx = fixture()
        .git(["branch", "side"])
        .git(["switch", "-q", "side"]);
    if fx.is_skipped() {
        return;
    }
    fx = fx
        .git(["commit", "-q", "-am", "side"])
        .git(["switch", "-q", "-"]);
    variants(&mut fx, "forceDeleteBranch", &["branch", "-d", "side"]);
    variants(
        &mut fx,
        "setUpstreamFailure",
        &["branch", "--set-upstream-to=origin/nope"],
    );
}

/// t0018: `--no-advice` and `GIT_ADVICE` also switch off the status
/// hints. (No color variants: `status` does not color its output yet.)
#[test]
fn status_hints() {
    let mut fx = fixture();
    for pre in [
        &[][..],
        &["-c", "advice.statusHints=true"],
        &["-c", "advice.statusHints=false"],
        &["-c", "advice.statusHints=bogus"],
        &["-c", "advice.rmHints=bogus"],
        &["--no-advice"],
    ] {
        let mut argv: Vec<&str> = pre.to_vec();
        argv.push("status");
        fx.compare(&argv).output_only().run();
    }
    for env in ["0", "false", "true", "bogus"] {
        fx.compare(["status"])
            .env("GIT_ADVICE", env)
            .output_only()
            .run();
    }
}

/// Pushes rejected by the remote (`fetch first`, `non-fast-forward`) and
/// an unqualified destination; push advice has no "Disable" line.
#[test]
fn push_hints() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "--bare", "-b", "main", "r.git"])
        .git(["init", "-q", "-b", "main", "a"])
        .write("a/f", "1\n")
        .git(["-C", "a", "add", "f"])
        .git(["-C", "a", "commit", "-q", "-m", "1"])
        .git(["-C", "a", "push", "-q", "../r.git", "main"])
        .git(["clone", "-q", "r.git", "b"])
        .write("b/f", "2\n")
        .git(["-C", "b", "commit", "-q", "-am", "2"])
        .git(["-C", "b", "push", "-q", "origin", "main"])
        .write("a/f", "3\n")
        .git(["-C", "a", "commit", "-q", "-am", "3"]);
    variants_in(
        &mut fx,
        "a",
        "pushFetchFirst",
        &["push", "../r.git", "main"],
    );
    variants_in(
        &mut fx,
        "a",
        "pushUpdateRejected",
        &["push", "../r.git", "main"],
    );
    variants_in(
        &mut fx,
        "a",
        "pushUnqualifiedRefname",
        &["push", "../r.git", "main~0:foo"],
    );
    variants_in(
        &mut fx,
        "a",
        "pushUpdateRejected",
        &["push", "../r.git", "main~0:foo"],
    );
    if fx.is_skipped() {
        return;
    }
    // Once the remote tip is known locally the rejection is non-fast-forward.
    let mut fx = fx.git(["-C", "a", "fetch", "-q", "../r.git", "main"]);
    variants_in(
        &mut fx,
        "a",
        "pushNonFFCurrent",
        &["push", "../r.git", "main"],
    );
    variants_in(
        &mut fx,
        "a",
        "pushUpdateRejected",
        &["push", "../r.git", "main"],
    );
}

#[test]
fn rm_hints() {
    let mut fx = fixture();
    variants(&mut fx, "rmHints", &["rm", "f", "g"]);
    variants(&mut fx, "rmHints", &["rm", "--cached", "g"]);
}

#[test]
fn object_name_warning() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let out = fx.git_output(["rev-parse", "HEAD"]);
    let head = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    fx = fx.git(["update-ref", &format!("refs/heads/{head}"), "HEAD"]);
    let argv = ["rev-parse", head.as_str()];
    for pre in [
        &[][..],
        &["-c", "advice.objectNameWarning=true"],
        &["-c", "advice.objectNameWarning=false"],
        &["--no-advice"],
        &["-c", "color.advice=always"],
    ] {
        let mut a: Vec<&str> = pre.to_vec();
        a.extend_from_slice(&argv);
        fx.compare(&a).output_only().run();
    }
    fx.compare(argv).env("GIT_ADVICE", "0").output_only().run();
}

#[test]
fn init_default_branch_name() {
    let mut fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"));
    let key = |v: &str| format!("advice.defaultBranchName={v}");
    let cases: Vec<(Vec<String>, Option<&str>)> = vec![
        (vec!["-c".into(), key("true")], None),
        (vec!["-c".into(), key("bogus")], None),
        (vec![], Some("0")),
        (vec![], Some("bogus")),
    ];
    for (i, (pre, env)) in cases.into_iter().enumerate() {
        let mut argv = pre;
        argv.push("init".into());
        argv.push(format!("r{i}"));
        let mut cmp = fx
            .compare(&argv)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output_only();
        if let Some(v) = env {
            cmp = cmp.env("GIT_ADVICE", v);
        }
        cmp.run();
    }
    // `init` does not read `color.ui` for its advice.
    fx.compare(["-c", "color.ui=always", "init", "u"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output_only()
        .run();
}

/// `clone --branch <tag>` ends on a detached HEAD: the advice is plain
/// text (no `hint: `), shown with `--quiet` too. (An invalid `GIT_ADVICE`
/// is not compared: git then reports a failed checkout, which rgit does
/// not print yet.)
#[test]
fn clone_detached_head() {
    let mut fx = fixture().git(["tag", "v1"]);
    for (i, pre) in [
        &[][..],
        &["-c", "advice.detachedHead=true"],
        &["-c", "advice.detachedHead=false"],
        &["--no-advice"],
    ]
    .iter()
    .enumerate()
    {
        for quiet in [false, true] {
            let mut argv: Vec<String> = pre.iter().map(|s| s.to_string()).collect();
            argv.extend(["clone", "--branch", "v1"].map(String::from));
            if quiet {
                argv.push("-q".into());
            }
            argv.push(".".into());
            argv.push(format!("c{i}{quiet}"));
            fx.compare(&argv).output_only().run();
        }
    }
    fx.compare(["clone", "--branch", "v1", ".", "e"])
        .env("GIT_ADVICE", "0")
        .output_only()
        .run();
}
