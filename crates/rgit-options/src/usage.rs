// SPDX-License-Identifier: MIT
//! Usage text (`-h`, `--help-all`) and `--git-completion-helper` output.

use crate::messages;
use crate::opt::{ArgMode, Kind, Opt};

/// Column at which option descriptions start.
const OPTS_WIDTH: usize = 26;

/// Format the usage lines.
///
/// The first alternative is prefixed with `usage: `, later ones with
/// `   or: `. A usage string may span several lines (separated by `\n`);
/// continuation lines are written without indentation and are aligned
/// under the first argument of the first line (the first word starting
/// with `[`, `(`, `<`, `{` or `-`).
pub(crate) fn usage_lines(usage: &[&str]) -> String {
    let mut out = String::new();
    for (i, u) in usage.iter().enumerate() {
        let mut lines = u.split('\n');
        let first = lines.next().unwrap_or("");
        out.push_str(if i == 0 { "usage: " } else { "   or: " });
        out.push_str(first);
        out.push('\n');
        let indent = 7 + continuation_indent(first);
        for l in lines {
            let l = l.trim_start();
            if l.is_empty() {
                out.push('\n');
                continue;
            }
            out.extend(std::iter::repeat_n(' ', indent));
            out.push_str(l);
            out.push('\n');
        }
    }
    out
}

fn continuation_indent(first: &str) -> usize {
    let mut pos = 0;
    for word in first.split(' ') {
        if word.starts_with(['[', '(', '<', '{', '-']) {
            return pos;
        }
        pos += word.len() + 1;
    }
    first.len() + 1
}

fn argh_text(opt: &Opt<'_>, target: &Opt<'_>) -> String {
    let argh = target.argh.or(opt.argh);
    match argh {
        Some(a) if target.flags.literal_argh => a.to_string(),
        Some(a) => format!("<{a}>"),
        None => "...".to_string(),
    }
}

/// The left column of an option line, e.g. `    -s, --[no-]string <str>`.
pub(crate) fn option_label(opt: &Opt<'_>, target: &Opt<'_>) -> String {
    let mut s = String::from("    ");
    if opt.flags.nodash {
        if let Some(c) = opt.short {
            s.push(c);
        }
    } else if opt.is_number() {
        s.push_str("-NUM");
    } else {
        if let Some(c) = opt.short {
            s.push('-');
            s.push(c);
            if opt.long.is_some() {
                s.push_str(", ");
            }
        }
        if let Some(l) = opt.long {
            s.push_str("--");
            if target.negatable() && !opt.flags.noneg && !l.starts_with("no-") {
                s.push_str("[no-]");
            }
            s.push_str(l);
        }
    }
    match target.arg_mode() {
        ArgMode::None => {}
        ArgMode::Required => {
            s.push(' ');
            s.push_str(&argh_text(opt, target));
        }
        ArgMode::Optional => {
            if opt.long.is_some() {
                s.push_str("[=");
            } else {
                s.push('[');
            }
            s.push_str(&argh_text(opt, target));
            s.push(']');
        }
    }
    s
}

/// Full usage text: usage lines, then the option list.
pub(crate) fn usage_text(usage: &[&str], opts: &[Opt<'_>], all: bool) -> String {
    let mut out = usage_lines(usage);
    out.push('\n');
    let mut printed = false;
    for opt in opts {
        if opt.flags.hidden && !all {
            continue;
        }
        if opt.is_subcommand() {
            continue;
        }
        if opt.is_group() {
            if opt.help.is_empty() {
                out.push('\n');
            } else {
                if printed {
                    out.push('\n');
                }
                out.push_str(opt.help);
                out.push('\n');
            }
            printed = true;
            continue;
        }
        let target = resolve(opts, opt);
        let label = option_label(opt, target);
        let help_owned;
        let help = match opt.kind {
            Kind::Alias(t) if opt.help.is_empty() => {
                help_owned = messages::alias_of(t);
                help_owned.as_str()
            }
            _ => opt.help,
        };
        let width = label.chars().count();
        out.push_str(&label);
        if width < OPTS_WIDTH {
            out.extend(std::iter::repeat_n(' ', OPTS_WIDTH - width));
        } else {
            out.push('\n');
            out.extend(std::iter::repeat_n(' ', OPTS_WIDTH));
        }
        let mut lines = help.split('\n');
        out.push_str(lines.next().unwrap_or(""));
        for l in lines {
            out.push('\n');
            out.extend(std::iter::repeat_n(' ', OPTS_WIDTH));
            out.push_str(l);
        }
        out.push('\n');
        printed = true;
    }
    out.push('\n');
    out
}

/// Resolve an alias to the option it names (or return the option itself).
pub(crate) fn resolve<'o, 'a>(opts: &'o [Opt<'a>], opt: &'o Opt<'a>) -> &'o Opt<'a> {
    if let Kind::Alias(t) = opt.kind {
        if let Some(target) = opts.iter().find(|o| !o.is_alias() && o.long == Some(t)) {
            return target;
        }
    }
    opt
}

/// Output of `--git-completion-helper`.
pub(crate) fn completion(opts: &[Opt<'_>]) -> String {
    let mut out = String::new();
    for (idx, opt) in opts.iter().enumerate() {
        if opt.is_group() || opt.flags.hidden || opt.flags.nocomplete || opt.is_alias() {
            continue;
        }
        let Some(long) = opt.long else { continue };
        if idx > 0 {
            out.push(' ');
        }
        if opt.is_subcommand() {
            out.push_str(long);
            continue;
        }
        out.push_str("--");
        out.push_str(long);
        if opt.arg_mode() == ArgMode::Required && !opt.flags.lastarg_default {
            out.push('=');
        }
    }
    let negs: Vec<&str> = opts
        .iter()
        .filter(|o| !o.flags.hidden && !o.flags.nocomplete && !o.is_subcommand())
        .filter(|o| !o.flags.noneg && resolve(opts, o).negatable())
        .filter_map(|o| o.long)
        .filter(|l| !l.starts_with("no-"))
        .collect();
    for (k, n) in negs.iter().enumerate() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str("--no-");
        out.push_str(n);
        if k == 0 && negs.len() > 1 {
            out.push_str(" --");
        }
    }
    out.push('\n');
    out
}
