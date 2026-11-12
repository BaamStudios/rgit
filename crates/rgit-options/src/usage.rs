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
///
/// An empty usage string ends the alternatives: the strings after it are a
/// description, printed indented by four spaces (an empty one as a blank
/// line), as `git rev-parse --parseopt` shows for a spec with a blank line.
pub(crate) fn usage_lines(usage: &[&str]) -> String {
    let mut out = String::new();
    let (forms, description) = match usage.iter().position(|u| u.is_empty()) {
        Some(n) => (&usage[..n], &usage[n..]),
        None => (usage, &[][..]),
    };
    for (i, u) in forms.iter().enumerate() {
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
    // The leading empty string becomes the blank line after the forms.
    for l in description.iter().flat_map(|d| d.split('\n')) {
        if !l.is_empty() {
            out.push_str("    ");
            out.push_str(l);
        }
        out.push('\n');
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
///
/// With `after_parse`, aliases are shown as git shows them in usage
/// messages printed after parsing (`usage_with_options`): unresolved, as
/// an option taking an unnamed argument (`--[no-]mailmap ...`). During
/// parsing (`-h`, unknown options) they are resolved to their target.
pub(crate) fn usage_text(usage: &[&str], opts: &[Opt<'_>], all: bool, after_parse: bool) -> String {
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
        let label = match (&opt.kind, opt.long) {
            (Kind::Alias(_), Some(l)) if after_parse => {
                let neg = if l.starts_with("no-") { "" } else { "[no-]" };
                format!("    --{neg}{l} ...")
            }
            _ => option_label(opt, target),
        };
        let help_owned;
        let help = match opt.kind {
            Kind::Alias(t) if opt.help.is_empty() => {
                help_owned = messages::alias_of(t);
                help_owned.as_str()
            }
            _ => opt.help,
        };
        push_entry(&mut out, &label, help);
        if let Some(positive) = positive_form(opt, target) {
            push_entry(
                &mut out,
                &format!("    --{positive}"),
                &messages::opposite_of(opt.long.unwrap_or_default()),
            );
        }
        printed = true;
    }
    // Without options git prints only the usage and one empty line.
    if printed {
        out.push('\n');
    }
    out
}

/// One option line: the label, then the help aligned at [`OPTS_WIDTH`]
/// (on the next line when the label is too wide).
fn push_entry(out: &mut String, label: &str, help: &str) {
    let width = label.chars().count();
    out.push_str(label);
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
}

/// For a negatable option named `no-<x>`, the positive form `<x>`: git
/// lists it after the option as "opposite of --no-<x>" and offers it for
/// completion (observed with `git commit -h` and `git rev-parse --parseopt`).
fn positive_form<'a>(opt: &Opt<'a>, target: &Opt<'a>) -> Option<&'a str> {
    let long = opt.long?;
    if opt.flags.noneg || !target.negatable() || opt.is_alias() {
        return None;
    }
    long.strip_prefix("no-").filter(|p| !p.is_empty())
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

/// Output of `--git-completion-helper`; with `all`, that of
/// `--git-completion-helper-all`, which also lists hidden options and
/// aliases.
pub(crate) fn completion(opts: &[Opt<'_>], all: bool) -> String {
    let mut out = String::new();
    for opt in opts {
        if opt.is_group() || opt.flags.nocomplete || (!all && (opt.flags.hidden || opt.is_alias()))
        {
            continue;
        }
        let Some(long) = opt.long else { continue };
        if !out.is_empty() {
            out.push(' ');
        }
        if opt.is_subcommand() {
            out.push_str(long);
            continue;
        }
        out.push_str("--");
        out.push_str(long);
        let target = resolve(opts, opt);
        if target.arg_mode() == ArgMode::Required && !target.flags.lastarg_default {
            out.push('=');
        }
    }
    let shown = || {
        opts.iter()
            .filter(move |o| (all || !o.flags.hidden) && !o.flags.nocomplete && !o.is_subcommand())
    };
    // Positive forms of `no-<x>` options come after all other options.
    for p in shown().filter_map(|o| positive_form(o, resolve(opts, o))) {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str("--");
        out.push_str(p);
    }
    let negs: Vec<&str> = shown()
        .filter(|o| !o.flags.noneg && resolve(opts, o).negatable())
        .filter_map(|o| o.long)
        .filter(|l| !l.starts_with("no-"))
        .collect();
    // The `--` separates the negations completion only offers after
    // `--no`. When an option named `no-<x>` is listed already, all of them
    // go after it; otherwise the first negation is listed before it.
    let mut separator_done = shown()
        .any(|o| o.long.is_some_and(|l| l.starts_with("no-")) && !o.is_alias() && !o.is_group());
    for (k, n) in negs.iter().enumerate() {
        if separator_done && k == 0 {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str("--");
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str("--no-");
        out.push_str(n);
        if !separator_done && negs.len() > 1 {
            out.push_str(" --");
            separator_done = true;
        }
    }
    out.push('\n');
    out
}
