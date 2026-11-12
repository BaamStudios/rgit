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
}

struct Snapshot {
    /// Process that registered the paths. A forked child that has not yet
    /// called `exec` must not delete its parent's files.
    #[cfg(unix)]
    pid: libc::pid_t,
    #[cfg(unix)]
    paths: Vec<CString>,
    #[cfg(not(unix))]
    paths: Vec<PathBuf>,
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
    for e in &inner.entries {
        let _ = std::fs::remove_file(&e.path);
    }
    inner.entries.clear();
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
        paths: inner.entries.iter().map(|e| e.c_path.clone()).collect(),
        #[cfg(not(unix))]
        paths: inner.entries.iter().map(|e| e.path.clone()).collect(),
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

/// Async-signal-safe removal of all registered files.
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
