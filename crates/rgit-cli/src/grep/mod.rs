// SPDX-License-Identifier: MIT
//! Searching and printing for `git grep` (Documentation/git-grep.adoc,
//! Documentation/config/grep.adoc, `color.grep.<slot>` in
//! Documentation/config/color.adoc).
//!
//! The output format was taken from the `git` binary:
//!
//! - A line is `<name><sep><line number><sep><column><sep><text>`, the
//!   fields present as asked for; `<sep>` is `:` for selected lines, `-`
//!   for context lines and `=` for function lines (`-p`, `-W`). With `-z`
//!   every separator is a NUL. `--heading` prints the name on a line of its
//!   own instead.
//! - `--` separates groups of lines that are not adjacent when context is
//!   shown (`-A`, `-B`, `-C`, `-W`), also between files; `--break` prints
//!   an empty line between files instead.
//! - `-p` shows the closest line before a selected line that looks like a
//!   function header (even when the selected line looks like one itself),
//!   unless it was already shown. `-W` shows the whole function: from the
//!   header at or before the line, extended upwards over lines that are
//!   neither blank nor headers, down to the line before the next header,
//!   without trailing blank lines.
//! - A file that looks binary (a NUL in its first 8000 bytes, or the
//!   `diff` attribute unset) prints `Binary file <name> matches` instead of
//!   lines; with context it prints nothing but still counts as output for
//!   the `--` between files.
//! - Colours: every match in a shown line is coloured (`matchSelected`
//!   in selected lines, `matchContext` in context and function lines), the
//!   text around it in `selected`, `context` or `function`.
//! - A search for exactly `^$` also reports the empty "line" after the
//!   final newline of a file whose last line is not empty.

pub mod pattern;

use regex::bytes::Regex;

use crate::color::RESET;
use crate::messages::grep as msg;
use pattern::Matcher;

/// Bytes looked at to decide whether a file is binary.
const BINARY_CHECK: usize = 8000;

/// Whether `data` looks binary (a NUL in the first 8000 bytes).
pub fn looks_binary(data: &[u8]) -> bool {
    data[..data.len().min(BINARY_CHECK)].contains(&0)
}

/// How function header lines are recognised (`-p`, `-W`).
pub enum FuncName {
    /// The built-in rule: a line starting with a letter, `_` or `$`.
    Default,
    /// `diff.<driver>.funcname` / `xfuncname`: patterns tried in order;
    /// the first that matches decides (`!` patterns reject the line).
    Custom(Vec<(bool, Regex)>),
}

impl FuncName {
    /// Compile the value of `diff.<driver>.funcname` (`extended`: of
    /// `xfuncname`). Lines that do not compile are skipped.
    pub fn custom(value: &str, extended: bool) -> FuncName {
        let mut pats = Vec::new();
        for line in value.split('\n') {
            let (neg, p) = match line.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, line),
            };
            let Ok(text) = rgit_revwalk::walk::grep::translate(p, extended) else {
                continue;
            };
            if let Ok(re) = regex::bytes::RegexBuilder::new(&text)
                .unicode(false)
                .build()
            {
                pats.push((neg, re));
            }
        }
        FuncName::Custom(pats)
    }

    fn is_func(&self, line: &[u8]) -> bool {
        match self {
            FuncName::Default => rgit_diff::default_funcname(line),
            FuncName::Custom(pats) => pats
                .iter()
                .find(|(_, re)| re.is_match(line))
                .is_some_and(|(neg, _)| !neg),
        }
    }
}

/// Output options.
#[derive(Debug, Default, Clone)]
pub struct Options {
    pub line_number: bool,
    pub column: bool,
    /// Not `-h`.
    pub with_filename: bool,
    pub files_with_matches: bool,
    pub files_without_match: bool,
    pub count: bool,
    pub quiet: bool,
    pub only_matching: bool,
    pub null: bool,
    pub heading: bool,
    pub file_break: bool,
    pub before: usize,
    pub after: usize,
    pub show_function: bool,
    pub function_context: bool,
    pub max_count: Option<usize>,
    /// `-a`.
    pub text: bool,
    /// `-I`.
    pub skip_binary: bool,
}

/// The escape sequences of the colour slots; `None` without colour.
#[derive(Debug, Default, Clone)]
pub struct Colors {
    pub filename: String,
    pub separator: String,
    pub line_number: String,
    pub column: String,
    pub function: String,
    pub match_selected: String,
    pub match_context: String,
    pub selected: String,
    pub context: String,
}

/// One file to search.
pub struct Source<'a> {
    /// The name as shown (with the revision prefix, quoted as needed).
    pub name: &'a [u8],
    pub data: &'a [u8],
    /// The `diff` attribute or `-a`/`--textconv` decided already:
    /// `Some(true)` binary, `Some(false)` text; `None` looks at the data.
    pub binary: Option<bool>,
    pub funcname: &'a FuncName,
}

/// Searches files and prints the results.
pub struct Printer {
    pub opts: Options,
    colors: Option<Colors>,
    pub out: Vec<u8>,
    /// Something was printed in line mode (for `--` and `--break`).
    shown_any: bool,
    /// The exit status is 0: a file matched (with `-L`: a file was
    /// listed).
    pub hit: bool,
    /// `-O`: the names of matching files are collected here instead of
    /// being printed.
    pub collect: Option<Vec<Vec<u8>>>,
}

/// The state of the file being printed in line mode.
struct FileOut<'a, 'b> {
    p: &'a mut Printer,
    m: &'a Matcher,
    src: &'a Source<'b>,
    lines: &'a [&'b [u8]],
    started: bool,
    last: Option<usize>,
}

fn seg(out: &mut Vec<u8>, color: &str, text: &[u8]) {
    if color.is_empty() {
        out.extend_from_slice(text);
    } else {
        out.extend_from_slice(color.as_bytes());
        out.extend_from_slice(text);
        out.extend_from_slice(RESET.as_bytes());
    }
}

fn is_blank(line: &[u8]) -> bool {
    line.iter().all(u8::is_ascii_whitespace)
}

/// Split into lines without their `\n`.
fn split_lines(data: &[u8]) -> Vec<&[u8]> {
    let mut lines: Vec<&[u8]> = data.split(|&b| b == b'\n').collect();
    if data.is_empty() || data.ends_with(b"\n") {
        lines.pop();
    }
    lines
}

impl Printer {
    pub fn new(opts: Options, colors: Option<Colors>) -> Printer {
        Printer {
            opts,
            colors,
            out: Vec::new(),
            shown_any: false,
            hit: false,
            collect: None,
        }
    }

    fn context_mode(&self) -> bool {
        self.opts.before > 0 || self.opts.after > 0 || self.opts.function_context
    }

    fn colored(&mut self, slot: fn(&Colors) -> &str, text: &[u8]) {
        match &self.colors {
            Some(c) => seg(&mut self.out, slot(c), text),
            None => self.out.extend_from_slice(text),
        }
    }

    fn name(&mut self, name: &[u8]) {
        self.colored(|c| &c.filename, name);
    }

    fn sep(&mut self, sep: u8) {
        if self.opts.null {
            self.out.push(0);
        } else {
            self.colored(|c| &c.separator, &[sep]);
        }
    }

    /// Search one file.
    pub fn grep(&mut self, m: &Matcher, src: &Source<'_>) {
        let binary = !self.opts.text && src.binary.unwrap_or_else(|| looks_binary(src.data));
        if binary && self.opts.skip_binary {
            return;
        }
        let lines = if m.rules_out(src.data) {
            Vec::new()
        } else {
            let mut l = split_lines(src.data);
            if m.empty_line_at_end
                && src.data.ends_with(b"\n")
                && l.last().is_some_and(|last| !last.is_empty())
            {
                l.push(&src.data[src.data.len()..]);
            }
            l
        };
        let max = self.opts.max_count.unwrap_or(usize::MAX);
        let mut selected = vec![false; lines.len()];
        let mut count = 0;
        let mut hits = vec![false; m.term_count()];
        for (i, l) in lines.iter().enumerate() {
            if !hits.is_empty() {
                m.term_hits(l, &mut hits);
            }
            if count < max && m.matches(l) != m.invert {
                selected[i] = true;
                count += 1;
            }
        }
        if hits.iter().any(|h| !h) {
            count = 0;
            selected.iter_mut().for_each(|s| *s = false);
        }
        let without = self.opts.files_without_match;
        if self.opts.quiet {
            self.hit |= if without { count == 0 } else { count > 0 };
            return;
        }
        if without {
            if count == 0 {
                self.hit = true;
                self.name_line(src.name);
            }
            return;
        }
        if count == 0 {
            return;
        }
        self.hit = true;
        if self.opts.files_with_matches {
            self.name_line(src.name);
            return;
        }
        if self.opts.count {
            if self.opts.with_filename {
                self.name(src.name);
                self.sep(b':');
            }
            self.out.extend_from_slice(format!("{count}\n").as_bytes());
            return;
        }
        if binary {
            if !self.context_mode() {
                self.out.extend_from_slice(msg::BINARY_BEFORE.as_bytes());
                self.name(src.name);
                self.out.extend_from_slice(msg::BINARY_AFTER.as_bytes());
            }
            self.shown_any = true;
            return;
        }
        let mut f = FileOut {
            p: self,
            m,
            src,
            lines: &lines,
            started: false,
            last: None,
        };
        f.run(&selected);
    }

    /// `-l`, `-L`: the name alone.
    fn name_line(&mut self, name: &[u8]) {
        if let Some(c) = self.collect.as_mut() {
            c.push(name.to_vec());
            return;
        }
        self.name(name);
        self.out.push(if self.opts.null { 0 } else { b'\n' });
    }
}

impl FileOut<'_, '_> {
    fn is_func(&self, i: usize) -> bool {
        self.src.funcname.is_func(self.lines[i])
    }

    fn run(&mut self, selected: &[bool]) {
        let opts = self.p.opts.clone();
        let n = self.lines.len();
        let mut after_end: Option<usize> = None;
        let last_selected = selected.iter().rposition(|s| *s);
        for (i, &sel) in selected.iter().enumerate() {
            if sel {
                let lo = self.last.map_or(0, |l| l + 1);
                let mut start = i.saturating_sub(opts.before).max(lo);
                let mut func = None;
                if opts.function_context {
                    let f = (0..=i).rev().find(|&k| self.is_func(k));
                    let mut s0 = f.unwrap_or(0);
                    if f.is_some() {
                        while s0 > 0 && !is_blank(self.lines[s0 - 1]) && !self.is_func(s0 - 1) {
                            s0 -= 1;
                        }
                    }
                    start = start.min(s0.max(lo));
                    func = f.filter(|&f| f < i);
                }
                if opts.show_function {
                    func = (0..i).rev().find(|&k| self.is_func(k));
                }
                if let Some(f) = func.filter(|&f| f >= lo && f < start) {
                    self.line(f, b'=');
                }
                for k in start..i {
                    self.line(k, if Some(k) == func { b'=' } else { b'-' });
                }
                self.line(i, b':');
                let mut end = (i + opts.after).min(n - 1);
                if opts.function_context {
                    let next = (i + 1..n).find(|&k| self.is_func(k)).unwrap_or(n);
                    let mut e = next - 1;
                    while e > i && is_blank(self.lines[e]) {
                        e -= 1;
                    }
                    end = end.max(e);
                }
                after_end = Some(after_end.map_or(end, |a| a.max(end)));
            } else if after_end.is_some_and(|a| i <= a) {
                self.line(i, b'-');
            } else if last_selected.is_none_or(|l| i > l) {
                break;
            }
        }
    }

    /// Separators before line `i`: between files, and between groups.
    fn begin(&mut self, i: usize) {
        let ctx = self.p.context_mode();
        if !self.started {
            self.started = true;
            if self.p.shown_any {
                if self.p.opts.file_break {
                    self.p.out.push(b'\n');
                } else if ctx {
                    self.group_sep();
                }
            }
            if self.p.opts.heading {
                self.p.name(self.src.name);
                self.p.out.push(b'\n');
            }
            self.p.shown_any = true;
        } else if ctx && self.last.is_some_and(|l| i > l + 1) {
            self.group_sep();
        }
        self.last = Some(i);
    }

    fn group_sep(&mut self) {
        self.p.colored(|c| &c.separator, b"--");
        self.p.out.push(b'\n');
    }

    fn prefix(&mut self, i: usize, sep: u8, column: Option<usize>) {
        if self.p.opts.with_filename && !self.p.opts.heading {
            self.p.name(self.src.name);
            self.p.sep(sep);
        }
        if self.p.opts.line_number {
            self.p
                .colored(|c| &c.line_number, (i + 1).to_string().as_bytes());
            self.p.sep(sep);
        }
        if let Some(col) = column {
            self.p.colored(|c| &c.column, col.to_string().as_bytes());
            self.p.sep(sep);
        }
    }

    /// Print line `i` as a selected (`:`), context (`-`) or function (`=`)
    /// line.
    fn line(&mut self, i: usize, sep: u8) {
        let text = self.lines[i];
        if self.p.opts.only_matching {
            if sep != b':' {
                return;
            }
            let mut from = 0;
            while let Some((s, e)) = self.m.next_match(text, from) {
                if s == e {
                    break;
                }
                self.begin(i);
                let col = self.p.opts.column.then_some(s + 1);
                self.prefix(i, sep, col);
                self.p.colored(|c| &c.match_selected, &text[s..e]);
                self.p.out.push(b'\n');
                from = e;
            }
            return;
        }
        self.begin(i);
        let col = (self.p.opts.column && sep == b':').then(|| self.m.column(text));
        self.prefix(i, sep, col);
        let Some(colors) = self.p.colors.clone() else {
            self.p.out.extend_from_slice(text);
            self.p.out.push(b'\n');
            return;
        };
        let out = &mut self.p.out;
        let (line_c, match_c) = match sep {
            b':' => (&colors.selected, &colors.match_selected),
            b'=' => (&colors.function, &colors.match_context),
            _ => (&colors.context, &colors.match_context),
        };
        let mut from = 0;
        while let Some((s, e)) = self.m.next_match(text, from) {
            if s == e {
                break;
            }
            seg(out, line_c, &text[from..s]);
            seg(out, match_c, &text[s..e]);
            from = e;
        }
        seg(out, line_c, &text[from..]);
        out.push(b'\n');
    }
}

#[cfg(test)]
mod tests {
    use super::pattern::{Atom, Syntax, Token, parse};
    use super::*;

    fn run(opts: Options, pats: &[&str], data: &[u8]) -> String {
        let tokens: Vec<Token> = pats.iter().map(|p| Token::Pattern(p.to_string())).collect();
        let (e, atoms) = parse(&tokens).unwrap();
        let atoms = atoms
            .iter()
            .map(|p| Atom::new(p, Syntax::Basic, false, false, false).unwrap())
            .collect();
        let m = Matcher::new(e, atoms, false, false);
        let mut p = Printer::new(opts, None);
        let f = FuncName::Default;
        p.grep(
            &m,
            &Source {
                name: b"f",
                data,
                binary: None,
                funcname: &f,
            },
        );
        String::from_utf8(p.out).unwrap()
    }

    const FILE: &[u8] = b"a1\n\n# c1\n# c2\nfunc one\n x\n\n\ncode\n# c3\nfunc two\n y\n\n";

    #[test]
    fn function_context() {
        let opts = Options {
            line_number: true,
            with_filename: true,
            function_context: true,
            ..Default::default()
        };
        assert_eq!(
            run(opts.clone(), &["x"], FILE),
            "f-3-# c1\nf-4-# c2\nf=5=func one\nf:6: x\n"
        );
        assert_eq!(
            run(opts, &["y"], FILE),
            "f-10-# c3\nf=11=func two\nf:12: y\n"
        );
    }

    #[test]
    fn show_function() {
        let opts = Options {
            line_number: true,
            with_filename: true,
            show_function: true,
            ..Default::default()
        };
        assert_eq!(
            run(opts, &["one", "y"], FILE),
            "f=1=a1\nf:5:func one\nf=11=func two\nf:12: y\n"
        );
    }

    #[test]
    fn context_and_max_count() {
        let opts = Options {
            with_filename: true,
            before: 1,
            after: 1,
            max_count: Some(1),
            ..Default::default()
        };
        assert_eq!(
            run(
                opts,
                &["foo_mmap"],
                b"foo mmap bar\nfoo_mmap bar\nfoo_mmap bar mmap\nx\n"
            ),
            "f-foo mmap bar\nf:foo_mmap bar\nf-foo_mmap bar mmap\n"
        );
    }

    #[test]
    fn binary() {
        let opts = Options {
            with_filename: true,
            ..Default::default()
        };
        assert_eq!(
            run(opts, &["ina"], b"bin\0binary\n"),
            "Binary file f matches\n"
        );
    }
}
