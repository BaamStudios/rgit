// SPDX-License-Identifier: MIT
//! Commands in reftable repositories, against the reference git: the same
//! commands run with git in one directory and with rgit in another. Exit
//! code and output have to agree, and afterwards both repositories have
//! to hold the same tables (records, update indices, block layout; the
//! random part of the table names aside), the same configuration and the
//! same stand-in files. The reference git also reads the repository rgit
//! wrote (`for-each-ref`, `fsck`).
//!
//! The comparison needs the git version rgit follows here (2.56): what
//! git writes for an operation has changed between releases.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use rgit_difftest::Fixture;
use rgit_refs::reftable::{RefValue, Table};

struct Pair {
    fx: Fixture,
    /// Parent directories of the two sides.
    git: PathBuf,
    rgit: PathBuf,
    /// Compact after every write (`GIT_TEST_REFTABLE_AUTOCOMPACTION`).
    auto_compact: bool,
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Pair {
    /// `None` (test skipped) without a reference git of the right version.
    fn new(auto_compact: bool) -> Option<Pair> {
        let fx = Fixture::new();
        if fx.is_skipped() {
            return None;
        }
        let version = String::from_utf8_lossy(&fx.git_output(["version"]).stdout).into_owned();
        if !version.contains(" 2.56.") {
            eprintln!(
                "skipped: the comparison needs git 2.56, found {}",
                version.trim()
            );
            return None;
        }
        let base = fx.home_dir().join("sides");
        // Names of the same length: paths in reflog messages (`clone`)
        // then give blocks of the same size.
        let (git, rgit) = (base.join("gitx"), base.join("rgit"));
        std::fs::create_dir_all(&git).unwrap();
        std::fs::create_dir_all(&rgit).unwrap();
        Some(Pair {
            fx,
            git,
            rgit,
            auto_compact,
        })
    }

    fn exec(
        &self,
        bin: &Path,
        root: &Path,
        dir: &str,
        args: &[&str],
        stdin: &str,
        env: &[(&str, &str)],
    ) -> Run {
        let mut cmd = Command::new(bin);
        cmd.env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.fx.home_dir())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", root)
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env(
                "GIT_TEST_REFTABLE_AUTOCOMPACTION",
                if self.auto_compact { "true" } else { "false" },
            )
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0100")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_COMMITTER_DATE", "1700000000 +0100")
            // Commits would otherwise compact the stack depending on the
            // sizes of deflated log blocks.
            .args(["-c", "maintenance.auto=false"])
            .args(args)
            .current_dir(root.join(dir))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("spawn");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        let clean = |b: &[u8]| {
            let text = String::from_utf8_lossy(b).replace(&root.display().to_string(), "ROOT");
            // Table names end in eight random hex digits.
            text.lines()
                .map(|line| match line.strip_suffix(".ref") {
                    Some(stem) if stem.len() > 9 && stem.contains("0x") => {
                        format!("{}-RANDOM.ref\n", &stem[..stem.len() - 9])
                    }
                    _ => format!("{line}\n"),
                })
                .collect::<String>()
        };
        Run {
            code: out.status.code(),
            stdout: clean(&out.stdout),
            stderr: clean(&out.stderr),
        }
    }

    /// Runs `args` in `dir` on both sides and expects the same result.
    fn both_in(&self, dir: &str, args: &[&str], stdin: &str, env: &[(&str, &str)]) -> Run {
        let g = self.exec(self.fx.reference_git(), &self.git, dir, args, stdin, env);
        let r = self.exec(
            Path::new(env!("CARGO_BIN_EXE_rgit")),
            &self.rgit,
            dir,
            args,
            stdin,
            env,
        );
        let show = |x: &Run| {
            format!(
                "exit {:?}\n--- stdout\n{}--- stderr\n{}",
                x.code, x.stdout, x.stderr
            )
        };
        rgit_difftest::assert_same_text(&format!("{args:?} in {dir:?}"), &show(&g), &show(&r));
        g
    }

    fn both(&self, dir: &str, args: &[&str]) -> Run {
        self.both_in(dir, args, "", &[])
    }

    /// Runs the commands in `dir`; each must succeed on both sides.
    fn ok(&self, dir: &str, commands: &[&[&str]]) {
        for args in commands {
            let run = self.both(dir, args);
            assert_eq!(run.code, Some(0), "{args:?}: {}", run.stderr);
        }
    }

    /// The reference git, run on one side.
    fn inspect(&self, root: &Path, dir: &str, args: &[&str]) -> String {
        let run = self.exec(self.fx.reference_git(), root, dir, args, "", &[]);
        format!("{}{}", run.stdout, run.stderr)
    }

    /// Compares the repositories in `dir` of both sides.
    fn same_state(&self, label: &str, dir: &str) {
        let state = |root: &Path| {
            let repo = root.join(dir);
            let git_dir = if repo.join(".git").is_dir() {
                repo.join(".git")
            } else {
                repo.clone()
            };
            let mut out = String::new();
            for file in ["config", "HEAD", "refs/heads", "packed-refs"] {
                if let Ok(text) = std::fs::read_to_string(git_dir.join(file)) {
                    out.push_str(&format!("== {file}\n{text}"));
                }
            }
            out.push_str(&dump_stack(&git_dir, "main"));
            let mut ids: Vec<String> = std::fs::read_dir(git_dir.join("worktrees"))
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            ids.sort();
            for id in ids {
                out.push_str(&dump_stack(&git_dir.join("worktrees").join(&id), &id));
            }
            let mut files = Vec::new();
            list_files(&git_dir, &git_dir, &mut files);
            files.sort();
            out.push_str(&format!("== files\n{}\n", files.join("\n")));
            out.push_str("== for-each-ref\n");
            out.push_str(&self.inspect(root, dir, &["for-each-ref", "--include-root-refs"]));
            out.push_str("== fsck\n");
            out.push_str(&self.inspect(root, dir, &["fsck", "--no-dangling"]));
            out
        };
        let clean = |root: &Path| state(root).replace(&root.display().to_string(), "ROOT");
        rgit_difftest::assert_same_text(label, &clean(&self.git), &clean(&self.rgit));
    }
}

/// The reference files of a git directory, table names without their
/// random part.
fn list_files(root: &Path, dir: &Path, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if [
            "objects",
            "hooks",
            "info",
            "index",
            "description",
            "COMMIT_EDITMSG",
        ]
        .iter()
        .any(|skip| rel == *skip || rel.ends_with("/index"))
        {
            continue;
        }
        if path.is_dir() {
            out.push(format!("{rel}/"));
            list_files(root, &path, out);
        } else if rel.ends_with(".ref") && rel.contains("reftable/0x") {
            out.push(format!(
                "{}-RANDOM.ref",
                &rel[..rel.len() - ".ref".len() - 9]
            ));
        } else if rel.ends_with(".ref.lock") && rel.contains("reftable/0x") {
            out.push(format!(
                "{}-RANDOM.ref.lock",
                &rel[..rel.len() - ".ref.lock".len() - 9]
            ));
        } else {
            out.push(rel);
        }
    }
}

fn dump_stack(git_dir: &Path, label: &str) -> String {
    let dir = git_dir.join("reftable");
    let Ok(list) = std::fs::read_to_string(dir.join("tables.list")) else {
        return String::new();
    };
    let mut out = format!("== stack {label}\n");
    for name in list.lines() {
        let data = std::fs::read(dir.join(name)).unwrap();
        let table = Arc::new(Table::parse(name, data).expect("table parses"));
        out.push_str(&format!(
            "table {}..{}\n",
            table.min_update_index(),
            table.max_update_index()
        ));
        for b in table.blocks().unwrap() {
            out.push_str(&format!(
                "  block {} len={} restarts={}\n",
                b.typ as char, b.len, b.restarts
            ));
        }
        let mut refs = table.refs();
        while let Some(r) = refs.next_record().unwrap() {
            let value = match &r.value {
                RefValue::Deletion => "deleted".to_owned(),
                RefValue::Direct(id) => id.to_hex(),
                RefValue::Peeled(id, p) => format!("{id} ^{p}"),
                RefValue::Symbolic(t) => format!("-> {}", String::from_utf8_lossy(t)),
            };
            out.push_str(&format!(
                "  ref {} @{} {value}\n",
                String::from_utf8_lossy(&r.name),
                r.update_index
            ));
        }
        let mut logs = table.logs();
        while let Some(l) = logs.next_record().unwrap() {
            let name = String::from_utf8_lossy(&l.refname).into_owned();
            match &l.update {
                None => out.push_str(&format!("  log {name} @{} deleted\n", l.update_index)),
                Some(u) => out.push_str(&format!(
                    "  log {name} @{} {} {} {} <{}> {} {} {:?}\n",
                    l.update_index,
                    u.old,
                    u.new,
                    String::from_utf8_lossy(&u.name),
                    String::from_utf8_lossy(&u.email),
                    u.time,
                    u.tz_offset,
                    String::from_utf8_lossy(&u.message)
                )),
            }
        }
    }
    out
}

const INIT: &[&str] = &["init", "-q", "-b", "main", "--ref-format=reftable", "repo"];
const COMMIT_ONE: &[&str] = &["commit", "-q", "--allow-empty", "-m", "one"];
const COMMIT_TWO: &[&str] = &["commit", "-q", "--allow-empty", "-m", "two"];

#[test]
fn init_and_formats() {
    let Some(p) = Pair::new(false) else { return };
    p.ok("", &[INIT]);
    p.same_state("init --ref-format=reftable", "repo");
    p.ok("repo", &[&["rev-parse", "--show-ref-format"]]);
    // Reinitialising with the same format works, with another one it
    // fails. (A reinit without `--ref-format` is not compared: git 2.56
    // then drops `extensions.refStorage`, leaving the references behind;
    // rgit keeps the format.)
    p.both("", &["init", "-q", "--ref-format=reftable", "repo"]);
    p.both("", &["init", "--ref-format=files", "repo"]);
    p.same_state("after reinit", "repo");

    p.both_in(
        "",
        &["init", "-q", "-b", "main", "env"],
        "",
        &[("GIT_DEFAULT_REF_FORMAT", "reftable")],
    );
    p.same_state("GIT_DEFAULT_REF_FORMAT", "env");
    p.both(
        "",
        &[
            "-c",
            "init.defaultRefFormat=reftable",
            "init",
            "-q",
            "-b",
            "main",
            "--bare",
            "bare.git",
        ],
    );
    p.same_state("init.defaultRefFormat, bare", "bare.git");
    p.both(
        "",
        &["init", "-q", "-b", "main", "--ref-format=files", "files"],
    );
    p.ok("files", &[&["rev-parse", "--show-ref-format"]]);
    p.both("", &["init", "--ref-format=reftable", "files"]);
}

#[test]
fn reference_commands() {
    let Some(p) = Pair::new(false) else { return };
    p.ok("", &[INIT]);
    p.ok(
        "repo",
        &[
            COMMIT_ONE,
            COMMIT_TWO,
            &["branch", "b1"],
            &["tag", "-a", "-m", "x", "atag"],
            &["tag", "ltag"],
            &["update-ref", "refs/heads/b2", "HEAD"],
            &["update-ref", "-m", "my msg", "refs/heads/b2", "HEAD~1"],
            // An unchanged value writes nothing.
            &["update-ref", "-m", "again", "refs/heads/b2", "HEAD~1"],
            &["update-ref", "--create-reflog", "refs/foo/x", "HEAD"],
            &["symbolic-ref", "refs/heads/sym", "refs/heads/b2"],
            &["update-ref", "-m", "viasym", "refs/heads/sym", "HEAD"],
            &["symbolic-ref", "-m", "switch", "HEAD", "refs/heads/b2"],
            &["symbolic-ref", "HEAD", "refs/heads/main"],
            &["checkout", "-q", "--detach"],
            &["checkout", "-q", "main"],
            &["update-ref", "-d", "refs/foo/x"],
            &["branch", "-D", "b1"],
        ],
    );
    p.same_state("after the updates", "repo");
    for args in [
        &["update-ref", "refs/heads/b2/sub", "HEAD"][..],
        &["update-ref", "refs/heads", "HEAD"],
        &["update-ref", "refs/heads/b2", "HEAD", "HEAD~1"],
        &["update-ref", "-d", "refs/heads/nope", "HEAD"],
        &["symbolic-ref", "refs/heads", "refs/heads/main"],
        &["show-ref"],
        &["show-ref", "-d"],
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname) %(symref) %(*objectname)",
        ],
        &["rev-parse", "HEAD", "b2", "atag^{}", "sym", "HEAD@{1}"],
        &["reflog", "show", "refs/heads/b2"],
        &["reflog", "show", "HEAD"],
        &["reflog", "exists", "refs/heads/b2"],
        &["reflog", "exists", "refs/tags/ltag"],
        &["reflog", "list"],
        &["refs", "verify", "--verbose"],
    ] {
        p.both("repo", args);
    }
    p.both_in(
        "repo",
        &["update-ref", "--stdin"],
        "create refs/heads/n1 HEAD\ncreate refs/heads/n2 HEAD~1\ndelete refs/heads/sym\nverify refs/heads/b2 HEAD\n",
        &[],
    );
    p.ok(
        "repo",
        &[
            &["reflog", "expire", "--expire=all", "refs/heads/b2"],
            &["reflog", "delete", "HEAD@{1}"],
            &["reflog", "show", "refs/heads/b2"],
            &["reflog", "exists", "refs/heads/b2"],
        ],
    );
    p.same_state("after editing reflogs", "repo");
    // A long reflog message is cut to half a block.
    let long = "word ".repeat(1000);
    p.ok(
        "repo",
        &[&["update-ref", "-m", &long, "refs/heads/long", "HEAD"]],
    );
    p.same_state("long message", "repo");
    p.ok("repo", &[&["pack-refs"]]);
    p.same_state("after pack-refs", "repo");
    p.both("repo", &["-c", "reftable.blockSize=16777216", "pack-refs"]);
    p.both(
        "repo",
        &["-c", "reftable.restartInterval=65536", "pack-refs"],
    );
    p.both(
        "repo",
        &[
            "-c",
            "reftable.blockSize=50",
            "update-ref",
            "refs/heads/tiny",
            "HEAD",
        ],
    );
    p.same_state("refused writes", "repo");
}

#[test]
fn write_options_from_the_configuration() {
    let Some(p) = Pair::new(false) else { return };
    p.ok("", &[INIT]);
    p.ok(
        "repo",
        &[&["config", "core.logAllRefUpdates", "false"], COMMIT_ONE],
    );
    let updates: String = (1..=60)
        .map(|i| format!("update refs/heads/branch-{i} HEAD\n"))
        .collect();
    p.both_in("repo", &["update-ref", "--stdin"], &updates, &[]);
    p.ok(
        "repo",
        &[&[
            "-c",
            "reftable.blockSize=200",
            "-c",
            "reftable.restartInterval=4",
            "pack-refs",
        ]],
    );
    p.same_state("small blocks", "repo");
    p.ok("repo", &[&["update-ref", "refs/heads/more", "HEAD"]]);
    p.ok(
        "repo",
        &[&[
            "-c",
            "reftable.blockSize=150",
            "-c",
            "reftable.indexObjects=false",
            "refs",
            "optimize",
        ]],
    );
    p.same_state("without object index", "repo");
}

#[test]
fn rename_and_copy_branches() {
    let Some(p) = Pair::new(false) else { return };
    p.ok("", &[INIT]);
    p.ok(
        "repo",
        &[
            COMMIT_ONE,
            COMMIT_TWO,
            &["branch", "-m", "main", "trunk"],
            &["branch", "src", "HEAD~1"],
            &["branch", "-c", "src", "dst"],
            &["branch", "other"],
            &["branch", "-M", "src", "other"],
            &["branch", "-m", "other", "other/sub"],
            &["branch", "-C", "trunk", "dst"],
        ],
    );
    p.same_state("after renames and copies", "repo");
    for args in [
        &["branch", "-m", "dst", "trunk/x"][..],
        &["branch", "-c", "dst", "trunk/x"],
        &["symbolic-ref", "HEAD"],
        &["reflog", "show", "trunk"],
        &["reflog", "show", "dst"],
        &["reflog", "show", "other/sub"],
        &["reflog", "show", "HEAD"],
        &["rev-parse", "trunk@{1}"],
        &["refs", "rename", "refs/heads/dst", "refs/heads/dst2"],
    ] {
        p.both("repo", args);
    }
    p.same_state("at the end", "repo");
}

#[test]
fn worktrees_have_their_own_stack() {
    let Some(p) = Pair::new(false) else { return };
    p.ok("", &[INIT]);
    p.ok(
        "repo",
        &[
            COMMIT_ONE,
            &["branch", "b1"],
            &["worktree", "add", "-q", "../wt1", "b1"],
            &["worktree", "add", "-q", "../wt2"],
            &["worktree", "add", "-q", "--detach", "../wt3"],
        ],
    );
    p.same_state("after worktree add", "repo");
    p.ok(
        "wt1",
        &[
            &["update-ref", "refs/bisect/bad", "HEAD"],
            &["update-ref", "refs/heads/shared", "HEAD"],
            &["update-ref", "ORIG_HEAD", "HEAD"],
            COMMIT_TWO,
            &["for-each-ref", "--include-root-refs"],
            &["symbolic-ref", "HEAD"],
            &["rev-parse", "main-worktree/HEAD", "worktrees/wt3/HEAD"],
            &["reflog", "show", "HEAD"],
        ],
    );
    p.ok(
        "repo",
        &[
            &["for-each-ref", "--include-root-refs"],
            &[
                "rev-parse",
                "worktrees/wt1/HEAD",
                "worktrees/wt1/refs/bisect/bad",
            ],
            &["worktree", "list", "--porcelain"],
            &["refs", "verify"],
        ],
    );
    p.same_state("after updates from a worktree", "repo");
    // `pack-refs` compacts the stack of the worktree it runs in.
    p.ok("wt1", &[&["pack-refs"]]);
    p.same_state("pack-refs in a worktree", "repo");
    p.ok("repo", &[&["pack-refs"]]);
    p.same_state("pack-refs in the main worktree", "repo");
    p.both("repo", &["refs", "migrate", "--ref-format=files"]);
}

/// `clone` and `fetch --atomic` write their tables like git, with the
/// `reftable.*` settings of the configuration; the initial transaction of
/// a clone logs the remote-tracking branches with an empty message.
/// (Without `--atomic` rgit writes one table per reference, git one for
/// all.)
#[test]
fn clone_and_fetch_write_tables_like_gits() {
    let Some(p) = Pair::new(false) else { return };
    let input: String = (1..=60)
        .map(|i| format!("create refs/heads/branch-{i} HEAD\n"))
        .collect();
    p.ok("", &[INIT]);
    p.ok("repo", &[COMMIT_ONE, &["tag", "-m", "t", "v1"]]);
    let run = p.both_in("repo", &["update-ref", "--stdin"], &input, &[]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let settings: &[&str] = &[
        "-c",
        "reftable.blockSize=512",
        "-c",
        "reftable.restartInterval=3",
    ];
    let clone = [settings, &["clone", "-q", "--no-local", "repo", "copy"]].concat();
    p.ok("", &[&clone]);
    p.same_state("after clone", "copy");
    p.ok("", &[&["init", "-q", "--ref-format=reftable", "empty"]]);
    let fetch = [
        settings,
        &[
            "fetch",
            "-q",
            "--atomic",
            "../repo",
            "refs/heads/*:refs/remotes/o/*",
        ],
    ]
    .concat();
    p.ok("empty", &[&fetch]);
    p.same_state("after fetch", "empty");
}

#[test]
fn migrate_between_formats() {
    let Some(p) = Pair::new(false) else { return };
    p.ok(
        "",
        &[&["init", "-q", "-b", "main", "--ref-format=files", "repo"]],
    );
    p.ok(
        "repo",
        &[
            COMMIT_ONE,
            COMMIT_TWO,
            &["branch", "b1"],
            &["tag", "-a", "-m", "x", "atag"],
            &["tag", "ltag"],
            &["update-ref", "FOO_HEAD", "HEAD"],
            &["symbolic-ref", "refs/heads/sym", "refs/heads/b1"],
            &["symbolic-ref", "DANGLING_HEAD", "refs/heads/nonexistent"],
            &["pack-refs", "--all"],
            &["update-ref", "refs/heads/b1", "HEAD~1"],
        ],
    );
    for root in [&p.git, &p.rgit] {
        std::fs::write(
            root.join("repo/.git/MERGE_HEAD"),
            "0000000000000000000000000000000000000001\n",
        )
        .unwrap();
    }
    p.same_state("files repository", "repo");

    // A dry run leaves the repository alone and reports where the result is.
    let dry = |root: &Path, bin: &Path| {
        let run = p.exec(
            bin,
            root,
            "repo",
            &["refs", "migrate", "--ref-format=reftable", "--dry-run"],
            "",
            &[],
        );
        assert_eq!(run.code, Some(0), "{}", run.stderr);
        let line = run.stdout.trim().to_owned();
        let prefix =
            "Finished dry-run migration of refs, the result can be found at '.git/ref_migration.";
        assert!(line.starts_with(prefix) && line.ends_with('\''), "{line}");
        let dir = root
            .join("repo")
            .join(&line[line.find(".git/").unwrap()..line.len() - 1]);
        let out = dump_stack(&dir, "dry run");
        std::fs::remove_dir_all(dir).unwrap();
        out
    };
    let by_git = dry(&p.git, p.fx.reference_git());
    let by_rgit = dry(&p.rgit, Path::new(env!("CARGO_BIN_EXE_rgit")));
    rgit_difftest::assert_same_text("dry run", &by_git, &by_rgit);
    p.same_state("after the dry run", "repo");

    p.ok("repo", &[&["refs", "migrate", "--ref-format=reftable"]]);
    p.same_state("files -> reftable", "repo");
    p.ok(
        "repo",
        &[
            &["rev-parse", "--show-ref-format"],
            &["reflog", "show", "b1"],
        ],
    );
    p.both("repo", &["refs", "migrate", "--ref-format=reftable"]);
    p.ok("repo", &[&["refs", "migrate", "--ref-format=files"]]);
    p.same_state("reftable -> files", "repo");
    p.ok(
        "repo",
        &[
            &["rev-parse", "--show-ref-format"],
            &["reflog", "show", "b1"],
        ],
    );
    p.ok(
        "repo",
        &[&["refs", "migrate", "--ref-format=reftable", "--no-reflog"]],
    );
    p.same_state("without reflogs", "repo");
    p.both("repo", &["refs", "migrate", "--ref-format=bogus"]);
    p.both("repo", &["refs", "migrate"]);
    p.both("repo", &["refs", "migrate", "--ref-format=files", "extra"]);
}

#[test]
fn verify_and_fsck() {
    let Some(p) = Pair::new(false) else { return };
    p.ok("", &[INIT]);
    p.ok(
        "repo",
        &[
            COMMIT_ONE,
            &["update-ref", "refs/heads/a", "HEAD"],
            &["symbolic-ref", "refs/heads/good", "refs/heads/a"],
            &["refs", "verify"],
            &["symbolic-ref", "refs/heads/symref", "garbage"],
            &["worktree", "add", "-q", "../wt"],
        ],
    );
    for args in [
        &["refs", "verify"][..],
        &["refs", "verify", "--strict"],
        &["fsck", "--no-dangling"],
    ] {
        p.both("repo", args);
    }
    // A table whose name does not follow the pattern.
    for root in [&p.git, &p.rgit] {
        let dir = root.join("repo/.git/reftable");
        let list = std::fs::read_to_string(dir.join("tables.list")).unwrap();
        let first = list.lines().next().unwrap().to_owned();
        std::fs::rename(dir.join(&first), dir.join("broken.ref")).unwrap();
        std::fs::write(dir.join("tables.list"), list.replace(&first, "broken.ref")).unwrap();
    }
    for args in [
        &["refs", "verify"][..],
        &["refs", "verify", "--strict"],
        &["-c", "fsck.badReftableTableName=error", "refs", "verify"],
        &["-c", "fsck.badReftableTableName=ignore", "refs", "verify"],
        &["-c", "fsck.badReferentName=warn", "refs", "verify"],
    ] {
        p.both("repo", args);
    }
    // A stack of a worktree that cannot be read.
    for root in [&p.git, &p.rgit] {
        std::fs::write(
            root.join("repo/.git/worktrees/wt/reftable/tables.list"),
            "garbage\n",
        )
        .unwrap();
    }
    p.both("repo", &["refs", "verify"]);
}

#[test]
fn stack_is_compacted_like_gits() {
    let Some(p) = Pair::new(true) else { return };
    p.ok("", &[INIT]);
    // Without log blocks the table sizes, which steer compaction, do not
    // depend on the deflater.
    p.ok(
        "repo",
        &[&["config", "core.logAllRefUpdates", "false"], COMMIT_ONE],
    );
    let mut seed: u64 = 99;
    for round in 0..30 {
        seed = (seed * 1_103_515_245 + 12_345) % 2_147_483_648;
        let count = match seed % 9 {
            0..=3 => 1,
            4 | 5 => 3,
            6 => 10,
            7 => 40,
            _ => 150,
        };
        let updates: String = (0..count)
            .map(|i| format!("update refs/heads/r{round}-{i} HEAD\n"))
            .collect();
        let run = p.both_in("repo", &["update-ref", "--stdin"], &updates, &[]);
        assert_eq!(run.code, Some(0), "{}", run.stderr);
        if seed % 5 == 0 {
            p.ok(
                "repo",
                &[&["update-ref", "-d", &format!("refs/heads/r{round}-0")]],
            );
        }
        p.same_state(&format!("round {round}"), "repo");
    }
    // Compaction leaves tables alone that another process holds.
    for root in [&p.git, &p.rgit] {
        let dir = root.join("repo/.git/reftable");
        let list = std::fs::read_to_string(dir.join("tables.list")).unwrap();
        for name in list.lines() {
            std::fs::write(dir.join(format!("{name}.lock")), "").unwrap();
        }
    }
    p.ok(
        "repo",
        &[&["branch", "x1"], &["branch", "x2"], &["branch", "x3"]],
    );
    p.same_state("with locked tables", "repo");
    p.both("repo", &["pack-refs", "--auto"]);
    p.both("repo", &["pack-refs"]);
    for root in [&p.git, &p.rgit] {
        let dir = root.join("repo/.git/reftable");
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            if entry.file_name().to_string_lossy().ends_with(".lock") {
                std::fs::remove_file(entry.path()).unwrap();
            }
        }
    }
    p.ok("repo", &[&["pack-refs", "--auto"], &["pack-refs"]]);
    p.same_state("compacted", "repo");
}

/// A scenario of [`operations_continue_with_the_other_tool`].
struct Operation {
    label: &'static str,
    /// Commands (reference git) after the common setup.
    before: &'static [&'static [&'static str]],
    /// The command that stops in the middle of the operation.
    start: &'static [&'static str],
    /// Resolve the conflict in `f` before `end`.
    resolve: bool,
    /// The command that continues, concludes or aborts the operation.
    end: &'static [&'static str],
}

/// An operation that stops (cherry-pick, revert, rebase, merge with
/// `--autostash`, bisect) keeps its pseudo-references in the stack, as
/// git does: started by one tool, the other one continues or aborts it.
/// One side starts with rgit and continues with git, the other the other
/// way round; both have to see the same root references at every step.
#[test]
fn operations_continue_with_the_other_tool() {
    let Some(p) = Pair::new(false) else { return };
    let rgit = Path::new(env!("CARGO_BIN_EXE_rgit"));
    let git = p.fx.reference_git();
    let env = [("GIT_EDITOR", "true")];
    let run = |root: &Path, dir: &str, args: &[&str]| {
        let r = p.exec(git, root, dir, args, "", &env);
        assert_eq!(r.code, Some(0), "{args:?}: {}", r.stderr);
    };
    // `f` differs on `main` and `topic`, so that picking `topic` stops.
    let setup = |root: &Path| {
        let _ = std::fs::remove_dir_all(root.join("repo"));
        run(root, "", INIT);
        std::fs::write(root.join("repo/f"), "a\n").unwrap();
        run(root, "repo", &["add", "f"]);
        run(root, "repo", &["commit", "-q", "-m", "a"]);
        run(root, "repo", &["checkout", "-q", "-b", "topic"]);
        std::fs::write(root.join("repo/f"), "b\n").unwrap();
        run(root, "repo", &["commit", "-q", "-a", "-m", "b"]);
        run(root, "repo", &["checkout", "-q", "main"]);
        std::fs::write(root.join("repo/f"), "c\n").unwrap();
        run(root, "repo", &["commit", "-q", "-a", "-m", "c"]);
        std::fs::write(root.join("repo/g"), "x\n").unwrap();
    };
    const PICK: &[&str] = &["cherry-pick", "topic"];
    const AUTOSTASH: &[&str] = &["merge", "--autostash", "topic"];
    const COMMIT: &[&str] = &["commit", "-q", "--no-edit"];
    let op = |label, before, start, resolve, end| Operation {
        label,
        before,
        start,
        resolve,
        end,
    };
    let operations = [
        op("pick, abort", &[], PICK, false, &["cherry-pick", "--abort"]),
        op(
            "pick, continue",
            &[],
            PICK,
            true,
            &["cherry-pick", "--continue"],
        ),
        op("pick, commit", &[], PICK, true, COMMIT),
        op("pick, reset", &[], PICK, false, &["reset", "-q", "--hard"]),
        op(
            "revert, abort",
            &[
                &["checkout", "-q", "topic"],
                &["commit", "-q", "--allow-empty", "-m", "d"],
            ],
            &["revert", "HEAD~1"],
            false,
            &["revert", "--abort"],
        ),
        op(
            "rebase, abort",
            &[],
            &["rebase", "topic"],
            false,
            &["rebase", "--abort"],
        ),
        op(
            "rebase, skip",
            &[],
            &["rebase", "topic"],
            false,
            &["rebase", "--skip"],
        ),
        op(
            "autostash, abort",
            &[&["add", "g"]],
            AUTOSTASH,
            false,
            &["merge", "--abort"],
        ),
        op(
            "autostash, commit",
            &[&["add", "g"]],
            AUTOSTASH,
            true,
            COMMIT,
        ),
        op(
            "autostash, quit",
            &[&["add", "g"]],
            AUTOSTASH,
            false,
            &["merge", "--quit"],
        ),
        op(
            "bisect --no-checkout",
            &[],
            &["bisect", "start", "--no-checkout", "main", "main~1"],
            false,
            &["bisect", "reset"],
        ),
        op(
            "bisect",
            &[&["commit", "-q", "--allow-empty", "-m", "d"]],
            &["bisect", "start", "main", "main~2"],
            false,
            &["bisect", "good"],
        ),
    ];
    let state = |root: &Path, out: &mut String| {
        for args in [
            &["for-each-ref", "--include-root-refs"][..],
            &["status", "--porcelain"],
            &["stash", "list"],
            &["log", "--format=%s", "-3"],
        ] {
            out.push_str(&p.inspect(root, "repo", args));
        }
    };
    for o in &operations {
        let mut sides = Vec::new();
        for (root, first, second) in [(&p.git, rgit, git), (&p.rgit, git, rgit)] {
            setup(root);
            for args in o.before {
                run(root, "repo", args);
            }
            let started = p.exec(first, root, "repo", o.start, "", &env);
            let mut out = format!("started: exit {:?}\n", started.code);
            state(root, &mut out);
            if o.resolve {
                std::fs::write(root.join("repo/f"), "r\n").unwrap();
                run(root, "repo", &["add", "f"]);
            }
            let ended = p.exec(second, root, "repo", o.end, "", &env);
            out.push_str(&format!(
                "ended: exit {:?}\n--- stdout\n{}--- stderr\n{}",
                ended.code, ended.stdout, ended.stderr
            ));
            state(root, &mut out);
            sides.push(out);
        }
        // The operation did stop with a pseudo-reference in the stack.
        assert!(
            ["AUTO_MERGE", "BISECT_EXPECTED_REV", "BISECT_HEAD"]
                .iter()
                .any(|name| sides[0].contains(name)),
            "{}: {}",
            o.label,
            sides[0]
        );
        rgit_difftest::assert_same_text(o.label, &sides[0], &sides[1]);
    }
}
