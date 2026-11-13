// SPDX-License-Identifier: MIT
//! Ignoring SIGPIPE while rgit writes to a child process.
//!
//! The rgit binary restores the default SIGPIPE action so that
//! `rgit log | head` dies quietly like git. Writing to a child that has
//! already exited (a credential helper that never reads its input, an
//! `index-pack` that failed) would then kill rgit. git ignores SIGPIPE
//! while it feeds such a child and treats the resulting EPIPE as "the child
//! did not read"; [`IgnoreSigpipe`] does the same.

#[cfg(unix)]
use std::sync::Mutex;

/// While at least one guard is alive, SIGPIPE is ignored process-wide and
/// a write to a closed pipe fails with EPIPE instead. Dropping the last
/// guard restores the disposition that was in place before the first one
/// (default action, or the lock-file cleanup handler of `rgit-fs`).
#[must_use = "SIGPIPE is ignored only while the guard is alive"]
#[derive(Debug)]
pub struct IgnoreSigpipe(());

/// Number of live guards and the disposition to restore.
#[cfg(unix)]
static STATE: Mutex<(usize, Option<libc::sigaction>)> = Mutex::new((0, None));

impl IgnoreSigpipe {
    /// Ignore SIGPIPE until the returned guard is dropped.
    pub fn new() -> Self {
        #[cfg(unix)]
        {
            let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
            if state.0 == 0 {
                // SAFETY: plain sigaction calls with valid pointers; the
                // mutex serialises them against other guards.
                unsafe {
                    let mut ign: libc::sigaction = std::mem::zeroed();
                    ign.sa_sigaction = libc::SIG_IGN;
                    libc::sigemptyset(&mut ign.sa_mask);
                    let mut old: libc::sigaction = std::mem::zeroed();
                    if libc::sigaction(libc::SIGPIPE, &ign, &mut old) == 0 {
                        state.1 = Some(old);
                    }
                }
            }
            state.0 += 1;
        }
        IgnoreSigpipe(())
    }
}

impl Default for IgnoreSigpipe {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for IgnoreSigpipe {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
            state.0 -= 1;
            if state.0 == 0 {
                if let Some(old) = state.1.take() {
                    // SAFETY: restores a disposition read by `sigaction`.
                    unsafe {
                        libc::sigaction(libc::SIGPIPE, &old, std::ptr::null_mut());
                    }
                }
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn current() -> libc::sighandler_t {
        unsafe {
            let mut old: libc::sigaction = std::mem::zeroed();
            libc::sigaction(libc::SIGPIPE, std::ptr::null(), &mut old);
            old.sa_sigaction
        }
    }

    #[test]
    fn nested_guards_restore_the_previous_disposition() {
        let before = current();
        {
            let _a = IgnoreSigpipe::new();
            assert_eq!(current(), libc::SIG_IGN);
            {
                let _b = IgnoreSigpipe::new();
                assert_eq!(current(), libc::SIG_IGN);
            }
            assert_eq!(current(), libc::SIG_IGN);
        }
        assert_eq!(current(), before);
    }
}
