// SPDX-License-Identifier: MIT
//! Column output (`column.ui`, `column.<cmd>`, `--column[=<options>]`;
//! Documentation/config/column.adoc, git-column.adoc).
//!
//! Layout as observed with the reference git: every cell is as wide as
//! the widest item plus the padding (the last cell of a line is not
//! padded); `row` fills the lines first, `column` the columns, with as
//! many columns as fit into the width. `dense` then sizes every column by
//! its own widest item and uses fewer lines while everything still fits.

use std::io::IsTerminal;

use rgit_config::ConfigSet;

use crate::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Enable {
    Always,
    Never,
    Auto,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    Column,
    Row,
    Plain,
}

/// The parsed column settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mode {
    enable: Option<Enable>,
    layout: Layout,
    dense: bool,
}

impl Default for Mode {
    fn default() -> Self {
        Mode {
            enable: None,
            layout: Layout::Column,
            dense: false,
        }
    }
}

impl Mode {
    /// Applies a list of tokens separated by spaces or commas; `Err` with
    /// the unknown token.
    pub fn apply(&mut self, spec: &str) -> Result<(), String> {
        for tok in spec.split([' ', ',']).filter(|t| !t.is_empty()) {
            let (neg, name) = match tok.strip_prefix("no") {
                Some(rest) if rest == "dense" => (true, rest),
                _ => (false, tok),
            };
            match name {
                "always" => self.enable = Some(Enable::Always),
                "never" => self.enable = Some(Enable::Never),
                "auto" => self.enable = Some(Enable::Auto),
                "column" | "row" | "plain" => {
                    self.layout = match name {
                        "column" => Layout::Column,
                        "row" => Layout::Row,
                        _ => Layout::Plain,
                    };
                    self.enable.get_or_insert(Enable::Always);
                }
                "dense" => {
                    self.dense = !neg;
                    self.enable.get_or_insert(Enable::Always);
                }
                _ => return Err(tok.to_owned()),
            }
        }
        Ok(())
    }

    /// `column.ui`, then `column.<command>`.
    pub fn from_config(config: &ConfigSet, command: &str) -> Result<Mode, Die> {
        let mut m = Mode::default();
        for key in ["column.ui".to_owned(), format!("column.{command}")] {
            let Some(e) = config.get(&key) else {
                continue;
            };
            let v = e.value.as_ref().map(|v| v.to_string()).unwrap_or_default();
            if let Err(t) = m.apply(&v) {
                crate::messages::print_error(&crate::messages::column_unsupported(&t));
                crate::messages::print_error(&crate::messages::column_bad_token(&t, &key));
                return Err(Die::Fatal(crate::revisions::unable_to_parse(&key, e)));
            }
        }
        Ok(m)
    }

    /// Whether the output goes into columns.
    pub fn active(&self) -> bool {
        match self.enable {
            Some(Enable::Always) => self.layout != Layout::Plain,
            Some(Enable::Auto) => self.layout != Layout::Plain && std::io::stdout().is_terminal(),
            _ => false,
        }
    }
}

/// The width commands lay their lists out in: one less than the terminal
/// (`COLUMNS`, else 80).
pub fn list_width() -> usize {
    term_width().saturating_sub(1)
}

/// The terminal width: `COLUMNS`, else 80.
pub fn term_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(80)
}

/// The lines of `items` laid out in `mode` within `width`, each cell
/// padded by `padding` and every line starting with `indent`.
pub fn layout(
    items: &[String],
    mode: Mode,
    width: usize,
    padding: usize,
    indent: &str,
) -> Vec<String> {
    if items.is_empty() {
        return Vec::new();
    }
    let lens: Vec<usize> = items.iter().map(|s| s.chars().count()).collect();
    let n = items.len();
    let avail = width.saturating_sub(indent.chars().count());
    let cell = lens.iter().max().copied().unwrap_or(0) + padding;
    let mut cols = (avail / cell.max(1)).max(1).min(n);
    let mut rows = n.div_ceil(cols);
    let row_major = mode.layout == Layout::Row;
    if !row_major {
        cols = n.div_ceil(rows);
    }
    let index = |r: usize, c: usize, rows: usize, cols: usize| {
        if row_major {
            r * cols + c
        } else {
            c * rows + r
        }
    };
    let widths = |rows: usize, cols: usize| -> Vec<usize> {
        (0..cols)
            .map(|c| {
                (0..rows)
                    .filter_map(|r| lens.get(index(r, c, rows, cols)))
                    .max()
                    .copied()
                    .unwrap_or(0)
                    + padding
            })
            .collect()
    };
    let mut col_widths = vec![cell; cols];
    if mode.dense {
        col_widths = widths(rows, cols);
        let mut r = rows;
        while r > 1 {
            r -= 1;
            let c = n.div_ceil(r);
            if !row_major && n.div_ceil(c) != r {
                continue;
            }
            let w = widths(r, c);
            if w.iter().sum::<usize>() > avail {
                break;
            }
            rows = r;
            cols = c;
            col_widths = w;
        }
    }
    let mut out = Vec::with_capacity(rows);
    for r in 0..rows {
        let mut line = indent.to_owned();
        let cells: Vec<usize> = (0..cols)
            .map(|c| index(r, c, rows, cols))
            .filter(|&i| i < n)
            .collect();
        for (k, &i) in cells.iter().enumerate() {
            line.push_str(&items[i]);
            if k + 1 < cells.len() {
                let c = if row_major { i % cols } else { i / rows };
                line.extend(std::iter::repeat_n(' ', col_widths[c] - lens[i]));
            }
        }
        out.push(line);
    }
    out
}
