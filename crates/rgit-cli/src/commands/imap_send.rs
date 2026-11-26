// SPDX-License-Identifier: MIT
//! `git imap-send` (Documentation/git-imap-send.adoc,
//! Documentation/config/imap.adoc): upload the messages of an mbox read
//! from standard input into an IMAP folder, over `imap://` (STARTTLS when
//! the server offers it), `imaps://` or a tunnel command; or list the
//! folders with `--list`.
//!
//! The password comes from `imap.pass` or `git credential`; LOGIN is used
//! unless `imap.authMethod` names a SASL mechanism (PLAIN, XOAUTH2,
//! OAUTHBEARER). Progress and status lines follow the `git` binary.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use rgit_mailnet::imap::{self, Client};
use rgit_mailnet::{Stream, TlsOptions};

use crate::messages::imap_send as msg;
use crate::prelude::*;

struct ServerConfig {
    tunnel: Option<String>,
    host: String,
    ssl: bool,
    port: u16,
    user: Option<String>,
    pass: Option<String>,
    verify: bool,
    auth_method: Option<String>,
    html: bool,
}

fn config_string(cfg: &rgit_config::ConfigSet, key: &str) -> Option<String> {
    cfg.get(key)
        .and_then(|e| e.value.as_ref())
        .map(|v| String::from_utf8_lossy(v).into_owned())
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let verbose = Cell::new(0);
    let quiet = Cell::new(0);
    let curl = Cell::new(false);
    let draft = Cell::new(false);
    let list = Cell::new(false);
    let folder: RefCell<Option<String>> = RefCell::new(None);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::verbose(&verbose, msg::HELP_VERBOSE),
            Opt::quiet(&quiet, msg::HELP_QUIET),
            Opt::flag(None, "curl", &curl, msg::HELP_CURL),
            Opt::flag(None, "draft", &draft, msg::HELP_DRAFT),
            Opt::string('f', "folder", &folder, "folder", msg::HELP_FOLDER),
            Opt::flag(None, "list", &list, msg::HELP_LIST),
        ],
    );
    let rest = parser.parse(argv)?.args;
    if !rest.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let verbose = verbose.get() > 0;
    let quiet = quiet.get() > 0 && !verbose;

    let setup = crate::repo::setup_gently(ctx)?;
    let cfg = match &setup {
        Some(s) => crate::repo::load_config(ctx, s)?,
        None => crate::repo::config_outside_repository(ctx),
    };
    let tunnel = config_string(&cfg, "imap.tunnel").filter(|t| !t.is_empty());
    let host_setting = config_string(&cfg, "imap.host").filter(|h| !h.is_empty());
    if tunnel.is_none() && host_setting.is_none() {
        eprintln!("{}", msg::NO_HOST);
        return Err(Die::Exit(1));
    }
    let folder = folder
        .borrow()
        .clone()
        .or_else(|| config_string(&cfg, "imap.folder"));
    if folder.is_none() && !list.get() {
        eprintln!("{}", msg::NO_FOLDER);
        return Err(Die::Exit(1));
    }

    // imap://host, imaps://host or a bare host.
    let raw_host = host_setting.unwrap_or_else(|| "tunnel".into());
    let (ssl, host) = if let Some(h) = raw_host.strip_prefix("imaps://") {
        (true, h.to_string())
    } else if let Some(h) = raw_host.strip_prefix("imap://") {
        (false, h.to_string())
    } else {
        (false, raw_host.clone())
    };
    let host = host.trim_end_matches('/').to_string();
    if host.is_empty() {
        eprintln!("{}", msg::bad_host(&raw_host));
        return Err(Die::Exit(1));
    }
    let port = cfg
        .int("imap.port")
        .ok()
        .flatten()
        .and_then(|p| u16::try_from(p).ok())
        .filter(|&p| p != 0)
        .unwrap_or(if ssl { 993 } else { 143 });
    let server = ServerConfig {
        tunnel,
        host,
        ssl,
        port,
        user: config_string(&cfg, "imap.user"),
        pass: config_string(&cfg, "imap.pass"),
        verify: cfg.bool("imap.sslverify").ok().flatten().unwrap_or(true),
        auth_method: config_string(&cfg, "imap.authMethod").filter(|m| !m.is_empty()),
        html: cfg
            .bool("imap.preformattedHTML")
            .ok()
            .flatten()
            .unwrap_or(false),
    };

    // The messages (not needed for --list).
    let messages = if list.get() {
        Vec::new()
    } else {
        let mut input = Vec::new();
        let _ = std::io::stdin().lock().read_to_end(&mut input);
        let m = split_mbox(&input);
        if m.is_empty() {
            eprintln!("{}", msg::NOTHING_TO_SEND);
            return Err(Die::Exit(1));
        }
        m
    };

    let mut client = open(ctx, &server, verbose, quiet)?;

    if list.get() {
        if !quiet {
            eprintln!("{}", msg::FETCHING_LIST);
        }
        let lines = client.list().map_err(imap_die)?;
        let mut out = std::io::stdout().lock();
        for l in lines {
            let _ = writeln!(out, "{l}");
        }
        let _ = out.flush();
        let _ = client.logout();
        return Ok(0);
    }

    let folder = folder.unwrap_or_default();
    eprintln!("{}", msg::sending(messages.len(), &folder));
    client
        .command(&format!("EXAMINE {}", imap::quote(&folder)))
        .map_err(imap_die)?;
    let flags: Vec<&str> = if draft.get() { vec!["\\Draft"] } else { vec![] };
    let total = messages.len();
    let mut err = std::io::stderr();
    for (i, m) in messages.iter().enumerate() {
        let _ = write!(err, "\r{:4}% ({i}/{total}) done", i * 100 / total);
        let body = if server.html { wrap_html(m) } else { m.clone() };
        client
            .append(&folder, &flags, &imap::to_crlf(&body))
            .map_err(imap_die)?;
    }
    let _ = writeln!(err, "\r{:4}% ({total}/{total}) done", 100);
    let _ = client.logout();
    Ok(0)
}

fn imap_die(e: imap::Error) -> Die {
    eprintln!("{}", msg::imap_error(&e.to_string()));
    Die::Exit(1)
}

/// `git credential <action>`; the answer's fields.
fn credential(ctx: &Context, action: &str, input: &str) -> Result<Vec<(String, String)>, i32> {
    let exe = std::env::current_exe().map_err(|_| 128)?;
    let mut child = Command::new(exe)
        .envs(ctx.child_env())
        .args(["credential", action])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|_| 128)?;
    if let Some(mut i) = child.stdin.take() {
        let _ = i.write_all(input.as_bytes());
    }
    let out = child.wait_with_output().map_err(|_| 128)?;
    if !out.status.success() {
        return Err(out.status.code().unwrap_or(128));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect())
}

fn describe(fields: &[(String, String)]) -> String {
    let mut s = String::new();
    for (k, v) in fields {
        s.push_str(&format!("{k}={v}\n"));
    }
    s.push('\n');
    s
}

/// Connect, start TLS if needed and log in.
fn open(ctx: &Context, s: &ServerConfig, verbose: bool, quiet: bool) -> Result<Client, Die> {
    let tls = TlsOptions {
        verify: s.verify,
        ..TlsOptions::default()
    };
    let stream = if let Some(t) = &s.tunnel {
        if !quiet {
            eprint!("{}", msg::starting_tunnel(t));
        }
        match Stream::tunnel(t) {
            Ok(st) => {
                if !quiet {
                    eprintln!("OK");
                }
                st
            }
            Err(e) => {
                eprintln!();
                eprintln!("{}", msg::tunnel_failed(t, &e.to_string()));
                return Err(Die::Exit(1));
            }
        }
    } else {
        let timeout = Some(Duration::from_secs(300));
        let r = if s.ssl {
            Stream::connect_tls(&s.host, s.port, timeout, &tls)
        } else {
            Stream::connect(&s.host, s.port, timeout)
        };
        match r {
            Ok(st) => st,
            Err(e) => {
                eprintln!("{}", msg::connect_failed(&s.host, &e.to_string()));
                return Err(Die::Exit(1));
            }
        }
    };
    let log: Option<rgit_mailnet::Log> = if verbose {
        Some(Box::new(|l: &str| eprintln!("{l}")))
    } else {
        None
    };
    let mut client = Client::start(stream, log).map_err(imap_die)?;
    if s.tunnel.is_none() && !s.ssl && client.has_capability("STARTTLS") {
        client.starttls(&s.host, &tls).map_err(imap_die)?;
    }
    if client.preauthenticated() {
        return Ok(client);
    }
    if !quiet {
        eprintln!("{}", msg::LOGGING_IN);
    }

    // User and password, from the configuration or a credential helper.
    let protocol = if s.ssl { "imaps" } else { "imap" };
    let mut fields: Vec<(String, String)> = vec![
        ("protocol".into(), protocol.into()),
        ("host".into(), format!("{}:{}", s.host, s.port)),
    ];
    if let Some(u) = &s.user {
        fields.push(("username".into(), u.clone()));
    }
    let mut from_helper = None;
    let (user, pass) = match (&s.user, &s.pass) {
        (Some(u), Some(p)) => (u.clone(), p.clone()),
        _ => {
            let answer = credential(ctx, "fill", &describe(&fields)).map_err(Die::Exit)?;
            let get = |k: &str| answer.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
            let user = get("username").or_else(|| s.user.clone()).unwrap_or_default();
            let pass = get("password").or_else(|| s.pass.clone()).unwrap_or_default();
            from_helper = Some(answer);
            (user, pass)
        }
    };

    let result = match &s.auth_method {
        Some(m) => {
            let m = m.to_ascii_uppercase();
            if !matches!(m.as_str(), "PLAIN" | "XOAUTH2" | "OAUTHBEARER") {
                eprintln!("{}", msg::unsupported_auth(&m));
                return Err(Die::Exit(1));
            }
            client.authenticate(&m, &user, &pass)
        }
        None => {
            if client.has_capability("LOGINDISABLED") {
                eprintln!("{}", msg::LOGIN_DISABLED);
                return Err(Die::Exit(1));
            }
            if !client.is_tls() {
                eprintln!("{}", msg::CLEARTEXT);
            }
            client.login(&user, &pass)
        }
    };
    match result {
        Ok(_) => {
            if let Some(answer) = from_helper {
                let _ = credential(ctx, "approve", &describe(&answer));
            }
            Ok(client)
        }
        Err(e) => {
            if let Some(answer) = from_helper {
                let _ = credential(ctx, "reject", &describe(&answer));
            }
            if verbose {
                eprintln!("{e}");
            }
            eprintln!("{}", msg::login_failed(&user, &s.host));
            Err(Die::Exit(1))
        }
    }
}

/// Split an mbox into messages; the `From ` separator lines are dropped.
fn split_mbox(data: &[u8]) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut cur: Option<Vec<u8>> = None;
    let mut prev_blank = true;
    for line in data.split_inclusive(|&b| b == b'\n') {
        if prev_blank && line.starts_with(b"From ") {
            if let Some(m) = cur.take() {
                out.push(m);
            }
            cur = Some(Vec::new());
            prev_blank = false;
            continue;
        }
        let body = line.strip_suffix(b"\n").unwrap_or(line);
        let body = body.strip_suffix(b"\r").unwrap_or(body);
        prev_blank = body.is_empty();
        cur.get_or_insert_with(Vec::new).extend_from_slice(line);
    }
    if let Some(m) = cur
        && !m.iter().all(u8::is_ascii_whitespace)
    {
        out.push(m);
    }
    out
}

/// `imap.preformattedHTML`: the body as HTML inside `<pre>`.
fn wrap_html(message: &[u8]) -> Vec<u8> {
    let split = message
        .windows(2)
        .position(|w| w == b"\n\n")
        .map_or(message.len(), |i| i + 1);
    let (head, body) = message.split_at(split);
    let mut out = Vec::with_capacity(message.len() + 64);
    for line in head.split_inclusive(|&b| b == b'\n') {
        if !line.to_ascii_lowercase().starts_with(b"content-type:") {
            out.extend_from_slice(line);
        }
    }
    if out.ends_with(b"\n") {
        out.pop();
    }
    out.extend_from_slice(b"\nContent-Type: text/html;\n\n<pre>\n");
    for &b in body.strip_prefix(b"\n").unwrap_or(body) {
        match b {
            b'&' => out.extend_from_slice(b"&amp;"),
            b'<' => out.extend_from_slice(b"&lt;"),
            b'>' => out.extend_from_slice(b"&gt;"),
            b'"' => out.extend_from_slice(b"&quot;"),
            _ => out.push(b),
        }
    }
    out.extend_from_slice(b"</pre>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mbox_split() {
        let m = split_mbox(b"From 1 Mon\nSubject: a\n\nbody From here\n\nFrom 2 Tue\nSubject: b\n\nx\n");
        assert_eq!(m.len(), 2);
        assert_eq!(m[0], b"Subject: a\n\nbody From here\n\n");
        assert_eq!(m[1], b"Subject: b\n\nx\n");
        assert!(split_mbox(b"").is_empty());
    }

    #[test]
    fn html_wrapping() {
        let w = wrap_html(b"Subject: s\nContent-Type: text/plain\n\na<b\n");
        let s = String::from_utf8(w).unwrap();
        assert!(s.contains("Content-Type: text/html;"));
        assert!(!s.contains("text/plain"));
        assert!(s.contains("<pre>\na&lt;b\n</pre>"));
    }
}
