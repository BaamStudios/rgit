// SPDX-License-Identifier: MIT
//! Progress meters on standard error, as git shows them.
//!
//! Behaviour observed from git 2.56.1 and its test suite (t0500):
//!
//! * `Title: N` counts, `Title:  PP% (N/T)` with a total; the line is
//!   rewritten in place with `\r` and finished with `, done.` and a newline.
//! * With a total the line is redrawn whenever the percentage changes;
//!   otherwise (and for unchanged percentages) at most once a second.
//! * A line that got shorter is padded with spaces to cover the old one.
//! * When `Title: <counters>` would not fit the terminal width, the title is
//!   written on a line of its own and the counters follow on the next lines,
//!   indented by two spaces.
//! * Byte counts and a throughput averaged over the last eight samples can
//!   be appended (`, 1.17 MiB | 400.00 KiB/s`).
//! * Delayed meters (`GIT_PROGRESS_DELAY`, default two seconds) only appear
//!   for operations that take that long.
//! * trace2: each meter is a `progress` region labelled with its title; the
//!   total and the byte count are logged as `total_objects`/`total_bytes`.

pub mod messages;

use std::io::Write;
use std::time::Instant;

use rgit_trace::trace2;

/// Seconds to nanoseconds.
const NS: u64 = 1_000_000_000;
/// Samples the throughput is averaged over.
const TP_SAMPLES: usize = 8;

/// The terminal width for line breaking: `COLUMNS`, else the width of the
/// terminal on standard error, else 80.
pub fn term_columns() -> usize {
    if let Some(n) = std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
    {
        return n;
    }
    #[cfg(unix)]
    {
        // SAFETY: TIOCGWINSZ fills the winsize struct we own; an fd that is
        // not a terminal makes the call fail and the struct is ignored.
        unsafe {
            let mut ws: libc::winsize = std::mem::zeroed();
            if libc::ioctl(2, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 {
                return usize::from(ws.ws_col);
            }
        }
    }
    80
}

/// Whether standard error is a terminal (where meters are shown unless
/// asked otherwise).
pub fn stderr_is_terminal() -> bool {
    use std::io::IsTerminal;
    std::io::stderr().is_terminal()
}

/// `GIT_PROGRESS_DELAY` in seconds (default 2).
fn delay_seconds() -> u64 {
    std::env::var("GIT_PROGRESS_DELAY")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(2)
}

#[derive(Debug)]
enum Clock {
    Real(Instant),
    /// Time set by the caller (`test-tool progress`), in nanoseconds.
    Test(u64),
}

/// Transfer rate bookkeeping: the bytes so far, the last measuring point
/// and the recent intervals as (bytes, misecs).
#[derive(Debug, Default)]
struct Throughput {
    bytes: u64,
    mark_bytes: u64,
    mark_ns: u64,
    recent: std::collections::VecDeque<(u64, u64)>,
    display: String,
}

impl Throughput {
    /// KiB/s over the new interval and the remembered ones; the new one is
    /// remembered afterwards (at most [`TP_SAMPLES`]).
    fn sample(&mut self, interval: (u64, u64)) -> u64 {
        let (b, t) = self
            .recent
            .iter()
            .fold(interval, |(b, t), &(rb, rt)| (b + rb, t + rt));
        self.recent.push_back(interval);
        if self.recent.len() > TP_SAMPLES {
            self.recent.pop_front();
        }
        b / t.max(1)
    }
}

/// Nanoseconds to git's "misecs" (1/1024 s), as the meter computes rates.
fn misecs(ns: u64) -> u64 {
    ((u128::from(ns) * 4398) >> 32) as u64
}

/// One progress meter.
#[derive(Debug)]
pub struct Progress {
    title: String,
    total: u64,
    /// Display enabled (else only the trace2 region is written).
    show: bool,
    last_value: Option<u64>,
    last_percent: Option<u64>,
    /// Earliest time (ns) a delayed meter may appear.
    delay_until: Option<u64>,
    /// A redraw is due (the once-a-second timer fired).
    update: bool,
    last_tick: u64,
    split: bool,
    last_len: usize,
    clock: Clock,
    start_ns: u64,
    throughput: Option<Throughput>,
    columns: usize,
    traced: bool,
    stopped: bool,
}

impl Progress {
    fn new(title: &str, total: u64, show: bool, delay: Option<u64>, clock: Clock) -> Progress {
        let traced = trace2::enabled();
        if traced {
            trace2::region_enter("progress", title, None, true);
        }
        let mut p = Progress {
            title: title.to_string(),
            total,
            show,
            last_value: None,
            last_percent: None,
            delay_until: None,
            update: false,
            last_tick: 0,
            split: false,
            last_len: 0,
            clock,
            start_ns: 0,
            throughput: None,
            columns: term_columns(),
            traced,
            stopped: false,
        };
        p.start_ns = p.now();
        p.last_tick = p.start_ns;
        p.delay_until = delay.map(|d| p.start_ns + d * NS);
        p
    }

    /// A meter shown at once when `show` (`total` 0: no total).
    pub fn start(title: &str, total: u64, show: bool) -> Progress {
        Progress::new(title, total, show, None, Clock::Real(Instant::now()))
    }

    /// A meter that only appears after `GIT_PROGRESS_DELAY` seconds.
    pub fn start_delayed(title: &str, total: u64, show: bool) -> Progress {
        Progress::new(
            title,
            total,
            show,
            Some(delay_seconds()),
            Clock::Real(Instant::now()),
        )
    }

    /// A meter on a clock set with [`Progress::set_test_time`] (for
    /// `test-tool progress`), starting at `now_ns`; no automatic redraws,
    /// no delay.
    pub fn start_for_test(title: &str, total: u64, now_ns: u64) -> Progress {
        Progress::new(title, total, true, None, Clock::Test(now_ns))
    }

    /// Set the test clock (nanoseconds since the start).
    pub fn set_test_time(&mut self, ns: u64) {
        if let Clock::Test(t) = &mut self.clock {
            *t = ns;
        }
    }

    /// Ask for a redraw at the next update, as git's once-a-second timer
    /// does.
    pub fn request_update(&mut self) {
        self.update = true;
    }

    fn now(&self) -> u64 {
        match &self.clock {
            Clock::Real(start) => start.elapsed().as_nanos() as u64,
            Clock::Test(t) => *t,
        }
    }

    fn tick(&mut self) {
        if let Clock::Real(_) = self.clock {
            let now = self.now();
            if now.saturating_sub(self.last_tick) >= NS {
                self.last_tick = now;
                self.update = true;
            }
        }
    }

    /// `n` items are done.
    pub fn set(&mut self, n: u64) {
        self.tick();
        self.display(n, false);
    }

    /// One more item is done.
    pub fn inc(&mut self) {
        let n = self.last_value.unwrap_or(0) + 1;
        self.set(n);
    }

    /// The total changed (counted while running).
    pub fn set_total(&mut self, total: u64) {
        self.total = total;
    }

    /// `total` bytes were transferred so far.
    pub fn throughput(&mut self, total: u64) {
        let now = self.now();
        // The first report only sets the starting point.
        let tp = self.throughput.get_or_insert_with(|| Throughput {
            bytes: total,
            mark_bytes: total,
            mark_ns: now,
            ..Default::default()
        });
        tp.bytes = total;
        // Intervals shorter than half a second are merged into the next.
        let elapsed = misecs(now.saturating_sub(tp.mark_ns));
        if elapsed <= 512 {
            return;
        }
        let delta = total.saturating_sub(tp.mark_bytes);
        (tp.mark_bytes, tp.mark_ns) = (total, now);
        let rate = tp.sample((delta, elapsed));
        tp.display = format!(
            ", {} | {}",
            messages::bytes(total),
            messages::rate(rate * 1024)
        );
        self.tick();
        if self.update {
            if let Some(n) = self.last_value {
                self.display(n, false);
            }
        }
    }

    fn display(&mut self, n: u64, done: bool) {
        if !done {
            if let Some(until) = self.delay_until {
                if self.now() < until {
                    return;
                }
                self.delay_until = None;
                // A delayed meter appears with its current state.
                self.update = true;
            }
        }
        self.last_value = Some(n);
        let pct = n.saturating_mul(100).checked_div(self.total);
        if !done && !self.update && (pct.is_none() || self.last_percent == pct) {
            return;
        }
        self.last_percent = pct;
        let mut counters = self.counters(n);
        self.update = false;
        if !self.show {
            return;
        }
        if let Some(tp) = &self.throughput {
            counters.push_str(&tp.display);
        }
        let eol = if done {
            format!("{}\n", messages::DONE)
        } else {
            "\r".to_string()
        };
        let pad = " ".repeat(self.last_len.saturating_sub(counters.len()));
        let title_len = self.title.chars().count();
        let line = if self.split {
            format!("  {counters}{pad}{eol}")
        } else if title_len + 2 + counters.len() > self.columns {
            self.split = true;
            let cover = " ".repeat(self.columns.saturating_sub(title_len + 1));
            format!("{}:{cover}\n  {counters}{eol}", self.title)
        } else {
            format!("{}: {counters}{pad}{eol}", self.title)
        };
        self.last_len = counters.len();
        let mut err = std::io::stderr().lock();
        let _ = err.write_all(line.as_bytes());
        let _ = err.flush();
    }

    /// Finish the meter: the last state with `, done.` (when anything was
    /// counted) and the trace2 summary.
    pub fn stop(mut self) {
        self.finish(None);
    }

    /// [`Progress::stop`] with a different final message (`, done.` is
    /// replaced by `, <msg>.`).
    pub fn stop_with(mut self, msg: &str) {
        self.finish(Some(msg));
    }

    fn finish(&mut self, msg: Option<&str>) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        if self.traced {
            trace2::data_int("progress", "total_objects", self.total as i64, true);
            if let Some(tp) = &self.throughput {
                trace2::data_int("progress", "total_bytes", tp.bytes as i64, true);
            }
            trace2::region_leave();
        }
        let Some(n) = self.last_value else {
            return;
        };
        let ms = misecs(self.now().saturating_sub(self.start_ns)).max(1);
        if let Some(tp) = &mut self.throughput {
            let rate = tp.bytes / ms;
            tp.display = format!(
                ", {} | {}",
                messages::bytes(tp.bytes),
                messages::rate(rate * 1024)
            );
        }
        match msg {
            None => self.display(n, true),
            Some(m) => self.display_final(n, m),
        }
    }

    /// `N`, or `PPP% (N/T)` with a total.
    fn counters(&self, n: u64) -> String {
        match n.saturating_mul(100).checked_div(self.total) {
            Some(pct) => format!("{pct:3}% ({n}/{})", self.total),
            None => n.to_string(),
        }
    }

    fn display_final(&mut self, n: u64, msg: &str) {
        let mut counters = self.counters(n);
        if !self.show {
            return;
        }
        if let Some(tp) = &self.throughput {
            counters.push_str(&tp.display);
        }
        let pad = " ".repeat(self.last_len.saturating_sub(counters.len()));
        let line = if self.split {
            format!("  {counters}{pad}, {msg}.\n")
        } else {
            format!("{}: {counters}{pad}, {msg}.\n", self.title)
        };
        let _ = std::io::stderr().lock().write_all(line.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn misecs_conversion() {
        assert_eq!(misecs(NS), 1023);
        assert_eq!(misecs(3 * NS), 3071);
    }
}
