// SPDX-License-Identifier: MIT
//! `send-email`, `imap-send` and `request-pull` end to end. No mail
//! leaves the machine: messages go to a sendmail-compatible script, to
//! `--dry-run`, or to fake SMTP / IMAP servers on localhost.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use rgit_difftest::{Check, Fixture, Normalizer};

fn rgit(home: &Path, dir: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_rgit"));
    c.current_dir(dir)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_EDITOR", ":")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .stdin(Stdio::null());
    c
}

fn run(home: &Path, dir: &Path, args: &[&str]) -> Output {
    rgit(home, dir).args(args).output().unwrap()
}

fn ok(home: &Path, dir: &Path, args: &[&str]) -> String {
    let o = run(home, dir, args);
    assert!(
        o.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

struct Repo {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    dir: PathBuf,
}

/// A repository with two commits and a fake sendmail that records its
/// arguments and input into `commandlineN` / `msgtxtN`.
fn repo() -> Repo {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let dir = tmp.path().join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    ok(&home, &dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("file"), "one\n").unwrap();
    ok(&home, &dir, &["add", "file"]);
    ok(&home, &dir, &["commit", "-q", "-m", "First."]);
    std::fs::write(dir.join("file"), "one\ntwo\n").unwrap();
    ok(&home, &dir, &["commit", "-q", "-a", "-s", "-m", "Second."]);
    let script = dir.join("fake.sendmail");
    std::fs::write(
        &script,
        "#!/bin/sh\nn=1\nwhile test -f commandline$n; do n=$((n+1)); done\n\
         for a; do echo \"!$a!\"; done >commandline$n\ncat >msgtxt$n\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    ok(&home, &dir, &["config", "sendemail.confirm", "never"]);
    Repo {
        _tmp: tmp,
        home,
        dir,
    }
}

impl Repo {
    fn sendmail(&self) -> String {
        format!("--smtp-server={}", self.dir.join("fake.sendmail").display())
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.join(name)).unwrap_or_default()
    }
}

#[test]
fn help_outside_a_repository() {
    let tmp = tempfile::tempdir().unwrap();
    for cmd in ["send-email", "request-pull", "imap-send"] {
        let o = run(tmp.path(), tmp.path(), &[cmd, "-h"]);
        assert!(matches!(o.status.code(), Some(0 | 129)), "{cmd}");
        assert!(
            String::from_utf8_lossy(&o.stdout).contains(&format!("git {cmd}")),
            "{cmd}"
        );
    }
}

#[test]
fn sends_through_a_sendmail_program_with_threading() {
    let r = repo();
    let files = ok(&r.home, &r.dir, &["format-patch", "-2", "-o", "out"]);
    let files: Vec<&str> = files.lines().collect();
    let mut args = vec![
        "send-email",
        "--from=Example <from@example.com>",
        "--to=to@example.com",
        "--cc=Some One <cc@example.com>",
        "--in-reply-to=<start@example.com>",
    ];
    let server = r.sendmail();
    args.push(&server);
    args.extend(files.iter().copied());
    let out = ok(&r.home, &r.dir, &args);
    assert!(out.contains("OK. Log says:"), "{out}");
    assert!(out.contains("(body) Adding cc: C O Mitter <committer@example.com>"));

    assert_eq!(
        r.read("commandline1"),
        "!-i!\n!to@example.com!\n!cc@example.com!\n!author@example.com!\n"
    );
    let m1 = r.read("msgtxt1");
    let m2 = r.read("msgtxt2");
    assert!(
        m1.starts_with("From: Example <from@example.com>\nTo: to@example.com\n"),
        "{m1}"
    );
    assert!(m1.contains("Subject: [PATCH 1/2] First.\n"));
    assert!(m1.contains("In-Reply-To: <start@example.com>\nReferences: <start@example.com>\n"));
    // The author differs from the sender: a From: line opens the body.
    assert!(
        m1.contains("\n\nFrom: A U Thor <author@example.com>\n\n"),
        "{m1}"
    );
    let id1 = m1
        .lines()
        .find_map(|l| l.strip_prefix("Message-ID: "))
        .unwrap();
    assert!(m2.contains(&format!("In-Reply-To: {id1}\n")), "{m2}");
    assert!(
        m2.contains(&format!("References: <start@example.com>\n {id1}\n")),
        "{m2}"
    );
    assert!(m2.contains("Cc: Some One <cc@example.com>,\n\tA U Thor <author@example.com>,\n\tC O Mitter <committer@example.com>\n"), "{m2}");
}

#[test]
fn dry_run_with_format_patch_arguments() {
    let r = repo();
    let out = ok(
        &r.home,
        &r.dir,
        &[
            "send-email",
            "--dry-run",
            "--suppress-cc=all",
            "--from=Example <from@example.com>",
            "--to=to@example.com",
            "--bcc=hidden@example.com",
            "--smtp-server=relay.example.com",
            "-1",
        ],
    );
    assert!(out.contains("Dry-OK. Log says:\nServer: relay.example.com\nMAIL FROM:<from@example.com>\nRCPT TO:<to@example.com>\nRCPT TO:<hidden@example.com>\n"), "{out}");
    assert!(out.contains("Subject: [PATCH] Second.\n"));
    assert!(!out.contains("Bcc:"));
    assert!(out.contains("Result: OK\n"));
}

#[test]
fn validation_rejects_long_lines_and_templates() {
    let r = repo();
    let patch = ok(&r.home, &r.dir, &["format-patch", "-1"]);
    let patch = patch.trim();
    let mut long = std::fs::read_to_string(r.dir.join(patch)).unwrap();
    long.push_str(&"z".repeat(1000));
    long.push('\n');
    std::fs::write(r.dir.join("long.patch"), long).unwrap();
    let server = r.sendmail();
    let o = run(
        &r.home,
        &r.dir,
        &[
            "send-email",
            "--to=x@example.com",
            "--transfer-encoding=8bit",
            &server,
            "long.patch",
        ],
    );
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("is longer than 998 characters"), "{err}");
    assert!(err.contains("warning: no patches were sent"));
    assert!(!r.dir.join("msgtxt1").exists());

    // With quoted-printable the line is fine.
    ok(
        &r.home,
        &r.dir,
        &[
            "send-email",
            "--to=x@example.com",
            "--transfer-encoding=auto",
            &server,
            "long.patch",
        ],
    );
    assert!(
        r.read("msgtxt1")
            .contains("Content-Transfer-Encoding: quoted-printable")
    );

    ok(
        &r.home,
        &r.dir,
        &["format-patch", "-q", "--cover-letter", "-1", "-o", "cover"],
    );
    let o = run(
        &r.home,
        &r.dir,
        &["send-email", "--to=x@example.com", &server, "cover"],
    );
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("*** SUBJECT HERE ***"));
}

#[test]
fn aliases_are_listed_and_translated() {
    let r = repo();
    std::fs::write(
        r.dir.join("aliases"),
        "alice: Alice <a@example.com>\nbob: b@example.com\ngroup: alice, bob\n",
    )
    .unwrap();
    ok(
        &r.home,
        &r.dir,
        &["config", "sendemail.aliasesFile", "aliases"],
    );
    ok(
        &r.home,
        &r.dir,
        &["config", "sendemail.aliasFileType", "sendmail"],
    );
    assert_eq!(
        ok(&r.home, &r.dir, &["send-email", "--dump-aliases"]),
        "alice\nbob\ngroup\n"
    );
    let mut c = rgit(&r.home, &r.dir);
    c.args(["send-email", "--translate-aliases"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut child = c.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"group\nOther <o@example.com>\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Alice <a@example.com>\nb@example.com\nOther <o@example.com>\n"
    );
    let o = run(&r.home, &r.dir, &["send-email", "--dump-aliases", "--to=x"]);
    assert!(!o.status.success());
}

/// A fake SMTP server; returns the port and the transcripts of all
/// connections (one string per connection).
fn smtp_server(connections: usize) -> (u16, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let log = Arc::new(Mutex::new(Vec::new()));
    let log2 = log.clone();
    let h = thread::spawn(move || {
        for _ in 0..connections {
            let (sock, _) = listener.accept().unwrap();
            let mut w = sock.try_clone().unwrap();
            let mut r = BufReader::new(sock);
            let mut t = String::new();
            w.write_all(b"220 fake.test ESMTP\r\n").unwrap();
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                t.push_str(&line);
                let upper = line.to_ascii_uppercase();
                let reply: &[u8] = if upper.starts_with("EHLO") {
                    b"250-fake.test\r\n250-AUTH PLAIN LOGIN\r\n250 8BITMIME\r\n"
                } else if upper.starts_with("AUTH PLAIN") {
                    b"235 2.7.0 accepted\r\n"
                } else if upper.starts_with("DATA") {
                    w.write_all(b"354 go ahead\r\n").unwrap();
                    loop {
                        let mut l = String::new();
                        r.read_line(&mut l).unwrap();
                        t.push_str(&l);
                        if l == ".\r\n" {
                            break;
                        }
                    }
                    b"250 2.0.0 queued\r\n"
                } else if upper.starts_with("QUIT") {
                    w.write_all(b"221 bye\r\n").unwrap();
                    break;
                } else {
                    b"250 ok\r\n"
                };
                w.write_all(reply).unwrap();
            }
            log2.lock().unwrap().push(t);
        }
    });
    (port, log, h)
}

#[test]
fn smtp_with_auth_and_batches() {
    let r = repo();
    let files = ok(&r.home, &r.dir, &["format-patch", "-2", "-o", "out"]);
    let files: Vec<&str> = files.lines().collect();
    let (port, log, h) = smtp_server(2);
    let port_arg = format!("--smtp-server-port={port}");
    let mut args = vec![
        "send-email",
        "--quiet",
        "--from=Example <from@example.com>",
        "--to=to@example.com",
        "--suppress-cc=all",
        "--smtp-server=127.0.0.1",
        &port_arg,
        "--smtp-domain=client.test",
        "--smtp-user=user",
        "--smtp-pass=secret",
        "--batch-size=1",
        "--relogin-delay=0",
    ];
    args.extend(files.iter().copied());
    let out = ok(&r.home, &r.dir, &args);
    assert_eq!(out, "Sent [PATCH 1/2] First.\nSent [PATCH 2/2] Second.\n");
    h.join().unwrap();
    let log = log.lock().unwrap();
    assert_eq!(log.len(), 2, "one connection per batch");
    for t in log.iter() {
        assert!(t.starts_with("EHLO client.test\r\n"), "{t}");
        assert!(t.contains("AUTH PLAIN AHVzZXIAc2VjcmV0\r\n"), "{t}");
        assert!(t.contains("MAIL FROM:<from@example.com>\r\nRCPT TO:<to@example.com>\r\nDATA\r\n"));
        assert!(t.ends_with(".\r\nQUIT\r\n"), "{t}");
    }
    assert!(log[1].contains("Subject: [PATCH 2/2] Second.\r\n"));
}

#[test]
fn imap_send_uploads_and_lists() {
    let r = repo();
    let mbox = ok(&r.home, &r.dir, &["format-patch", "--stdout", "-2"]);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = thread::spawn(move || {
        let mut transcripts = Vec::new();
        for _ in 0..2 {
            let (sock, _) = listener.accept().unwrap();
            let mut w = sock.try_clone().unwrap();
            let mut r = BufReader::new(sock);
            let mut t = String::new();
            w.write_all(b"* OK fake IMAP\r\n").unwrap();
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                t.push_str(&line);
                let (tag, rest) = line.trim_end().split_once(' ').unwrap();
                let rest = rest.to_ascii_uppercase();
                let reply = if rest.starts_with("CAPABILITY") {
                    format!("* CAPABILITY IMAP4rev1\r\n{tag} OK done\r\n")
                } else if rest.starts_with("LIST") {
                    format!("* LIST (\\Drafts) \"/\" \"Drafts\"\r\n{tag} OK done\r\n")
                } else if rest.starts_with("APPEND") {
                    let n: usize = rest
                        .rsplit('{')
                        .next()
                        .unwrap()
                        .trim_end_matches('}')
                        .parse()
                        .unwrap();
                    w.write_all(b"+ ready\r\n").unwrap();
                    let mut buf = vec![0; n + 2];
                    r.read_exact(&mut buf).unwrap();
                    t.push_str(&String::from_utf8_lossy(&buf));
                    format!("{tag} OK appended\r\n")
                } else if rest.starts_with("LOGOUT") {
                    w.write_all(format!("* BYE\r\n{tag} OK bye\r\n").as_bytes())
                        .unwrap();
                    break;
                } else {
                    format!("{tag} OK done\r\n")
                };
                w.write_all(reply.as_bytes()).unwrap();
            }
            transcripts.push(t);
        }
        transcripts
    });
    let host = "imap.host=imap://127.0.0.1".to_string();
    let port_cfg = format!("imap.port={port}");
    let base = [
        "-c",
        &host,
        "-c",
        &port_cfg,
        "-c",
        "imap.user=bob",
        "-c",
        "imap.pass=pw",
        "-c",
        "imap.folder=Drafts",
    ];
    let mut args: Vec<&str> = base.to_vec();
    args.extend(["imap-send", "--draft"]);
    let mut c = rgit(&r.home, &r.dir);
    c.args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(mbox.as_bytes())
        .unwrap();
    let o = child.wait_with_output().unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{err}");
    assert!(
        err.contains("Sending 2 messages to Drafts folder..."),
        "{err}"
    );

    let mut args: Vec<&str> = base.to_vec();
    args.extend(["imap-send", "--list"]);
    let out = ok(&r.home, &r.dir, &args);
    assert_eq!(out, "* LIST (\\Drafts) \"/\" \"Drafts\"\n");

    let t = h.join().unwrap();
    assert!(t[0].contains("LOGIN \"bob\" \"pw\"\r\n"), "{}", t[0]);
    assert!(t[0].contains("APPEND \"Drafts\" (\\Draft) {"));
    assert!(t[0].contains("Subject: [PATCH 1/2] First.\r\n"));
    assert!(t[0].contains("Subject: [PATCH 2/2] Second.\r\n"));
    assert!(!t[0].contains("From 0"), "mbox separators are not uploaded");
}

#[test]
fn imap_send_needs_a_host_and_a_folder() {
    let r = repo();
    let o = run(&r.home, &r.dir, &["imap-send"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).starts_with("error: no IMAP host specified\n"));
    let o = run(
        &r.home,
        &r.dir,
        &["-c", "imap.host=imap://127.0.0.1", "imap-send"],
    );
    assert!(String::from_utf8_lossy(&o.stderr).starts_with("error: no IMAP folder specified\n"));
}

#[test]
fn request_pull_matches_git() {
    let fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"));
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx
        .git(["init", "-q", "-b", "main"])
        .git(["init", "-q", "--bare", "pub.git"])
        .write("f", "one\n")
        .git(["add", "f"])
        .git(["commit", "-q", "-m", "First"])
        .git(["tag", "-a", "-m", "version 1", "v1"])
        .tick()
        .write("f", "one\ntwo\n")
        .git(["commit", "-q", "-a", "-m", "Second"])
        .git(["tag", "-a", "-m", "version 2", "v2"])
        .git(["config", "branch.main.description", "What this branch does"])
        .git(["push", "-q", "pub.git", "main:for-up", "v2"]);
    let url = Normalizer::regex(r"(?m)^  \S*pub\.git", "  URL");
    for args in [
        &["request-pull", "v1", "pub.git", "main:for-up"][..],
        &["request-pull", "-p", "v1", "pub.git", "main:for-up"],
        &["request-pull", "v1", "pub.git", "v2"],
        &["request-pull", "v1", "pub.git", "main:nothere"],
        &["request-pull", "v1", "pub.git"],
        &["request-pull", "nosuch", "pub.git"],
    ] {
        fx.compare(args)
            .checks(Check::OUTPUT)
            .normalize(url.clone())
            .run();
    }
}
