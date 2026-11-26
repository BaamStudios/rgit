// SPDX-License-Identifier: MIT
//! `git remote-ext <remote> <url>` (Documentation/git-remote-ext.adoc):
//! the remote helper of `ext::<command>` URLs. It only knows `connect`:
//! the command is run with the service's standard input and output.
//!
//! The command is split at unescaped spaces (empty words are kept);
//! `% ` is a space, `%%` a percent sign, `%s` / `%S` the service name
//! without / with `git-`. A word starting with `%G` is not passed on but
//! sends a `git://` request for the rest of the word first, `%V` sets its
//! `host=` field. The service names are also in `GIT_EXT_SERVICE` and
//! `GIT_EXT_SERVICE_NOPREFIX`.
//!
//! Behaviour observed with git 2.56: `-h` prints the usage on standard
//! output and succeeds, wrong arguments print it on standard error
//! (exit 129); end of input ends the helper, any other command (an empty
//! line too) prints `Bad command` (no newline) and
//! exits with 1.

use std::fs::File;
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};

use crate::messages::remote_helper as msg;
use crate::prelude::*;

pub fn run(_ctx: &mut Context, argv: &[String]) -> CmdResult {
    let url = match helper_args(argv, msg::EXT_USAGE) {
        Ok(url) => url,
        Err(code) => return Ok(code),
    };
    serve(|service| connect_ext(&url, service))
}

/// The `<url>` argument; `Err(exit code)` after printing the usage.
pub(crate) fn helper_args(argv: &[String], usage: &str) -> Result<String, i32> {
    let args = &argv[1..];
    if args.len() == 1 && (args[0] == "-h" || args[0] == "--help-all") {
        println!("{usage}");
        return Err(0);
    }
    if args.len() != 2 {
        eprintln!("{usage}");
        return Err(129);
    }
    Ok(args[1].clone())
}

/// Answer the helper protocol on standard input and output: the
/// `capabilities` (just `*connect`), then `connect <service>`, which
/// `connect` serves.
pub(crate) fn serve(connect: impl FnOnce(&str) -> CmdResult) -> CmdResult {
    // A duplicate of fd 0: reads are not buffered, so nothing after the
    // `connect` line is consumed here.
    let mut input = stdin_file()?;
    let mut out = io::stdout();
    loop {
        let Some(line) = read_line(&mut input)? else {
            return Ok(0);
        };
        if line == "capabilities" {
            let _ = out.write_all(b"*connect\n\n");
            let _ = out.flush();
        } else if let Some(service) = line.strip_prefix("connect ") {
            let _ = out.write_all(b"\n");
            let _ = out.flush();
            return connect(service);
        } else {
            eprint!("{}", msg::BAD_COMMAND);
            return Ok(1);
        }
    }
}

#[cfg(unix)]
fn stdin_file() -> Result<File, Die> {
    use std::os::fd::AsFd;
    io::stdin()
        .as_fd()
        .try_clone_to_owned()
        .map(File::from)
        .map_err(|e| Die::Fatal(e.to_string()))
}

#[cfg(not(unix))]
fn stdin_file() -> Result<File, Die> {
    Err(Die::Fatal("remote helpers need a Unix system".into()))
}

/// One line without its newline, read a byte at a time; `None` at end of
/// input.
fn read_line(input: &mut File) -> Result<Option<String>, Die> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match input.read(&mut byte) {
            Ok(0) if line.is_empty() => return Ok(None),
            Ok(0) => break,
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => line.push(byte[0]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(Die::Fatal(e.to_string())),
        }
    }
    Ok(Some(String::from_utf8_lossy(&line).into_owned()))
}

/// The command of an `ext::` URL for `service`.
#[derive(Debug, Default, PartialEq, Eq)]
struct ExtCommand {
    argv: Vec<String>,
    /// `%G<path>`: the repository of the `git://` request.
    git_path: Option<String>,
    /// `%V<host>`.
    vhost: Option<String>,
}

/// The whole command: words are split at spaces not escaped as `% `.
fn parse_command(cmd: &str, service: &str) -> Result<ExtCommand, Die> {
    let mut all = ExtCommand::default();
    for w in &split_words(cmd) {
        let one = parse_ext(w, service)?;
        all.argv.extend(one.argv);
        all.git_path = one.git_path.or(all.git_path);
        all.vhost = one.vhost.or(all.vhost);
    }
    Ok(all)
}

/// One word of the command.
fn parse_ext(word: &str, service: &str) -> Result<ExtCommand, Die> {
    let short = service.strip_prefix("git-").unwrap_or(service);
    let mut out = ExtCommand::default();
    {
        let mut special: Option<char> = None;
        let mut rest = word;
        if let Some(r) = word.strip_prefix("%G") {
            special = Some('G');
            rest = r;
        } else if let Some(r) = word.strip_prefix("%V") {
            special = Some('V');
            rest = r;
        }
        let mut value = String::new();
        let mut chars = rest.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                value.push(c);
                continue;
            }
            match chars.next() {
                None => return Err(Die::Fatal(msg::INCOMPLETE_PLACEHOLDER.into())),
                Some('%') => value.push('%'),
                Some(' ') => value.push(' '),
                Some('s') => value.push_str(short),
                Some('S') => value.push_str(service),
                Some(other) => return Err(Die::Fatal(msg::bad_placeholder(other))),
            }
        }
        match special {
            Some('G') => out.git_path = Some(value),
            Some(_) => out.vhost = Some(value),
            None => out.argv.push(value),
        }
    }
    Ok(out)
}

/// Words are split at spaces not escaped as `% `.
fn split_words(cmd: &str) -> Vec<String> {
    let mut words = vec![String::new()];
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '%' => {
                words.last_mut().expect("one word").push('%');
                if let Some(n) = chars.next() {
                    words.last_mut().expect("one word").push(n);
                }
            }
            ' ' => words.push(String::new()),
            c => words.last_mut().expect("one word").push(c),
        }
    }
    words
}

fn connect_ext(url: &str, service: &str) -> CmdResult {
    let parsed = parse_command(url, service)?;
    if parsed.argv.is_empty() || parsed.argv[0].is_empty() && parsed.argv.len() == 1 {
        return Err(Die::Fatal(msg::EMPTY_COMMAND.into()));
    }
    let short = service.strip_prefix("git-").unwrap_or(service);
    let mut cmd = Command::new(&parsed.argv[0]);
    cmd.args(&parsed.argv[1..])
        .env("GIT_EXT_SERVICE", service)
        .env("GIT_EXT_SERVICE_NOPREFIX", short);
    let Some(path) = parsed.git_path else {
        let status = cmd.status().map_err(|e| {
            Die::Fatal(rgit_transport::messages::cannot_exec(
                &parsed.argv[0],
                &crate::global::strerror(&e),
            ))
        })?;
        return Ok(status.code().unwrap_or(128));
    };
    // The `git://` request goes first, then everything git sends.
    let mut request = format!("{service} {path}\0").into_bytes();
    if let Some(host) = &parsed.vhost {
        request.extend_from_slice(format!("host={host}\0").as_bytes());
    }
    let mut child = cmd.stdin(Stdio::piped()).spawn().map_err(|e| {
        Die::Fatal(rgit_transport::messages::cannot_exec(
            &parsed.argv[0],
            &crate::global::strerror(&e),
        ))
    })?;
    let mut stdin = child.stdin.take().expect("piped");
    let mut packet = format!("{:04x}", request.len() + 4).into_bytes();
    packet.extend_from_slice(&request);
    let _ = stdin.write_all(&packet);
    let _ = stdin.flush();
    let mut input = stdin_file()?;
    std::thread::spawn(move || {
        let _ = io::copy(&mut input, &mut stdin);
    });
    let status = child.wait().map_err(|e| Die::Fatal(e.to_string()))?;
    Ok(status.code().unwrap_or(128))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(cmd: &str, service: &str) -> ExtCommand {
        parse_command(cmd, service).unwrap()
    }

    #[test]
    fn placeholders() {
        let p = parse("sh -c %S% .", "git-upload-pack");
        assert_eq!(p.argv, ["sh", "-c", "git-upload-pack ."]);
        let p = parse("wrap a%%b c% d %s x", "git-receive-pack");
        assert_eq!(p.argv, ["wrap", "a%b", "c d", "receive-pack", "x"]);
        let p = parse("wrap  %S  server", "git-upload-pack");
        assert_eq!(p.argv, ["wrap", "", "git-upload-pack", "", "server"]);
        let p = parse("fake %G/repo% x %Vhost", "git-upload-pack");
        assert_eq!(p.argv, ["fake"]);
        assert_eq!(p.git_path.as_deref(), Some("/repo x"));
        assert_eq!(p.vhost.as_deref(), Some("host"));
        assert!(parse_ext("a%x", "git-upload-pack").is_err());
        assert!(parse_ext("a%", "git-upload-pack").is_err());
        assert!(parse_ext("x%Gy", "git-upload-pack").is_err());
    }
}
