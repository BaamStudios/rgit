// SPDX-License-Identifier: MIT
//! When to colour output and with which colours
//! (Documentation/config/color.adoc, `--color[=<when>]`).
//!
//! The setting comes from the command line, else from the command's
//! variable (`color.diff`, `color.branch`, ...), else from `color.ui`,
//! else `auto`. `auto` colours when `TERM` is set and not `dumb`, and
//! stdout is a terminal or goes to a pager (unless `color.pager` is
//! false). Checked against the `git` binary.

use std::io::IsTerminal;

use rgit_config::ConfigSet;
use rgit_config::color::{ColorBool, parse_color, parse_colorbool};
use rgit_options::CallbackError;

use crate::Die;
use crate::messages::config as cmsg;
use crate::messages::pager as msg;

/// Ends a coloured part.
pub const RESET: &str = "\x1b[m";

/// The value of `--color[=<when>]` (`None`: no value) or `--no-color`
/// (`negated`).
pub fn parse_when(arg: Option<&str>, negated: bool) -> Result<ColorBool, CallbackError> {
    if negated {
        return Ok(ColorBool::Never);
    }
    match arg {
        None | Some("always") => Ok(ColorBool::Always),
        Some("never") => Ok(ColorBool::Never),
        Some("auto") => Ok(ColorBool::Auto),
        Some(_) => Err(CallbackError::Error(msg::color_expects("color"))),
    }
}

/// [`parse_when`] for commands that parse their arguments by hand:
/// `Some` for `--color`, `--color=<when>` and `--no-color`.
pub fn parse_arg(arg: &str) -> Option<Result<ColorBool, Die>> {
    let when = match arg {
        "--color" => None,
        "--no-color" => return Some(Ok(ColorBool::Never)),
        _ => Some(arg.strip_prefix("--color=")?),
    };
    Some(parse_when(when, false).map_err(|e| match e {
        CallbackError::Error(m) | CallbackError::Usage(m) | CallbackError::Fatal(m) => {
            Die::Error(m, rgit_options::EXIT_USAGE)
        }
        CallbackError::Silent => Die::Exit(rgit_options::EXIT_USAGE),
    }))
}

/// A colour boolean variable; a value that is none is fatal like in git.
fn colorbool(config: &ConfigSet, key: &str) -> Result<Option<ColorBool>, Die> {
    let Some(e) = config.get(key) else {
        return Ok(None);
    };
    match parse_colorbool(e.value.as_ref().map(|v| v.as_slice())) {
        Some(c) => Ok(Some(c)),
        None => {
            let v = e.value.as_ref().map(|v| v.to_string()).unwrap_or_default();
            Err(Die::Fatal(cmsg::bad_boolean(&v, key)))
        }
    }
}

/// Whether `auto` colours stdout now.
pub fn auto(config: &ConfigSet) -> bool {
    match std::env::var_os("TERM") {
        None => return false,
        Some(t) if t == "dumb" => return false,
        Some(_) => {}
    }
    if std::io::stdout().is_terminal() {
        return true;
    }
    crate::pager::in_use() && config.bool("color.pager").ok().flatten().unwrap_or(true)
}

/// Whether to colour stdout: `cli` from `--color`/`--no-color`, else
/// `var` (e.g. `color.diff`), else `color.ui`.
pub fn want(config: &ConfigSet, var: Option<&str>, cli: Option<ColorBool>) -> Result<bool, Die> {
    let setting = match cli {
        Some(c) => c,
        None => {
            let from_var = match var {
                Some(v) => colorbool(config, v)?,
                None => None,
            };
            match from_var {
                Some(c) => c,
                None => colorbool(config, "color.ui")?.unwrap_or(ColorBool::Auto),
            }
        }
    };
    Ok(match setting {
        ColorBool::Always => true,
        ColorBool::Never => false,
        ColorBool::Auto => auto(config),
    })
}

/// The escape sequence for the colour slot `key` (e.g.
/// `color.diff.meta`), or `default`. An invalid value is fatal.
pub fn slot(config: &ConfigSet, key: &str, default: &str) -> Result<String, Die> {
    let Some(e) = config.get(key) else {
        return Ok(default.to_owned());
    };
    let v = e.value.as_ref().map(|v| v.to_string()).unwrap_or_default();
    match parse_color(&v) {
        Some(seq) => Ok(seq),
        None => {
            eprintln!("error: {}", cmsg::invalid_color(&v));
            Err(Die::Fatal(crate::revisions::unable_to_parse(key, e)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgit_config::{Origin, Scope, parse_bytes};

    fn set(text: &str) -> ConfigSet {
        parse_bytes(text.as_bytes(), Origin::CommandLine, Scope::Command).unwrap()
    }

    #[test]
    fn when_values() {
        assert_eq!(parse_when(None, false), Ok(ColorBool::Always));
        assert_eq!(parse_when(Some("auto"), false), Ok(ColorBool::Auto));
        assert_eq!(parse_when(Some("never"), false), Ok(ColorBool::Never));
        assert_eq!(parse_when(None, true), Ok(ColorBool::Never));
        assert!(parse_when(Some("true"), false).is_err());
    }

    #[test]
    fn precedence() {
        let c = set("[color]\nui = always\ndiff = never\n");
        assert!(!want(&c, Some("color.diff"), None).unwrap());
        assert!(want(&c, Some("color.branch"), None).unwrap());
        assert!(want(&c, Some("color.diff"), Some(ColorBool::Always)).unwrap());
        assert!(want(&set("[color]\nui = bogus\n"), None, None).is_err());
    }

    #[test]
    fn slots() {
        let c = set("[color \"diff\"]\nmeta = reverse\n");
        assert_eq!(slot(&c, "color.diff.meta", "x").unwrap(), "\x1b[7m");
        assert_eq!(slot(&c, "color.diff.frag", "x").unwrap(), "x");
    }
}
