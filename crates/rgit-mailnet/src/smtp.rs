// SPDX-License-Identifier: MIT
//! A minimal SMTP client (RFC 5321) with STARTTLS (RFC 3207) and
//! SMTP AUTH (RFC 4954: PLAIN, LOGIN, XOAUTH2, OAUTHBEARER).

use std::fmt;
use std::io;
use std::time::Duration;

use crate::encoding;
use crate::stream::{LineStream, Log, Stream};
use crate::tls::TlsOptions;

/// How encryption starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encryption {
    /// Plain SMTP.
    None,
    /// Implicit TLS from the first byte (SMTPS).
    Ssl,
    /// Plain connection upgraded with STARTTLS.
    StartTls,
}

/// Where and how to connect.
#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub encryption: Encryption,
    /// The name sent with EHLO / HELO.
    pub domain: String,
    pub tls: TlsOptions,
    pub timeout: Option<Duration>,
}

/// A server reply: the code and the text of every line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub code: u16,
    pub lines: Vec<String>,
}

impl Reply {
    pub fn is_positive(&self) -> bool {
        (200..400).contains(&self.code)
    }

    /// The text of all lines, each followed by a line break.
    pub fn message(&self) -> String {
        let mut s = String::new();
        for l in &self.lines {
            s.push_str(l);
            s.push('\n');
        }
        s
    }
}

impl fmt::Display for Reply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.code, self.lines.join(" "))
    }
}

/// What went wrong.
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    /// The server rejected a command.
    Rejected {
        command: String,
        reply: Reply,
    },
    /// A required extension is missing (STARTTLS, AUTH).
    Unsupported(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Rejected { command, reply } => write!(f, "{command}: {reply}"),
            Error::Unsupported(what) => write!(f, "{what}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// An open SMTP session.
pub struct Client {
    conn: LineStream,
    host: String,
    tls: TlsOptions,
    /// EHLO keywords (upper case) with their parameters.
    extensions: Vec<(String, String)>,
    debug: Option<Log>,
}

impl Client {
    /// Connect, read the greeting, say EHLO and start TLS as configured.
    pub fn connect(config: &Config, debug: Option<Log>) -> Result<Client> {
        let stream = match config.encryption {
            Encryption::Ssl => {
                Stream::connect_tls(&config.host, config.port, config.timeout, &config.tls)?
            }
            _ => Stream::connect(&config.host, config.port, config.timeout)?,
        };
        Client::start(
            stream,
            &config.host,
            config.encryption,
            &config.domain,
            &config.tls,
            debug,
        )
    }

    /// Run the session start on an already open stream.
    pub fn start(
        stream: Stream,
        host: &str,
        encryption: Encryption,
        domain: &str,
        tls: &TlsOptions,
        debug: Option<Log>,
    ) -> Result<Client> {
        let mut c = Client {
            conn: LineStream::new(stream),
            host: host.to_string(),
            tls: tls.clone(),
            extensions: Vec::new(),
            debug,
        };
        let greeting = c.read_reply()?;
        if greeting.code != 220 {
            return Err(Error::Rejected {
                command: "connect".into(),
                reply: greeting,
            });
        }
        c.hello(domain)?;
        if encryption == Encryption::StartTls {
            if !c.has_extension("STARTTLS") {
                return Err(Error::Unsupported(
                    "STARTTLS not supported by the server".into(),
                ));
            }
            c.expect("STARTTLS", &[220])?;
            let host = c.host.clone();
            let tls = c.tls.clone();
            c.conn.start_tls(&host, &tls)?;
            c.hello(domain)?;
        }
        Ok(c)
    }

    fn log(&mut self, line: &str) {
        if let Some(d) = self.debug.as_mut() {
            d(line);
        }
    }

    fn hello(&mut self, domain: &str) -> Result<()> {
        let ehlo = format!("EHLO {domain}");
        let reply = self.command(&ehlo)?;
        if reply.code == 250 {
            self.extensions = reply
                .lines
                .iter()
                .skip(1)
                .map(|l| {
                    let (k, v) = l.split_once(' ').unwrap_or((l.as_str(), ""));
                    (k.to_ascii_uppercase(), v.to_string())
                })
                .collect();
            return Ok(());
        }
        self.extensions.clear();
        self.expect(&format!("HELO {domain}"), &[250]).map(|_| ())
    }

    /// Whether EHLO announced `keyword`.
    pub fn has_extension(&self, keyword: &str) -> bool {
        self.extensions.iter().any(|(k, _)| k == keyword)
    }

    /// The SASL mechanisms the server offers (upper case).
    pub fn auth_mechanisms(&self) -> Vec<String> {
        self.extensions
            .iter()
            .filter(|(k, _)| k == "AUTH" || k == "AUTH=")
            .flat_map(|(_, v)| v.split_ascii_whitespace().map(str::to_ascii_uppercase))
            .collect()
    }

    pub fn is_tls(&self) -> bool {
        self.conn.is_tls()
    }

    fn read_reply(&mut self) -> Result<Reply> {
        let mut lines = Vec::new();
        loop {
            let raw = self.conn.read_line()?;
            let line = String::from_utf8_lossy(&raw).into_owned();
            self.log(&format!("<<< {line}"));
            if line.len() < 3 || !line.as_bytes()[..3].iter().all(u8::is_ascii_digit) {
                return Err(Error::Io(io::Error::other(format!(
                    "malformed SMTP reply: {line}"
                ))));
            }
            let code: u16 = line[..3].parse().unwrap_or(0);
            let more = line.as_bytes().get(3) == Some(&b'-');
            lines.push(line.get(4..).unwrap_or("").to_string());
            if !more {
                return Ok(Reply { code, lines });
            }
        }
    }

    fn write_line(&mut self, line: &str, shown: &str) -> Result<()> {
        self.log(&format!(">>> {shown}"));
        self.conn.send(format!("{line}\r\n").as_bytes())?;
        Ok(())
    }

    /// Send a command line and read the reply.
    pub fn command(&mut self, line: &str) -> Result<Reply> {
        self.write_line(line, line)?;
        self.read_reply()
    }

    fn expect(&mut self, line: &str, codes: &[u16]) -> Result<Reply> {
        let reply = self.command(line)?;
        if codes.contains(&reply.code) {
            Ok(reply)
        } else {
            Err(Error::Rejected {
                command: line.to_string(),
                reply,
            })
        }
    }

    /// Authenticate with `mechanism` (PLAIN, LOGIN, XOAUTH2 or
    /// OAUTHBEARER). Secrets are not written to the debug log.
    pub fn authenticate(&mut self, mechanism: &str, user: &str, pass: &str) -> Result<Reply> {
        let mech = mechanism.to_ascii_uppercase();
        let initial = match mech.as_str() {
            "PLAIN" => Some(encoding::sasl_plain(user, pass)),
            "XOAUTH2" => Some(encoding::sasl_xoauth2(user, pass)),
            "OAUTHBEARER" => Some(encoding::sasl_oauthbearer(user, pass)),
            "LOGIN" => None,
            _ => {
                return Err(Error::Unsupported(format!(
                    "unsupported SASL mechanism {mech}"
                )));
            }
        };
        let reply = match initial {
            Some(resp) => {
                self.write_line(
                    &format!("AUTH {mech} {resp}"),
                    &format!("AUTH {mech} <secret>"),
                )?;
                let mut reply = self.read_reply()?;
                if reply.code == 334 {
                    // An error challenge (XOAUTH2): answer with an empty
                    // line to get the final reply.
                    self.write_line("", "")?;
                    reply = self.read_reply()?;
                }
                reply
            }
            None => {
                let reply = self.command("AUTH LOGIN")?;
                if reply.code != 334 {
                    reply
                } else {
                    self.write_line(&encoding::base64(user.as_bytes()), "<user>")?;
                    let reply = self.read_reply()?;
                    if reply.code != 334 {
                        reply
                    } else {
                        self.write_line(&encoding::base64(pass.as_bytes()), "<secret>")?;
                        self.read_reply()?
                    }
                }
            }
        };
        if reply.code == 235 {
            Ok(reply)
        } else {
            Err(Error::Rejected {
                command: format!("AUTH {mech}"),
                reply,
            })
        }
    }

    /// Send one message; `data` uses `\n` line endings and is dot-stuffed
    /// here. Returns the reply to the end of data.
    pub fn send(&mut self, from: &str, recipients: &[String], data: &[u8]) -> Result<Reply> {
        self.expect(&format!("MAIL FROM:<{from}>"), &[250])?;
        for r in recipients {
            self.expect(&format!("RCPT TO:<{r}>"), &[250, 251])?;
        }
        self.expect("DATA", &[354])?;
        let mut out = Vec::with_capacity(data.len() + data.len() / 32 + 8);
        let mut lines = data.split(|&b| b == b'\n').peekable();
        while let Some(line) = lines.next() {
            if lines.peek().is_none() && line.is_empty() {
                break;
            }
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.starts_with(b".") {
                out.push(b'.');
            }
            out.extend_from_slice(line);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b".\r\n");
        self.log(&format!(">>> <{} bytes of message data>", out.len()));
        self.conn.send(&out)?;
        let reply = self.read_reply()?;
        if reply.code == 250 {
            Ok(reply)
        } else {
            Err(Error::Rejected {
                command: "DATA".into(),
                reply,
            })
        }
    }

    /// End the session.
    pub fn quit(mut self) {
        let _ = self.command("QUIT");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;

    /// A scripted fake server on localhost; returns what the client sent.
    fn fake_server(script: Vec<(&'static str, &'static str)>) -> (u16, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let h = thread::spawn(move || {
            let (sock, _) = listener.accept().unwrap();
            let mut w = sock.try_clone().unwrap();
            let mut r = BufReader::new(sock);
            let mut log = String::new();
            w.write_all(b"220 fake ESMTP\r\n").unwrap();
            for (expect, answer) in script {
                let mut line = String::new();
                if expect == "DATA-BODY" {
                    loop {
                        line.clear();
                        r.read_line(&mut line).unwrap();
                        log.push_str(&line);
                        if line == ".\r\n" {
                            break;
                        }
                    }
                } else {
                    r.read_line(&mut line).unwrap();
                    log.push_str(&line);
                    assert!(line.starts_with(expect), "{line:?} vs {expect:?}");
                }
                w.write_all(answer.as_bytes()).unwrap();
            }
            log
        });
        (port, h)
    }

    #[test]
    fn sends_with_auth_plain() {
        let (port, h) = fake_server(vec![
            (
                "EHLO",
                "250-fake\r\n250-AUTH LOGIN PLAIN\r\n250 8BITMIME\r\n",
            ),
            ("AUTH PLAIN", "235 ok\r\n"),
            ("MAIL FROM:<a@x>", "250 ok\r\n"),
            ("RCPT TO:<b@x>", "250 ok\r\n"),
            ("DATA", "354 go\r\n"),
            ("DATA-BODY", "250 queued as 42\r\n"),
            ("QUIT", "221 bye\r\n"),
        ]);
        let cfg = Config {
            host: "127.0.0.1".into(),
            port,
            encryption: Encryption::None,
            domain: "client.test".into(),
            tls: TlsOptions::default(),
            timeout: Some(Duration::from_secs(10)),
        };
        let mut c = Client::connect(&cfg, None).unwrap();
        assert_eq!(c.auth_mechanisms(), vec!["LOGIN", "PLAIN"]);
        c.authenticate("PLAIN", "u", "p").unwrap();
        let reply = c
            .send("a@x", &["b@x".to_string()], b"Subject: s\n\n.dot\nend\n")
            .unwrap();
        assert_eq!(reply.code, 250);
        c.quit();
        let log = h.join().unwrap();
        assert!(log.contains("\r\n..dot\r\nend\r\n.\r\n"), "{log}");
        assert!(log.contains("EHLO client.test\r\n"));
    }

    #[test]
    fn auth_login_and_rejection() {
        let (port, h) = fake_server(vec![
            ("EHLO", "250-fake\r\n250 AUTH LOGIN\r\n"),
            ("AUTH LOGIN", "334 VXNlcm5hbWU6\r\n"),
            ("dQ==", "334 UGFzc3dvcmQ6\r\n"),
            ("cA==", "535 bad credentials\r\n"),
        ]);
        let cfg = Config {
            host: "127.0.0.1".into(),
            port,
            encryption: Encryption::None,
            domain: "d".into(),
            tls: TlsOptions::default(),
            timeout: Some(Duration::from_secs(10)),
        };
        let mut c = Client::connect(&cfg, None).unwrap();
        match c.authenticate("LOGIN", "u", "p") {
            Err(Error::Rejected { reply, .. }) => assert_eq!(reply.code, 535),
            other => panic!("{other:?}"),
        }
        drop(c);
        h.join().unwrap();
    }

    #[test]
    fn starttls_requires_the_extension() {
        let (port, h) = fake_server(vec![("EHLO", "250 fake\r\n")]);
        let cfg = Config {
            host: "127.0.0.1".into(),
            port,
            encryption: Encryption::StartTls,
            domain: "d".into(),
            tls: TlsOptions::default(),
            timeout: Some(Duration::from_secs(10)),
        };
        assert!(matches!(
            Client::connect(&cfg, None),
            Err(Error::Unsupported(_))
        ));
        h.join().unwrap();
    }
}
