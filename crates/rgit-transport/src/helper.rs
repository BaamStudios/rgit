// SPDX-License-Identifier: MIT
//! Remote helpers (`Documentation/gitremote-helpers.adoc`): transports
//! that are separate programs, `git remote-<transport>`, driven through a
//! line protocol on their standard input and output.
//!
//! # Which URLs use a helper
//!
//! - `<transport>::<address>`: `<transport>` consists of ASCII letters,
//!   digits, `+`, `-` and `.` (possibly none at all, observed with git
//!   2.56);
//! - `<scheme>://...` for a scheme rgit does not speak itself (anything
//!   but `file`, `ssh` and its aliases, `git`, `http` and `https`);
//! - any URL of a remote with `remote.<name>.vcs`.
//!
//! The helper gets two arguments: the remote's name (for a URL given on
//! the command line: the URL itself) and the address (the part after
//! `::`, or the whole URL). A `vcs` remote without URL gets its name
//! twice (observed). `GIT_DIR` is set for the helper when there is a
//! repository.
//!
//! # Protocol
//!
//! [`Helper::start`] runs the helper and reads its `capabilities`. A
//! helper with `connect` becomes an ordinary [`Transport`]
//! ([`HelperTransport`]): `connect <service>` turns its standard input
//! and output into the service's. Other helpers are driven by the
//! callers through [`Helper::list`], [`Helper::fetch`],
//! [`Helper::import`], [`Helper::push`] and [`Helper::export`].
//!
//! Commands and answers are lines; answers are read one byte at a time
//! so that nothing past the answer is consumed (after `import` the
//! helper's output is handed to `git fast-import` as it is).

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use bstr::ByteSlice;

use crate::child::ChildConnection;
use crate::command::Command;
use crate::{ConnectOptions, Connection, Error, Result, Transport, messages};

/// What the caller knows about the remote besides its URL.
#[derive(Debug, Clone, Default)]
pub struct HelperContext {
    /// The name of a configured remote (`None` for a URL given on the
    /// command line).
    pub remote: Option<String>,
    /// `remote.<name>.vcs`.
    pub vcs: Option<String>,
    /// The repository the helper works for (its `GIT_DIR`).
    pub git_dir: Option<PathBuf>,
}

/// The helper program to run and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperSpec {
    /// `<transport>` of `git remote-<transport>`.
    pub name: String,
    /// The first argument: remote name or URL.
    pub remote: String,
    /// The second argument: the address.
    pub url: String,
}

/// Schemes rgit handles without a helper.
const NATIVE_SCHEMES: &[&str] = &["file", "ssh", "git+ssh", "ssh+git", "git", "http", "https"];

fn transport_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || b"+-.".contains(&c)
}

/// `<transport>` and `<address>` of a `<transport>::<address>` URL.
pub fn split_transport(url: &str) -> Option<(&str, &str)> {
    let i = url.find("::")?;
    let name = &url[..i];
    name.bytes()
        .all(transport_char)
        .then(|| (name, &url[i + 2..]))
}

/// The scheme of a `<scheme>://` URL that needs a helper.
fn foreign_scheme(url: &str) -> Option<&str> {
    let i = url.find("://")?;
    let scheme = &url[..i];
    let b = scheme.as_bytes();
    let valid =
        !b.is_empty() && b[0].is_ascii_alphabetic() && b.iter().copied().all(transport_char);
    (valid && !NATIVE_SCHEMES.contains(&scheme)).then_some(scheme)
}

/// The helper `url` (with `ctx`) is reached through, if any.
pub fn spec_for(url: &str, ctx: &HelperContext) -> Option<HelperSpec> {
    let first = |default: &str| ctx.remote.clone().unwrap_or_else(|| default.to_owned());
    if let Some(vcs) = &ctx.vcs {
        let remote = first(url);
        let address = if url.is_empty() {
            remote.clone()
        } else {
            url.to_owned()
        };
        return Some(HelperSpec {
            name: vcs.clone(),
            remote,
            url: address,
        });
    }
    if let Some((name, address)) = split_transport(url) {
        return Some(HelperSpec {
            name: name.to_owned(),
            remote: first(url),
            url: address.to_owned(),
        });
    }
    foreign_scheme(url).map(|scheme| HelperSpec {
        name: scheme.to_owned(),
        remote: first(url),
        url: url.to_owned(),
    })
}

/// The protocol name `protocol.<name>.allow` checks for a helper URL
/// (`ext` for `ext::...`), `None` for URLs without helper.
pub fn protocol_name(url: &str) -> Option<String> {
    spec_for(url, &HelperContext::default()).map(|s| s.name)
}

/// Capabilities a helper announced.
#[derive(Debug, Clone, Default)]
pub struct Capabilities {
    /// `fetch`.
    pub fetch: bool,
    /// `import`.
    pub import: bool,
    /// `push`.
    pub push: bool,
    /// `export`.
    pub export: bool,
    /// `connect`.
    pub connect: bool,
    /// `stateless-connect`.
    pub stateless_connect: bool,
    /// `option`.
    pub option: bool,
    /// `refspec <spec>` lines, in order.
    pub refspecs: Vec<String>,
    /// `bidi-import`.
    pub bidi_import: bool,
    /// `export-marks <file>`.
    pub export_marks: Option<String>,
    /// `import-marks <file>`.
    pub import_marks: Option<String>,
    /// `signed-tags`.
    pub signed_tags: bool,
    /// `check-connectivity`.
    pub check_connectivity: bool,
    /// `no-private-update`.
    pub no_private_update: bool,
    /// `object-format`.
    pub object_format: bool,
    /// `get`.
    pub get: bool,
}

impl Capabilities {
    /// Record one line of the `capabilities` answer. Unknown mandatory
    /// capabilities (`*<name>`) are an error.
    fn add(&mut self, line: &str) -> Result<()> {
        let (mandatory, cap) = match line.strip_prefix('*') {
            Some(rest) => (true, rest),
            None => (false, line),
        };
        match cap {
            "fetch" => self.fetch = true,
            "import" => self.import = true,
            "push" => self.push = true,
            "export" => self.export = true,
            "connect" => self.connect = true,
            "stateless-connect" => self.stateless_connect = true,
            "option" => self.option = true,
            "bidi-import" => self.bidi_import = true,
            "signed-tags" => self.signed_tags = true,
            "check-connectivity" => self.check_connectivity = true,
            "no-private-update" => self.no_private_update = true,
            "object-format" => self.object_format = true,
            "get" => self.get = true,
            _ => {
                if let Some(s) = cap.strip_prefix("refspec ") {
                    self.refspecs.push(s.to_owned());
                } else if let Some(f) = cap.strip_prefix("export-marks ") {
                    self.export_marks = Some(f.to_owned());
                } else if let Some(f) = cap.strip_prefix("import-marks ") {
                    self.import_marks = Some(f.to_owned());
                } else if mandatory {
                    return Err(Error::Other(messages::unknown_mandatory_capability(cap)));
                }
            }
        }
        Ok(())
    }

    /// The private ref the helper writes for the remote ref `name`: the
    /// first `refspec` whose left side matches, else `name` itself (the
    /// implied `*:*`).
    pub fn private_ref(&self, name: &str) -> Option<String> {
        if self.refspecs.is_empty() {
            return Some(name.to_owned());
        }
        self.refspecs.iter().find_map(|s| map_refspec(s, name))
    }
}

/// Apply one `src:dst` refspec (at most one `*` per side) to `name`.
fn map_refspec(spec: &str, name: &str) -> Option<String> {
    let spec = spec.strip_prefix('+').unwrap_or(spec);
    let (src, dst) = spec.split_once(':').unwrap_or((spec, spec));
    match (src.split_once('*'), dst.split_once('*')) {
        (Some((sp, ss)), Some((dp, ds))) => {
            let mid = name.strip_prefix(sp)?.strip_suffix(ss)?;
            Some(format!("{dp}{mid}{ds}"))
        }
        (None, None) if src == name => Some(dst.to_owned()),
        _ => None,
    }
}

/// One line of the answer to `list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedRef {
    /// The ref name.
    pub name: String,
    /// The value: a hex object id, `None` for `?` (unknown).
    pub value: Option<String>,
    /// `@<target>`: a symbolic ref.
    pub symref: Option<String>,
}

/// The answer to `list`.
#[derive(Debug, Clone, Default)]
pub struct RefList {
    /// The refs in the order listed.
    pub refs: Vec<ListedRef>,
    /// `:object-format <name>`.
    pub object_format: Option<String>,
}

/// The answer to `option`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptionReply {
    /// `ok`.
    Ok,
    /// `unsupported` (or no `option` capability).
    Unsupported,
    /// `error <msg>`.
    Error(String),
}

/// The status of one ref after `push` or `export`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushStatus {
    /// The remote ref.
    pub name: String,
    /// `None` for `ok`, the reason for `error` (possibly empty).
    pub error: Option<String>,
}

/// The answer to a batch of `fetch` commands.
#[derive(Debug, Clone, Default)]
pub struct FetchReply {
    /// `lock <file>`: `.keep` files to remove after the refs are written.
    pub locks: Vec<PathBuf>,
    /// `connectivity-ok`.
    pub connectivity_ok: bool,
}

/// How to run `git` for the helper and for `fast-import`/`fast-export`:
/// the `git` of the exec path (`GIT_EXEC_PATH` among `base_env`), else
/// this program.
pub fn git_program(base_env: &[(OsString, OsString)]) -> OsString {
    let exec_path = base_env
        .iter()
        .rev()
        .find(|(k, _)| k == "GIT_EXEC_PATH")
        .map(|(_, v)| v.clone())
        .or_else(|| std::env::var_os("GIT_EXEC_PATH"));
    if let Some(dir) = exec_path {
        let git = Path::new(&dir).join("git");
        if git.is_file() {
            return git.into_os_string();
        }
    }
    std::env::current_exe()
        .map(PathBuf::into_os_string)
        .unwrap_or_else(|_| "git".into())
}

/// `GIT_DIR` for a helper: `.git` when that is the repository of the
/// current directory (as git passes it from the top of a work tree),
/// else the path as given.
fn git_dir_value(dir: &Path) -> OsString {
    let here = std::env::current_dir().ok().map(|d| d.join(".git"));
    if here.as_deref() == Some(dir) {
        return ".git".into();
    }
    dir.as_os_str().to_owned()
}

/// A running remote helper.
pub struct Helper {
    spec: HelperSpec,
    conn: ChildConnection,
    caps: Capabilities,
    base_env: Vec<(OsString, OsString)>,
    git_dir: Option<PathBuf>,
    debug: bool,
}

impl std::fmt::Debug for Helper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Helper").field("spec", &self.spec).finish()
    }
}

impl Helper {
    /// Run `git remote-<name>` and read its capabilities.
    pub fn start(
        spec: HelperSpec,
        ctx: &HelperContext,
        base_env: &[(OsString, OsString)],
    ) -> Result<Helper> {
        let mut cmd = Command::new(git_program(base_env));
        cmd.arg(format!("remote-{}", spec.name))
            .arg(&spec.remote)
            .arg(&spec.url);
        cmd.base_env(base_env);
        if let Some(dir) = &ctx.git_dir {
            cmd.base_env(&[("GIT_DIR".into(), git_dir_value(dir))]);
        }
        let conn = ChildConnection::spawn(&cmd)?;
        let debug = std::env::var_os("GIT_TRANSPORT_HELPER_DEBUG").is_some();
        let mut helper = Helper {
            spec,
            conn,
            caps: Capabilities::default(),
            base_env: base_env.to_vec(),
            git_dir: ctx.git_dir.clone(),
            debug,
        };
        helper.write_line("capabilities")?;
        loop {
            let line = helper.read_line_required()?;
            if line.is_empty() {
                break;
            }
            if helper.debug {
                eprintln!("{}", messages::debug_got_cap(line.trim_start_matches('*')));
            }
            helper.caps.add(&line)?;
        }
        if helper.debug {
            eprintln!("{}", messages::DEBUG_CAPS_COMPLETE);
        }
        Ok(helper)
    }

    /// The helper's capabilities.
    pub fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    /// The program and its arguments.
    pub fn spec(&self) -> &HelperSpec {
        &self.spec
    }

    fn aborted(&self) -> Error {
        Error::Other(messages::helper_aborted(&self.spec.name))
    }

    fn write_line(&mut self, line: &str) -> Result<()> {
        if self.debug {
            eprintln!("{}", messages::debug_sent(line));
        }
        let mut buf = line.as_bytes().to_vec();
        buf.push(b'\n');
        if self
            .conn
            .write_all(&buf)
            .and_then(|()| self.conn.flush())
            .is_err()
        {
            return Err(self.aborted());
        }
        Ok(())
    }

    /// One line of output without its newline; `None` at end of file.
    fn read_line(&mut self) -> Result<Option<String>> {
        if self.debug {
            eprintln!("{}", messages::DEBUG_WAITING);
        }
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match self.conn.read(&mut byte) {
                Ok(0) => {
                    if line.is_empty() {
                        return Ok(None);
                    }
                    break;
                }
                Ok(_) if byte[0] == b'\n' => break,
                Ok(_) => line.push(byte[0]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Ok(None),
            }
        }
        let line = String::from_utf8_lossy(&line).into_owned();
        if self.debug {
            eprintln!("{}", messages::debug_received(&line));
        }
        Ok(Some(line))
    }

    fn read_line_required(&mut self) -> Result<String> {
        match self.read_line()? {
            Some(l) => Ok(l),
            None => Err(self.aborted()),
        }
    }

    /// `option <name> <value>`; without the `option` capability the
    /// option counts as unsupported.
    pub fn set_option(&mut self, name: &str, value: &str) -> Result<OptionReply> {
        if !self.caps.option {
            return Ok(OptionReply::Unsupported);
        }
        self.write_line(&format!("option {name} {value}"))?;
        let line = self.read_line_required()?;
        Ok(if line == "ok" {
            OptionReply::Ok
        } else if let Some(msg) = line.strip_prefix("error") {
            OptionReply::Error(msg.trim_start().to_owned())
        } else {
            OptionReply::Unsupported
        })
    }

    /// The options git sets after the capabilities: `progress`,
    /// `verbosity` and (when announced) `object-format true`.
    pub fn standard_options(&mut self, progress: bool, verbosity: i32) -> Result<()> {
        self.set_option("progress", if progress { "true" } else { "false" })?;
        self.set_option("verbosity", &verbosity.max(0).to_string())?;
        if self.caps.object_format {
            self.set_option("object-format", "true")?;
        }
        Ok(())
    }

    /// `list` (or `list for-push`).
    pub fn list(&mut self, for_push: bool) -> Result<RefList> {
        self.write_line(if for_push { "list for-push" } else { "list" })?;
        let mut out = RefList::default();
        loop {
            let line = self.read_line_required()?;
            if line.is_empty() {
                break;
            }
            if let Some(kv) = line.strip_prefix(':') {
                if let Some(v) = kv.strip_prefix("object-format ") {
                    out.object_format = Some(v.to_owned());
                }
                continue;
            }
            let (value, rest) = line.split_once(' ').unwrap_or((line.as_str(), ""));
            // Attributes (`unchanged`) follow the name.
            let name = rest.split(' ').next().unwrap_or("").to_owned();
            if name.is_empty() {
                continue;
            }
            let entry = if let Some(target) = value.strip_prefix('@') {
                ListedRef {
                    name,
                    value: None,
                    symref: Some(target.to_owned()),
                }
            } else if value == "?" {
                ListedRef {
                    name,
                    value: None,
                    symref: None,
                }
            } else {
                ListedRef {
                    name,
                    value: Some(value.to_owned()),
                    symref: None,
                }
            };
            out.refs.push(entry);
        }
        if self.debug {
            eprintln!("{}", messages::DEBUG_READ_LISTING);
        }
        Ok(out)
    }

    /// `fetch <oid> <name>` for each pair, then wait for the batch to
    /// finish.
    pub fn fetch(&mut self, wants: &[(String, String)]) -> Result<FetchReply> {
        for (oid, name) in wants {
            self.write_line(&format!("fetch {oid} {name}"))?;
        }
        self.write_line("")?;
        let mut reply = FetchReply::default();
        loop {
            let line = self.read_line_required()?;
            if line.is_empty() {
                break;
            }
            if let Some(f) = line.strip_prefix("lock ") {
                reply.locks.push(PathBuf::from(f));
            } else if line == "connectivity-ok" {
                reply.connectivity_ok = true;
            } else {
                return Err(Error::Other(messages::unexpected_helper_line(&line)));
            }
        }
        Ok(reply)
    }

    /// A `git <args>` child for `fast-import` / `fast-export` in the
    /// helper's repository.
    fn git_command(&self, args: &[String]) -> Command {
        let mut cmd = Command::new(git_program(&self.base_env));
        for a in args {
            cmd.arg(a);
        }
        cmd.base_env(&self.base_env);
        if let Some(dir) = &self.git_dir {
            cmd.base_env(&[("GIT_DIR".into(), git_dir_value(dir))]);
        }
        cmd
    }

    /// `import <name>` for each name: the helper's output is read by
    /// `git fast-import` (started first, with `fast_import_args` after
    /// `fast-import`), which must succeed. The caller reads the imported
    /// values from the private refs ([`Capabilities::private_ref`]).
    pub fn import(&mut self, names: &[String], fast_import_args: &[String]) -> Result<()> {
        let mut args = vec!["fast-import".to_owned()];
        args.extend(fast_import_args.iter().cloned());
        let cmd = self.git_command(&args);
        cmd.trace();
        let mut c = cmd.to_std();
        c.stdin(self.conn.stdout_stdio()?);
        if self.caps.bidi_import {
            c.stdout(self.conn.stdin_stdio()?);
        }
        let mut child = c.spawn().map_err(|e| crate::child::spawn_error(&cmd, e))?;
        drop(c);
        for name in names {
            self.write_line(&format!("import {name}"))?;
        }
        self.write_line("")?;
        let status = child.wait()?;
        if !status.success() {
            return Err(Error::Other(messages::FAST_IMPORT_FAILED.into()));
        }
        Ok(())
    }

    /// `export`: `git fast-export` with `fast_export_args` writes the
    /// stream to the helper, which answers with the status of each ref.
    /// The export marks (`export-marks`) are written to `<file>.tmp` and
    /// renamed when the helper has answered.
    pub fn export(&mut self, fast_export_args: &[String]) -> Result<Vec<PushStatus>> {
        self.write_line("export")?;
        let mut args = vec!["fast-export".to_owned(), "--use-done-feature".to_owned()];
        args.push(
            if self.caps.signed_tags {
                "--signed-tags=verbatim"
            } else {
                "--signed-tags=warn-strip"
            }
            .to_owned(),
        );
        if let Some(m) = &self.caps.export_marks {
            args.push(format!("--export-marks={m}.tmp"));
        }
        if let Some(m) = &self.caps.import_marks {
            args.push(format!("--import-marks={m}"));
        }
        args.extend(fast_export_args.iter().cloned());
        let cmd = self.git_command(&args);
        cmd.trace();
        let mut c = cmd.to_std();
        c.stdout(self.conn.stdin_stdio()?);
        let mut child = c.spawn().map_err(|e| crate::child::spawn_error(&cmd, e))?;
        drop(c);
        let status = child.wait()?;
        if !status.success() {
            return Err(Error::Other(messages::FAST_EXPORT_FAILED.into()));
        }
        let statuses = self.read_push_status()?;
        if let Some(m) = &self.caps.export_marks {
            let tmp = format!("{m}.tmp");
            if let Err(e) = std::fs::rename(&tmp, m) {
                eprintln!(
                    "warning: {}",
                    messages::could_not_rename(&tmp, m, &rgit_trace::messages::strerror(&e))
                );
            }
        }
        Ok(statuses)
    }

    /// One batch of `push [+]<src>:<dst>` commands (an empty `src`
    /// deletes).
    pub fn push(&mut self, commands: &[(bool, String, String)]) -> Result<Vec<PushStatus>> {
        for (force, src, dst) in commands {
            let plus = if *force { "+" } else { "" };
            self.write_line(&format!("push {plus}{src}:{dst}"))?;
        }
        self.write_line("")?;
        self.read_push_status()
    }

    fn read_push_status(&mut self) -> Result<Vec<PushStatus>> {
        let mut out = Vec::new();
        loop {
            let line = self.read_line_required()?;
            if line.is_empty() {
                break;
            }
            if let Some(rest) = line.strip_prefix("ok ") {
                out.push(PushStatus {
                    name: rest.split(' ').next().unwrap_or(rest).to_owned(),
                    error: None,
                });
            } else if let Some(rest) = line.strip_prefix("error ") {
                let (name, why) = rest.split_once(' ').unwrap_or((rest, ""));
                let why = if why.starts_with('"') {
                    c_unquote(why).unwrap_or_else(|| why.to_owned())
                } else {
                    why.to_owned()
                };
                out.push(PushStatus {
                    name: name.to_owned(),
                    error: Some(why),
                });
            } else {
                return Err(Error::Other(messages::unexpected_helper_line(&line)));
            }
        }
        Ok(out)
    }

    /// `connect <service>`: on success the helper's standard input and
    /// output are the service's. `Ok(Err(helper))` when the helper asks
    /// to fall back to other commands.
    pub fn connect(
        mut self,
        options: &ConnectOptions,
    ) -> Result<std::result::Result<Box<dyn Connection>, Helper>> {
        if let Some(program) = &options.program {
            self.set_option("servpath", &c_quote(program))?;
        }
        self.write_line(&format!("connect {}", options.service.name()))?;
        let line = self.read_line_required()?;
        if line.is_empty() {
            if self.debug {
                eprintln!("{}", messages::DEBUG_CONNECTED);
            }
            return Ok(Ok(Box::new(self.conn)));
        }
        if line == "fallback" {
            return Ok(Err(self));
        }
        Err(Error::Other(messages::unknown_connect_response(&line)))
    }

    /// End the session: an empty line, then wait for the helper.
    pub fn disconnect(mut self) -> Result<()> {
        if self.debug {
            eprintln!("{}", messages::DEBUG_DISCONNECTING);
        }
        let _ = self.write_line("");
        let conn: Box<dyn Connection> = Box::new(self.conn);
        // How the helper exits after the session does not matter.
        let _ = conn.close();
        Ok(())
    }
}

/// A [`Transport`] through a helper with the `connect` capability. Every
/// connection runs the helper anew (a helper serves one connection); the
/// helper started to learn its capabilities serves the first one.
pub struct HelperTransport {
    spec: HelperSpec,
    ctx: HelperContext,
    base_env: Vec<(OsString, OsString)>,
    first: Option<Helper>,
}

impl HelperTransport {
    /// Wrap a started helper that announced `connect`.
    pub fn new(helper: Helper, ctx: &HelperContext) -> Self {
        HelperTransport {
            spec: helper.spec.clone(),
            ctx: ctx.clone(),
            base_env: helper.base_env.clone(),
            first: Some(helper),
        }
    }
}

impl Transport for HelperTransport {
    fn connect(&mut self, options: &ConnectOptions) -> Result<Box<dyn Connection>> {
        let helper = match self.first.take() {
            Some(h) => h,
            None => Helper::start(self.spec.clone(), &self.ctx, &self.base_env)?,
        };
        let name = helper.spec.name.clone();
        match helper.connect(options)? {
            Ok(conn) => Ok(conn),
            Err(h) => {
                let _ = h.disconnect();
                Err(Error::Other(messages::cannot_connect_to_subservice(&name)))
            }
        }
    }
}

/// What a URL leads to: a transport for the pack protocol, or a helper
/// without `connect` that the caller drives command by command.
pub enum Target {
    /// A transport speaking the pack protocol.
    Transport(Box<dyn Transport>),
    /// A helper with `fetch`/`import`/`push`/`export`.
    Helper(Box<Helper>),
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Target::Transport(_) => f.write_str("Target::Transport"),
            Target::Helper(h) => f.debug_tuple("Target::Helper").field(h).finish(),
        }
    }
}

/// Start the helper of `spec` and decide how to use it.
pub fn start_target(
    spec: HelperSpec,
    ctx: &HelperContext,
    base_env: &[(OsString, OsString)],
) -> Result<Target> {
    let helper = Helper::start(spec, ctx, base_env)?;
    if helper.caps.connect {
        return Ok(Target::Transport(Box::new(HelperTransport::new(
            helper, ctx,
        ))));
    }
    Ok(Target::Helper(Box::new(helper)))
}

/// C-style quoting (`"..."` with backslash escapes) when `s` needs it.
pub fn c_quote(s: &str) -> String {
    let needs = s
        .bytes()
        .any(|b| b == b'"' || b == b'\\' || !(0x20..0x7f).contains(&b));
    if !needs {
        return s.to_owned();
    }
    let mut out = String::from("\"");
    for b in s.bytes() {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\t' => out.push_str("\\t"),
            b'\r' => out.push_str("\\r"),
            0x07 => out.push_str("\\a"),
            0x08 => out.push_str("\\b"),
            0x0b => out.push_str("\\v"),
            0x0c => out.push_str("\\f"),
            b if !(0x20..0x7f).contains(&b) => out.push_str(&format!("\\{b:03o}")),
            b => out.push(b as char),
        }
    }
    out.push('"');
    out
}

/// Undo [`c_quote`]; `None` when `s` is not a complete quoted string.
pub fn c_unquote(s: &str) -> Option<String> {
    let b = s.as_bytes();
    if b.len() < 2 || b[0] != b'"' {
        return None;
    }
    let mut out = Vec::new();
    let mut i = 1;
    while i < b.len() {
        match b[i] {
            b'"' => return Some(String::from_utf8_lossy(&out).into_owned()),
            b'\\' => {
                i += 1;
                let c = *b.get(i)?;
                match c {
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'r' => out.push(b'\r'),
                    b'a' => out.push(0x07),
                    b'b' => out.push(0x08),
                    b'v' => out.push(0x0b),
                    b'f' => out.push(0x0c),
                    b'0'..=b'3' => {
                        let oct = b.get(i..i + 3)?;
                        let v = oct
                            .to_str()
                            .ok()
                            .and_then(|o| u8::from_str_radix(o, 8).ok())?;
                        out.push(v);
                        i += 2;
                    }
                    other => out.push(other),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(
        url: &str,
        remote: Option<&str>,
        vcs: Option<&str>,
    ) -> Option<(String, String, String)> {
        let ctx = HelperContext {
            remote: remote.map(Into::into),
            vcs: vcs.map(Into::into),
            git_dir: None,
        };
        spec_for(url, &ctx).map(|s| (s.name, s.remote, s.url))
    }

    fn t(a: &str, b: &str, c: &str) -> Option<(String, String, String)> {
        Some((a.into(), b.into(), c.into()))
    }

    #[test]
    fn helper_urls() {
        // Observed with git 2.56 (arguments of the helper).
        assert_eq!(
            spec("show::foo/bar", None, None),
            t("show", "show::foo/bar", "foo/bar")
        );
        assert_eq!(
            spec("show://foo/bar", None, None),
            t("show", "show://foo/bar", "show://foo/bar")
        );
        assert_eq!(spec("show::abc", Some("r1"), None), t("show", "r1", "abc"));
        assert_eq!(
            spec("show://abc", Some("r2"), None),
            t("show", "r2", "show://abc")
        );
        assert_eq!(
            spec("/some/path", Some("r3"), Some("show")),
            t("show", "r3", "/some/path")
        );
        assert_eq!(spec("", Some("r4"), Some("show")), t("show", "r4", "r4"));
        assert_eq!(spec("::x", None, None), t("", "::x", "x"));
        assert_eq!(spec("9s::x", None, None), t("9s", "9s::x", "x"));
        assert_eq!(spec("s.h+-::x", None, None), t("s.h+-", "s.h+-::x", "x"));
        assert_eq!(
            spec("ext::sh -c %S% .", None, None),
            t("ext", "ext::sh -c %S% .", "sh -c %S% .")
        );
        for native in [
            "a/b::x",
            "s_h::x",
            "sh ow::x",
            "https://h/r",
            "http://h/r",
            "git://h/r",
            "ssh://h/r",
            "file:///r",
            "/local/path",
            "host:path",
        ] {
            assert_eq!(spec(native, None, None), None, "{native}");
        }
        assert_eq!(protocol_name("ext::x").as_deref(), Some("ext"));
        assert_eq!(protocol_name("foo://x").as_deref(), Some("foo"));
        assert_eq!(protocol_name("ssh://x/y"), None);
    }

    #[test]
    fn capabilities_and_refspecs() {
        let mut c = Capabilities::default();
        for l in [
            "import",
            "export",
            "refspec refs/heads/*:refs/testgit/origin/heads/*",
            "refspec refs/tags/*:refs/testgit/origin/tags/*",
            "*import-marks /x/git.marks",
            "*export-marks /x/git.marks",
            "option",
            "false",
        ] {
            c.add(l).unwrap();
        }
        assert!(c.import && c.export && c.option && !c.fetch);
        assert_eq!(c.import_marks.as_deref(), Some("/x/git.marks"));
        assert_eq!(
            c.private_ref("refs/heads/main").as_deref(),
            Some("refs/testgit/origin/heads/main")
        );
        assert_eq!(
            c.private_ref("refs/tags/v1").as_deref(),
            Some("refs/testgit/origin/tags/v1")
        );
        assert_eq!(c.private_ref("refs/notes/x"), None);
        assert!(c.add("*frobnicate").is_err());
        assert!(c.add("frobnicate").is_ok());
        let none = Capabilities::default();
        assert_eq!(
            none.private_ref("refs/heads/a").as_deref(),
            Some("refs/heads/a")
        );
        assert_eq!(
            map_refspec("refs/heads/a:refs/x/a", "refs/heads/a").as_deref(),
            Some("refs/x/a")
        );
    }

    #[test]
    fn quoting() {
        assert_eq!(c_quote("plain path"), "plain path");
        assert_eq!(c_quote("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
        assert_eq!(
            c_unquote("\"a\\\"b\\\\c\\n\"").as_deref(),
            Some("a\"b\\c\n")
        );
        assert_eq!(c_unquote("\"\\303\\244\"").as_deref(), Some("ä"));
        assert_eq!(c_unquote("\"open"), None);
    }
}
