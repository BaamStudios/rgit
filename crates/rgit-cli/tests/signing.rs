// SPDX-License-Identifier: MIT
//! Signing commits with OpenPGP (`commit -S`, `commit.gpgSign`,
//! `rebase -S`, `--no-gpg-sign`) against the reference git. `gpg.program`
//! is a stub in the work tree that records its arguments and payload
//! (`gpg-args`, `gpg-payload`) and answers with a fixed signature, so the
//! objects, the program's arguments and the payload are compared.

use rgit_difftest::Fixture;

/// Reports `SIG_CREATED` and prints a fixed armoured signature.
const STUB: &str = "#!/bin/sh\n\
echo \"$*\" >>gpg-args\n\
cat >gpg-payload\n\
echo '[GNUPG:] SIG_CREATED D 1 8 00 1 X' >&2\n\
printf -- '-----BEGIN PGP SIGNATURE-----\\n\\nc2ln\\n-----END PGP SIGNATURE-----\\n'\n";

/// Fails with a message on standard error.
const FAILING: &str = "#!/bin/sh\ncat >/dev/null\necho 'no secret key' >&2\nexit 2\n";

/// Exits 0 without the status line.
const SILENT: &str = "#!/bin/sh\ncat >/dev/null\nprintf 'sig\\n'\n";

fn base(init: &[&str]) -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(init)
        .write_executable("gpg-stub", STUB)
        .write_executable("gpg-failing", FAILING)
        .write_executable("gpg-silent", SILENT)
        .write(".git/info/exclude", "gpg-*\n")
        .git(["config", "gpg.program", "./gpg-stub"])
        .git(["config", "advice.detachedHead", "false"])
        .write("a", "a\n")
        .git(["add", "a"])
        .git(["commit", "-q", "-m", "A"])
        .tick()
}

/// `main`: A - B (b) - C (c); `side`: A - X (b=x), conflicting with B.
fn history() -> Fixture {
    base(&["init", "-q", "-b", "main"])
        .write("b", "b\n")
        .git(["add", "b"])
        .git(["commit", "-q", "-m", "B"])
        .tick()
        .write("c", "c\n")
        .git(["add", "c"])
        .git(["commit", "-q", "-m", "C"])
        .git(["checkout", "-q", "-b", "side", "main~2"])
        .tick()
        .write("b", "x\n")
        .git(["add", "b"])
        .git(["commit", "-q", "-m", "X"])
        .git(["checkout", "-q", "main"])
        .tick()
}

fn cases(prep: impl Fn() -> Fixture, cases: &[&[&str]]) {
    let mut failed = Vec::new();
    for args in cases {
        let mut fx = prep();
        if fx.is_skipped() {
            return;
        }
        if let Err(m) = fx.compare(*args).try_run() {
            failed.push(m.to_string());
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

/// A shell alias `t` running `script` with the binary under test.
fn script(s: &str) -> Vec<String> {
    vec!["-c".to_owned(), format!("alias.t=!{s}"), "t".to_owned()]
}

#[test]
fn commit_signs() {
    cases(
        || history().write("d", "d\n").git(["add", "d"]),
        &[
            &["commit", "-q", "-S", "-m", "D"],
            &["commit", "-q", "-SKEY", "-m", "D"],
            &["commit", "-q", "--gpg-sign=KEY", "-m", "D"],
            &["-c", "commit.gpgSign=true", "commit", "-q", "-m", "D"],
            &[
                "-c",
                "commit.gpgSign=true",
                "commit",
                "-q",
                "--no-gpg-sign",
                "-m",
                "D",
            ],
            &["-c", "user.signingKey=CFG", "commit", "-q", "-S", "-m", "D"],
            &[
                "-c",
                "user.signingKey=CFG",
                "commit",
                "-q",
                "-SOPT",
                "-m",
                "D",
            ],
            &[
                "-c",
                "gpg.openpgp.program=./gpg-stub",
                "-c",
                "gpg.program=./none",
                "commit",
                "-q",
                "-S",
                "-m",
                "D",
            ],
            &[
                "-c",
                "gpg.program=./none",
                "-c",
                "gpg.openpgp.program=./gpg-stub",
                "commit",
                "-q",
                "-S",
                "-m",
                "D",
            ],
            &[
                "-c",
                "i18n.commitEncoding=ISO-8859-1",
                "commit",
                "-q",
                "-S",
                "-m",
                "D",
            ],
            &["commit", "-q", "-S", "--amend", "--no-edit"],
            &[
                "-c",
                "gpg.program=./gpg-failing",
                "commit",
                "-q",
                "-S",
                "-m",
                "D",
            ],
            &[
                "-c",
                "gpg.program=./gpg-silent",
                "commit",
                "-q",
                "-S",
                "-m",
                "D",
            ],
            &[
                "-c",
                "gpg.program=./gpg-missing",
                "commit",
                "-q",
                "-S",
                "-m",
                "D",
            ],
        ],
    );
}

#[test]
fn rebase_signs() {
    cases(
        history,
        &[
            &["-c", "commit.gpgSign=true", "rebase", "-f", "HEAD~2"],
            &["rebase", "-f", "-S", "HEAD~2"],
            &["rebase", "-f", "--gpg-sign=KEY", "HEAD~2"],
            &[
                "-c",
                "commit.gpgSign=true",
                "rebase",
                "-f",
                "--no-gpg-sign",
                "HEAD~2",
            ],
            &["-c", "commit.gpgSign=true", "rebase", "side"],
        ],
    );
}

#[test]
fn rebase_stops_and_continues_signed() {
    let scripts = [
        // A conflict, resolved, then --continue commits with the option.
        "git -c commit.gpgSign=true rebase side; echo x >b; git add b; \
         GIT_EDITOR=: git rebase --continue; cat .git/rebase-merge/gpg_sign_opt",
        "git rebase -SKEY side; cat .git/rebase-merge/gpg_sign_opt; echo x >b; git add b; \
         GIT_EDITOR=: git rebase --continue",
        "git -c commit.gpgSign=true rebase --no-gpg-sign side; \
         test -f .git/rebase-merge/gpg_sign_opt; echo $?; echo x >b; git add b; \
         GIT_EDITOR=: git -c commit.gpgSign=true rebase --continue",
        // The signing program fails: the rebase stops at that commit.
        "git -c gpg.program=./gpg-failing -c commit.gpgSign=true rebase -f HEAD~2; \
         git rebase --continue",
    ];
    let all: Vec<Vec<String>> = scripts.iter().map(|s| script(s)).collect();
    let refs: Vec<Vec<&str>> = all
        .iter()
        .map(|a| a.iter().map(String::as_str).collect())
        .collect();
    let slices: Vec<&[&str]> = refs.iter().map(Vec::as_slice).collect();
    cases(history, &slices);
}
