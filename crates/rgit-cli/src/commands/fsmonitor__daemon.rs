// SPDX-License-Identifier: MIT
//! `git fsmonitor--daemon` (Documentation/git-fsmonitor--daemon.adoc): the
//! built-in file system monitor. On Linux it watches the work tree with
//! inotify and answers queries (Documentation/technical/api-simple-ipc.adoc)
//! on `<git-dir>/fsmonitor--daemon.ipc`; elsewhere `start` and `run` say
//! it is not supported and no daemon is ever running.

use crate::messages::helpers as msg;
use crate::prelude::*;
use crate::repo::{self, Mode};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let detach = Cell::new(0);
    let ipc_threads = Cell::new(0);
    let start_timeout = Cell::new(-1);
    let mut parser = Parser::new(
        msg::FSMONITOR_USAGE,
        vec![
            Opt::bool(None, "detach", &detach, msg::FSMONITOR_H_DETACH),
            Opt::integer(None, "ipc-threads", &ipc_threads, msg::FSMONITOR_H_IPC_THREADS),
            Opt::integer(
                None,
                "start-timeout",
                &start_timeout,
                msg::FSMONITOR_H_START_TIMEOUT,
            ),
        ],
    );
    let args = parser.parse(argv)?.args;
    let Some(sub) = args.first() else {
        return Err(parser.usage_error().into());
    };
    drop(parser);
    let setup = repo::setup(ctx, Mode::Required)?;
    let work_tree = setup.work_tree.as_ref().map(|w| {
        crate::fsmonitor::normalize(&setup.cwd.join(w))
            .display()
            .to_string()
    });
    let git_dir = crate::fsmonitor::normalize(&setup.cwd.join(&setup.git_dir));
    let shown = work_tree
        .clone()
        .unwrap_or_else(|| git_dir.display().to_string());
    let bare = || {
        Die::Fatal(crate::messages::fsmonitor::bare_incompatible(
            &git_dir.display().to_string(),
        ))
    };
    let socket = crate::fsmonitor::socket_path(&git_dir);
    match sub.as_str() {
        "start" | "run" => {
            let Some(wt) = work_tree else {
                return Err(bare());
            };
            if imp::is_running(&socket) {
                return Err(Die::Fatal(msg::fsmonitor_already_running(&wt)));
            }
            if sub == "start" {
                let timeout = match start_timeout.get() {
                    t if t >= 0 => t as u64,
                    _ => 60,
                };
                imp::start(&wt, &socket, i64::from(ipc_threads.get()), timeout)
            } else {
                imp::serve(&wt, &git_dir, &socket, detach.get() != 0)
            }
        }
        "stop" => {
            if !imp::is_running(&socket) {
                return Err(Die::Fatal(msg::FSMONITOR_NOT_RUNNING.to_owned()));
            }
            imp::stop(&socket);
            Ok(0)
        }
        "status" => {
            if imp::is_running(&socket) {
                println!("{}", msg::fsmonitor_watching(&shown));
                Ok(0)
            } else {
                println!("{}", msg::fsmonitor_not_watching(&shown));
                Ok(1)
            }
        }
        other => Err(Die::Fatal(msg::fsmonitor_unhandled_subcommand(other))),
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::{HashMap, HashSet};
    use std::ffi::CString;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    use crate::commands::credential_cache::imp::with_short_path;
    use crate::global::strerror;
    use crate::messages::helpers as msg;
    use crate::prelude::*;

    /// How long a query waits for its cookie file to show up.
    const COOKIE_WAIT: Duration = Duration::from_secs(1);
    /// How often the loops look at the shutdown flag.
    const TICK: i32 = 100;
    /// Largest payload of one pkt-line.
    const MAX_PKT: usize = 65516;

    pub fn is_running(socket: &Path) -> bool {
        with_short_path(socket, |p| UnixStream::connect(p)).is_ok()
    }

    /// Starts `run --detach` in the background and waits until it listens.
    pub fn start(work_tree: &str, socket: &Path, threads: i64, timeout: u64) -> CmdResult {
        let exe = std::env::current_exe().map_err(|e| Die::Fatal(strerror(&e)))?;
        let mut c = Command::new(exe);
        c.args(["fsmonitor--daemon", "run", "--detach"]);
        if threads > 0 {
            c.arg(format!("--ipc-threads={threads}"));
        }
        c.current_dir(work_tree)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let mut child = c
            .spawn()
            .map_err(|e| Die::Fatal(msg::fsmonitor_could_not_spawn(&strerror(&e))))?;
        let deadline = Instant::now() + Duration::from_secs(timeout);
        loop {
            if is_running(socket) {
                return Ok(0);
            }
            if let Ok(Some(_)) = child.try_wait() {
                return Err(Die::Fatal(msg::FSMONITOR_FAILED_TO_START.to_owned()));
            }
            if Instant::now() >= deadline {
                return Err(Die::Fatal(msg::FSMONITOR_NOT_ONLINE.to_owned()));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Asks the daemon to quit and waits until it is gone.
    pub fn stop(socket: &Path) {
        let _ = crate::fsmonitor::ipc_request(socket, b"quit");
        let deadline = Instant::now() + Duration::from_secs(30);
        while is_running(socket) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The token id: `<flushes>.<pid>.<UTC time>` (or
    /// `test_<n>` with `GIT_TEST_FSMONITOR_TOKEN`).
    fn token_id(flushes: u64) -> String {
        if std::env::var_os("GIT_TEST_FSMONITOR_TOKEN").is_some() {
            return format!("test_{:08x}", flushes + 1);
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let secs = i64::try_from(now.as_secs()).unwrap_or(0);
        let (y, mo, d, h, mi, s) = civil(secs);
        format!(
            "{flushes}.{}.{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}.{:06}Z",
            std::process::id(),
            now.subsec_micros()
        )
    }

    /// UTC date and time of a Unix timestamp.
    fn civil(t: i64) -> (i64, u32, u32, u32, u32, u32) {
        let days = t.div_euclid(86400);
        let rem = t.rem_euclid(86400);
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        let y = yoe + era * 400 + i64::from(m <= 2);
        (
            y,
            m,
            d,
            (rem / 3600) as u32,
            (rem / 60 % 60) as u32,
            (rem % 60) as u32,
        )
    }

    #[derive(Default)]
    struct State {
        flushes: u64,
        token: String,
        seq: u64,
        /// Paths changed, by the sequence number they were published with.
        batches: Vec<(u64, Vec<Vec<u8>>)>,
        pending: Vec<Vec<u8>>,
        cookies: HashSet<Vec<u8>>,
        shutdown: bool,
    }

    impl State {
        fn flush(&mut self) {
            self.flushes += 1;
            self.token = token_id(self.flushes);
            self.seq = 0;
            self.batches.clear();
            self.pending.clear();
        }

        /// The answer to a query for `since`: the current token and what
        /// changed, or `/` (everything) for a token of another epoch.
        fn answer(&mut self, since: &[u8]) -> Vec<u8> {
            if !self.pending.is_empty() {
                self.seq += 1;
                let batch = std::mem::take(&mut self.pending);
                self.batches.push((self.seq, batch));
            }
            let mut out = format!("builtin:{}:{}", self.token, self.seq).into_bytes();
            out.push(0);
            let parsed = since
                .strip_prefix(b"builtin:")
                .and_then(|r| {
                    let colon = r.iter().rposition(|&b| b == b':')?;
                    let seq = std::str::from_utf8(&r[colon + 1..]).ok()?.parse::<u64>().ok()?;
                    Some((&r[..colon], seq))
                })
                .filter(|(id, seq)| *id == self.token.as_bytes() && *seq <= self.seq);
            match parsed {
                Some((_, seq)) => {
                    let mut seen = HashSet::new();
                    for (_, batch) in self.batches.iter().filter(|(s, _)| *s > seq) {
                        for p in batch {
                            if seen.insert(p.as_slice()) {
                                out.extend_from_slice(p);
                                out.push(0);
                            }
                        }
                    }
                }
                None => out.extend_from_slice(b"/\0"),
            }
            out
        }
    }

    type Shared = Arc<(Mutex<State>, Condvar)>;

    fn trace_event(path: &[u8]) {
        rgit_trace::trace!(
            rgit_trace::TRACE_FSMONITOR,
            "event: {}",
            String::from_utf8_lossy(path)
        );
    }

    /// `run`: watch the work tree and serve queries until told to quit or
    /// until the repository goes away.
    pub fn serve(work_tree: &str, git_dir: &Path, socket: &Path, detach: bool) -> CmdResult {
        if detach {
            // SAFETY: setsid has no memory-safety preconditions; it fails
            // harmlessly when the process already leads a group.
            unsafe {
                libc::setsid();
            }
        }
        // SAFETY: ignoring SIGPIPE so a client that went away cannot kill
        // the daemon.
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        }
        let root = PathBuf::from(work_tree);
        let cookie_dir = git_dir.join("fsmonitor--daemon").join("cookies");
        let _ = std::fs::create_dir_all(&cookie_dir);
        let mut watcher = Watcher::new(&root, git_dir, &cookie_dir)
            .map_err(|e| Die::Fatal(msg::fsmonitor_could_not_watch(work_tree, &strerror(&e))))?;
        rgit_trace::trace!(
            rgit_trace::TRACE_FSMONITOR,
            "Watching: worktree '{}'",
            work_tree
        );
        let _ = std::fs::remove_file(socket);
        let listener = with_short_path(socket, |p| UnixListener::bind(p))
            .map_err(|e| Die::Fatal(msg::fsmonitor_could_not_listen(&socket.display().to_string(), &strerror(&e))))?;
        let shared: Shared = Arc::new((Mutex::new(State::default()), Condvar::new()));
        {
            let mut st = shared.0.lock().expect("state");
            st.flushes = 0;
            st.token = token_id(0);
        }
        let watch_state = shared.clone();
        let watch_thread = std::thread::spawn(move || watcher.run(&watch_state));
        let cookies = Cookies {
            dir: cookie_dir,
            next: 0,
        };
        serve_loop(&listener, &shared, cookies);
        {
            let mut st = shared.0.lock().expect("state");
            st.shutdown = true;
        }
        let _ = watch_thread.join();
        let _ = std::fs::remove_file(socket);
        Ok(0)
    }

    struct Cookies {
        dir: PathBuf,
        next: u64,
    }

    impl Cookies {
        /// Writes a cookie file and waits until the watcher saw it, so
        /// that every change made before the query has been recorded.
        fn sync(&mut self, shared: &Shared) {
            let name = format!("{}-{}", std::process::id(), self.next);
            self.next += 1;
            let path = self.dir.join(&name);
            if std::fs::File::create(&path).is_err() {
                return;
            }
            let (lock, cvar) = &**shared;
            let deadline = Instant::now() + COOKIE_WAIT;
            let mut st = lock.lock().expect("state");
            while !st.cookies.contains(name.as_bytes()) && !st.shutdown {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    rgit_trace::trace!(
                        rgit_trace::TRACE_FSMONITOR,
                        "cookie_wait timed out: '{}'",
                        name
                    );
                    break;
                }
                st = cvar.wait_timeout(st, left).expect("state").0;
            }
            st.cookies.remove(name.as_bytes());
            drop(st);
            let _ = std::fs::remove_file(&path);
        }
    }

    fn serve_loop(listener: &UnixListener, shared: &Shared, mut cookies: Cookies) {
        loop {
            if shared.0.lock().expect("state").shutdown {
                return;
            }
            let mut fds = libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd for the duration of the call.
            let n = unsafe { libc::poll(&mut fds, 1, TICK) };
            if n <= 0 {
                continue;
            }
            let Ok((stream, _)) = listener.accept() else {
                continue;
            };
            if handle(stream, shared, &mut cookies) {
                return;
            }
        }
    }

    /// Reads pkt-lines up to a flush; the concatenated payload.
    fn read_request(s: &mut UnixStream) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        loop {
            let mut len = [0u8; 4];
            s.read_exact(&mut len).ok()?;
            let n = usize::from_str_radix(std::str::from_utf8(&len).ok()?, 16).ok()?;
            if n == 0 {
                return Some(out);
            }
            if n < 4 {
                return None;
            }
            let mut buf = vec![0; n - 4];
            s.read_exact(&mut buf).ok()?;
            out.extend_from_slice(&buf);
        }
    }

    /// Answers one client; true when it asked the daemon to quit.
    fn handle(mut s: UnixStream, shared: &Shared, cookies: &mut Cookies) -> bool {
        let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
        let Some(req) = read_request(&mut s) else {
            return false;
        };
        rgit_trace::trace!(
            rgit_trace::TRACE_FSMONITOR,
            "requested token: {}",
            String::from_utf8_lossy(&req)
        );
        let answer = match req.as_slice() {
            b"quit" => {
                let _ = s.write_all(b"0000");
                shared.0.lock().expect("state").shutdown = true;
                return true;
            }
            b"flush" => {
                let mut st = shared.0.lock().expect("state");
                st.flush();
                st.answer(b"")
            }
            since => {
                cookies.sync(shared);
                shared.0.lock().expect("state").answer(since)
            }
        };
        let mut out = Vec::with_capacity(answer.len() + 16);
        for chunk in answer.chunks(MAX_PKT) {
            out.extend_from_slice(format!("{:04x}", chunk.len() + 4).as_bytes());
            out.extend_from_slice(chunk);
        }
        out.extend_from_slice(b"0000");
        let _ = s.write_all(&out);
        false
    }

    const DIR_MASK: u32 = libc::IN_ATTRIB
        | libc::IN_CLOSE_WRITE
        | libc::IN_CREATE
        | libc::IN_DELETE
        | libc::IN_DELETE_SELF
        | libc::IN_MODIFY
        | libc::IN_MOVED_FROM
        | libc::IN_MOVED_TO
        | libc::IN_MOVE_SELF
        | libc::IN_EXCL_UNLINK
        | libc::IN_ONLYDIR;

    /// The inotify watches: one per directory of the work tree (but not
    /// `.git`), the cookie directory and an external git directory.
    struct Watcher {
        fd: OwnedFd,
        root: PathBuf,
        /// Watch descriptor -> directory relative to the top ("" for it).
        dirs: HashMap<i32, Vec<u8>>,
        cookie_wd: i32,
        git_dir_wd: Option<i32>,
    }

    impl Watcher {
        fn new(root: &Path, git_dir: &Path, cookie_dir: &Path) -> std::io::Result<Watcher> {
            // SAFETY: inotify_init1 returns a new descriptor or -1.
            let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: fd is a fresh descriptor that nothing else owns.
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            let mut w = Watcher {
                fd,
                root: root.to_path_buf(),
                dirs: HashMap::new(),
                cookie_wd: -1,
                git_dir_wd: None,
            };
            w.cookie_wd = w.add(cookie_dir, libc::IN_CREATE | libc::IN_CLOSE_WRITE | libc::IN_ONLYDIR)?;
            if !git_dir.starts_with(root) {
                w.git_dir_wd = Some(w.add(git_dir, libc::IN_DELETE_SELF | libc::IN_MOVE_SELF)?);
            }
            let mut ignored = Vec::new();
            w.watch_tree(b"", &mut ignored)?;
            Ok(w)
        }

        fn add(&self, path: &Path, mask: u32) -> std::io::Result<i32> {
            let c = CString::new(path.as_os_str().as_bytes())
                .map_err(|_| std::io::Error::other("NUL in path"))?;
            // SAFETY: valid descriptor and NUL-terminated path.
            let wd = unsafe { libc::inotify_add_watch(self.fd.as_raw_fd(), c.as_ptr(), mask) };
            if wd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(wd)
        }

        fn full(&self, rel: &[u8]) -> PathBuf {
            if rel.is_empty() {
                self.root.clone()
            } else {
                self.root.join(std::ffi::OsStr::from_bytes(rel))
            }
        }

        /// Watches `rel` and every directory below it; what it finds below
        /// goes to `found` (paths made before the watch existed).
        fn watch_tree(&mut self, rel: &[u8], found: &mut Vec<Vec<u8>>) -> std::io::Result<()> {
            let wd = self.add(&self.full(rel), DIR_MASK)?;
            self.dirs.insert(wd, rel.to_vec());
            let Ok(rd) = std::fs::read_dir(self.full(rel)) else {
                return Ok(());
            };
            for e in rd.flatten() {
                let name = e.file_name();
                let mut child = rel.to_vec();
                if !child.is_empty() {
                    child.push(b'/');
                }
                child.extend_from_slice(name.as_bytes());
                if rel.is_empty() && name.as_bytes() == b".git" {
                    continue;
                }
                found.push(child.clone());
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    // A directory that vanished meanwhile is no error.
                    let _ = self.watch_tree(&child, found);
                }
            }
            Ok(())
        }

        /// Forgets the watches of `rel` and below (it was moved away).
        fn unwatch_tree(&mut self, rel: &[u8]) {
            let gone: Vec<i32> = self
                .dirs
                .iter()
                .filter(|(_, d)| {
                    d.as_slice() == rel
                        || (d.starts_with(rel) && d.get(rel.len()) == Some(&b'/'))
                })
                .map(|(&wd, _)| wd)
                .collect();
            for wd in gone {
                self.dirs.remove(&wd);
                // SAFETY: removing a watch of our own descriptor.
                unsafe {
                    libc::inotify_rm_watch(self.fd.as_raw_fd(), wd);
                }
            }
        }

        fn run(&mut self, shared: &Shared) {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                if shared.0.lock().expect("state").shutdown {
                    return;
                }
                let mut fds = libc::pollfd {
                    fd: self.fd.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one valid pollfd for the duration of the call.
                let n = unsafe { libc::poll(&mut fds, 1, TICK) };
                if n <= 0 {
                    continue;
                }
                // SAFETY: reading into a buffer we own, of the given size.
                let got = unsafe {
                    libc::read(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len())
                };
                let Ok(got) = usize::try_from(got) else {
                    continue;
                };
                let mut events = Vec::new();
                let mut pos = 0;
                const HEADER: usize = std::mem::size_of::<libc::inotify_event>();
                while pos + HEADER <= got {
                    let h = &buf[pos..pos + HEADER];
                    let wd = i32::from_ne_bytes(h[0..4].try_into().expect("4"));
                    let mask = u32::from_ne_bytes(h[4..8].try_into().expect("4"));
                    let len = u32::from_ne_bytes(h[12..16].try_into().expect("4")) as usize;
                    let name = &buf[pos + HEADER..(pos + HEADER + len).min(got)];
                    let name = name.split(|&b| b == 0).next().unwrap_or_default().to_vec();
                    events.push((wd, mask, name));
                    pos += HEADER + len;
                }
                let mut changed = Vec::new();
                let mut cookies = Vec::new();
                let mut shutdown = false;
                let mut overflow = false;
                for (wd, mask, name) in events {
                    if mask & libc::IN_Q_OVERFLOW != 0 {
                        overflow = true;
                        continue;
                    }
                    if wd == self.cookie_wd {
                        if !name.is_empty() {
                            cookies.push(name);
                        }
                        continue;
                    }
                    if Some(wd) == self.git_dir_wd {
                        if mask & (libc::IN_DELETE_SELF | libc::IN_MOVE_SELF) != 0 {
                            shutdown = true;
                        }
                        continue;
                    }
                    let Some(dir) = self.dirs.get(&wd).cloned() else {
                        continue;
                    };
                    if mask & libc::IN_IGNORED != 0 {
                        self.dirs.remove(&wd);
                        continue;
                    }
                    if name.is_empty() {
                        if dir.is_empty() && mask & (libc::IN_DELETE_SELF | libc::IN_MOVE_SELF) != 0 {
                            shutdown = true;
                        }
                        continue;
                    }
                    if dir.is_empty() && name == b".git" {
                        if mask & (libc::IN_DELETE | libc::IN_MOVED_FROM) != 0 {
                            shutdown = true;
                        }
                        continue;
                    }
                    let mut rel = dir.clone();
                    if !rel.is_empty() {
                        rel.push(b'/');
                    }
                    rel.extend_from_slice(&name);
                    trace_event(&rel);
                    changed.push(rel.clone());
                    if mask & libc::IN_ISDIR != 0 {
                        if mask & (libc::IN_CREATE | libc::IN_MOVED_TO) != 0 {
                            let mut found = Vec::new();
                            let _ = self.watch_tree(&rel, &mut found);
                            for f in found {
                                trace_event(&f);
                                changed.push(f);
                            }
                        } else if mask & libc::IN_MOVED_FROM != 0 {
                            self.unwatch_tree(&rel);
                        }
                    }
                }
                let (lock, cvar) = &**shared;
                let mut st = lock.lock().expect("state");
                if overflow {
                    st.flush();
                }
                st.pending.extend(changed);
                st.cookies.extend(cookies);
                if shutdown {
                    st.shutdown = true;
                }
                cvar.notify_all();
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use std::path::Path;

    use crate::messages::helpers as msg;
    use crate::prelude::*;

    pub fn is_running(_socket: &Path) -> bool {
        false
    }

    pub fn start(_work_tree: &str, _socket: &Path, _threads: i64, _timeout: u64) -> CmdResult {
        Err(Die::Fatal(msg::FSMONITOR_NOT_SUPPORTED.to_owned()))
    }

    pub fn serve(_work_tree: &str, _git_dir: &Path, _socket: &Path, _detach: bool) -> CmdResult {
        Err(Die::Fatal(msg::FSMONITOR_NOT_SUPPORTED.to_owned()))
    }

    pub fn stop(_socket: &Path) {}
}
