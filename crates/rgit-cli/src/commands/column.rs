// SPDX-License-Identifier: MIT
//! `git column` (Documentation/git-column.adoc): the lines of standard
//! input laid out in columns. The layout comes from `column.ui` and
//! `column.<command>` (`--command`), then `--mode`/`--raw-mode`.

use std::io::{Read, Write};

use crate::column::{Enable, Layout, Mode, Options, format};
use crate::messages::column as msg;
use crate::prelude::*;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let command = RefCell::new(None::<String>);
    let mode = RefCell::new(None::<String>);
    let raw = Cell::new(-1);
    let width = Cell::new(-1);
    let indent = RefCell::new(None::<String>);
    let nl = RefCell::new(None::<String>);
    let padding = Cell::new(1);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::string(None, "command", &command, "name", msg::HELP_COMMAND),
            Opt::string(None, "mode", &mode, "style", msg::HELP_MODE).optional(Some("")),
            Opt::integer(None, "raw-mode", &raw, msg::HELP_MODE).noneg(),
            Opt::integer(None, "width", &width, msg::HELP_WIDTH),
            Opt::string(None, "indent", &indent, "string", msg::HELP_INDENT),
            Opt::string(None, "nl", &nl, "string", msg::HELP_NL),
            Opt::integer(None, "padding", &padding, msg::HELP_PADDING),
        ],
    );
    let args = parser.parse(argv)?.args;
    if !args.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    if padding.get() < 0 {
        return Err(Die::Fatal(msg::PADDING_NEGATIVE.into()));
    }

    let mut m = match command.borrow().as_deref() {
        Some(c) => {
            let config = crate::pager::config(ctx);
            Mode::from_config(&config, Some(c))?
        }
        None => Mode::default(),
    };
    if raw.get() >= 0 {
        m = raw_mode(raw.get());
    }
    if let Some(v) = mode.borrow().as_deref() {
        if let Err(tok) = m.apply(v) {
            eprintln!("error: {}", msg::unsupported_option(&tok));
            return Ok(129);
        }
    }

    let mut input = Vec::new();
    let _ = std::io::stdin().lock().read_to_end(&mut input);
    let mut out = std::io::stdout().lock();
    if !m.active() {
        let _ = out.write_all(&input);
        let _ = out.flush();
        return Ok(0);
    }
    let text = String::from_utf8_lossy(&input);
    let cells: Vec<String> = text.lines().map(str::to_owned).collect();
    let mut opts = Options::new(m);
    if width.get() >= 0 {
        opts.width = usize::try_from(width.get()).unwrap_or(0);
    }
    opts.padding = usize::try_from(padding.get()).unwrap_or(1);
    if let Some(i) = indent.borrow().as_ref() {
        opts.indent = i.clone();
    }
    if let Some(n) = nl.borrow().as_ref() {
        opts.nl = n.clone();
    }
    let _ = out.write_all(format(&cells, &opts).as_bytes());
    let _ = out.flush();
    Ok(0)
}

/// `--raw-mode=<n>`: the layout in the low four bits (0 column, 1 row,
/// otherwise one per line), the enable state in the next two (1 always,
/// 3 auto, otherwise never).
fn raw_mode(n: i32) -> Mode {
    Mode {
        enable: match (n >> 4) & 3 {
            1 => Enable::Always,
            3 => Enable::Auto,
            _ => Enable::Never,
        },
        layout: match n & 0xf {
            0 => Layout::Column,
            1 => Layout::Row,
            _ => Layout::Plain,
        },
        dense: false,
    }
}
