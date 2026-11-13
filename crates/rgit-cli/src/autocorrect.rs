// SPDX-License-Identifier: MIT
//! Unknown commands: "did you mean" suggestions and `help.autoCorrect`
//! (Documentation/config/help.adoc).
//!
//! The candidates are the built-ins, the `git-<name>` programs in the exec
//! path and on `$PATH`, and the aliases. Their distance to the typed name is
//! a weighted Damerau-Levenshtein distance (adjacent transposition 0,
//! substitution 2, inserting a character 1, deleting one 3), plus one. A
//! common command (`git help`) that starts with the typed name gets
//! distance 0. Sorted by distance, then name: the prefix matches are shown
//! together with every candidate at the next distance, provided that
//! distance is below 7; otherwise nothing is suggested. The weights and
//! limits were determined by comparing suggestions of the `git` binary.

use std::io::{BufRead, IsTerminal, Write};

use crate::alias::EarlyConfig;
use crate::messages::help as msg;
use crate::{CmdResult, Context, Die, cmdlist};

const SWAP: usize = 0;
const SUBSTITUTE: usize = 2;
const INSERT: usize = 1;
const DELETE: usize = 3;
/// Candidates at this distance or more are not similar.
const SIMILARITY_FLOOR: usize = 7;

/// Weighted optimal-string-alignment distance from `typed` to `cand`.
pub fn distance(typed: &[u8], cand: &[u8]) -> usize {
    let n = cand.len();
    let mut prev2 = vec![0usize; n + 1];
    let mut prev: Vec<usize> = (0..=n).map(|j| j * INSERT).collect();
    let mut cur = vec![0usize; n + 1];
    for i in 1..=typed.len() {
        cur[0] = i * DELETE;
        for j in 1..=n {
            let mut best = prev[j - 1]
                + if typed[i - 1] == cand[j - 1] {
                    0
                } else {
                    SUBSTITUTE
                };
            if i > 1 && j > 1 && typed[i - 1] == cand[j - 2] && typed[i - 2] == cand[j - 1] {
                best = best.min(prev2[j - 2] + SWAP);
            }
            best = best.min(prev[j] + DELETE);
            best = best.min(cur[j - 1] + INSERT);
            cur[j] = best;
        }
        std::mem::swap(&mut prev2, &mut prev);
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[n]
}

/// The suggestions for `typed` among `candidates`, best first.
pub fn suggest(
    typed: &str,
    candidates: &[String],
    is_common: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut scored: Vec<(usize, &String)> = candidates
        .iter()
        .map(|c| {
            let d = if is_common(c) && c.starts_with(typed) {
                0
            } else {
                distance(typed.as_bytes(), c.as_bytes()) + 1
            };
            (d, c)
        })
        .collect();
    scored.sort();
    scored.dedup_by(|a, b| a.1 == b.1);
    let zeros = scored.iter().take_while(|(d, _)| *d == 0).count();
    let Some(&(best, _)) = scored.get(zeros) else {
        return Vec::new();
    };
    if best >= SIMILARITY_FLOOR {
        return Vec::new();
    }
    let n = zeros
        + scored[zeros..]
            .iter()
            .take_while(|(d, _)| *d == best)
            .count();
    scored[..n].iter().map(|(_, c)| (*c).clone()).collect()
}

/// `help.autoCorrect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Show,
    Never,
    Immediate,
    Prompt,
    /// Run after this many deciseconds.
    Delay(i64),
}

pub fn mode(config: &rgit_config::ConfigSet) -> Result<Mode, Die> {
    let Some(entry) = config.get("help.autocorrect") else {
        return Ok(Mode::Show);
    };
    let value = entry.value.as_deref().map(|v| v.to_vec());
    match value.as_deref() {
        Some(b"never") => return Ok(Mode::Never),
        Some(b"immediate") => return Ok(Mode::Immediate),
        Some(b"show") => return Ok(Mode::Show),
        Some(b"prompt") => return Ok(Mode::Prompt),
        _ => {}
    }
    if let Some(b) = rgit_config::value::parse_bool_text(value.as_deref()) {
        return Ok(if b { Mode::Immediate } else { Mode::Show });
    }
    let n = entry
        .int(rgit_config::IntWidth::I32)
        .map_err(|e| Die::Fatal(e.to_string()))?;
    Ok(match n {
        0 => Mode::Show,
        n if n == 1 || n < 0 => Mode::Immediate,
        n => Mode::Delay(n),
    })
}

/// All candidate names: built-ins, programs, aliases.
fn candidates(ctx: &Context, early: &EarlyConfig) -> Vec<String> {
    let main = cmdlist::main_commands(ctx);
    let others = cmdlist::other_commands(ctx, &main);
    let mut all: Vec<String> = main.into_iter().chain(others).collect();
    all.extend(
        crate::alias::list(&early.config)
            .into_iter()
            .map(|(n, _)| n),
    );
    all
}

/// `git <typed>` is not a command: suggest, or run the one similar command.
/// Returns the corrected name to run, or the exit result.
pub fn unknown(
    ctx: &Context,
    early: &EarlyConfig,
    typed: &str,
) -> Result<Option<String>, CmdResult> {
    let mode = match mode(&early.config) {
        Ok(m) => m,
        Err(e) => return Err(Err(e)),
    };
    let not_a_command = || eprintln!("{}", crate::messages::not_a_git_command(typed));
    if mode == Mode::Never {
        not_a_command();
        return Err(Ok(1));
    }
    let found = suggest(typed, &candidates(ctx, early), cmdlist::is_common);
    if found.len() == 1 && mode != Mode::Show {
        let assumed = &found[0];
        let mut err = std::io::stderr();
        match mode {
            Mode::Prompt => {
                if !(std::io::stdin().is_terminal() && err.is_terminal()) {
                    not_a_command();
                    return Err(Ok(1));
                }
                let _ = writeln!(err, "{}", msg::autocorrect_warning(typed));
                let _ = write!(err, "{}", msg::autocorrect_prompt(assumed));
                let _ = err.flush();
                let mut line = String::new();
                let _ = std::io::stdin().lock().read_line(&mut line);
                if !line.trim_start().starts_with(['y', 'Y']) {
                    return Err(Ok(1));
                }
            }
            Mode::Delay(ds) => {
                let _ = writeln!(err, "{}", msg::autocorrect_warning(typed));
                let secs = format!("{}.{}", ds / 10, ds % 10);
                let _ = writeln!(err, "{}", msg::autocorrect_in(&secs, assumed));
                std::thread::sleep(std::time::Duration::from_millis(ds as u64 * 100));
            }
            _ => {
                let _ = writeln!(err, "{}", msg::autocorrect_warning(typed));
                let _ = writeln!(err, "{}", msg::autocorrect_immediately(assumed));
            }
        }
        return Ok(Some(assumed.clone()));
    }
    if mode == Mode::Prompt && found.len() == 1 {
        not_a_command();
        return Err(Ok(1));
    }
    not_a_command();
    if !found.is_empty() {
        let mut text = format!("\n{}\n", msg::similar_commands(found.len()));
        for f in &found {
            text.push('\t');
            text.push_str(f);
            text.push('\n');
        }
        eprint!("{text}");
    }
    Err(Ok(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distances() {
        assert_eq!(distance(b"abc", b"abc"), 0);
        assert_eq!(distance(b"abc", b"abcd"), INSERT);
        assert_eq!(distance(b"abcd", b"abc"), DELETE);
        assert_eq!(distance(b"abc", b"abd"), SUBSTITUTE);
        assert_eq!(distance(b"stauts", b"status"), SWAP);
        assert_eq!(distance(b"sta", b"stage"), 2 * INSERT);
    }

    #[test]
    fn suggestions() {
        let c = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let common = |n: &str| ["status", "switch"].contains(&n);
        let all = c(&["stab", "stage", "stash", "status", "switch"]);
        assert_eq!(suggest("sta", &all, common), ["status", "stab"]);
        assert_eq!(
            suggest("sta", &c(&["stage", "stash", "status"]), common),
            ["status", "stage", "stash"]
        );
        // A prefix match alone is not enough.
        assert!(suggest("switc", &c(&["switch", "zzzzzzzzz"]), common).is_empty());
        assert_eq!(suggest("stauts", &all, common), ["status"]);
        assert!(suggest("qqqqqq", &all, common).is_empty());
    }
}
