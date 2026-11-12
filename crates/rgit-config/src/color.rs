// SPDX-License-Identifier: MIT
//! The `color` value type: a color specification such as `bold red blue`
//! turned into an ANSI escape sequence, and color booleans
//! (`color.ui = auto`).
//!
//! Written from git-config(1) ("color" under "Values") and checked against
//! the `git` binary: the sequence lists `reset` first (as an empty
//! parameter), then the attributes in ascending code order, then the
//! foreground and background color.

/// A color word: basic (0-7), bright (0-7), 256-color index or RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Color {
    Normal,
    Default,
    Basic(u8),
    Bright(u8),
    Index(u8),
    Rgb(u8, u8, u8),
}

const NAMES: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
];

/// Attribute words and their SGR codes; `no`/`no-` prefixed forms use the
/// second code.
const ATTRS: [(&str, u8, u8); 7] = [
    ("bold", 1, 22),
    ("dim", 2, 22),
    ("italic", 3, 23),
    ("ul", 4, 24),
    ("blink", 5, 25),
    ("reverse", 7, 27),
    ("strike", 9, 29),
];

fn parse_color_word(word: &str) -> Option<Color> {
    let w = word.to_ascii_lowercase();
    match w.as_str() {
        "normal" => return Some(Color::Normal),
        "default" => return Some(Color::Default),
        _ => {}
    }
    if let Some(i) = NAMES.iter().position(|n| *n == w) {
        return Some(Color::Basic(i as u8));
    }
    if let Some(rest) = w.strip_prefix("bright") {
        if let Some(i) = NAMES.iter().position(|n| *n == rest) {
            return Some(Color::Bright(i as u8));
        }
    }
    if let Some(hex) = w.strip_prefix('#') {
        let digit = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
        let b = hex.as_bytes();
        return match b.len() {
            3 => {
                let r = digit(b[0])?;
                let g = digit(b[1])?;
                let bl = digit(b[2])?;
                Some(Color::Rgb(r * 17, g * 17, bl * 17))
            }
            6 => {
                let v = |i: usize| Some(digit(b[i])? * 16 + digit(b[i + 1])?);
                Some(Color::Rgb(v(0)?, v(2)?, v(4)?))
            }
            _ => None,
        };
    }
    let n: i64 = w.parse().ok()?;
    match n {
        -1 => Some(Color::Normal),
        0..=7 => Some(Color::Basic(n as u8)),
        8..=15 => Some(Color::Bright(n as u8 - 8)),
        16..=255 => Some(Color::Index(n as u8)),
        _ => None,
    }
}

/// The SGR parameters for a color; `base` is 30 (foreground) or 40.
fn color_code(c: Color, base: u16) -> Option<String> {
    Some(match c {
        Color::Normal => return None,
        Color::Default => format!("{}", base + 9),
        Color::Basic(i) => format!("{}", base + u16::from(i)),
        Color::Bright(i) => format!("{}", base + 60 + u16::from(i)),
        Color::Index(i) => format!("{};5;{i}", base + 8),
        Color::Rgb(r, g, b) => format!("{};2;{r};{g};{b}", base + 8),
    })
}

/// Turn a color specification into its escape sequence (empty for no
/// effect). `None` if the value is not a valid color.
pub fn parse_color(value: &str) -> Option<String> {
    let mut fg = None;
    let mut bg = None;
    let mut reset = false;
    let mut codes: Vec<u8> = Vec::new();
    for word in value
        .split([' ', '\t', '\n', '\r'])
        .filter(|w| !w.is_empty())
    {
        if let Some(c) = parse_color_word(word) {
            if fg.is_none() {
                fg = Some(c);
            } else if bg.is_none() {
                bg = Some(c);
            } else {
                return None;
            }
            continue;
        }
        let w = word.to_ascii_lowercase();
        if w == "reset" {
            reset = true;
            continue;
        }
        let (negated, name) = match w.strip_prefix("no") {
            Some(rest) => (true, rest.strip_prefix('-').unwrap_or(rest)),
            None => (false, w.as_str()),
        };
        let (_, on, off) = ATTRS.iter().find(|(n, _, _)| *n == name)?;
        let code = if negated { *off } else { *on };
        if !codes.contains(&code) {
            codes.push(code);
        }
    }
    codes.sort_unstable();
    let mut parts: Vec<String> = Vec::new();
    if reset {
        parts.push(String::new());
    }
    parts.extend(codes.iter().map(|c| c.to_string()));
    parts.extend(fg.and_then(|c| color_code(c, 30)));
    parts.extend(bg.and_then(|c| color_code(c, 40)));
    if parts.is_empty() {
        return Some(String::new());
    }
    Some(format!("\x1b[{}m", parts.join(";")))
}

/// A color boolean (`color.ui`, `color.diff`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorBool {
    /// `never`, `false`, ...
    Never,
    /// `always`.
    Always,
    /// `auto`, or `true`: color when writing to a terminal.
    Auto,
}

/// Parse a color boolean; `None` for a value that is neither a boolean nor
/// `never`/`always`/`auto`.
pub fn parse_colorbool(value: Option<&[u8]>) -> Option<ColorBool> {
    match value {
        Some(v) if v.eq_ignore_ascii_case(b"never") => Some(ColorBool::Never),
        Some(v) if v.eq_ignore_ascii_case(b"always") => Some(ColorBool::Always),
        Some(v) if v.eq_ignore_ascii_case(b"auto") => Some(ColorBool::Auto),
        v => {
            crate::value::parse_bool(v).map(|b| if b { ColorBool::Auto } else { ColorBool::Never })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors() {
        for (spec, want) in [
            ("red", "\x1b[31m"),
            ("bold red", "\x1b[1;31m"),
            ("red bold blue", "\x1b[1;31;44m"),
            ("normal brightred", "\x1b[101m"),
            ("8", "\x1b[90m"),
            ("196", "\x1b[38;5;196m"),
            ("#f1b", "\x1b[38;2;255;17;187m"),
            ("nobold nodim", "\x1b[22m"),
            ("reset", "\x1b[m"),
            ("ul reset", "\x1b[;4m"),
            ("default default", "\x1b[39;49m"),
            ("", ""),
            ("normal", ""),
            ("-1 red", "\x1b[41m"),
        ] {
            assert_eq!(parse_color(spec).as_deref(), Some(want), "{spec:?}");
        }
        for bad in [
            "256",
            "red blue green",
            "foo",
            "brightdefault",
            "#ff",
            "no-reset",
        ] {
            assert_eq!(parse_color(bad), None, "{bad:?}");
        }
    }
}
