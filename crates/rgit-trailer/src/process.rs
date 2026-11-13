// SPDX-License-Identifier: MIT
//! Adding trailers to a message and writing the result: what
//! `git interpret-trailers` does to each input
//! (Documentation/git-interpret-trailers.adoc, Documentation/config/trailer.adoc).

use bstr::{BString, ByteSlice};

use crate::command;
use crate::config::{Config, IfExists, IfMissing, Where};
use crate::messages;
use crate::parse::{self, Line, ParseOptions, Trailer, find_separator, trim};

/// A `--trailer` argument with the `--where`, `--if-exists` and
/// `--if-missing` in effect where it was given (`None`: use the
/// configuration).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTrailer {
    /// The argument: `<key>[(=|<separator>)<value>]`.
    pub text: BString,
    pub where_: Option<Where>,
    pub if_exists: Option<IfExists>,
    pub if_missing: Option<IfMissing>,
}

impl NewTrailer {
    /// A trailer argument that follows the configuration.
    pub fn new(text: impl Into<BString>) -> Self {
        NewTrailer {
            text: text.into(),
            where_: None,
            if_exists: None,
            if_missing: None,
        }
    }
}

/// How the input is processed and written.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Drop trailers whose value is only whitespace.
    pub trim_empty: bool,
    /// Write only the trailers.
    pub only_trailers: bool,
    /// Do not add trailers from the arguments or from configured commands.
    pub only_input: bool,
    /// Write multi-line values on one line.
    pub unfold: bool,
    /// Do not treat `---` as the end of the message.
    pub no_divider: bool,
    /// The comment string.
    pub comment: BString,
}

/// A trailer about to be added, with its rules resolved.
struct Pending {
    trailer: Trailer,
    where_: Where,
    if_exists: IfExists,
    if_missing: IfMissing,
}

fn same_token(a: &[u8], b: &[u8]) -> bool {
    let n = a.len().min(b.len());
    a[..n].eq_ignore_ascii_case(&b[..n])
}

fn same(line: &Line, t: &Trailer) -> bool {
    matches!(line, Line::Trailer(x) if same_token(&x.token, &t.token) && x.value.eq_ignore_ascii_case(&t.value))
}

fn has_token(line: &Line, token: &[u8]) -> bool {
    matches!(line, Line::Trailer(x) if same_token(&x.token, token))
}

/// Parse a `--trailer` argument. `Err` carries the message for an empty key.
pub fn parse_argument(arg: &[u8], conf: &Config) -> Result<Trailer, String> {
    let mut seps = conf.separators.to_vec();
    seps.push(b'=');
    match parse::trailer_separator(arg, &seps).or_else(|| {
        // An empty key is an error; a `://` after the key just means
        // there is no separator.
        find_separator(arg, &seps).filter(|&p| p == 0)
    }) {
        Some(0) => Err(messages::empty_token(&arg.to_str_lossy())),
        Some(p) => Ok(parse::make_trailer(&arg[..p], &arg[p + 1..], conf)),
        None => Ok(parse::make_trailer(arg, b"", conf)),
    }
}

/// The trailers to add: the configured commands first (those with
/// `trailer.<alias>.command`), then the arguments. Errors (empty keys) go
/// to `on_error`.
fn pending(conf: &Config, args: &[NewTrailer], on_error: &mut dyn FnMut(&str)) -> Vec<Pending> {
    let mut out = Vec::new();
    for (idx, item) in conf.items.iter().enumerate() {
        if item.command.is_none() {
            continue;
        }
        out.push(Pending {
            trailer: Trailer {
                token: item.key_or_name().into(),
                value: BString::default(),
                item: Some(idx),
            },
            where_: item.where_.unwrap_or(conf.where_),
            if_exists: item.if_exists.unwrap_or(conf.if_exists),
            if_missing: item.if_missing.unwrap_or(conf.if_missing),
        });
    }
    for a in args {
        let trailer = match parse_argument(&a.text, conf) {
            Ok(t) => t,
            Err(msg) => {
                on_error(&msg);
                continue;
            }
        };
        let item = trailer.item.map(|i| &conf.items[i]);
        out.push(Pending {
            where_: a
                .where_
                .or(item.and_then(|i| i.where_))
                .unwrap_or(conf.where_),
            if_exists: a
                .if_exists
                .or(item.and_then(|i| i.if_exists))
                .unwrap_or(conf.if_exists),
            if_missing: a
                .if_missing
                .or(item.and_then(|i| i.if_missing))
                .unwrap_or(conf.if_missing),
            trailer,
        });
    }
    out
}

/// Apply one new trailer to `lines`.
fn apply(lines: &mut Vec<Line>, p: Pending, conf: &Config, on_error: &mut dyn FnMut(&str)) {
    let mut t = p.trailer;
    let on_tok = if p.where_.counts_from_end() {
        lines.iter().rposition(|l| has_token(l, &t.token))
    } else {
        lines.iter().position(|l| has_token(l, &t.token))
    };
    let action = match on_tok {
        Some(_) if p.if_exists == IfExists::DoNothing => return,
        Some(_) => Some(p.if_exists),
        None if p.if_missing == IfMissing::DoNothing => return,
        None => None,
    };
    // The command computes the value, from the argument or (without one)
    // from the existing trailer next to which it goes.
    if let Some(item) = t.item.map(|i| &conf.items[i]).filter(|i| i.has_command()) {
        let arg: BString = if !t.value.is_empty() {
            t.value.clone()
        } else {
            match on_tok.map(|i| &lines[i]) {
                Some(Line::Trailer(x)) => x.value.clone(),
                _ => BString::default(),
            }
        };
        t.value = match command::run(item, &arg) {
            Ok(v) => v,
            Err(shown) => {
                on_error(&messages::command_failed(&shown.to_str_lossy()));
                BString::default()
            }
        };
    }
    let mut at = match p.where_ {
        Where::End => lines.len(),
        Where::Start => 0,
        Where::After => on_tok.map_or(lines.len(), |i| i + 1),
        Where::Before => on_tok.unwrap_or(0),
    };
    match action {
        Some(IfExists::AddIfDifferent) => {
            if lines.iter().any(|l| same(l, &t)) {
                return;
            }
        }
        Some(IfExists::AddIfDifferentNeighbor) => {
            let neighbor = match p.where_ {
                Where::End => lines.last(),
                Where::Start => lines.first(),
                Where::After | Where::Before => on_tok.map(|i| &lines[i]),
            };
            if neighbor.is_some_and(|n| same(n, &t)) {
                return;
            }
        }
        Some(IfExists::Replace) => {
            if let Some(i) = on_tok {
                lines.remove(i);
                at = match p.where_ {
                    Where::End => lines.len(),
                    Where::Start => 0,
                    // The new trailer takes the place of the old one.
                    Where::After | Where::Before => i,
                };
            }
        }
        _ => {}
    }
    lines.insert(at, Line::Trailer(t));
}

/// Add the configured and given trailers to the trailer lines of a message.
pub fn add_trailers(
    lines: &mut Vec<Line>,
    conf: &Config,
    args: &[NewTrailer],
    on_error: &mut dyn FnMut(&str),
) {
    for p in pending(conf, args, on_error) {
        apply(lines, p, conf, on_error);
    }
}

/// Write one trailer: `<key><sep> <value>`, or `<key><value>` when the key
/// already ends with a separator (and possibly spaces).
pub fn format_trailer(out: &mut Vec<u8>, t: &Trailer, conf: &Config, unfold: bool) {
    out.extend_from_slice(&t.token);
    let key_end = trim(&t.token).last().copied();
    if !key_end.is_some_and(|c| conf.separators.contains(&c)) {
        out.push(conf.default_separator());
        out.push(b' ');
    }
    if unfold {
        out.extend_from_slice(&unfold_value(&t.value));
    } else {
        out.extend_from_slice(&t.value);
    }
    out.push(b'\n');
}

/// A multi-line value on one line: every newline with the whitespace after
/// it becomes one space.
pub fn unfold_value(value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(value.len());
    let mut i = 0;
    while i < value.len() {
        if value[i] == b'\n' {
            i += 1;
            while i < value.len() && parse::is_space(value[i]) {
                i += 1;
            }
            out.push(b' ');
        } else {
            out.push(value[i]);
            i += 1;
        }
    }
    out
}

/// Process one input like `git interpret-trailers`: find the trailer
/// block, add the trailers (unless `only_input`) and write the result.
/// Errors that do not stop processing (empty keys, failing commands) go
/// to `on_error` as they happen.
pub fn process(
    input: &[u8],
    conf: &Config,
    opts: &Options,
    args: &[NewTrailer],
    on_error: &mut dyn FnMut(&str),
) -> Vec<u8> {
    let popts = ParseOptions {
        comment: &opts.comment,
        divider: !opts.no_divider,
    };
    let info = parse::parse(input, conf, &popts);
    let mut lines = info.lines.clone();
    if !opts.only_input {
        add_trailers(&mut lines, conf, args, on_error);
    }
    let mut out = Vec::with_capacity(input.len() + 64);
    if !opts.only_trailers {
        out.extend_from_slice(&input[..info.start]);
        if !info.found {
            if !out.is_empty() && out.last() != Some(&b'\n') {
                out.push(b'\n');
            }
            if !info.blank_line_before {
                out.push(b'\n');
            }
        }
    }
    for line in &lines {
        match line {
            Line::Trailer(t) => {
                if opts.trim_empty && trim(&t.value).is_empty() {
                    continue;
                }
                format_trailer(&mut out, t, conf, opts.unfold);
            }
            Line::Other(raw) => {
                if !opts.only_trailers {
                    out.extend_from_slice(raw);
                    out.push(b'\n');
                }
            }
        }
    }
    if !opts.only_trailers {
        out.extend_from_slice(&input[info.end..]);
    }
    out
}
