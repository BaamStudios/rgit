// SPDX-License-Identifier: MIT
//! Credentials: the helper protocol, `credential.*` configuration,
//! prompting, and the operations behind `git credential fill`, `approve`
//! and `reject`.
//!
//! Spec: `Documentation/gitcredentials.adoc`, `git-credential.adoc`,
//! `config/credential.adoc`. Details (attribute order, which attributes
//! go where, messages) were observed with git 2.56 and recording helper
//! scripts; they are noted at the code that implements them.

mod pattern;
mod prompt;

pub use pattern::Pattern;
pub use prompt::{Prompter, SystemPrompter, askpass_program};

use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

use bstr::{BStr, BString, ByteSlice};
use rgit_config::{ConfigSet, Entry, Environment, Origin};

use crate::command::Command;
use crate::os::os_string;
use crate::url::percent_decode;
use crate::{Error, Result, messages};

/// A capability (`authtype`, `state`) as announced by both sides.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capability {
    /// The caller (standard input of `git credential`) supports it.
    pub caller: bool,
    /// A helper announced it in its answer.
    pub helper: bool,
}

/// Who receives a written credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    /// A helper: gets everything the caller's capabilities allow, plus
    /// `wwwauth[]`.
    Helper,
    /// The caller (`git credential fill` output): capability-dependent
    /// attributes only when both the caller and a helper support them.
    Caller,
}

/// Where a credential being read comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The caller (`git credential` standard input).
    Caller,
    /// A helper's answer.
    Helper,
}

/// Settings from `credential.*` that apply to one credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialConfig {
    /// `credential.helper` list (an empty value resets it).
    pub helpers: Vec<BString>,
    /// `credential.useHttpPath`.
    pub use_http_path: bool,
    /// `credential.interactive`.
    pub interactive: bool,
    /// `credential.sanitizePrompt`.
    pub sanitize_prompt: bool,
    /// `credential.protectProtocol`.
    pub protect_protocol: bool,
}

impl Default for CredentialConfig {
    fn default() -> Self {
        CredentialConfig {
            helpers: Vec::new(),
            use_http_path: false,
            interactive: true,
            sanitize_prompt: true,
            protect_protocol: true,
        }
    }
}

/// A credential and its context.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Credential {
    /// `protocol`.
    pub protocol: Option<BString>,
    /// `host` (with port).
    pub host: Option<BString>,
    /// `path`.
    pub path: Option<BString>,
    /// `username`.
    pub username: Option<BString>,
    /// `password`.
    pub password: Option<BString>,
    /// `password_expiry_utc`; 0 = none.
    pub password_expiry_utc: u64,
    /// `oauth_refresh_token`.
    pub oauth_refresh_token: Option<BString>,
    /// `authtype`.
    pub authtype: Option<BString>,
    /// `credential` (pre-encoded).
    pub credential: Option<BString>,
    /// `ephemeral`.
    pub ephemeral: bool,
    /// `continue` (multistage authentication).
    pub multistage: bool,
    /// `wwwauth[]`.
    pub wwwauth: Vec<BString>,
    /// `state[]`.
    pub state: Vec<BString>,
    /// `capability[]=authtype`.
    pub capa_authtype: Capability,
    /// `capability[]=state`.
    pub capa_state: Capability,
    /// A helper said `quit`.
    pub quit: bool,
    /// The configuration that was applied, once [`Credential::apply_config`]
    /// ran.
    pub config: Option<CredentialConfig>,
}

/// What credential operations need from the outside world.
pub struct CredentialContext<'a> {
    /// The environment (`GIT_ASKPASS`, `SSH_ASKPASS`,
    /// `GIT_TERMINAL_PROMPT`).
    pub env: &'a dyn Environment,
    /// The configuration.
    pub config: &'a ConfigSet,
    /// Extra environment for helpers and askpass (the global options'
    /// exports, `GIT_EXEC_PATH`, `PATH`).
    pub base_env: Vec<(OsString, OsString)>,
    /// How to ask the user.
    pub prompter: &'a mut dyn Prompter,
}

/// Why reading a credential stopped early.
#[derive(Debug)]
pub enum ReadError {
    /// A line without `=` (the warning was printed).
    InvalidLine,
    /// A fatal problem (bad `url=`), with git's message.
    Fatal(Error),
}

fn fatal(msg: impl Into<String>) -> Error {
    Error::Other(msg.into())
}

fn warn(msg: &str) {
    let _ = writeln!(io::stderr(), "warning: {msg}");
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `strtoumax` on the value: leading blanks, then as many digits as there
/// are; anything else gives 0.
fn parse_expiry(v: &[u8]) -> u64 {
    let v = v.trim_ascii_start();
    let digits = v.iter().take_while(|b| b.is_ascii_digit()).count();
    let mut n: u64 = 0;
    for d in &v[..digits] {
        n = n.saturating_mul(10).saturating_add(u64::from(d - b'0'));
    }
    n
}

fn parse_flag(v: &[u8]) -> bool {
    rgit_config::value::parse_bool(Some(v)).unwrap_or(false)
}

/// Components of a credential URL (`url=` attribute).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UrlParts {
    /// Scheme, not decoded.
    pub protocol: BString,
    /// Decoded host (possibly empty).
    pub host: BString,
    /// Decoded user name, if the URL has `user@`.
    pub username: Option<BString>,
    /// Decoded password, if the URL has `user:password@`.
    pub password: Option<BString>,
    /// Decoded path without leading and trailing slashes; `None` if empty.
    pub path: Option<BString>,
}

/// Split the part after `://` into user, password, host and path, decoded.
fn split_authority(rest: &[u8]) -> UrlParts {
    let end = rest.find_byteset(b"/?#").unwrap_or(rest.len());
    let authority = &rest[..end];
    let (userinfo, host) = match authority.rfind_byte(b'@') {
        Some(at) => (Some(&authority[..at]), &authority[at + 1..]),
        None => (None, authority),
    };
    let (username, password) = match userinfo {
        Some(ui) => match ui.find_byte(b':') {
            Some(c) => (
                Some(percent_decode(&ui[..c])),
                Some(percent_decode(&ui[c + 1..])),
            ),
            None => (Some(percent_decode(ui)), None),
        },
        None => (None, None),
    };
    let mut path = &rest[end..];
    while path.first() == Some(&b'/') {
        path = &path[1..];
    }
    while path.last() == Some(&b'/') {
        path = &path[..path.len() - 1];
    }
    UrlParts {
        protocol: BString::default(),
        host: percent_decode(host),
        username,
        password,
        path: (!path.is_empty()).then(|| percent_decode(path)),
    }
}

/// Reject decoded components with a newline (warning printed).
fn check_newlines(parts: &UrlParts, url: &[u8]) -> bool {
    let url = url.to_str_lossy();
    let checks: [(&str, Option<&BString>); 4] = [
        ("username", parts.username.as_ref()),
        ("password", parts.password.as_ref()),
        ("host", Some(&parts.host)),
        ("path", parts.path.as_ref()),
    ];
    for (name, v) in checks {
        if v.is_some_and(|v| v.contains(&b'\n')) {
            warn(&messages::url_newline(name, &url));
            return false;
        }
    }
    true
}

/// Parse a credential URL like `git credential` does for `url=`. Prints
/// git's warning and returns `None` if it cannot be used.
pub fn parse_url(url: &[u8]) -> Option<UrlParts> {
    let Some(sep) = url.find(b"://").filter(|&i| i > 0) else {
        warn(&messages::url_has_no_scheme(&url.to_str_lossy()));
        return None;
    };
    let mut parts = split_authority(&url[sep + 3..]);
    parts.protocol = url[..sep].into();
    if !check_newlines(&parts, url) {
        return None;
    }
    Some(parts)
}

impl Credential {
    /// An empty credential.
    pub fn new() -> Self {
        Credential::default()
    }

    /// Apply a `url=` value: the URL replaces protocol, host, path, user
    /// name and password (missing parts become unset).
    pub fn set_url(&mut self, url: &[u8]) -> Result<()> {
        self.protocol = None;
        self.host = None;
        self.path = None;
        self.username = None;
        self.password = None;
        let Some(p) = parse_url(url) else {
            return Err(fatal(messages::url_cannot_be_parsed(&url.to_str_lossy())));
        };
        self.protocol = Some(p.protocol);
        self.host = Some(p.host);
        self.username = p.username;
        self.password = p.password;
        self.path = p.path;
        Ok(())
    }

    /// Read `key=value` lines up to an empty line or end of input.
    pub fn read_from(&mut self, input: &mut dyn BufRead, source: Source) -> Result<(), ReadError> {
        let mut line = Vec::new();
        loop {
            line.clear();
            match input.read_until(b'\n', &mut line) {
                Ok(0) => return Ok(()),
                Ok(_) => {}
                Err(e) => return Err(ReadError::Fatal(e.into())),
            }
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            if line.is_empty() {
                return Ok(());
            }
            let Some(eq) = line.find_byte(b'=') else {
                warn(&messages::invalid_credential_line(&line.to_str_lossy()));
                return Err(ReadError::InvalidLine);
            };
            let (key, value) = (&line[..eq], &line[eq + 1..]);
            self.set_attribute(key, value, source)
                .map_err(ReadError::Fatal)?;
        }
    }

    fn set_attribute(&mut self, key: &[u8], value: &[u8], source: Source) -> Result<()> {
        let v = || Some(BString::from(value));
        match key {
            b"protocol" => self.protocol = v(),
            b"host" => self.host = v(),
            b"path" => self.path = v(),
            b"username" => self.username = v(),
            b"password" => self.password = v(),
            b"password_expiry_utc" => self.password_expiry_utc = parse_expiry(value),
            b"oauth_refresh_token" => self.oauth_refresh_token = v(),
            b"authtype" => self.authtype = v(),
            b"credential" => self.credential = v(),
            b"ephemeral" => self.ephemeral = parse_flag(value),
            b"continue" => self.multistage = parse_flag(value),
            b"wwwauth[]" => self.wwwauth.push(value.into()),
            b"state[]" => self.state.push(value.into()),
            b"capability[]" => {
                let capa = match value {
                    b"authtype" => &mut self.capa_authtype,
                    b"state" => &mut self.capa_state,
                    _ => return Ok(()),
                };
                match source {
                    Source::Caller => capa.caller = true,
                    Source::Helper => capa.helper = true,
                }
            }
            b"url" => self.set_url(value)?,
            b"quit" => self.quit = parse_flag(value),
            _ => {}
        }
        Ok(())
    }

    fn capa_active(capa: Capability, audience: Audience) -> bool {
        match audience {
            Audience::Helper => capa.caller,
            Audience::Caller => capa.caller && capa.helper,
        }
    }

    /// Write the credential in the order git uses. Stops with an error at
    /// a value containing a newline, or a carriage return when
    /// `protect_protocol` is set; the lines before it are written.
    pub fn write_to(
        &self,
        out: &mut dyn Write,
        audience: Audience,
        protect_protocol: bool,
    ) -> Result<()> {
        let mut put = |key: &str, value: &[u8]| -> Result<()> {
            if value.contains(&b'\n') {
                return Err(fatal(messages::value_contains_newline(key)));
            }
            if protect_protocol && value.contains(&b'\r') {
                return Err(fatal(messages::value_contains_cr(key)));
            }
            out.write_all(key.as_bytes())?;
            out.write_all(b"=")?;
            out.write_all(value)?;
            out.write_all(b"\n")?;
            Ok(())
        };
        let authtype = Self::capa_active(self.capa_authtype, audience);
        let state = Self::capa_active(self.capa_state, audience);
        if authtype {
            put("capability[]", b"authtype")?;
        }
        if state {
            put("capability[]", b"state")?;
        }
        if authtype {
            if let Some(v) = &self.authtype {
                put("authtype", v)?;
            }
            if let Some(v) = &self.credential {
                put("credential", v)?;
            }
            if self.ephemeral {
                put("ephemeral", b"1")?;
            }
        }
        for (k, v) in [
            ("protocol", &self.protocol),
            ("host", &self.host),
            ("path", &self.path),
            ("username", &self.username),
            ("password", &self.password),
            ("oauth_refresh_token", &self.oauth_refresh_token),
        ] {
            if let Some(v) = v {
                put(k, v)?;
            }
        }
        if self.password_expiry_utc != 0 {
            put(
                "password_expiry_utc",
                self.password_expiry_utc.to_string().as_bytes(),
            )?;
        }
        if audience == Audience::Helper {
            for v in &self.wwwauth {
                put("wwwauth[]", v)?;
            }
        }
        if state {
            if self.multistage {
                put("continue", b"1")?;
            }
            for v in &self.state {
                put("state[]", v)?;
            }
        }
        Ok(())
    }

    /// The configuration in effect (defaults before [`apply_config`]).
    ///
    /// [`apply_config`]: Credential::apply_config
    pub fn settings(&self) -> CredentialConfig {
        self.config.clone().unwrap_or_default()
    }

    /// Read `credential.*` and `credential.<url>.*` once: helpers, default
    /// user name, `useHttpPath` (drops the path of http/https credentials
    /// unless set) and the prompt settings. Entries apply in configuration
    /// order; a later matching value wins.
    pub fn apply_config(&mut self, config: &ConfigSet) -> Result<()> {
        if self.config.is_some() {
            return Ok(());
        }
        let mut cc = CredentialConfig::default();
        let mut username: Option<BString> = None;
        for entry in config.entries() {
            let (section, sub, key) = rgit_config::key::split_canonical(entry.name.as_bstr());
            if section != "credential" {
                continue;
            }
            if let Some(sub) = sub {
                match Pattern::parse(sub) {
                    Some(p) => {
                        if !p.matches(self) {
                            continue;
                        }
                    }
                    None => {
                        warn(&messages::skipping_credential_lookup(&format!(
                            "credential.{}",
                            sub.to_str_lossy()
                        )));
                        continue;
                    }
                }
            }
            match key.as_bytes() {
                b"helper" => {
                    let v = required_value(entry)?;
                    if v.is_empty() {
                        cc.helpers.clear();
                    } else {
                        cc.helpers.push(v.into());
                    }
                }
                b"username" => username = Some(required_value(entry)?.into()),
                b"usehttppath" => cc.use_http_path = config_bool(entry)?,
                b"sanitizeprompt" => cc.sanitize_prompt = config_bool(entry)?,
                b"protectprotocol" => cc.protect_protocol = config_bool(entry)?,
                b"interactive" => {
                    cc.interactive = match entry.value.as_deref().map(|v| v.as_bytes()) {
                        Some(b"never") => false,
                        v => rgit_config::value::parse_bool(v).unwrap_or(true),
                    }
                }
                _ => {}
            }
        }
        if self.username.is_none() {
            self.username = username;
        }
        if !cc.use_http_path
            && matches!(
                self.protocol.as_deref().map(|p| p.as_bytes()),
                Some(b"http" | b"https")
            )
        {
            self.path = None;
        }
        self.config = Some(cc);
        Ok(())
    }

    /// `username` and `password`, or a pre-encoded `credential`.
    pub fn is_complete(&self) -> bool {
        (self.username.is_some() && self.password.is_some()) || self.credential.is_some()
    }

    fn password_expired(&self) -> bool {
        self.password_expiry_utc != 0 && self.password_expiry_utc < now()
    }

    fn drop_expired_password(&mut self) {
        if self.password_expired() {
            self.password = None;
            self.password_expiry_utc = 0;
        }
    }

    fn check_context(&self) -> Result<()> {
        if self.host.is_none() {
            return Err(fatal(messages::MISSING_HOST));
        }
        if self.protocol.is_none() {
            return Err(fatal(messages::MISSING_PROTOCOL));
        }
        Ok(())
    }

    /// `git credential fill`: ask the helpers in turn until the credential
    /// is complete, then prompt for what is missing.
    ///
    /// A credential that is already complete is returned as is, without
    /// reading the configuration (so its `path` is kept).
    pub fn fill(&mut self, ctx: &mut CredentialContext<'_>) -> Result<()> {
        if self.is_complete() {
            return Ok(());
        }
        self.apply_config(ctx.config)?;
        self.check_context()?;
        let settings = self.settings();
        for helper in &settings.helpers {
            self.run_helper(helper, "get", ctx, settings.protect_protocol)?;
            if self.quit {
                return Err(fatal(messages::helper_quit(&helper.to_str_lossy())));
            }
            self.drop_expired_password();
            if self.is_complete() {
                return Ok(());
            }
        }
        if !settings.interactive {
            return Err(fatal(messages::UNABLE_TO_GET_PASSWORD));
        }
        if self.username.is_none() {
            let what = messages::username_for(&self.describe(false, settings.sanitize_prompt));
            self.username = Some(prompt::ask(ctx, &what, true)?);
        }
        if self.password.is_none() {
            let what = messages::password_for(&self.describe(true, settings.sanitize_prompt));
            self.password = Some(prompt::ask(ctx, &what, false)?);
        }
        Ok(())
    }

    /// `git credential approve`: hand a working credential to every helper
    /// (`store`). Credentials without user name and password (or a
    /// pre-encoded credential) and expired passwords are not stored.
    pub fn approve(&mut self, ctx: &mut CredentialContext<'_>) -> Result<()> {
        if !self.is_complete() || self.password_expired() {
            return Ok(());
        }
        self.apply_config(ctx.config)?;
        self.check_context()?;
        let settings = self.settings();
        for helper in &settings.helpers {
            self.run_helper(helper, "store", ctx, settings.protect_protocol)?;
        }
        Ok(())
    }

    /// `git credential reject`: ask every helper to `erase` the credential.
    pub fn reject(&mut self, ctx: &mut CredentialContext<'_>) -> Result<()> {
        self.apply_config(ctx.config)?;
        self.check_context()?;
        let settings = self.settings();
        for helper in &settings.helpers {
            self.run_helper(helper, "erase", ctx, settings.protect_protocol)?;
        }
        Ok(())
    }

    /// `<protocol>://[<user>@]<host>[/<path>]` for prompts; user name, host
    /// and path are percent-encoded when `sanitize` is set.
    pub fn describe(&self, with_user: bool, sanitize: bool) -> String {
        let mut out: Vec<u8> = Vec::new();
        let enc = |out: &mut Vec<u8>, v: &[u8], keep: &dyn Fn(u8) -> bool| {
            for &b in v {
                if !sanitize || keep(b) {
                    out.push(b);
                } else {
                    out.extend_from_slice(format!("%{b:02X}").as_bytes());
                }
            }
        };
        let unreserved = |b: u8| b.is_ascii_alphanumeric() || b"-._~".contains(&b);
        if let Some(p) = &self.protocol {
            out.extend_from_slice(p);
        }
        out.extend_from_slice(b"://");
        if with_user {
            if let Some(u) = self.username.as_ref().filter(|u| !u.is_empty()) {
                enc(&mut out, u, &unreserved);
                out.push(b'@');
            }
        }
        if let Some(h) = &self.host {
            enc(&mut out, h, &|b| {
                b.is_ascii_alphanumeric() || b"-.:[]".contains(&b)
            });
        }
        if let Some(p) = &self.path {
            out.push(b'/');
            enc(&mut out, p, &|b| unreserved(b) || b == b'/');
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// The shell command for a `credential.helper` value and operation.
    pub fn helper_command(helper: &[u8], op: &str) -> BString {
        let mut cmd: Vec<u8> = if let Some(snippet) = helper.strip_prefix(b"!") {
            snippet.to_vec()
        } else if helper.first() == Some(&b'/') {
            helper.to_vec()
        } else {
            let mut c = b"git credential-".to_vec();
            c.extend_from_slice(helper);
            c
        };
        cmd.push(b' ');
        cmd.extend_from_slice(op.as_bytes());
        cmd.into()
    }

    fn run_helper(
        &mut self,
        helper: &[u8],
        op: &str,
        ctx: &CredentialContext<'_>,
        protect_protocol: bool,
    ) -> Result<()> {
        // Serialize first: a bad value aborts before the helper starts.
        let mut input = Vec::new();
        self.write_to(&mut input, Audience::Helper, protect_protocol)?;
        let mut cmd = Command::shell(os_string(&Self::helper_command(helper, op)));
        cmd.base_env(&ctx.base_env);
        cmd.trace();
        let get = op == "get";
        let mut child = match cmd
            .to_std()
            .stdin(Stdio::piped())
            .stdout(if get { Stdio::piped() } else { Stdio::null() })
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                let _ = writeln!(
                    io::stderr(),
                    "error: {}",
                    messages::cannot_run(
                        &cmd.program_display(),
                        &rgit_trace::messages::strerror(&e)
                    )
                );
                return Ok(());
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            // A helper may exit without reading its input.
            let _ = stdin.write_all(&input);
        }
        let mut result = Ok(());
        if let Some(stdout) = child.stdout.take() {
            let mut reader = BufReader::new(stdout);
            match self.read_from(&mut reader, Source::Helper) {
                Ok(()) | Err(ReadError::InvalidLine) => {}
                Err(ReadError::Fatal(e)) => result = Err(e),
            }
            // Drain the rest so the helper does not die of SIGPIPE.
            let _ = io::copy(&mut reader.take(u64::MAX), &mut io::sink());
        }
        let _ = child.wait();
        result
    }
}

fn required_value(entry: &Entry) -> Result<&BStr> {
    match &entry.value {
        Some(v) => Ok(v.as_bstr()),
        None => Err(config_error(entry, entry.string().unwrap_err().to_string())),
    }
}

fn config_bool(entry: &Entry) -> Result<bool> {
    entry.bool().map_err(|e| fatal(e.to_string()))
}

/// git reports a missing value as `error: <msg>` followed by a fatal line
/// naming the source.
fn config_error(entry: &Entry, msg: String) -> Error {
    let _ = writeln!(io::stderr(), "error: {msg}");
    let name = entry.name.to_str_lossy();
    match &entry.source.origin {
        Origin::File(p) => fatal(messages::bad_config_variable(
            &name,
            &p.display().to_string(),
            entry.line,
        )),
        _ => fatal(messages::unable_to_parse_cmdline(&name)),
    }
}

/// `git credential capability`: the capabilities rgit supports.
pub fn write_capabilities(out: &mut dyn Write) -> io::Result<()> {
    out.write_all(b"version 0\ncapability authtype\ncapability state\n")
}

/// Read a credential from `input` for `git credential`. Invalid lines and
/// bad URLs are fatal.
pub fn read_caller_credential(input: &mut dyn Read) -> Result<Credential> {
    let mut cred = Credential::new();
    let mut reader = BufReader::new(input);
    match cred.read_from(&mut reader, Source::Caller) {
        Ok(()) => Ok(cred),
        Err(ReadError::InvalidLine) => Err(fatal(messages::UNABLE_TO_READ_CREDENTIAL)),
        Err(ReadError::Fatal(e)) => Err(e),
    }
}

#[cfg(test)]
mod tests;
