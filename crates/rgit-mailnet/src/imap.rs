// SPDX-License-Identifier: MIT
//! A minimal IMAP client (RFC 9051 / RFC 3501) for uploading messages:
//! greeting, CAPABILITY, STARTTLS, LOGIN, AUTHENTICATE (PLAIN, XOAUTH2,
//! OAUTHBEARER), LIST, EXAMINE, APPEND and LOGOUT.

use std::fmt;
use std::io;

use crate::encoding;
use crate::stream::{LineStream, Log, Stream};
use crate::tls::TlsOptions;

/// The tagged status of a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    No,
    Bad,
}

/// The response to one command.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: Status,
    /// The text after the status of the tagged line.
    pub text: String,
    /// Untagged lines (`* ...`), literals inlined.
    pub untagged: Vec<String>,
}

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    /// The server answered NO or BAD.
    Failed {
        command: String,
        response: Response,
    },
    /// The server closed the session (BYE) or said something unexpected.
    Protocol(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Failed { command, response } => {
                write!(f, "{command} failed: {}", response.text)
            }
            Error::Protocol(s) => write!(f, "{s}"),
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

/// Quote `s` as an IMAP quoted string.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// An open IMAP session.
pub struct Client {
    conn: LineStream,
    tag: u32,
    capabilities: Vec<String>,
    preauth: bool,
    verbose: Option<Log>,
}

impl Client {
    /// Start a session on `stream`: read the greeting and the capabilities.
    pub fn start(stream: Stream, verbose: Option<Log>) -> Result<Client> {
        let mut c = Client {
            conn: LineStream::new(stream),
            tag: 0,
            capabilities: Vec::new(),
            preauth: false,
            verbose,
        };
        let greeting = c.read_line()?;
        let upper = greeting.to_ascii_uppercase();
        if upper.starts_with("* PREAUTH") {
            c.preauth = true;
        } else if upper.starts_with("* BYE") {
            return Err(Error::Protocol(greeting));
        } else if !upper.starts_with("* OK") {
            return Err(Error::Protocol(format!("unexpected greeting: {greeting}")));
        }
        c.refresh_capabilities()?;
        Ok(c)
    }

    fn log(&mut self, line: &str) {
        if let Some(v) = self.verbose.as_mut() {
            v(line);
        }
    }

    /// Whether the server said PREAUTH (no login needed).
    pub fn preauthenticated(&self) -> bool {
        self.preauth
    }

    pub fn has_capability(&self, cap: &str) -> bool {
        self.capabilities
            .iter()
            .any(|c| c.eq_ignore_ascii_case(cap))
    }

    pub fn is_tls(&self) -> bool {
        self.conn.is_tls()
    }

    fn refresh_capabilities(&mut self) -> Result<()> {
        let r = self.command("CAPABILITY")?;
        self.capabilities = r
            .untagged
            .iter()
            .filter_map(|l| {
                let rest = l.strip_prefix("* ")?;
                let (word, caps) = rest.split_once(' ').unwrap_or((rest, ""));
                word.eq_ignore_ascii_case("CAPABILITY").then_some(caps)
            })
            .flat_map(|caps| caps.split_ascii_whitespace().map(str::to_string))
            .collect();
        Ok(())
    }

    /// Upgrade the connection with STARTTLS.
    pub fn starttls(&mut self, host: &str, tls: &TlsOptions) -> Result<()> {
        self.command("STARTTLS")?;
        self.conn.start_tls(host, tls)?;
        self.refresh_capabilities()
    }

    /// Read one line; a literal (`{n}` at the end) is read and appended,
    /// and the line continues after it.
    fn read_line(&mut self) -> Result<String> {
        let mut out = Vec::new();
        loop {
            let line = self.conn.read_line()?;
            out.extend_from_slice(&line);
            let Some(n) = literal_size(&line) else { break };
            out.extend_from_slice(&self.conn.read_exact_bytes(n)?);
        }
        let s = String::from_utf8_lossy(&out).into_owned();
        self.log(&s);
        Ok(s)
    }

    fn next_tag(&mut self) -> String {
        self.tag += 1;
        self.tag.to_string()
    }

    fn send_line(&mut self, line: &str, shown: &str) -> Result<()> {
        self.log(&format!(">>> {shown}"));
        self.conn.send(format!("{line}\r\n").as_bytes())?;
        Ok(())
    }

    /// Read untagged lines up to the tagged completion of `tag`.
    fn complete(&mut self, tag: &str, command: &str) -> Result<Response> {
        let mut untagged = Vec::new();
        loop {
            let line = self.read_line()?;
            if line.starts_with("* ") || line == "*" {
                if line.to_ascii_uppercase().starts_with("* BYE") && command != "LOGOUT" {
                    return Err(Error::Protocol(line));
                }
                untagged.push(line);
                continue;
            }
            if line.starts_with('+') {
                // A continuation request where none is expected.
                continue;
            }
            let Some(rest) = line.strip_prefix(tag).and_then(|r| r.strip_prefix(' ')) else {
                continue;
            };
            let (word, text) = rest.split_once(' ').unwrap_or((rest, ""));
            let status = match word.to_ascii_uppercase().as_str() {
                "OK" => Status::Ok,
                "NO" => Status::No,
                _ => Status::Bad,
            };
            let response = Response {
                status,
                text: text.to_string(),
                untagged,
            };
            return if status == Status::Ok {
                Ok(response)
            } else {
                Err(Error::Failed {
                    command: command.to_string(),
                    response,
                })
            };
        }
    }

    /// Run a command without literals.
    pub fn command(&mut self, command: &str) -> Result<Response> {
        let shown = command.to_string();
        self.command_shown(command, &shown)
    }

    fn command_shown(&mut self, command: &str, shown: &str) -> Result<Response> {
        let tag = self.next_tag();
        self.send_line(&format!("{tag} {command}"), &format!("{tag} {shown}"))?;
        let name = command.split(' ').next().unwrap_or("").to_string();
        self.complete(&tag, &name)
    }

    /// LOGIN with a user name and password.
    pub fn login(&mut self, user: &str, pass: &str) -> Result<Response> {
        let cmd = format!("LOGIN {} {}", quote(user), quote(pass));
        self.command_shown(&cmd, "LOGIN <user> <pass>")
    }

    /// AUTHENTICATE with PLAIN, XOAUTH2 or OAUTHBEARER.
    pub fn authenticate(&mut self, mechanism: &str, user: &str, pass: &str) -> Result<Response> {
        let mech = mechanism.to_ascii_uppercase();
        let response = match mech.as_str() {
            "PLAIN" => encoding::sasl_plain(user, pass),
            "XOAUTH2" => encoding::sasl_xoauth2(user, pass),
            "OAUTHBEARER" => encoding::sasl_oauthbearer(user, pass),
            _ => {
                return Err(Error::Protocol(format!(
                    "unsupported authentication method {mech}"
                )));
            }
        };
        let tag = self.next_tag();
        self.send_line(
            &format!("{tag} AUTHENTICATE {mech}"),
            &format!("{tag} AUTHENTICATE {mech}"),
        )?;
        let line = self.read_line()?;
        if !line.starts_with('+') {
            return Err(Error::Protocol(format!(
                "AUTHENTICATE {mech}: unexpected answer: {line}"
            )));
        }
        self.send_line(&response, "<secret>")?;
        // A failure challenge (XOAUTH2) is answered with an empty line.
        let tagged = loop {
            let line = self.read_line()?;
            if line.starts_with('+') {
                self.send_line("", "")?;
                continue;
            }
            break line;
        };
        let mut untagged = Vec::new();
        let mut line = tagged;
        loop {
            if line.starts_with("* ") {
                untagged.push(line);
                line = self.read_line()?;
                continue;
            }
            break;
        }
        let ok = line
            .strip_prefix(&tag)
            .and_then(|r| r.strip_prefix(' '))
            .is_some_and(|r| r.to_ascii_uppercase().starts_with("OK"));
        let text = line.split_once(' ').map_or("", |x| x.1).to_string();
        let response = Response {
            status: if ok { Status::Ok } else { Status::No },
            text,
            untagged,
        };
        if ok {
            Ok(response)
        } else {
            Err(Error::Failed {
                command: format!("AUTHENTICATE {mech}"),
                response,
            })
        }
    }

    /// LIST "" "*": the untagged LIST lines.
    pub fn list(&mut self) -> Result<Vec<String>> {
        let r = self.command("LIST \"\" \"*\"")?;
        Ok(r.untagged)
    }

    /// APPEND `message` (CRLF line endings) to `folder`.
    pub fn append(&mut self, folder: &str, flags: &[&str], message: &[u8]) -> Result<Response> {
        let tag = self.next_tag();
        let flags = if flags.is_empty() {
            String::new()
        } else {
            format!("({}) ", flags.join(" "))
        };
        let plus = self.has_capability("LITERAL+");
        let line = format!(
            "{tag} APPEND {} {flags}{{{}{}}}",
            quote(folder),
            message.len(),
            if plus { "+" } else { "" }
        );
        self.send_line(&line, &line)?;
        if !plus {
            loop {
                let l = self.read_line()?;
                if l.starts_with('+') {
                    break;
                }
                if l.starts_with("* ") {
                    continue;
                }
                return Err(Error::Protocol(format!("APPEND: {l}")));
            }
        }
        let mut data = message.to_vec();
        data.extend_from_slice(b"\r\n");
        self.conn.send(&data)?;
        self.complete(&tag, "APPEND")
    }

    /// LOGOUT and close.
    pub fn logout(mut self) -> Result<()> {
        self.command("LOGOUT").map(|_| ())
    }
}

/// The size of a literal announced at the end of `line` (`{n}` or `{n+}`).
fn literal_size(line: &[u8]) -> Option<usize> {
    let inner = line.strip_suffix(b"}")?;
    let open = inner.iter().rposition(|&b| b == b'{')?;
    let digits = &inner[open + 1..];
    let digits = digits.strip_suffix(b"+").unwrap_or(digits);
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

/// Convert `\n` line endings to `\r\n` (an existing `\r\n` stays).
pub fn to_crlf(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 32);
    let mut prev = 0u8;
    for &b in data {
        if b == b'\n' && prev != b'\r' {
            out.push(b'\r');
        }
        out.push(b);
        prev = b;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn quoting() {
        assert_eq!(quote(r#"a"b\c"#), r#""a\"b\\c""#);
    }

    #[test]
    fn literal_sizes() {
        assert_eq!(literal_size(b"* LIST () \"/\" {5}"), Some(5));
        assert_eq!(literal_size(b"1 APPEND x {12+}"), Some(12));
        assert_eq!(literal_size(b"nothing"), None);
    }

    #[test]
    fn crlf_conversion() {
        assert_eq!(to_crlf(b"a\nb\r\nc"), b"a\r\nb\r\nc".to_vec());
    }

    #[test]
    fn login_list_append() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let h = thread::spawn(move || {
            let (sock, _) = listener.accept().unwrap();
            let mut w = sock.try_clone().unwrap();
            let mut r = BufReader::new(sock);
            let mut log = String::new();
            w.write_all(b"* OK ready\r\n").unwrap();
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                log.push_str(&line);
                let (tag, rest) = line.trim_end().split_once(' ').unwrap();
                let reply = if rest.starts_with("CAPABILITY") {
                    format!("* CAPABILITY IMAP4rev1\r\n{tag} OK done\r\n")
                } else if rest.starts_with("LOGIN") {
                    format!("{tag} OK in\r\n")
                } else if rest.starts_with("LIST") {
                    format!("* LIST () \"/\" {{6}}\r\nDrafts\r\n{tag} OK listed\r\n")
                } else if rest.starts_with("APPEND") {
                    let n: usize = rest
                        .rsplit('{')
                        .next()
                        .unwrap()
                        .trim_end_matches('}')
                        .parse()
                        .unwrap();
                    w.write_all(b"+ go\r\n").unwrap();
                    let mut buf = vec![0; n + 2];
                    r.read_exact(&mut buf).unwrap();
                    log.push_str(&String::from_utf8_lossy(&buf));
                    format!("{tag} OK appended\r\n")
                } else if rest.starts_with("LOGOUT") {
                    w.write_all(format!("* BYE\r\n{tag} OK bye\r\n").as_bytes())
                        .unwrap();
                    break;
                } else {
                    format!("{tag} BAD what\r\n")
                };
                w.write_all(reply.as_bytes()).unwrap();
            }
            log
        });
        let stream = Stream::connect("127.0.0.1", port, Some(Duration::from_secs(10))).unwrap();
        let mut c = Client::start(stream, None).unwrap();
        assert!(c.has_capability("imap4rev1"));
        c.login("bob", "s\"x").unwrap();
        let list = c.list().unwrap();
        assert_eq!(list, vec!["* LIST () \"/\" {6}Drafts".to_string()]);
        c.append("Drafts", &["\\Draft"], b"Subject: x\r\n\r\nbody\r\n")
            .unwrap();
        c.logout().unwrap();
        let log = h.join().unwrap();
        assert!(log.contains("LOGIN \"bob\" \"s\\\"x\"\r\n"), "{log}");
        assert!(
            log.contains("APPEND \"Drafts\" (\\Draft) {20}\r\n"),
            "{log}"
        );
        assert!(log.contains("Subject: x\r\n\r\nbody\r\n\r\n"), "{log}");
    }
}
