// SPDX-License-Identifier: MIT

//! The file system monitor client (git-config(1) `core.fsmonitor`,
//! githooks(5) `fsmonitor-watchman`, gitformat-index(5) "File System
//! Monitor cache"): after the index is read, the monitor is asked what
//! changed since the token stored in the `FSMN` extension, and only the
//! entries (and untracked cache directories) it names are checked again.
//!
//! The monitor is a hook (protocol version 2: `<hook> 2 <token>` prints
//! the new token, a NUL and NUL-terminated paths; version 1:
//! `<hook> 1 <nanoseconds>` prints the paths) or, with `core.fsmonitor`
//! true, the built-in `fsmonitor--daemon`, asked over its socket.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use rgit_index::Index;

use crate::messages::fsmonitor as msg;
use crate::repository::Repo;

/// What `core.fsmonitor` asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Setting {
    /// No file system monitor.
    Off,
    /// A hook command (shell syntax allowed).
    Hook(String),
    /// The built-in daemon.
    Builtin,
}

/// `core.fsmonitor` as configured, without checking the repository.
pub fn configured(config: &rgit_config::ConfigSet) -> Setting {
    let Some(entry) = config.get("core.fsmonitor") else {
        return Setting::Off;
    };
    let Ok(value) = entry.string() else {
        // `core.fsmonitor` without a value: true.
        return Setting::Builtin;
    };
    let bytes: &[u8] = value.as_ref();
    match rgit_config::value::parse_bool(Some(bytes)) {
        Some(true) => Setting::Builtin,
        Some(false) => Setting::Off,
        None => Setting::Hook(String::from_utf8_lossy(bytes).into_owned()),
    }
}

/// The monitor in use for `repo`: none in a bare repository.
pub fn setting(repo: &Repo) -> Setting {
    if repo.setup.work_tree.is_none() {
        return Setting::Off;
    }
    configured(&repo.config)
}

/// Whether a monitor is in use, so that entries it vouches for need not
/// be checked.
pub fn enabled(repo: &Repo) -> bool {
    setting(repo) != Setting::Off
}

/// git's message for a repository the monitor cannot work with.
pub fn incompatible(repo: &Repo) -> Option<String> {
    if repo.setup.work_tree.is_none() {
        let dir = repo.setup.cwd.join(&repo.setup.git_dir);
        let dir = normalize(&dir);
        return Some(msg::bare_incompatible(&dir.display().to_string()));
    }
    None
}

/// `path` without `.` components and a trailing slash.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

fn now_ns() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        .to_string()
}

/// The answer of a monitor: the new token and the changed paths, or on
/// failure the token to store instead.
type Answer = Result<(Vec<u8>, Vec<Vec<u8>>), Vec<u8>>;

/// Applies the monitor to a freshly read `index` (git's tweak and refresh
/// of the fsmonitor data): drops the extension when no monitor is used,
/// adds it when one is, and invalidates what the monitor reports.
pub fn after_read(repo: &Repo, index: &mut Index) {
    let setting = setting(repo);
    if setting == Setting::Off {
        if index.fsmonitor_token().is_some() {
            index.set_fsmonitor_token(None);
            repo.fsmonitor_changed.set(true);
        }
        return;
    }
    let previous = index.fsmonitor_token().map(<[u8]>::to_vec);
    let since = previous.clone().unwrap_or_else(|| now_ns().into_bytes());
    let answer = match &setting {
        Setting::Hook(cmd) => query_hook(repo, cmd, &since),
        Setting::Builtin => query_daemon(repo, &since),
        Setting::Off => unreachable!(),
    };
    let token = match answer {
        Ok((token, paths)) => {
            let trivial = paths.iter().any(|p| p == b"/");
            if trivial || previous.is_none() {
                invalidate_all(index);
            } else {
                for p in &paths {
                    invalidate(index, p);
                }
                index.set_fsmonitor_has_run(true);
            }
            token
        }
        Err(token) => {
            invalidate_all(index);
            token
        }
    };
    index.set_fsmonitor_token(Some(token));
    repo.fsmonitor_changed.set(true);
}

/// Everything is dirty: the entries, and the untracked cache, whose
/// records may predate the new token.
fn invalidate_all(index: &mut Index) {
    index.invalidate_fsmonitor();
    if let Some(root) = index.untracked_cache_mut().and_then(|uc| uc.root.as_mut()) {
        root.invalidate_all();
    }
}

/// Adds the extension (`update-index --fsmonitor` without a configured
/// monitor): every entry is dirty.
pub fn add(index: &mut Index) {
    if index.fsmonitor_token().is_none() {
        invalidate_all(index);
        index.set_fsmonitor_token(Some(now_ns().into_bytes()));
    }
}

/// Invalidates the entries at or below `path` (a trailing slash names a
/// directory) and the untracked cache directories leading to it.
fn invalidate(index: &mut Index, path: &[u8]) {
    let path = path.strip_suffix(b"/").unwrap_or(path);
    if path.is_empty() || path == b".git" || path.starts_with(b".git/") {
        return;
    }
    // A path inside a sparse directory entry needs the full index.
    crate::sparse_index::expand_for_path(path);
    let exact = index.path_range(path);
    let below = index.dir_range(path);
    for pos in exact.chain(below) {
        if let Some(e) = index.entry_mut(pos) {
            e.set_fsmonitor_valid(false);
        }
    }
    if let Some(uc) = index.untracked_cache_mut() {
        let mut p = path.to_vec();
        p.extend_from_slice(b"/");
        uc.invalidate_path(&p);
    }
}

/// Splits NUL-terminated paths.
fn split_paths(data: &[u8]) -> Vec<Vec<u8>> {
    data.split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .map(<[u8]>::to_vec)
        .collect()
}

fn query_hook(repo: &Repo, cmd: &str, since: &[u8]) -> Answer {
    let now = now_ns();
    let versions: &[u32] = match repo.config.int("core.fsmonitorhookversion") {
        Ok(Some(1)) => &[1],
        Ok(Some(2)) => &[2],
        Ok(Some(v)) => {
            eprintln!("warning: {}", msg::invalid_hook_version(v));
            &[2, 1]
        }
        _ => &[2, 1],
    };
    let _region = rgit_trace::trace2::region("fsm_hook", "query", true);
    let root = repo
        .setup
        .cwd
        .join(repo.setup.work_tree.as_deref().unwrap_or(Path::new(".")));
    for &v in versions {
        let version = v.to_string();
        let token_arg = if v == 1 {
            std::ffi::OsString::from(&now)
        } else {
            std::ffi::OsString::from(String::from_utf8_lossy(since).into_owned())
        };
        let args = [std::ffi::OsStr::new(&version), token_arg.as_os_str()];
        let mut c = rgit_ident::editor::shell_command(cmd, &args);
        c.current_dir(&root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
        let out = match c.output() {
            Ok(o) => o,
            Err(e) => {
                eprintln!(
                    "fatal: {}",
                    msg::cannot_exec(cmd, &crate::global::strerror(&e))
                );
                continue;
            }
        };
        if !out.status.success() {
            continue;
        }
        if v == 1 {
            return Ok((now.into_bytes(), split_paths(&out.stdout)));
        }
        let nul = out.stdout.iter().position(|&b| b == 0);
        let token = nul.map_or(&out.stdout[..], |n| &out.stdout[..n]);
        if token.is_empty() {
            eprintln!("warning: {}", msg::EMPTY_TOKEN);
            return Err(Vec::new());
        }
        let rest = nul.map_or(&[][..], |n| &out.stdout[n + 1..]);
        return Ok((token.to_vec(), split_paths(rest)));
    }
    Err(now.into_bytes())
}

/// The socket of the built-in daemon of the repository at `git_dir`.
pub fn socket_path(git_dir: &Path) -> PathBuf {
    git_dir.join("fsmonitor--daemon.ipc")
}

/// One request to the daemon at `socket` (a pkt-line and a flush); the
/// concatenated payload of its answer.
#[cfg(unix)]
pub fn ipc_request(socket: &Path, request: &[u8]) -> std::io::Result<Vec<u8>> {
    use std::os::unix::net::UnixStream;
    let mut s = crate::commands::credential_cache::imp::with_short_path(socket, |p| {
        UnixStream::connect(p)
    })?;
    let mut out = Vec::with_capacity(request.len() + 8);
    if !request.is_empty() {
        out.extend_from_slice(format!("{:04x}", request.len() + 4).as_bytes());
        out.extend_from_slice(request);
    }
    out.extend_from_slice(b"0000");
    s.write_all(&out)?;
    let mut data = Vec::new();
    s.read_to_end(&mut data)?;
    let mut payload = Vec::new();
    let mut pos = 0;
    while pos + 4 <= data.len() {
        let len = std::str::from_utf8(&data[pos..pos + 4])
            .ok()
            .and_then(|h| usize::from_str_radix(h, 16).ok())
            .ok_or_else(|| std::io::Error::other("bad pkt-line"))?;
        if len == 0 {
            return Ok(payload);
        }
        if len < 4 || pos + len > data.len() {
            break;
        }
        payload.extend_from_slice(&data[pos + 4..pos + len]);
        pos += len;
    }
    Err(std::io::Error::other("truncated answer"))
}

#[cfg(not(unix))]
pub fn ipc_request(_socket: &Path, _request: &[u8]) -> std::io::Result<Vec<u8>> {
    Err(std::io::Error::other("unsupported"))
}

/// Starts the daemon in the background (as git does when a command finds
/// none running) and waits until it answers.
fn start_daemon(repo: &Repo) -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let mut c = std::process::Command::new(exe);
    c.args(["fsmonitor--daemon", "start"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(wt) = &repo.setup.work_tree {
        c.current_dir(repo.setup.cwd.join(wt));
    }
    let info = rgit_trace::trace2::ChildInfo::default();
    rgit_trace::trace2::status(&mut c, info, &["git", "fsmonitor--daemon", "start"])
        .is_ok_and(|s| s.success())
}

fn query_daemon(repo: &Repo, since: &[u8]) -> Answer {
    let now = now_ns().into_bytes();
    let socket = socket_path(&repo.setup.cwd.join(&repo.setup.git_dir));
    let answer = match ipc_request(&socket, since) {
        Ok(a) => a,
        Err(_) => {
            if !start_daemon(repo) {
                return Err(now);
            }
            match ipc_request(&socket, since) {
                Ok(a) => a,
                Err(_) => return Err(now),
            }
        }
    };
    rgit_trace::trace2::data(
        "fsm_client",
        "query/response-length",
        &answer.len().to_string(),
        true,
    );
    let Some(nul) = answer.iter().position(|&b| b == 0) else {
        return Err(now);
    };
    Ok((answer[..nul].to_vec(), split_paths(&answer[nul + 1..])))
}
