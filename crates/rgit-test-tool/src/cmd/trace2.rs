// SPDX-License-Identifier: MIT

//! `trace2` (t0210, t0211, t0212, t0213): drive rgit's trace2 events.
//!
//! Every verb runs as a traced process (`version`, `start`, `cmd_ancestry`,
//! `cmd_name trace2`, the `def_param` events, then the verb's events, then
//! `exit` / `atexit`); the verbs and their outputs are observed from the
//! reference binary (`test-tool trace2` lists them):
//!
//! - `001return <code>`, `002exit <code>`: end with `<code>` (modulo 256)
//! - `003error <msg>...`: `error: <msg>` and an `error` event (format `%s`)
//!   for each message
//! - `004child [<cmd>...]`: run `<cmd>` as a traced child process, exit with
//!   its code
//! - `005exec <args>...`: an `exec` event for `git <args>`, then exec it
//! - `006data (<category> <key> <value>)...`: `data` events (repository 0)
//! - `007bug` ... `010bug_BUG`: `BUG()` / `bug()` reports, exit code 99
//! - `100timer <count> <ms>`: time `<count>` sleeps with timer `test/test1`
//! - `101timer <count> <ms> <threads>`: the same in threads (`test/test2`,
//!   per-thread summaries)
//! - `200counter <v>...`: add to counter `test/test1`
//! - `201counter <v1> <v2> <threads>`: add both in each thread to the
//!   per-thread counter `test/test2`
//! - `300redact_start <argv>...`, `301redact_child_start <argv>...`,
//!   `302redact_exec <exe> <argv>...`, `303redact_def_param <key> <value>`:
//!   one `start` / `child_start` / `exec` / `def_param` event with that
//!   content (URL passwords redacted)
//! - `400ancestry <target> <file> <cmd>...`: run `<cmd>` with only the trace2
//!   target `<target>` (`normal`, `perf`, `event`) going to `<file>`
//!   (`GIT_TRACE2_BRIEF=1`)
//!
//! Usage problems are `fatal: expect ...` (exit 128); no or an unknown verb
//! prints the usage (exit 129).

use std::panic::Location;
use std::process::Command;
use std::time::Duration;

use rgit_config::sources::{ScopeFilter, load_standard};
use rgit_config::{CommandLine, ConfigSet, LoadOptions, SystemEnv};
use rgit_trace::trace2::{self, ChildInfo, Counter, Timer};

use crate::messages;
use crate::repo;

static TIMER_1: Timer = Timer {
    category: "test",
    name: "test1",
    per_thread: false,
};
static TIMER_2: Timer = Timer {
    category: "test",
    name: "test2",
    per_thread: true,
};
static COUNTER_1: Counter = Counter {
    category: "test",
    name: "test1",
    per_thread: false,
};
static COUNTER_2: Counter = Counter {
    category: "test",
    name: "test2",
    per_thread: true,
};

/// Exit code of `BUG()`.
const BUG_EXIT: i32 = 99;

/// How a verb ends.
enum End {
    /// Return this code.
    Code(i32),
    /// `fatal:` with a message (and its trace2 format string).
    Fatal(String, &'static str),
    /// Print the usage.
    Usage,
}

pub fn trace2(args: &[String]) -> i32 {
    // The reference binary is called `test-tool`, and t0213 looks for that
    // name among the parent processes of the traced children.
    #[cfg(target_os = "linux")]
    let _ = std::fs::write("/proc/self/comm", "test-tool");

    let argv0 = std::env::args()
        .next()
        .unwrap_or_else(|| "test-tool".into());
    let mut argv = vec![argv0, "trace2".to_string()];
    argv.extend(args.iter().cloned());
    let settings = settings_config();
    trace2::init(&argv, &rgit_version::version(), &|key| {
        settings
            .get(key)
            .map(|e| e.value.as_ref().map_or_else(String::new, |v| v.to_string()))
    });
    trace2::cmd_name("trace2");
    list_params();

    let mut bugs = Bugs::default();
    let end = match args.split_first() {
        None => End::Usage,
        Some((verb, rest)) => run(verb, rest, &mut bugs),
    };
    let code = match end {
        End::Code(c) => c,
        End::Fatal(msg, fmt) => {
            eprintln!("fatal: {msg}");
            trace2::error_fmt(&msg, fmt);
            128
        }
        End::Usage => {
            eprint!("{}", messages::TRACE2_USAGE);
            129
        }
    };
    // bug() without BUG_if_bug() is reported when the process exits.
    let code = if bugs.pending {
        bug_report(
            Location::caller(),
            messages::TRACE2_BUG_UNCHECKED,
            messages::TRACE2_BUG_UNCHECKED,
        );
        BUG_EXIT
    } else {
        code
    };
    let code = code & 0xff;
    trace2::exit(code);
    trace2::atexit();
    code
}

fn run(verb: &str, args: &[String], bugs: &mut Bugs) -> End {
    match verb {
        "001return" | "002exit" => match args.first().and_then(|a| int(a)) {
            Some(n) => End::Code((n & 0xff) as i32),
            None => fatal(messages::TRACE2_EXPECT_EXIT_CODE),
        },
        "003error" => {
            if args.is_empty() {
                return fatal(messages::TRACE2_EXPECT_ERROR);
            }
            for a in args {
                eprintln!("error: {a}");
                trace2::error_fmt(a, "%s");
            }
            End::Code(0)
        }
        "004child" => End::Code(match args.first() {
            None => 0,
            Some(_) => run_child(args, None),
        }),
        "005exec" => exec_git(args),
        "006data" => {
            if args.is_empty() || args.len() % 3 != 0 {
                return fatal_s(messages::TRACE2_EXPECT_DATA);
            }
            for t in args.chunks(3) {
                trace2::data_repo_id(&t[0], &t[1], &t[2], Some(0));
            }
            End::Code(0)
        }
        "007bug" => {
            bug_report(
                Location::caller(),
                messages::TRACE2_BUG_007,
                messages::TRACE2_BUG_007,
            );
            End::Code(BUG_EXIT)
        }
        "008bug" => {
            bugs.bug(messages::TRACE2_BUG_A, messages::TRACE2_BUG_A);
            bugs.bug(messages::TRACE2_BUG_ANOTHER, messages::TRACE2_BUG_ANOTHER);
            bugs.bug(messages::TRACE2_BUG_008_LAST, messages::TRACE2_BUG_008_LAST);
            // BUG_if_bug(): the reports were made, exit quietly.
            bugs.pending = false;
            End::Code(BUG_EXIT)
        }
        "009bug_BUG" => {
            bugs.bug(messages::TRACE2_BUG_A, messages::TRACE2_BUG_A);
            bugs.bug(messages::TRACE2_BUG_ANOTHER, messages::TRACE2_BUG_ANOTHER);
            End::Code(0)
        }
        "010bug_BUG" => {
            bugs.bug(messages::TRACE2_BUG_A, messages::TRACE2_BUG_010_FMT);
            // BUG() after bug(): its own report, no "on exit()" one.
            bugs.pending = false;
            bug_report(
                Location::caller(),
                &messages::TRACE2_BUG_010_FMT.replace("%s", "BUG"),
                messages::TRACE2_BUG_010_FMT,
            );
            End::Code(BUG_EXIT)
        }
        "100timer" => {
            let [Some(count), Some(ms)] = [0, 1].map(|i| args.get(i).and_then(|a| int(a))) else {
                return fatal_s(messages::TRACE2_EXPECT_TIMER);
            };
            if args.len() != 2 {
                return fatal_s(messages::TRACE2_EXPECT_TIMER);
            }
            sleep_loop(&TIMER_1, count, ms);
            End::Code(0)
        }
        "101timer" => {
            let [Some(count), Some(ms), Some(threads)] =
                [0, 1, 2].map(|i| args.get(i).and_then(|a| int(a)))
            else {
                return fatal_s(messages::TRACE2_EXPECT_TH_TIMER);
            };
            if args.len() != 3 {
                return fatal_s(messages::TRACE2_EXPECT_TH_TIMER);
            }
            in_threads(threads, "ut_101", move || sleep_loop(&TIMER_2, count, ms));
            End::Code(0)
        }
        "200counter" => {
            if args.is_empty() {
                return fatal_s(messages::TRACE2_EXPECT_COUNTER);
            }
            let mut values = Vec::new();
            for a in args {
                match int(a) {
                    Some(v) => values.push(v),
                    None => {
                        return End::Fatal(
                            messages::trace2_invalid_value(a),
                            messages::TRACE2_INVALID_VALUE_FMT,
                        );
                    }
                }
            }
            for v in values {
                COUNTER_1.add(v as u64);
            }
            End::Code(0)
        }
        "201counter" => {
            let [Some(v1), Some(v2), Some(threads)] =
                [0, 1, 2].map(|i| args.get(i).and_then(|a| int(a)))
            else {
                return fatal_s(messages::TRACE2_EXPECT_TH_COUNTER);
            };
            if args.len() != 3 {
                return fatal_s(messages::TRACE2_EXPECT_TH_COUNTER);
            }
            in_threads(threads, "ut_201", move || {
                COUNTER_2.add(v1 as u64);
                COUNTER_2.add(v2 as u64);
            });
            End::Code(0)
        }
        "300redact_start" => {
            if args.is_empty() {
                return fatal(messages::TRACE2_EXPECT_ARGV);
            }
            trace2::start(args);
            End::Code(0)
        }
        "301redact_child_start" => {
            if args.is_empty() {
                return fatal(messages::TRACE2_EXPECT_ARGV);
            }
            // Only the start is reported; nothing is run.
            let _ = trace2::child_start(ChildInfo::default(), args);
            End::Code(0)
        }
        "302redact_exec" => {
            let Some((exe, rest)) = args.split_first() else {
                return fatal(messages::TRACE2_EXPECT_EXEC);
            };
            trace2::exec(exe, rest);
            End::Code(0)
        }
        "303redact_def_param" => {
            if args.len() < 2 {
                return fatal(messages::TRACE2_EXPECT_PARAM);
            }
            trace2::def_param("unknown", &args[0], &args[1]);
            End::Code(0)
        }
        "400ancestry" => {
            if args.len() < 3 {
                return fatal(messages::TRACE2_EXPECT_ANCESTRY);
            }
            let var = match args[0].as_str() {
                "normal" => "GIT_TRACE2",
                "perf" => "GIT_TRACE2_PERF",
                "event" => "GIT_TRACE2_EVENT",
                t => {
                    return End::Fatal(
                        messages::trace2_invalid_target(t),
                        messages::TRACE2_INVALID_TARGET_FMT,
                    );
                }
            };
            End::Code(run_child(&args[2..], Some((var, &args[1]))))
        }
        _ => End::Usage,
    }
}

/// `fatal: <msg>`, a fixed text (its own trace2 format string).
fn fatal(msg: &'static str) -> End {
    End::Fatal(msg.to_string(), msg)
}

/// `fatal: <msg>` printed through `%s` (observed for the data, timer and
/// counter verbs).
fn fatal_s(msg: &str) -> End {
    End::Fatal(msg.to_string(), "%s")
}

/// An integer as `strtol` reads a whole argument (leading blanks and a
/// sign allowed, nothing after the digits).
fn int(s: &str) -> Option<i64> {
    let t = s.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse::<i64>().ok()
}

fn sleep_loop(timer: &'static Timer, count: i64, ms: i64) {
    for _ in 0..count.max(0) {
        timer.start();
        std::thread::sleep(Duration::from_millis(ms.max(0) as u64));
        timer.stop();
    }
}

/// Run `work` in `n` threads announced as `label`, then wait for them.
fn in_threads(n: i64, label: &'static str, work: impl Fn() + Send + Copy + 'static) {
    let handles: Vec<_> = (0..n.max(0))
        .map(|_| {
            std::thread::spawn(move || {
                trace2::thread_start(label);
                work();
                trace2::thread_exit();
            })
        })
        .collect();
    for h in handles {
        let _ = h.join();
    }
}

/// Run `cmd` as a traced child process (class `?`) and return its exit code
/// (255 when it cannot be started). `trace`: the only trace2 target of the
/// child and its destination.
fn run_child(cmd: &[String], trace: Option<(&str, &String)>) -> i32 {
    let mut c = Command::new(&cmd[0]);
    c.args(&cmd[1..]);
    if let Some((var, file)) = trace {
        for v in ["GIT_TRACE2", "GIT_TRACE2_PERF", "GIT_TRACE2_EVENT"] {
            c.env(v, "");
        }
        c.env(var, file).env("GIT_TRACE2_BRIEF", "1");
    }
    let traced = trace2::child_start(ChildInfo::default(), cmd);
    match c.spawn() {
        Ok(mut child) => {
            let pid = child.id();
            match child.wait() {
                Ok(status) => {
                    let code = trace2::status_code(status);
                    if let Some(t) = traced {
                        t.exit(i64::from(pid), code);
                    }
                    code
                }
                Err(_) => {
                    if let Some(t) = traced {
                        t.exit(i64::from(pid), -1);
                    }
                    255
                }
            }
        }
        Err(e) => {
            let (msg, fmt) = messages::trace2_cannot_run(&cmd[0], &strerror(&e));
            eprintln!("error: {msg}");
            trace2::error_fmt(&msg, &fmt);
            if let Some(t) = traced {
                t.exit(-1, -1);
            }
            255
        }
    }
}

/// The system's text for an I/O error, without Rust's ` (os error N)`.
fn strerror(e: &std::io::Error) -> String {
    rgit_fs::messages::strerror(e)
}

/// `005exec`: replace this process by `git <args>`.
fn exec_git(args: &[String]) -> End {
    let mut argv = vec!["git".to_string()];
    argv.extend(args.iter().cloned());
    trace2::exec("git", &argv);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let e = Command::new("git").args(args).exec();
        let (msg, fmt) = messages::trace2_cannot_run("git", &strerror(&e));
        eprintln!("error: {msg}");
        trace2::error_fmt(&msg, &fmt);
        End::Code(-1)
    }
    #[cfg(not(unix))]
    {
        let code = Command::new("git")
            .args(args)
            .status()
            .map(trace2::status_code)
            .unwrap_or(-1);
        End::Code(code)
    }
}

/// `bug()` calls that still need `BUG_if_bug()`.
#[derive(Default)]
struct Bugs {
    pending: bool,
}

impl Bugs {
    #[track_caller]
    fn bug(&mut self, msg: &str, fmt: &str) {
        let msg = if fmt.contains("%s") {
            fmt.replace("%s", "bug")
        } else {
            msg.to_string()
        };
        bug_report(Location::caller(), &msg, fmt);
        self.pending = true;
    }
}

/// `BUG: <file>:<line>: <msg>` on standard error and an `error` event.
fn bug_report(loc: &Location, msg: &str, fmt: &str) {
    eprintln!("BUG: {}:{}: {msg}", loc.file(), loc.line());
    trace2::error_fmt(msg, fmt);
}

/// The system and global configuration, where the `trace2.*` settings
/// come from.
fn settings_config() -> ConfigSet {
    let filter = ScopeFilter {
        local: false,
        worktree: false,
        command: false,
        ..ScopeFilter::default()
    };
    load_config(None, filter)
}

/// All configuration (and that of the repository the current directory is
/// in), for the `def_param` events.
fn list_params() {
    if !trace2::enabled() {
        return;
    }
    let patterns = trace2::config_param_patterns();
    let mut params = Vec::new();
    if !patterns.is_empty() {
        let found = repo::discover(false).ok();
        let set = load_config(found.as_ref(), ScopeFilter::default());
        for e in set.entries() {
            let key = e.name.to_string();
            if trace2::is_config_param(&patterns, &key) {
                let value = e.value.as_ref().map_or_else(String::new, |v| v.to_string());
                params.push((key, value, e.source.scope.as_str().to_string()));
            }
        }
    }
    trace2::def_params(&params);
}

fn load_config(found: Option<&repo::Repo>, filter: ScopeFilter) -> ConfigSet {
    let env = SystemEnv::default();
    let facts = found.map(|r| super::config::facts(r, false));
    let opts = LoadOptions {
        env: &env,
        repo: facts.as_ref().map(|f| f as &dyn rgit_config::RepoFacts),
        includes: true,
    };
    let repo_config = found.map(|r| rgit_config::sources::RepoConfig::new(r.abs_git_dir.clone()));
    let mut set = ConfigSet::new();
    // Unreadable files only mean there are no settings from them.
    let _ = load_standard(
        opts,
        repo_config.as_ref(),
        &CommandLine::default(),
        filter,
        &mut set,
    );
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers() {
        assert_eq!(int("12"), Some(12));
        assert_eq!(int("-1"), Some(-1));
        assert_eq!(int(" +3"), Some(3));
        assert_eq!(int("1x"), None);
        assert_eq!(int(""), None);
        assert_eq!(int("-"), None);
    }
}
