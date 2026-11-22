// SPDX-License-Identifier: MIT
//! Differential tests of `rgit mailinfo` and `rgit mailsplit` against the
//! reference git.

use rgit_difftest::{Check, Fixture};

const STATE: [Check; 4] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Worktree,
];

fn fx() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
}

const H: &str = "From: A U Thor <a@example.com>\nSubject: [PATCH 1/2] s\nDate: Mon, 17 Sep 2001 00:00:00 +0900\n";

/// Mails written for these tests (headers, encodings, MIME, in-body
/// headers, scissors).
fn mails() -> Vec<Vec<u8>> {
    let mut v: Vec<String> = vec![
        format!("From 1234 Mon Sep 17 00:00:00 2001\n{H}\nbody\n\n---\n f | 1 +\ndiff --git a/f b/f\n"),
        format!("{H}\nFrom: Other <o@example.com>\nSubject: inner\nDate: later\n\nbody\n---\npatch\n"),
        format!("{H}\nFrom: Other <o@example.com>\n\nno patch follows\n"),
        format!("{H}\n[PATCH] title from the body\n\ntext\ndiff --git a/x b/x\n"),
        format!("{H}\nchatter\n-- >8 --\nSubject: real\n\nbody\n---\np\n"),
        "From: =?UTF-8?q?J=C3=B6rg?= \"Q\" (c) <j@example.com>\nSubject: =?ISO-8859-1?Q?caf=E9?=\n =?UTF-8?B?w6k=?=\nMessage-ID: <id@example.com>\n\nx\n---\ny\n".into(),
        "Subject: Re: [RFC] [PATCH v2] fix\nContent-Type: text/plain; charset=iso-8859-1\nContent-Transfer-Encoding: quoted-printable\n\ncaf=E9 soft=\nbreak\n---\npatch=20\n".into(),
        "Subject: b64\nContent-Transfer-Encoding: base64\n\nYm9keQpsaW5lMgotLS0KcGF0Y2gK\n".into(),
        "Subject: flowed\nContent-Type: text/plain; format=flowed\n\nline one \nline two\n  stuffed\n---\npatch \n x\n".into(),
        "Subject: m\nContent-Type: multipart/mixed; boundary=XX\n\npre\n--XX\nContent-Type: text/plain\n\nmessage text\n\n--XX\nContent-Transfer-Encoding: base64\n\nLS0tCmRpZmYgLS1naXQgYS9mIGIvZgo=\n--XX--\nepilogue\n".into(),
        "Subject: crlf\nContent-Transfer-Encoding: base64\n\nYQ0KYg0K\n".into(),
        "Subject: =?bogus?q?x?=\n\nbody\n".into(),
        "Subject: s\n".into(),
    ];
    v.push(String::new());
    let mut out: Vec<Vec<u8>> = v.into_iter().map(String::into_bytes).collect();
    out.push(
        b"Subject: caf\xe9\nContent-Type: text/plain; charset=latin1\n\ncaf\xe9\n---\ncaf\xe9\n"
            .to_vec(),
    );
    out
}

#[test]
fn mailinfo() {
    let mut fx = fx();
    for mail in mails() {
        for opts in [
            &[][..],
            &["-k"],
            &["-b"],
            &["-m"],
            &["-n"],
            &["--scissors"],
            &["--no-inbody-headers"],
            &["--quoted-cr=strip"],
        ] {
            let mut args = vec!["mailinfo"];
            args.extend_from_slice(opts);
            args.extend(["msg", "patch"]);
            fx.compare(&args).stdin(mail.clone()).checks(STATE).run();
        }
    }
}

#[test]
fn mailinfo_usage() {
    let mut fx = fx();
    for args in [
        &["mailinfo"][..],
        &["mailinfo", "a"],
        &["mailinfo", "--quoted-cr=bad", "a", "b"],
        &["mailinfo", "-h"],
    ] {
        fx.compare(args).checks(STATE).run();
    }
}

#[test]
fn mailsplit() {
    let from = "From x Mon Sep 17 00:00:00 2001";
    let mut fx = fx()
        .write(
            "box",
            format!("\n  \n{from}\nSubject: a\r\n\nx\n>From y\n{from}\nSubject: b\n\n>>From z\n"),
        )
        .write("bare", "Subject: c\n\nbody\n")
        .write("empty", "")
        .write("md/cur/2", "Subject: d\n\n")
        .write("md/cur/1", "Subject: e\n\n")
        .write("md/new/1", "Subject: f\n\n");
    for args in [
        &["mailsplit", "-oout", "box"][..],
        &["mailsplit", "-oout", "--keep-cr", "--mboxrd", "box"],
        &["mailsplit", "-oout", "-d3", "-f7", "box", "md"],
        &["mailsplit", "-oout", "bare"],
        &["mailsplit", "-oout", "-b", "bare", "box"],
        &["mailsplit", "-oout", "empty"],
        &["mailsplit", "-oout", "missing"],
        &["mailsplit", "-oout", "-d2", "box"],
        &["mailsplit", "-x", "box"],
        &["mailsplit", "box", "out"],
        &["mailsplit"],
    ] {
        let fx2 = &mut fx;
        let _ = std::fs::create_dir_all(fx2.work_dir().join("out"));
        fx2.compare(args).checks(STATE).run();
    }
}
