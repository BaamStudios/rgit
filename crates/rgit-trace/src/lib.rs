// SPDX-License-Identifier: MIT
//! `GIT_TRACE*` tracing for rgit.
//!
//! Each trace variable is a static [`TraceKey`]. The environment is read on
//! first use; the destination rules are in [`target`]. Lines are written as
//!
//! ```text
//! 19:46:55.692038 setup.rs:42             setup: git_dir: .git
//! ```
//!
//! (local time, source location, message) or just the message when
//! `GIT_TRACE_BARE` is enabled, which git's test suite always sets.
//!
//! Call sites use the [`trace!`] macro, which only formats when the key is
//! enabled:
//!
//! ```
//! rgit_trace::trace!(rgit_trace::TRACE, "trace: exec: {}", "git-foo");
//! ```
//!
//! Behaviour follows `Documentation/git.adoc` (ENVIRONMENT, "Debugging") and
//! the git 2.56.0 binary. trace2 (`GIT_TRACE2*`) is not part of this crate.

pub mod format;
mod key;
pub mod messages;
pub mod quote;
pub mod target;

use std::ffi::OsStr;
use std::path::Path;
use std::time::{Duration, Instant};

pub use key::{Location, TraceKey};
pub use quote::{quote_arg_into, quote_argv};
pub use target::Target;

/// `GIT_TRACE`: general messages (built-ins, alias expansion, exec, run_command).
pub static TRACE: TraceKey = TraceKey::new("GIT_TRACE");
/// `GIT_TRACE_SETUP`: repository setup result.
pub static TRACE_SETUP: TraceKey = TraceKey::new("GIT_TRACE_SETUP");
/// `GIT_TRACE_PERFORMANCE`: timings.
pub static TRACE_PERFORMANCE: TraceKey = TraceKey::new("GIT_TRACE_PERFORMANCE");
/// `GIT_TRACE_PACKET`: pkt-line traffic (written by the protocol crate).
pub static TRACE_PACKET: TraceKey = TraceKey::new("GIT_TRACE_PACKET");
/// `GIT_TRACE_PACKFILE`: verbatim pack data, use [`TraceKey::write_raw`].
pub static TRACE_PACKFILE: TraceKey = TraceKey::new("GIT_TRACE_PACKFILE");
/// `GIT_TRACE_CURL`: HTTP transport dump (written by the transport crate).
pub static TRACE_CURL: TraceKey = TraceKey::new("GIT_TRACE_CURL");
/// `GIT_TRACE_REFS`: ref database operations.
pub static TRACE_REFS: TraceKey = TraceKey::new("GIT_TRACE_REFS");
/// `GIT_TRACE_SHALLOW`: shallow fetch/clone.
pub static TRACE_SHALLOW: TraceKey = TraceKey::new("GIT_TRACE_SHALLOW");
/// `GIT_TRACE_PACK_ACCESS`: pack file accesses.
pub static TRACE_PACK_ACCESS: TraceKey = TraceKey::new("GIT_TRACE_PACK_ACCESS");
/// `GIT_TRACE_FSMONITOR`: filesystem monitor.
pub static TRACE_FSMONITOR: TraceKey = TraceKey::new("GIT_TRACE_FSMONITOR");
/// `GIT_TRACE_BARE`: parsed like a trace target (git 2.56.0 warns about
/// unknown values and even opens a given path); when enabled, the time and
/// location columns are omitted.
pub static TRACE_BARE: TraceKey = TraceKey::new("GIT_TRACE_BARE");

/// How a registered variable is interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A trace target with line headers.
    Lines,
    /// A trace target written verbatim (no headers, no quoting).
    Verbatim,
    /// A boolean switch that modifies other tracing.
    Switch,
}

/// One entry of the [`REGISTRY`].
#[derive(Debug, Clone, Copy)]
pub struct Entry {
    /// Environment variable name.
    pub name: &'static str,
    /// Interpretation.
    pub kind: Kind,
    /// The key, for target variables.
    pub key: Option<&'static TraceKey>,
    /// Issue implementing the producer of this output, if not this crate.
    pub owner: Option<u32>,
}

const fn lines(key: &'static TraceKey, owner: Option<u32>) -> Entry {
    Entry {
        name: key.name(),
        kind: Kind::Lines,
        key: Some(key),
        owner,
    }
}

const fn switch(name: &'static str, owner: Option<u32>) -> Entry {
    Entry {
        name,
        kind: Kind::Switch,
        key: None,
        owner,
    }
}

/// All `GIT_TRACE*`-family variables rgit knows about. Booleans
/// (`GIT_TRACE_CURL_NO_DATA`, `GIT_CURL_VERBOSE`, `GIT_TRACE_REDACT`) are
/// listed for completeness; their semantics belong to the crates that
/// produce the respective output.
pub static REGISTRY: &[Entry] = &[
    lines(&TRACE, None),
    lines(&TRACE_SETUP, None),
    lines(&TRACE_PERFORMANCE, None),
    lines(&TRACE_PACKET, None),
    Entry {
        name: "GIT_TRACE_PACKFILE",
        kind: Kind::Verbatim,
        key: Some(&TRACE_PACKFILE),
        owner: Some(60),
    },
    lines(&TRACE_CURL, None),
    switch("GIT_TRACE_CURL_NO_DATA", None),
    switch("GIT_CURL_VERBOSE", None),
    switch("GIT_TRACE_REDACT", Some(162)),
    lines(&TRACE_REFS, None),
    lines(&TRACE_SHALLOW, None),
    lines(&TRACE_PACK_ACCESS, None),
    lines(&TRACE_FSMONITOR, None),
    lines(&TRACE_BARE, None),
];

/// Look up a registered variable by name.
pub fn lookup(name: &str) -> Option<&'static Entry> {
    REGISTRY.iter().find(|e| e.name == name)
}

/// Whether headers are suppressed (`GIT_TRACE_BARE`).
pub fn bare() -> bool {
    TRACE_BARE.enabled()
}

/// The location of the macro call site.
#[macro_export]
macro_rules! location {
    () => {
        $crate::Location {
            file: ::core::file!(),
            line: ::core::line!(),
        }
    };
}

/// Write a formatted line to a trace key if it is enabled.
///
/// The arguments are only evaluated when the key is enabled.
#[macro_export]
macro_rules! trace {
    ($key:expr, $($arg:tt)+) => {{
        let key: &$crate::TraceKey = &$key;
        if key.enabled() {
            key.write_fmt_line($crate::location!(), ::core::format_args!($($arg)+));
        }
    }};
}

/// Write `performance: <seconds> s: <message>` to `GIT_TRACE_PERFORMANCE`
/// with the time elapsed since `start` (an [`Instant`]).
#[macro_export]
macro_rules! trace_performance_since {
    ($start:expr, $($arg:tt)+) => {{
        if $crate::TRACE_PERFORMANCE.enabled() {
            let elapsed = ::std::time::Instant::elapsed(&$start);
            $crate::TRACE_PERFORMANCE.write_fmt_line(
                $crate::location!(),
                ::core::format_args!(
                    "performance: {} s: {}",
                    $crate::format::seconds(elapsed),
                    ::core::format_args!($($arg)+)
                ),
            );
        }
    }};
}

fn line_with_argv<S: AsRef<OsStr>>(prefix: &str, argv: &[S]) -> Vec<u8> {
    let mut msg = prefix.as_bytes().to_vec();
    msg.extend_from_slice(&quote_argv(argv));
    msg
}

fn push_bytes(out: &mut Vec<u8>, s: &OsStr) {
    out.extend_from_slice(s.as_encoded_bytes());
}

/// `GIT_TRACE_SETUP`: the five `setup:` lines after repository discovery.
///
/// Values are written unquoted; `None` prints `(null)` (no worktree, or no
/// prefix when the cwd is the top of the worktree or outside it).
#[track_caller]
pub fn trace_setup(
    git_dir: &Path,
    common_dir: &Path,
    worktree: Option<&Path>,
    cwd: &Path,
    prefix: Option<&OsStr>,
) {
    if !TRACE_SETUP.enabled() {
        return;
    }
    let loc = Location::caller();
    let null = OsStr::new("(null)");
    let fields: [(&str, &OsStr); 5] = [
        ("git_dir", git_dir.as_os_str()),
        ("git_common_dir", common_dir.as_os_str()),
        ("worktree", worktree.map_or(null, Path::as_os_str)),
        ("cwd", cwd.as_os_str()),
        ("prefix", prefix.unwrap_or(null)),
    ];
    for (name, value) in fields {
        let mut msg = format!("setup: {name}: ").into_bytes();
        push_bytes(&mut msg, value);
        TRACE_SETUP.write_line(loc, &msg);
    }
}

/// `GIT_TRACE`: `trace: built-in: git <args>`. `args` excludes `argv[0]`;
/// git always prints `git` there.
#[track_caller]
pub fn trace_builtin<S: AsRef<OsStr>>(args: &[S]) {
    if TRACE.enabled() {
        let mut msg = b"trace: built-in: git".to_vec();
        for a in args {
            msg.push(b' ');
            quote_arg_into(&mut msg, a.as_ref().as_encoded_bytes());
        }
        TRACE.write_line(Location::caller(), &msg);
    }
}

/// `GIT_TRACE`: `trace: alias expansion: <alias> => <expansion>`.
#[track_caller]
pub fn trace_alias_expansion<S: AsRef<OsStr>>(alias: &str, expansion: &[S]) {
    if TRACE.enabled() {
        let msg = line_with_argv(&format!("trace: alias expansion: {alias} => "), expansion);
        TRACE.write_line(Location::caller(), &msg);
    }
}

/// `GIT_TRACE`: `trace: exec: <argv>`.
#[track_caller]
pub fn trace_exec<S: AsRef<OsStr>>(argv: &[S]) {
    if TRACE.enabled() {
        TRACE.write_line(Location::caller(), &line_with_argv("trace: exec: ", argv));
    }
}

/// `GIT_TRACE`: `trace: run_command: [NAME=value ...] <argv>`; `env` lists
/// the variables the child gets in addition to the inherited environment.
#[track_caller]
pub fn trace_run_command<S: AsRef<OsStr>>(env: &[(&str, &OsStr)], argv: &[S]) {
    if TRACE.enabled() {
        let mut msg = b"trace: run_command: ".to_vec();
        for (name, value) in env {
            msg.extend_from_slice(name.as_bytes());
            msg.push(b'=');
            quote_arg_into(&mut msg, value.as_encoded_bytes());
            msg.push(b' ');
        }
        msg.extend_from_slice(&quote_argv(argv));
        TRACE.write_line(Location::caller(), &msg);
    }
}

/// `GIT_TRACE`: `trace: start_command: <argv>` (the resolved program).
#[track_caller]
pub fn trace_start_command<S: AsRef<OsStr>>(argv: &[S]) {
    if TRACE.enabled() {
        TRACE.write_line(
            Location::caller(),
            &line_with_argv("trace: start_command: ", argv),
        );
    }
}

/// `GIT_TRACE_PERFORMANCE`: `performance: <seconds> s: <message>`.
#[track_caller]
pub fn trace_performance(elapsed: Duration, message: &str) {
    if TRACE_PERFORMANCE.enabled() {
        let msg = format!("performance: {} s: {message}", format::seconds(elapsed));
        TRACE_PERFORMANCE.write_line(Location::caller(), msg.as_bytes());
    }
}

/// `GIT_TRACE_PERFORMANCE`: the line printed when a command exits,
/// `performance: <seconds> s: git command: <argv>`. `argv` is the full
/// command line as invoked, including `argv[0]`.
#[track_caller]
pub fn trace_command_performance<S: AsRef<OsStr>>(start: Instant, argv: &[S]) {
    if TRACE_PERFORMANCE.enabled() {
        let mut msg = format!(
            "performance: {} s: git command: ",
            format::seconds(start.elapsed())
        )
        .into_bytes();
        msg.extend_from_slice(&quote_argv(argv));
        TRACE_PERFORMANCE.write_line(Location::caller(), &msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_complete() {
        for name in [
            "GIT_TRACE",
            "GIT_TRACE_SETUP",
            "GIT_TRACE_PERFORMANCE",
            "GIT_TRACE_PACKET",
            "GIT_TRACE_PACKFILE",
            "GIT_TRACE_CURL",
            "GIT_TRACE_CURL_NO_DATA",
            "GIT_CURL_VERBOSE",
            "GIT_TRACE_REDACT",
            "GIT_TRACE_REFS",
            "GIT_TRACE_SHALLOW",
            "GIT_TRACE_PACK_ACCESS",
            "GIT_TRACE_FSMONITOR",
            "GIT_TRACE_BARE",
        ] {
            assert!(lookup(name).is_some(), "{name}");
        }
        assert_eq!(lookup("GIT_TRACE_PACKFILE").unwrap().kind, Kind::Verbatim);
        assert!(lookup("GIT_TRACE2").is_none());
    }

    #[test]
    fn local_key_disabled_does_not_format() {
        static K: TraceKey = TraceKey::new("RGIT_TRACE_TEST_LOCAL_UNSET");
        K.init_from(None);
        let mut evaluated = false;
        trace!(K, "{}", {
            evaluated = true;
            1
        });
        assert!(!evaluated);
        assert!(!K.enabled());
    }

    #[test]
    fn location_basename() {
        let l = location!();
        assert!(l.to_string().starts_with("lib.rs:"));
    }
}
