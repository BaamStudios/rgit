// SPDX-License-Identifier: MIT
//! The command line and the library (`rgit`, ADR 0005) answer alike, and
//! both like the reference git.
//!
//! While commands move onto the library one by one, this is the test that
//! notices when the two drift apart: the same repository is asked through
//! the `rgit` binary, through the library API and through git.

use std::path::Path;
use std::process::{Command, Output};

use rgit::{LogOptions, MapEnv, OpenOptions, Repository, StatusOptions};
use rgit_difftest::Fixture;

const RGIT: &str = env!("CARGO_BIN_EXE_rgit");

/// Run the binary in the fixture, with the configuration the library
/// gets in [`library`].
fn rgit(fx: &Fixture, args: &[&str]) -> Output {
    let mut command = Command::new(RGIT);
    command.args(args).current_dir(fx.work_dir()).env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    command
        .env("HOME", fx.home_dir())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", fx.work_dir().parent().unwrap())
        .env("TZ", "UTC")
        .env("LC_ALL", "C")
        .env("GIT_PAGER", "cat")
        .output()
        .expect("run rgit")
}

/// Open the fixture through the library with the same home directory.
fn library(fx: &Fixture) -> Repository {
    let env = MapEnv::new([
        ("HOME", fx.home_dir().into_os_string()),
        ("GIT_CONFIG_NOSYSTEM", "1".into()),
        ("GIT_ATTR_NOSYSTEM", "1".into()),
    ]);
    OpenOptions::new()
        .environment(env)
        .open(fx.work_dir())
        .expect("open the fixture")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn fixture() -> Fixture {
    Fixture::new()
        .git(["init", "-q"])
        .write(".gitignore", "*.tmp\n")
        .write("README", "read me\n")
        .write("src/main.c", "int main(void) { return 0; }\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "start"])
        .tick()
        .write("src/main.c", "int main(void) { return 1; }\n")
        .git(["commit", "-q", "-am", "fail on purpose"])
        .git(["tag", "-a", "-m", "first release", "v0.1"])
        .tick()
        .git(["switch", "-q", "-c", "topic"])
        .write("src/util.c", "void util(void) {}\n")
        .git(["add", "src/util.c"])
        .git(["commit", "-q", "-m", "a helper"])
        .tick()
        .git(["switch", "-q", "main"])
        .write("README", "read me first\n")
        .git(["commit", "-q", "-am", "more words"])
        .tick()
        .git(["merge", "-q", "--no-edit", "topic"])
        // and a work tree that is not clean
        .write("README", "read me first, please\n")
        .write("src/main.c", "int main(void) { return 2; }\n")
        .git(["add", "src/main.c"])
        .write("src/new.c", "/* new */\n")
        .write("notes/todo", "later\n")
        .write("scratch.tmp", "ignored\n")
        .git(["rm", "-q", "--cached", ".gitignore"])
        .git(["add", ".gitignore"])
}

#[test]
fn status_agrees_three_ways() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    for mode in ["normal", "all", "no"] {
        let untracked = format!("--untracked-files={mode}");
        let args = ["status", "--porcelain", "--no-renames", "-z", &untracked];
        let files = match mode {
            "normal" => rgit::UntrackedFiles::Normal,
            "all" => rgit::UntrackedFiles::All,
            _ => rgit::UntrackedFiles::No,
        };
        let from_library: String = library(&fx)
            .status(StatusOptions::new().untracked_files(files))
            .unwrap()
            .entries
            .iter()
            .map(|e| format!("{} {}\0", text(&e.code()), e.path))
            .collect();
        let from_binary = rgit(&fx, &args);
        assert!(from_binary.status.success(), "{from_binary:?}");
        let from_git = fx.git_stdout(args);
        assert_eq!(from_library, text(&from_git), "library, {mode}");
        assert_eq!(text(&from_binary.stdout), text(&from_git), "binary, {mode}");
    }
}

#[test]
fn history_agrees_three_ways() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let repo = library(&fx);
    let cases: [(&[&str], LogOptions); 5] = [
        (&["HEAD"], LogOptions::new()),
        (
            &["--first-parent", "HEAD"],
            LogOptions::new().first_parent(true),
        ),
        (
            &["-n", "2", "topic"],
            LogOptions::new().rev("topic").max_count(2),
        ),
        (&["v0.1..main"], LogOptions::new().rev("v0.1..main")),
        (&["topic...main~1"], LogOptions::new().rev("topic...main~1")),
    ];
    for (args, options) in cases {
        let mut command = vec!["rev-list"];
        command.extend_from_slice(args);
        let from_library: String = repo
            .log(options)
            .unwrap()
            .map(|c| format!("{}\n", c.unwrap().id))
            .collect();
        let from_binary = rgit(&fx, &command);
        let from_git = fx.git_stdout(&command);
        assert_eq!(from_library, text(&from_git), "library, {args:?}");
        assert_eq!(
            text(&from_binary.stdout),
            text(&from_git),
            "binary, {args:?}"
        );
    }
}

#[test]
fn revisions_and_their_errors_agree_three_ways() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let repo = library(&fx);
    for spec in [
        "HEAD",
        "main~2",
        "v0.1",
        "v0.1^{commit}",
        "topic^{tree}",
        "HEAD:src/main.c",
        ":README",
        "HEAD^2",
        "main@{1}",
        ":/helper",
    ] {
        let args = ["rev-parse", "--verify", spec];
        let from_git = fx.git_stdout(args);
        let from_binary = rgit(&fx, &args);
        assert_eq!(text(&from_binary.stdout), text(&from_git), "binary, {spec}");
        assert_eq!(
            format!("{}\n", repo.rev_parse(spec).unwrap()),
            text(&from_git),
            "library, {spec}"
        );
    }

    // What the binary prints for a failure is the error of the library
    // behind `fatal: `, and its exit code.
    for spec in ["HEAD:no/such/file", "main@{50}", "topic@{upstream}"] {
        let args = ["rev-parse", spec];
        let from_git = fx.git_output(args);
        let from_binary = rgit(&fx, &args);
        let err = repo.rev_parse(spec).unwrap_err();
        let fatal = format!("fatal: {err}\n");
        assert!(
            text(&from_git.stderr).ends_with(&fatal),
            "git, {spec}: {from_git:?}"
        );
        assert!(
            text(&from_binary.stderr).ends_with(&fatal),
            "binary, {spec}: {from_binary:?}"
        );
        assert_eq!(from_git.status.code(), Some(err.exit_code()));
        assert_eq!(from_binary.status.code(), Some(err.exit_code()));
    }
}

#[test]
fn outside_a_repository_both_say_so() {
    let fx = Fixture::new();
    if fx.is_skipped() {
        return;
    }
    let from_binary = rgit(&fx, &["rev-parse", "HEAD"]);
    let err = OpenOptions::isolated()
        .ceiling_dir(fx.work_dir().parent().unwrap())
        .discover(fx.work_dir())
        .unwrap_err();
    assert_eq!(text(&from_binary.stderr), format!("fatal: {err}\n"));
    assert_eq!(from_binary.status.code(), Some(err.exit_code()));
    assert!(!Path::new(&fx.work_dir()).join(".git").exists());
}
