// SPDX-License-Identifier: MIT
//! trace2: structured tracing to the normal, perf and event targets.
//!
//! Formats follow `Documentation/technical/api-trace2.adoc` (the "Trace2
//! Target Formats" section) and the output of the git 2.56.1 binary;
//! settings follow `Documentation/config/trace2.adoc`. Threads that want
//! their own name in the events (and per-thread timer and counter
//! summaries) announce themselves with [`thread_start`] and [`thread_exit`];
//! every other thread writes as `main`.
//!
//! The process calls [`init`] once with its argument vector and a lookup
//! for the system/global `trace2.*` configuration, then reports what it
//! does ([`cmd_name`], [`def_repo`], [`child_start`], [`region_enter`],
//! [`data`], ...) and finally [`exit`]. Every function is a cheap no-op
//! while no target is enabled.

mod dest;
mod json;
mod stats;

use std::ffi::OsStr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::key::Location;
use crate::quote::quote_argv;
use dest::{Dest, Opened, Spec};
use json::Json;
pub use stats::{Counter, Timer, thread_exit, thread_start};

/// The EVENT format version written in the `version` event.
const EVENT_FORMAT_VERSION: &str = "4";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Normal,
    Perf,
    Event,
}

struct Target {
    format: Format,
    brief: bool,
    dest: Dest,
    /// Only `version` and `too_many_files` go to a discard sentinel.
    discard: bool,
}

struct Region {
    category: String,
    label: String,
    repo: Option<u32>,
    start: Instant,
}

struct State {
    start: Instant,
    sid: String,
    /// Number of parent git processes (`d<depth>` in the perf format).
    depth: usize,
    targets: Vec<Target>,
    event_nesting: usize,
    regions: Vec<Region>,
    next_child: u32,
    next_exec: u32,
    config_params: Vec<String>,
    env_vars: Vec<String>,
    redact: bool,
    exit_code: Option<i32>,
    /// `def_param` keys already written (alias unpeeling re-lists them).
    params_done: bool,
    /// `def_repo` was written.
    repo_done: bool,
    /// Process-wide timer and counter totals, in order of first use.
    timers: Vec<stats::TimerSlot>,
    counters: Vec<stats::CounterSlot>,
    /// Number of threads announced with [`thread_start`] so far.
    threads: u32,
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Whether any trace2 target is enabled.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    if !enabled() {
        return None;
    }
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    guard.as_mut().map(f)
}

/// Settings read before tracing starts: the environment overrides the
/// system/global configuration.
struct Settings {
    targets: [(Format, &'static str, String, bool); 3],
    event_nesting: usize,
    config_params: Vec<String>,
    env_vars: Vec<String>,
    dst_debug: bool,
    max_files: usize,
    redact: bool,
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "" => Some(false),
        "true" | "yes" | "on" => Some(true),
        "false" | "no" | "off" => Some(false),
        n => n.parse::<i64>().ok().map(|n| n != 0),
    }
}

fn split_list(v: &str) -> Vec<String> {
    v.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

impl Settings {
    fn read(config: &dyn Fn(&str) -> Option<String>) -> Settings {
        let get = |env: &str, key: &str| -> Option<String> {
            std::env::var_os(env)
                .map(|v| v.to_string_lossy().into_owned())
                .or_else(|| config(key))
        };
        let flag = |env: &str, key: &str| get(env, key).and_then(|v| parse_bool(&v)) == Some(true);
        let target = |format, env: &'static str, key: &str, brief_env: &str, brief_key: &str| {
            (
                format,
                env,
                get(env, key).unwrap_or_default(),
                flag(brief_env, brief_key),
            )
        };
        Settings {
            targets: [
                target(
                    Format::Normal,
                    "GIT_TRACE2",
                    "trace2.normaltarget",
                    "GIT_TRACE2_BRIEF",
                    "trace2.normalbrief",
                ),
                target(
                    Format::Perf,
                    "GIT_TRACE2_PERF",
                    "trace2.perftarget",
                    "GIT_TRACE2_PERF_BRIEF",
                    "trace2.perfbrief",
                ),
                target(
                    Format::Event,
                    "GIT_TRACE2_EVENT",
                    "trace2.eventtarget",
                    "GIT_TRACE2_EVENT_BRIEF",
                    "trace2.eventbrief",
                ),
            ],
            event_nesting: get("GIT_TRACE2_EVENT_NESTING", "trace2.eventnesting")
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(2),
            config_params: get("GIT_TRACE2_CONFIG_PARAMS", "trace2.configparams")
                .map(|v| split_list(&v))
                .unwrap_or_default(),
            env_vars: get("GIT_TRACE2_ENV_VARS", "trace2.envvars")
                .map(|v| split_list(&v))
                .unwrap_or_default(),
            dst_debug: flag("GIT_TRACE2_DST_DEBUG", "trace2.destinationdebug"),
            max_files: get("GIT_TRACE2_MAX_FILES", "trace2.maxfiles")
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0),
            redact: std::env::var("GIT_TRACE2_REDACT")
                .ok()
                .and_then(|v| parse_bool(&v))
                .unwrap_or(true),
        }
    }
}

/// A hash of the host name for the session id.
fn host_hash() -> u32 {
    let name = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| std::fs::read_to_string("/etc/hostname"))
        .unwrap_or_default();
    // FNV-1a.
    let mut h: u32 = 0x811c_9dc5;
    for b in name.trim().bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

fn new_sid() -> String {
    let now = Timestamp::now().to_zoned(TimeZone::UTC);
    let own = format!(
        "{}{:02}{:02}T{:02}{:02}{:02}.{:06}Z-H{:08x}-P{:08x}",
        now.year(),
        now.month(),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        now.subsec_nanosecond() / 1000,
        host_hash(),
        std::process::id()
    );
    match std::env::var("GIT_TRACE2_PARENT_SID") {
        Ok(parent) if !parent.is_empty() => format!("{parent}/{own}"),
        _ => own,
    }
}

/// The session id of this process: the trace2 `sid` while tracing, else a
/// value of the same form made once per process (the `session-id`
/// capability, `Documentation/gitprotocol-capabilities.adoc`).
pub fn session_id() -> String {
    if let Some(sid) = with_state(|s| s.sid.clone()) {
        return sid;
    }
    static SID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SID.get_or_init(new_sid).clone()
}

/// Start tracing: read the settings (environment first, then `config`,
/// which answers lower-case `trace2.*` keys from the system and global
/// configuration), open the targets and write `version`, `start` and
/// `cmd_ancestry`. `version` is what `git version` prints after
/// `git version `.
#[track_caller]
pub fn init<S: AsRef<OsStr>>(argv: &[S], version: &str, config: &dyn Fn(&str) -> Option<String>) {
    let loc = Location::caller();
    let start = Instant::now();
    if enabled() {
        return;
    }
    let settings = Settings::read(config);
    if settings
        .targets
        .iter()
        .all(|(_, _, v, _)| matches!(Spec::parse(v), Spec::Off))
    {
        return;
    }
    let sid = new_sid();
    let tail = sid.rsplit('/').next().unwrap_or(&sid).to_string();
    let mut targets = Vec::new();
    for (format, name, value, brief) in &settings.targets {
        let spec = Spec::parse(value);
        let (dest, discard) = match dest::open(
            name,
            value,
            &spec,
            &tail,
            settings.max_files,
            settings.dst_debug,
        ) {
            Opened::Ready(d) => (d, false),
            Opened::Discard(d) => (d, true),
            Opened::None => continue,
        };
        targets.push(Target {
            format: *format,
            brief: *brief,
            dest,
            discard,
        });
    }
    if targets.is_empty() {
        return;
    }
    let depth = sid.matches('/').count();
    // Children see this process as their parent.
    // SAFETY: called at process start, before other threads exist.
    unsafe { std::env::set_var("GIT_TRACE2_PARENT_SID", &sid) };
    *STATE.lock().unwrap_or_else(|e| e.into_inner()) = Some(State {
        start,
        sid,
        depth,
        targets,
        event_nesting: settings.event_nesting,
        regions: Vec::new(),
        next_child: 0,
        next_exec: 0,
        config_params: settings.config_params,
        env_vars: settings.env_vars,
        redact: settings.redact,
        exit_code: None,
        params_done: false,
        repo_done: false,
        timers: Vec::new(),
        counters: Vec::new(),
        threads: 0,
    });
    ENABLED.store(true, Ordering::Relaxed);

    emit(loc, |_| {
        let mut ev = Ev::new("version");
        ev.normal = Some(version.to_string());
        ev.perf = version.to_string();
        ev.json.str("evt", EVENT_FORMAT_VERSION).str("exe", version);
        ev.time_in_brief = true;
        ev.always = true;
        ev
    });
    with_state(|s| {
        // The discard sentinel gets `too_many_files` and nothing else.
        let mut discarding = Vec::new();
        for (i, t) in s.targets.iter().enumerate() {
            if t.discard {
                discarding.push(i);
            }
        }
        for i in discarding.into_iter().rev() {
            let mut t = s.targets.remove(i);
            let ev = Ev::new("too_many_files");
            let line = render(s, &t, loc, &ev);
            let _ = t.dest.write_event(&line);
        }
    });
    start_event(loc, argv);
    if let Some(chain) = ancestry() {
        emit(loc, |_| {
            let mut ev = Ev::new("cmd_ancestry");
            ev.normal = Some(chain.join(" <- "));
            ev.perf = format!("ancestry:[{}]", chain.join(" "));
            ev.json.arr("ancestry", &chain);
            ev
        });
    }
}

/// `start`: the command line of the process. [`init`] writes it; this is
/// for a process that reports a further one (an exec emulation).
#[track_caller]
pub fn start<S: AsRef<OsStr>>(argv: &[S]) {
    start_event(Location::caller(), argv);
}

fn start_event<S: AsRef<OsStr>>(loc: Location, argv: &[S]) {
    emit(loc, |s| {
        let args = redacted(s, argv);
        let mut ev = Ev::new("start");
        ev.t_abs = Some(s.start.elapsed().as_secs_f64());
        let q = String::from_utf8_lossy(&quote_argv(&args)).into_owned();
        ev.normal = Some(q.clone());
        ev.perf = q;
        ev.json
            .float("t_abs", ev.t_abs.unwrap_or(0.0))
            .arr("argv", &args);
        ev
    });
}

/// `argv` as strings, with the passwords in URLs redacted unless
/// `GIT_TRACE2_REDACT` is off.
fn redacted<S: AsRef<OsStr>>(s: &State, argv: &[S]) -> Vec<String> {
    argv.iter()
        .map(|a| {
            let a = a.as_ref().to_string_lossy();
            if s.redact {
                redact_url(&a)
            } else {
                a.into_owned()
            }
        })
        .collect()
}

/// The names of the parent processes, nearest first (Linux only).
fn ancestry() -> Option<Vec<String>> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let mut out = Vec::new();
    let mut pid = parent_of(std::process::id())?;
    while pid != 0 && out.len() < 64 {
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
        out.push(comm.trim_end_matches('\n').to_string());
        pid = parent_of(pid).unwrap_or(0);
    }
    Some(out)
}

fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `pid (comm) state ppid ...`; comm may contain spaces and parentheses.
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// Replace the password of an `http://` or `https://` URL
/// (`scheme://user:pass@host`) by `<REDACTED>`, as git does with
/// `GIT_TRACE2_REDACT` (default on). Observed from git 2.56.1: only these two
/// (lower-case) schemes, the authority ends at the first `/`, the user
/// information at the first `@` in it and the user name at its first `:`;
/// the user name stays.
pub fn redact_url(s: &str) -> String {
    let Some(rest) = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
    else {
        return s.to_string();
    };
    let start = s.len() - rest.len();
    let authority = &rest[..rest.find('/').unwrap_or(rest.len())];
    let Some(at) = authority.find('@') else {
        return s.to_string();
    };
    let Some(colon) = authority[..at].find(':') else {
        return s.to_string();
    };
    format!("{}<REDACTED>{}", &s[..start + colon + 1], &s[start + at..])
}

/// One event, prepared for every format.
struct Ev {
    name: &'static str,
    /// The event name in the normal format, when it differs.
    normal_name: Option<&'static str>,
    /// Message of the normal format; `None`: not written there.
    normal: Option<String>,
    /// Message column of the perf format.
    perf: String,
    /// Event-specific JSON fields.
    json: Json,
    repo: Option<u32>,
    t_abs: Option<f64>,
    t_rel: Option<f64>,
    category: String,
    /// Region depth for the perf dots.
    dots: usize,
    /// JSON nesting level; `Some` for regions and data (filtered by
    /// `trace2.eventNesting`).
    nesting: Option<usize>,
    /// The brief event format still has `time` (version, atexit).
    time_in_brief: bool,
    /// Also written to a discard sentinel.
    always: bool,
}

impl Ev {
    fn new(name: &'static str) -> Ev {
        Ev {
            name,
            normal_name: None,
            normal: None,
            perf: String::new(),
            json: Json::default(),
            repo: None,
            t_abs: None,
            t_rel: None,
            category: String::new(),
            dots: 0,
            nesting: None,
            time_in_brief: false,
            always: false,
        }
    }
}

fn local_time() -> String {
    let now = jiff::Zoned::now();
    format!(
        "{:02}:{:02}:{:02}.{:06}",
        now.hour(),
        now.minute(),
        now.second(),
        now.subsec_nanosecond() / 1000
    )
}

fn utc_time() -> String {
    let now = Timestamp::now().to_zoned(TimeZone::UTC);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}Z",
        now.year(),
        now.month(),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        now.subsec_nanosecond() / 1000
    )
}

fn secs(t: f64) -> String {
    format!("{t:.6}")
}

fn render(s: &State, t: &Target, loc: Location, ev: &Ev) -> Vec<u8> {
    match t.format {
        Format::Normal => {
            let mut out = String::new();
            if !t.brief {
                out.push_str(&format!("{} {:<33} ", local_time(), loc.to_string()));
            }
            out.push_str(ev.normal_name.unwrap_or(ev.name));
            if let Some(m) = &ev.normal {
                if !m.is_empty() && !m.starts_with('[') {
                    out.push(' ');
                }
                out.push_str(m);
            }
            out.push('\n');
            out.into_bytes()
        }
        Format::Perf => {
            let mut out = String::new();
            if !t.brief {
                out.push_str(&format!("{} {:<28} | ", local_time(), loc.to_string()));
            }
            let thread: String = stats::thread_name().chars().take(24).collect();
            let repo = ev.repo.map(|r| format!("r{r}")).unwrap_or_default();
            let opt = |v: Option<f64>| v.map(|v| format!("{v:9.6}")).unwrap_or(" ".repeat(9));
            let category: String = ev.category.chars().take(12).collect();
            out.push_str(&format!(
                "d{} | {:<24} | {:<12} | {:<3} | {} | {} | {:<12} | {}{}\n",
                s.depth,
                thread,
                ev.name,
                repo,
                opt(ev.t_abs),
                opt(ev.t_rel),
                category,
                "..".repeat(ev.dots),
                ev.perf
            ));
            out.into_bytes()
        }
        Format::Event => {
            let mut j = Json::default();
            j.str("event", ev.name)
                .str("sid", &s.sid)
                .str("thread", &stats::thread_name());
            if !t.brief || ev.time_in_brief {
                j.str("time", &utc_time());
            }
            if !t.brief {
                j.str(
                    "file",
                    loc.file.rsplit(['/', '\\']).next().unwrap_or(loc.file),
                )
                .int("line", i64::from(loc.line));
            }
            if let Some(r) = ev.repo {
                j.int("repo", i64::from(r));
            }
            j.append(&ev.json);
            let mut out = j.finish().into_bytes();
            out.push(b'\n');
            out
        }
    }
}

/// Build an event with access to the state and write it to every target.
fn emit(loc: Location, build: impl FnOnce(&State) -> Ev) {
    with_state(|s| {
        let ev = build(s);
        let nesting = s.event_nesting;
        // Render needs `s` immutably; collect lines first.
        let mut lines = Vec::with_capacity(s.targets.len());
        for t in s.targets.iter() {
            if t.discard && !ev.always {
                lines.push(None);
                continue;
            }
            if t.format == Format::Normal && ev.normal.is_none() {
                lines.push(None);
                continue;
            }
            if t.format == Format::Event && ev.nesting.is_some_and(|n| n > nesting) {
                lines.push(None);
                continue;
            }
            lines.push(Some(render(s, t, loc, &ev)));
        }
        for (t, line) in s.targets.iter_mut().zip(lines) {
            if let Some(line) = line {
                let _ = t.dest.write_event(&line);
            }
        }
    });
}

/// `cmd_name`: the command's name; the hierarchy adds the names of the
/// parent git processes (`GIT_TRACE2_PARENT_NAME`), which children inherit.
#[track_caller]
pub fn cmd_name(name: &str) {
    if !enabled() {
        return;
    }
    let hierarchy = match std::env::var("GIT_TRACE2_PARENT_NAME") {
        Ok(p) if !p.is_empty() => format!("{p}/{name}"),
        _ => name.to_string(),
    };
    // SAFETY: the main thread names the command before starting threads.
    unsafe { std::env::set_var("GIT_TRACE2_PARENT_NAME", &hierarchy) };
    emit(Location::caller(), |_| {
        let mut ev = Ev::new("cmd_name");
        let m = format!("{name} ({hierarchy})");
        ev.normal = Some(m.clone());
        ev.perf = m;
        ev.json.str("name", name).str("hierarchy", &hierarchy);
        ev
    });
}

/// `cmd_mode`: the variant of the command.
#[track_caller]
pub fn cmd_mode(mode: &str) {
    emit(Location::caller(), |_| {
        let mut ev = Ev::new("cmd_mode");
        ev.normal = Some(mode.to_string());
        ev.perf = mode.to_string();
        ev.json.str("name", mode);
        ev
    });
}

/// `def_repo`: the repository with this work tree is in use (repo 1).
#[track_caller]
pub fn def_repo(worktree: &str) {
    if with_state(|s| std::mem::replace(&mut s.repo_done, true)) != Some(false) {
        return;
    }
    emit(Location::caller(), |_| {
        let mut ev = Ev::new("def_repo");
        ev.repo = Some(1);
        ev.normal_name = Some("worktree");
        ev.normal = Some(worktree.to_string());
        ev.perf = format!("worktree:{worktree}");
        ev.json.str("worktree", worktree);
        ev
    });
}

/// The `trace2.configParams` patterns (config keys to list as `def_param`).
pub fn config_param_patterns() -> Vec<String> {
    with_state(|s| s.config_params.clone()).unwrap_or_default()
}

/// Whether `key` (canonical, section and variable lower case) matches one
/// of the `trace2.configParams` patterns (`*` matches anything, also `.`).
pub fn is_config_param(patterns: &[String], key: &str) -> bool {
    patterns
        .iter()
        .any(|p| glob(p.to_ascii_lowercase().as_bytes(), key.as_bytes()))
}

fn glob(p: &[u8], s: &[u8]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((b'*', rest)) => (0..=s.len()).any(|i| glob(rest, &s[i..])),
        Some((c, rest)) => s
            .split_first()
            .is_some_and(|(d, srest)| c.eq_ignore_ascii_case(d) && glob(rest, srest)),
    }
}

/// List the "important" settings once per process: `params` are the
/// matching config values (key, value, scope name) in order, followed by the
/// `trace2.envVars` variables that are set (scope `command`).
#[track_caller]
pub fn def_params(params: &[(String, String, String)]) {
    if !enabled() {
        return;
    }
    let loc = Location::caller();
    let (first, env_vars, redact) = match with_state(|s| {
        let first = !s.params_done;
        s.params_done = true;
        (first, s.env_vars.clone(), s.redact)
    }) {
        Some(v) => v,
        None => return,
    };
    if !first {
        return;
    }
    for (key, value, scope) in params {
        def_param_at(loc, scope, key, value, redact);
    }
    for name in env_vars {
        if let Some(v) = std::env::var_os(&name) {
            def_param_at(loc, "command", &name, &v.to_string_lossy(), redact);
        }
    }
}

/// One `def_param` event (`scope` is a config scope name, `command` or
/// `unknown`), independent of [`def_params`].
#[track_caller]
pub fn def_param(scope: &str, key: &str, value: &str) {
    let Some(redact) = with_state(|s| s.redact) else {
        return;
    };
    def_param_at(Location::caller(), scope, key, value, redact);
}

fn def_param_at(loc: Location, scope: &str, key: &str, value: &str, redact: bool) {
    let value = if redact {
        redact_url(value)
    } else {
        value.to_string()
    };
    emit(loc, |_| {
        let mut ev = Ev::new("def_param");
        ev.normal = Some(format!("scope:{scope} {key}={value}"));
        ev.category = format!("scope:{scope}");
        ev.perf = format!("{key}:{value}");
        ev.json
            .str("scope", scope)
            .str("param", key)
            .str("value", &value);
        ev
    });
}

/// `alias`: `alias` expands to `argv`.
#[track_caller]
pub fn alias<S: AsRef<str>>(alias: &str, argv: &[S]) {
    emit(Location::caller(), |_| {
        let args: Vec<&str> = argv.iter().map(AsRef::as_ref).collect();
        let q = String::from_utf8_lossy(&quote_argv(&args)).into_owned();
        let mut ev = Ev::new("alias");
        ev.normal = Some(format!("{alias} -> {q}"));
        ev.perf = format!("alias:{alias} argv:[{q}]");
        ev.json.str("alias", alias).arr("argv", &args);
        ev
    });
}

/// `error`: a message printed with `fatal:`, `error:` and the like.
#[track_caller]
pub fn error(msg: &str) {
    error_at(Location::caller(), msg, msg);
}

/// [`error`] for a message made from a format: the event format also
/// lists the format string (`fmt`), e.g. `cannot run %s: %s`.
#[track_caller]
pub fn error_fmt(msg: &str, fmt: &str) {
    error_at(Location::caller(), msg, fmt);
}

fn error_at(loc: Location, msg: &str, fmt: &str) {
    emit(loc, |_| {
        let mut ev = Ev::new("error");
        ev.normal = Some(msg.to_string());
        ev.perf = msg.to_string();
        ev.json.str("msg", msg).str("fmt", fmt);
        ev
    });
}

/// `exec`: the process is about to replace itself by `exe` with the
/// arguments `argv` (git lists `exe` in front of `argv` in the normal and
/// perf formats, and separately in the event format).
#[track_caller]
pub fn exec<S: AsRef<OsStr>>(exe: &str, argv: &[S]) -> Option<u32> {
    let id = with_state(|s| {
        let id = s.next_exec;
        s.next_exec += 1;
        id
    })?;
    emit(Location::caller(), |s| {
        let exe = if s.redact {
            redact_url(exe)
        } else {
            exe.to_string()
        };
        let args = redacted(s, argv);
        let line: Vec<&str> = std::iter::once(exe.as_str())
            .chain(args.iter().map(String::as_str))
            .collect();
        let q = String::from_utf8_lossy(&quote_argv(&line)).into_owned();
        // The normal format quotes `exe` and `argv` separately (a space
        // follows `exe` even without arguments).
        let q_exe = String::from_utf8_lossy(&quote_argv([exe.as_str()])).into_owned();
        let q_args = String::from_utf8_lossy(&quote_argv(&args)).into_owned();
        let mut ev = Ev::new("exec");
        ev.t_abs = Some(s.start.elapsed().as_secs_f64());
        ev.normal = Some(format!("[{id}] {q_exe} {q_args}"));
        ev.perf = format!("id:{id} argv:[{q}]");
        ev.json
            .int("exec_id", i64::from(id))
            .str("exe", &exe)
            .arr("argv", &args);
        ev
    });
    Some(id)
}

/// A started child process, for [`ChildTrace::exit`].
#[derive(Debug)]
pub struct ChildTrace {
    id: u32,
    start: Instant,
}

/// What a child process is (the `child_class` field).
#[derive(Debug, Clone, Copy)]
pub struct ChildInfo<'a> {
    /// `?`, `hook`, `pager`, `editor`, `dashed`, `shell_alias`,
    /// `git_alias`, `transport/ssh`, ...
    pub class: &'a str,
    /// The hook name, for class `hook`.
    pub hook: Option<&'a str>,
    /// The command runs through the shell.
    pub use_shell: bool,
    /// The directory the child runs in, when it changes.
    pub cd: Option<&'a str>,
}

impl Default for ChildInfo<'_> {
    fn default() -> Self {
        ChildInfo {
            class: "?",
            hook: None,
            use_shell: false,
            cd: None,
        }
    }
}

/// `child_start`: a child process is about to be spawned with `argv`.
/// Returns `None` while tracing is off.
#[track_caller]
pub fn child_start<S: AsRef<OsStr>>(info: ChildInfo<'_>, argv: &[S]) -> Option<ChildTrace> {
    let id = with_state(|s| {
        let id = s.next_child;
        s.next_child += 1;
        id
    })?;
    emit(Location::caller(), |s| {
        let args = redacted(s, argv);
        let q = String::from_utf8_lossy(&quote_argv(&args)).into_owned();
        let mut ev = Ev::new("child_start");
        ev.t_abs = Some(s.start.elapsed().as_secs_f64());
        ev.dots = s.regions.len();
        ev.normal = Some(format!("[{id}] {q}"));
        let mut perf = format!("[ch{id}] class:{}", info.class);
        if let Some(h) = info.hook {
            perf.push_str(&format!(" hook:{h}"));
        }
        if let Some(cd) = info.cd {
            perf.push_str(&format!(" cd:{cd}"));
        }
        perf.push_str(&format!(" argv:[{q}]"));
        ev.perf = perf;
        ev.json
            .int("child_id", i64::from(id))
            .str("child_class", info.class);
        if let Some(h) = info.hook {
            ev.json.str("hook_name", h);
        }
        if let Some(cd) = info.cd {
            ev.json.str("cd", cd);
        }
        ev.json.bool("use_shell", info.use_shell).arr("argv", &args);
        ev
    });
    Some(ChildTrace {
        id,
        start: Instant::now(),
    })
}

impl ChildTrace {
    /// `child_exit`: the child ended; `pid` -1 and `code` -1 when it could
    /// not be started; a signal is reported as 128 + its number.
    #[track_caller]
    pub fn exit(self, pid: i64, code: i32) {
        let t_rel = self.start.elapsed().as_secs_f64();
        emit(Location::caller(), |s| {
            let mut ev = Ev::new("child_exit");
            ev.t_abs = Some(s.start.elapsed().as_secs_f64());
            ev.t_rel = Some(t_rel);
            ev.dots = s.regions.len();
            ev.normal = Some(format!(
                "[{}] pid:{pid} code:{code} elapsed:{}",
                self.id,
                secs(t_rel)
            ));
            ev.perf = format!("[ch{}] pid:{pid} code:{code}", self.id);
            ev.json
                .int("child_id", i64::from(self.id))
                .int("pid", pid)
                .int("code", i64::from(code))
                .float("t_rel", t_rel);
            ev
        });
    }

    /// [`ChildTrace::exit`] from an [`std::process::ExitStatus`].
    #[track_caller]
    pub fn exit_status(self, pid: u32, status: std::process::ExitStatus) {
        self.exit(i64::from(pid), status_code(status));
    }
}

/// `Command::status` with `child_start`/`child_exit` events; `argv` is
/// the command line as git reports it (`git <args>` for git children).
#[track_caller]
pub fn status<S: AsRef<OsStr>>(
    cmd: &mut std::process::Command,
    info: ChildInfo<'_>,
    argv: &[S],
) -> std::io::Result<std::process::ExitStatus> {
    let Some(traced) = child_start(info, argv) else {
        return cmd.status();
    };
    match cmd.spawn() {
        Ok(mut child) => {
            let r = child.wait();
            match &r {
                Ok(s) => traced.exit_status(child.id(), *s),
                Err(_) => traced.exit(i64::from(child.id()), -1),
            }
            r
        }
        Err(e) => {
            traced.exit(-1, -1);
            Err(e)
        }
    }
}

/// `Command::output` with `child_start`/`child_exit` events (the pid is
/// not known and reported as 0).
#[track_caller]
pub fn output<S: AsRef<OsStr>>(
    cmd: &mut std::process::Command,
    info: ChildInfo<'_>,
    argv: &[S],
) -> std::io::Result<std::process::Output> {
    let Some(traced) = child_start(info, argv) else {
        return cmd.output();
    };
    let r = cmd.output();
    match &r {
        Ok(o) => traced.exit(0, status_code(o.status)),
        Err(_) => traced.exit(-1, -1),
    }
    r
}

/// `git` followed by `args`, the argument vector git reports for a git
/// child process.
pub fn git_argv<S: AsRef<OsStr>>(args: &[S]) -> Vec<String> {
    std::iter::once("git".to_string())
        .chain(
            args.iter()
                .map(|a| a.as_ref().to_string_lossy().into_owned()),
        )
        .collect()
}

/// The exit code git reports for a child: its exit status, or 128 plus the
/// signal that killed it.
pub fn status_code(status: std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    status.code().unwrap_or(-1)
}

/// `region_enter`: a region (`category`, `label`, optional `msg`) starts.
/// `repo`: the region belongs to the main repository.
#[track_caller]
pub fn region_enter(category: &str, label: &str, msg: Option<&str>, repo: bool) {
    let loc = Location::caller();
    if !enabled() {
        return;
    }
    emit(loc, |s| {
        let mut ev = Ev::new("region_enter");
        ev.repo = repo.then_some(1);
        ev.t_abs = Some(s.start.elapsed().as_secs_f64());
        ev.category = category.to_string();
        ev.dots = s.regions.len();
        ev.nesting = Some(s.regions.len() + 1);
        ev.perf = match msg {
            Some(m) => format!("label:{label} {m}"),
            None => format!("label:{label}"),
        };
        ev.json.int("nesting", (s.regions.len() + 1) as i64);
        region_fields(&mut ev.json, category, label, msg);
        ev
    });
    with_state(|s| {
        s.regions.push(Region {
            category: category.to_string(),
            label: label.to_string(),
            repo: repo.then_some(1),
            start: Instant::now(),
        });
    });
}

fn region_fields(j: &mut Json, category: &str, label: &str, msg: Option<&str>) {
    if !category.is_empty() {
        j.str("category", category);
    }
    if !label.is_empty() {
        j.str("label", label);
    }
    if let Some(m) = msg {
        j.str("msg", m);
    }
}

/// `region_leave`: the innermost region ends.
#[track_caller]
pub fn region_leave() {
    leave(Location::caller(), None);
}

/// [`region_leave`] with a message of its own.
#[track_caller]
pub fn region_leave_msg(msg: &str) {
    leave(Location::caller(), Some(msg));
}

fn leave(loc: Location, msg: Option<&str>) {
    let Some(Some(r)) = with_state(|s| s.regions.pop()) else {
        return;
    };
    emit(loc, |s| {
        let t_rel = r.start.elapsed().as_secs_f64();
        let mut ev = Ev::new("region_leave");
        ev.repo = r.repo;
        ev.t_abs = Some(s.start.elapsed().as_secs_f64());
        ev.t_rel = Some(t_rel);
        ev.category.clone_from(&r.category);
        ev.dots = s.regions.len();
        ev.nesting = Some(s.regions.len() + 1);
        ev.perf = match msg {
            Some(m) => format!("label:{} {m}", r.label),
            None => format!("label:{}", r.label),
        };
        ev.json
            .float("t_rel", t_rel)
            .int("nesting", (s.regions.len() + 1) as i64);
        region_fields(&mut ev.json, &r.category, &r.label, msg);
        ev
    });
}

/// A region that ends when dropped.
#[must_use]
pub struct RegionGuard(bool);

impl Drop for RegionGuard {
    fn drop(&mut self) {
        if self.0 {
            region_leave();
        }
    }
}

/// [`region_enter`] with a guard that calls [`region_leave`].
#[track_caller]
pub fn region(category: &str, label: &str, repo: bool) -> RegionGuard {
    let on = enabled();
    region_enter(category, label, None, repo);
    RegionGuard(on)
}

/// `data`: a key/value pair in the current region.
#[track_caller]
pub fn data(category: &str, key: &str, value: &str, repo: bool) {
    data_impl(Location::caller(), category, key, value, repo.then_some(1));
}

/// [`data`] for the repository with the trace2 id `repo` (`0` for a
/// repository structure that was never set up).
#[track_caller]
pub fn data_repo_id(category: &str, key: &str, value: &str, repo: Option<u32>) {
    data_impl(Location::caller(), category, key, value, repo);
}

fn data_impl(loc: Location, category: &str, key: &str, value: &str, repo: Option<u32>) {
    emit(loc, |s| {
        let t_abs = s.start.elapsed().as_secs_f64();
        let t_rel = s
            .regions
            .last()
            .map_or(t_abs, |r| r.start.elapsed().as_secs_f64());
        let mut ev = Ev::new("data");
        ev.repo = repo;
        ev.t_abs = Some(t_abs);
        ev.t_rel = Some(t_rel);
        ev.category = category.to_string();
        ev.dots = s.regions.len();
        ev.nesting = Some(s.regions.len() + 1);
        ev.perf = format!("{key}:{value}");
        ev.json
            .float("t_abs", t_abs)
            .float("t_rel", t_rel)
            .int("nesting", (s.regions.len() + 1) as i64)
            .str("category", category)
            .str("key", key)
            .str("value", value);
        ev
    });
}

/// [`data`] with an integer value.
#[track_caller]
pub fn data_int(category: &str, key: &str, value: i64, repo: bool) {
    data_impl(
        Location::caller(),
        category,
        key,
        &value.to_string(),
        repo.then_some(1),
    );
}

/// `exit`: the process is about to end with `code`. [`atexit`] follows
/// after the remaining cleanup (waiting for the pager).
#[track_caller]
pub fn exit(code: i32) {
    let loc = Location::caller();
    if with_state(|s| s.exit_code.replace(code)) != Some(None) {
        return;
    }
    // Regions still open are not closed (git does not either).
    emit(loc, |s| exit_event(s, "exit", code));
}

/// `atexit`: the last event of the process; nothing is written after it.
#[track_caller]
pub fn atexit() {
    let loc = Location::caller();
    let Some(code) = with_state(|s| s.exit_code.unwrap_or(0)) else {
        return;
    };
    stats::summaries(loc);
    emit(loc, |s| exit_event(s, "atexit", code));
    ENABLED.store(false, Ordering::Relaxed);
}

fn exit_event(s: &State, name: &'static str, code: i32) -> Ev {
    let t_abs = s.start.elapsed().as_secs_f64();
    let mut ev = Ev::new(name);
    ev.t_abs = Some(t_abs);
    ev.time_in_brief = name == "atexit";
    ev.dots = if name == "exit" { s.regions.len() } else { 0 };
    ev.normal = Some(format!("elapsed:{} code:{code}", secs(t_abs)));
    ev.perf = format!("code:{code}");
    ev.json.float("t_abs", t_abs).int("code", i64::from(code));
    ev
}

/// [`ChildTrace::exit`] from a signal handler: does nothing when the
/// trace state is busy (the handler interrupted a trace write).
pub fn child_exit_in_signal_handler(child: ChildTrace, pid: i64, code: i32) {
    if STATE.try_lock().is_ok() {
        child.exit(pid, code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction() {
        // Observed from git 2.56.1 (`test-tool trace2 303redact_def_param`).
        for (url, want) in [
            (
                "https://user:pwd@example.com/x",
                "https://user:<REDACTED>@example.com/x",
            ),
            ("http://u:p@h/", "http://u:<REDACTED>@h/"),
            ("https://:p@h/", "https://:<REDACTED>@h/"),
            ("https://u:@h/", "https://u:<REDACTED>@h/"),
            ("https://u:p:q@h/", "https://u:<REDACTED>@h/"),
            ("https://u:p@h", "https://u:<REDACTED>@h"),
            ("https://h?x:y@z", "https://h?x:<REDACTED>@z"),
            ("https://u:p@h#a@b", "https://u:<REDACTED>@h#a@b"),
            ("https://user@h/x", "https://user@h/x"),
            ("https://a@b:p@h/", "https://a@b:p@h/"),
            ("https://u:p/x@h/", "https://u:p/x@h/"),
            ("https://example.com/a@b", "https://example.com/a@b"),
            ("ssh://u:p@h/", "ssh://u:p@h/"),
            ("HTTP://u:p@h/", "HTTP://u:p@h/"),
            ("x https://u:p@h/", "x https://u:p@h/"),
            ("plain", "plain"),
        ] {
            assert_eq!(redact_url(url), want, "{url}");
        }
    }

    #[test]
    fn config_patterns() {
        let p = vec!["core.*".to_string(), "remote.*.url".to_string()];
        assert!(is_config_param(&p, "core.abbrev"));
        assert!(is_config_param(&p, "remote.origin.url"));
        assert!(!is_config_param(&p, "remote.origin.fetch"));
        assert!(!is_config_param(&p, "user.name"));
    }

    #[test]
    fn bools() {
        assert_eq!(parse_bool("1"), Some(true));
        assert_eq!(parse_bool("Yes"), Some(true));
        assert_eq!(parse_bool("0"), Some(false));
        assert_eq!(parse_bool("x"), None);
    }
}
