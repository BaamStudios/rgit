// SPDX-License-Identifier: MIT
//! `rgit shell` against the reference `git shell`: the same command
//! lines (`-c <command>`) and the same interactive input must give the
//! same output, messages and exit code. The home directory is the
//! fixture's.

#![cfg(unix)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use rgit_difftest::Fixture;

mod common;
use common::servers::isolated;

fn rgit() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rgit"))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A home directory with the repository `r.git`.
fn fixture() -> Fixture {
    let fx = Fixture::new().rgit(rgit());
    if fx.is_skipped() {
        return fx;
    }
    let repo = fx.home_dir().join("r.git");
    fx.git(["init", "-q", "--bare", repo.to_str().unwrap()])
}

/// Programs for `~/git-shell-commands`.
fn with_commands(fx: &Fixture) {
    let dir = fx.home_dir().join("git-shell-commands");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let write = |name: &str, body: &str| {
        rgit_difftest::write_executable(dir.join(name), format!("#!/bin/sh\n{body}\n"));
    };
    write("ping", "echo pong \"$@\"");
    write("args", "echo \"args: $# <$1> <$2> <$3>\"; pwd");
    write("fail", "exit 7");
    write("a-b", "echo \"ran $0\"");
    write("sp ace", "echo \"ran $0\"");
    write("p.sh", "echo never");
    std::fs::write(dir.join("noexec"), "echo never\n").unwrap();
}

fn shell(fx: &Fixture, program: &Path, args: &[&str], input: &[u8], home: Option<&Path>) -> Output {
    let mut cmd = Command::new(program);
    isolated(fx, &mut cmd);
    match home {
        Some(h) if h.as_os_str().is_empty() => {
            cmd.env_remove("HOME");
        }
        Some(h) => {
            cmd.env("HOME", h);
        }
        None => {}
    }
    cmd.arg("shell")
        .args(args)
        // Not the home directory: the shell goes there itself.
        .current_dir(fx.work_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let mut pipe = child.stdin.take().unwrap();
    let input = input.to_vec();
    let feeder = std::thread::spawn(move || {
        let _ = pipe.write_all(&input);
    });
    let out = child.wait_with_output().unwrap();
    let _ = feeder.join();
    out
}

/// Both shells must behave alike; returns rgit's stdout, stderr and exit
/// code.
#[track_caller]
fn same(fx: &Fixture, args: &[&str], input: &[u8]) -> (String, String, Option<i32>) {
    let result = |program: &Path| {
        let out = shell(fx, program, args, input, None);
        (text(&out.stdout), text(&out.stderr), out.status.code())
    };
    let (git, ours) = (result(fx.reference_git()), result(&rgit()));
    assert_eq!(git, ours, "shell {args:?} with input {:?}", text(input));
    ours
}

#[test]
fn commands_equal_git() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    // Without ~/git-shell-commands only the services are allowed, with
    // one quoted argument.
    let fatal = |args: &[&str], message: &str| {
        let (out, err, code) = same(&fx, args, b"");
        assert_eq!((out.as_str(), code), ("", Some(128)), "{args:?}");
        assert_eq!(err, format!("fatal: {message}\n"), "{args:?}");
    };
    for args in [
        &["-h"][..],
        &["-c"],
        &["-x", "foo"],
        &["foo"],
        &["-c", "a", "b"],
    ] {
        fatal(args, "Run with no arguments or with -c cmd");
    }
    for command in [
        "git-upload-pack",
        "git-upload-pack r.git",
        "git-upload-pack 'r.git' extra",
        "git-upload-pack 'r.git' 'b'",
        "git-upload-pack  'r.git'",
        "git-upload-pack 'r.git' ",
        "git-upload-pack 'r.git",
        "git-upload-pack '-h'",
        "git-upload-pack '--strict' 'r.git'",
        "git-upload-pack '-'",
        "git-receive-pack",
        "git-receive-pack r.git",
        "git-upload-archive '--help'",
        "git upload-pack",
    ] {
        fatal(&["-c", command], "bad argument");
    }
    for command in [
        "git config foo.bar baz",
        "git-upload-packx 'r.git'",
        "git-upload-pack\t'r.git'",
        "git  upload-pack 'r.git'",
        "cvs server",
        "ls",
        "/bin/ls",
        "",
        " ",
    ] {
        fatal(
            &["-c", command],
            &format!("unrecognized command '{command}'"),
        );
    }
    // The service runs with the argument unquoted, from where the shell
    // was started.
    let direct = |program: &Path, dir: &str| {
        let mut cmd = Command::new(program);
        isolated(&fx, &mut cmd);
        let mut child = cmd
            .args(["upload-pack", dir])
            .current_dir(fx.work_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"0000").unwrap();
        child.wait_with_output().unwrap()
    };
    let repo = fx.home_dir().join("r.git");
    let repo = repo.to_str().unwrap();
    let expected = direct(&rgit(), repo);
    assert!(expected.status.success() && text(&expected.stdout).contains("capabilities^{}"));
    for command in [
        format!("git-upload-pack '{repo}'"),
        format!("git upload-pack '{repo}'"),
        "git-upload-pack '~/r.git'".to_owned(),
    ] {
        let out = shell(&fx, &rgit(), &["-c", &command], b"0000", None);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{command}: {}",
            text(&out.stderr)
        );
        assert_eq!(out.stdout, expected.stdout, "{command}");
        let git = shell(&fx, fx.reference_git(), &["-c", &command], b"0000", None);
        assert_eq!(git.status.code(), Some(0), "{command}");
    }
    // The quoting of the clients: `'\''` and `'\!'`.
    for (command, dir) in [
        ("git-upload-pack 'no'\\''such.git'", "no'such.git"),
        ("git-upload-pack 'no'\\!'such.git'", "no!such.git"),
        ("git-upload-pack ''", ""),
    ] {
        fatal(
            &["-c", command],
            &format!("'{dir}' does not appear to be a git repository"),
        );
    }

    // Programs of ~/git-shell-commands.
    with_commands(&fx);
    let home = fx.home_dir();
    let ran = |command: &str, stdout: &str, code: i32| {
        let (out, err, got) = same(&fx, &["-c", command], b"");
        assert_eq!(
            (out.as_str(), err.as_str(), got),
            (stdout, "", Some(code)),
            "{command}"
        );
    };
    ran("ping", "pong\n", 0);
    ran("ping a 'b c'", "pong a b c\n", 0);
    ran("ping ", "pong \n", 0);
    ran(
        "args a 'b c' \"d e\"",
        &format!("args: 3 <a> <b c> <d e>\n{}\n", home.display()),
        0,
    );
    ran(
        "args a\\ b \"c\\\"d\" 'e\\f'",
        &format!("args: 3 <a b> <c\"d> <e\\f>\n{}\n", home.display()),
        0,
    );
    ran("fail", "", 7);
    ran("a-b", "ran git-shell-commands/a-b\n", 0);
    ran("'sp ace'", "ran git-shell-commands/sp ace\n", 0);
    for command in [
        "noexec",
        "nope",
        "p.sh",
        "sub",
        "../ping",
        "git-shell-commands/ping",
        " ping",
        "git ping",
        "help",
        ".",
        "-x",
    ] {
        fatal(
            &["-c", command],
            &format!("unrecognized command '{command}'"),
        );
    }
    fatal(
        &["-c", "ping 'x"],
        "invalid command format 'ping 'x': unclosed quote",
    );
    fatal(
        &["-c", "ping x\\"],
        "invalid command format 'ping x\\': cmdline ends with \\",
    );
}

#[test]
fn home_directory() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    with_commands(&fx);
    let both = |args: &[&str], home: &Path| {
        let result = |program: &Path| {
            let out = shell(&fx, program, args, b"", Some(home));
            (text(&out.stdout), text(&out.stderr), out.status.code())
        };
        let (git, ours) = (result(fx.reference_git()), result(&rgit()));
        assert_eq!(git, ours, "{args:?} with HOME={}", home.display());
        ours
    };
    let (_, err, code) = both(&["-c", "ping"], Path::new(""));
    assert_eq!(code, Some(128));
    assert_eq!(
        err,
        "fatal: could not determine user's home directory; HOME is unset\n"
    );
    for args in [&["-c", "ping"][..], &[]] {
        let (_, err, _) = both(args, Path::new("/nonexistent/home"));
        assert_eq!(err, "fatal: could not chdir to user's home directory\n");
    }
}

#[test]
fn interactive_equals_git() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    // Not without ~/git-shell-commands.
    let (out, err, code) = same(&fx, &[], b"ping\n");
    assert_eq!((out.as_str(), code), ("", Some(128)));
    assert_eq!(
        err,
        "fatal: Interactive git shell is not enabled.\n\
         hint: ~/git-shell-commands should exist and have read and execute access.\n"
    );

    with_commands(&fx);
    let home = fx.home_dir();
    let session = |input: &str| same(&fx, &[], input.as_bytes());
    // The prompt and the messages are on standard error; the end of
    // input ends the shell with a new line.
    assert_eq!(session(""), (String::new(), "git> \n".to_owned(), Some(0)));
    assert_eq!(
        session("ping\n"),
        ("pong\n".to_owned(), "git> git> \n".to_owned(), Some(0))
    );
    assert_eq!(
        session("ping"),
        ("pong\n".to_owned(), "git> git> \n".to_owned(), Some(0))
    );
    assert_eq!(session("ping\r\n").0, "pong\n");
    for word in ["exit", "quit", "logout", "bye", "exit now"] {
        let (out, err, code) = session(&format!("ping a b\n{word}\nping never\n"));
        assert_eq!((out.as_str(), code), ("pong a b\n", Some(0)), "{word}");
        assert_eq!(err, "git> git> ", "{word}");
    }
    let (out, err, _) = session("args x \"y z\"\nfail\n\n  ping  \n exit\nping\n");
    assert_eq!(
        out,
        format!("args: 2 <x> <y z> <>\n{}\npong\n", home.display())
    );
    assert_eq!(err, "git> git> git> git> git> git> git> \n");
    let (out, err, _) = session("nope\nEXIT\nhelp\nnoexec\np.sh\nsub/x\nping 'x\nargs a\\\nping\n");
    assert_eq!(out, "pong\n");
    assert_eq!(
        err,
        "git> unrecognized command 'nope'\n\
         git> unrecognized command 'EXIT'\n\
         git> unrecognized command 'help'\n\
         git> fatal: cannot exec 'git-shell-commands/noexec': Permission denied\n\
         git> invalid command format 'p.sh'\n\
         git> invalid command format 'sub/x'\n\
         git> invalid command format 'ping 'x': unclosed quote\n\
         git> invalid command format 'args a\\': cmdline ends with \\\n\
         git> git> \n"
    );
    // The services are for `-c` only.
    let (_, err, _) = session("git-upload-pack 'r.git'\n");
    assert_eq!(err, "git> unrecognized command 'git-upload-pack'\ngit> \n");

    // A `help` program runs before the first prompt, and on demand.
    let commands = home.join("git-shell-commands");
    rgit_difftest::write_executable(commands.join("help"), "#!/bin/sh\necho 'this is help'\n");
    assert_eq!(session("").0, "this is help\n");
    assert_eq!(
        session("help\nping\n").0,
        "this is help\nthis is help\npong\n"
    );

    // A line that is too long is fatal, with and without its end.
    let long = "a".repeat(4 * 1024 * 1024 - 1);
    for input in [long.clone(), format!("{}\nping\n", &long[1..])] {
        let (out, err, code) = session(&input);
        assert_eq!(out, "this is help\n");
        assert_eq!(err, "git> fatal: invalid command format: input too long\n");
        assert_eq!(code, Some(128));
    }

    // `no-interactive-login` replaces the shell; `-c` still works.
    for status in [128, 0, 3] {
        rgit_difftest::write_executable(
            commands.join("no-interactive-login"),
            format!("#!/bin/sh\necho 'no login'\nexit {status}\n"),
        );
        assert_eq!(
            session("ping\n"),
            ("no login\n".to_owned(), String::new(), Some(status))
        );
    }
    assert_eq!(same(&fx, &["-c", "ping"], b"").0, "pong\n");
}
