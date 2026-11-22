// SPDX-License-Identifier: MIT

//! Whitespace errors (`core.whitespace`, the `whitespace` attribute;
//! Documentation/config/core.adoc and gitattributes.adoc): parsing the
//! rule list and finding the errors of one line.
//!
//! Where the documentation is silent the behaviour follows the `git`
//! binary:
//!
//! * rule names may be abbreviated to a prefix (`trail`, `space`,
//!   `indent`); unknown names are ignored;
//! * the indent is the leading run of spaces and tabs of the part before
//!   the trailing whitespace (a line of whitespace only has no indent);
//! * `space-before-tab`: spaces before the last tab of the indent;
//!   `tab-in-indent`: tabs of the indent, except (with
//!   `space-before-tab`) a tab that directly follows a space;
//!   `indent-with-non-tab`: at least `tabwidth` spaces right after the
//!   last tab of the indent (or at the start);
//! * with `cr-at-eol` a carriage return at the end belongs to the line
//!   terminator; otherwise it is trailing whitespace.

/// Trailing whitespace (`blank-at-eol`).
pub const BLANK_AT_EOL: u32 = 1 << 0;
/// Spaces before a tab in the indent.
pub const SPACE_BEFORE_TAB: u32 = 1 << 1;
/// Indent with spaces where a tab would do.
pub const INDENT_WITH_NON_TAB: u32 = 1 << 2;
/// Tabs in the indent.
pub const TAB_IN_INDENT: u32 = 1 << 3;
/// Blank lines added at the end of the file.
pub const BLANK_AT_EOF: u32 = 1 << 4;
/// A carriage return before the line feed is not an error.
pub const CR_AT_EOL: u32 = 1 << 5;
/// The last line has no line feed.
pub const INCOMPLETE_LINE: u32 = 1 << 6;

/// The rules that are on unless switched off.
pub const DEFAULT_RULES: u32 = BLANK_AT_EOL | SPACE_BEFORE_TAB | BLANK_AT_EOF;

/// The rules of a set `whitespace` attribute: every error except
/// `tab-in-indent` (which contradicts `indent-with-non-tab`).
pub const ATTRIBUTE_SET_RULES: u32 =
    BLANK_AT_EOL | SPACE_BEFORE_TAB | INDENT_WITH_NON_TAB | BLANK_AT_EOF;

/// Rule names and the bits they stand for, in the order prefixes are
/// tried.
const NAMES: &[(&str, u32)] = &[
    ("trailing-space", BLANK_AT_EOL | BLANK_AT_EOF),
    ("space-before-tab", SPACE_BEFORE_TAB),
    ("indent-with-non-tab", INDENT_WITH_NON_TAB),
    ("cr-at-eol", CR_AT_EOL),
    ("blank-at-eol", BLANK_AT_EOL),
    ("blank-at-eof", BLANK_AT_EOF),
    ("tab-in-indent", TAB_IN_INDENT),
    ("incomplete-line", INCOMPLETE_LINE),
];

/// A set of whitespace rules with the tab width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WsRule {
    pub bits: u32,
    pub tab_width: usize,
}

impl Default for WsRule {
    fn default() -> Self {
        WsRule {
            bits: DEFAULT_RULES,
            tab_width: 8,
        }
    }
}

/// Why a rule list was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsRuleError {
    /// `tab-in-indent` together with `indent-with-non-tab`.
    Conflict,
}

impl WsRule {
    /// No rules at all (an unset `whitespace` attribute).
    pub fn none(tab_width: usize) -> WsRule {
        WsRule { bits: 0, tab_width }
    }

    /// Whether `rule` is on.
    pub fn has(&self, rule: u32) -> bool {
        self.bits & rule != 0
    }

    /// Parses a comma separated list, starting from `base`.
    pub fn parse_from(base: WsRule, value: &str) -> Result<WsRule, WsRuleError> {
        let mut rule = base;
        for item in value.split([',', ' ', '\t', '\n']) {
            if item.is_empty() {
                continue;
            }
            let (neg, name) = match item.strip_prefix('-') {
                Some(n) => (true, n),
                None => (false, item),
            };
            if let Some(w) = name.strip_prefix("tabwidth=") {
                // Values outside 1..=63 are ignored (see
                // [`invalid_tab_widths`]).
                if let Ok(n) = w.parse::<usize>() {
                    if (1..64).contains(&n) {
                        rule.tab_width = n;
                    }
                }
                continue;
            }
            if name.is_empty() {
                continue;
            }
            if let Some(&(_, bits)) = NAMES.iter().find(|(n, _)| n.starts_with(name)) {
                if neg {
                    rule.bits &= !bits;
                } else {
                    rule.bits |= bits;
                }
            }
        }
        if rule.has(TAB_IN_INDENT) && rule.has(INDENT_WITH_NON_TAB) {
            return Err(WsRuleError::Conflict);
        }
        Ok(rule)
    }

    /// Parses `core.whitespace`.
    pub fn parse(value: &str) -> Result<WsRule, WsRuleError> {
        WsRule::parse_from(WsRule::default(), value)
    }
}

/// The `tabwidth=` values of a rule list that are out of range (git warns
/// about them and keeps the previous width).
pub fn invalid_tab_widths(value: &str) -> Vec<&str> {
    value
        .split([',', ' ', '\t', '\n'])
        .filter_map(|item| item.strip_prefix("tabwidth="))
        .filter(|w| !matches!(w.parse::<usize>(), Ok(n) if (1..64).contains(&n)))
        .collect()
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\r' | b'\n')
}

/// Whether a line (without its line feed) is blank for `blank-at-eof`.
pub fn is_blank(line: &[u8]) -> bool {
    line.iter().all(|&c| is_space(c))
}

/// One part of a line for output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// Indent characters that are no error (shown without colour).
    Plain,
    /// A whitespace error.
    Error,
    /// The text (shown in the colour of the line).
    Text,
}

/// The errors of one line and how to show it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineCheck {
    /// The error bits found (`BLANK_AT_EOL`, `SPACE_BEFORE_TAB`, ...).
    pub errors: u32,
    /// Consecutive parts covering the line up to `body_end`.
    pub parts: Vec<(Part, std::ops::Range<usize>)>,
    /// End of the line without a carriage return that belongs to the
    /// terminator (`cr-at-eol`); the rest is shown after the colours.
    pub body_end: usize,
}

/// Checks `line` (without its line feed).
pub fn check_line(line: &[u8], rule: &WsRule) -> LineCheck {
    let body_end = if rule.has(CR_AT_EOL) && line.last() == Some(&b'\r') {
        line.len() - 1
    } else {
        line.len()
    };
    let body = &line[..body_end];
    let mut errors = 0;
    let mut trail = body.len();
    if rule.has(BLANK_AT_EOL) {
        while trail > 0 && is_space(body[trail - 1]) {
            trail -= 1;
        }
        if trail < body.len() {
            errors |= BLANK_AT_EOL;
        }
    }
    // Without `blank-at-eol` a line of whitespace is all indent.
    let indent_end = body[..trail]
        .iter()
        .take_while(|&&c| c == b' ' || c == b'\t')
        .count();
    let indent = &body[..indent_end];
    let last_tab = indent.iter().rposition(|&c| c == b'\t');
    let mut written = last_tab.map_or(0, |t| t + 1);
    let sbt = rule.has(SPACE_BEFORE_TAB) && indent[..written].contains(&b' ');
    if sbt {
        errors |= SPACE_BEFORE_TAB;
    }
    let mut parts: Vec<(Part, std::ops::Range<usize>)> = Vec::new();
    // Erroneous tabs are coloured one by one, everything else in runs.
    let mut push = |part: Part, range: std::ops::Range<usize>| {
        if range.is_empty() {
            return;
        }
        let tab_error = part == Part::Error && body[range.start] == b'\t';
        match parts.last_mut() {
            Some((p, r))
                if *p == part
                    && r.end == range.start
                    && !tab_error
                    && !(part == Part::Error && body[r.start] == b'\t') =>
            {
                r.end = range.end
            }
            _ => parts.push((part, range)),
        }
    };
    for i in 0..written {
        let part = if indent[i] == b' ' {
            if sbt { Part::Error } else { Part::Plain }
        } else if rule.has(TAB_IN_INDENT) && !(sbt && i > 0 && indent[i - 1] == b' ') {
            errors |= TAB_IN_INDENT;
            Part::Error
        } else {
            Part::Plain
        };
        push(part, i..i + 1);
    }
    if rule.has(INDENT_WITH_NON_TAB) {
        let run = indent[written..].iter().take_while(|&&c| c == b' ').count();
        if run >= rule.tab_width {
            errors |= INDENT_WITH_NON_TAB;
            push(Part::Error, written..written + run);
            written += run;
        }
    }
    push(Part::Text, written.min(trail)..trail);
    push(Part::Error, trail..body.len());
    LineCheck {
        errors,
        parts,
        body_end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(line: &str, rule: &str) -> (u32, String) {
        let r = WsRule::parse(rule).unwrap();
        let c = check_line(line.as_bytes(), &r);
        let mut s = String::new();
        for (p, range) in &c.parts {
            let mark = match p {
                Part::Plain => 'p',
                Part::Error => 'E',
                Part::Text => 't',
            };
            for _ in range.clone() {
                s.push(mark);
            }
        }
        (c.errors, s)
    }

    #[test]
    fn parse_rules() {
        let r = WsRule::parse("-trail,indent,tabwidth=4").unwrap();
        assert_eq!(r.bits, SPACE_BEFORE_TAB | INDENT_WITH_NON_TAB);
        assert_eq!(r.tab_width, 4);
        assert_eq!(
            WsRule::parse("tab-in-indent,indent-with-non-tab"),
            Err(WsRuleError::Conflict)
        );
        assert_eq!(WsRule::parse("nonsense").unwrap(), WsRule::default());
    }

    #[test]
    fn indent_errors() {
        assert_eq!(
            shape("\t \tz", "tab-in-indent"),
            (SPACE_BEFORE_TAB | TAB_IN_INDENT, "EEpt".into())
        );
        assert_eq!(
            shape(" \t \t x", "tab-in-indent"),
            (SPACE_BEFORE_TAB, "EpEptt".into())
        );
        assert_eq!(
            shape("\t \tz", "tab-in-indent,-space-before-tab"),
            (TAB_IN_INDENT, "EpEt".into())
        );
        assert_eq!(
            shape("\t        w", "indent"),
            (INDENT_WITH_NON_TAB, "pEEEEEEEEt".into())
        );
        assert_eq!(shape("       w", "indent"), (0, "tttttttt".into()));
        assert_eq!(shape("a \t", ""), (BLANK_AT_EOL, "tEE".into()));
        assert_eq!(shape("\td", ""), (0, "pt".into()));
    }
}
