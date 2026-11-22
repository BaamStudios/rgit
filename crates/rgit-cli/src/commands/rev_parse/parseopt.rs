// SPDX-License-Identifier: MIT
//! `git rev-parse --parseopt` and `--sq-quote` (Documentation/git-rev-parse.adoc,
//! PARSEOPT and SQ-QUOTE): option normalisation for shell scripts.
//!
//! The option specification on standard input becomes an option table of
//! `rgit-options`, so parsing, abbreviations, error messages and the usage
//! text are the ones every command has. Each recognised option is echoed
//! in a canonical form: the short name if there is one, else the long
//! name; with `--stuck-long` the long name with `=<value>` (short-only
//! options as `-<c><value>`).

use std::cell::{Cell, RefCell};
use std::io::{Read, Write};

use rgit_options::{Opt, ParseFlags, Parser};

use super::sq_quote;
use crate::messages;
use crate::prelude::*;

/// `rev-parse --sq-quote <arg>...`: one line, every argument quoted.
pub fn sq_quote_mode(args: &[String]) -> CmdResult {
    let mut line = String::new();
    for a in args {
        line.push(' ');
        line.push_str(&sq_quote(a));
    }
    line.push('\n');
    let _ = std::io::stdout().lock().write_all(line.as_bytes());
    Ok(0)
}

/// One option of the specification.
struct SpecOpt {
    short: Option<char>,
    long: Option<String>,
    takes_arg: bool,
    optional: bool,
    hidden: bool,
    noneg: bool,
    argh: Option<String>,
    help: String,
}

enum Line {
    Group(String),
    Opt(SpecOpt),
}

/// Parses the option lines after the `--` separator.
fn parse_spec_line(line: &str) -> Result<Option<Line>, Die> {
    if line.trim().is_empty() {
        return Ok(None);
    }
    let Some(sp) = line.find([' ', '\t']) else {
        return Ok(Some(Line::Group(line.to_owned())));
    };
    if sp == 0 {
        return Ok(Some(Line::Group(line.trim_start().to_owned())));
    }
    let spec = &line[..sp];
    let help = line[sp..].trim_start().to_owned();
    let flags_at = spec.find(['*', '=', '?', '!']).unwrap_or(spec.len());
    if flags_at == 0 {
        return Err(Die::Fatal(messages::PARSEOPT_MISSING_SPEC.to_owned()));
    }
    let name = &spec[..flags_at];
    let rest = &spec[flags_at..];
    let flags_len = rest
        .find(|c: char| !matches!(c, '*' | '=' | '?' | '!'))
        .unwrap_or(rest.len());
    let (flags, argh) = rest.split_at(flags_len);
    let (short, long) = match name.char_indices().nth(1) {
        Some((i, ',')) => {
            let long = &name[i + 1..];
            (name.chars().next(), (!long.is_empty()).then(|| long.to_owned()))
        }
        None => (name.chars().next(), None),
        Some(_) => (None, Some(name.to_owned())),
    };
    Ok(Some(Line::Opt(SpecOpt {
        short,
        long,
        takes_arg: flags.contains('=') || flags.contains('?'),
        optional: flags.contains('?'),
        hidden: flags.contains('*'),
        noneg: flags.contains('!'),
        argh: (!argh.is_empty()).then(|| argh.to_owned()),
        help,
    })))
}

/// The canonical form of one parsed option.
fn render(o: &SpecOpt, stuck_long: bool, arg: Option<&str>, unset: bool) -> String {
    if unset {
        return format!("--no-{}", o.long.as_deref().unwrap_or_default());
    }
    let value = arg.map(sq_quote);
    match (stuck_long, o.short, o.long.as_deref()) {
        (true, _, Some(l)) => match value {
            Some(v) => format!("--{l}={v}"),
            None => format!("--{l}"),
        },
        (true, Some(c), None) => format!("-{c}{}", value.unwrap_or_default()),
        (false, Some(c), _) => match value {
            Some(v) => format!("-{c} {v}"),
            None => format!("-{c}"),
        },
        (_, None, l) => match value {
            Some(v) => format!("--{} {v}", l.unwrap_or_default()),
            None => format!("--{}", l.unwrap_or_default()),
        },
    }
}

/// `rev-parse --parseopt [<options>] -- [<args>...]`.
pub fn parseopt_mode(args: &[String]) -> CmdResult {
    let keep_dashdash = Cell::new(false);
    let stop_at_non_option = Cell::new(false);
    let stuck_long = Cell::new(false);
    let mut own = Parser::new(
        &[messages::PARSEOPT_USAGE],
        vec![
            Opt::flag(None, "keep-dashdash", &keep_dashdash, messages::PARSEOPT_HELP_KEEP_DASHDASH),
            Opt::flag(
                None,
                "stop-at-non-option",
                &stop_at_non_option,
                messages::PARSEOPT_HELP_STOP_AT_NON_OPTION,
            ),
            Opt::flag(None, "stuck-long", &stuck_long, messages::PARSEOPT_HELP_STUCK_LONG),
        ],
    )
    .flags(ParseFlags {
        keep_dashdash: true,
        ..Default::default()
    });
    let mut argv = vec!["rev-parse --parseopt".to_owned()];
    argv.extend(args.iter().cloned());
    let rest = own.parse(&argv)?.args;
    if rest.first().map(String::as_str) != Some("--") {
        return Err(own.usage_error().into());
    }

    let mut input = Vec::new();
    let _ = std::io::stdin().lock().read_to_end(&mut input);
    let input = String::from_utf8_lossy(&input).into_owned();
    let mut lines = input.lines();
    let mut usage: Vec<String> = Vec::new();
    loop {
        match lines.next() {
            None => return Err(Die::Fatal(messages::PARSEOPT_PREMATURE_END.to_owned())),
            Some("--") => break,
            Some(l) => usage.push(l.to_owned()),
        }
    }
    if usage.is_empty() {
        return Err(Die::Fatal(messages::PARSEOPT_NO_USAGE.to_owned()));
    }
    let mut spec = Vec::new();
    for l in lines {
        if let Some(line) = parse_spec_line(l)? {
            spec.push(line);
        }
    }

    let out: RefCell<Vec<String>> = RefCell::new(Vec::new());
    // `-h`/`--help` show the usage even when the specification lists them.
    let help = Cell::new(false);
    let stuck = stuck_long.get();
    let mut table = Vec::new();
    for line in &spec {
        match line {
            Line::Group(title) => table.push(Opt::group(title)),
            Line::Opt(o) => {
                let out = &out;
                let help = &help;
                let is_help = o.short == Some('h') || o.long.as_deref() == Some("help");
                let mut opt = Opt::callback(
                    o.short,
                    o.long.as_deref(),
                    None,
                    &o.help,
                    move |arg, unset| {
                        if is_help && !unset {
                            help.set(true);
                            return Err(rgit_options::CallbackError::Silent);
                        }
                        out.borrow_mut().push(render(o, stuck, arg, unset));
                        Ok(())
                    },
                );
                if let Some(a) = &o.argh {
                    opt = opt.argh(a);
                }
                if !o.takes_arg {
                    opt = opt.noarg();
                } else if o.optional {
                    opt = opt.optional(None);
                }
                if o.hidden {
                    opt = opt.hidden();
                }
                if o.noneg {
                    opt = opt.noneg();
                }
                table.push(opt);
            }
        }
    }
    let usage_refs: Vec<&str> = usage.iter().map(String::as_str).collect();
    let mut parser = Parser::new(&usage_refs, table).flags(ParseFlags {
        keep_dashdash: keep_dashdash.get(),
        stop_at_non_option: stop_at_non_option.get(),
        ..Default::default()
    });
    let mut user = vec!["rev-parse --parseopt".to_owned()];
    user.extend(rest[1..].iter().cloned());
    let parsed = match parser.parse(&user) {
        Ok(p) => p,
        Err(e) if e.code == 0 || help.get() => {
            let text = if help.get() {
                parser.usage_text(false)
            } else {
                e.stdout
            };
            let mut stdout = std::io::stdout().lock();
            let _ = write!(stdout, "cat <<\\EOF\n{text}EOF\nexit 0\n");
            return Ok(0);
        }
        Err(e) => return Err(e.into()),
    };
    drop(parser);
    let mut line = String::from("set --");
    for o in out.borrow().iter() {
        line.push(' ');
        line.push_str(o);
    }
    line.push_str(" --");
    for a in &parsed.args {
        line.push(' ');
        line.push_str(&sq_quote(a));
    }
    line.push('\n');
    let _ = std::io::stdout().lock().write_all(line.as_bytes());
    Ok(0)
}
