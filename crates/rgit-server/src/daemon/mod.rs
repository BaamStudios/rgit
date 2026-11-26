// SPDX-License-Identifier: MIT
//! The request side of `git daemon` (`Documentation/git-daemon.adoc`,
//! "Git Transport" in `Documentation/gitprotocol-pack.adoc`): what a
//! client asks for in its first packet, and how the requested path
//! becomes a directory to look at.
//!
//! This module only holds the rules; accepting connections, finding the
//! repository and running the service is the business of the program
//! (`rgit daemon`), one process per connection (ADR 0004).
//!
//! Where the documentation is silent the rules follow what git 2.56
//! does:
//!
//! - The first packet is `git-<service> <path>`, optionally with one LF,
//!   then NUL-terminated parameters: `host=<name>[:<port>]`, and after an
//!   empty parameter the "extra parameters" that become `GIT_PROTOCOL`,
//!   joined with colons. A first parameter that is not `host=` makes the
//!   request invalid.
//! - A path is refused as "aliased" unless it starts with `/` or `~` and
//!   has no empty, `.` or `..` component (one trailing `/` is fine).
//! - The host name is lower-cased; slashes are dropped, runs of dots
//!   become one dot and dots at either end are dropped, so that it cannot
//!   lead out of a directory when it is put into a path.
//! - `--interpolated-path` knows `%H`, `%CH`, `%IP`, `%P`, `%D` and `%%`;
//!   anything else stays as it is.

pub mod messages;

/// A service `git daemon` can run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    /// `git-upload-pack`: fetch and clone.
    UploadPack,
    /// `git-upload-archive`: `git archive --remote`.
    UploadArchive,
    /// `git-receive-pack`: push.
    ReceivePack,
}

impl Service {
    /// All services, in the order git knows them.
    pub const ALL: [Service; 3] = [
        Service::UploadPack,
        Service::UploadArchive,
        Service::ReceivePack,
    ];

    /// The name as in `--enable=<service>` and in the request
    /// (`git-<name>`).
    pub fn name(self) -> &'static str {
        match self {
            Service::UploadPack => "upload-pack",
            Service::UploadArchive => "upload-archive",
            Service::ReceivePack => "receive-pack",
        }
    }

    /// The repository's configuration variable that switches the service
    /// on or off.
    pub fn config_key(self) -> &'static str {
        match self {
            Service::UploadPack => "daemon.uploadpack",
            Service::UploadArchive => "daemon.uploadarch",
            Service::ReceivePack => "daemon.receivepack",
        }
    }

    /// Whether the service runs without `--enable`.
    pub fn enabled_by_default(self) -> bool {
        matches!(self, Service::UploadPack)
    }

    /// The service called `name`.
    pub fn from_name(name: &str) -> Option<Service> {
        Service::ALL.into_iter().find(|s| s.name() == name)
    }

    fn index(self) -> usize {
        match self {
            Service::UploadPack => 0,
            Service::UploadArchive => 1,
            Service::ReceivePack => 2,
        }
    }
}

/// Which services run (`--enable`, `--disable`) and which of them a
/// repository may switch with `daemon.*` (`--allow-override`,
/// `--forbid-override`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Services {
    enabled: [bool; 3],
    overridable: [bool; 3],
}

impl Default for Services {
    fn default() -> Services {
        Services {
            enabled: Service::ALL.map(Service::enabled_by_default),
            overridable: [true; 3],
        }
    }
}

impl Services {
    /// `--enable=<service>` / `--disable=<service>`.
    pub fn set_enabled(&mut self, service: Service, on: bool) {
        self.enabled[service.index()] = on;
    }

    /// `--allow-override=<service>` / `--forbid-override=<service>`.
    pub fn set_overridable(&mut self, service: Service, on: bool) {
        self.overridable[service.index()] = on;
    }

    /// Whether the service runs unless a repository says otherwise.
    pub fn enabled(&self, service: Service) -> bool {
        self.enabled[service.index()]
    }

    /// Whether a repository's `daemon.*` setting counts.
    pub fn overridable(&self, service: Service) -> bool {
        self.overridable[service.index()]
    }
}

/// The first packet of a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The service asked for.
    pub service: Service,
    /// The path as the client wrote it.
    pub dir: String,
    /// The `host=` parameter as the client wrote it.
    pub host: Option<String>,
    /// The extra parameters (`version=2`, ...).
    pub extra: Vec<String>,
}

/// Why a first packet is no request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestError {
    /// The line is not `git-<service> <path>`; carries the line.
    Protocol {
        /// The line.
        line: String,
        /// The `host=` parameter, if the packet had one.
        host: Option<String>,
        /// The extra parameters.
        extra: Vec<String>,
    },
    /// The parameters after the line are malformed.
    Invalid {
        /// The `host=` parameter, if the packet had one in front of the
        /// malformed part.
        host: Option<String>,
    },
}

impl Request {
    /// Parse the payload of the first packet.
    pub fn parse(packet: &[u8]) -> Result<Request, RequestError> {
        let (line, params) = match packet.iter().position(|b| *b == 0) {
            Some(at) => (&packet[..at], &packet[at + 1..]),
            None => (packet, &[][..]),
        };
        let line = String::from_utf8_lossy(line.strip_suffix(b"\n").unwrap_or(line)).into_owned();

        let mut host = None;
        let mut extra = Vec::new();
        let mut rest = params;
        if let Some(value) = rest.strip_prefix(b"host=") {
            let end = value.iter().position(|b| *b == 0).unwrap_or(value.len());
            host = Some(String::from_utf8_lossy(&value[..end]).into_owned());
            rest = value.get(end + 1..).unwrap_or(&[]);
        }
        if !rest.is_empty() {
            // An empty parameter separates the extra parameters.
            let Some(extras) = rest.strip_prefix(b"\0") else {
                return Err(RequestError::Invalid { host });
            };
            extra = extras
                .split(|b| *b == 0)
                .filter(|p| !p.is_empty())
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .collect();
        }

        let parsed = line.strip_prefix("git-").and_then(|rest| {
            Service::ALL.into_iter().find_map(|service| {
                let dir = rest.strip_prefix(service.name())?.strip_prefix(' ')?;
                Some((service, dir.to_owned()))
            })
        });
        match parsed {
            Some((service, dir)) => Ok(Request {
                service,
                dir,
                host,
                extra,
            }),
            None => Err(RequestError::Protocol { line, host, extra }),
        }
    }

    /// The value for `GIT_PROTOCOL`, if the client sent extra parameters.
    pub fn protocol(&self) -> Option<String> {
        join_extra(&self.extra)
    }
}

/// The extra parameters as one colon-separated value.
pub fn join_extra(extra: &[String]) -> Option<String> {
    (!extra.is_empty()).then(|| extra.join(":"))
}

/// The `host=` parameter taken apart.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Host {
    /// The host name, made safe for use in a path (see the module
    /// documentation).
    pub name: String,
    /// The port as the client wrote it, or empty.
    pub port: String,
}

impl Host {
    /// Take `<name>[:<port>]` (or `[<address>][:<port>]`) apart.
    pub fn parse(value: &str) -> Host {
        let (name, port) = if let Some(bracketed) = value.strip_prefix('[') {
            match bracketed.split_once(']') {
                Some((addr, rest)) => (addr, rest.strip_prefix(':').unwrap_or("")),
                None => (value, ""),
            }
        } else {
            value.rsplit_once(':').unwrap_or((value, ""))
        };
        Host {
            name: sanitize_host(name),
            port: port.to_owned(),
        }
    }
}

/// A client-supplied host name that is safe as a path component.
fn sanitize_host(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            '/' => {}
            '.' if out.is_empty() || out.ends_with('.') => {}
            c => out.push(c.to_ascii_lowercase()),
        }
    }
    while out.ends_with('.') {
        out.pop();
    }
    out
}

/// Whether `dir` could name a repository in more than one way (or is no
/// absolute path at all), see the module documentation.
pub fn is_aliased(dir: &str) -> bool {
    let Some(rest) = dir.strip_prefix(['/', '~']) else {
        return true;
    };
    if rest.is_empty() {
        return false;
    }
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let mut components = rest.split('/');
    // What follows `~` is a user name, not a directory.
    if dir.starts_with('~') {
        components.next();
    }
    components.any(|c| matches!(c, "" | "." | ".."))
}

/// `--user-path=<path>`: `~user/rest` names `<path>/rest` in the home
/// directory of `user`. Returns the rewritten request.
pub fn user_path(dir: &str, path: &str) -> String {
    let (user, rest) = match dir.find('/') {
        Some(at) => dir.split_at(at),
        None => (dir, ""),
    };
    format!("{user}/{path}{rest}")
}

/// The values `--interpolated-path` can refer to.
#[derive(Debug, Clone, Default)]
pub struct Interpolation<'a> {
    /// `%H`: the host name the client gave.
    pub host: &'a str,
    /// `%CH`: its canonical name.
    pub canonical_host: &'a str,
    /// `%IP`: its address.
    pub ip: &'a str,
    /// `%P`: the port the client gave.
    pub port: &'a str,
    /// `%D`: the requested path.
    pub dir: &'a str,
}

/// Whether `template` refers to the canonical host name or the address,
/// which cost a name lookup.
pub fn needs_lookup(template: &str) -> bool {
    template.contains("%CH") || template.contains("%IP")
}

/// Fill in an `--interpolated-path` template.
pub fn interpolate(template: &str, values: &Interpolation<'_>) -> String {
    let mut out = String::with_capacity(template.len() + values.dir.len());
    let mut rest = template;
    while let Some(at) = rest.find('%') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let (value, used) = if after.starts_with("CH") {
            (values.canonical_host, 2)
        } else if after.starts_with("IP") {
            (values.ip, 2)
        } else if after.starts_with('H') {
            (values.host, 1)
        } else if after.starts_with('P') {
            (values.port, 1)
        } else if after.starts_with('D') {
            (values.dir, 1)
        } else if after.starts_with('%') {
            ("%", 1)
        } else {
            ("%", 0)
        };
        out.push_str(value);
        rest = &after[used..];
    }
    out.push_str(rest);
    out
}

/// Whether the repository at `path` is covered by the directories given
/// on the command line: one of them itself or, without `--strict-paths`,
/// something below one of them. An empty list covers everything.
pub fn in_directory_list(path: &str, list: &[String], strict: bool) -> bool {
    if list.is_empty() {
        return true;
    }
    list.iter().any(|dir| {
        if strict {
            return path == dir;
        }
        match path.strip_prefix(dir.as_str()) {
            Some(rest) => rest.is_empty() || rest.starts_with('/'),
            None => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn requests() {
        let r = Request::parse(b"git-upload-pack /project.git\0host=myserver.com\0").unwrap();
        assert_eq!(r.service, Service::UploadPack);
        assert_eq!(r.dir, "/project.git");
        assert_eq!(r.host.as_deref(), Some("myserver.com"));
        assert_eq!(r.protocol(), None);

        let r =
            Request::parse(b"git-receive-pack /p\0host=h:9\0\0version=1\0foo\0bar=baz\0").unwrap();
        assert_eq!(r.service, Service::ReceivePack);
        assert_eq!(r.host.as_deref(), Some("h:9"));
        assert_eq!(r.protocol().as_deref(), Some("version=1:foo:bar=baz"));

        // No host, extra parameters only.
        let r = Request::parse(b"git-upload-archive /p\0\0version=2\0").unwrap();
        assert_eq!(r.service, Service::UploadArchive);
        assert_eq!(r.host, None);
        assert_eq!(r.extra, strings(&["version=2"]));

        // Nothing but the line; one LF is dropped, a space is part of the
        // path.
        let r = Request::parse(b"git-upload-pack  /p\n").unwrap();
        assert_eq!(r.dir, " /p");
        let r = Request::parse(b"git-upload-pack /p\n\0host=localhost").unwrap();
        assert_eq!(
            (r.dir.as_str(), r.host.as_deref()),
            ("/p", Some("localhost"))
        );
    }

    #[test]
    fn bad_requests() {
        for line in ["git-foo /x", "upload-pack /x", "git-upload-pack", ""] {
            let mut packet = line.as_bytes().to_vec();
            packet.extend_from_slice(b"\0host=h\0");
            match Request::parse(&packet) {
                Err(RequestError::Protocol { line: l, host, .. }) => {
                    assert_eq!(l, line);
                    assert_eq!(host.as_deref(), Some("h"));
                }
                other => panic!("{line:?}: {other:?}"),
            }
        }
        assert_eq!(
            Request::parse(b"git-upload-pack /p\0foo=bar\0host=h\0"),
            Err(RequestError::Invalid { host: None })
        );
        assert_eq!(
            Request::parse(b"git-upload-pack /p\0host=h\0version=2\0"),
            Err(RequestError::Invalid {
                host: Some("h".to_owned())
            })
        );
    }

    // Host names as git 2.56 puts them into an interpolated path.
    #[test]
    fn hosts() {
        let host = |v: &str| {
            let h = Host::parse(v);
            (h.name, h.port)
        };
        let pair = |n: &str, p: &str| (n.to_owned(), p.to_owned());
        assert_eq!(host("MyHost.Example:1234"), pair("myhost.example", "1234"));
        assert_eq!(host("a..b"), pair("a.b", ""));
        assert_eq!(host(".a"), pair("a", ""));
        assert_eq!(host("a."), pair("a", ""));
        assert_eq!(host("a/.b"), pair("a.b", ""));
        assert_eq!(host("A_b-C"), pair("a_b-c", ""));
        assert_eq!(host("..a.."), pair("a", ""));
        assert_eq!(host(".."), pair("", ""));
        assert_eq!(host("a:1:2"), pair("a:1", "2"));
        assert_eq!(host("[::1]:55"), pair("::1", "55"));
        assert_eq!(host("h:port"), pair("h", "port"));
    }

    // As git 2.56 logs "aliased" (or not).
    #[test]
    fn aliases() {
        for dir in [
            "",
            "srv",
            " /srv",
            "/srv/.",
            "/./srv",
            "//srv",
            "/srv//",
            "/srv/..",
            "/srv/../srv",
            "/srv/a/../..",
        ] {
            assert!(is_aliased(dir), "{dir:?}");
        }
        for dir in [
            "/",
            "/srv",
            "/srv/",
            "/srv/.git",
            "/srv/...",
            "/.srv",
            "/srv/.x/y",
            "~root/x",
            "~",
            "/~root/x",
        ] {
            assert!(!is_aliased(dir), "{dir:?}");
        }
    }

    #[test]
    fn user_paths() {
        assert_eq!(user_path("~alice/foo", "pub"), "~alice/pub/foo");
        assert_eq!(user_path("~alice", "pub"), "~alice/pub");
    }

    #[test]
    fn interpolation() {
        let values = Interpolation {
            host: "myhost.example",
            canonical_host: "canon",
            ip: "::1",
            port: "1234",
            dir: "/r.git",
        };
        let fill = |t: &str| interpolate(t, &values);
        assert_eq!(fill("/pub/%H%D"), "/pub/myhost.example/r.git");
        assert_eq!(
            fill("/v/%H/%P/%IP/%CH%D"),
            "/v/myhost.example/1234/::1/canon/r.git"
        );
        assert_eq!(fill("/v/%X%%%H%D"), "/v/%X%myhost.example/r.git");
        assert_eq!(fill("100%"), "100%");
        assert!(needs_lookup("/%IP/%D") && needs_lookup("%CH") && !needs_lookup("/%H%D"));
    }

    #[test]
    fn directory_list() {
        let list = strings(&["/pub/foo", "/srv/"]);
        assert!(in_directory_list("/anything", &[], true));
        assert!(in_directory_list("/pub/foo", &list, false));
        assert!(in_directory_list("/pub/foo/bar.git", &list, false));
        assert!(!in_directory_list("/pub/foobar", &list, false));
        assert!(!in_directory_list("/pub", &list, false));
        // A trailing slash asks for another slash.
        assert!(!in_directory_list("/srv/.git", &list, false));
        assert!(in_directory_list("/pub/foo", &list, true));
        assert!(!in_directory_list("/pub/foo/bar.git", &list, true));
    }

    #[test]
    fn services() {
        let mut s = Services::default();
        assert!(s.enabled(Service::UploadPack));
        assert!(!s.enabled(Service::ReceivePack) && !s.enabled(Service::UploadArchive));
        assert!(Service::ALL.into_iter().all(|x| s.overridable(x)));
        s.set_enabled(Service::ReceivePack, true);
        s.set_overridable(Service::UploadPack, false);
        assert!(s.enabled(Service::ReceivePack) && !s.overridable(Service::UploadPack));
        assert_eq!(
            Service::from_name("upload-archive"),
            Some(Service::UploadArchive)
        );
        assert_eq!(Service::from_name("bogus"), None);
    }
}
