// SPDX-License-Identifier: MIT
//! `archive --remote` over smart HTTP (protocol v2 to
//! `git-upload-archive`): the reference git and rgit as clients, each
//! against `git http-backend` and `rgit http-backend` behind a small CGI
//! web server. The archives must equal the local `git archive`, and the
//! errors (protocol v1, a disabled service) must equal git's.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rgit_difftest::Fixture;

mod common;
use common::servers::{isolated, start_cgi};

fn rgit() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rgit"))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn fixture() -> Fixture {
    let fx = Fixture::new().rgit(rgit()).git(["init", "-q", "src"]);
    if fx.is_skipped() {
        return fx;
    }
    fx.write("src/a", "a\n")
        .write("src/d/b", "b\n")
        .git(["-C", "src", "add", "."])
        .git(["-C", "src", "commit", "-q", "-m", "one"])
        .git(["clone", "-q", "--bare", "src", "bare.git"])
}

fn client(fx: &Fixture, program: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(program);
    isolated(fx, &mut cmd);
    cmd.args(args).current_dir(fx.work_dir()).output().unwrap()
}

#[test]
fn archive_remote_over_http() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let work = fx.work_dir();
    let git = fx.reference_git().to_path_buf();
    let expected = client(
        &fx,
        &git,
        &["-C", "bare.git", "archive", "--format=zip", "HEAD"],
    );
    assert!(expected.status.success());
    let user = vec![("REMOTE_USER".to_owned(), "user".into())];
    for (backend_name, backend) in [("git", git.clone()), ("rgit", rgit())] {
        let authenticated = start_cgi(&fx, backend.clone(), user.clone(), true);
        let anonymous = start_cgi(&fx, backend.clone(), Vec::new(), true);
        let url = format!("--remote={authenticated}/bare.git");
        let anon_url = format!("--remote={anonymous}/bare.git");
        let mut errors = Vec::new();
        for (client_name, program) in [("git", git.clone()), ("rgit", rgit())] {
            let what = format!("{client_name} client, {backend_name} backend");
            let out_file = format!("{client_name}-{backend_name}.zip");
            let out = client(
                &fx,
                &program,
                &["archive", &url, &format!("--output={out_file}"), "HEAD"],
            );
            assert!(out.status.success(), "{what}: {}", text(&out.stderr));
            let got = std::fs::read(work.join(&out_file)).unwrap();
            assert!(got == expected.stdout, "{what}: archive differs");

            let v1 = client(
                &fx,
                &program,
                &["-c", "protocol.version=1", "archive", &url, "HEAD"],
            );
            let denied = client(&fx, &program, &["archive", &anon_url, "HEAD"]);
            errors.push((
                text(&v1.stderr),
                v1.status.code(),
                text(&denied.stderr),
                denied.status.code(),
            ));
        }
        assert_eq!(errors[0], errors[1], "{backend_name} backend");
        assert_eq!(
            errors[0].0,
            "fatal: can't connect to subservice git-upload-archive\n"
        );
    }
}
