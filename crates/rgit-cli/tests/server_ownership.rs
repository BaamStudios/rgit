// SPDX-License-Identifier: MIT
//! Who owns the repository a server enters (`safe.directory`,
//! Documentation/config/safe.adoc), against the reference git.
//!
//! `receive-pack`, `upload-archive` and `http-backend` refuse a
//! repository owned by someone else unless it is listed in
//! `safe.directory`; `upload-pack` serves it (`daemon` is in
//! `daemon.rs`). Ownership is faked with `GIT_TEST_ASSUME_DIFFERENT_OWNER`
//! (as git's own t0033 does); the tests never change the owner of a file.

#![cfg(unix)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use rgit_difftest::{Check, Fixture, Normalizer};

mod common;
use common::servers::{isolated, server_path};

fn rgit() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rgit"))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `w` (a work tree with one commit), `b.git` (a bare clone), `lnk.git`
/// (a link to it), `lw` (a link to `w`), `wt` (a linked work tree of
/// `w`), `sep` (a work tree whose git directory is `sep.git`), `gf` (a
/// broken `.git` file).
fn fixture() -> Fixture {
    let fx = Fixture::new()
        .rgit(rgit())
        .git(["init", "-q", "-b", "main", "w"]);
    if fx.is_skipped() {
        return fx;
    }
    fx.write("w/f", "f\n")
        .git(["-C", "w", "add", "f"])
        .git(["-C", "w", "commit", "-q", "-m", "f"])
        .tick()
        .git(["clone", "-q", "--bare", "w", "b.git"])
        .symlink("b.git", "lnk.git")
        .symlink("w", "lw")
        .git(["-C", "w", "worktree", "add", "-q", "../wt"])
        .git(["init", "-q", "--separate-git-dir=sep.git", "sep"])
        .write("gf", "junk\n")
}

/// Exit code and standard error of `args` must be equal; standard output
/// too, apart from the `agent` capability and the length of its packet.
#[track_caller]
fn same(fx: &mut Fixture, dir: &str, env: &[(&str, &str)], args: &[&str]) {
    let mut c = fx
        .compare(args)
        .in_dir(dir)
        .checks([Check::ExitCode, Check::Stderr, Check::Stdout])
        .normalize_stdout(Normalizer::regex(
            r"(?m)^[0-9a-f]{4}([0-9a-f]{40} [^\x00\n]*)\x00(.*) agent=\S*",
            "XXXX$1\0$2 agent=X",
        ));
    for (k, v) in env {
        c = c.env(k, v);
    }
    c.run();
}

const FOREIGN: &[(&str, &str)] = &[("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1")];

#[test]
fn receive_pack_checks_the_owner() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let work = fx.work_dir().display().to_string();
    for dir in [
        "w", "w/", "w/.git", "b.git", "b", "./b.git/", "lnk.git", "lw", "wt", "wt/.git", "sep",
        "nope", "gf", &work,
    ] {
        same(
            &mut fx,
            ".",
            FOREIGN,
            &["receive-pack", "--advertise-refs", dir],
        );
    }
    // What `safe.directory` has to name: the git directory (or the `.git`
    // file) with links resolved; `.` is the current directory.
    for value in [
        format!("{work}/w"),
        format!("{work}/w/.git"),
        format!("{work}/w/.git/"),
        format!("{work}/lw/.git"),
        format!("{work}/b.git"),
        format!("{work}/lnk.git"),
        format!("{work}/*"),
        format!("{work}/w/*"),
        format!("{work}/wt/.git"),
        format!("{work}/w/.git/worktrees/wt"),
        format!("{work}/sep/.git"),
        format!("{work}/sep.git"),
        "*".to_owned(),
        ".".to_owned(),
        "relative".to_owned(),
        String::new(),
    ] {
        let config = format!("safe.directory={value}");
        for dir in ["w", "b.git", "lnk.git", "wt", "sep"] {
            same(
                &mut fx,
                ".",
                FOREIGN,
                &["-c", &config, "receive-pack", "--advertise-refs", dir],
            );
        }
    }
    for (cwd, dir) in [("w", ".git"), ("w", "."), ("w/.git", ".."), ("b.git", ".")] {
        same(
            &mut fx,
            cwd,
            FOREIGN,
            &[
                "-c",
                "safe.directory=.",
                "receive-pack",
                "--advertise-refs",
                dir,
            ],
        );
    }
    // Only protected configuration counts; `GIT_DIR` makes no difference.
    fx = fx.git(["-C", "b.git", "config", "safe.directory", "*"]);
    same(
        &mut fx,
        ".",
        FOREIGN,
        &["receive-pack", "--advertise-refs", "b.git"],
    );
    let with_git_dir = [FOREIGN[0], ("GIT_DIR", "b.git")];
    same(
        &mut fx,
        ".",
        &with_git_dir,
        &["receive-pack", "--advertise-refs", "b.git"],
    );
    same(
        &mut fx,
        ".",
        &[],
        &["receive-pack", "--advertise-refs", "b.git"],
    );
}

#[test]
fn upload_pack_does_not_check() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    for dir in ["w", "b.git", "wt", "sep", "gf"] {
        same(
            &mut fx,
            ".",
            FOREIGN,
            &["upload-pack", "--advertise-refs", dir],
        );
        same(
            &mut fx,
            ".",
            FOREIGN,
            &["upload-pack", "--strict", "--advertise-refs", dir],
        );
    }
}

#[test]
fn upload_archive_checks_the_owner() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let work = fx.work_dir().display().to_string();
    for dir in ["w", "w/", "b.git", "b", "lw", "wt", "sep", "nope", "gf"] {
        same(&mut fx, ".", FOREIGN, &["upload-archive", dir]);
    }
    for config in [
        format!("safe.directory={work}/w/.git"),
        format!("safe.directory={work}/w"),
        format!("safe.directory={work}/b.git"),
    ] {
        same(
            &mut fx,
            ".",
            FOREIGN,
            &["-c", &config, "upload-archive", "w"],
        );
    }
    same(
        &mut fx,
        "w",
        FOREIGN,
        &["-c", "safe.directory=.", "upload-archive", "."],
    );
    // The repository is named explicitly: `safe.bareRepository` does not
    // apply.
    same(
        &mut fx,
        ".",
        &[],
        &[
            "-c",
            "safe.bareRepository=explicit",
            "upload-archive",
            "b.git",
        ],
    );
}

/// `<program> http-backend` with the CGI variables `vars`.
fn cgi(fx: &Fixture, program: &Path, vars: &[(&str, String)]) -> Output {
    let mut cmd = Command::new(program);
    isolated(fx, &mut cmd);
    cmd.env("PATH", server_path(fx))
        .arg("http-backend")
        .envs(vars.iter().map(|(k, v)| (*k, v.as_str())))
        .current_dir(fx.work_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    drop(child.stdin.take());
    child.wait_with_output().unwrap()
}

#[test]
fn http_backend_checks_the_owner() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let work = fx.work_dir().display().to_string();
    let requests = [
        ("/b.git/HEAD", ""),
        ("/b.git/info/refs", ""),
        ("/b.git/info/refs", "service=git-upload-pack"),
        ("/b.git/info/refs", "service=git-receive-pack"),
        ("/w/HEAD", ""),
        ("/w/.git/HEAD", ""),
        ("/wt/HEAD", ""),
        ("/sep/HEAD", ""),
        ("/lnk.git/HEAD", ""),
        ("/nope/HEAD", ""),
        ("/gf/HEAD", ""),
        ("/b.git/foo", ""),
    ];
    let settings: [&[(&str, String)]; 5] = [
        &[("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1".into())],
        &[("GIT_HTTP_EXPORT_ALL", "1".into())],
        &[
            ("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1".into()),
            ("GIT_HTTP_EXPORT_ALL", "1".into()),
        ],
        &[
            ("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1".into()),
            ("GIT_HTTP_EXPORT_ALL", "1".into()),
            ("GIT_CONFIG_COUNT", "1".into()),
            ("GIT_CONFIG_KEY_0", "safe.directory".into()),
            ("GIT_CONFIG_VALUE_0", format!("{work}/b.git")),
        ],
        &[
            ("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1".into()),
            ("GIT_HTTP_EXPORT_ALL", "1".into()),
            ("GIT_CONFIG_COUNT", "1".into()),
            ("GIT_CONFIG_KEY_0", "safe.directory".into()),
            ("GIT_CONFIG_VALUE_0", format!("{work}/w")),
        ],
    ];
    for extra in settings {
        for (path, query) in requests {
            let mut vars = vec![
                ("GIT_PROJECT_ROOT", work.clone()),
                ("PATH_INFO", path.to_owned()),
                ("QUERY_STRING", query.to_owned()),
                ("REQUEST_METHOD", "GET".to_owned()),
            ];
            vars.extend(extra.iter().cloned());
            let git = cgi(&fx, fx.reference_git(), &vars);
            let ours = cgi(&fx, &rgit(), &vars);
            // The advertisements differ in `agent=`; their headers and
            // everything else must be equal.
            let cut = |out: &[u8]| {
                let s = text(out);
                match s.find("\r\n\r\n") {
                    Some(at) if query.starts_with("service=") => s[..at].to_owned(),
                    _ => s,
                }
            };
            assert_eq!(
                (git.status.code(), cut(&git.stdout), text(&git.stderr)),
                (ours.status.code(), cut(&ours.stdout), text(&ours.stderr)),
                "{path}?{query} with {extra:?}"
            );
        }
    }
    // The same request again: rgit really refused (500 and the message).
    let vars = vec![
        ("GIT_PROJECT_ROOT", work.clone()),
        ("PATH_INFO", "/b.git/HEAD".to_owned()),
        ("REQUEST_METHOD", "GET".to_owned()),
        ("GIT_HTTP_EXPORT_ALL", "1".to_owned()),
        ("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1".to_owned()),
    ];
    let ours = cgi(&fx, &rgit(), &vars);
    assert!(text(&ours.stdout).starts_with("Status: 500 Internal Server Error\r\n"));
    assert!(
        text(&ours.stderr).starts_with(&format!(
            "fatal: detected dubious ownership in repository at '{work}/b.git'\n"
        )),
        "{}",
        text(&ours.stderr)
    );
    let _ = std::io::stderr().flush();
}

/// Run inside a user namespace as its root, with `chown` to other users
/// of the namespace: real owners instead of the test variable, also the
/// root cases of git's t0034 (`SUDO_UID`). `$G` is the program under
/// test, `$REF` the reference git that sets up the repositories, `$T` a
/// directory of its own that the script removes again (from outside the
/// namespace the files of other users could not be removed).
const USERNS_SCRIPT: &str = r#"
set -u
trap 'cd /; rm -rf "$T"' EXIT
mkdir -p "$T" && cd "$T" || exit 1
export HOME="$T" GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=A GIT_AUTHOR_EMAIL=a@x GIT_COMMITTER_NAME=A GIT_COMMITTER_EMAIL=a@x
unset SUDO_UID
"$REF" init -q -b main w && "$REF" -C w commit -q --allow-empty -m x
"$REF" clone -q --bare w foreign.git
"$REF" init -q -b main --separate-git-dir="$T/sep.git" sep
"$REF" init -q -b main --separate-git-dir="$T/sep2.git" sep2
"$REF" clone -q --bare w top.git
chown -R 4242:4242 foreign.git sep.git
chown 4242:4242 sep2/.git top.git
mkdir -p root/r root/p
"$REF" init -q root/r && chown -R 4242:4242 root/r
"$REF" init -q root/p
show() {
    label=$1; shift
    out=$("$@" </dev/null 2>&1); code=$?
    case "$out" in fatal:*) first=message ;; ????*) first=packets ;; *) first=nothing ;; esac
    msg=$(printf '%s\n' "$out" | tr -d '\000' | grep -a -o -m 1 -E "(fatal|warning): [^']*('[^']*')?" | sed "s#$T#T#g")
    printf '%s: %s %s %s\n' "$label" "$code" "$first" "$msg"
}
for repo in w foreign.git sep sep2 top.git; do
    show "receive-pack $repo" "$G" receive-pack --advertise-refs "$repo"
    show "upload-pack $repo" "$G" upload-pack --advertise-refs "$repo"
    show "upload-archive $repo" "$G" upload-archive "$repo"
    show "receive-pack $repo SUDO_UID" env SUDO_UID=4242 "$G" receive-pack --advertise-refs "$repo"
    show "receive-pack $repo safe" "$G" -c safe.directory="$T/$repo" receive-pack --advertise-refs "$repo"
done
for uid in 4242 ' 4242' +4242 4294971538 -4294963054 4243 '' 4242x 0x1092; do
    show "status r SUDO_UID=[$uid]" env SUDO_UID="$uid" "$G" -C root/r status --porcelain
done
show "status r" "$G" -C root/r status --porcelain
show "status p SUDO_UID" env SUDO_UID=4242 "$G" -C root/p status --porcelain
chown -R 4242:4242 root/p
show "status p explicit" sh -c 'cd root/p && GIT_DIR=.git GIT_WORK_TREE=. "$0" status --porcelain' "$G"
"#;

/// Whether a directory can be given to the other user of
/// [`USERNS_SCRIPT`] and then has that owner.
const USERNS_PROBE: &str = r#"
mkdir userns-probe || exit 1
chown 4242:4242 userns-probe && test "$(stat -c %u userns-probe)" = 4242
code=$?
rmdir userns-probe
exit $code
"#;

#[test]
fn real_owners_in_a_user_namespace() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    // The test needs a user namespace in which files can be given to
    // another user. `unshare` alone can succeed with only our own user
    // mapped (the CI runners); `chown` to the other user then fails and
    // no repository would have a foreign owner.
    let probe = Command::new("unshare")
        .args(["--map-auto", "--map-root-user", "sh", "-c", USERNS_PROBE])
        .current_dir(fx.work_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if !probe.is_ok_and(|s| s.success()) {
        eprintln!("skipping: no user namespace with other users");
        return;
    }
    let run = |side: &str, program: &Path| {
        let mut cmd = Command::new("unshare");
        isolated(&fx, &mut cmd);
        let out = cmd
            .args(["--map-auto", "--map-root-user", "sh", "-c", USERNS_SCRIPT])
            .env("G", program)
            .env("REF", fx.reference_git())
            .env("T", fx.work_dir().join(side))
            .env("PATH", server_path(&fx))
            .current_dir(fx.work_dir())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
        text(&out.stdout)
    };
    let git = run("ns-git", fx.reference_git());
    let ours = run("ns-rgit", &rgit());
    assert_eq!(git, ours);
    assert!(
        ours.contains("receive-pack foreign.git: 128 message fatal: detected dubious"),
        "{ours}"
    );
}
