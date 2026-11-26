// SPDX-License-Identifier: MIT
//! Handing a message over: to a sendmail-like program, or to an SMTP
//! server (with STARTTLS / implicit TLS and AUTH, the password from
//! `git credential`), or nowhere with `--dry-run`. Each message is
//! reported with a log of what was (or would have been) done.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use rgit_mailnet::TlsOptions;
use rgit_mailnet::smtp::{self, Encryption};

use crate::messages::send_email as msg;
use crate::prelude::*;

/// What the configuration says about delivery.
pub struct Choice {
    pub sendmail_cmd: Option<String>,
    pub smtp_server: Option<String>,
    pub port: Option<String>,
    pub server_options: Vec<String>,
    pub encryption: String,
    pub domain: Option<String>,
    pub user: Option<String>,
    pub pass: Option<String>,
    pub auth: Option<String>,
    pub cert_path: Option<String>,
    pub client_cert: Option<String>,
    pub client_key: Option<String>,
    pub debug: bool,
    pub batch_size: usize,
    pub relogin_delay: u64,
    pub outlook_id_fix: Option<bool>,
}

/// How messages leave.
pub enum Delivery {
    /// A sendmail-like program: run directly (`--smtp-server=/path`) or
    /// through the shell (`--sendmail-cmd`).
    Sendmail {
        program: String,
        shell: bool,
        options: Vec<String>,
    },
    Smtp(Box<SmtpSettings>),
}

pub struct SmtpSettings {
    /// As given (`host` or `host:port`), for the log.
    pub display: String,
    pub host: String,
    pub port: Option<String>,
    pub encryption: String,
    pub domain: Option<String>,
    pub user: Option<String>,
    pub pass: Option<String>,
    pub auth: Option<String>,
    pub cert_path: Option<String>,
    pub client_cert: Option<String>,
    pub client_key: Option<String>,
    pub debug: bool,
    pub batch_size: usize,
    pub relogin_delay: u64,
    pub outlook_id_fix: Option<bool>,
}

/// The envelope of one message.
pub struct Envelope {
    pub from: String,
    /// `--envelope-sender` (resolved), passed to sendmail as `-f`.
    pub explicit_envelope: Option<String>,
    pub recipients: Vec<String>,
}

fn find_sendmail() -> Option<String> {
    let mut dirs: Vec<PathBuf> = vec!["/usr/sbin".into(), "/usr/lib".into()];
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.into_iter()
        .map(|d| d.join("sendmail"))
        .find(|p| p.is_file())
        .map(|p| p.display().to_string())
}

pub fn choose(c: Choice) -> Result<Delivery, Die> {
    // A port that is neither a number nor a known service name is an
    // error whatever the transport.
    port_number(c.port.as_deref(), Encryption::None)?;
    Ok(choose_inner(c))
}

fn choose_inner(c: Choice) -> Delivery {
    if let Some(cmd) = c.sendmail_cmd {
        return Delivery::Sendmail {
            program: cmd,
            shell: true,
            options: c.server_options,
        };
    }
    let server = match c.smtp_server {
        Some(s) if s.starts_with('/') => {
            return Delivery::Sendmail {
                program: s,
                shell: false,
                options: c.server_options,
            };
        }
        Some(s) => s,
        None => match find_sendmail() {
            Some(p) => {
                return Delivery::Sendmail {
                    program: p,
                    shell: false,
                    options: c.server_options,
                };
            }
            None => "localhost".to_string(),
        },
    };
    // `host:port` (not for IPv6 addresses with several colons).
    let (host, port) = match server.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') && !p.is_empty() => {
            (h.to_string(), Some(c.port.clone().unwrap_or_else(|| p.to_string())))
        }
        _ => (server.clone(), c.port.clone()),
    };
    Delivery::Smtp(Box::new(SmtpSettings {
        display: server,
        host,
        port,
        encryption: c.encryption,
        domain: c.domain,
        user: c.user,
        pass: c.pass,
        auth: c.auth,
        cert_path: c.cert_path,
        client_cert: c.client_cert,
        client_key: c.client_key,
        debug: c.debug,
        batch_size: c.batch_size,
        relogin_delay: c.relogin_delay,
        outlook_id_fix: c.outlook_id_fix,
    }))
}

/// An open SMTP connection kept across messages.
#[derive(Default)]
pub struct Session {
    client: Option<smtp::Client>,
    sent_on_connection: usize,
}

impl Session {
    pub fn finish(&mut self) {
        if let Some(c) = self.client.take() {
            c.quit();
        }
    }
}

fn log_header(header: &str) -> String {
    let mut s = header.to_string();
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s.push('\n');
    s
}

/// Send `message`; prints the log. Returns a Message-ID the server
/// assigned (Outlook) when it differs from ours.
#[allow(clippy::too_many_arguments)]
pub fn send(
    session: &mut Session,
    delivery: &Delivery,
    ctx: &Context,
    env: &Envelope,
    header: &str,
    message: &[u8],
    subject: &str,
    dry_run: bool,
    quiet: bool,
) -> Result<Option<String>, Die> {
    let prefix = if dry_run { "Dry-" } else { "" };
    match delivery {
        Delivery::Sendmail {
            program,
            shell,
            options,
        } => {
            let mut args: Vec<String> = Vec::new();
            if let Some(e) = &env.explicit_envelope {
                args.push("-f".into());
                args.push(e.clone());
            }
            args.extend(options.iter().cloned());
            args.push("-i".into());
            args.extend(env.recipients.iter().cloned());
            if !dry_run {
                run_sendmail(program, *shell, &args, message)?;
            }
            if quiet {
                println!("{prefix}Sent {subject}");
            } else {
                print!(
                    "{prefix}OK. Log says:\nSendmail: {program} {}\n{}Result: OK\n",
                    args.join(" "),
                    log_header(header)
                );
            }
            let _ = std::io::stdout().flush();
            Ok(None)
        }
        Delivery::Smtp(s) => {
            let mut new_id = None;
            let mut result = "OK".to_string();
            if !dry_run {
                let reply = smtp_send(session, s, ctx, env, message, subject)?;
                let outlook = s.outlook_id_fix.unwrap_or(matches!(
                    s.host.as_str(),
                    "smtp.office365.com" | "smtp-mail.outlook.com"
                ));
                if outlook {
                    new_id = reply.lines.iter().find_map(|l| {
                        let lt = l.find('<')?;
                        let gt = l[lt..].find('>')?;
                        Some(l[lt..lt + gt + 1].to_string())
                    });
                }
                result = format!("{} {}", reply.code, reply.lines.join("\n"));
            }
            if quiet {
                println!("{prefix}Sent {subject}");
            } else {
                let mut log = format!("{prefix}OK. Log says:\nServer: {}\n", s.display);
                log.push_str(&format!("MAIL FROM:<{}>\n", env.from));
                for r in &env.recipients {
                    log.push_str(&format!("RCPT TO:<{r}>\n"));
                }
                log.push_str(&log_header(header));
                log.push_str(&format!("Result: {result}\n"));
                print!("{log}");
            }
            let _ = std::io::stdout().flush();
            Ok(new_id)
        }
    }
}

fn run_sendmail(program: &str, shell: bool, args: &[String], message: &[u8]) -> Result<(), Die> {
    let mut cmd = if shell {
        let a: Vec<&std::ffi::OsStr> = args.iter().map(|s| std::ffi::OsStr::new(s.as_str())).collect();
        rgit_ident::editor::shell_command(program, &a)
    } else {
        let mut c = Command::new(program);
        c.args(args);
        c
    };
    let mut child = cmd
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| Die::Fatal(format!("{program}: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(message);
    }
    let status = child
        .wait()
        .map_err(|e| Die::Fatal(format!("{program}: {e}")))?;
    if !status.success() {
        let s = status
            .code()
            .map_or_else(|| "killed by a signal".to_string(), |c| format!("exit code {c}"));
        let _ = std::io::stdout().flush();
        eprintln!("{}", msg::sendmail_failed(program, &s));
        return Err(Die::Exit(status.code().unwrap_or(1).max(1)));
    }
    Ok(())
}

pub fn port_number(port: Option<&str>, encryption: Encryption) -> Result<u16, Die> {
    let Some(p) = port else {
        return Ok(if encryption == Encryption::Ssl { 465 } else { 25 });
    };
    let p = p.trim();
    if let Ok(n) = p.parse::<u16>() {
        return Ok(n);
    }
    match p {
        "smtp" => Ok(25),
        "submission" => Ok(587),
        "smtps" | "ssmtp" | "urd" | "submissions" => Ok(465),
        _ => {
            eprintln!("{}", msg::bad_port(p));
            Err(Die::Exit(1))
        }
    }
}

/// This host's name for EHLO.
fn local_domain() -> String {
    let name = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| std::fs::read_to_string("/etc/hostname"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if name.is_empty() {
        "localhost.localdomain".into()
    } else if name.contains('.') {
        name
    } else {
        format!("{name}.localdomain")
    }
}

/// `git credential <action>` with the given description; the answer.
fn credential(ctx: &Context, action: &str, input: &str) -> Option<Vec<(String, String)>> {
    let exe = std::env::current_exe().ok()?;
    let mut child = Command::new(exe)
        .envs(ctx.child_env())
        .args(["credential", action])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    if let Some(mut i) = child.stdin.take() {
        let _ = i.write_all(input.as_bytes());
    }
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    )
}

fn connect(s: &SmtpSettings, ctx: &Context) -> Result<smtp::Client, Die> {
    let encryption = match s.encryption.to_ascii_lowercase().as_str() {
        "ssl" => Encryption::Ssl,
        "tls" => Encryption::StartTls,
        _ => Encryption::None,
    };
    let port = port_number(s.port.as_deref(), encryption)?;
    let domain = s.domain.clone().unwrap_or_else(local_domain);
    let tls = TlsOptions {
        verify: s.cert_path.as_deref() != Some(""),
        ca_path: s
            .cert_path
            .as_deref()
            .filter(|p| !p.is_empty())
            .map(super::aliases::expand_home),
        client_cert: s.client_cert.as_deref().map(super::aliases::expand_home),
        client_key: s.client_key.as_deref().map(super::aliases::expand_home),
    };
    let config = smtp::Config {
        host: s.host.clone(),
        port,
        encryption,
        domain: domain.clone(),
        tls,
        timeout: Some(Duration::from_secs(300)),
    };
    let debug: Option<rgit_mailnet::Log> = if s.debug {
        Some(Box::new(|l: &str| eprintln!("{l}")))
    } else {
        None
    };
    let mut client = match smtp::Client::connect(&config, debug) {
        Ok(c) => c,
        Err(e) => {
            let _ = std::io::stdout().flush();
            if s.debug {
                eprintln!("{e}");
            }
            eprintln!(
                "{}",
                msg::smtp_init_failed(&s.host, &s.encryption, &domain, port)
            );
            return Err(Die::Exit(1));
        }
    };

    // Authentication.
    let Some(user) = s.user.clone().filter(|u| !u.is_empty()) else {
        return Ok(client);
    };
    let allowed: Option<Vec<String>> = s.auth.as_ref().map(|a| {
        a.split_ascii_whitespace()
            .map(str::to_ascii_uppercase)
            .collect()
    });
    if allowed
        .as_ref()
        .is_some_and(|a| a.iter().any(|m| m == "NONE"))
    {
        return Ok(client);
    }
    let offered = client.auth_mechanisms();
    let supported = ["PLAIN", "LOGIN", "XOAUTH2", "OAUTHBEARER"];
    let mech = match &allowed {
        Some(list) => list
            .iter()
            .find(|m| supported.contains(&m.as_str()) && offered.contains(m))
            .cloned(),
        None => ["PLAIN", "LOGIN"]
            .iter()
            .find(|m| offered.iter().any(|o| o == *m))
            .map(|m| m.to_string()),
    };
    let Some(mech) = mech else {
        // Nothing to authenticate with: send without.
        return Ok(client);
    };
    let description = format!(
        "protocol=smtp\nhost={}:{port}\nusername={user}\n",
        s.host
    );
    let (pass, from_helper) = match &s.pass {
        Some(p) => (p.clone(), None),
        None => {
            let answer = credential(ctx, "fill", &format!("{description}\n"));
            let Some(answer) = answer else {
                return Err(Die::Exit(128));
            };
            let get = |k: &str| {
                answer
                    .iter()
                    .find(|(n, _)| n == k)
                    .map(|(_, v)| v.clone())
            };
            let pass = get("password").unwrap_or_default();
            let user2 = get("username").unwrap_or_else(|| user.clone());
            (pass, Some((user2, answer)))
        }
    };
    let login = from_helper
        .as_ref()
        .map_or(user.clone(), |(u, _)| u.clone());
    match client.authenticate(&mech, &login, &pass) {
        Ok(_) => {
            if let Some((_, answer)) = &from_helper {
                let mut d = String::new();
                for (k, v) in answer {
                    d.push_str(&format!("{k}={v}\n"));
                }
                d.push('\n');
                let _ = credential(ctx, "approve", &d);
            }
            Ok(client)
        }
        Err(e) => {
            if let Some((_, answer)) = &from_helper {
                let mut d = String::new();
                for (k, v) in answer {
                    d.push_str(&format!("{k}={v}\n"));
                }
                d.push('\n');
                let _ = credential(ctx, "reject", &d);
            }
            let _ = std::io::stdout().flush();
            eprintln!("{}", msg::smtp_auth_failed(&e.to_string()));
            Err(Die::Exit(1))
        }
    }
}

fn smtp_send(
    session: &mut Session,
    s: &SmtpSettings,
    ctx: &Context,
    env: &Envelope,
    message: &[u8],
    subject: &str,
) -> Result<smtp::Reply, Die> {
    if s.batch_size > 0 && session.sent_on_connection >= s.batch_size {
        session.finish();
        session.sent_on_connection = 0;
        if s.relogin_delay > 0 {
            std::thread::sleep(Duration::from_secs(s.relogin_delay));
        }
    }
    if session.client.is_none() {
        session.client = Some(connect(s, ctx)?);
        session.sent_on_connection = 0;
    }
    let client = session.client.as_mut().expect("connected");
    match client.send(&env.from, &env.recipients, message) {
        Ok(reply) => {
            session.sent_on_connection += 1;
            Ok(reply)
        }
        Err(e) => {
            let text = match &e {
                smtp::Error::Rejected { reply, .. } => reply.to_string(),
                other => other.to_string(),
            };
            let _ = std::io::stdout().flush();
            eprintln!("{}", msg::smtp_failed(subject, &text));
            Err(Die::Exit(1))
        }
    }
}

/// Store a copy with `git imap-send --folder <folder>`.
pub fn store_imap(ctx: &Context, folder: &str, message: &[u8], id: &str) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut mbox = msg::mbox_from_line(id).into_bytes();
    mbox.extend_from_slice(message);
    let child = Command::new(exe)
        .envs(ctx.child_env())
        .args(["imap-send", "--quiet", "--folder", folder])
        .stdin(Stdio::piped())
        .spawn();
    let ok = match child {
        Ok(mut c) => {
            if let Some(mut i) = c.stdin.take() {
                let _ = i.write_all(&mbox);
            }
            c.wait().is_ok_and(|s| s.success())
        }
        Err(_) => false,
    };
    if !ok {
        eprintln!("{}", msg::imap_failed(folder));
    }
}
