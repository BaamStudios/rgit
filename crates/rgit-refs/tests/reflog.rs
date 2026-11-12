// SPDX-License-Identifier: MIT
//! Reflogs against the reference git: the files written, the list of
//! reflogs and `@{...}` lookups must match byte for byte.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rgit_date::DateContext;
use rgit_difftest::{Fixture, assert_same_text, env};
use rgit_hash::{HashKind, ObjectId};
use rgit_refs::LogAllRefUpdates;
use rgit_refs::reflog::lookup::Target;
use rgit_refs::reflog::{Committer, FilesReflog, Selector, resolve};

const K: HashKind = HashKind::Sha1;

fn oid(fx: &Fixture, rev: &str) -> Option<ObjectId> {
    let out = fx.git_output(["rev-parse", "-q", "--verify", rev]);
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).unwrap();
    Some(ObjectId::from_hex(K, s.trim().as_bytes()).unwrap())
}

/// Every file and directory below `dir`, with file contents.
fn tree(dir: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Option<Vec<u8>>>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
            if e.file_type().unwrap().is_dir() {
                out.insert(format!("{rel}/"), None);
                walk(base, &p, out);
            } else {
                out.insert(rel, Some(fs::read(&p).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn render(t: &BTreeMap<String, Option<Vec<u8>>>) -> String {
    let mut s = String::new();
    for (k, v) in t {
        s.push_str(k);
        if let Some(v) = v {
            s.push_str(" = ");
            s.push_str(&String::from_utf8_lossy(v));
        }
        s.push('\n');
    }
    s
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap().flatten() {
        let dst = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dst);
        } else {
            fs::copy(e.path(), dst).unwrap();
        }
    }
}

fn committer(fx: &Fixture) -> Committer {
    Committer::new(
        env::COMMITTER_NAME.as_bytes(),
        env::COMMITTER_EMAIL.as_bytes(),
        fx.time() as u64,
        0,
    )
}

fn setup() -> Option<Fixture> {
    let fx = Fixture::new()
        .git(["init", "-q"])
        .git(["commit", "-q", "--allow-empty", "-m", "one"])
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "two"]);
    (!fx.is_skipped()).then_some(fx)
}

#[test]
fn appended_lines_and_created_files_match_git() {
    let Some(mut fx) = setup() else { return };
    let git_dir = fx.work_dir().join(".git");
    let shadow = tempfile::tempdir().unwrap();
    copy_dir(&git_dir.join("logs"), &shadow.path().join("logs"));
    let ours = FilesReflog::new(shadow.path(), shadow.path(), K);

    let one = oid(&fx, "HEAD~1").unwrap();
    let two = oid(&fx, "HEAD").unwrap();
    // (config, ref, new, message)
    let steps: &[(&str, &str, ObjectId, Option<&str>)] = &[
        (
            "true",
            "refs/heads/main",
            one,
            Some("  multi\n line\t tab  \n\n"),
        ),
        ("true", "refs/heads/main", two, None),
        (
            "true",
            "refs/heads/main",
            one,
            Some("a\rb\x0bc\x0cd\x01e  \u{e4} x"),
        ),
        (
            "true",
            "refs/heads/topic/deep",
            two,
            Some("branch: Created from main"),
        ),
        ("true", "refs/tags/v1", two, Some("not logged")),
        ("true", "ORIG_HEAD", two, Some("not logged either")),
        ("true", "refs/notes/commits", two, Some("notes")),
        (
            "true",
            "refs/remotes/origin/main",
            two,
            Some("fetch: fast-forward"),
        ),
        ("always", "refs/tags/v2", one, Some("tag")),
        ("always", "BAR_HEAD", one, Some("root ref")),
        ("always", "refs/heads/main", two, Some(" ")),
        ("false", "refs/heads/nolog", two, Some("no log")),
        (
            "false",
            "refs/heads/main",
            one,
            Some("existing log is appended"),
        ),
    ];
    for &(cfg, name, new, msg) in steps {
        fx = fx.tick();
        let old = oid(&fx, name).unwrap_or(K.null());
        let mut args = vec![
            "-c".to_string(),
            format!("core.logAllRefUpdates={cfg}"),
            "update-ref".into(),
        ];
        if let Some(m) = msg {
            args.push("-m".into());
            args.push(m.into());
        }
        args.push(name.into());
        args.push(new.to_hex());
        fx.git_stdout(&args);

        let mode = LogAllRefUpdates::parse(cfg).unwrap();
        let c = committer(&fx);
        let m = msg.unwrap_or("").as_bytes();
        ours.log_update(name, old, new, &c, m, mode.should_autocreate(name))
            .unwrap();
        if name == "refs/heads/main" {
            // HEAD points at main: the transaction logs HEAD as well.
            ours.log_update("HEAD", old, new, &c, m, mode.should_autocreate("HEAD"))
                .unwrap();
        }
        assert_same_text(
            &format!("logs after {name} ({cfg})"),
            &render(&tree(&git_dir.join("logs"))),
            &render(&tree(&shadow.path().join("logs"))),
        );
    }

    // Deleting a reference removes its log and the directories it leaves empty.
    for name in ["refs/heads/topic/deep", "refs/tags/v2", "BAR_HEAD"] {
        fx.git_stdout(["update-ref", "-d", name]);
        ours.delete(name).unwrap();
        assert_same_text(
            &format!("logs after deleting {name}"),
            &render(&tree(&git_dir.join("logs"))),
            &render(&tree(&shadow.path().join("logs"))),
        );
    }
}

#[test]
fn directories_in_the_way() {
    let Some(fx) = setup() else { return };
    let git_dir = fx.work_dir().join(".git");
    // An empty directory tree where the log goes is replaced.
    fs::create_dir_all(git_dir.join("logs/refs/heads/yy/sub/deeper")).unwrap();
    let shadow = tempfile::tempdir().unwrap();
    copy_dir(&git_dir.join("logs"), &shadow.path().join("logs"));
    let ours = FilesReflog::new(shadow.path(), shadow.path(), K);
    fx.git_stdout(["branch", "yy", "main"]);
    let head = oid(&fx, "HEAD").unwrap();
    ours.log_update(
        "refs/heads/yy",
        K.null(),
        head,
        &committer(&fx),
        b"branch: Created from main",
        true,
    )
    .unwrap();
    assert_same_text(
        "logs",
        &render(&tree(&git_dir.join("logs"))),
        &render(&tree(&shadow.path().join("logs"))),
    );

    // A directory with files in it is an error.
    fs::create_dir_all(git_dir.join("logs/refs/heads/zz/sub")).unwrap();
    fs::write(git_dir.join("logs/refs/heads/zz/sub/f"), "j\n").unwrap();
    let out = fx.git_output(["branch", "zz", "main"]);
    assert!(!out.status.success());
    let ours = FilesReflog::new(&git_dir, &git_dir, K);
    let err = ours
        .log_update("refs/heads/zz", K.null(), head, &committer(&fx), b"x", true)
        .unwrap_err()
        .to_string();
    let err = err.replace(&format!("{}/", fx.work_dir().display()), "");
    assert_same_text(
        "error",
        &String::from_utf8_lossy(&out.stderr),
        &format!("fatal: cannot update the ref 'refs/heads/zz': {err}\n"),
    );

    // A stale log file where a directory is needed.
    fs::write(git_dir.join("logs/refs/heads/one"), "junk\n").unwrap();
    let out = fx.git_output(["branch", "one/two", "main"]);
    assert!(!out.status.success());
    let err = ours
        .log_update(
            "refs/heads/one/two",
            K.null(),
            head,
            &committer(&fx),
            b"x",
            true,
        )
        .unwrap_err()
        .to_string();
    let err = err.replace(&format!("{}/", fx.work_dir().display()), "");
    assert_same_text(
        "error",
        &String::from_utf8_lossy(&out.stderr),
        &format!("fatal: cannot update the ref 'refs/heads/one/two': {err}\n"),
    );
}

#[test]
fn rename_matches_git() {
    let Some(fx) = setup() else { return };
    let git_dir = fx.work_dir().join(".git");
    fx.git_stdout(["branch", "q/r", "main"]);
    fx.git_stdout(["branch", "a", "main"]);
    let shadow = tempfile::tempdir().unwrap();
    copy_dir(&git_dir.join("logs"), &shadow.path().join("logs"));
    let ours = FilesReflog::new(shadow.path(), shadow.path(), K);
    for (from, to) in [("q/r", "q"), ("a", "a/b")] {
        fx.git_stdout(["branch", "-m", from, to]);
        let from = format!("refs/heads/{from}");
        let to = format!("refs/heads/{to}");
        ours.rename(&from, &to).unwrap();
        // `branch -m` then logs the rename itself.
        let head = oid(&fx, "main").unwrap();
        let msg = format!("Branch: renamed {from} to {to}");
        ours.log_update(&to, head, head, &committer(&fx), msg.as_bytes(), true)
            .unwrap();
        assert_same_text(
            &format!("logs after {from} -> {to}"),
            &render(&tree(&git_dir.join("logs/refs"))),
            &render(&tree(&shadow.path().join("logs/refs"))),
        );
    }
}

#[test]
fn list_matches_git() {
    let Some(fx) = setup() else { return };
    let git_dir = fx.work_dir().join(".git");
    fx.git_stdout([
        "-c",
        "core.logAllRefUpdates=always",
        "update-ref",
        "BAR_HEAD",
        "HEAD",
    ]);
    for f in [
        "refs/heads/a.lock",
        "refs/heads/.hidden",
        "lowercase",
        "foo/bar",
        "refs/heads/b..c",
        "refs/heads/sp ace",
        "FOO-BAR",
        "refs/tags/ok",
        "refs/heads/x@{y}",
        "refs/heads/empty",
    ] {
        let p = git_dir.join("logs").join(f);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "").unwrap();
    }
    fs::create_dir_all(git_dir.join("logs/refs/heads/emptydir")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("main", git_dir.join("logs/refs/heads/symlinked")).unwrap();
    let ours = FilesReflog::new(&git_dir, &git_dir, K);
    let mut text = ours.list().unwrap().join("\n");
    text.push('\n');
    assert_same_text(
        "reflog list",
        &String::from_utf8_lossy(&fx.git_stdout(["reflog", "list"])),
        &text,
    );
}

#[test]
fn list_with_worktrees_matches_git() {
    let Some(fx) = setup() else { return };
    let git_dir = fx.work_dir().join(".git");
    fx.git_stdout(["worktree", "add", "-q", "wt"]);
    let always = ["-c", "core.logAllRefUpdates=always"];
    let run = |dir: Option<&str>, args: &[&str]| {
        let mut v: Vec<&str> = Vec::new();
        if let Some(d) = dir {
            v.extend(["-C", d]);
        }
        v.extend(always);
        v.extend(args);
        fx.git_stdout(v);
    };
    run(None, &["update-ref", "refs/worktree/main", "HEAD"]);
    run(None, &["update-ref", "refs/worktree/per-worktree", "HEAD"]);
    run(
        Some("wt"),
        &["update-ref", "refs/worktree/per-worktree", "HEAD"],
    );
    run(
        Some("wt"),
        &["update-ref", "refs/worktree/worktree", "HEAD"],
    );
    let check = |ours: FilesReflog, dir: &str| {
        let mut text = ours.list().unwrap().join("\n");
        text.push('\n');
        assert_same_text(
            &format!("reflog list in {dir}"),
            &String::from_utf8_lossy(&fx.git_stdout(["-C", dir, "reflog", "list"])),
            &text,
        );
    };
    check(FilesReflog::new(&git_dir, &git_dir, K), ".");
    check(
        FilesReflog::new(git_dir.join("worktrees/wt"), &git_dir, K),
        "wt",
    );
    let main_wt = FilesReflog::new(git_dir.join("worktrees/wt"), &git_dir, K);
    assert_eq!(
        main_wt.path("main-worktree/HEAD"),
        git_dir.join("logs/HEAD")
    );
    assert_eq!(
        FilesReflog::new(&git_dir, &git_dir, K).path("worktrees/wt/HEAD"),
        git_dir.join("worktrees/wt/logs/HEAD")
    );
}

/// What `git rev-parse --verify <ref>@{<spec>}` prints, computed by us.
fn our_rev_parse(logs: &FilesReflog, fx: &Fixture, short: &str, spec: &str) -> (String, String) {
    let refname = format!("refs/heads/{short}");
    let entries = logs.read(&refname).unwrap().unwrap_or_default();
    let ctx = DateContext::utc(fx.time());
    let sel = Selector::parse(spec, &ctx).unwrap();
    let target = Target {
        refname: &refname,
        display: short,
        current: oid(fx, &refname),
    };
    match resolve(&entries, sel, target) {
        Ok(r) => {
            let err: String = r
                .warnings
                .iter()
                .map(|w| format!("warning: {w}\n"))
                .collect();
            (format!("{}\n", r.oid), err)
        }
        Err(e) => (String::new(), format!("fatal: {e}\n")),
    }
}

#[test]
fn lookups_match_git() {
    let Some(mut fx) = setup() else { return };
    let mut ids = Vec::new();
    for i in 0..8 {
        fx = fx
            .tick()
            .git(["commit", "-q", "--allow-empty", "-m", &format!("c{i}")]);
        ids.push(oid(&fx, "HEAD").unwrap());
    }
    let git_dir = fx.work_dir().join(".git");
    let logs = FilesReflog::new(&git_dir, &git_dir, K);
    let z = K.null();
    let line = |o: ObjectId, n: ObjectId, t: u64, tz: &str, m: &str| {
        format!("{o} {n} C O Mitter <c@x> {t} {tz}\t{m}\n")
    };
    let specs = [
        "0",
        "1",
        "2",
        "3",
        "4",
        "5",
        "1112900500",
        "1112901000",
        "1112901500",
        "1112902500",
        "1112903500",
        "1112999999",
        "1112902000",
        "1112903000",
        "1112903001",
        "1112901001",
    ];
    let scenarios: Vec<(String, usize)> = vec![
        // consistent history created from nothing
        (
            line(z, ids[0], 1112901000, "+0000", "m0")
                + &line(ids[0], ids[1], 1112902000, "+0100", "m1")
                + &line(ids[1], ids[2], 1112903000, "-0700", "m2"),
            2,
        ),
        // gaps everywhere, ref elsewhere
        (
            line(ids[0], ids[1], 1112901000, "+0000", "m0")
                + &line(ids[2], ids[3], 1112902000, "+0000", "m1")
                + &line(ids[4], ids[5], 1112903000, "+0000", "m2"),
            7,
        ),
        // same, ref matches the newest entry
        (
            line(ids[0], ids[1], 1112901000, "+0000", "m0")
                + &line(ids[2], ids[3], 1112902000, "+0000", "m1")
                + &line(ids[4], ids[5], 1112903000, "+0000", "m2"),
            5,
        ),
        // malformed lines are skipped, unterminated last line too
        (
            "garbage\n".to_string()
                + &line(ids[0], ids[1], 1112901000, "+0100", "m1")
                + "\n"
                + &format!("{} {} bad\n", ids[1], ids[2])
                + line(ids[1], ids[2], 1112903000, "-0700", "m3").trim_end(),
            2,
        ),
        // empty log
        (String::new(), 3),
    ];
    let mut scenarios = scenarios;
    // the reference is deleted and created again in the middle
    for (old, cur) in [(z, 4), (z, 7), (ids[6], 4), (ids[6], 7)] {
        scenarios.push((
            line(ids[0], ids[1], 1112901000, "+0000", "m0")
                + &line(ids[1], ids[2], 1112902000, "+0000", "m1")
                + &line(old, ids[3], 1112903000, "+0000", "m2")
                + &line(ids[3], ids[4], 1112904000, "+0000", "m3"),
            cur,
        ));
    }
    let specs: Vec<&str> = specs
        .into_iter()
        .chain(["1112904000", "1112904500", "1112903999"])
        .collect();
    for (k, (content, cur)) in scenarios.iter().enumerate() {
        let name = format!("x{k}");
        fx.git_stdout([
            "update-ref",
            &format!("refs/heads/{name}"),
            &ids[*cur].to_hex(),
        ]);
        fs::write(git_dir.join("logs/refs/heads").join(&name), content).unwrap();
        for &spec in &specs {
            let arg = format!("{name}@{{{spec}}}");
            let out = fx.git_output(["rev-parse", "--verify", &arg]);
            let (o, e) = our_rev_parse(&logs, &fx, &name, spec);
            let label = format!("scenario {k}: {arg}");
            assert_same_text(&label, &String::from_utf8_lossy(&out.stdout), &o);
            assert_same_text(&label, &String::from_utf8_lossy(&out.stderr), &e);
        }
    }
}

#[test]
fn prior_checkouts_match_git() {
    let Some(fx) = setup() else { return };
    for args in [
        &["checkout", "-q", "-b", "f1"][..],
        &["checkout", "-q", "main"],
        &["checkout", "-q", "--detach"],
        &["checkout", "-q", "f1"],
        &["switch", "-q", "main"],
    ] {
        fx.git_stdout(args);
    }
    let git_dir: PathBuf = fx.work_dir().join(".git");
    let logs = FilesReflog::new(&git_dir, &git_dir, K);
    let head = logs.read("HEAD").unwrap().unwrap();
    for n in 1..=6 {
        let spec = format!("@{{-{n}}}");
        let out = fx.git_output(["rev-parse", "--verify", "-q", &spec]);
        let ours = rgit_refs::reflog::nth_prior_checkout(&head, n).map(|b| {
            let s = std::str::from_utf8(b).unwrap();
            format!("{}\n", oid(&fx, s).unwrap())
        });
        assert_same_text(
            &spec,
            &String::from_utf8_lossy(&out.stdout),
            ours.as_deref().unwrap_or(""),
        );
    }
}

mod common;

/// Reference transactions with `ReflogWriter` as hooks write the same logs
/// as `git update-ref`: `HEAD` follows its branch, `core.logAllRefUpdates`
/// decides which logs are created, deleted references lose their log.
#[test]
fn transactions_log_like_git() {
    use common::{Cmd, Repo, Step, run_git, stdin_text, to_update};
    use rgit_refs::files::{FilesOptions, FilesStore};
    use rgit_refs::reflog::ReflogWriter;
    use rgit_refs::{RefUpdate, Transaction};

    let Some(repo) = Repo::new() else { return };
    let c = repo.c.clone();
    let step = |cmd: Cmd| Step {
        cmd,
        no_deref: false,
    };
    let r = |s: &str| s.to_string();
    // (core.logAllRefUpdates, --create-reflog, message, steps)
    let cases: Vec<(&str, bool, &str, Vec<Step>)> = vec![
        (
            "true",
            false,
            "via HEAD",
            vec![step(Cmd::Update(r("HEAD"), c[0], None))],
        ),
        (
            "true",
            false,
            "branch",
            vec![step(Cmd::Update(r("refs/heads/main"), c[1], None))],
        ),
        (
            "true",
            false,
            "  odd\n message\t ",
            vec![step(Cmd::Create(r("refs/heads/new/x"), c[1]))],
        ),
        (
            "true",
            false,
            "tag",
            vec![step(Cmd::Create(r("refs/tags/t1"), c[1]))],
        ),
        (
            "true",
            true,
            "forced",
            vec![step(Cmd::Create(r("refs/tags/t2"), c[1]))],
        ),
        (
            "true",
            false,
            "orig",
            vec![step(Cmd::Update(r("ORIG_HEAD"), c[1], None))],
        ),
        (
            "always",
            false,
            "orig",
            vec![step(Cmd::Update(r("ORIG_HEAD"), c[0], None))],
        ),
        (
            "always",
            false,
            "tag",
            vec![step(Cmd::Create(r("refs/tags/t3"), c[0]))],
        ),
        (
            "false",
            false,
            "no log",
            vec![step(Cmd::Create(r("refs/heads/quiet"), c[0]))],
        ),
        (
            "false",
            false,
            "existing",
            vec![step(Cmd::Update(r("refs/heads/main"), c[2], None))],
        ),
        (
            "true",
            false,
            "detach",
            vec![Step {
                cmd: Cmd::Update(r("HEAD"), c[1], None),
                no_deref: true,
            }],
        ),
        (
            "true",
            false,
            "delete",
            vec![step(Cmd::Delete(r("refs/heads/new/x"), None))],
        ),
        (
            "true",
            false,
            "several",
            vec![
                step(Cmd::Create(r("refs/remotes/o/a"), c[0])),
                step(Cmd::Create(r("refs/notes/n"), c[0])),
                step(Cmd::Delete(r("refs/tags/t2"), None)),
            ],
        ),
    ];
    let (a, b) = repo.twins("reflog");
    for (cfg, create, msg, steps) in &cases {
        let text = stdin_text(steps);
        let cfg_arg = format!("core.logAllRefUpdates={cfg}");
        let mut args = vec!["-c", cfg_arg.as_str(), "update-ref", "-m", msg];
        if *create {
            args.push("--create-reflog");
        }
        args.push("--stdin");
        let git = run_git(&repo, &a, &args, text.as_bytes());
        assert!(git.ok, "git failed: {}", git.stderr);

        let opts = FilesOptions {
            log_all_ref_updates: LogAllRefUpdates::parse(cfg).unwrap(),
            ..FilesOptions::default()
        };
        let store = FilesStore::with_options(&b, K, opts);
        let mut tx = Transaction::new(&store);
        for s in steps {
            let u: RefUpdate = to_update(s).message(*msg).create_reflog(*create);
            tx.push(u).unwrap();
        }
        let mut hooks = ReflogWriter::new(
            FilesReflog::new(&b, &b, K),
            Committer::new(b"C", b"c@example.com", 1_700_000_000, 0),
        );
        tx.commit(&mut hooks).unwrap();
        assert_same_text(
            &format!("logs after ({cfg}, -m {msg:?}):\n{text}"),
            &render(&tree(&a.join("logs"))),
            &render(&tree(&b.join("logs"))),
        );
    }
}
