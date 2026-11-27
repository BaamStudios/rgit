// SPDX-License-Identifier: MIT
//! The library API against the reference `git`: repositories are built
//! with git, then the answers of the library are compared with what git
//! prints for the same question.
//!
//! Every test opens its repository with `OpenOptions::isolated()` (or an
//! explicit `MapEnv`), so neither the user's configuration nor the
//! environment of the test process play a part.

use rgit::{
    Cancel, Change, Conflict, Error, Head, LogOptions, MapEnv, OpenOptions, Order, Repository,
    State, StatusOptions, UntrackedFiles,
};
use rgit_difftest::Fixture;

fn open(fx: &Fixture) -> Repository {
    OpenOptions::isolated()
        .open(fx.work_dir())
        .expect("open the fixture repository")
}

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).expect("git output is UTF-8 in these tests")
}

/// A repository with three commits on `main`, a side branch merged into
/// it, an annotated tag `v1` on the second commit and a lightweight tag.
fn history() -> Fixture {
    Fixture::new()
        .git(["init", "-q"])
        .write("a.txt", "one\n")
        .write("dir/b.txt", "bee\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "first commit"])
        .tick()
        .write("a.txt", "one\ntwo\n")
        .git(["commit", "-q", "-am", "second commit"])
        .git(["tag", "-a", "-m", "release one", "v1"])
        .git(["tag", "light"])
        .tick()
        .git(["switch", "-q", "-c", "side"])
        .write("side.txt", "side\n")
        .git(["add", "side.txt"])
        .git(["commit", "-q", "-m", "side work"])
        .tick()
        .git(["switch", "-q", "main"])
        .write("a.txt", "one\ntwo\nthree\n")
        .git(["commit", "-q", "-am", "third commit"])
        .tick()
        .git(["merge", "-q", "--no-edit", "side"])
        .tick()
        .write("dir/b.txt", "bee\nbuzz\n")
        .git(["commit", "-q", "-am", "after the merge"])
}

#[test]
fn open_finds_only_the_named_directory() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let work = fx.work_dir();
    let repo = open(&fx);
    assert_eq!(repo.work_dir(), Some(work.as_path()));
    assert_eq!(repo.git_dir(), work.join(".git"));
    assert_eq!(repo.common_dir(), work.join(".git"));
    assert!(!repo.is_bare());
    assert_eq!(repo.object_format(), rgit::HashKind::Sha1);

    // The `.git` directory names the same repository.
    let by_git_dir = OpenOptions::isolated().open(work.join(".git")).unwrap();
    assert_eq!(by_git_dir.work_dir(), Some(work.as_path()));

    // A subdirectory is not a repository; a discovery finds the one above.
    let sub = work.join("dir");
    let err = OpenOptions::isolated().open(&sub).unwrap_err();
    assert!(matches!(err, Error::NotARepository { .. }), "{err:?}");
    assert_eq!(
        format!("fatal: {err}"),
        "fatal: not a git repository (or any of the parent directories): .git"
    );
    assert_eq!(err.exit_code(), 128);
    let found = OpenOptions::isolated().discover(&sub).unwrap();
    assert_eq!(found.work_dir(), Some(work.as_path()));

    // A ceiling stops the discovery.
    let err = OpenOptions::isolated()
        .ceiling_dir(&work)
        .discover(&sub)
        .unwrap_err();
    assert!(matches!(err, Error::NotARepository { .. }), "{err:?}");

    // A directory that does not exist is an I/O error, not a panic.
    let err = OpenOptions::isolated()
        .open(work.join("no/such/dir"))
        .unwrap_err();
    assert!(matches!(err, Error::Io { .. }), "{err:?}");
}

#[test]
fn bare_repositories_and_linked_work_trees() {
    let fx = history()
        .git(["clone", "-q", "--bare", ".", "../bare.git"])
        .git(["worktree", "add", "-q", "../linked", "side"]);
    if fx.is_skipped() {
        return;
    }
    let work = fx.work_dir();
    let parent = work.parent().unwrap();

    let bare = OpenOptions::isolated()
        .open(parent.join("bare.git"))
        .unwrap();
    assert!(bare.is_bare());
    assert_eq!(bare.work_dir(), None);
    assert_eq!(bare.git_dir(), parent.join("bare.git"));
    assert_eq!(
        bare.rev_parse("HEAD").unwrap().to_string(),
        text(fx.git_stdout(["rev-parse", "HEAD"])).trim()
    );
    let err = bare.status(StatusOptions::new()).unwrap_err();
    assert!(matches!(err, Error::BareRepository), "{err:?}");
    assert_eq!(err.to_string(), "this operation must be run in a work tree");

    let linked = OpenOptions::isolated().open(parent.join("linked")).unwrap();
    assert_eq!(linked.work_dir(), Some(parent.join("linked").as_path()));
    assert_eq!(linked.common_dir(), work.join(".git"));
    assert_eq!(linked.git_dir(), work.join(".git/worktrees/linked"));
    assert_eq!(
        linked.head().unwrap().branch().map(ToString::to_string),
        Some("refs/heads/side".to_string())
    );
    assert!(linked.status(StatusOptions::new()).unwrap().is_clean());
}

#[test]
fn head_on_a_branch_unborn_and_detached() {
    let fx = Fixture::new().git(["init", "-q"]);
    if fx.is_skipped() {
        return;
    }
    let head = open(&fx).head().unwrap();
    assert!(head.is_unborn());
    assert_eq!(head.id(), None);
    assert_eq!(head.shorthand().unwrap(), "main");

    let fx = history();
    let tip = text(fx.git_stdout(["rev-parse", "HEAD"]));
    let head = open(&fx).head().unwrap();
    assert_eq!(head.branch().unwrap(), "refs/heads/main");
    assert_eq!(head.id().unwrap().to_string(), tip.trim());
    assert!(!head.is_detached());

    let fx = fx.git(["checkout", "-q", "--detach", "HEAD~1"]);
    let at = text(fx.git_stdout(["rev-parse", "HEAD"]));
    match open(&fx).head().unwrap() {
        Head::Detached(id) => assert_eq!(id.to_string(), at.trim()),
        other => panic!("expected a detached HEAD, got {other:?}"),
    }
}

#[test]
fn references_are_listed_with_byte_string_names() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let repo = open(&fx);
    let listed: Vec<String> = repo
        .references("refs/")
        .unwrap()
        .iter()
        .map(|r| format!("{} {}", r.id.unwrap(), r.name))
        .collect();
    let expected = text(fx.git_stdout(["for-each-ref", "--format=%(objectname) %(refname)"]));
    assert_eq!(listed, expected.lines().collect::<Vec<_>>());

    let main = repo.find_reference("refs/heads/main").unwrap().unwrap();
    assert_eq!(main.shorthand(), "main");
    assert!(matches!(main.target, rgit::Target::Object(_)));
    let head = repo.find_reference(b"HEAD").unwrap().unwrap();
    assert_eq!(
        head.target,
        rgit::Target::Symbolic("refs/heads/main".into())
    );
    assert_eq!(head.id, main.id);
    assert_eq!(repo.find_reference("refs/heads/none").unwrap(), None);

    // A name that is not UTF-8 is a value of the API, and an error (not a
    // panic) while the backend cannot store it.
    let err = repo.find_reference(b"refs/heads/caf\xe9").unwrap_err();
    assert!(matches!(err, Error::InvalidRefName { .. }), "{err:?}");
}

#[test]
fn rev_parse_agrees_with_git() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let repo = open(&fx);
    let short = text(fx.git_stdout(["rev-parse", "--short=8", "HEAD~2"]));
    let specs = [
        "HEAD",
        "@",
        "main",
        "side",
        "refs/heads/side",
        "heads/main",
        "v1",
        "v1^{commit}",
        "v1^{}",
        "light",
        "HEAD~1",
        "HEAD~1^2",
        "HEAD^^",
        "HEAD^{tree}",
        "HEAD:a.txt",
        "HEAD:dir",
        "HEAD~3:dir/b.txt",
        ":a.txt",
        ":0:dir/b.txt",
        "HEAD@{0}",
        "main@{1}",
        "@{-1}",
        ":/second",
        "HEAD^{/first}",
        short.trim(),
    ];
    for spec in specs {
        let expected = text(fx.git_stdout(["rev-parse", "--verify", spec]));
        match repo.rev_parse(spec) {
            Ok(id) => assert_eq!(id.to_string(), expected.trim(), "rev-parse {spec}"),
            Err(e) => panic!("rev-parse {spec}: {e}"),
        }
    }
}

#[test]
fn rev_parse_errors_carry_the_message_of_git() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let repo = open(&fx);

    let err = repo.rev_parse("no-such-thing").unwrap_err();
    assert!(
        matches!(&err, Error::RevisionNotFound { spec } if spec == "no-such-thing"),
        "{err:?}"
    );
    assert_eq!(err.to_string(), "bad revision 'no-such-thing'");

    // Where git explains the failure, the library has the same text.
    for spec in [
        "HEAD:missing.txt",
        "main@{99}",
        "@{upstream}",
        ":missing.txt",
    ] {
        let git = fx.git_output(["rev-parse", spec]);
        let stderr = String::from_utf8_lossy(&git.stderr).into_owned();
        let fatal = stderr
            .lines()
            .find_map(|l| l.strip_prefix("fatal: "))
            .unwrap_or_else(|| panic!("git rev-parse {spec} printed no fatal line: {stderr}"));
        match repo.rev_parse(spec) {
            Err(e @ Error::Revision { .. }) => assert_eq!(e.to_string(), fatal, "{spec}"),
            other => panic!("rev-parse {spec}: expected a revision error, got {other:?}"),
        }
    }

    // Not UTF-8: names nothing (and does not panic).
    let err = repo.rev_parse(b"ma\xffin").unwrap_err();
    assert!(matches!(err, Error::RevisionNotFound { .. }), "{err:?}");
}

/// The ids of a log as lines, like `git rev-list` prints them.
fn log_ids(repo: &Repository, options: LogOptions) -> Vec<String> {
    repo.log(options)
        .expect("start the walk")
        .map(|c| c.expect("read a commit").id.to_string())
        .collect()
}

#[test]
fn log_agrees_with_rev_list() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let repo = open(&fx);
    let new = LogOptions::new;
    let cases: Vec<(&[&str], LogOptions)> = vec![
        (&["HEAD"], new()),
        (&["HEAD"], new().rev("HEAD")),
        (&["--first-parent", "HEAD"], new().first_parent(true)),
        (&["--topo-order", "HEAD"], new().order(Order::Topological)),
        (
            &["--date-order", "HEAD"],
            new().order(Order::DateTopological),
        ),
        (&["-n", "2", "HEAD"], new().max_count(2)),
        (&["--skip=1", "-n", "3", "HEAD"], new().skip(1).max_count(3)),
        (&["--reverse", "HEAD"], new().reverse(true)),
        (
            &["--reverse", "-n", "2", "HEAD"],
            new().reverse(true).max_count(2),
        ),
        (&["v1..HEAD"], new().rev("v1..HEAD")),
        (&["HEAD", "^v1"], new().rev("HEAD").rev("^v1")),
        (&["side...HEAD~1"], new().rev("side...HEAD~1")),
        (&["side", "light"], new().rev("side").rev("light")),
        (&["v1"], new().rev("v1")),
    ];
    for (args, options) in cases {
        let mut command = vec!["rev-list"];
        command.extend_from_slice(args);
        let expected = text(fx.git_stdout(&command));
        assert_eq!(
            log_ids(&repo, options),
            expected.lines().collect::<Vec<_>>(),
            "rev-list {args:?}"
        );
    }
}

#[test]
fn log_hands_out_parsed_commits() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let repo = open(&fx);
    let format = "--format=%H%x09%T%x09%P%x09%an%x09%ae%x09%at%x09%cn%x09%ce%x09%ct%x09%s";
    let expected = text(fx.git_stdout(["log", format]));
    let got: Vec<String> = repo
        .log(LogOptions::new())
        .unwrap()
        .map(|c| {
            let c = c.unwrap();
            let parents: Vec<String> = c.parents.iter().map(ToString::to_string).collect();
            format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                c.id,
                c.tree,
                parents.join(" "),
                c.author.name,
                c.author.email,
                c.author.time.unwrap().seconds,
                c.committer.name,
                c.committer.email,
                c.committer.time.unwrap().seconds,
                c.summary()
            )
        })
        .collect();
    assert_eq!(got, expected.lines().collect::<Vec<_>>());

    let err = repo.log(LogOptions::new().rev("nope")).unwrap_err();
    assert!(matches!(err, Error::RevisionNotFound { .. }), "{err:?}");
    // A tree is not a commit.
    let err = repo.log(LogOptions::new().rev("HEAD^{tree}")).unwrap_err();
    assert!(
        matches!(err, Error::Revision { .. } | Error::RevisionNotFound { .. }),
        "{err:?}"
    );
}

/// The status as `git status --porcelain` lines.
fn status_lines(repo: &Repository, options: StatusOptions) -> Vec<String> {
    repo.status(options)
        .expect("status")
        .entries
        .iter()
        .map(|e| format!("{} {}", String::from_utf8_lossy(&e.code()), e.path))
        .collect()
}

/// What git prints for the same status.
fn git_status(fx: &Fixture, mode: &str) -> Vec<String> {
    let out = fx.git_stdout([
        "status",
        "--porcelain",
        "--no-renames",
        "-z",
        &format!("--untracked-files={mode}"),
    ]);
    text(out)
        .split_terminator('\0')
        .map(str::to_string)
        .collect()
}

/// Compare the three untracked modes with git.
fn assert_status_like_git(fx: &Fixture) {
    for (mode, files) in [
        ("normal", UntrackedFiles::Normal),
        ("all", UntrackedFiles::All),
        ("no", UntrackedFiles::No),
    ] {
        // The library first: git refreshes the index as a side effect.
        let got = status_lines(&open(fx), StatusOptions::new().untracked_files(files));
        assert_eq!(got, git_status(fx, mode), "--untracked-files={mode}");
    }
}

#[test]
fn status_of_a_clean_and_of_a_new_repository() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let status = open(&fx).status(StatusOptions::new()).unwrap();
    assert!(status.is_clean());
    assert_eq!(status.head.shorthand().unwrap(), "main");
    assert_status_like_git(&fx);

    // No commit yet: everything staged is new.
    let fx = Fixture::new()
        .git(["init", "-q"])
        .write("new.txt", "new\n")
        .write("other.txt", "other\n")
        .git(["add", "new.txt"]);
    let status = open(&fx).status(StatusOptions::new()).unwrap();
    assert!(status.head.is_unborn());
    assert_eq!(
        status.entries[0].state,
        State::Tracked {
            index: Change::Added,
            worktree: Change::Unmodified
        }
    );
    assert_eq!(status.entries[1].state, State::Untracked);
    assert_status_like_git(&fx);
}

#[test]
fn status_of_staged_and_unstaged_changes() {
    let fx = history()
        .write("exec.sh", "#!/bin/sh\n")
        .write("gone.txt", "gone\n")
        .write("staged-gone.txt", "gone\n")
        .write("link-later.txt", "file\n")
        .write("both.txt", "v1\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "more files"])
        // staged modification
        .write("a.txt", "changed\n")
        .git(["add", "a.txt"])
        // unstaged modification
        .write("dir/b.txt", "changed too\n")
        // staged and then changed again
        .write("both.txt", "v2\n")
        .git(["add", "both.txt"])
        .write("both.txt", "v3\n")
        // new file, staged
        .write("added.txt", "added\n")
        .git(["add", "added.txt"])
        // new file, staged, then deleted from the work tree
        .write("added-gone.txt", "x\n")
        .git(["add", "added-gone.txt"])
        .remove("added-gone.txt")
        // deleted in the work tree only
        .remove("gone.txt")
        // deleted in the index
        .git(["rm", "-q", "staged-gone.txt"])
        // executable bit
        .git(["update-index", "--chmod=+x", "exec.sh"])
        // file replaced by a symbolic link
        .remove("link-later.txt")
        .symlink("a.txt", "link-later.txt")
        // intent to add
        .write("intent.txt", "later\n")
        .git(["add", "-N", "intent.txt"])
        // untracked
        .write("untracked.txt", "u\n")
        .write("new-dir/deep/file.txt", "u\n")
        .write("new-dir/other.txt", "u\n")
        .write("dir/untracked-in-tracked.txt", "u\n");
    if fx.is_skipped() {
        return;
    }
    assert_status_like_git(&fx);

    let status = open(&fx).status(StatusOptions::new()).unwrap();
    let state = |path: &str| {
        status
            .entries
            .iter()
            .find(|e| e.path == path)
            .unwrap_or_else(|| panic!("{path} is missing from the status"))
            .state
    };
    assert_eq!(
        state("both.txt"),
        State::Tracked {
            index: Change::Modified,
            worktree: Change::Modified
        }
    );
    assert_eq!(
        state("link-later.txt"),
        State::Tracked {
            index: Change::Unmodified,
            worktree: Change::TypeChanged
        }
    );
    assert_eq!(
        state("intent.txt"),
        State::Tracked {
            index: Change::Unmodified,
            worktree: Change::Added
        }
    );
    assert_eq!(state("new-dir/"), State::Untracked);
}

#[test]
fn status_honours_the_ignore_rules() {
    let fx = history()
        .write(".gitignore", "*.log\n/build/\n!keep.log\n")
        .write("dir/.gitignore", "local-*\n")
        .git(["add", ".gitignore", "dir/.gitignore"])
        .git(["commit", "-q", "-m", "ignore rules"])
        .write("debug.log", "x\n")
        .write("keep.log", "x\n")
        .write("build/out.o", "x\n")
        .write("dir/local-notes", "x\n")
        .write("dir/visible", "x\n")
        .write("only-ignored/trace.log", "x\n")
        .write("mixed/trace.log", "x\n")
        .write("mixed/readme", "x\n")
        .write(".git/info/exclude", "excluded-by-info\n")
        .write("excluded-by-info", "x\n")
        .write("empty-dir/nested/.keepme.log", "x\n");
    if fx.is_skipped() {
        return;
    }
    assert_status_like_git(&fx);
}

#[test]
fn status_reads_the_user_configuration_only_through_its_environment() {
    let fx = history().write("scratch.tmp", "x\n");
    if fx.is_skipped() {
        return;
    }
    let ignore = fx.work_dir().parent().unwrap().join("global-ignore");
    std::fs::write(&ignore, "*.tmp\n").unwrap();
    let untracked = |options: OpenOptions| {
        status_lines(&options.open(fx.work_dir()).unwrap(), StatusOptions::new())
    };

    // Isolated: no user configuration, the file is untracked.
    assert_eq!(untracked(OpenOptions::isolated()), ["?? scratch.tmp"]);

    // A setting given by the caller.
    let overridden = OpenOptions::isolated().config_override(
        "core.excludesFile",
        Some(ignore.as_os_str().as_encoded_bytes()),
    );
    assert!(untracked(overridden).is_empty());

    // A home directory given by the caller, with its own configuration.
    let home = fx.work_dir().parent().unwrap().join("other-home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join(".gitconfig"),
        // With `/`: a backslash starts an escape sequence in a value.
        format!(
            "[core]\n\texcludesFile = {}\n",
            rgit_fs::path::to_slash(ignore.clone()).display()
        ),
    )
    .unwrap();
    let env = MapEnv::new([
        ("HOME", home.as_os_str().to_os_string()),
        ("GIT_CONFIG_NOSYSTEM", "1".into()),
    ]);
    assert!(untracked(OpenOptions::new().environment(env)).is_empty());
}

#[test]
fn status_of_merge_conflicts() {
    let fx = Fixture::new()
        .git(["init", "-q"])
        .write("both-modified", "base\n")
        .write("deleted-by-them", "base\n")
        .write("deleted-by-us", "base\n")
        .write("clean", "base\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "base"])
        .tick()
        .git(["switch", "-q", "-c", "theirs"])
        .write("both-modified", "theirs\n")
        .write("deleted-by-us", "theirs\n")
        .write("both-added", "theirs\n")
        .git(["rm", "-q", "deleted-by-them"])
        .git(["add", "."])
        .git(["commit", "-q", "-m", "theirs"])
        .tick()
        .git(["switch", "-q", "main"])
        .write("both-modified", "ours\n")
        .write("deleted-by-them", "ours\n")
        .write("both-added", "ours\n")
        .git(["rm", "-q", "deleted-by-us"])
        .git(["add", "."])
        .git(["commit", "-q", "-m", "ours"]);
    if fx.is_skipped() {
        return;
    }
    let merge = fx.git_output(["merge", "theirs"]);
    assert!(!merge.status.success(), "the merge is meant to conflict");
    assert_status_like_git(&fx);

    let status = open(&fx).status(StatusOptions::new()).unwrap();
    let conflict = |path: &str| {
        status
            .entries
            .iter()
            .find(|e| e.path == path)
            .map(|e| e.state)
    };
    assert_eq!(
        conflict("both-modified"),
        Some(State::Conflicted(Conflict::BothModified))
    );
    assert_eq!(
        conflict("both-added"),
        Some(State::Conflicted(Conflict::BothAdded))
    );
    assert_eq!(
        conflict("deleted-by-them"),
        Some(State::Conflicted(Conflict::DeletedByThem))
    );
    assert_eq!(
        conflict("deleted-by-us"),
        Some(State::Conflicted(Conflict::DeletedByUs))
    );
}

#[test]
fn status_converts_line_endings_like_git() {
    // With core.autocrlf the work tree file has CR LF and the blob LF:
    // rewriting the file changes its times, not its status.
    let fx = Fixture::new()
        .git(["init", "-q"])
        .git(["config", "core.autocrlf", "true"])
        .write(".gitattributes", "*.bin -text\n*.lf text eol=lf\n")
        .write("text.txt", "one\r\ntwo\r\n")
        .write("data.bin", "one\r\ntwo\r\n")
        .write("unix.lf", "one\ntwo\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "line endings"])
        .tick()
        .write("text.txt", "one\r\ntwo\r\n")
        .write("data.bin", "one\r\ntwo\r\n")
        .write("unix.lf", "one\ntwo\n");
    if fx.is_skipped() {
        return;
    }
    assert_status_like_git(&fx);
    assert!(open(&fx).status(StatusOptions::new()).unwrap().is_clean());

    let fx = fx.write("text.txt", "one\r\ntwo\r\nthree\r\n");
    assert_status_like_git(&fx);
    assert_eq!(
        status_lines(&open(&fx), StatusOptions::new()),
        [" M text.txt"]
    );
}

#[test]
fn status_reports_a_submodule_with_another_commit() {
    let fx = history()
        .git(["init", "-q", "sub"])
        .write("sub/file", "x\n")
        .git(["-C", "sub", "add", "file"])
        .git(["-C", "sub", "commit", "-q", "-m", "in the submodule"])
        .git(["add", "sub"])
        .git(["commit", "-q", "-m", "add a gitlink"]);
    if fx.is_skipped() {
        return;
    }
    assert_status_like_git(&fx);
    let fx = fx
        .tick()
        .write("sub/file", "y\n")
        .git(["-C", "sub", "commit", "-q", "-am", "moved on"]);
    assert_status_like_git(&fx);
    assert_eq!(status_lines(&open(&fx), StatusOptions::new()), [" M sub"]);
}

#[test]
fn status_refuses_what_it_cannot_compare() {
    // A file that has to go through a clean filter to be compared: the
    // library says so instead of guessing.
    let fx = Fixture::new()
        .git(["init", "-q"])
        .git(["config", "filter.upper.clean", "tr a-z A-Z"])
        .git(["config", "filter.upper.smudge", "cat"])
        .write(".gitattributes", "*.up filter=upper\n")
        .write("word.up", "hello\n")
        .write("plain.txt", "hello\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "filtered"]);
    if fx.is_skipped() {
        return;
    }
    // Same size, so the content decides, and the content has to go through
    // the filter first.
    let fx = fx.write("word.up", "jello\n");
    let err = open(&fx).status(StatusOptions::new()).unwrap_err();
    match &err {
        Error::Unsupported { detail, .. } => assert_eq!(detail, "word.up"),
        other => panic!("expected Unsupported, got {other:?}"),
    }

    // Another size is a modification for git without looking at the
    // content, and for the library as well.
    let fx = fx.write("word.up", "hello again\n");
    assert_status_like_git(&fx);
    assert_eq!(
        status_lines(&open(&fx), StatusOptions::new()),
        [" M word.up"]
    );
}

#[test]
fn operations_stop_when_cancelled() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let repo = open(&fx);
    let cancel = Cancel::new();
    cancel.cancel();

    let err = repo
        .status(StatusOptions::new().cancel(cancel.clone()))
        .unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err:?}");
    assert_eq!(err.exit_code(), 130);

    let first = repo
        .log(LogOptions::new().cancel(cancel.clone()))
        .and_then(|mut log| log.next().transpose());
    assert!(matches!(first, Err(Error::Cancelled)), "{first:?}");

    // A token that is not cancelled changes nothing.
    let running = Cancel::new();
    assert!(
        repo.status(StatusOptions::new().cancel(running))
            .unwrap()
            .is_clean()
    );
}

#[test]
fn one_handle_serves_many_threads() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let repo = open(&fx);
    let tip = text(fx.git_stdout(["rev-parse", "HEAD"]));
    let count = text(fx.git_stdout(["rev-list", "--count", "HEAD"]));
    let count: usize = count.trim().parse().unwrap();
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                for _ in 0..10 {
                    assert_eq!(repo.rev_parse("HEAD").unwrap().to_string(), tip.trim());
                    assert_eq!(repo.log(LogOptions::new()).unwrap().count(), count);
                    assert!(repo.status(StatusOptions::new()).unwrap().is_clean());
                }
            });
        }
    });
    // Nothing was printed and nothing went wrong on the way.
    assert!(repo.take_diagnostics().is_empty());
}

#[test]
fn a_damaged_repository_is_an_error_not_a_panic() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let tip = text(fx.git_stdout(["rev-parse", "HEAD"]));
    let tip = tip.trim();
    let object = fx
        .work_dir()
        .join(".git/objects")
        .join(&tip[..2])
        .join(&tip[2..]);
    let mut permissions = std::fs::metadata(&object).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    std::fs::set_permissions(&object, permissions).unwrap();
    std::fs::write(&object, b"this is not a zlib stream").unwrap();

    let repo = open(&fx);
    let first = repo
        .log(LogOptions::new())
        .and_then(|mut log| log.next().transpose());
    assert!(matches!(first, Err(Error::Corrupt { .. })), "{first:?}");
    let err = repo.status(StatusOptions::new()).unwrap_err();
    assert!(matches!(err, Error::Corrupt { .. }), "{err:?}");

    // An index that is not an index.
    let fx = history().write(".git/index", "garbage");
    let err = open(&fx).status(StatusOptions::new()).unwrap_err();
    assert!(matches!(err, Error::Corrupt { .. }), "{err:?}");
}
