// SPDX-License-Identifier: MIT
//! Signature verification (`verify-commit`, `verify-tag`, `tag -v`,
//! `log --show-signature`, the `%G` placeholders, `merge
//! --verify-signatures`) and signing of tags, merges, `commit-tree`,
//! `cherry-pick`/`revert` and SSH signatures against the reference git.
//! The programs are stubs in the work tree: `gpg-verify` answers with
//! the status lines in the file `status`, `ssh-stub` acts like
//! `ssh-keygen -Y`. No real key is used.

use std::path::Path;
use std::process::Command;

use rgit_difftest::{Fixture, Normalizer};

/// Signs with a fixed signature (reporting `SIG_CREATED`) and answers
/// `--verify` with the status lines in the file `status`, the text in
/// `human` and the exit code in `code`, recording the options (not the
/// temporary file) in `verify-args`.
const VERIFY_STUB: &str = "#!/bin/sh\n\
case \"$*\" in\n\
*-bsau*) cat >/dev/null; echo '[GNUPG:] SIG_CREATED D 1 8 00 1 X' >&2;\n\
  printf -- '-----BEGIN PGP SIGNATURE-----\\n\\nc2ln\\n-----END PGP SIGNATURE-----\\n';;\n\
*) for a; do case \"$a\" in /*) ;; *) printf '%s ' \"$a\" >>verify-args;; esac; done; echo >>verify-args;\n\
  cat >/dev/null; cat status 2>/dev/null; cat human >&2 2>/dev/null; exit $(cat code 2>/dev/null || echo 0);;\n\
esac\n";

/// Fails with a message on standard error.
const FAILING: &str = "#!/bin/sh\ncat >/dev/null\necho 'no secret key' >&2\nexit 2\n";

/// Status lines of gpg for the verification cases.
const STATUSES: &[(&str, &str)] = &[
    (
        "good-ultimate",
        "[GNUPG:] NEWSIG\n[GNUPG:] GOODSIG 13B6F51ECDDE430D C O Mitter <c@e>\n\
         [GNUPG:] VALIDSIG 73D7 2026-11-22 1 0 4 0 17 2 00 73D7\n[GNUPG:] TRUST_ULTIMATE 0 pgp\n",
    ),
    (
        "good-undefined",
        "[GNUPG:] NEWSIG\n[GNUPG:] GOODSIG 65A0 Eris <d@n>\n\
         [GNUPG:] VALIDSIG F836 2026-11-22 1 0 4 0 1 8 00 D4BE\n[GNUPG:] TRUST_UNDEFINED 0 pgp\n",
    ),
    (
        "good-marginal",
        "[GNUPG:] NEWSIG\n[GNUPG:] GOODSIG K1 N <e>\n[GNUPG:] TRUST_MARGINAL 0 pgp\n",
    ),
    ("bad", "[GNUPG:] NEWSIG\n[GNUPG:] BADSIG K1 N <e>\n"),
    (
        "expsig",
        "[GNUPG:] NEWSIG\n[GNUPG:] EXPSIG K1 N <e>\n[GNUPG:] TRUST_FULLY 0\n",
    ),
    (
        "expkey",
        "[GNUPG:] NEWSIG\n[GNUPG:] EXPKEYSIG K1 N <e>\n[GNUPG:] TRUST_FULLY 0\n",
    ),
    (
        "revoked",
        "[GNUPG:] NEWSIG\n[GNUPG:] REVKEYSIG K1 N <e>\n[GNUPG:] TRUST_FULLY 0\n",
    ),
    (
        "errsig",
        "[GNUPG:] NEWSIG\n[GNUPG:] ERRSIG K1 17 2 00 1 9 -\n",
    ),
    (
        "double",
        "[GNUPG:] NEWSIG\n[GNUPG:] GOODSIG K1 N <e>\n[GNUPG:] NEWSIG\n[GNUPG:] BADSIG K2 M <f>\n",
    ),
    (
        "no-newsig",
        "[GNUPG:] GOODSIG K1 N <e>\n[GNUPG:] TRUST_FULLY 0\n",
    ),
    ("empty", ""),
];

fn base() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write_executable("gpg-verify", VERIFY_STUB)
        .write_executable("gpg-failing", FAILING)
        .write(".git/info/exclude", "*\n")
        .git(["config", "gpg.program", "./gpg-verify"])
        .git(["config", "advice.detachedHead", "false"])
        .git(["commit", "-q", "--allow-empty", "-m", "A"])
        .tick()
}

/// `main`: A (unsigned) - S (signed) - merge of the signed tag `mt` (on
/// `side`); tag `st` signed (S), `at` annotated, `lt` lightweight.
fn signed_history() -> Fixture {
    base()
        .write(
            "human",
            "gpg: Good signature from \"C O Mitter\"\ngpg: second line\n",
        )
        .git(["commit", "-q", "--allow-empty", "-S", "-m", "S"])
        .tick()
        .git(["tag", "-s", "-m", "signed tag", "st"])
        .git(["tag", "-a", "-m", "annotated", "at"])
        .git(["tag", "lt"])
        .git(["checkout", "-q", "-b", "side", "HEAD~1"])
        .git(["commit", "-q", "--allow-empty", "-m", "side"])
        .git(["tag", "-s", "-m", "merged tag", "mt"])
        .git(["checkout", "-q", "main"])
        .tick()
        .git(["merge", "-q", "--no-ff", "--no-edit", "mt"])
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

#[test]
fn verification_results() {
    let commands: &[&[&str]] = &[
        &["verify-commit", "HEAD~1"],
        &["verify-commit", "-v", "--raw", "HEAD~1"],
        &[
            "-c",
            "gpg.minTrustLevel=marginal",
            "verify-commit",
            "HEAD~1",
        ],
        &["-c", "gpg.minTrustLevel=Fully", "verify-commit", "HEAD~1"],
        &[
            "log",
            "-1",
            "--format=%G?|%GT|%GK|%GS|%GF|%GP|%GG",
            "HEAD~1",
        ],
        &["log", "-2", "--show-signature"],
        &["log", "--oneline", "--show-signature", "-3"],
        &["log", "-1", "--show-signature", "--color=always", "HEAD~1"],
        &["-c", "log.showSignature=true", "log", "-1", "HEAD~1"],
        &["verify-tag", "st"],
        &["verify-tag", "-v", "--raw", "st"],
        &["verify-tag", "--format=%(refname) %(tag)", "st"],
        &["tag", "-v", "st"],
        &["merge", "--ff-only", "--verify-signatures", "HEAD~1"],
        &[
            "-c",
            "gpg.minTrustLevel=undefined",
            "merge",
            "--verify-signatures",
            "HEAD~1",
        ],
    ];
    for (i, (name, status)) in STATUSES.iter().enumerate() {
        // A failing exit only for the first (good) status.
        let codes: &[&str] = if i == 0 { &["0", "1"] } else { &["0"] };
        for code in codes {
            let mut failed = Vec::new();
            for args in commands {
                let mut fx = signed_history()
                    .write("status", status)
                    .write("code", format!("{code}\n"));
                if fx.is_skipped() {
                    return;
                }
                if let Err(m) = fx.compare(*args).try_run() {
                    failed.push(format!("[{name} exit {code}] {m}"));
                }
            }
            assert!(failed.is_empty(), "{}", failed.join("\n\n"));
        }
    }
}

#[test]
fn verification_errors() {
    cases(
        || signed_history().write("status", STATUSES[0].1),
        &[
            &["verify-commit"],
            &["verify-commit", "-h"],
            &["verify-tag", "-h"],
            &["verify-commit", "HEAD"],
            &["verify-commit", "-v", "main~2"],
            &["verify-commit", "nonexistent", "HEAD~1"],
            &["verify-commit", "HEAD^{tree}"],
            &["verify-commit", "st"],
            &["verify-tag", "at"],
            &["verify-tag", "-v", "at"],
            &["verify-tag", "lt"],
            &["verify-tag", "nonexistent"],
            &["verify-tag", "--format=%(rest)", "st"],
            &["tag", "-v", "at"],
            &["tag", "-v", "lt"],
            &["tag", "-v", "nope"],
            &["-c", "gpg.minTrustLevel=bogus", "verify-commit", "HEAD~1"],
            &["-c", "gpg.minTrustLevel=bogus", "verify-commit", "main~2"],
            &["-c", "gpg.format=bogus", "verify-tag", "st"],
            &["-c", "gpg.program=./gpg-missing", "verify-commit", "HEAD~1"],
            &[
                "-c",
                "gpg.program=./gpg-missing",
                "log",
                "-1",
                "--format=%G?",
                "HEAD~1",
            ],
            &["merge", "--verify-signatures", "main~2"],
        ],
    );
}

#[test]
fn signing_tags_and_merges() {
    cases(
        || signed_history().write("status", STATUSES[0].1),
        &[
            &["tag", "-s", "-m", "msg", "new"],
            &["tag", "-u", "KEY", "-m", "msg", "new"],
            &["-c", "tag.gpgSign=true", "tag", "-m", "msg", "new"],
            &[
                "-c",
                "tag.gpgSign=true",
                "tag",
                "--no-sign",
                "-m",
                "msg",
                "new",
            ],
            &["-c", "user.signingKey=CFG", "tag", "-s", "-m", "msg", "new"],
            &[
                "-c",
                "gpg.program=./gpg-failing",
                "tag",
                "-s",
                "-m",
                "msg",
                "new",
            ],
            &["merge", "-S", "--no-ff", "--no-edit", "side"],
            &["merge", "-SKEY", "--no-ff", "--no-edit", "side"],
            &[
                "-c",
                "commit.gpgSign=true",
                "merge",
                "--no-ff",
                "--no-edit",
                "side",
            ],
            &[
                "-c",
                "commit.gpgSign=true",
                "merge",
                "--no-gpg-sign",
                "--no-ff",
                "--no-edit",
                "side",
            ],
            &[
                "-c",
                "gpg.program=./gpg-failing",
                "merge",
                "-S",
                "--no-ff",
                "--no-edit",
                "side",
            ],
            &["commit-tree", "-S", "-m", "x", "HEAD^{tree}"],
            &["commit-tree", "--gpg-sign=KEY", "-m", "x", "HEAD^{tree}"],
            &[
                "-c",
                "commit.gpgSign=true",
                "commit-tree",
                "-m",
                "x",
                "HEAD^{tree}",
            ],
            &[
                "-c",
                "gpg.program=./gpg-failing",
                "commit-tree",
                "-S",
                "-m",
                "x",
                "HEAD^{tree}",
            ],
            &["cherry-pick", "-S", "side"],
            &["-c", "commit.gpgSign=true", "cherry-pick", "side"],
            &[
                "-c",
                "commit.gpgSign=true",
                "cherry-pick",
                "--no-gpg-sign",
                "side",
            ],
            &["revert", "-S", "--no-edit", "HEAD~1"],
        ],
    );
}

/// X.509: the format comes from the signature; `gpgsm` gets no
/// `--keyid-format`.
#[test]
fn x509_signatures() {
    let stub = VERIFY_STUB
        .replace("BEGIN PGP SIGNATURE", "BEGIN SIGNED MESSAGE")
        .replace("END PGP SIGNATURE", "END SIGNED MESSAGE");
    cases(
        || {
            signed_history()
                .write("status", STATUSES[0].1)
                .write_executable("gpg-x509", &stub)
                .git(["config", "gpg.format", "x509"])
                .git(["config", "gpg.x509.program", "./gpg-x509"])
                .git(["commit", "-q", "--allow-empty", "-S", "-m", "X"])
        },
        &[
            &["verify-commit", "--raw", "HEAD"],
            &["log", "-1", "--format=%G?|%GT|%GK|%GS", "HEAD"],
            &["tag", "-s", "-m", "x509", "new"],
            &[
                "-c",
                "gpg.x509.program=./gpg-missing",
                "verify-commit",
                "HEAD",
            ],
        ],
    );
}

/// A stub of `ssh-keygen`: signs with a fixed signature, `find-principals`
/// prints the file `principals` (exit code in `fp-code`), `verify`
/// succeeds for the principal in `good-principal`, `check-novalidate`
/// reports an untrusted good signature. Arguments are recorded in
/// `ssh-args` (temporary file names as `FILE`).
const SSH_STUB: &str = "#!/bin/sh\n\
for a; do case \"$a\" in /*) printf 'FILE ' >>ssh-args;; *) printf '%s ' \"$a\" >>ssh-args;; esac; done; echo >>ssh-args\n\
case \"$2\" in\n\
sign) for f; do :; done; cat \"$f\" >ssh-payload;\n\
  printf -- '-----BEGIN SSH SIGNATURE-----\\nc3No\\n-----END SSH SIGNATURE-----\\n' >\"$f.sig\";;\n\
find-principals) cat principals 2>/dev/null; echo 'fp-err' >&2; exit $(cat fp-code 2>/dev/null || echo 0);;\n\
verify) cat >/dev/null; p=; prev=; for a; do [ \"$prev\" = -I ] && p=$a; prev=$a; done;\n\
  if [ \"$p\" = \"$(cat good-principal 2>/dev/null)\" ]; then\n\
    echo \"Good \\\"git\\\" signature for $p with ED25519 key SHA256:abc\"; exit 0; fi;\n\
  echo 'Signature verification failed: incorrect signature' >&2; exit 1;;\n\
check-novalidate) cat >/dev/null; echo 'Good \"git\" signature with ED25519 key SHA256:xyz'; exit 0;;\n\
esac\n";

fn ssh_history() -> Fixture {
    base()
        .write_executable("ssh-stub", SSH_STUB)
        .git(["config", "gpg.format", "ssh"])
        .git(["config", "gpg.ssh.program", "./ssh-stub"])
        .git(["config", "user.signingKey", "/keys/id_test"])
        .write("allowed", "p1 ssh-ed25519 AAAA\n")
        .git(["commit", "-q", "--allow-empty", "-S", "-m", "S"])
        .tick()
        .git(["tag", "-s", "-m", "ssh tag", "st"])
}

#[test]
fn ssh_signatures() {
    let configs: &[(&str, &str, &str)] = &[
        ("p1\np2\n", "0", "p2"),
        ("p1\n", "0", "p1"),
        ("p1\n", "0", "nobody"),
        ("", "1", "p1"),
        ("", "0", "p1"),
    ];
    let allowed = "gpg.ssh.allowedSignersFile=allowed";
    let commands: &[&[&str]] = &[
        &["verify-commit", "HEAD"],
        &["-c", allowed, "verify-commit", "-v", "HEAD"],
        &[
            "-c",
            allowed,
            "-c",
            "gpg.minTrustLevel=ultimate",
            "verify-commit",
            "HEAD",
        ],
        &[
            "-c",
            allowed,
            "log",
            "-1",
            "--format=%G?|%GT|%GK|%GS|%GF|%GP",
        ],
        &["-c", allowed, "log", "-1", "--show-signature"],
        &["-c", allowed, "verify-tag", "--raw", "st"],
        &[
            "-c",
            allowed,
            "-c",
            "gpg.ssh.revocationFile=allowed",
            "verify-commit",
            "HEAD",
        ],
        &[
            "-c",
            allowed,
            "-c",
            "gpg.ssh.revocationFile=missing",
            "verify-commit",
            "HEAD",
        ],
    ];
    for (principals, code, good) in configs {
        let mut failed = Vec::new();
        for args in commands {
            let mut fx = ssh_history()
                .write("principals", principals)
                .write("fp-code", format!("{code}\n"))
                .write("good-principal", format!("{good}\n"));
            if fx.is_skipped() {
                return;
            }
            if let Err(m) = fx.compare(*args).try_run() {
                failed.push(format!("[{principals:?} {code} {good}] {m}"));
            }
        }
        assert!(failed.is_empty(), "{}", failed.join("\n\n"));
    }
}

#[test]
fn ssh_signing_keys() {
    let commit = |key: &'static str| -> Vec<&'static str> {
        vec!["commit", "-q", "--allow-empty", key, "-m", "x"]
    };
    let a = commit("-S");
    let b = commit("-S/other/key");
    let c = commit("-Sssh-ed25519 AAAA literal");
    let mut d = vec!["-c", "user.signingKey=key::ssh-ed25519 AAAA conf"];
    d.extend(commit("-S"));
    let mut e = vec!["-c", "user.signingKey="];
    e.extend(commit("-S"));
    cases(ssh_history, &[&a, &b, &c, &d, &e]);

    // Without a key.
    let mut f = vec![
        "-c",
        "gpg.ssh.defaultKeyCommand=echo key::ssh-ed25519 AAAA cmd",
    ];
    f.extend(commit("-S"));
    let mut g = vec!["-c", "gpg.ssh.defaultKeyCommand=echo nokey"];
    g.extend(commit("-S"));
    let mut h = vec!["-c", "gpg.ssh.defaultKeyCommand=false"];
    h.extend(commit("-S"));
    cases(
        || ssh_history().git(["config", "--unset", "user.signingKey"]),
        &[&a, &f, &g, &h],
    );
}

/// Whether `program` can be run.
fn have(program: &str) -> bool {
    Command::new(program).arg("--version").output().is_ok()
}

/// Runs `program` in `dir`; panics when it fails.
fn run_in(dir: &Path, program: &str, args: &[&str], env: &[(&str, &Path)]) {
    let out = Command::new(program)
        .args(args)
        .current_dir(dir)
        .envs(env.iter().map(|(k, v)| (*k, *v)))
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("{program}: {e}"));
    assert!(
        out.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Real SSH signatures with throwaway Ed25519 keys (generated in the
/// fixture, never the user's keys). Ed25519 signatures are
/// deterministic, so git and rgit create identical objects.
#[test]
fn ssh_real_keys() {
    if !have("ssh-keygen") {
        return;
    }
    let prep = || {
        let fx = base();
        if fx.is_skipped() {
            return fx;
        }
        let dir = fx.work_dir();
        for (key, comment) in [("key1", "k1"), ("key2", "k2")] {
            let args = ["-q", "-t", "ed25519", "-N", "", "-C", comment, "-f", key];
            run_in(&dir, "ssh-keygen", &args, &[]);
        }
        let pubkey = std::fs::read_to_string(dir.join("key1.pub")).expect("key1.pub");
        let key1 = dir.join("key1").display().to_string();
        fx.write("allowed", format!("\"principal one\" {pubkey}"))
            .git(["config", "gpg.format", "ssh"])
            .git(["config", "user.signingKey", &key1])
            .git(["config", "gpg.ssh.allowedSignersFile", "allowed"])
            .git(["commit", "-q", "--allow-empty", "-S", "-m", "signed"])
            .tick()
            .git([
                "commit",
                "-q",
                "--allow-empty",
                "-S./key2",
                "-m",
                "untrusted",
            ])
            .tick()
    };
    cases(
        prep,
        &[
            &["commit", "-q", "--allow-empty", "-S", "-m", "x"],
            &["tag", "-s", "-m", "x", "t"],
            &["merge", "-S", "--no-ff", "--no-edit", "HEAD~1"],
            &["verify-commit", "HEAD~1"],
            &["verify-commit", "--raw", "HEAD"],
            &["log", "-2", "--format=%G?|%GT|%GK|%GS|%GF|%GP"],
            &["log", "-2", "--show-signature"],
        ],
    );
}

/// Kills the gpg-agent of a throwaway `GNUPGHOME` when dropped.
struct Agent(std::path::PathBuf);

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = Command::new("gpgconf")
            .args(["--kill", "all"])
            .env("GNUPGHOME", &self.0)
            .output();
    }
}

/// Real OpenPGP signatures with a key generated in a throwaway
/// `GNUPGHOME`: commits and tags signed by git verify with rgit and the
/// other way round.
#[test]
fn openpgp_real_keys() {
    if !have("gpg") || !have("gpgconf") {
        return;
    }
    let fx = base();
    if fx.is_skipped() {
        return;
    }
    let home = fx.home_dir().join("gnupg");
    std::fs::create_dir_all(&home).expect("gnupg home");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700));
    }
    let _agent = Agent(home.clone());
    let env = [("GNUPGHOME", home.as_path())];
    let args = [
        "--batch",
        "--pinentry-mode",
        "loopback",
        "--passphrase",
        "",
        "--quick-gen-key",
        "Cy Committer <cy@example.com>",
        "ed25519",
        "sign",
        "never",
    ];
    run_in(&fx.work_dir(), "gpg", &args, &env);
    run_in(&fx.work_dir(), "gpg", &["--batch", "--check-trustdb"], &env);
    let prep = || {
        Fixture::new()
            .rgit(env!("CARGO_BIN_EXE_rgit"))
            .env("GNUPGHOME", &home)
            .git(["init", "-q", "-b", "main"])
            .git(["commit", "-q", "--allow-empty", "-S", "-m", "signed"])
            .tick()
            .git(["tag", "-s", "-m", "tag", "st"])
    };
    let dates = || Normalizer::regex("Signature made .*", "Signature made DATE");
    let verify: &[&[&str]] = &[
        &["verify-commit", "HEAD"],
        &["verify-tag", "st"],
        &["log", "-1", "--format=%G?|%GT|%GK|%GS|%GF|%GP"],
        &["log", "-1", "--show-signature"],
    ];
    let mut failed = Vec::new();
    for args in verify {
        let mut fx = prep();
        if let Err(m) = fx.compare(*args).normalize(dates()).try_run() {
            failed.push(m.to_string());
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));

    // Signed by rgit, verified by git.
    let fx = prep();
    let rgit = env!("CARGO_BIN_EXE_rgit");
    for args in [
        &["commit", "-q", "--allow-empty", "-S", "-m", "by rgit"][..],
        &["tag", "-s", "-m", "by rgit", "rt"],
    ] {
        let out = Command::new(rgit)
            .args(args)
            .current_dir(fx.work_dir())
            .env("GNUPGHOME", &home)
            .env("HOME", fx.home_dir())
            .env("XDG_CONFIG_HOME", fx.home_dir())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "Cy Committer")
            .env("GIT_COMMITTER_EMAIL", "cy@example.com")
            .output()
            .expect("rgit");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    for args in [&["verify-commit", "HEAD"][..], &["verify-tag", "rt"]] {
        let out = fx.git_output(args);
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
