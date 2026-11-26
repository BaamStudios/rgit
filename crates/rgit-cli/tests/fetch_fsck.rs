// SPDX-License-Identifier: MIT
//! `fetch.fsckObjects` / `transfer.fsckObjects` with `fetch.fsck.<msg-id>`
//! on the client side of fetch and clone, against the reference git.

use rgit_difftest::{Check, Fixture};

/// A server repository `srv` whose branch `bad` holds a commit with a
/// broken committer email.
fn fixture() -> Fixture {
    let fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"));
    if fx.is_skipped() {
        return fx;
    }
    let fx = fx
        .git(["init", "-q", "srv"])
        .write("srv/a", "a\n")
        .git(["-C", "srv", "add", "a"])
        .git(["-C", "srv", "commit", "-q", "-m", "one"]);
    let tree = String::from_utf8(fx.git_stdout(["-C", "srv", "rev-parse", "HEAD^{tree}"])).unwrap();
    let text = format!(
        "tree {}\nauthor A <a@b> 1 +0000\ncommitter Bad <bad@b 1 +0000\n\nbad\n",
        tree.trim()
    );
    let fx = fx.write("bad-commit", text);
    let id = String::from_utf8(fx.git_stdout([
        "-C",
        "srv",
        "hash-object",
        "-t",
        "commit",
        "-w",
        "--literally",
        "../bad-commit",
    ]))
    .unwrap();
    fx.git(["-C", "srv", "update-ref", "refs/heads/bad", id.trim()])
        .git(["init", "-q", "dst"])
}

#[test]
fn clone_and_fetch_check_objects() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let checks = [Check::ExitCode, Check::Stderr];
    for args in [
        &[
            "-c",
            "transfer.fsckObjects=true",
            "clone",
            "-q",
            "--no-local",
            "srv",
            "c1",
        ][..],
        &[
            "-c",
            "fetch.fsckObjects=true",
            "clone",
            "-q",
            "--no-local",
            "srv",
            "c2",
        ],
        &[
            "-c",
            "fetch.fsckObjects=true",
            "-c",
            "fetch.fsck.badEmail=warn",
            "clone",
            "-q",
            "--no-local",
            "srv",
            "c3",
        ],
        &[
            "-c",
            "fetch.fsckObjects=true",
            "-c",
            "fetch.fsck.badEmail=ignore",
            "clone",
            "-q",
            "--no-local",
            "srv",
            "c4",
        ],
        &[
            "-C",
            "dst",
            "-c",
            "fetch.fsckObjects=true",
            "fetch",
            "-q",
            "../srv",
            "bad",
        ],
        // fsck.<msg-id> does not apply to fetch.
        &[
            "-C",
            "dst",
            "-c",
            "fetch.fsckObjects=true",
            "-c",
            "fsck.badEmail=ignore",
            "fetch",
            "-q",
            "../srv",
            "bad",
        ],
        &[
            "-C",
            "dst",
            "-c",
            "transfer.fsckObjects=true",
            "-c",
            "fetch.fsckObjects=false",
            "fetch",
            "-q",
            "../srv",
            "bad",
        ],
    ] {
        fx.compare(args).checks(checks).run();
    }
    // fetch.fsck.skipList leaves the listed objects unchecked.
    let bad = fx.git_stdout(["-C", "srv", "rev-parse", "bad"]);
    let mut list = b"# skipped\n".to_vec();
    list.extend(bad);
    let mut fx = fx.write("skip", list);
    let skip = format!(
        "fetch.fsck.skipList={}",
        fx.work_dir().join("skip").display()
    );
    fx.compare([
        "-C",
        "dst",
        "-c",
        "fetch.fsckObjects=true",
        "-c",
        &skip,
        "fetch",
        "-q",
        "../srv",
        "bad",
    ])
    .checks(checks)
    .run();
}

/// `fetch.fsck.*` values are checked like git when the pack arrives: a
/// missing value or an unknown severity ends the fetch, unknown ids are
/// skipped with a warning, and the skip list and the ids that cannot be
/// demoted fail the way git's index-pack child does.
#[test]
fn fetch_checks_fsck_settings() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let bad = fx.git_stdout(["-C", "srv", "rev-parse", "bad"]);
    let mut fx = fx.write("skip", &bad).write("junk", b"[core]\n").write(
        "comments",
        [b"# c\n  ".as_slice(), bad.trim_ascii(), b"  # x\n\n"].concat(),
    );
    let dir = fx.work_dir();
    let path = |name: &str| dir.join(name).display().to_string();
    let settings: Vec<Vec<String>> = vec![
        vec!["fetch.fsck.skipList=does-not-exist".into()],
        vec![format!("fetch.fsck.skipList={}", path("junk"))],
        vec![format!("fetch.fsck.skipList={}", path("comments"))],
        vec!["fetch.fsck.skipList".into()],
        vec!["fetch.fsck.badEmail".into()],
        vec!["fetch.fsck.unterminatedHeader=warn".into()],
        vec!["fetch.fsck.nulInHeader=ignore".into()],
        vec!["fetch.fsck.unterminatedHeader=error".into()],
        vec!["fetch.fsck.badEmail=Warn".into()],
        vec!["fetch.fsck.badEmail=info".into()],
        vec![
            "fetch.fsck.whatEver=error".into(),
            "fetch.fsck.badEmail=warn".into(),
        ],
        vec!["fetch.fsck.badRefName=ignore".into()],
    ];
    for (i, set) in settings.iter().enumerate() {
        let mut args = vec![
            "-C".to_string(),
            "dst".into(),
            "-c".into(),
            "fetch.fsckObjects=true".into(),
        ];
        for s in set {
            args.push("-c".into());
            args.push(s.clone());
        }
        let refspec = format!("bad:refs/heads/b{i}");
        args.extend(["fetch".into(), "-q".into(), "../srv".into(), refspec]);
        fx.compare(&args)
            .checks([Check::ExitCode, Check::Stderr])
            .run();
    }
}

/// `index-pack --strict=<list>` and `unpack-objects --strict=<list>`: the
/// list syntax, skip lists and the ids that cannot be demoted.
#[test]
fn strict_lists_of_index_pack_and_unpack_objects() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let bad = fx.git_stdout(["-C", "srv", "rev-parse", "bad"]);
    let mut input = bad.clone();
    input.push(b'\n');
    let pack = {
        let out = std::process::Command::new(fx.reference_git())
            .current_dir(fx.work_dir().join("srv"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", fx.home_dir())
            .env("XDG_CONFIG_HOME", fx.home_dir().join(".config"))
            .args(["pack-objects", "--stdout", "--revs"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut c| {
                use std::io::Write;
                c.stdin.take().unwrap().write_all(&input)?;
                c.wait_with_output()
            })
            .unwrap();
        out.stdout
    };
    let mut fx = fx.write("skip", &bad).write("junk", b"x\n");
    let dir = fx.work_dir();
    let path = |name: &str| dir.join(name).display().to_string();
    let lists = [
        "--strict=badEmail=warn".to_string(),
        "--strict=unterminatedHeader=warn".into(),
        "--strict=NulInHeader=ignore".into(),
        "--strict=badEmail=WARN".into(),
        "--strict=badEmail=info".into(),
        "--strict=whatEver=warn".into(),
        "--strict=foo".into(),
        "--strict==warn".into(),
        "--strict=,".into(),
        "--strict=badEmail:warn  badDate=ignore".into(),
        format!("--strict=SkipList={}", path("skip")),
        format!("--strict=skiplist={}", path("nope")),
        format!("--strict=skiplist={}", path("junk")),
        format!("--strict=whatever=warn,skiplist={}", path("nope")),
        format!("--strict=skiplist={},whatever=warn", path("nope")),
    ];
    for list in &lists {
        fx.compare(["-C", "dst", "index-pack", "--stdin", list.as_str()])
            .stdin(pack.clone())
            .checks([Check::ExitCode, Check::Stderr])
            .run();
        fx.compare(["-C", "dst", "unpack-objects", "-q", list.as_str()])
            .stdin(pack.clone())
            .checks([Check::ExitCode, Check::Stderr])
            .run();
    }
}
