// SPDX-License-Identifier: MIT
//! `http-backend` against the reference git.
//!
//! - CGI level: the same requests (environment and body) go to `git
//!   http-backend` and `rgit http-backend`; status, headers, body, the
//!   messages on standard error and the exit code must be equal (apart
//!   from dates, `agent=` and progress; packs are compared by the
//!   clients below).
//! - Clients: the reference git and rgit clone, fetch (protocol v2 and
//!   v0, smart and dumb) and push through `rgit http-backend` behind a
//!   small CGI web server; the repositories must end up as the server's
//!   and pass `git fsck --strict`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use rgit_difftest::Fixture;

mod common;
use common::servers::{isolated, server_path, start_cgi};

fn rgit() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rgit"))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn commit(fx: Fixture, dir: &str, file: &str, content: &str) -> Fixture {
    fx.write(format!("{dir}/{file}"), content)
        .git(["-C", dir, "add", file])
        .git(["-C", dir, "commit", "-q", "-m", file])
        .tick()
}

/// `work` with three commits and an annotated tag; `repo.git`, a bare
/// clone with a pack, one loose commit and empty alternates files.
fn fixture() -> Fixture {
    let fx = Fixture::new()
        .rgit(rgit())
        .git(["init", "-q", "-b", "main", "work"]);
    if fx.is_skipped() {
        return fx;
    }
    let fx = commit(fx, "work", "a", "one\n");
    let fx = commit(fx, "work", "dir/b", "two\n").git(["-C", "work", "tag", "-a", "-m", "t", "v1"]);
    let fx = fx
        .git(["clone", "-q", "--bare", "work", "repo.git"])
        .git(["-C", "repo.git", "repack", "-q", "-a", "-d"]);
    let fx =
        commit(fx, "work", "c", "three\n").git(["-C", "work", "push", "-q", "../repo.git", "main"]);
    fx.write("repo.git/objects/info/alternates", "")
        .write("repo.git/objects/info/http-alternates", "")
        .write("work/.git/git-daemon-export-ok", "")
}

/// Run `<program> http-backend` with the CGI variables `vars` and `body`
/// on standard input.
fn cgi(fx: &Fixture, program: &Path, vars: &[(&str, String)], body: &[u8]) -> Output {
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
    let mut pipe = child.stdin.take().unwrap();
    let input = body.to_vec();
    let feeder = std::thread::spawn(move || {
        let _ = pipe.write_all(&input);
    });
    let out = child.wait_with_output().unwrap();
    let _ = feeder.join();
    out
}

/// Dates and agent strings vary; with `whole` false only the headers
/// count (a pack is made differently by each server).
fn normalize_stdout(out: &[u8], whole: bool) -> String {
    let split = out
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| p + 4)
        .unwrap_or(out.len());
    let (head, body) = out.split_at(split);
    let mut s = String::new();
    for line in text(head).split_inclusive("\r\n") {
        match line.split_once(": ") {
            Some((k @ ("Date" | "Last-Modified"), _)) => s.push_str(&format!("{k}: DATE\r\n")),
            Some(("Expires", v)) if !v.starts_with("Fri, 01 Jan 1980") => {
                s.push_str("Expires: DATE\r\n")
            }
            _ => s.push_str(line),
        }
    }
    if whole {
        let body = text(body);
        let re_agent = body
            .split("agent=")
            .enumerate()
            .map(|(i, part)| {
                if i == 0 {
                    return part.to_owned();
                }
                let end = part.find([' ', '\n', '\0']).unwrap_or(part.len());
                format!("agent=X{}", &part[end..])
            })
            .collect::<Vec<_>>()
            .join("");
        // The pkt-line length of the agent line differs with the agent
        // (after any flush packets in front of it).
        let fixed: Vec<String> = re_agent
            .split('\n')
            .map(|l| {
                if !l.contains("agent=X") {
                    return l.to_owned();
                }
                let mut pos = 0;
                while l.get(pos..pos + 4) == Some("0000") {
                    pos += 4;
                }
                match l.get(pos + 4..) {
                    Some(rest) => format!("{}XXXX{rest}", &l[..pos]),
                    None => l.to_owned(),
                }
            })
            .collect();
        s.push_str(&fixed.join("\n"));
    }
    s
}

fn normalize_stderr(err: &[u8]) -> Vec<String> {
    let mut lines: Vec<String> = text(err)
        .lines()
        .filter(|l| {
            !l.is_empty()
                && ![
                    "Enumerating objects",
                    "Counting objects",
                    "Compressing objects",
                    "Writing objects",
                    "Total ",
                ]
                .iter()
                .any(|p| l.starts_with(p))
        })
        .map(str::to_owned)
        .collect();
    lines.sort();
    lines
}

fn pkt(line: &str) -> String {
    format!("{:04x}{line}", line.len() + 4)
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

#[test]
fn cgi_equals_git() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let work = fx.work_dir();
    let root = work.display().to_string();
    let repo = format!("{root}/repo.git");
    let find = |sub: &str, ext: &str| -> String {
        let dir = work.join("repo.git").join(sub);
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(ext))
            .collect();
        names.sort();
        format!("{sub}/{}", names[0])
    };
    let pack = find("objects/pack", ".pack");
    let idx = find("objects/pack", ".idx");
    let head = text(&fx.git_stdout(["-C", "repo.git", "rev-parse", "main"]));
    let head = head.trim();
    let loose = format!("objects/{}/{}", &head[..2], &head[2..]);
    let first = text(&fx.git_stdout(["-C", "repo.git", "rev-parse", "main~1"]));
    let fetch = format!(
        "{}0000{}{}",
        pkt(&format!("want {head}\n")),
        pkt(&format!("have {}\n", first.trim())),
        pkt("done\n")
    );

    let get = |path: &str| -> Vec<(&'static str, String)> {
        vec![
            ("REQUEST_METHOD", "GET".into()),
            ("PATH_TRANSLATED", format!("{repo}/{path}")),
            ("GIT_HTTP_EXPORT_ALL", "1".into()),
        ]
    };
    let post = |svc: &str| -> Vec<(&'static str, String)> {
        vec![
            ("REQUEST_METHOD", "POST".into()),
            ("GIT_PROJECT_ROOT", root.clone()),
            ("PATH_INFO", format!("/repo.git/git-{svc}")),
            ("CONTENT_TYPE", format!("application/x-git-{svc}-request")),
            ("GIT_HTTP_EXPORT_ALL", "1".into()),
        ]
    };
    let with = |mut v: Vec<(&'static str, String)>, extra: &[(&'static str, &str)]| {
        for (k, val) in extra {
            v.retain(|(n, _)| n != k);
            v.push((k, (*val).to_owned()));
        }
        v
    };
    let without = |mut v: Vec<(&'static str, String)>, name: &str| {
        v.retain(|(n, _)| *n != name);
        v
    };

    type Case = (Vec<(&'static str, String)>, Vec<u8>, bool);
    let mut cases: Vec<Case> = vec![
        // dumb files
        (get("HEAD"), vec![], true),
        (get("info/refs"), vec![], true),
        (get("objects/info/packs"), vec![], true),
        (get("objects/info/alternates"), vec![], true),
        (get("objects/info/http-alternates"), vec![], true),
        (get(&loose), vec![], true),
        (get(&pack), vec![], true),
        (get(&idx), vec![], true),
        (
            get("objects/pack/pack-0000000000000000000000000000000000000000.idx"),
            vec![],
            true,
        ),
        (get("objects/info/foo"), vec![], true),
        (get("refs/heads/main"), vec![], true),
        (without(get("HEAD"), "GIT_HTTP_EXPORT_ALL"), vec![], true),
        (
            vec![
                ("REQUEST_METHOD", "GET".into()),
                ("PATH_TRANSLATED", format!("{root}/work/HEAD")),
            ],
            vec![],
            true,
        ),
        (
            with(get("HEAD"), &[("REQUEST_METHOD", "HEAD")]),
            vec![],
            true,
        ),
        // methods
        (
            with(get("HEAD"), &[("REQUEST_METHOD", "POST")]),
            vec![],
            true,
        ),
        (
            with(
                get("HEAD"),
                &[("REQUEST_METHOD", "PUT"), ("SERVER_PROTOCOL", "HTTP/1.1")],
            ),
            vec![],
            true,
        ),
        (
            with(get("git-upload-pack"), &[("SERVER_PROTOCOL", "HTTP/1.1")]),
            vec![],
            true,
        ),
        // environment errors
        (without(get("HEAD"), "REQUEST_METHOD"), vec![], true),
        (without(get("HEAD"), "PATH_TRANSLATED"), vec![], true),
        (
            vec![
                ("REQUEST_METHOD", "GET".into()),
                ("GIT_PROJECT_ROOT", root.clone()),
            ],
            vec![],
            true,
        ),
        (get("../nope.git/HEAD"), vec![], true),
        // smart advertisements
        (
            with(
                get("info/refs"),
                &[("QUERY_STRING", "service=git-upload-pack")],
            ),
            vec![],
            true,
        ),
        (
            with(
                get("info/refs"),
                &[
                    ("QUERY_STRING", "service=git-upload-pack"),
                    ("HTTP_GIT_PROTOCOL", "version=2"),
                ],
            ),
            vec![],
            true,
        ),
        (
            with(
                get("info/refs"),
                &[("QUERY_STRING", "service=git-receive-pack")],
            ),
            vec![],
            true,
        ),
        (
            with(
                get("info/refs"),
                &[
                    ("QUERY_STRING", "service=git-receive-pack"),
                    ("REMOTE_USER", "me"),
                ],
            ),
            vec![],
            true,
        ),
        (
            with(get("info/refs"), &[("QUERY_STRING", "service=git-foo")]),
            vec![],
            true,
        ),
        (
            with(get("info/refs"), &[("QUERY_STRING", "foo=bar")]),
            vec![],
            true,
        ),
        // service calls
        (post("upload-pack"), b"0000".to_vec(), true),
        (post("upload-pack"), fetch.clone().into_bytes(), false),
        (
            with(post("upload-pack"), &[("HTTP_CONTENT_ENCODING", "gzip")]),
            gzip(fetch.as_bytes()),
            false,
        ),
        (
            with(post("upload-pack"), &[("HTTP_CONTENT_ENCODING", "x-gzip")]),
            gzip(fetch.as_bytes()),
            false,
        ),
        (
            with(post("upload-pack"), &[("HTTP_CONTENT_ENCODING", "gzip")]),
            gzip(fetch.as_bytes())[..10].to_vec(),
            true,
        ),
        (
            with(post("upload-pack"), &[("HTTP_CONTENT_ENCODING", "gzip")]),
            fetch.clone().into_bytes(),
            true,
        ),
        (
            with(post("upload-pack"), &[("CONTENT_LENGTH", "abc")]),
            vec![],
            true,
        ),
        (
            with(post("upload-pack"), &[("CONTENT_LENGTH", "10")]),
            fetch.clone().into_bytes(),
            true,
        ),
        (
            with(
                post("upload-pack"),
                &[
                    ("CONTENT_LENGTH", "5000"),
                    ("GIT_HTTP_MAX_REQUEST_BUFFER", "1k"),
                ],
            ),
            fetch.clone().into_bytes(),
            true,
        ),
        (
            with(post("upload-pack"), &[("GIT_HTTP_MAX_REQUEST_BUFFER", "x")]),
            vec![],
            true,
        ),
        (
            with(post("upload-pack"), &[("CONTENT_TYPE", "text/plain")]),
            vec![],
            true,
        ),
        (post("receive-pack"), b"0000".to_vec(), true),
        (
            with(post("receive-pack"), &[("REMOTE_USER", "me")]),
            b"0000".to_vec(),
            true,
        ),
        (
            with(
                post("receive-pack"),
                &[("PATH_INFO", "/repo.git/../repo.git/git-receive-pack")],
            ),
            vec![],
            true,
        ),
    ];
    // http.getanyfile / http.uploadpack off: checked after the cases above.
    let off: Vec<Case> = vec![
        (get("HEAD"), vec![], true),
        (get("info/refs"), vec![], true),
        (get(&pack), vec![], true),
        (
            with(
                get("info/refs"),
                &[("QUERY_STRING", "service=git-upload-pack")],
            ),
            vec![],
            true,
        ),
        (post("upload-pack"), b"0000".to_vec(), true),
    ];

    let mut failures = Vec::new();
    let mut check = |cases: &mut Vec<Case>| {
        for (vars, body, whole) in cases.drain(..) {
            let g = cgi(&fx, fx.reference_git(), &vars, &body);
            let r = cgi(&fx, &rgit(), &vars, &body);
            let (gs, rs) = (
                normalize_stdout(&g.stdout, whole),
                normalize_stdout(&r.stdout, whole),
            );
            let (ge, re) = (normalize_stderr(&g.stderr), normalize_stderr(&r.stderr));
            if gs != rs || ge != re || g.status.code() != r.status.code() {
                failures.push(format!(
                    "{vars:?}\n--- git ({:?})\n{gs}{ge:?}\n--- rgit ({:?})\n{rs}{re:?}",
                    g.status.code(),
                    r.status.code()
                ));
            }
        }
    };
    check(&mut cases);
    for name in ["http.getanyfile", "http.uploadpack"] {
        let out = fx.git_output(["-C", "repo.git", "config", name, "false"]);
        assert!(out.status.success());
    }
    let mut off = off;
    check(&mut off);
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// CGI variables and a request body.
type ArchiveCase = (Vec<(&'static str, String)>, Vec<u8>);

/// `POST .../git-upload-archive` (`archive --remote` over protocol v2):
/// enabled by `http.uploadarchive`, by default for authenticated users
/// only; `info/refs?service=git-upload-archive` as git answers it.
#[test]
fn upload_archive_cgi_equals_git() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let root = fx.work_dir().display().to_string();
    let request = format!(
        "{}{}0000",
        pkt("argument --format=tar\n"),
        pkt("argument HEAD\n")
    );
    let post = |extra: &[(&'static str, &str)]| -> Vec<(&'static str, String)> {
        let mut v: Vec<(&'static str, String)> = vec![
            ("REQUEST_METHOD", "POST".into()),
            ("GIT_PROJECT_ROOT", root.clone()),
            ("PATH_INFO", "/repo.git/git-upload-archive".into()),
            (
                "CONTENT_TYPE",
                "application/x-git-upload-archive-request".into(),
            ),
            ("GIT_HTTP_EXPORT_ALL", "1".into()),
        ];
        for (k, val) in extra {
            v.retain(|(n, _)| n != k);
            v.push((k, (*val).to_owned()));
        }
        v
    };
    let user = ("REMOTE_USER", "user");
    let v2 = ("HTTP_GIT_PROTOCOL", "version=2");
    let info = |extra: &[(&'static str, &str)]| {
        let mut v = post(&[
            ("REQUEST_METHOD", "GET"),
            ("PATH_INFO", "/repo.git/info/refs"),
            ("QUERY_STRING", "service=git-upload-archive"),
        ]);
        v.retain(|(n, _)| *n != "CONTENT_TYPE");
        for (k, val) in extra {
            v.push((k, (*val).to_owned()));
        }
        v
    };
    let bad = format!("{}0000", pkt("argument nope\n"));
    let mut cases: Vec<ArchiveCase> = vec![
        (post(&[user, v2]), request.clone().into_bytes()),
        (post(&[user]), request.clone().into_bytes()),
        (post(&[v2]), request.clone().into_bytes()),
        (post(&[user, v2]), bad.into_bytes()),
        (
            post(&[user, v2, ("CONTENT_TYPE", "text/plain")]),
            request.clone().into_bytes(),
        ),
        (
            post(&[user, v2, ("REQUEST_METHOD", "GET")]),
            request.clone().into_bytes(),
        ),
        (info(&[user, v2]), vec![]),
        (info(&[user]), vec![]),
        (info(&[v2]), vec![]),
    ];
    // The first request is served: an archive follows the `ACK`.
    let served = cgi(&fx, &rgit(), &cases[0].0, &cases[0].1);
    assert!(
        text(&served.stdout).contains("0008ACK\n0000") && served.stdout.len() > 1024,
        "{}",
        text(&served.stdout)
    );
    let mut failures = Vec::new();
    let mut check = |cases: &mut Vec<ArchiveCase>| {
        for (vars, body) in cases.drain(..) {
            let g = cgi(&fx, fx.reference_git(), &vars, &body);
            let r = cgi(&fx, &rgit(), &vars, &body);
            let (gs, rs) = (
                normalize_stdout(&g.stdout, true),
                normalize_stdout(&r.stdout, true),
            );
            let (ge, re) = (normalize_stderr(&g.stderr), normalize_stderr(&r.stderr));
            if gs != rs || ge != re || g.status.code() != r.status.code() {
                failures.push(format!(
                    "{vars:?}\n--- git ({:?})\n{gs}{ge:?}\n--- rgit ({:?})\n{rs}{re:?}",
                    g.status.code(),
                    r.status.code()
                ));
            }
        }
    };
    check(&mut cases);
    for (value, extra) in [("true", None), ("false", Some(user))] {
        let out = fx.git_output(["-C", "repo.git", "config", "http.uploadarchive", value]);
        assert!(out.status.success());
        let mut vars = vec![v2];
        vars.extend(extra);
        check(&mut vec![(post(&vars), request.clone().into_bytes())]);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Run `program` (a git client) in `dir`.
fn client(fx: &Fixture, program: &Path, args: &[&str], env: &[(&str, &str)], dir: &Path) -> Output {
    let mut cmd = Command::new(program);
    isolated(fx, &mut cmd);
    cmd.args(args)
        .envs(env.iter().copied())
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// References and `git fsck --strict` verdict of a repository.
fn state(fx: &Fixture, repo: &Path) -> String {
    let git = fx.reference_git();
    let refs = client(
        fx,
        git,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
        &[],
        repo,
    );
    let fsck = client(fx, git, &["fsck", "--strict", "--no-dangling"], &[], repo);
    format!(
        "{}fsck: {:?} {}",
        text(&refs.stdout),
        fsck.status.code(),
        text(&fsck.stderr)
    )
}

/// One client call: arguments, environment, the repository it leaves.
type Step<'a> = (Vec<&'a str>, Vec<(&'a str, &'a str)>, &'a str);

#[test]
fn clients_through_rgit_backend() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let fx = fx
        .git(["-C", "repo.git", "config", "http.receivepack", "true"])
        .git(["-C", "repo.git", "config", "core.logAllRefUpdates", "true"]);
    let work = fx.work_dir();
    let base = start_cgi(&fx, rgit(), Vec::new(), true);
    let url = format!("{base}/repo.git");
    let server_main = || text(&fx.git_stdout(["-C", "repo.git", "rev-parse", "main"]));
    for (name, program) in [("git", fx.reference_git().to_path_buf()), ("rgit", rgit())] {
        let dir = work.join(format!("client-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        let mut steps: Vec<Step> = vec![
            (vec!["clone", "-q", &url, "v2"], vec![], "v2"),
            (
                vec!["-c", "protocol.version=0", "clone", "-q", &url, "v0"],
                vec![],
                "v0",
            ),
            (
                vec!["clone", "-q", "--depth", "1", &url, "shallow"],
                vec![],
                "shallow",
            ),
        ];
        if name == "git" {
            // The dumb protocol: rgit's client does not speak it (yet).
            steps.push((
                vec!["clone", "-q", &url, "dumb"],
                vec![("GIT_SMART_HTTP", "0")],
                "dumb",
            ));
        }
        for (args, env, repo) in &steps {
            let out = client(&fx, &program, args, env, &dir);
            assert!(
                out.status.success(),
                "{name}: {args:?}: {}",
                text(&out.stderr)
            );
            let st = state(&fx, &dir.join(repo));
            assert!(st.contains("fsck: Some(0)"), "{name}: {args:?}: {st}");
            assert!(
                st.contains(&format!(
                    "refs/remotes/origin/main {}",
                    server_main().trim()
                )),
                "{name}: {args:?}: {st}"
            );
        }

        // Push a new commit and a new branch, then fetch them back.
        let v2 = dir.join("v2");
        std::fs::write(v2.join(format!("{name}.txt")), "pushed\n").unwrap();
        for args in [vec!["add", "."], vec!["commit", "-q", "-m", "pushed"]] {
            let out = client(&fx, fx.reference_git(), &args, &[], &v2);
            assert!(out.status.success(), "{args:?}: {}", text(&out.stderr));
        }
        let branch = format!("HEAD:refs/heads/from-{name}");
        for args in [
            vec!["push", "-q", "origin", "HEAD:main"],
            vec!["push", "-q", "origin", &branch],
        ] {
            let out = client(&fx, &program, &args, &[], &v2);
            assert!(
                out.status.success(),
                "{name}: {args:?}: {}",
                text(&out.stderr)
            );
        }
        let pushed = text(&fx.git_stdout(["-C", v2.to_str().unwrap(), "rev-parse", "HEAD"]));
        assert_eq!(server_main(), pushed, "{name}: push did not move main");
        let reflog = text(&fx.git_stdout([
            "-C",
            "repo.git",
            "log",
            "-g",
            "-1",
            "--format=%gn %ge",
            "main",
        ]));
        // The pusher is named after REMOTE_USER (here: none).
        assert_eq!(
            reflog.trim(),
            "anonymous anonymous@http.127.0.0.1",
            "{name}"
        );

        let v0 = dir.join("v0");
        for args in [
            vec!["fetch", "-q", "origin"],
            vec!["-c", "protocol.version=0", "fetch", "-q", "origin"],
        ] {
            let out = client(&fx, &program, &args, &[], &v0);
            assert!(
                out.status.success(),
                "{name}: {args:?}: {}",
                text(&out.stderr)
            );
        }
        let st = state(&fx, &v0);
        assert!(st.contains("fsck: Some(0)"), "{name}: {st}");
        assert!(
            st.contains(&format!(
                "refs/remotes/origin/from-{name} {}",
                pushed.trim()
            )),
            "{name}: {st}"
        );
    }
}
