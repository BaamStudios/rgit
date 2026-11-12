// SPDX-License-Identifier: MIT
//! Remote services and connection options.

use rgit_protocol::ProtocolVersion;

/// A service program on the remote side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Service {
    /// `git-upload-pack` (fetch, clone, ls-remote).
    UploadPack,
    /// `git-receive-pack` (push).
    ReceivePack,
    /// `git-upload-archive` (`archive --remote`).
    UploadArchive,
}

impl Service {
    /// The program name, e.g. `git-upload-pack`. This is also the service
    /// name of the HTTP protocol (`?service=git-upload-pack`).
    pub fn name(self) -> &'static str {
        match self {
            Service::UploadPack => "git-upload-pack",
            Service::ReceivePack => "git-receive-pack",
            Service::UploadArchive => "git-upload-archive",
        }
    }
}

/// What to connect to and how.
#[derive(Debug, Clone)]
pub struct ConnectOptions {
    /// The service to run.
    pub service: Service,
    /// Program to run instead of [`Service::name`] (`--upload-pack`,
    /// `--receive-pack`, `remote.<name>.uploadpack`/`receivepack`). Only
    /// meaningful for transports that run a program; it may contain shell
    /// syntax.
    pub program: Option<String>,
    /// The protocol version the client asks for (`protocol.version`).
    pub protocol_version: ProtocolVersion,
    /// Further `key=value` parameters for the `GIT_PROTOCOL` value besides
    /// `version=N` (e.g. `object-format=sha256`). Usually empty.
    pub extra_parameters: Vec<String>,
}

impl ConnectOptions {
    /// Options for `service` with protocol v0 and no overrides.
    pub fn new(service: Service) -> Self {
        ConnectOptions {
            service,
            program: None,
            protocol_version: ProtocolVersion::V0,
            extra_parameters: Vec::new(),
        }
    }

    /// The program to run: the override or the service name.
    pub fn program(&self) -> &str {
        self.program.as_deref().unwrap_or(self.service.name())
    }

    /// The value for `GIT_PROTOCOL` / `Git-Protocol:`, parameters joined
    /// with `:`; `None` when there is nothing to send (protocol v0 without
    /// extra parameters).
    pub fn git_protocol(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        match self.protocol_version {
            ProtocolVersion::V0 => {}
            ProtocolVersion::V1 => parts.push("version=1".into()),
            ProtocolVersion::V2 => parts.push("version=2".into()),
        }
        parts.extend(self.extra_parameters.iter().cloned());
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(":"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_protocol_value() {
        let mut o = ConnectOptions::new(Service::UploadPack);
        assert_eq!(o.git_protocol(), None);
        assert_eq!(o.program(), "git-upload-pack");
        o.protocol_version = ProtocolVersion::V2;
        assert_eq!(o.git_protocol().as_deref(), Some("version=2"));
        o.extra_parameters.push("object-format=sha256".into());
        assert_eq!(
            o.git_protocol().as_deref(),
            Some("version=2:object-format=sha256")
        );
        o.program = Some("/opt/git-upload-pack".into());
        assert_eq!(o.program(), "/opt/git-upload-pack");
    }
}
