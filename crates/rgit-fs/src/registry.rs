// SPDX-License-Identifier: MIT
//! Process-wide list of files to delete when the process dies.
//!
//! Lock files, PID files and temp files register their paths here while they
//! are "open". The list is removed:
//!
//! * by `Drop` of the owning value (normal path, including unwinding),
//! * at `exit(3)` (covers `std::process::exit`) through an `atexit` hook,
//! * when one of SIGINT, SIGTERM, SIGHUP, SIGQUIT or SIGPIPE arrives.
//!
//! Directories (a clone in progress) can be registered too, with
//! [`remove_dir_on_signal`]: they are removed recursively, or only emptied,
//! when one of those signals arrives, never at exit.
//!
//! The signal handler may only call async-signal-safe functions, so it can
//! neither allocate nor take a lock. Writers therefore publish an immutable
//! snapshot of NUL-terminated paths through an atomic pointer. The handler
//! announces itself in `READERS` before loading the pointer; a writer that
//! replaced the pointer waits until `READERS` is zero before it frees the
//! old snapshot. With sequentially consistent ordering a handler that starts
//! after the writer saw zero readers always loads the new snapshot.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

#[cfg(unix)]
use std::ffi::CString;

struct Entry {
    id: u64,
    path: PathBuf,
    #[cfg(unix)]
    c_path: CString,
    /// `None`: a file. `Some(keep)`: a directory removed on a signal,
    /// itself too unless `keep`.
    dir: Option<bool>,
}

struct Snapshot {
    /// Process that registered the paths. A forked child that has not yet
    /// called `exec` must not delete its parent's files.
    #[cfg(unix)]
    pid: libc::pid_t,
    #[cfg(unix)]
    paths: Vec<CString>,
    // No signal handler removes them on other platforms yet (#183).
    #[cfg(not(unix))]
    #[allow(dead_code)]
    paths: Vec<PathBuf>,
    /// Directories to remove on a signal (`true`: only their contents).
    #[cfg(unix)]
    dirs: Vec<(CString, bool)>,
}

struct Inner {
    next_id: u64,
    entries: Vec<Entry>,
    hooks_installed: bool,
}

static INNER: Mutex<Inner> = Mutex::new(Inner {
    next_id: 1,
    entries: Vec::new(),
    hooks_installed: false,
});
static CURRENT: AtomicPtr<Snapshot> = AtomicPtr::new(std::ptr::null_mut());
static READERS: AtomicUsize = AtomicUsize::new(0);

/// Proof of registration; pass it back to [`unregister`].
#[derive(Debug)]
pub(crate) struct Registration(u64);

/// Adds `path` to the list of files removed on exit or fatal signal.
pub(crate) fn register(path: &Path) -> Registration {
    add(path, None)
}

/// Removes the directory `path` (with everything in it; with `keep`, only
/// what is in it) when the process is killed by SIGINT, SIGTERM, SIGHUP,
/// SIGQUIT or SIGPIPE while the returned guard lives. `path` should be
/// absolute.
pub fn remove_dir_on_signal(path: &Path, keep: bool) -> DirGuard {
    DirGuard(Some(add(path, Some(keep))))
}

/// Keeps a directory registered with [`remove_dir_on_signal`]; dropping it
/// (or calling [`DirGuard::disarm`]) unregisters the directory.
#[derive(Debug)]
pub struct DirGuard(Option<Registration>);

impl DirGuard {
    /// Stop watching the directory.
    pub fn disarm(&mut self) {
        if let Some(r) = self.0.take() {
            unregister(r);
        }
    }
}

impl Drop for DirGuard {
    fn drop(&mut self) {
        self.disarm();
    }
}

fn add(path: &Path, dir: Option<bool>) -> Registration {
    let mut inner = lock_inner();
    if !inner.hooks_installed {
        inner.hooks_installed = true;
        install_hooks();
    }
    let id = inner.next_id;
    inner.next_id += 1;
    inner.entries.push(Entry {
        id,
        path: path.to_owned(),
        #[cfg(unix)]
        c_path: to_cstring(path),
        dir,
    });
    publish(&inner);
    Registration(id)
}

/// Removes a path from the list without touching the file.
pub(crate) fn unregister(reg: Registration) {
    let mut inner = lock_inner();
    inner.entries.retain(|e| e.id != reg.0);
    publish(&inner);
}

/// Deletes every registered file now and empties the list.
///
/// Meant for a command-line front end that terminates the process in a way
/// that skips destructors and `atexit` handlers. Values that still own a
/// removed file notice nothing; their later rollback ignores the missing
/// file.
pub fn remove_registered_files() {
    let mut inner = lock_inner();
    for e in inner.entries.iter().filter(|e| e.dir.is_none()) {
        let _ = std::fs::remove_file(&e.path);
    }
    inner.entries.retain(|e| e.dir.is_some());
    publish(&inner);
}

fn lock_inner() -> std::sync::MutexGuard<'static, Inner> {
    INNER.lock().unwrap_or_else(|p| p.into_inner())
}

fn publish(inner: &Inner) {
    let snapshot = Box::new(Snapshot {
        #[cfg(unix)]
        pid: unsafe { libc::getpid() },
        #[cfg(unix)]
        paths: inner
            .entries
            .iter()
            .filter(|e| e.dir.is_none())
            .map(|e| e.c_path.clone())
            .collect(),
        #[cfg(not(unix))]
        paths: inner
            .entries
            .iter()
            .filter(|e| e.dir.is_none())
            .map(|e| e.path.clone())
            .collect(),
        #[cfg(unix)]
        dirs: inner
            .entries
            .iter()
            .filter_map(|e| e.dir.map(|keep| (e.c_path.clone(), keep)))
            .collect(),
    });
    let old = CURRENT.swap(Box::into_raw(snapshot), Ordering::SeqCst);
    while READERS.load(Ordering::SeqCst) != 0 {
        std::thread::yield_now();
    }
    if !old.is_null() {
        // SAFETY: `old` came from `Box::into_raw` in an earlier `publish`, is
        // no longer reachable through CURRENT and no reader holds it.
        drop(unsafe { Box::from_raw(old) });
    }
}

/// Removal of the registered directories from a signal handler.
#[cfg(unix)]
fn remove_dirs_from_snapshot() {
    READERS.fetch_add(1, Ordering::SeqCst);
    let snap = CURRENT.load(Ordering::SeqCst);
    if !snap.is_null() {
        // SAFETY: the snapshot stays alive while READERS is non-zero.
        let snap = unsafe { &*snap };
        if unsafe { libc::getpid() } == snap.pid {
            for (p, keep) in &snap.dirs {
                // SAFETY: `p` is a NUL-terminated path.
                unsafe { tree::remove(p.as_ptr(), *keep) };
            }
        }
    }
    READERS.fetch_sub(1, Ordering::SeqCst);
}

/// Recursive removal with file descriptors only (no allocation on Linux,
/// where the directory is read with `getdents64`).
#[cfg(unix)]
mod tree {
    use libc::{c_char, c_int};

    const FLAGS: c_int = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

    /// Remove what is in `path`, and `path` itself unless `keep`.
    pub(super) unsafe fn remove(path: *const c_char, keep: bool) {
        unsafe {
            let fd = libc::open(path, FLAGS);
            if fd < 0 {
                return;
            }
            clear(fd, 0);
            libc::close(fd);
            if !keep {
                libc::rmdir(path);
            }
        }
    }

    /// Remove everything in the open directory `fd`.
    unsafe fn clear(fd: c_int, depth: u32) {
        if depth > 256 {
            return;
        }
        // Read again from the start until a pass removes nothing: entries
        // may be skipped while the directory changes.
        for _ in 0..64 {
            let mut removed = false;
            unsafe {
                libc::lseek(fd, 0, libc::SEEK_SET);
                for_each_entry(fd, &mut |name| {
                    if libc::unlinkat(fd, name, 0) == 0 {
                        removed = true;
                        return;
                    }
                    let sub = libc::openat(fd, name, FLAGS);
                    if sub >= 0 {
                        clear(sub, depth + 1);
                        libc::close(sub);
                    }
                    if libc::unlinkat(fd, name, libc::AT_REMOVEDIR) == 0 {
                        removed = true;
                    }
                });
            }
            if !removed {
                break;
            }
        }
    }

    fn is_dot(name: *const c_char) -> bool {
        // SAFETY: `name` is a NUL-terminated entry name.
        unsafe {
            *name == b'.' as c_char
                && (*name.add(1) == 0 || (*name.add(1) == b'.' as c_char && *name.add(2) == 0))
        }
    }

    #[cfg(target_os = "linux")]
    unsafe fn for_each_entry(fd: c_int, f: &mut dyn FnMut(*const c_char)) {
        let mut buf = [0u8; 4096];
        loop {
            let n = unsafe { libc::syscall(libc::SYS_getdents64, fd, buf.as_mut_ptr(), buf.len()) };
            if n <= 0 {
                return;
            }
            let mut off = 0usize;
            while off < n as usize {
                // SAFETY: the kernel filled `buf` with `linux_dirent64`
                // records: u64 inode, i64 offset, u16 record length, u8 type,
                // then the NUL-terminated name.
                let rec = unsafe { buf.as_ptr().add(off) };
                let reclen =
                    unsafe { std::ptr::read_unaligned(rec.add(16) as *const u16) } as usize;
                let name = unsafe { rec.add(19) } as *const c_char;
                if reclen == 0 {
                    return;
                }
                off += reclen;
                if !is_dot(name) {
                    f(name);
                }
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    unsafe fn for_each_entry(fd: c_int, f: &mut dyn FnMut(*const c_char)) {
        unsafe {
            let dup = libc::dup(fd);
            if dup < 0 {
                return;
            }
            let dir = libc::fdopendir(dup);
            if dir.is_null() {
                libc::close(dup);
                return;
            }
            loop {
                let e = libc::readdir(dir);
                if e.is_null() {
                    break;
                }
                let name = (*e).d_name.as_ptr();
                if !is_dot(name) {
                    f(name);
                }
            }
            libc::closedir(dir);
        }
    }
}

/// Async-signal-safe removal of all registered files.
#[cfg_attr(not(unix), allow(dead_code))]
fn remove_from_snapshot() {
    READERS.fetch_add(1, Ordering::SeqCst);
    let snap = CURRENT.load(Ordering::SeqCst);
    if !snap.is_null() {
        // SAFETY: the snapshot stays alive while READERS is non-zero.
        let snap = unsafe { &*snap };
        #[cfg(unix)]
        if unsafe { libc::getpid() } == snap.pid {
            for p in &snap.paths {
                unsafe { libc::unlink(p.as_ptr()) };
            }
        }
        #[cfg(not(unix))]
        for p in &snap.paths {
            let _ = std::fs::remove_file(p);
        }
    }
    READERS.fetch_sub(1, Ordering::SeqCst);
}

#[cfg(unix)]
fn to_cstring(path: &Path) -> CString {
    use std::os::unix::ffi::OsStrExt;
    // Paths with interior NUL bytes cannot be opened either, so the lock
    // could never have been created; register an empty (harmless) path.
    CString::new(path.as_os_str().as_bytes()).unwrap_or_default()
}

#[cfg(unix)]
mod hooks {
    use super::remove_from_snapshot;
    use std::sync::OnceLock;

    /// Signals after which git removes its lock and temp files.
    pub(super) const SIGNALS: [libc::c_int; 5] = [
        libc::SIGINT,
        libc::SIGTERM,
        libc::SIGHUP,
        libc::SIGQUIT,
        libc::SIGPIPE,
    ];

    /// The disposition each signal had before ours was installed.
    struct Previous([Option<libc::sigaction>; 5]);
    // SAFETY: `sigaction` is plain data; it is written once before any
    // handler is installed and only read afterwards.
    unsafe impl Sync for Previous {}
    unsafe impl Send for Previous {}

    static PREVIOUS: OnceLock<Previous> = OnceLock::new();

    extern "C" fn at_exit() {
        remove_from_snapshot();
    }

    extern "C" fn on_signal(sig: libc::c_int, info: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
        remove_from_snapshot();
        super::remove_dirs_from_snapshot();
        let idx = SIGNALS.iter().position(|&s| s == sig);
        let prev = idx.and_then(|i| PREVIOUS.get().and_then(|p| p.0[i]));
        match prev {
            Some(old) if old.sa_sigaction != libc::SIG_DFL && old.sa_sigaction != libc::SIG_IGN => {
                // Chain to the handler that was there before us.
                if old.sa_flags & libc::SA_SIGINFO != 0 {
                    let f: extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void) =
                        unsafe { std::mem::transmute(old.sa_sigaction) };
                    f(sig, info, ctx);
                } else {
                    let f: extern "C" fn(libc::c_int) =
                        unsafe { std::mem::transmute(old.sa_sigaction) };
                    f(sig);
                }
            }
            _ => unsafe {
                // Restore the default action and deliver the signal again so
                // the parent sees the usual "killed by signal" status. The
                // signal is blocked while this handler runs and is delivered
                // on return.
                let mut dfl: libc::sigaction = std::mem::zeroed();
                dfl.sa_sigaction = libc::SIG_DFL;
                libc::sigemptyset(&mut dfl.sa_mask);
                libc::sigaction(sig, &dfl, std::ptr::null_mut());
                libc::raise(sig);
            },
        }
    }

    pub(super) fn install() {
        let mut previous = [None; 5];
        for (i, &sig) in SIGNALS.iter().enumerate() {
            unsafe {
                let mut old: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(sig, std::ptr::null(), &mut old) == 0 {
                    previous[i] = Some(old);
                }
            }
        }
        let previous = PREVIOUS.get_or_init(|| Previous(previous));
        for (i, &sig) in SIGNALS.iter().enumerate() {
            // A signal the process ignores (e.g. SIGPIPE in Rust programs,
            // SIGINT in background jobs) stays ignored: installing a handler
            // would turn it into a fatal one.
            match previous.0[i] {
                Some(old) if old.sa_sigaction != libc::SIG_IGN => {}
                _ => continue,
            }
            unsafe {
                let mut act: libc::sigaction = std::mem::zeroed();
                act.sa_sigaction = on_signal as *const () as usize;
                act.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
                libc::sigemptyset(&mut act.sa_mask);
                libc::sigaction(sig, &act, std::ptr::null_mut());
            }
        }
        unsafe {
            libc::atexit(at_exit);
        }
    }
}

#[cfg(unix)]
fn install_hooks() {
    hooks::install();
}

#[cfg(not(unix))]
fn install_hooks() {}
