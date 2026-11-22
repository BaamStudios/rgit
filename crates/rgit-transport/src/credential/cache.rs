// SPDX-License-Identifier: MIT
//! The in-memory store of `git credential-cache--daemon` and the requests
//! `git credential-cache` sends it (`Documentation/git-credential-cache.adoc`,
//! `git-credential-cache--daemon.adoc`).
//!
//! The wire format was observed with git 2.56 (a recording socket server
//! and a scripted client): the client sends `action=<action>`,
//! `timeout=<seconds>` and then its standard input unchanged, closes its
//! writing side and copies the answer to its standard output. The daemon
//! answers `get` with `capability[]=authtype` followed by the stored
//! attributes, and every other action with nothing.

use std::io::BufRead;
use std::time::{Duration, Instant};

use bstr::{BString, ByteSlice};

use super::{Credential, ReadError, Source, strip_cr};
use crate::messages;

/// `--timeout` default (seconds).
pub const DEFAULT_TIMEOUT: i64 = 900;

/// A parsed client request.
#[derive(Debug)]
pub struct Request {
    /// `action=` value.
    pub action: BString,
    /// `timeout=` value in seconds.
    pub timeout: i64,
    /// The credential that follows.
    pub credential: Credential,
}

/// The request bytes the client sends.
pub fn encode_request(action: &str, timeout: i64, input: &[u8]) -> Vec<u8> {
    let mut out = format!("action={action}\ntimeout={timeout}\n").into_bytes();
    out.extend_from_slice(input);
    out
}

/// Parse a request; `Err` carries the daemon's complaint (printed only
/// with `--debug`).
pub fn parse_request(data: &[u8]) -> Result<Request, String> {
    let data = strip_cr(data);
    let mut reader: &[u8] = &data;
    let mut line = Vec::new();
    let mut next_line = |reader: &mut &[u8]| {
        line.clear();
        let _ = reader.read_until(b'\n', &mut line);
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        line.clone()
    };
    let first = next_line(&mut reader);
    let Some(action) = first.strip_prefix(b"action=") else {
        return Err(messages::cache_bogus_action(&first.to_str_lossy()));
    };
    let second = next_line(&mut reader);
    let Some(timeout) = second.strip_prefix(b"timeout=") else {
        return Err(messages::cache_bogus_timeout(&second.to_str_lossy()));
    };
    let timeout = parse_seconds(timeout);
    let mut credential = Credential::new();
    match credential.read_from(&mut reader, Source::Caller) {
        Ok(()) => {}
        Err(ReadError::InvalidLine) => {
            return Err(messages::UNABLE_TO_READ_HELPER_CREDENTIAL.into());
        }
        Err(ReadError::Fatal(e)) => return Err(e.to_string()),
    }
    Ok(Request {
        action: action.into(),
        timeout,
        credential,
    })
}

/// `atoi`-like: optional sign, then digits; anything else ends the number.
fn parse_seconds(v: &[u8]) -> i64 {
    let v = v.trim_ascii_start();
    let (neg, digits) = match v.first() {
        Some(b'-') => (true, &v[1..]),
        Some(b'+') => (false, &v[1..]),
        _ => (false, v),
    };
    let mut n: i64 = 0;
    for &d in digits.iter().take_while(|b| b.is_ascii_digit()) {
        n = n.saturating_mul(10).saturating_add(i64::from(d - b'0'));
    }
    if neg { -n } else { n }
}

struct Entry {
    credential: Credential,
    expires: Instant,
}

/// What the daemon should do after answering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum After {
    /// Keep serving.
    Continue,
    /// The client asked the daemon to exit.
    Exit,
}

/// The credentials held by the daemon.
#[derive(Default)]
pub struct Cache {
    entries: Vec<Entry>,
}

impl Cache {
    /// An empty cache.
    pub fn new() -> Self {
        Cache::default()
    }

    /// Number of held credentials.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// No credentials held.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Forget expired credentials; returns how many were dropped.
    pub fn prune(&mut self, now: Instant) -> usize {
        let before = self.entries.len();
        self.entries.retain(|e| e.expires > now);
        before - self.entries.len()
    }

    /// When the next credential expires.
    pub fn next_expiry(&self) -> Option<Instant> {
        self.entries.iter().map(|e| e.expires).min()
    }

    /// Handle one request; the answer goes to `out`. Unknown actions are
    /// reported through `Err` (with an empty answer).
    pub fn handle(
        &mut self,
        req: &Request,
        now: Instant,
        out: &mut Vec<u8>,
    ) -> Result<After, String> {
        self.prune(now);
        let want = &req.credential;
        match req.action.as_bytes() {
            b"get" => {
                if let Some(e) = self
                    .entries
                    .iter()
                    .find(|e| want.matches(&e.credential, false))
                {
                    write_answer(&e.credential, want.capa_authtype.caller, out);
                }
            }
            b"store" => {
                // Ephemeral credentials and those without user name and
                // password (or a pre-encoded credential) are not kept.
                let usable = (want.username.is_some() && want.password.is_some())
                    || want.credential.is_some();
                if usable && !want.ephemeral {
                    self.entries.retain(|e| !want.matches(&e.credential, false));
                    let secs = u64::try_from(req.timeout).unwrap_or(0);
                    self.entries.push(Entry {
                        credential: stored_copy(want),
                        expires: now + Duration::from_secs(secs),
                    });
                }
            }
            b"erase" => self.entries.retain(|e| !want.matches(&e.credential, true)),
            b"exit" => return Ok(After::Exit),
            other => return Err(messages::cache_unknown_action(&other.to_str_lossy())),
        }
        Ok(After::Continue)
    }
}

/// The attributes the daemon keeps.
fn stored_copy(c: &Credential) -> Credential {
    Credential {
        protocol: c.protocol.clone(),
        host: c.host.clone(),
        path: c.path.clone(),
        username: c.username.clone(),
        password: c.password.clone(),
        password_expiry_utc: c.password_expiry_utc,
        oauth_refresh_token: c.oauth_refresh_token.clone(),
        authtype: c.authtype.clone(),
        credential: c.credential.clone(),
        ..Credential::default()
    }
}

fn write_answer(have: &Credential, authtype: bool, out: &mut Vec<u8>) {
    let mut put = |k: &str, v: &[u8]| {
        out.extend_from_slice(k.as_bytes());
        out.push(b'=');
        out.extend_from_slice(v);
        out.push(b'\n');
    };
    put("capability[]", b"authtype");
    if authtype {
        if let Some(v) = &have.authtype {
            put("authtype", v);
        }
        if let Some(v) = &have.credential {
            put("credential", v);
        }
    }
    if let Some(v) = &have.username {
        put("username", v);
    }
    if let Some(v) = &have.password {
        put("password", v);
    }
    if have.password_expiry_utc != 0 {
        put(
            "password_expiry_utc",
            have.password_expiry_utc.to_string().as_bytes(),
        );
    }
    if let Some(v) = &have.oauth_refresh_token {
        put("oauth_refresh_token", v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(cache: &mut Cache, now: Instant, req: &str) -> String {
        let req = parse_request(req.as_bytes()).unwrap();
        let mut out = Vec::new();
        cache.handle(&req, now, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn store_get_erase() {
        let mut c = Cache::new();
        let t = Instant::now();
        run(
            &mut c,
            t,
            "action=store\ntimeout=100\nprotocol=https\nhost=h\nusername=u\npassword=p\npassword_expiry_utc=99\noauth_refresh_token=t\nwwwauth[]=w\n",
        );
        assert_eq!(
            run(
                &mut c,
                t,
                "action=get\ntimeout=100\nprotocol=https\nhost=h\n"
            ),
            "capability[]=authtype\nusername=u\npassword=p\npassword_expiry_utc=99\noauth_refresh_token=t\n"
        );
        assert_eq!(
            run(
                &mut c,
                t,
                "action=get\ntimeout=100\nprotocol=https\nhost=h\npath=x\n"
            ),
            ""
        );
        run(&mut c, t, "action=erase\ntimeout=1\nhost=h\npassword=zz\n");
        assert_eq!(c.len(), 1);
        run(&mut c, t, "action=erase\ntimeout=1\nhost=h\n");
        assert!(c.is_empty());
    }

    #[test]
    fn authtype_and_ephemeral() {
        let mut c = Cache::new();
        let t = Instant::now();
        run(
            &mut c,
            t,
            "action=store\ntimeout=100\ncapability[]=authtype\nauthtype=Bearer\ncredential=tok\nprotocol=https\nhost=g\n",
        );
        assert_eq!(
            run(
                &mut c,
                t,
                "action=get\ntimeout=100\ncapability[]=authtype\nprotocol=https\nhost=g\n"
            ),
            "capability[]=authtype\nauthtype=Bearer\ncredential=tok\n"
        );
        assert_eq!(
            run(
                &mut c,
                t,
                "action=get\ntimeout=100\nprotocol=https\nhost=g\n"
            ),
            "capability[]=authtype\n"
        );
        run(
            &mut c,
            t,
            "action=store\ntimeout=100\nprotocol=https\nhost=e\nusername=u\npassword=p\nephemeral=1\n",
        );
        run(
            &mut c,
            t,
            "action=store\ntimeout=100\nprotocol=https\nhost=n\nusername=u\n",
        );
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn expiry_and_bogus() {
        let mut c = Cache::new();
        let t = Instant::now();
        run(
            &mut c,
            t,
            "action=store\ntimeout=1\nprotocol=https\nhost=h\nusername=u\npassword=p\n",
        );
        assert_eq!(c.next_expiry(), Some(t + Duration::from_secs(1)));
        assert_eq!(c.prune(t + Duration::from_secs(2)), 1);
        assert!(parse_request(b"garbage\n").is_err());
        assert!(parse_request(b"timeout=1\naction=get\n").is_err());
        assert!(parse_request(b"action=get\nprotocol=https\n").is_err());
        assert_eq!(
            parse_request(b"action=get\r\ntimeout=-5\r\n")
                .unwrap()
                .timeout,
            -5
        );
        assert_eq!(
            encode_request("get", 900, b"x=y\n"),
            b"action=get\ntimeout=900\nx=y\n"
        );
    }
}
