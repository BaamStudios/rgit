// SPDX-License-Identifier: MIT

//! Filter drivers (gitattributes(5) `filter`): external `clean` and
//! `smudge` commands run once per file, and long-running `process`
//! filters that speak the protocol of
//! Documentation/technical/long-running-process-protocol.adoc.
//!
//! Behaviour observed from git 2.56:
//!
//! - `%f` in `clean` / `smudge` becomes the shell-quoted path; the command
//!   is run through `/bin/sh -c` when it contains shell syntax, from the
//!   top of the work tree. A filter that does not read its input is fine.
//! - A single-file filter that exits with status `n` is reported as
//!   `error: external filter '<cmd>' failed <n>` and
//!   `error: external filter '<cmd>' failed`; the content then passes
//!   unchanged, unless the driver is `required` (the caller dies).
//! - A `process` filter takes precedence over `clean` / `smudge`. It is
//!   started for the first file that needs it and kept for the rest of the
//!   command (one process per command line). A failed start is retried for
//!   the next file. `status=error` fails one file; `status=abort` stops
//!   using the capability; a broken conversation
//!   (`error: external filter '<cmd>' failed`) stops the process, which is
//!   started again for the next file.
//! - At exit the filter's standard input is closed and rgit waits for it.

use std::ffi::{OsStr, OsString};
use std::io::{BufWriter, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::Mutex;

use rgit_protocol::pktline::{MAX_DATA_LEN, Packet, Reader, Writer};
use rgit_transport::command::{Command, sq_quote};
use rgit_transport::sigpipe::IgnoreSigpipe;

use crate::messages;

/// A filter driver (`filter.<name>.*`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Driver {
    /// The driver name (the `filter` attribute's value).
    pub name: String,
    /// `filter.<name>.clean` (an empty value counts as unset).
    pub clean: Option<String>,
    /// `filter.<name>.smudge`.
    pub smudge: Option<String>,
    /// `filter.<name>.process`.
    pub process: Option<String>,
    /// `filter.<name>.required`.
    pub required: bool,
}

impl Driver {
    /// The command run for `dir` when no `process` filter is configured.
    #[must_use]
    pub fn command(&self, dir: Direction) -> Option<&str> {
        match dir {
            Direction::Clean => self.clean.as_deref(),
            Direction::Smudge => self.smudge.as_deref(),
        }
    }
}

/// Which way content goes through a filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Into the repository (`clean`).
    Clean,
    /// Into the work tree (`smudge`).
    Smudge,
}

impl Direction {
    fn word(self) -> &'static str {
        match self {
            Direction::Clean => "clean",
            Direction::Smudge => "smudge",
        }
    }
}

/// What a `process` filter is told about the blob it smudges.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Meta {
    /// The ref being checked out (`ref=`).
    pub refname: Option<String>,
    /// The commit or tree being checked out (`treeish=`).
    pub treeish: Option<String>,
    /// The blob (`blob=`).
    pub blob: Option<String>,
}

/// Where filters run.
#[derive(Debug, Clone, Default)]
pub struct Context {
    /// The directory filters run in (the top of the work tree).
    pub cwd: Option<PathBuf>,
    /// Environment variables to set for the filter.
    pub env: Vec<(OsString, OsString)>,
}

/// The result of [`apply`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The filtered content.
    Filtered(Vec<u8>),
    /// The driver has no command for this direction (or the process filter
    /// lacks or aborted the capability).
    NotRun,
    /// The filter failed (already reported).
    Failed,
    /// A `process` filter delayed the blob (only when asked with
    /// `can_delay`); fetch it after [`list_available`] names it.
    Delayed,
}

/// Environment the command line front end wants every filter to see
/// (exports of the global options).
static BASE_ENV: Mutex<Vec<(OsString, OsString)>> = Mutex::new(Vec::new());

/// The running `process` filters, by command.
static PROCESSES: Mutex<Vec<Process>> = Mutex::new(Vec::new());

/// Sets the environment every filter inherits on top of this process's.
pub fn set_base_env(env: Vec<(OsString, OsString)>) {
    *BASE_ENV.lock().unwrap_or_else(|e| e.into_inner()) = env;
}

/// Ends all `process` filters: closes their input and waits for them.
pub fn finish_all() {
    let procs = std::mem::take(&mut *PROCESSES.lock().unwrap_or_else(|e| e.into_inner()));
    for p in procs {
        p.finish();
    }
}

/// Runs `data`, the content of `path`, through `driver` in direction `dir`.
/// With `can_delay` a `process` filter that supports `delay` may answer
/// [`Outcome::Delayed`].
pub fn apply(
    driver: &Driver,
    dir: Direction,
    path: &[u8],
    data: &[u8],
    meta: &Meta,
    ctx: &Context,
    can_delay: bool,
) -> Outcome {
    if let Some(cmd) = &driver.process {
        return apply_process(cmd, dir, path, data, meta, ctx, can_delay);
    }
    match driver.command(dir) {
        Some(cmd) => run_single(cmd, path, data, ctx),
        None => Outcome::NotRun,
    }
}

fn command_for(cmd: &OsStr, ctx: &Context) -> std::process::Command {
    let mut c = Command::shell(cmd.to_owned());
    c.base_env(&BASE_ENV.lock().unwrap_or_else(|e| e.into_inner()));
    c.trace();
    let mut std = c.to_std();
    for (k, v) in &ctx.env {
        std.env(k, v);
    }
    if let Some(d) = &ctx.cwd {
        std.current_dir(d);
    }
    std
}

/// Reports a failed start like git: `cannot run` for a program looked up
/// in `PATH`, `cannot exec` otherwise.
fn report_spawn_error(cmd: &str, e: &std::io::Error) {
    let reason = rgit_trace::messages::strerror(e);
    let program = cmd.split_ascii_whitespace().next().unwrap_or(cmd);
    if program.contains('/') {
        eprintln!("fatal: {}", messages::cannot_exec(program, &reason));
    } else {
        eprintln!("error: {}", messages::cannot_run(program, &reason));
    }
}

/// `%f` replaced by the quoted path.
fn expand(cmd: &str, path: &[u8]) -> OsString {
    let bytes = cmd.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() + path.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && bytes.get(i + 1) == Some(&b'f') {
            out.extend_from_slice(&sq_quote(path));
            i += 2;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    os_from_bytes(out)
}

#[cfg(unix)]
fn os_from_bytes(b: Vec<u8>) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(b)
}

#[cfg(not(unix))]
fn os_from_bytes(b: Vec<u8>) -> OsString {
    OsString::from(String::from_utf8_lossy(&b).into_owned())
}

fn run_single(cmd: &str, path: &[u8], data: &[u8], ctx: &Context) -> Outcome {
    let mut c = command_for(&expand(cmd, path), ctx);
    c.stdin(Stdio::piped()).stdout(Stdio::piped());
    let _sigpipe = IgnoreSigpipe::new();
    let mut child = match c.spawn() {
        Ok(c) => c,
        Err(e) => {
            report_spawn_error(cmd, &e);
            eprintln!("error: {}", messages::cannot_fork_filter(cmd));
            eprintln!("error: {}", messages::filter_failed(cmd));
            return Outcome::Failed;
        }
    };
    let mut stdin = child.stdin.take().expect("piped");
    let mut stdout = child.stdout.take().expect("piped");
    let mut out = Vec::new();
    let read_ok = std::thread::scope(|s| {
        s.spawn(move || {
            // A filter may exit without reading everything.
            let _ = stdin.write_all(data);
        });
        stdout.read_to_end(&mut out).is_ok()
    });
    let status = child.wait();
    let ok = match status {
        Ok(st) if st.success() => read_ok,
        Ok(st) => {
            if let Some(code) = st.code() {
                eprintln!("error: {}", messages::filter_failed_code(cmd, code));
            }
            false
        }
        Err(_) => false,
    };
    if ok {
        Outcome::Filtered(out)
    } else {
        eprintln!("error: {}", messages::filter_failed(cmd));
        Outcome::Failed
    }
}

/// A running long-running filter process.
struct Process {
    cmd: String,
    child: Child,
    reader: Reader<ChildStdout>,
    writer: Writer<BufWriter<ChildStdin>>,
    clean: bool,
    smudge: bool,
    delay: bool,
}

/// A broken conversation (`external filter '<cmd>' failed`).
struct Broken;

impl Process {
    /// Starts `cmd` and performs the handshake. Errors are reported.
    fn start(cmd: &str, ctx: &Context) -> Option<Process> {
        let mut c = command_for(OsStr::new(cmd), ctx);
        c.stdin(Stdio::piped()).stdout(Stdio::piped());
        let mut child = match c.spawn() {
            Ok(c) => c,
            Err(e) => {
                report_spawn_error(cmd, &e);
                eprintln!("error: {}", messages::cannot_fork_subprocess(cmd));
                return None;
            }
        };
        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let mut reader = Reader::new(stdout, "git");
        reader.chomp_newline(false).err_packets(false);
        let mut p = Process {
            cmd: cmd.to_string(),
            child,
            reader,
            writer: Writer::new(BufWriter::new(stdin), "git"),
            clean: false,
            smudge: false,
            delay: false,
        };
        let _sigpipe = IgnoreSigpipe::new();
        match p.handshake() {
            Ok(()) => Some(p),
            Err(msg) => {
                if let Some(m) = msg {
                    eprintln!("error: {m}");
                }
                eprintln!("error: {}", messages::subprocess_init_failed(cmd));
                p.kill();
                None
            }
        }
    }

    /// Reads one packet as an owned value: `None` for a flush packet.
    fn read_line(&mut self) -> Result<Option<Vec<u8>>, ReadError> {
        match self.reader.read_or_eof() {
            Ok(Some(Packet::Flush)) => Ok(None),
            Ok(Some(Packet::Data(d))) => Ok(Some(d.to_vec())),
            Ok(Some(_)) => Err(ReadError::Protocol),
            Ok(None) => Err(ReadError::Eof),
            Err(_) => Err(ReadError::Protocol),
        }
    }

    /// Reads `key=value` lines up to a flush packet (trailing LF removed).
    fn read_list(&mut self) -> Result<Vec<Vec<u8>>, ReadError> {
        let mut out = Vec::new();
        while let Some(mut l) = self.read_line()? {
            if l.last() == Some(&b'\n') {
                l.pop();
            }
            out.push(l);
        }
        Ok(out)
    }

    fn handshake(&mut self) -> Result<(), Option<String>> {
        let w = (|| {
            self.writer.write_line("git-filter-client")?;
            self.writer.write_line("version=2")?;
            self.writer.flush_pkt()?;
            self.writer.flush_io()
        })();
        if w.is_err() {
            return Err(None);
        }
        let expect = |got: Result<Option<Vec<u8>>, ReadError>, want: &str| match got {
            Ok(Some(mut l)) => {
                if l.last() == Some(&b'\n') {
                    l.pop();
                }
                if l == want.as_bytes() {
                    Ok(())
                } else {
                    Err(Some(messages::unexpected_line(
                        &String::from_utf8_lossy(&l),
                        want,
                    )))
                }
            }
            Ok(None) => Err(Some(messages::unexpected_line("0000", want))),
            Err(_) => Err(None),
        };
        match self.read_line() {
            Err(ReadError::Eof) => {
                return Err(Some(messages::could_not_read_greeting(&self.cmd)));
            }
            r => expect(r, "git-filter-server")?,
        }
        let r = self.read_line();
        expect(r, "version=2")?;
        match self.read_line() {
            Ok(None) => {}
            Ok(Some(l)) => {
                return Err(Some(messages::unexpected_line(
                    &String::from_utf8_lossy(&l),
                    "flush packet",
                )));
            }
            Err(_) => return Err(None),
        }
        let w = (|| {
            self.writer.write_line("capability=clean")?;
            self.writer.write_line("capability=smudge")?;
            self.writer.write_line("capability=delay")?;
            self.writer.flush_pkt()?;
            self.writer.flush_io()
        })();
        if w.is_err() {
            return Err(None);
        }
        let caps = self.read_list().map_err(|_| None)?;
        for c in caps {
            match c.as_slice() {
                b"capability=clean" => self.clean = true,
                b"capability=smudge" => self.smudge = true,
                b"capability=delay" => self.delay = true,
                _ => {}
            }
        }
        Ok(())
    }

    fn supports(&self, dir: Direction) -> bool {
        match dir {
            Direction::Clean => self.clean,
            Direction::Smudge => self.smudge,
        }
    }

    fn drop_capability(&mut self, dir: Direction) {
        match dir {
            Direction::Clean => self.clean = false,
            Direction::Smudge => self.smudge = false,
        }
    }

    /// Sends one file and reads the answer.
    fn filter(
        &mut self,
        dir: Direction,
        path: &[u8],
        data: &[u8],
        meta: &Meta,
        can_delay: bool,
    ) -> Result<Answer, Broken> {
        let w = (|| -> rgit_protocol::Result<()> {
            self.writer.write_line(&format!("command={}", dir.word()))?;
            let mut l = b"pathname=".to_vec();
            l.extend_from_slice(path);
            l.push(b'\n');
            self.writer.write_data(&l)?;
            if dir == Direction::Smudge {
                if let Some(r) = &meta.refname {
                    self.writer.write_line(&format!("ref={r}"))?;
                }
                if let Some(t) = &meta.treeish {
                    self.writer.write_line(&format!("treeish={t}"))?;
                }
                if let Some(b) = &meta.blob {
                    self.writer.write_line(&format!("blob={b}"))?;
                }
                if can_delay && self.delay {
                    self.writer.write_line("can-delay=1")?;
                }
            }
            self.writer.flush_pkt()?;
            for chunk in data.chunks(MAX_DATA_LEN) {
                self.writer.write_data(chunk)?;
            }
            self.writer.flush_pkt()?;
            self.writer.flush_io()
        })();
        if w.is_err() {
            return Err(Broken);
        }
        let status = |list: &[Vec<u8>]| {
            list.iter()
                .rev()
                .find_map(|l| l.strip_prefix(b"status=").map(<[u8]>::to_vec))
        };
        let first = self.read_list().map_err(|_| Broken)?;
        let st = status(&first);
        match st.as_deref() {
            Some(b"success") => {}
            Some(b"abort") => return Ok(Answer::Abort),
            Some(b"delayed") if can_delay && self.delay => return Ok(Answer::Delayed),
            _ => return Ok(Answer::Error),
        }
        let mut content = Vec::new();
        while let Some(d) = self.read_line().map_err(|_| Broken)? {
            content.extend_from_slice(&d);
        }
        let second = self.read_list().map_err(|_| Broken)?;
        match status(&second).as_deref() {
            None | Some(b"success") => Ok(Answer::Content(content)),
            Some(b"abort") => Ok(Answer::Abort),
            Some(_) => Ok(Answer::Error),
        }
    }

    /// `command=list_available_blobs`: the delayed paths that are ready.
    fn list_available(&mut self) -> Result<Vec<Vec<u8>>, Broken> {
        let w = (|| -> rgit_protocol::Result<()> {
            self.writer.write_line("command=list_available_blobs")?;
            self.writer.flush_pkt()?;
            self.writer.flush_io()
        })();
        if w.is_err() {
            return Err(Broken);
        }
        let list = self.read_list().map_err(|_| Broken)?;
        let status = self.read_list().map_err(|_| Broken)?;
        if status
            .iter()
            .any(|l| l.starts_with(b"status=") && l != b"status=success")
        {
            return Err(Broken);
        }
        Ok(list
            .into_iter()
            .filter_map(|l| l.strip_prefix(b"pathname=").map(<[u8]>::to_vec))
            .collect())
    }

    /// Stops the process after a failure.
    fn kill(mut self) {
        // The signal goes first: with its input closed first, a filter
        // like `cat` would end by itself.
        #[cfg(unix)]
        {
            if let Ok(pid) = i32::try_from(self.child.id()) {
                // SAFETY: sends a signal to our own child process.
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
            }
        }
        #[cfg(not(unix))]
        let _ = self.child.kill();
        drop(self.writer);
        if let Ok(st) = self.child.wait() {
            report_signal(&self.cmd, st);
        }
    }

    /// Closes the input and waits (normal end).
    fn finish(mut self) {
        let _ = self.writer.flush_io();
        drop(self.writer);
        drop(self.reader);
        if let Ok(st) = self.child.wait() {
            report_signal(&self.cmd, st);
        }
    }
}

#[cfg(unix)]
fn report_signal(cmd: &str, st: std::process::ExitStatus) {
    use std::os::unix::process::ExitStatusExt;
    if let Some(sig) = st.signal() {
        eprintln!("error: {}", messages::died_of_signal(cmd, sig));
    }
}

#[cfg(not(unix))]
fn report_signal(_cmd: &str, _st: std::process::ExitStatus) {}

enum ReadError {
    Eof,
    Protocol,
}

enum Answer {
    Content(Vec<u8>),
    Error,
    Abort,
    Delayed,
}

fn apply_process(
    cmd: &str,
    dir: Direction,
    path: &[u8],
    data: &[u8],
    meta: &Meta,
    ctx: &Context,
    can_delay: bool,
) -> Outcome {
    let mut procs = PROCESSES.lock().unwrap_or_else(|e| e.into_inner());
    let idx = match procs.iter().position(|p| p.cmd == cmd) {
        Some(i) => i,
        None => match Process::start(cmd, ctx) {
            Some(p) => {
                procs.push(p);
                procs.len() - 1
            }
            None => return Outcome::Failed,
        },
    };
    if !procs[idx].supports(dir) {
        return Outcome::NotRun;
    }
    let _sigpipe = IgnoreSigpipe::new();
    match procs[idx].filter(dir, path, data, meta, can_delay) {
        Ok(Answer::Content(c)) => Outcome::Filtered(c),
        Ok(Answer::Delayed) => Outcome::Delayed,
        Ok(Answer::Error) => Outcome::Failed,
        Ok(Answer::Abort) => {
            procs[idx].drop_capability(dir);
            Outcome::Failed
        }
        Err(Broken) => {
            let p = procs.remove(idx);
            eprintln!("error: {}", messages::filter_failed(cmd));
            p.kill();
            Outcome::Failed
        }
    }
}

/// Asks the `process` filter `cmd` which delayed blobs are ready
/// (`command=list_available_blobs`). An empty list means that no more will
/// come. `Err` when the filter failed (reported, the process is stopped).
#[allow(clippy::result_unit_err)]
pub fn list_available(cmd: &str) -> Result<Vec<Vec<u8>>, ()> {
    let mut procs = PROCESSES.lock().unwrap_or_else(|e| e.into_inner());
    let Some(idx) = procs.iter().position(|p| p.cmd == cmd) else {
        return Err(());
    };
    let _sigpipe = IgnoreSigpipe::new();
    match procs[idx].list_available() {
        Ok(l) => Ok(l),
        Err(Broken) => {
            let p = procs.remove(idx);
            eprintln!("error: {}", messages::filter_failed(cmd));
            p.kill();
            Err(())
        }
    }
}

/// The drivers configured for `name`, from a lookup of `filter.<name>.<key>`
/// values (`None`: unset; `Some(None)`: set without a value).
pub fn driver(
    name: &str,
    get: impl Fn(&str) -> Option<Option<Vec<u8>>>,
    required: Option<bool>,
) -> Option<Driver> {
    let cmd = |key: &str| match get(key) {
        Some(Some(v)) if !v.is_empty() => Some(String::from_utf8_lossy(&v).into_owned()),
        _ => None,
    };
    let d = Driver {
        name: name.to_string(),
        clean: cmd("clean"),
        smudge: cmd("smudge"),
        process: cmd("process"),
        required: required.unwrap_or(false),
    };
    if d.clean.is_none() && d.smudge.is_none() && d.process.is_none() && !d.required {
        return None;
    }
    Some(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_path() {
        assert_eq!(
            expand("sh ./argc.sh %f --x", b"a 'b'"),
            OsString::from("sh ./argc.sh 'a '\\''b'\\''' --x")
        );
        assert_eq!(expand("cat", b"p"), OsString::from("cat"));
    }

    #[cfg(unix)]
    #[test]
    fn single_filters() {
        let ctx = Context::default();
        let d = Driver {
            name: "t".into(),
            clean: Some("tr a-z A-Z".into()),
            smudge: Some("false".into()),
            ..Driver::default()
        };
        let m = Meta::default();
        assert_eq!(
            apply(&d, Direction::Clean, b"f", b"abc\n", &m, &ctx, false),
            Outcome::Filtered(b"ABC\n".to_vec())
        );
        assert_eq!(
            apply(&d, Direction::Smudge, b"f", b"abc\n", &m, &ctx, false),
            Outcome::Failed
        );
        let none = Driver {
            name: "n".into(),
            required: true,
            ..Driver::default()
        };
        assert_eq!(
            apply(&none, Direction::Clean, b"f", b"x", &m, &ctx, false),
            Outcome::NotRun
        );
    }

    #[cfg(unix)]
    #[test]
    fn filter_that_does_not_read() {
        let d = Driver {
            name: "t".into(),
            clean: Some("echo xyzzy".into()),
            ..Driver::default()
        };
        let big = vec![b'a'; 1 << 20];
        assert_eq!(
            apply(
                &d,
                Direction::Clean,
                b"f",
                &big,
                &Meta::default(),
                &Context::default(),
                false
            ),
            Outcome::Filtered(b"xyzzy\n".to_vec())
        );
    }
}
