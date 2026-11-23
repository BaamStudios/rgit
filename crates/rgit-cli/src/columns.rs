// SPDX-License-Identifier: MIT
//! Column layout for `git column` and `status --column` (`column.ui`,
//! `column.<cmd>`; Documentation/git-column.adoc and
//! Documentation/config/column.adoc). `tag` and `branch` use
//! [`crate::column`] (to be unified).
//!
//! The layout was determined from the behaviour of git: every column is as
//! wide as its widest cell plus the padding (the last cell of a row is not
//! padded), and as many columns are used as fit into the width minus the
//! indent. Without `dense` all columns share the width of the widest cell;
//! with `dense` each column is as wide as its own cells and more columns
//! are tried as long as they fit.

use std::io::IsTerminal;

use rgit_config::ConfigSet;
use unicode_width::UnicodeWidthChar;

use crate::messages::column as msg;

/// When columns are used.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Enable {
    #[default]
    Never,
    Always,
    /// When standard output is a terminal.
    Auto,
}

/// How the cells are arranged.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Layout {
    /// Fill the columns first.
    #[default]
    Column,
    /// Fill the rows first.
    Row,
    /// One cell per line.
    Plain,
}

/// A parsed `column.*` value or `--column` argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Mode {
    pub enable: Enable,
    pub layout: Layout,
    pub dense: bool,
}

impl Mode {
    /// Applies the tokens of `value` (separated by spaces or commas) on
    /// top of `self`. A layout or density token without `always`,
    /// `never` or `auto` in the same value switches columns on. Returns
    /// the first unknown token.
    pub fn apply(&mut self, value: &str) -> Result<(), String> {
        let mut enable = None;
        let mut implied = false;
        for tok in value.split([' ', ',']).filter(|t| !t.is_empty()) {
            match tok {
                "always" => enable = Some(Enable::Always),
                "never" => enable = Some(Enable::Never),
                "auto" => enable = Some(Enable::Auto),
                "column" => (self.layout, implied) = (Layout::Column, true),
                "row" => (self.layout, implied) = (Layout::Row, true),
                "plain" => (self.layout, implied) = (Layout::Plain, true),
                "dense" => (self.dense, implied) = (true, true),
                "nodense" => (self.dense, implied) = (false, true),
                other => return Err(other.to_owned()),
            }
        }
        match enable {
            Some(e) => self.enable = e,
            None if implied => self.enable = Enable::Always,
            None => {}
        }
        Ok(())
    }

    /// `column.ui`, then `column.<command>` from `config`; an unknown
    /// token ends the command like in git.
    pub fn from_config(config: &ConfigSet, command: Option<&str>) -> Result<Mode, crate::Die> {
        let mut mode = Mode::default();
        let mut keys = vec!["column.ui".to_owned()];
        if let Some(c) = command {
            keys.push(format!("column.{c}"));
        }
        for key in keys {
            let Some(entry) = config.get(&key) else {
                continue;
            };
            let v = entry
                .value
                .as_ref()
                .map(|v| String::from_utf8_lossy(v).into_owned())
                .unwrap_or_default();
            if let Err(tok) = mode.apply(&v) {
                eprintln!("error: {}", msg::unsupported_option(&tok));
                eprintln!("error: {}", msg::invalid_mode(&key, &v));
                return Err(crate::Die::Fatal(crate::revisions::unable_to_parse(
                    &key, entry,
                )));
            }
        }
        Ok(mode)
    }

    /// Whether columns are used for standard output.
    pub fn active(&self) -> bool {
        match self.enable {
            Enable::Never => false,
            Enable::Always => true,
            Enable::Auto => std::io::stdout().is_terminal() || crate::pager::in_use(),
        }
    }
}

/// Everything [`format`] needs besides the cells.
#[derive(Clone, Debug)]
pub struct Options {
    pub layout: Layout,
    pub dense: bool,
    /// The total width.
    pub width: usize,
    /// Spaces between columns.
    pub padding: usize,
    /// Printed at the start of every line.
    pub indent: String,
    /// Printed at the end of every line.
    pub nl: String,
}

impl Options {
    /// The defaults for `mode`: the terminal width, one space of padding,
    /// no indent.
    pub fn new(mode: Mode) -> Options {
        Options {
            layout: mode.layout,
            dense: mode.dense,
            width: term_width(),
            padding: 1,
            indent: String::new(),
            nl: "\n".to_owned(),
        }
    }
}

/// The terminal width: `$COLUMNS`, else the width of the terminal on
/// standard output or standard error, else 80.
pub fn term_width() -> usize {
    if let Some(w) = std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.trim().parse::<usize>().ok())
        .filter(|w| *w > 0)
    {
        return w;
    }
    ioctl_width().unwrap_or(80)
}

#[cfg(unix)]
fn ioctl_width() -> Option<usize> {
    for fd in [1, 2] {
        let mut ws = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: TIOCGWINSZ writes one `struct winsize` into `ws`.
        let rc = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws as *mut libc::winsize) };
        if rc == 0 && ws.ws_col > 0 {
            return Some(usize::from(ws.ws_col));
        }
    }
    None
}

#[cfg(not(unix))]
fn ioctl_width() -> Option<usize> {
    None
}

/// The width of `s` on a terminal; control characters take none.
pub fn display_width(s: &str) -> usize {
    s.chars()
        .filter(|c| !c.is_control())
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

/// The cells laid out in rows: `(rows, column widths)` for `cols`
/// requested columns.
fn shape(widths: &[usize], cols: usize, layout: Layout, dense: bool) -> (usize, Vec<usize>) {
    let n = widths.len();
    let cols = cols.clamp(1, n.max(1));
    let rows = n.div_ceil(cols);
    let cols = match layout {
        Layout::Column => n.div_ceil(rows),
        _ => cols,
    };
    let max = widths.iter().copied().max().unwrap_or(0);
    let mut col_w = vec![if dense { 0 } else { max }; cols];
    if dense {
        for (i, w) in widths.iter().enumerate() {
            let c = match layout {
                Layout::Column => i / rows,
                _ => i % cols,
            };
            col_w[c] = col_w[c].max(*w);
        }
    }
    (rows, col_w)
}

/// `cells` laid out as `opts` says.
pub fn format(cells: &[String], opts: &Options) -> String {
    let mut out = String::new();
    if cells.is_empty() {
        return out;
    }
    if opts.layout == Layout::Plain {
        for c in cells {
            out.push_str(&opts.indent);
            out.push_str(c);
            out.push_str(&opts.nl);
        }
        return out;
    }
    let widths: Vec<usize> = cells.iter().map(|c| display_width(c)).collect();
    let avail = opts.width.saturating_sub(display_width(&opts.indent));
    let cell = widths.iter().copied().max().unwrap_or(0) + opts.padding;
    let mut cols = (avail / cell.max(1)).max(1);
    let (mut rows, mut col_w) = shape(&widths, cols, opts.layout, false);
    if opts.dense {
        (rows, col_w) = shape(&widths, cols, opts.layout, true);
        while cols < cells.len() {
            let (r, w) = shape(&widths, cols + 1, opts.layout, true);
            if w.iter().map(|w| w + opts.padding).sum::<usize>() > avail {
                break;
            }
            cols += 1;
            (rows, col_w) = (r, w);
        }
    }
    let ncols = col_w.len();
    for r in 0..rows {
        let row: Vec<usize> = (0..ncols)
            .map(|c| match opts.layout {
                Layout::Column => c * rows + r,
                _ => r * ncols + c,
            })
            .filter(|&i| i < cells.len())
            .collect();
        out.push_str(&opts.indent);
        for (k, &i) in row.iter().enumerate() {
            out.push_str(&cells[i]);
            if k + 1 < row.len() {
                let c = match opts.layout {
                    Layout::Column => i / rows,
                    _ => i % ncols,
                };
                let pad = col_w[c] + opts.padding - widths[i];
                out.extend(std::iter::repeat_n(' ', pad));
            }
        }
        out.push_str(&opts.nl);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cells() -> Vec<String> {
        "one two three four five six seven eight nine ten eleven"
            .split(' ')
            .map(String::from)
            .collect()
    }

    fn opts(layout: Layout, dense: bool, width: usize) -> Options {
        Options {
            layout,
            dense,
            width,
            padding: 1,
            indent: String::new(),
            nl: "\n".into(),
        }
    }

    #[test]
    fn layouts() {
        let c = cells();
        assert_eq!(
            format(&c, &opts(Layout::Column, false, 20)),
            "one    seven\ntwo    eight\nthree  nine\nfour   ten\nfive   eleven\nsix\n"
        );
        assert_eq!(
            format(&c, &opts(Layout::Column, true, 20)),
            "one   five  nine\ntwo   six   ten\nthree seven eleven\nfour  eight\n"
        );
        assert_eq!(
            format(&c, &opts(Layout::Row, true, 20)),
            "one   two    three\nfour  five   six\nseven eight  nine\nten   eleven\n"
        );
        assert_eq!(
            format(&c, &opts(Layout::Column, false, 13)).lines().count(),
            11
        );
    }

    #[test]
    fn modes() {
        let mut m = Mode::default();
        m.apply("row").unwrap();
        assert_eq!(m.enable, Enable::Always);
        m.apply("never,column").unwrap();
        assert_eq!(m.enable, Enable::Never);
        assert_eq!(m.apply("bogus"), Err("bogus".into()));
    }
}
