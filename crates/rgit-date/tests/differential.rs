// SPDX-License-Identifier: MIT
//! Differential tests against the `git` binary (skipped when it is missing).
//!
//! * approxidate: `git rev-parse --since=<s>` with `GIT_TEST_DATE_NOW`
//! * strict parser: `git var GIT_COMMITTER_IDENT` with `GIT_COMMITTER_DATE`
//! * formatting: `git show -s --format=%ad --date=<mode>` of generated commits
//!
//! `RGIT_DATE_DIFF_COUNT` sets the number of generated approxidate strings
//! per time zone (default 1000; the acceptance run of issue #74 uses 10000).

use std::path::PathBuf;
use std::process::{Command, Stdio};

use rgit_date::{DateContext, TimeZone, approxidate, format_raw, parse_date, show_date};

const ZONES: [&str; 3] = ["UTC", "Europe/Berlin", "America/New_York"];

/// Small deterministic generator (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + self.below((hi - lo) as u64) as i64
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len() as u64) as usize]
    }
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const UNITS: [&str; 7] = ["second", "minute", "hour", "day", "week", "month", "year"];
const NUMBER_WORDS: [&str; 10] = [
    "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
];

/// Date strings of the kinds people type: relative phrases, times of day,
/// month names, numeric and ISO dates, RFC 2822, epoch timestamps.
fn phrase(r: &mut Rng) -> String {
    let month = |r: &mut Rng| {
        let m = r.pick(&MONTHS);
        match r.below(4) {
            0 => m.to_string(),
            1 => m[..3].to_string(),
            2 => m.to_lowercase(),
            _ => m[..3].to_lowercase(),
        }
    };
    let day = |r: &mut Rng| {
        let d = r.pick(&DAYS);
        match r.below(3) {
            0 => d.to_string(),
            1 => d[..3].to_string(),
            _ => d.to_lowercase(),
        }
    };
    let ordinal = |n: i64| {
        let suffix = match (n % 10, n % 100) {
            (_, 10..=20) => "th",
            (1, _) => "st",
            (2, _) => "nd",
            (3, _) => "rd",
            _ => "th",
        };
        format!("{n}{suffix}")
    };
    let year = |r: &mut Rng| r.range(1971, 2099).to_string();
    let tod = |r: &mut Rng| {
        let (h, m, s) = (r.range(0, 24), r.range(0, 60), r.range(0, 60));
        match r.below(5) {
            0 => format!("{h:02}:{m:02}"),
            1 => format!("{h:02}:{m:02}:{s:02}"),
            2 => format!(
                "{}{}",
                r.range(1, 13),
                r.pick(&["am", "pm", "AM", "PM", " am", " pm"])
            ),
            3 => format!("{}:{m:02}{}", r.range(1, 13), r.pick(&["am", "pm", " PM"])),
            _ => r.pick(&["noon", "midnight", "tea"]).to_string(),
        }
    };
    let zone = |r: &mut Rng| {
        r.pick(&[
            "+0000", "+0100", "+0200", "-0500", "-0700", "+0530", "UTC", "GMT", "EST", "PDT",
            "CEST", "Z", "+01:00", "-08:00",
        ])
        .to_string()
    };
    let rel = |r: &mut Rng| {
        let n = r.range(1, 40).to_string();
        let num = if r.below(2) == 0 {
            n
        } else {
            r.pick(&NUMBER_WORDS).to_string()
        };
        let unit = r.pick(&UNITS);
        let plural = if num == "1" || num == "one" {
            r.pick(&["", "s"])
        } else {
            "s"
        };
        match r.below(3) {
            0 => format!("{num} {unit}{plural} ago"),
            1 => format!("{num}.{unit}{plural}.ago"),
            _ => format!("{num} {unit}{plural}"),
        }
    };
    let absdate = |r: &mut Rng| {
        let (y, m, d) = (year(r), r.range(1, 13), r.range(1, 29));
        match r.below(9) {
            0 => format!("{y}-{m:02}-{d:02}"),
            1 => format!("{m}/{d}/{y}"),
            2 => format!("{d}.{m}.{y}"),
            3 => format!("{} {d}, {y}", month(r)),
            4 => format!("{d} {} {y}", month(r)),
            5 => format!("{} {}", month(r), ordinal(d)),
            6 => format!("{} {}", ordinal(d), month(r)),
            7 => format!("{y}.{m:02}.{d:02}"),
            _ => format!("{y}/{m:02}/{d:02}"),
        }
    };
    match r.below(12) {
        0 => rel(r),
        1 => format!("{} {}", rel(r), rel(r)),
        2 => format!("last {}", day(r)),
        3 => format!(
            "{} {} {}{}",
            r.pick(&["last", "2", "3", "two"]),
            day(r),
            r.pick(&["", "at "]),
            tod(r)
        ),
        4 => r
            .pick(&[
                "now",
                "today",
                "yesterday",
                "noon",
                "midnight",
                "tea",
                "never",
                "noon yesterday",
                "midnight today",
                "yesterday noon",
                "today noon",
                "last week",
                "last month",
                "last year",
            ])
            .to_string(),
        5 => format!("{} {}", tod(r), r.pick(&["yesterday", "today", ""]))
            .trim()
            .to_string(),
        6 => format!(
            "{} {}{}",
            r.pick(&["yesterday", "today"]),
            r.pick(&["at ", ""]),
            tod(r)
        ),
        7 => absdate(r),
        8 => match r.below(5) {
            0 => format!("{} {}", absdate(r), tod(r)),
            1 => format!("{} {} {}", absdate(r), tod(r), zone(r)),
            2 => format!(
                "{}-{:02}-{:02}T{:02}:{:02}:{:02}{}",
                year(r),
                r.range(1, 13),
                r.range(1, 29),
                r.range(0, 24),
                r.range(0, 60),
                r.range(0, 60),
                r.pick(&["", "Z", "+02:00", "-0500"])
            ),
            3 => {
                let t = r.range(100_000_000, 4_000_000_000);
                match r.below(4) {
                    0 => format!("@{t}"),
                    1 => format!("{t} {}", zone(r)),
                    2 => format!("@{t} {}", zone(r)),
                    _ => t.to_string(),
                }
            }
            _ => format!(
                "{}, {} {} {} {:02}:{:02}:{:02} {}",
                &day(r)[..3],
                r.range(1, 29),
                &month(r)[..3],
                year(r),
                r.range(0, 24),
                r.range(0, 60),
                r.range(0, 60),
                zone(r)
            ),
        },
        9 => format!("{} {}", absdate(r), rel(r)),
        10 => format!(
            "{} {}",
            month(r),
            if r.below(2) == 0 {
                year(r)
            } else {
                String::new()
            }
        )
        .trim()
        .to_string(),
        _ => format!("{} {}", tod(r), rel(r)),
    }
}

struct Git {
    dir: PathBuf,
}

impl Git {
    fn new(name: &str) -> Option<Git> {
        let ok = Command::new("git")
            .arg("--version")
            .stdout(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            eprintln!("git not found, skipping differential test");
            return None;
        }
        let dir = std::env::temp_dir().join(format!("rgit-date-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = Git { dir };
        git.cmd(&["init", "-q"], &[]);
        Some(git)
    }

    fn command(&self, args: &[&str], env: &[(&str, &str)]) -> Command {
        let mut c = Command::new("git");
        c.arg("-C").arg(&self.dir).args(args);
        c.env("LC_ALL", "C")
            .env_remove("GIT_TEST_DATE_NOW")
            .env_remove("GIT_COMMITTER_DATE")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("HOME", &self.dir)
            .env("XDG_CONFIG_HOME", self.dir.join(".config"));
        for (k, v) in env {
            c.env(k, v);
        }
        c
    }

    fn cmd(&self, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
        self.command(args, env).output().expect("run git")
    }

    fn cmd_stdin(&self, args: &[&str], env: &[(&str, &str)], input: &str) -> String {
        use std::io::Write;
        let mut child = self
            .command(args, env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("run git");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "git {args:?} failed");
        String::from_utf8(out.stdout).unwrap()
    }
}

impl Drop for Git {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn count() -> usize {
    std::env::var("RGIT_DATE_DIFF_COUNT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000)
}

#[test]
fn approxidate_matches_git() {
    let Some(git) = Git::new("approx") else {
        return;
    };
    let mut rng = Rng(0x5eed_1234_abcd_0001);
    let mut failures = Vec::new();
    let mut total = 0;
    for (i, zone) in ZONES.iter().enumerate() {
        let now = [1251660000i64, 1262390000, 1782000000][i];
        let ctx = DateContext::new(now, TimeZone::get(zone).unwrap());
        let inputs: Vec<String> = (0..count()).map(|_| phrase(&mut rng)).collect();
        for chunk in inputs.chunks(500) {
            let mut args = vec!["rev-parse".to_string()];
            args.extend(chunk.iter().map(|s| format!("--since={s}")));
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let now_s = now.to_string();
            let out = git.cmd(&args, &[("GIT_TEST_DATE_NOW", &now_s), ("TZ", zone)]);
            let stdout = String::from_utf8(out.stdout).unwrap();
            let lines: Vec<&str> = stdout.lines().collect();
            assert_eq!(lines.len(), chunk.len());
            for (input, line) in chunk.iter().zip(lines) {
                let want: u64 = line.trim_start_matches("--max-age=").parse().unwrap();
                let got = approxidate(input, &ctx);
                total += 1;
                if got != want {
                    failures.push(format!("{zone} {input:?}: git {want}, rgit {got}"));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {total} differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn strict_parser_matches_git() {
    let Some(git) = Git::new("strict") else {
        return;
    };
    let mut rng = Rng(0x5eed_1234_abcd_0002);
    let mut failures = Vec::new();
    for zone in ["UTC", "Europe/Berlin"] {
        let ctx = DateContext::new(rgit_date::now_from_env(), TimeZone::get(zone).unwrap());
        for _ in 0..(count() / 10).max(50) {
            let input = phrase(&mut rng);
            let out = git.cmd(
                &["var", "GIT_COMMITTER_IDENT"],
                &[
                    ("GIT_COMMITTER_DATE", &input),
                    ("GIT_COMMITTER_NAME", "A"),
                    ("GIT_COMMITTER_EMAIL", "a@b"),
                    ("TZ", zone),
                ],
            );
            let want = out.status.success().then(|| {
                let s = String::from_utf8(out.stdout).unwrap();
                s.trim().rsplit_once("> ").unwrap().1.to_string()
            });
            let got = parse_date(&input, &ctx).map(|(t, tz)| format_raw(t, tz));
            if got != want {
                failures.push(format!("{zone} {input:?}: git {want:?}, rgit {got:?}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn formats_match_git() {
    let Some(git) = Git::new("format") else {
        return;
    };
    let mut rng = Rng(0x5eed_1234_abcd_0003);
    let now: i64 = 1600000000;
    let offsets = [
        0, 100, 200, -500, -430, 530, 545, 1245, -1100, 1400, -1200, 99, -30,
    ];
    let mut dates: Vec<(u64, i32)> = (0..200)
        .map(|_| {
            let t = match rng.below(3) {
                0 => rng.range(0, 4_100_000_000),
                1 => now - rng.range(0, 400_000_000),
                _ => now - rng.range(-200_000, 3_000_000),
            };
            let tz = offsets[rng.below(offsets.len() as u64) as usize];
            (t as u64, tz)
        })
        .collect();
    // Keep only dates that are not before the epoch in their own zone (git
    // dies on those) and drop duplicates (git shows each commit once).
    dates.retain(|&(t, tz)| t as i64 + rgit_date::tz_to_minutes(tz) * 60 >= 0);
    dates.sort();
    dates.dedup();

    let tree = git.cmd_stdin(&["hash-object", "-t", "tree", "-w", "--stdin"], &[], "");
    let tree = tree.trim();
    let commits: Vec<String> = dates
        .iter()
        .map(|&(t, tz)| {
            let raw = format_raw(t, tz);
            let body = format!("tree {tree}\nauthor A <a@b> {raw}\ncommitter A <a@b> {raw}\n\nm\n");
            git.cmd_stdin(
                &[
                    "hash-object",
                    "-t",
                    "commit",
                    "-w",
                    "--literally",
                    "--stdin",
                ],
                &[],
                &body,
            )
            .trim()
            .to_string()
        })
        .collect();

    let modes = [
        "default",
        "local",
        "iso",
        "iso-local",
        "iso-strict",
        "iso-strict-local",
        "rfc",
        "rfc-local",
        "short",
        "short-local",
        "raw",
        "raw-local",
        "unix",
        "relative",
        "human",
        "human-local",
        "format:%c %a %A %b %B %d %e %H %I %j %k %l %m %M %p %S %U %u %V %w %W %y %Y %z %Z %%",
        "format-local:%c %z %Z %T %D %F %R %r",
    ];
    let mut failures = Vec::new();
    for zone in ["UTC", "Europe/Berlin", "Asia/Kolkata"] {
        let ctx = DateContext::new(now, TimeZone::get(zone).unwrap());
        for mode in modes {
            let date_arg = format!("--date={mode}");
            let mut args = vec![
                "show",
                "-s",
                "--no-walk=unsorted",
                "--format=%ad%x00",
                &date_arg,
            ];
            args.extend(commits.iter().map(String::as_str));
            let now_s = now.to_string();
            let out = git.cmd(&args, &[("GIT_TEST_DATE_NOW", &now_s), ("TZ", zone)]);
            assert!(out.status.success(), "git show --date={mode} failed");
            let stdout = String::from_utf8(out.stdout).unwrap();
            let shown: Vec<&str> = stdout.split("\0\n").collect();
            let parsed = rgit_date::parse_date_mode(mode, false).unwrap();
            for (i, &(t, tz)) in dates.iter().enumerate() {
                let got = show_date(t, tz, &parsed, &ctx).unwrap();
                if got != shown[i] {
                    failures.push(format!(
                        "{zone} --date={mode} {}: git {:?}, rgit {got:?}",
                        format_raw(t, tz),
                        shown[i]
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
