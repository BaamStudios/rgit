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
    let scheme = url.split_once("://").map(|(s, _)| s);
    if !scheme.is_some_and(|s| s.eq_ignore_ascii_case("http") || s.eq_ignore_ascii_case("https")) {
        return rgit_transport::transport_for(url, settings);
    }
    let parsed = Url::parse(url).map_err(other)?;
    let opts = HttpOptions::load(settings.config, &parsed, settings.env).map_err(other)?;
    let credentials = SystemCredentials {
        config: settings.config.clone(),
        base_env: settings.base_env.clone(),
    };
    Ok(Some(Box::new(
        HttpTransport::new(url, opts).with_credentials(Box::new(credentials)),
    )))
}

/// The credential subsystem of `rgit-transport` (helpers from
/// `credential.*`, askpass, terminal prompts) as a [`CredentialProvider`].
struct SystemCredentials {
    config: ConfigSet,
    base_env: Vec<(OsString, OsString)>,
}

impl SystemCredentials {
    fn run(
        &self,
        cred: &Credential,
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
        let mut c = cred::Credential {
            protocol: Some(cred.protocol.as_str().into()),
            host: Some(cred.host.as_str().into()),
            path: cred.path.as_deref().map(Into::into),
            username: cred.username.as_deref().map(Into::into),
            password: cred.password.as_deref().map(Into::into),
            ..cred::Credential::default()
        };
        op(&mut c, &mut ctx)?;
        Ok(c)
    }
}

impl CredentialProvider for SystemCredentials {
    fn fill(&mut self, cred: &mut Credential) -> Result<(), String> {
        let c = self
            .run(cred, |c, ctx| c.fill(ctx))
            .map_err(|e| e.to_string())?;
        cred.username = c.username.map(|u| u.to_str_lossy().into_owned());
        cred.password = c.password.map(|p| p.to_str_lossy().into_owned());
        Ok(())
    }

    fn approve(&mut self, cred: &Credential) {
        let _ = self.run(cred, |c, ctx| c.approve(ctx));
    }

    fn reject(&mut self, cred: &Credential) {
        let _ = self.run(cred, |c, ctx| c.reject(ctx));
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
        let mut session = SmartHttp::new(&self.url, self.opts.clone()).map_err(other)?;
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

    fn split(&mut self) -> (&mut (dyn Read + Send), &mut (dyn Write + Send)) {
        (&mut self.response, &mut self.request)
    }

    fn close(mut self: Box<Self>) -> TResult<()> {
        io::copy(&mut self.response, &mut io::sink())?;
        Ok(())
    }
}
