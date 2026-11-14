// SPDX-License-Identifier: MIT
//! `format:` and `tformat:` strings of `log` and `rev-list`
//! (Documentation/pretty-formats.adoc, "PRETTY FORMATS").
//!
//! A format string is expanded once per commit. Behaviour that the
//! documentation leaves open was taken from the reference `git`:
//!
//! - Text that does not form a known placeholder is copied as is, `%`
//!   included.
//! - A padding placeholder (`%<(...)`, `%>(...)`, ...) applies to the next
//!   placeholder other than `%%` and `%C...`, whatever it is; when that is
//!   another padding placeholder, it pads its empty expansion and its own
//!   setting is dropped. An unknown truncation mode is printed literally,
//!   the padding still applies.
//! - `%>>` first takes spaces at the end of the output before it when the
//!   expansion is too wide, then pads or truncates.
//! - `%+`, `%-` and `% ` look at whether the output changed; the newline or
//!   space goes where the output ended before the placeholder.
//! - `%C(auto)` resets the colour when something was output before it, and
//!   colours `%H`, `%h` and the decorations until the next colour
//!   placeholder.
//! - `%w(...)` wraps everything from it to the next `%w` or the end.

use rgit_config::value::parse_bool_text;
use rgit_date::{DateContext, DateMode, DateStyle};
use rgit_hash::ObjectId;
use rgit_object::{Commit, SignatureRef};

use super::decorate::{Colors, Parts};
use super::layout;
use super::mailmap::Mailmap;
use super::pretty::{message_lines, subject};

/// Repository lookups the placeholders need.
pub trait Lookup {
    /// `id` abbreviated (`%h`, `%t`, `%p`).
    fn abbrev(&self, id: &ObjectId) -> String;
    /// The decoration of `id` with the given parts (`%d`, `%D`).
    fn decoration(&self, id: &ObjectId, parts: &Parts, colors: Option<&Colors>) -> Vec<u8>;
    /// The note of `id` (`%N`).
    fn notes(&self, id: &ObjectId) -> Option<Vec<u8>>;
}

/// Settings shared by every commit.
pub struct Settings {
    pub mailmap: Mailmap,
    pub date_mode: DateMode,
    pub date_ctx: DateContext,
    /// Colours are on (`--color`, `color.diff`, `color.ui`).
    pub color: bool,
    /// The colours of `%C(auto)` (when colouring).
    pub colors: Option<Colors>,
    pub trailers: rgit_trailer::Config,
    /// The terminal width for negative `%<|(...)` columns.
    pub columns: usize,
    /// `%N` expands (not after `--no-notes`).
    pub notes: bool,
    /// `%S` expands (`log` only).
    pub source: bool,
}

/// The commit being formatted.
pub struct Item<'a> {
    pub id: &'a ObjectId,
    pub commit: &'a Commit,
    /// The parents as the walk shows them.
    pub parents: &'a [ObjectId],
    /// `%m`: `<`, `>` or `-`.
    pub mark: u8,
    /// `%S`: the revision the commit was reached from.
    pub source: Option<&'a str>,
}

/// The largest width, indent or column a placeholder accepts (larger
/// numbers make it literal text; the limit was found with the reference
/// `git`).
const MAX_NUMBER: usize = 16384;

/// A colour that cannot be parsed (`%C(<bad>)` while colouring).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadColor(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Align {
    Left,
    Right,
    /// `%>>`: right, taking spaces from the left.
    Steal,
    Center,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trunc {
    None,
    End,
    Start,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Size {
    Width(usize),
    /// Up to this column.
    Column(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pad {
    align: Align,
    size: Size,
    trunc: Trunc,
}

/// `%w(<width>,<indent1>,<indent2>)` in effect from `start`.
#[derive(Debug, Clone, Copy)]
struct Wrap {
    start: usize,
    width: usize,
    indent1: usize,
    indent2: usize,
}

struct Expander<'a, L: Lookup> {
    s: &'a Settings,
    item: &'a Item<'a>,
    lookup: &'a L,
    auto_color: bool,
    pending: Option<Pad>,
    wrap: Option<Wrap>,
    error: Option<BadColor>,
}

/// Expand `fmt` for one commit.
pub fn expand<L: Lookup>(
    fmt: &[u8],
    s: &Settings,
    item: &Item<'_>,
    lookup: &L,
) -> Result<Vec<u8>, BadColor> {
    let mut e = Expander {
        s,
        item,
        lookup,
        auto_color: false,
        pending: None,
        wrap: None,
        error: None,
    };
    let mut out = Vec::with_capacity(fmt.len() + 64);
    let mut i = 0;
    while i < fmt.len() {
        let Some(p) = fmt[i..].iter().position(|&b| b == b'%') else {
            out.extend_from_slice(&fmt[i..]);
            break;
        };
        out.extend_from_slice(&fmt[i..i + p]);
        i += p + 1;
        let (magic, body) = match fmt.get(i) {
            Some(&m @ (b'+' | b'-' | b' ')) => (Some(m), &fmt[i + 1..]),
            _ => (None, &fmt[i..]),
        };
        let before = out.len();
        let used = if magic.is_some() && body.first() == Some(&b'w') {
            0
        } else {
            e.placeholder(body, &mut out)
        };
        if let Some(err) = e.error.take() {
            return Err(err);
        }
        if used == 0 {
            out.push(b'%');
            continue;
        }
        let changed = out.len() != before;
        match magic {
            Some(b'+') if changed => out.insert(before.min(out.len()), b'\n'),
            Some(b' ') if changed => out.insert(before.min(out.len()), b' '),
            Some(b'-') if !changed => {
                while out.last() == Some(&b'\n') {
                    out.pop();
                }
            }
            _ => {}
        }
        i += usize::from(magic.is_some()) + used;
    }
    e.finish_wrap(&mut out);
    Ok(out)
}

/// `%n`, `%%` and `%xNN` in an option value; other text is kept.
fn literal_codes(v: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len());
    let mut i = 0;
    while i < v.len() {
        if v[i] == b'%' {
            match v.get(i + 1) {
                Some(b'n') => {
                    out.push(b'\n');
                    i += 2;
                    continue;
                }
                Some(b'%') => {
                    out.push(b'%');
                    i += 2;
                    continue;
                }
                Some(b'x') => {
                    if let Some(b) = hex_byte(&v[i + 2..]) {
                        out.push(b);
                        i += 4;
                        continue;
                    }
                }
                _ => {}
            }
        }
        out.push(v[i]);
        i += 1;
    }
    out
}

fn hex_byte(s: &[u8]) -> Option<u8> {
    let digit = |b: u8| (b as char).to_digit(16);
    let hi = digit(*s.first()?)?;
    let lo = digit(*s.get(1)?)?;
    u8::try_from(hi * 16 + lo).ok()
}

/// The text up to the closing parenthesis of `%...(`; `body` starts after
/// the `(`.
fn in_parens(body: &[u8]) -> Option<&[u8]> {
    let close = body.iter().position(|&b| b == b')')?;
    Some(&body[..close])
}

/// `±hhmm` as an integer, the form `rgit-date` takes.
fn tz_hhmm(offset_minutes: i32) -> i32 {
    let abs = offset_minutes.abs();
    let v = abs / 60 * 100 + abs % 60;
    if offset_minutes < 0 { -v } else { v }
}

/// `%f`: the first line of the subject with runs of characters other than
/// ASCII letters, digits, `.` and `_` turned into one `-` (none at the
/// start), runs of dots into one dot, and without dots and dashes at the
/// end.
fn sanitize(message: &[u8]) -> Vec<u8> {
    let lines = message_lines(message);
    let first = lines.first().copied().unwrap_or_default();
    let mut out = Vec::with_capacity(first.len());
    let mut dash = false;
    for &b in first {
        if b.is_ascii_alphanumeric() || b == b'.' || b == b'_' {
            if dash && !out.is_empty() {
                out.push(b'-');
            }
            dash = false;
            // Two dots in a row become one.
            if b == b'.' && out.last() == Some(&b'.') {
                continue;
            }
            out.push(b);
        } else {
            dash = true;
        }
    }
    while matches!(out.last(), Some(b'.' | b'-')) {
        out.pop();
    }
    out
}

/// The body: the message after the subject paragraph and the empty lines
/// after it.
fn message_body(message: &[u8]) -> &[u8] {
    // Leading empty lines, the subject paragraph, then empty lines.
    let mut rest = message;
    let mut seen_text = false;
    while !rest.is_empty() {
        let (line, next) = match rest.iter().position(|&b| b == b'\n') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, &rest[rest.len()..]),
        };
        let blank = line.iter().all(u8::is_ascii_whitespace);
        if !blank {
            seen_text = true;
        } else if seen_text {
            // Skip the blank lines after the subject.
            rest = next;
            while let Some(i) = rest.iter().position(|&b| b == b'\n') {
                if !rest[..i].iter().all(u8::is_ascii_whitespace) {
                    break;
                }
                rest = &rest[i + 1..];
            }
            return rest;
        }
        rest = next;
    }
    rest
}

impl<L: Lookup> Expander<'_, L> {
    fn color_on(&self) -> bool {
        self.s.color && self.auto_color
    }

    fn finish_wrap(&mut self, out: &mut Vec<u8>) {
        if let Some(w) = self.wrap.take() {
            let start = w.start.min(out.len());
            let text = out.split_off(start);
            out.extend(layout::wrap(&text, w.width, w.indent1, w.indent2));
        }
    }

    /// One placeholder at the start of `body` (after `%` and a modifier);
    /// returns the bytes used, 0 if it is none.
    fn placeholder(&mut self, body: &[u8], out: &mut Vec<u8>) -> usize {
        match body.first() {
            Some(b'%') => {
                out.push(b'%');
                return 1;
            }
            Some(b'C') => return self.color(body, out),
            _ => {}
        }
        let Some(pad) = self.pending else {
            return self.one(body, out);
        };
        let start = out.len();
        let used = self.one(body, out);
        // Even text that is no placeholder takes the padding (of nothing).
        self.pending = None;
        self.apply_pad(out, start, pad);
        used
    }

    fn apply_pad(&self, out: &mut Vec<u8>, mut start: usize, pad: Pad) {
        let w = layout::width(&out[start..]);
        let mut n = match pad.size {
            Size::Width(n) => n,
            Size::Column(c) => c.saturating_sub(layout::column(&out[..start])),
        };
        if n == 0 {
            return;
        }
        if w > n && pad.align == Align::Steal {
            let spaces = out[..start]
                .iter()
                .rev()
                .take_while(|&&b| b == b' ')
                .count();
            let take = spaces.min(w - n);
            out.drain(start - take..start);
            start -= take;
            n += take;
        }
        if w > n {
            let content = out.split_off(start);
            let keep = n.saturating_sub(2);
            let cut = match pad.trunc {
                Trunc::None => content,
                Trunc::End => layout::cut(&content, keep, 0),
                Trunc::Start => layout::cut(&content, 0, keep),
                Trunc::Middle => layout::cut(&content, keep / 2, keep - keep / 2),
            };
            out.extend(cut);
            return;
        }
        let fill = n - w;
        let (before, after) = match pad.align {
            Align::Left => (0, fill),
            Align::Right | Align::Steal => (fill, 0),
            Align::Center => (fill / 2, fill - fill / 2),
        };
        if before > 0 {
            out.splice(start..start, std::iter::repeat_n(b' ', before));
        }
        out.extend(std::iter::repeat_n(b' ', after));
    }

    /// `%Cred`, `%Cgreen`, `%Cblue`, `%Creset`, `%C(<spec>)`.
    fn color(&mut self, body: &[u8], out: &mut Vec<u8>) -> usize {
        let rest = &body[1..];
        for (name, code) in [
            (&b"red"[..], "\x1b[31m"),
            (b"green", "\x1b[32m"),
            (b"blue", "\x1b[34m"),
            (b"reset", crate::color::RESET),
        ] {
            if rest.starts_with(name) {
                if self.s.color {
                    out.extend_from_slice(code.as_bytes());
                }
                self.auto_color = false;
                return 1 + name.len();
            }
        }
        let Some(spec) = rest.strip_prefix(b"(").and_then(in_parens) else {
            return 0;
        };
        let used = 1 + 1 + spec.len() + 1;
        let spec = String::from_utf8_lossy(spec);
        if spec == "auto" {
            self.auto_color = true;
            if self.s.color && !out.is_empty() && self.pending.is_none() {
                out.extend_from_slice(crate::color::RESET.as_bytes());
            }
            return used;
        }
        self.auto_color = false;
        let (show, spec) = if let Some(s) = spec.strip_prefix("always,") {
            (true, s)
        } else if let Some(s) = spec.strip_prefix("auto,") {
            (self.s.color, s)
        } else {
            (self.s.color, &*spec)
        };
        if !show {
            return used;
        }
        match rgit_config::color::parse_color(spec) {
            Some(code) => out.extend_from_slice(code.as_bytes()),
            None => self.error = Some(BadColor(spec.to_string())),
        }
        used
    }

    fn push_auto(&self, out: &mut Vec<u8>, text: &[u8]) {
        if let Some(colors) = self.s.colors.as_ref().filter(|_| self.color_on()) {
            out.extend_from_slice(colors.commit.as_bytes());
            out.extend_from_slice(text);
            out.extend_from_slice(crate::color::RESET.as_bytes());
        } else {
            out.extend_from_slice(text);
        }
    }

    fn one(&mut self, body: &[u8], out: &mut Vec<u8>) -> usize {
        let item = self.item;
        let Some(&c) = body.first() else {
            return 0;
        };
        match c {
            b'n' => out.push(b'\n'),
            b'x' => match hex_byte(&body[1..]) {
                Some(b) => {
                    out.push(b);
                    return 3;
                }
                None => return 0,
            },
            b'H' => self.push_auto(out, item.id.to_hex().as_bytes()),
            b'h' => {
                let a = self.lookup.abbrev(item.id);
                self.push_auto(out, a.as_bytes());
            }
            b'T' | b't' => {
                if let Ok(tree) = item.commit.tree() {
                    let s = if c == b'T' {
                        tree.to_hex()
                    } else {
                        self.lookup.abbrev(&tree)
                    };
                    out.extend_from_slice(s.as_bytes());
                }
            }
            b'P' | b'p' => {
                for (i, p) in item.parents.iter().enumerate() {
                    if i > 0 {
                        out.push(b' ');
                    }
                    let s = if c == b'P' {
                        p.to_hex()
                    } else {
                        self.lookup.abbrev(p)
                    };
                    out.extend_from_slice(s.as_bytes());
                }
            }
            b'a' | b'c' => {
                let sig = if c == b'a' {
                    item.commit.author()
                } else {
                    item.commit.committer()
                };
                return self.person(sig.as_ref(), body.get(1).copied(), out);
            }
            b'd' | b'D' => {
                let mut parts = Parts::default();
                if c == b'D' {
                    parts.prefix.clear();
                    parts.suffix.clear();
                }
                self.decoration(&parts, out);
            }
            b'S' if !self.s.source => return 0,
            b'S' => {
                if let Some(s) = item.source {
                    out.extend_from_slice(s.as_bytes());
                }
            }
            b'e' => {
                if let Some(e) = item.commit.encoding() {
                    out.extend_from_slice(e);
                }
            }
            b's' => out.extend_from_slice(&subject(&message_lines(item.commit.message()))),
            b'f' => out.extend(sanitize(item.commit.message())),
            b'b' => out.extend_from_slice(message_body(item.commit.message())),
            b'B' => out.extend_from_slice(item.commit.message()),
            b'N' if !self.s.notes => return 0,
            b'N' => {
                if let Some(n) = self.lookup.notes(item.id) {
                    out.extend_from_slice(&n);
                }
            }
            b'm' => out.push(item.mark),
            b'G' => {
                let Some(&k) = body.get(1) else {
                    return 0;
                };
                if !b"G?SKFPT".contains(&k) {
                    return 0;
                }
                if k == b'?' {
                    let signed =
                        item.commit.gpgsig().is_some() || item.commit.gpgsig_sha256().is_some();
                    out.push(if signed { b'E' } else { b'N' });
                }
                return 2;
            }
            b'g' => {
                // Reflog placeholders are empty unless walking reflogs.
                return match body.get(1) {
                    Some(b'd' | b'D' | b'n' | b'N' | b'e' | b'E' | b's') => 2,
                    _ => 0,
                };
            }
            b'w' => return self.wrap(body, out),
            b'<' | b'>' => return self.padding(body, out),
            b'(' => return self.extended(body, out),
            _ => return 0,
        }
        1
    }

    /// `%a?` / `%c?`.
    fn person(&self, sig: Option<&SignatureRef<'_>>, k: Option<u8>, out: &mut Vec<u8>) -> usize {
        let Some(k) = k else {
            return 0;
        };
        if !b"nNeElLdDrtiIsh".contains(&k) {
            return 0;
        }
        let Some(sig) = sig else {
            return 2;
        };
        let (name, email) = if matches!(k, b'N' | b'E' | b'L') {
            self.s.mailmap.map(sig.name, sig.email)
        } else {
            (sig.name, sig.email)
        };
        let local = |e: &[u8]| -> Vec<u8> {
            let end = e.iter().position(|&b| b == b'@').unwrap_or(e.len());
            e[..end].to_vec()
        };
        match k {
            b'n' | b'N' => out.extend_from_slice(name),
            b'e' | b'E' => out.extend_from_slice(email),
            b'l' | b'L' => out.extend(local(email)),
            _ => {
                let Some(t) = sig.time() else {
                    return 2;
                };
                let secs = u64::try_from(t.seconds).unwrap_or(0);
                let tz = tz_hhmm(t.offset_minutes);
                let text = match k {
                    b't' => secs.to_string(),
                    _ => {
                        let mode = match k {
                            b'd' => self.s.date_mode.clone(),
                            b'D' => DateMode::new(DateStyle::Rfc2822),
                            b'r' => DateMode::new(DateStyle::Relative),
                            b'i' => DateMode::new(DateStyle::Iso8601),
                            b'I' => DateMode::new(DateStyle::Iso8601Strict),
                            b's' => DateMode::new(DateStyle::Short),
                            _ => DateMode::new(DateStyle::Human),
                        };
                        rgit_date::show_date(secs, tz, &mode, &self.s.date_ctx).unwrap_or_default()
                    }
                };
                out.extend_from_slice(text.as_bytes());
            }
        }
        2
    }

    fn decoration(&self, parts: &Parts, out: &mut Vec<u8>) {
        let colors = self.s.colors.as_ref().filter(|_| self.color_on());
        out.extend(self.lookup.decoration(self.item.id, parts, colors));
    }

    /// `%w(...)`.
    fn wrap(&mut self, body: &[u8], out: &mut Vec<u8>) -> usize {
        let Some(args) = body[1..].strip_prefix(b"(").and_then(in_parens) else {
            return 0;
        };
        let used = 2 + args.len() + 1;
        let text = String::from_utf8_lossy(args);
        let mut values = [0usize; 3];
        if !text.trim().is_empty() {
            let fields: Vec<&str> = text.split(',').collect();
            if fields.len() > 3 {
                return 0;
            }
            for (slot, f) in values.iter_mut().zip(&fields) {
                // An empty field keeps its default (`%w(,1,2)`).
                if f.trim().is_empty() {
                    continue;
                }
                match f.trim().parse::<usize>() {
                    Ok(v) if v <= MAX_NUMBER => *slot = v,
                    _ => return 0,
                }
            }
        }
        self.finish_wrap(out);
        if values != [0, 0, 0] {
            self.wrap = Some(Wrap {
                start: out.len(),
                width: values[0],
                indent1: values[1],
                indent2: values[2],
            });
        }
        used
    }

    /// `%<(...)`, `%<|(...)`, `%>(...)`, `%>|(...)`, `%>>(...)`,
    /// `%>>|(...)`, `%><(...)`, `%><|(...)`.
    fn padding(&mut self, body: &[u8], out: &mut Vec<u8>) -> usize {
        let (align, mut i) = match (body[0], body.get(1)) {
            (b'<', _) => (Align::Left, 1),
            (b'>', Some(b'>')) => (Align::Steal, 2),
            (b'>', Some(b'<')) => (Align::Center, 2),
            _ => (Align::Right, 1),
        };
        let column = body.get(i) == Some(&b'|');
        if column {
            i += 1;
        }
        let Some(args) = body[i..].strip_prefix(b"(").and_then(in_parens) else {
            return 0;
        };
        let used = i + 1 + args.len() + 1;
        let text = String::from_utf8_lossy(args);
        let (num, mode) = match text.split_once(',') {
            Some((n, m)) => (n, Some(m)),
            None => (&*text, None),
        };
        let num = match num.trim().parse::<i64>() {
            Ok(n) if n.unsigned_abs() <= MAX_NUMBER as u64 => n,
            _ => return 0,
        };
        // A negative column counts from the right edge of the terminal.
        let num = if column && num < 0 {
            match i64::try_from(self.s.columns).unwrap_or(80) + num {
                n if n >= 0 => n,
                _ => return 0,
            }
        } else if num > 0 {
            num
        } else {
            return 0;
        };
        let size = if column {
            Size::Column(usize::try_from(num).unwrap_or(0))
        } else {
            match usize::try_from(num) {
                Ok(n) if n > 0 => Size::Width(n),
                _ => return 0,
            }
        };
        let trunc = match mode {
            None => Trunc::None,
            Some("trunc") => Trunc::End,
            Some("ltrunc") => Trunc::Start,
            Some("mtrunc") => Trunc::Middle,
            Some(_) => {
                out.push(b'%');
                out.extend_from_slice(&body[..used]);
                Trunc::None
            }
        };
        self.pending = Some(Pad { align, size, trunc });
        used
    }

    /// `%(trailers...)`, `%(decorate...)`.
    fn extended(&mut self, body: &[u8], out: &mut Vec<u8>) -> usize {
        let Some(inner) = in_parens(&body[1..]) else {
            return 0;
        };
        let used = 1 + inner.len() + 1;
        let (name, opts) = match inner.iter().position(|&b| b == b':') {
            Some(i) => (&inner[..i], Some(&inner[i + 1..])),
            None => (inner, None),
        };
        let ok = match name {
            b"trailers" => self.trailers(opts.unwrap_or(b""), out),
            b"decorate" => self.decorate(opts.unwrap_or(b""), out),
            _ => false,
        };
        if ok { used } else { 0 }
    }

    fn decorate(&self, opts: &[u8], out: &mut Vec<u8>) -> bool {
        let mut parts = Parts::default();
        for opt in opts.split(|&b| b == b',').filter(|o| !o.is_empty()) {
            let Some(eq) = opt.iter().position(|&b| b == b'=') else {
                return false;
            };
            let value = literal_codes(&opt[eq + 1..]);
            match &opt[..eq] {
                b"prefix" => parts.prefix = value,
                b"suffix" => parts.suffix = value,
                b"separator" => parts.separator = value,
                b"pointer" => parts.pointer = value,
                b"tag" => parts.tag = value,
                _ => return false,
            }
        }
        self.decoration(&parts, out);
        true
    }

    fn trailers(&self, opts: &[u8], out: &mut Vec<u8>) -> bool {
        let mut only: Option<bool> = None;
        let mut unfold = false;
        let mut keyonly = false;
        let mut valueonly = false;
        let mut keys: Vec<Vec<u8>> = Vec::new();
        let mut separator: Option<Vec<u8>> = None;
        let mut kv_separator: Option<Vec<u8>> = None;
        for opt in opts.split(|&b| b == b',').filter(|o| !o.is_empty()) {
            let (name, value) = match opt.iter().position(|&b| b == b'=') {
                Some(i) => (&opt[..i], Some(&opt[i + 1..])),
                None => (opt, None),
            };
            let flag = |v: Option<&[u8]>| parse_bool_text(v);
            match name {
                b"key" => {
                    let Some(v) = value else {
                        return false;
                    };
                    let v = v.strip_suffix(b":").unwrap_or(v);
                    keys.push(v.to_vec());
                    only = Some(true);
                }
                b"separator" => separator = Some(literal_codes(value.unwrap_or(b""))),
                b"key_value_separator" => {
                    kv_separator = Some(literal_codes(value.unwrap_or(b"")));
                }
                b"only" => match flag(value) {
                    Some(b) => only = Some(b),
                    None => return false,
                },
                b"unfold" => match flag(value) {
                    Some(b) => unfold = b,
                    None => return false,
                },
                b"keyonly" => match flag(value) {
                    Some(b) => keyonly = b,
                    None => return false,
                },
                b"valueonly" => match flag(value) {
                    Some(b) => valueonly = b,
                    None => return false,
                },
                _ => return false,
            }
        }
        let only = only.unwrap_or(false);
        let message = self.item.commit.message();
        let info = rgit_trailer::parse(
            message,
            &self.s.trailers,
            &rgit_trailer::ParseOptions {
                comment: b"#",
                divider: false,
            },
        );
        // A message without a final newline ends its last trailer line
        // without one.
        let unterminated = info.found && !message[..info.end.min(message.len())].ends_with(b"\n");
        let last = info.lines.len().saturating_sub(1);
        let mut first = true;
        for (index, line) in info.lines.iter().enumerate() {
            let mut text = Vec::new();
            match line {
                rgit_trailer::Line::Trailer(t) => {
                    if !keys.is_empty()
                        && !keys
                            .iter()
                            .any(|k| k.eq_ignore_ascii_case(t.token.as_slice()))
                    {
                        continue;
                    }
                    if !valueonly {
                        text.extend_from_slice(&t.token);
                    }
                    if !keyonly && !valueonly {
                        match &kv_separator {
                            Some(s) => text.extend_from_slice(s),
                            None => text.extend_from_slice(b": "),
                        }
                    }
                    if !keyonly {
                        if unfold {
                            text.extend(rgit_trailer::unfold_value(&t.value));
                        } else {
                            text.extend_from_slice(&t.value);
                        }
                    }
                }
                rgit_trailer::Line::Other(o) => {
                    if only {
                        continue;
                    }
                    text.extend_from_slice(o);
                }
            }
            match &separator {
                Some(sep) => {
                    if !first {
                        out.extend_from_slice(sep);
                    }
                    out.extend(text);
                }
                None => {
                    out.extend(text);
                    if !(unterminated && index == last) {
                        out.push(b'\n');
                    }
                }
            }
            first = false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitized() {
        assert_eq!(sanitize(b"Fix: the  bug!"), b"Fix-the-bug");
        assert_eq!(sanitize(b"..a..b..\nc"), b".a.b");
        assert_eq!(sanitize(b"x/y z"), b"x-y-z");
    }

    #[test]
    fn bodies() {
        assert_eq!(message_body(b"s\n\nb\nc\n"), b"b\nc\n");
        assert_eq!(message_body(b"\n\ns\nt\n\n\nb\n"), b"b\n");
        assert_eq!(message_body(b"s\n"), b"");
        assert_eq!(message_body(b""), b"");
    }

    #[test]
    fn codes() {
        assert_eq!(literal_codes(b"a%x2C%n%%%q"), b"a,\n%%q");
    }
}
