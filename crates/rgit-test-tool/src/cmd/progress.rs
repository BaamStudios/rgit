// SPDX-License-Identifier: MIT

//! `progress` (t0500): drive a progress meter from commands on standard
//! input, one per line:
//!
//! - `start <total> [<title>]`: a new meter (title `Working hard`; a meter
//!   still running is left as it is)
//! - `progress <n>`: `n` items are done
//! - `throughput <bytes> <milliseconds>`: set the clock and report bytes
//! - `update`: redraw at the next change, as the once-a-second timer does
//! - `stop`: finish the meter
//!
//! Observed from the reference binary and t0500: the meter writes to
//! standard error, on a clock that only the `throughput` lines move, and
//! trace2 (`GIT_TRACE2_EVENT`) gets its `progress` region.

use std::io::BufRead;

use rgit_progress::Progress;
use rgit_trace::trace2;

use crate::messages;

const DEFAULT_TITLE: &str = "Working hard";

pub fn progress(args: &[String]) -> i32 {
    if !args.is_empty() {
        eprintln!("{}\n", messages::PROGRESS_USAGE);
        return 129;
    }
    let mut argv = vec!["test-tool".to_string(), "progress".to_string()];
    argv.extend(args.iter().cloned());
    trace2::init(&argv, &rgit_version::version(), &|_| None);
    trace2::cmd_name("progress");
    let mut meter: Option<Progress> = None;
    let mut test_ns = 0u64;
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let mut words = line.splitn(3, ' ');
        let cmd = words.next().unwrap_or("");
        match cmd {
            "start" => {
                let total = words.next().and_then(|w| w.parse().ok()).unwrap_or(0);
                let title = words.next().unwrap_or(DEFAULT_TITLE);
                // A running meter is left as it is (its region stays open).
                meter = Some(Progress::start_for_test(title, total, test_ns));
            }
            "progress" => {
                let n = words.next().and_then(|w| w.parse().ok()).unwrap_or(0);
                if let Some(m) = &mut meter {
                    m.set(n);
                }
            }
            "throughput" => {
                let bytes = words.next().and_then(|w| w.parse().ok()).unwrap_or(0);
                let ms: u64 = words.next().and_then(|w| w.parse().ok()).unwrap_or(0);
                test_ns = ms * 1_000_000;
                if let Some(m) = &mut meter {
                    m.set_test_time(test_ns);
                    m.throughput(bytes);
                }
            }
            "update" => {
                if let Some(m) = &mut meter {
                    m.request_update();
                }
            }
            "stop" => {
                if let Some(m) = meter.take() {
                    m.stop();
                }
            }
            _ => {
                eprintln!("{}", messages::progress_invalid_input(&line));
                trace2::exit(128);
                trace2::atexit();
                return 128;
            }
        }
    }
    // A meter that was not stopped stays open (its region is not left).
    drop(meter);
    trace2::exit(0);
    trace2::atexit();
    0
}
