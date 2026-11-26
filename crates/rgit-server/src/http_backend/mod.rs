// SPDX-License-Identifier: MIT
//! `git http-backend`: a CGI program that serves repositories over HTTP
//! (`Documentation/git-http-backend.adoc`, `gitprotocol-http.adoc`).
//!
//! The web server starts one process per request and describes it in
//! environment variables (RFC 3875): `REQUEST_METHOD`, `PATH_INFO` with
//! `GIT_PROJECT_ROOT` (or `PATH_TRANSLATED`), `QUERY_STRING`,
//! `CONTENT_TYPE`, `CONTENT_LENGTH`, `HTTP_CONTENT_ENCODING`,
//! `HTTP_GIT_PROTOCOL`, `REMOTE_USER`, `REMOTE_ADDR`. The request body is
//! on standard input; the response (headers, an empty line, the body)
//! goes to standard output.
//!
//! - Smart HTTP: `GET .../info/refs?service=git-upload-pack` (or
//!   `git-receive-pack`) and `POST .../git-upload-pack` (or
//!   `git-receive-pack`) run the service with `--stateless-rpc` as a
//!   child process, the way git does, and pass the body through (inflated
//!   when it is gzip-compressed).
//! - Dumb HTTP: `HEAD`, `info/refs`, `objects/info/packs`,
//!   `objects/info/alternates`, `objects/info/http-alternates`, loose
//!   objects, packs and their indexes, unless `http.getanyfile` is off.
//!
//! Which services run is decided by `http.uploadpack` (default on),
//! `http.receivepack` (default: on for an authenticated user, i.e. when
//! `REMOTE_USER` is set) and `http.getanyfile` (default on). A
//! repository is only served with `GIT_HTTP_EXPORT_ALL` set or when it has
//! a `git-daemon-export-ok` file.
//!
//! Status codes, headers and messages follow observations of the git
//! 2.56 binary, including its quirks: errors go to standard error (the
//! web server's log) and the exit code is 0 for every answered request;
//! a refused file request after the cache headers repeats them.

mod body;
mod headers;
pub mod messages;

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use rgit_config::ConfigSet;
use rgit_config::value::IntWidth;
use rgit_hash::{HashKind, ObjectId};
use rgit_object::{ObjectKind, Tag};
use rgit_odb::ObjectDatabase;
use rgit_refs::{IterItem, IterOptions, RefStore, Target};

use body::Body;
use headers::Headers;
use messages as msg;

/// `GIT_HTTP_MAX_REQUEST_BUFFER` / `http.maxRequestBuffer` default.
const DEFAULT_MAX_REQUEST_BUFFER: u64 = 10 * 1024 * 1024;

/// A repository opened for a request.
pub struct Repository {
    /// The git directory (absolute); services run inside it.
    pub git_dir: PathBuf,
    /// The objects directory.
    pub objects_dir: PathBuf,
    /// Hash algorithm.
    pub hash: HashKind,
    /// The repository's configuration (with the system and global files).
    pub config: ConfigSet,
    /// References.
    pub refs: Box<dyn RefStore>,
    /// Objects (for peeling tags in `info/refs`).
    pub odb: ObjectDatabase,
}

/// What the caller provides: finding and opening repositories.
pub trait Host {
    /// The repository at `dir` (the work tree or git directory, with the
    /// `.git` suffixes `git upload-pack` tries), or `None`.
    fn open(&mut self, dir: &str) -> Option<Repository>;
}

/// How to run the services.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// The command that runs git, e.g. `["/usr/bin/rgit"]`; the service
    /// name and its options are appended.
    pub git: Vec<OsString>,
    /// Environment for the services on top of this process's.
    pub env: Vec<(OsString, OsString)>,
    /// `GIT_NAMESPACE`: the namespace whose references the dumb protocol
    /// shows (the services read the variable themselves).
    pub namespace: Option<String>,
}

/// Where a request ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    UploadPack,
    ReceivePack,
    InfoRefs,
    Head,
    Alternates,
    HttpAlternates,
    Packs,
    Loose,
    Pack,
    Idx,
}

impl Route {
    fn is_post(self) -> bool {
        matches!(self, Route::UploadPack | Route::ReceivePack)
    }
}

/// A service a client can ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Service {
    UploadPack,
    ReceivePack,
}

impl Service {
    fn name(self) -> &'static str {
        match self {
            Service::UploadPack => "upload-pack",
            Service::ReceivePack => "receive-pack",
        }
    }

    fn from_name(name: &str) -> Option<Service> {
        match name {
            "upload-pack" => Some(Service::UploadPack),
            "receive-pack" => Some(Service::ReceivePack),
            _ => None,
        }
    }
}

/// The end of the request in progress: answered, or a fatal error.
enum Stop {
    Fatal(String),
    Io,
}

impl From<io::Error> for Stop {
    fn from(_: io::Error) -> Self {
        Stop::Io
    }
}

type Result<T> = std::result::Result<T, Stop>;

/// One request.
struct Request<'a> {
    var: &'a dyn Fn(&str) -> Option<String>,
    opts: &'a Options,
    out: &'a mut dyn Write,
    err: &'a mut dyn Write,
    /// Child process exit status that makes the program fail.
    exit: i32,
}

/// Answer one CGI request: read the description from `var` (the
/// environment), the body from `input`, write the response to `out` and
/// messages to `err`. Returns the exit code.
pub fn http_backend<R: Read + Send>(
    var: &dyn Fn(&str) -> Option<String>,
    host: &mut dyn Host,
    opts: &Options,
    input: R,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let mut req = Request {
        var,
        opts,
        out,
        err,
        exit: 0,
    };
    match req.serve(host, input) {
        Ok(()) => {}
        Err(Stop::Fatal(m)) => {
            let _ = writeln!(req.err, "fatal: {m}");
            let _ = req.status_only(500, "Internal Server Error");
        }
        // The web server went away; nothing to tell anyone.
        Err(Stop::Io) => {}
    }
    let _ = req.out.flush();
    req.exit
}

impl Request<'_> {
    fn var(&self, name: &str) -> Option<String> {
        (self.var)(name)
    }

    fn headers(&mut self) -> Headers<'_> {
        Headers::new(self.out)
    }

    /// `Status:` plus the no-cache headers and nothing else.
    fn status_only(&mut self, code: u16, text: &str) -> io::Result<()> {
        let mut h = self.headers();
        h.status(code, text)?;
        h.nocache()?;
        h.end()
    }

    fn not_found(&mut self, message: &str) -> Result<()> {
        let _ = writeln!(self.err, "{message}");
        self.status_only(404, "Not Found")?;
        Ok(())
    }

    fn forbidden(&mut self, message: &str) -> Result<()> {
        let _ = writeln!(self.err, "{message}");
        self.status_only(403, "Forbidden")?;
        Ok(())
    }

    fn serve<R: Read + Send>(&mut self, host: &mut dyn Host, input: R) -> Result<()> {
        let Some(method) = self.var("REQUEST_METHOD") else {
            return Err(Stop::Fatal(msg::NO_REQUEST_METHOD.into()));
        };
        let max_env = match self.var("GIT_HTTP_MAX_REQUEST_BUFFER") {
            Some(v) => Some(
                rgit_config::value::parse_int(v.as_bytes(), IntWidth::U64)
                    .map_err(|_| Stop::Fatal(msg::BAD_MAX_REQUEST_BUFFER.into()))?
                    as u64,
            ),
            None => None,
        };
        let path = self.request_path()?;
        let Some((start, route)) = match_route(&path) else {
            return self.not_found(&msg::request_not_supported(&path));
        };
        let method_ok = if route.is_post() {
            method == "POST"
        } else {
            method == "GET" || method == "HEAD"
        };
        if !method_ok {
            let http11 = self.var("SERVER_PROTOCOL").as_deref() == Some("HTTP/1.1");
            let mut h = self.headers();
            if http11 {
                h.status(405, "Method Not Allowed")?;
                h.str("Allow", if route.is_post() { "POST" } else { "GET, HEAD" })?;
            } else {
                h.status(400, "Bad Request")?;
            }
            h.nocache()?;
            h.end()?;
            return Ok(());
        }
        let dir = &path[..start];
        let file = &path[start + 1..];
        let Some(repo) = host.open(dir) else {
            return self.not_found(&msg::not_a_repository(dir));
        };
        if self.var("GIT_HTTP_EXPORT_ALL").is_none()
            && !repo.git_dir.join("git-daemon-export-ok").exists()
        {
            return self.not_found(&msg::not_exported(dir));
        }
        let config = Config::load(&repo.config, self.var("REMOTE_USER").is_some(), max_env)?;
        match route {
            Route::UploadPack => self.service_rpc(&repo, &config, Service::UploadPack, input),
            Route::ReceivePack => self.service_rpc(&repo, &config, Service::ReceivePack, input),
            Route::InfoRefs => self.info_refs(&repo, &config),
            Route::Head => self.head(&repo, &config),
            Route::Packs => self.packs(&repo, &config),
            Route::Alternates | Route::HttpAlternates => {
                if !config.getanyfile {
                    return self.forbidden(msg::GETANYFILE_DISABLED);
                }
                self.headers().nocache()?;
                self.send_file(&repo, file, "text/plain")
            }
            Route::Loose | Route::Pack | Route::Idx => {
                if !config.getanyfile {
                    return self.forbidden(msg::GETANYFILE_DISABLED);
                }
                self.headers().cache_forever()?;
                let kind = match route {
                    Route::Loose => "application/x-git-loose-object",
                    Route::Pack => "application/x-git-packed-objects",
                    _ => "application/x-git-packed-objects-toc",
                };
                self.send_file(&repo, file, kind)
            }
        }
    }

    /// `GIT_PROJECT_ROOT` + `PATH_INFO`, or `PATH_TRANSLATED`.
    fn request_path(&self) -> Result<String> {
        if let Some(root) = self.var("GIT_PROJECT_ROOT") {
            let Some(info) = self.var("PATH_INFO") else {
                return Err(Stop::Fatal(msg::NO_PATH_INFO.into()));
            };
            if aliased(&info) {
                return Err(Stop::Fatal(msg::aliased(&info)));
            }
            return Ok(format!("{}{info}", root.trim_end_matches('/')));
        }
        self.var("PATH_TRANSLATED")
            .ok_or_else(|| Stop::Fatal(msg::NO_PROJECT_ROOT.into()))
    }

    /// The protocol the client asked for: `GIT_PROTOCOL` set by the web
    /// server, else the `Git-Protocol` header.
    fn git_protocol(&self) -> Option<String> {
        self.var("GIT_PROTOCOL")
            .or_else(|| self.var("HTTP_GIT_PROTOCOL"))
    }

    fn info_refs(&mut self, repo: &Repository, config: &Config) -> Result<()> {
        let query = self.var("QUERY_STRING").unwrap_or_default();
        let service = query
            .split('&')
            .find_map(|p| p.strip_prefix("service="))
            .map(str::to_owned);
        self.headers().nocache()?;
        if let Some(name) = service {
            let Some(svc) = name.strip_prefix("git-").and_then(Service::from_name) else {
                return self.forbidden(&msg::unsupported_service(&name));
            };
            if !config.enabled(svc) {
                return self.forbidden(&msg::service_not_enabled(svc.name()));
            }
            {
                let mut h = self.headers();
                h.str("Content-Type", &msg::advertisement_content_type(svc.name()))?;
                h.end()?;
            }
            let v2 = self
                .git_protocol()
                .is_some_and(|p| p.split(':').any(|e| e == "version=2"));
            if !v2 {
                let line = format!("# service=git-{}\n", svc.name());
                write!(self.out, "{:04x}{line}0000", line.len() + 4)?;
            }
            return self.run_service(repo, svc, true, None::<&[u8]>, None);
        }
        if !config.getanyfile {
            return self.forbidden(msg::GETANYFILE_DISABLED);
        }
        let text = dumb_info_refs(repo, self.opts.namespace.as_deref());
        self.send_text(&text, "text/plain")
    }

    fn head(&mut self, repo: &Repository, config: &Config) -> Result<()> {
        if !config.getanyfile {
            return self.forbidden(msg::GETANYFILE_DISABLED);
        }
        let prefix = namespace_prefix(self.opts.namespace.as_deref());
        let text = match repo.refs.read_raw(&format!("{prefix}HEAD")) {
            Ok(Some(Target::Symbolic(name))) => {
                let name = name.strip_prefix(prefix.as_str()).unwrap_or(&name);
                format!("ref: {name}\n")
            }
            Ok(Some(Target::Object(id))) => format!("{}\n", id.to_hex()),
            _ => {
                let e = io::Error::from(io::ErrorKind::NotFound);
                return self.not_found(&msg::cannot_open("HEAD", &rgit_messages::strerror(&e)));
            }
        };
        let mut h = self.headers();
        h.str("Content-Length", &text.len().to_string())?;
        h.str("Content-Type", "text/plain")?;
        h.end()?;
        self.out.write_all(text.as_bytes())?;
        Ok(())
    }

    fn packs(&mut self, repo: &Repository, config: &Config) -> Result<()> {
        if !config.getanyfile {
            return self.forbidden(msg::GETANYFILE_DISABLED);
        }
        self.headers().nocache()?;
        let mut names: Vec<String> = std::fs::read_dir(repo.objects_dir.join("pack"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with("pack-") && n.ends_with(".pack"))
            .filter(|n| {
                // Only packs with an index can be used.
                let idx = format!("{}.idx", &n[..n.len() - 5]);
                repo.objects_dir.join("pack").join(idx).is_file()
            })
            .collect();
        names.sort_unstable();
        let mut text = String::new();
        for n in names {
            text.push_str(&format!("P {n}\n"));
        }
        text.push('\n');
        self.send_text(&text, "text/plain; charset=utf-8")
    }

    fn send_text(&mut self, text: &str, content_type: &str) -> Result<()> {
        let mut h = self.headers();
        h.str("Content-Length", &text.len().to_string())?;
        h.str("Content-Type", content_type)?;
        h.end()?;
        self.out.write_all(text.as_bytes())?;
        Ok(())
    }

    /// A file of the repository, `rel` to the git directory.
    fn send_file(&mut self, repo: &Repository, rel: &str, content_type: &str) -> Result<()> {
        let path = repo.git_dir.join(rel);
        let opened = std::fs::File::open(&path).and_then(|f| {
            let meta = f.metadata()?;
            Ok((f, meta))
        });
        let (mut file, meta) = match opened {
            Ok(x) => x,
            Err(e) => {
                return self.not_found(&msg::cannot_open(rel, &rgit_messages::strerror(&e)));
            }
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        {
            let mut h = self.headers();
            h.str("Content-Length", &meta.len().to_string())?;
            h.str("Content-Type", content_type)?;
            h.str("Last-Modified", &headers::http_date(mtime))?;
            h.end()?;
        }
        io::copy(&mut file, &mut self.out)?;
        Ok(())
    }

    fn service_rpc<R: Read + Send>(
        &mut self,
        repo: &Repository,
        config: &Config,
        svc: Service,
        input: R,
    ) -> Result<()> {
        if !config.enabled(svc) {
            return self.forbidden(&msg::service_not_enabled(svc.name()));
        }
        let expected = format!("application/x-git-{}-request", svc.name());
        let got = self.var("CONTENT_TYPE");
        if got.as_deref() != Some(expected.as_str()) {
            let mut h = self.headers();
            h.status(415, "Unsupported Media Type")?;
            h.nocache()?;
            h.end()?;
            let got = got.as_deref().unwrap_or("");
            self.out
                .write_all(msg::wrong_content_type(&expected, got).as_bytes())?;
            return Ok(());
        }
        {
            let mut h = self.headers();
            h.nocache()?;
            h.str(
                "Content-Type",
                &format!("application/x-git-{}-result", svc.name()),
            )?;
            h.end()?;
        }
        let length = match self.var("CONTENT_LENGTH").filter(|v| !v.is_empty()) {
            None => None,
            Some(v) => {
                let n = rgit_config::value::parse_int(v.as_bytes(), IntWidth::I64)
                    .map_err(|_| Stop::Fatal(msg::bad_content_length(&v)))?;
                // A negative length means "unknown".
                u64::try_from(n).ok()
            }
        };
        if svc == Service::UploadPack {
            // git keeps the negotiation request in memory and refuses one
            // announced as larger than the limit.
            if let Some(len) = length.filter(|&l| l > config.max_request_buffer) {
                let _ = writeln!(
                    self.err,
                    "fatal: {}",
                    msg::request_too_large(config.max_request_buffer, len as i64)
                );
                return Ok(());
            }
        }
        let gzip = matches!(
            self.var("HTTP_CONTENT_ENCODING").as_deref(),
            Some("gzip" | "x-gzip")
        );
        let body = Body { length, gzip };
        let mut env = Vec::new();
        if svc == Service::ReceivePack {
            // Reflog entries name the user who pushed.
            let user = self
                .var("REMOTE_USER")
                .unwrap_or_else(|| "anonymous".to_owned());
            let addr = self
                .var("REMOTE_ADDR")
                .unwrap_or_else(|| "(none)".to_owned());
            if self.var("GIT_COMMITTER_NAME").is_none() {
                env.push(("GIT_COMMITTER_NAME".into(), user.clone().into()));
            }
            if self.var("GIT_COMMITTER_EMAIL").is_none() {
                env.push((
                    "GIT_COMMITTER_EMAIL".into(),
                    format!("{user}@http.{addr}").into(),
                ));
            }
        }
        self.run_service(repo, svc, false, Some(input), Some((body, env)))
    }

    /// Run `<svc> --stateless-rpc [--http-backend-info-refs] .` in the
    /// repository, feed it the body and copy its output to the response.
    fn run_service<R: Read + Send>(
        &mut self,
        repo: &Repository,
        svc: Service,
        advertise: bool,
        input: Option<R>,
        body: Option<(Body, Vec<(OsString, OsString)>)>,
    ) -> Result<()> {
        self.out.flush()?;
        let Some((program, args)) = self.opts.git.split_first() else {
            return Err(Stop::Fatal(msg::cannot_run(svc.name(), "no git command")));
        };
        let mut cmd = Command::new(program);
        cmd.args(args).arg(svc.name()).arg("--stateless-rpc");
        if advertise {
            cmd.arg("--http-backend-info-refs");
        }
        cmd.arg(".")
            .current_dir(&repo.git_dir)
            .envs(self.opts.env.iter().map(|(k, v)| (k, v)))
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(p) = self.git_protocol() {
            cmd.env("GIT_PROTOCOL", p);
        }
        let extra_env = body.as_ref().map(|(_, e)| e.clone()).unwrap_or_default();
        cmd.envs(extra_env);
        cmd.stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        let mut child = cmd
            .spawn()
            .map_err(|e| Stop::Fatal(msg::cannot_run(svc.name(), &rgit_messages::strerror(&e))))?;
        let stdin = child.stdin.take();
        let mut stdout = child.stdout.take().expect("piped stdout");
        let child: Arc<Mutex<Child>> = Arc::new(Mutex::new(child));
        let out = &mut *self.out;
        let (copied, fed) = std::thread::scope(|s| {
            let feeder = match (input, stdin, &body) {
                (Some(input), Some(stdin), Some((b, _))) => {
                    let b = *b;
                    let child = Arc::clone(&child);
                    Some(s.spawn(move || {
                        let result = body::copy(b, input, stdin);
                        if result.is_err() {
                            // Like git: the service does not see a broken
                            // request.
                            if let Ok(mut c) = child.lock() {
                                let _ = c.kill();
                            }
                        }
                        result
                    }))
                }
                _ => None,
            };
            let copied = copy_out(&mut stdout, out);
            let fed = feeder.map(|f| f.join().unwrap_or(Ok(())));
            (copied, fed)
        });
        let status = child.lock().map(|mut c| c.wait());
        if let Some(Err(failure)) = fed {
            if let Some(e) = &failure.error {
                let _ = writeln!(self.err, "error: {e}");
            }
            let _ = writeln!(self.err, "fatal: {}", failure.fatal);
            return Ok(());
        }
        copied?;
        if let Ok(Ok(st)) = status {
            if !st.success() {
                self.exit = 1;
            }
        }
        Ok(())
    }
}

fn copy_out(from: &mut dyn Read, to: &mut dyn Write) -> io::Result<()> {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        to.write_all(&buf[..n])?;
        // Progress and keepalives must reach the client while the service
        // works.
        to.flush()?;
    }
    Ok(())
}

/// The settings of the served repository.
struct Config {
    getanyfile: bool,
    upload_pack: bool,
    receive_pack: bool,
    max_request_buffer: u64,
}

impl Config {
    fn load(config: &ConfigSet, authenticated: bool, max_env: Option<u64>) -> Result<Config> {
        let flag = |name: &str, default: bool| -> Result<bool> {
            config
                .bool(name)
                .map(|v| v.unwrap_or(default))
                .map_err(|e| Stop::Fatal(e.to_string()))
        };
        let max_request_buffer = match max_env {
            Some(v) => v,
            None => match config.get("http.maxrequestbuffer") {
                Some(e) => e
                    .int(IntWidth::U64)
                    .map_err(|e| Stop::Fatal(e.to_string()))? as u64,
                None => DEFAULT_MAX_REQUEST_BUFFER,
            },
        };
        Ok(Config {
            getanyfile: flag("http.getanyfile", true)?,
            upload_pack: flag("http.uploadpack", true)?,
            receive_pack: flag("http.receivepack", authenticated)?,
            max_request_buffer,
        })
    }

    fn enabled(&self, svc: Service) -> bool {
        match svc {
            Service::UploadPack => self.upload_pack,
            Service::ReceivePack => self.receive_pack,
        }
    }
}

/// `PATH_INFO` must be an absolute path without empty, `.` or `..`
/// components, so that it cannot leave `GIT_PROJECT_ROOT`.
fn aliased(path: &str) -> bool {
    let Some(rest) = path.strip_prefix('/') else {
        return true;
    };
    rest.split('/')
        .enumerate()
        .any(|(i, c)| c == "." || c == ".." || (c.is_empty() && i + 1 < rest.split('/').count()))
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The route `path` ends in and where its `/<file>` part starts.
fn match_route(path: &str) -> Option<(usize, Route)> {
    const FIXED: &[(&str, Route)] = &[
        ("/git-upload-pack", Route::UploadPack),
        ("/git-receive-pack", Route::ReceivePack),
        ("/info/refs", Route::InfoRefs),
        ("/HEAD", Route::Head),
        ("/objects/info/alternates", Route::Alternates),
        ("/objects/info/http-alternates", Route::HttpAlternates),
        ("/objects/info/packs", Route::Packs),
    ];
    for (suffix, route) in FIXED {
        if path.ends_with(suffix) {
            return Some((path.len() - suffix.len(), *route));
        }
    }
    let (head, last) = path.rsplit_once('/')?;
    // objects/<2 hex>/<38 or 62 hex>
    if let Some((before, fan)) = head.rsplit_once('/') {
        if is_hex(fan, 2) && (is_hex(last, 38) || is_hex(last, 62)) {
            if let Some(dir) = before.strip_suffix("/objects") {
                return Some((dir.len(), Route::Loose));
            }
        }
    }
    // objects/pack/pack-<40 or 64 hex>.(pack|idx)
    let dir = head.strip_suffix("/objects/pack")?;
    let name = last.strip_prefix("pack-")?;
    let (hex, route) = if let Some(h) = name.strip_suffix(".pack") {
        (h, Route::Pack)
    } else {
        (name.strip_suffix(".idx")?, Route::Idx)
    };
    (is_hex(hex, 40) || is_hex(hex, 64)).then_some((dir.len(), route))
}

/// `refs/namespaces/<a>/refs/namespaces/<b>/` for namespace `a/b`.
fn namespace_prefix(namespace: Option<&str>) -> String {
    let mut prefix = String::new();
    for part in namespace.unwrap_or("").split('/').filter(|p| !p.is_empty()) {
        prefix.push_str("refs/namespaces/");
        prefix.push_str(part);
        prefix.push('/');
    }
    prefix
}

/// The dumb `info/refs`: `<id> TAB <name>` for every reference, followed
/// by `<peeled id> TAB <name>^{}` for an annotated tag.
fn dumb_info_refs(repo: &Repository, namespace: Option<&str>) -> String {
    let prefix = namespace_prefix(namespace);
    let opts = IterOptions::prefix(if prefix.is_empty() {
        "refs/".to_owned()
    } else {
        prefix.clone()
    });
    let mut text = String::new();
    for item in repo.refs.iter(&opts).unwrap_or_default() {
        let IterItem::Ref(r) = item else { continue };
        let Some(id) = r.oid else { continue };
        // A namespace's own `HEAD` is not listed.
        let Some(name) = r
            .name
            .strip_prefix(prefix.as_str())
            .filter(|n| n.starts_with("refs/"))
        else {
            continue;
        };
        text.push_str(&format!("{}\t{name}\n", id.to_hex()));
        if let Some(peeled) = peel(&repo.odb, repo.hash, id) {
            text.push_str(&format!("{}\t{name}^{{}}\n", peeled.to_hex()));
        }
    }
    text
}

/// What the annotated tag `id` finally points at; `None` for other
/// objects.
fn peel(odb: &ObjectDatabase, hash: HashKind, id: ObjectId) -> Option<ObjectId> {
    let mut current = id;
    let mut was_tag = false;
    for _ in 0..64 {
        match odb.read(&current) {
            Ok(Some(o)) if o.kind == ObjectKind::Tag => {
                match Tag::parse(hash, &o.data).and_then(|t| t.object()) {
                    Ok(t) => {
                        current = t;
                        was_tag = true;
                    }
                    Err(_) => break,
                }
            }
            _ => break,
        }
    }
    was_tag.then_some(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes() {
        let h40 = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(match_route("/r/HEAD"), Some((2, Route::Head)));
        assert_eq!(match_route("/r.git/info/refs"), Some((6, Route::InfoRefs)));
        assert_eq!(
            match_route(&format!("/r/objects/{}/{}", &h40[..2], &h40[2..])),
            Some((2, Route::Loose))
        );
        assert_eq!(
            match_route(&format!("/r/objects/pack/pack-{h40}.idx")),
            Some((2, Route::Idx))
        );
        assert_eq!(
            match_route(&format!("/r/objects/pack/pack-{h40}.pack")),
            Some((2, Route::Pack))
        );
        assert_eq!(
            match_route(&format!("/r/objects/pack/pack-{h40}.rev")),
            None
        );
        assert_eq!(match_route("/r/objects/info/foo"), None);
        assert_eq!(match_route("/r/refs/heads/main"), None);
        assert_eq!(match_route("/r/objects/00/0000"), None);
    }

    #[test]
    fn aliasing() {
        assert!(!aliased("/repo.git/HEAD"));
        assert!(aliased("/repo.git/../HEAD"));
        assert!(aliased("/../etc/passwd"));
        assert!(aliased("../etc/passwd"));
        assert!(aliased("/etc//passwd"));
        assert!(aliased("/etc/./passwd"));
        assert!(aliased("//domain/data.txt"));
        assert!(aliased("/repo.git/."));
    }

    #[test]
    fn namespaces() {
        assert_eq!(namespace_prefix(None), "");
        assert_eq!(
            namespace_prefix(Some("a/b")),
            "refs/namespaces/a/refs/namespaces/b/"
        );
    }
}
