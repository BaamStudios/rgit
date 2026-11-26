// SPDX-License-Identifier: MIT
//! The system calls `rgit daemon` needs that the standard library does
//! not offer: alarms, syslog, user and group lookup, a new session for
//! `--detach`, the canonical name of a host.

use std::ffi::{CStr, CString};
use std::fs::File;
use std::mem::ManuallyDrop;
use std::os::fd::FromRawFd;
use std::os::unix::process::CommandExt;
use std::process::Command;

/// End this process with `SIGALRM` after `seconds` (0: never).
pub fn alarm(seconds: u32) {
    // SAFETY: alarm(2) only replaces the pending alarm of this process;
    // the default action of SIGALRM ends it, which is what a time-out is.
    unsafe {
        libc::alarm(seconds);
    }
}

/// Ask `pid` to end.
pub fn terminate(pid: u32) {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return;
    };
    // SAFETY: kill(2) with a pid of a child we started; a pid that is gone
    // is an error we do not care about.
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
}

/// Standard input without the standard library's buffer in between: what
/// is not read here stays for the service that is started afterwards.
pub fn raw_stdin() -> ManuallyDrop<File> {
    // SAFETY: descriptor 0 is open for the lifetime of the process;
    // `ManuallyDrop` keeps the `File` from closing it.
    ManuallyDrop::new(unsafe { File::from_raw_fd(0) })
}

/// Priorities for [`syslog`].
#[derive(Debug, Clone, Copy)]
pub enum Priority {
    Info,
    Error,
}

/// Send `message` to the system log, as `git-daemon`.
pub fn syslog(priority: Priority, message: &str) {
    static OPENED: std::sync::Once = std::sync::Once::new();
    OPENED.call_once(|| {
        // SAFETY: the identifier is a static C string, as openlog(3)
        // requires (it keeps the pointer).
        unsafe {
            libc::openlog(c"git-daemon".as_ptr(), libc::LOG_PID, libc::LOG_DAEMON);
        }
    });
    let Ok(text) = CString::new(message.replace('\0', " ")) else {
        return;
    };
    let priority = match priority {
        Priority::Info => libc::LOG_INFO,
        Priority::Error => libc::LOG_ERR,
    };
    // SAFETY: a constant format with one string argument that lives until
    // the call returns.
    unsafe {
        libc::syslog(priority, c"%s".as_ptr(), text.as_ptr());
    }
}

/// The user and group `--user`/`--group` name.
#[derive(Debug, Clone)]
pub struct Credentials {
    user: CString,
    uid: libc::uid_t,
    gid: libc::gid_t,
}

/// What [`credentials`] could not find.
pub enum LookupError {
    User,
    Group,
}

/// Look up `--user=<user>` and `--group=<group>`; without a group the
/// user's primary group counts.
pub fn credentials(user: &str, group: Option<&str>) -> Result<Credentials, LookupError> {
    let user = CString::new(user).map_err(|_| LookupError::User)?;
    // SAFETY: getpwnam(3) with a valid C string; the result is read at
    // once, before anything else could overwrite the static buffer (the
    // daemon has no other threads yet).
    let (uid, mut gid) = unsafe {
        let pw = libc::getpwnam(user.as_ptr());
        if pw.is_null() {
            return Err(LookupError::User);
        }
        ((*pw).pw_uid, (*pw).pw_gid)
    };
    if let Some(group) = group {
        let group = CString::new(group).map_err(|_| LookupError::Group)?;
        // SAFETY: as above, for getgrnam(3).
        gid = unsafe {
            let gr = libc::getgrnam(group.as_ptr());
            if gr.is_null() {
                return Err(LookupError::Group);
            }
            (*gr).gr_gid
        };
    }
    Ok(Credentials { user, uid, gid })
}

/// Become the user and group of `credentials`. `false` when the system
/// does not allow it.
pub fn drop_privileges(credentials: &Credentials) -> bool {
    // The base group of initgroups is an int on macOS and the BSDs.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let base = credentials.gid;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let base = credentials.gid as libc::c_int;
    // SAFETY: plain system calls with values looked up above; on failure
    // the caller ends the process.
    unsafe {
        libc::initgroups(credentials.user.as_ptr(), base) == 0
            && libc::setgid(credentials.gid) == 0
            && libc::setuid(credentials.uid) == 0
    }
}

/// Start `command` in a session of its own, away from the terminal.
pub fn new_session(command: &mut Command) {
    // SAFETY: setsid(2) is async-signal-safe and touches nothing of the
    // parent's state; it runs in the child between fork and exec.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

/// The canonical name of `host` (`getaddrinfo` with `AI_CANONNAME`).
pub fn canonical_name(host: &str) -> Option<String> {
    let host = CString::new(host).ok()?;
    // SAFETY: zeroed is a valid `addrinfo` (all-null pointers, no flags).
    let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
    hints.ai_flags = libc::AI_CANONNAME;
    let mut result: *mut libc::addrinfo = std::ptr::null_mut();
    // SAFETY: valid pointers for the duration of the call; the list is
    // freed below, after the name has been copied out of it.
    unsafe {
        if libc::getaddrinfo(host.as_ptr(), std::ptr::null(), &hints, &mut result) != 0
            || result.is_null()
        {
            return None;
        }
        let name = (*result).ai_canonname;
        let text = (!name.is_null()).then(|| CStr::from_ptr(name).to_string_lossy().into_owned());
        libc::freeaddrinfo(result);
        text
    }
}
