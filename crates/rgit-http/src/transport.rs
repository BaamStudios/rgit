// SPDX-License-Identifier: MIT
//! Smart HTTP as an `rgit-transport` [`Transport`].
//!
//! [`HttpTransport::connect`] runs discovery and hands out a stateless
//! [`Connection`] whose first readable bytes are the advertisement with the
//! `# service=` packet and its flush removed. Every request written to it
//! is buffered and sent as one POST on [`Connection::end_request`]; its
//! response then becomes readable.

use std::ffi::OsString;
use std::io::{self, Read, Write};

use bstr::ByteSlice;
use rgit_config::{ConfigSet, SystemEnv};
use rgit_transport::credential as cred;
use rgit_transport::{ConnectOptions, Connection, Error as TError, Result as TResult, Transport};

use crate::auth::{Credential, CredentialProvider};
use crate::options::HttpOptions;
use crate::smart::{RpcBody, Service, SmartHttp};
use crate::url::Url;

/// Choose the transport for `url`: smart HTTP for `http://` and
/// `https://` (settings from `http.*` and the environment), otherwise
/// whatever [`rgit_transport::transport_for`] picks (local, SSH).
/// `Ok(None)` for schemes no transport handles.
pub fn transport_for(
    url: &str,
    settings: &rgit_transport::Settings<'_>,
) -> TResult<Option<Box<dyn Transport>>> {
    check_credentials_in_url(url, settings.config)?;
    let scheme = url.split_once("://").map(|(s, _)| s);
    if !scheme.is_some_and(|s| s.eq_ignore_ascii_case("http") || s.eq_ignore_ascii_case("https")) {
        return rgit_transport::transport_for(url, settings);
    }
    let parsed = Url::parse(url).map_err(other)?;
    let mut opts = HttpOptions::load(settings.config, &parsed, settings.env).map_err(other)?;
    apply_remote_proxy(&mut opts, settings.config, url, settings.env);
    for w in &opts.warnings {
        eprintln!("warning: {w}");
    }
    let credentials = SystemCredentials {
        config: settings.config.clone(),
        base_env: settings.base_env.clone(),
        helper_caps: (false, false),
    };
    Ok(Some(Box::new(
        HttpTransport::new(url, opts).with_credentials(Box::new(credentials)),
    )))
}

/// `remote.<name>.proxy` and `remote.<name>.proxyAuthMethod` of the remote
/// whose `url` or `pushurl` is `url` (they override `http.proxy` and
/// `http.proxyAuthMethod`; `GIT_HTTP_PROXY_AUTHMETHOD` still wins).
fn apply_remote_proxy(
    opts: &mut HttpOptions,
    config: &ConfigSet,
    url: &str,
    env: &dyn rgit_config::Environment,
) {
    let mut names: Vec<Vec<u8>> = Vec::new();
    for e in config.entries() {
        let (section, sub, key) = rgit_config::key::split_canonical(e.name.as_bstr());
        if section != "remote" || !(key == b"url" || key == b"pushurl") {
            continue;
        }
        if let (Some(sub), Some(v)) = (sub, e.value.as_deref()) {
            if v.as_bytes() == url.as_bytes() && !names.iter().any(|n| n == sub.as_bytes()) {
                names.push(sub.to_vec());
            }
        }
    }
    let Some(name) = names.first() else {
        return;
    };
    let name = name.to_str_lossy();
    if let Ok(Some(p)) = config.string(format!("remote.{name}.proxy")) {
        opts.proxy = Some(p.to_str_lossy().into_owned());
    }
    if env.var("GIT_HTTP_PROXY_AUTHMETHOD").is_none() {
        if let Ok(Some(m)) = config.string(format!("remote.{name}.proxyauthmethod")) {
            opts.set_proxy_auth_method(&m.to_str_lossy());
        }
    }
}

/// `transfer.credentialsInUrl` (`Documentation/config/transfer.adoc`):
/// a URL with `user:password@` is allowed (default), warned about or
/// refused. The password is shown as `<redacted>`.
pub fn check_credentials_in_url(url: &str, config: &ConfigSet) -> TResult<()> {
    let mode = match config.string("transfer.credentialsinurl") {
        Ok(Some(v)) => v.to_str_lossy().into_owned(),
        _ => return Ok(()),
    };
    let Some(redacted) = redact_password(url) else {
        return Ok(());
    };
    let msg = crate::messages::plaintext_credentials(&redacted);
    match mode.as_str() {
        "allow" => Ok(()),
        "warn" => {
            eprintln!("warning: {msg}");
            Ok(())
        }
        "die" => Err(TError::Other(msg)),
        other => Err(TError::Other(
            crate::messages::unrecognized_credentials_in_url(other),
        )),
    }
}

/// `scheme://user:<redacted>@rest` for a URL with a password.
fn redact_password(url: &str) -> Option<String> {
    let i = url.find("://")?;
    let rest = &url[i + 3..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let at = rest[..end].rfind('@')?;
    let colon = rest[..at].find(':')?;
    Some(format!(
        "{}{}:<redacted>{}",
        &url[..i + 3],
        &rest[..colon],
        &rest[at..]
    ))
}

/// The credential subsystem of `rgit-transport` (helpers from
/// `credential.*`, askpass, terminal prompts) as a [`CredentialProvider`].
struct SystemCredentials {
    config: ConfigSet,
    base_env: Vec<(OsString, OsString)>,
    /// The capabilities (`authtype`, `state`) the helpers announced in
    /// the last `get`; `store` and `erase` pass on only those.
    helper_caps: (bool, bool),
}

impl SystemCredentials {
    fn run(
        &self,
        cred: &Credential,
        caps: (bool, bool),
        op: impl FnOnce(&mut cred::Credential, &mut cred::CredentialContext<'_>) -> TResult<()>,
    ) -> TResult<cred::Credential> {
        let env = SystemEnv::default();
        let mut prompter = cred::SystemPrompter;
        let mut ctx = cred::CredentialContext {
            env: &env,
            config: &self.config,
            base_env: self.base_env.clone(),
            prompter: &mut prompter,
        };
        let s = |v: &Option<String>| v.as_deref().map(Into::into);
        let mut c = cred::Credential {
            protocol: Some(cred.protocol.as_str().into()),
            host: Some(cred.host.as_str().into()),
            path: s(&cred.path),
            username: s(&cred.username),
            password: s(&cred.password),
            authtype: s(&cred.authtype),
            credential: s(&cred.credential),
            wwwauth: cred.wwwauth.iter().map(|v| v.as_str().into()).collect(),
            state: cred.state.iter().map(|v| v.as_str().into()).collect(),
            multistage: cred.multistage,
            capa_authtype: cred::Capability {
                caller: caps.0,
                helper: false,
            },
            capa_state: cred::Capability {
                caller: caps.1,
                helper: false,
            },
            ..cred::Credential::default()
        };
        op(&mut c, &mut ctx)?;
        Ok(c)
    }
}

impl CredentialProvider for SystemCredentials {
    fn fill(&mut self, cred: &mut Credential) -> Result<(), String> {
        let c = self
            .run(cred, (true, true), |c, ctx| c.fill(ctx))
            .map_err(|e| e.to_string())?;
        self.helper_caps = (c.capa_authtype.helper, c.capa_state.helper);
        let s = |v: Option<bstr::BString>| v.map(|v| v.to_str_lossy().into_owned());
        cred.username = s(c.username);
        cred.password = s(c.password);
        // Pre-encoded credentials count only from a helper that announced
        // `authtype`.
        if c.capa_authtype.helper {
            cred.authtype = s(c.authtype);
            cred.credential = s(c.credential);
        } else {
            cred.authtype = None;
            cred.credential = None;
        }
        // `state[]` is the helper's answer only; what was sent is consumed.
        let sent = cred.state.len();
        let answered = if c.state.len() >= sent
            && c.state[..sent]
                .iter()
                .zip(&cred.state)
                .all(|(a, b)| a.as_slice() == b.as_bytes())
        {
            &c.state[sent..]
        } else {
            &c.state[..]
        };
        cred.state = if c.capa_state.helper {
            answered
                .iter()
                .map(|v| v.to_str_lossy().into_owned())
                .collect()
        } else {
            Vec::new()
        };
        cred.multistage = c.capa_state.helper && c.multistage;
        Ok(())
    }

    fn approve(&mut self, cred: &Credential) {
        let _ = self.run(cred, self.helper_caps, |c, ctx| c.approve(ctx));
    }

    fn reject(&mut self, cred: &Credential) {
        let _ = self.run(cred, self.helper_caps, |c, ctx| c.reject(ctx));
    }
}

/// A smart HTTP remote.
pub struct HttpTransport {
    url: String,
    opts: HttpOptions,
    provider: Option<Box<dyn CredentialProvider + Send>>,
}

impl HttpTransport {
    /// A transport for the repository at `url` with the given settings.
    pub fn new(url: impl Into<String>, opts: HttpOptions) -> HttpTransport {
        HttpTransport {
            url: url.into(),
            opts,
            provider: None,
        }
    }

    /// The credential subsystem to use after a `401`.
    pub fn with_credentials(mut self, provider: Box<dyn CredentialProvider + Send>) -> Self {
        self.provider = Some(provider);
        self
    }
}

impl Transport for HttpTransport {
    fn connect(&mut self, options: &ConnectOptions) -> TResult<Box<dyn Connection>> {
        let service = match options.service {
            rgit_transport::Service::UploadPack => Service::UploadPack,
            rgit_transport::Service::ReceivePack => Service::ReceivePack,
            rgit_transport::Service::UploadArchive => {
                return Err(TError::Refused(format!(
                    "{} is not supported over HTTP",
                    options.service.name()
                )));
            }
        };
        let mut opts = self.opts.clone();
        opts.verbosity = opts.verbosity.max(options.verbosity);
        let mut session = SmartHttp::new(&self.url, opts).map_err(other)?;
        if let Some(p) = self.provider.take() {
            session = session.with_credentials(p);
        }
        session.set_git_protocol(options.git_protocol());
        let adv = session.discover(service).map_err(other)?;
        Ok(Box::new(HttpConnection {
            session,
            service,
            request: Vec::new(),
            response: Box::new(io::Cursor::new(adv.data)),
        }))
    }
}

fn other(e: crate::Error) -> TError {
    let mut msg = String::new();
    for l in e.remote_lines() {
        msg.push_str(&crate::messages::remote_line(l));
        msg.push('\n');
    }
    msg.push_str(&e.to_string());
    TError::Other(msg)
}

/// A stateless connection to one service.
struct HttpConnection {
    session: SmartHttp<'static>,
    service: Service,
    request: Vec<u8>,
    response: Box<dyn Read + Send>,
}

impl Read for HttpConnection {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.response.read(buf)
    }
}

impl Write for HttpConnection {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.request.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Connection for HttpConnection {
    fn is_stateless(&self) -> bool {
        true
    }

    fn end_request(&mut self) -> TResult<()> {
        let body = std::mem::take(&mut self.request);
        let resp = self
            .session
            .rpc(self.service, RpcBody::Bytes(&body))
            .map_err(other)?;
        self.response = Box::new(resp);
        Ok(())
    }

    fn split(&mut self) -> (&mut (dyn Read + Send), &mut dyn rgit_transport::WriteHalf) {
        (&mut self.response, &mut self.request)
    }

    fn close(mut self: Box<Self>) -> TResult<()> {
        io::copy(&mut self.response, &mut io::sink())?;
        Ok(())
    }
}
