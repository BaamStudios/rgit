// SPDX-License-Identifier: MIT
//! Taking one e-mail apart (Documentation/git-mailinfo.adoc): the author,
//! date and subject for the info text, the commit message and the patch.
//!
//! The documentation describes the options; the details below were
//! determined by running `git mailinfo` (CLEANROOM.md):
//!
//! - Headers: a first line starting with `From ` is skipped. Headers end
//!   at an empty line or at the first line that is not `name:` or a
//!   continuation; that line belongs to the body. Each physical line loses
//!   its trailing whitespace, a continuation line's first character
//!   becomes a space. The last `From`, `Subject`, `Date`, `Message-ID`,
//!   `Content-Type` and `Content-Transfer-Encoding` win.
//! - Header values are decoded (RFC 2047) and converted to the target
//!   charset; other header bytes are left alone. `Date:` has whitespace
//!   runs collapsed.
//! - Body: leading empty lines are skipped. Then "in-body headers"
//!   (`From:`, `Subject:`, `Date:`, each once, with continuation lines,
//!   and `[PATCH] <title>` lines) are read; an empty line after them is
//!   dropped. They replace the mail's headers only when a patch follows.
//! - The patch starts at a line beginning with `---` followed by
//!   whitespace or the end of the line, `diff -` or `Index: `. Message
//!   lines are converted to the target charset, patch lines are not.
//! - With `-m` the `Message-ID:` is appended to the message, also only
//!   when a patch follows.
//! - Transfer encodings are decoded per part; lines ending in CR LF after
//!   decoding are reported (`--quoted-cr=warn`) or fixed (`strip`).
//!   `format=flowed` text is unwrapped (in message and patch).
//! - Multipart: everything before the first boundary is skipped; after a
//!   boundary the part's headers run to the first line that is empty or
//!   not a header, and the line after them is never taken as a boundary.
//!   A closing boundary ends the part with an empty line (unless the
//!   part's decoded text did not end in a newline) and the text up to the
//!   enclosing part's next boundary is skipped; after the outermost one
//!   the rest of the mail is ignored.

use crate::messages;
use crate::rfc2047::{self, DecodeError};
use crate::{from, subject};

/// What to do with lines ending in CR LF after transfer decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QuotedCr {
    NoWarn,
    #[default]
    Warn,
    Strip,
}

impl QuotedCr {
    /// Parses a `--quoted-cr` / `mailinfo.quotedCr` value.
    pub fn parse(s: &str) -> Option<QuotedCr> {
        match s {
            "nowarn" => Some(QuotedCr::NoWarn),
            "warn" => Some(QuotedCr::Warn),
            "strip" => Some(QuotedCr::Strip),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            QuotedCr::NoWarn => "nowarn",
            QuotedCr::Warn => "warn",
            QuotedCr::Strip => "strip",
        }
    }
}

/// Options of `git mailinfo`.
#[derive(Debug, Clone)]
pub struct Options {
    /// `-k`: keep the subject as it is.
    pub keep_subject: bool,
    /// `-b`: keep bracketed strings without `PATCH`.
    pub keep_non_patch: bool,
    /// `-m`: append the `Message-ID:` to the message.
    pub add_message_id: bool,
    /// `--scissors`.
    pub use_scissors: bool,
    /// Read `From:`/`Subject:`/`Date:` at the start of the body.
    pub use_inbody_headers: bool,
    /// The charset to convert metadata and message to; `None` (`-n`)
    /// converts nothing.
    pub metainfo_charset: Option<String>,
    /// `--quoted-cr`.
    pub quoted_cr: QuotedCr,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            keep_subject: false,
            keep_non_patch: false,
            add_message_id: false,
            use_scissors: false,
            use_inbody_headers: true,
            metainfo_charset: Some("UTF-8".into()),
            quoted_cr: QuotedCr::Warn,
        }
    }
}

/// The result of taking a mail apart.
#[derive(Debug, Clone, Default)]
pub struct Info {
    /// The info text (`Author:`, `Email:`, `Subject:`, `Date:`, empty
    /// line).
    pub info: Vec<u8>,
    /// The commit message (without the title).
    pub msg: Vec<u8>,
    /// The patch.
    pub patch: Vec<u8>,
    /// Errors to report (without `error: `).
    pub errors: Vec<String>,
    /// Warnings to report (without `warning: `).
    pub warnings: Vec<String>,
    /// The mail could not be processed (exit code 1).
    pub failed: bool,
    /// Some part was sent with `format=flowed`.
    pub format_flowed: bool,
}

#[derive(Default, Clone)]
struct Fields {
    from: Option<Vec<u8>>,
    subject: Option<Vec<u8>>,
    date: Option<Vec<u8>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cte {
    None,
    Qp,
    Base64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Skipping empty lines at the start of the body.
    Start,
    InBody,
    Message,
    Patch,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    From,
    Subject,
    Date,
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

fn rtrim(s: &[u8]) -> &[u8] {
    let mut end = s.len();
    while end > 0 && is_space(s[end - 1]) {
        end -= 1;
    }
    &s[..end]
}

fn ltrim(s: &[u8]) -> &[u8] {
    let start = s.iter().position(|&b| !is_space(b)).unwrap_or(s.len());
    &s[start..]
}

/// Collapses whitespace runs to one space (no trimming).
fn collapse(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut space = false;
    for &b in s {
        if is_space(b) {
            if !space {
                out.push(b' ');
            }
            space = true;
        } else {
            space = false;
            out.push(b);
        }
    }
    out
}

/// The header name of `line` if it is `name:...`.
fn header_name(line: &[u8]) -> Option<&[u8]> {
    let colon = line.iter().position(|&b| b == b':')?;
    let name = &line[..colon];
    if name.is_empty() || name.iter().any(|&b| b <= b' ' || b >= 0x7f) {
        return None;
    }
    Some(name)
}

fn is_continuation(line: &[u8]) -> bool {
    matches!(line.first(), Some(b' ' | b'\t'))
}

fn is_blank_line(line: &[u8]) -> bool {
    line == b"\n" || line == b"\r\n" || line.is_empty()
}

/// Splits `data` into lines, each with its newline.
fn lines(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        let end = data[pos..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(data.len(), |n| pos + n + 1);
        out.push(&data[pos..end]);
        pos = end;
    }
    out
}

/// A header block: (name, unfolded value) pairs. `lines[*pos..]` starts
/// with it; afterwards `*pos` is behind it (and behind the empty or
/// non-header line ending it when `consume_end`).
fn read_headers(lines: &[&[u8]], pos: &mut usize, consume_end: bool) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut out: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    while *pos < lines.len() {
        let line = lines[*pos];
        if is_blank_line(line) {
            *pos += 1;
            break;
        }
        if is_continuation(line) && !out.is_empty() {
            let t = rtrim(line);
            if !t.is_empty() {
                let value = &mut out.last_mut().expect("checked").1;
                value.push(b' ');
                value.extend_from_slice(&t[1..]);
            }
            *pos += 1;
            continue;
        }
        let Some(name) = header_name(line) else {
            if consume_end {
                *pos += 1;
            }
            break;
        };
        let value = rtrim(&line[name.len() + 1..]).to_vec();
        out.push((name.to_ascii_lowercase(), value));
        *pos += 1;
    }
    for (_, v) in &mut out {
        *v = ltrim(v).to_vec();
    }
    out
}

/// The value of the attribute `name` (lowercase, without `=`) in a
/// `Content-Type` value.
fn attribute(value: &[u8], name: &[u8]) -> Option<Vec<u8>> {
    let lower = value.to_ascii_lowercase();
    let mut search = 0;
    while let Some(off) = lower[search..]
        .windows(name.len() + 1)
        .position(|w| &w[..name.len()] == name && w[name.len()] == b'=')
    {
        let at = search + off;
        let preceded = at == 0 || matches!(lower[at - 1], b';' | b' ' | b'\t');
        let start = at + name.len() + 1;
        if !preceded {
            search = at + 1;
            continue;
        }
        let rest = &value[start..];
        if let Some(q) = rest.strip_prefix(b"\"") {
            let end = q.iter().position(|&b| b == b'"').unwrap_or(q.len());
            return Some(q[..end].to_vec());
        }
        let end = rest
            .iter()
            .position(|&b| b == b';' || is_space(b))
            .unwrap_or(rest.len());
        return Some(rest[..end].to_vec());
    }
    None
}

/// The part being read: transfer encoding and text attributes.
#[derive(Clone)]
struct Part {
    cte: Cte,
    charset: Option<String>,
    flowed: bool,
    delsp: bool,
}

impl Part {
    fn plain() -> Part {
        Part {
            cte: Cte::None,
            charset: None,
            flowed: false,
            delsp: false,
        }
    }
}

struct State<'o> {
    opts: &'o Options,
    out: Info,
    phase: Phase,
    part: Part,
    boundaries: Vec<Vec<u8>>,
    /// Decoded text not yet ending in a newline.
    pending: Vec<u8>,
    /// Joined `format=flowed` text waiting for its hard break.
    flow: Vec<u8>,
    inbody: Fields,
    /// In-body headers already seen (each counts once).
    seen: Vec<Field>,
    /// The in-body header continuation lines go to.
    current: Option<Field>,
    patch_found: bool,
    crlf_seen: bool,
}

/// The decoded header fields of a mail, with the charset target applied.
fn decode_value(v: &[u8], target: Option<&str>) -> Result<Vec<u8>, DecodeError> {
    rfc2047::decode_header(v, target)
}

/// `>From <40 hex digits> Mon Sep 17 00:00:00 2001`, the first line of a
/// `git format-patch` mail, quoted.
fn is_format_patch_from(line: &[u8]) -> bool {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let Some(rest) = line.strip_prefix(b">From ") else {
        return false;
    };
    rest.len() == 40 + 25
        && rest[..40].iter().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
        && &rest[40..] == b" Mon Sep 17 00:00:00 2001"
}

fn is_patch_start(line: &[u8]) -> bool {
    if let Some(rest) = line.strip_prefix(b"---") {
        return rest.is_empty() || is_space(rest[0]);
    }
    line.starts_with(b"diff -") || line.starts_with(b"Index: ")
}

/// Whether `line` is a scissors line ("-- >8 --").
///
/// Fitted to the behaviour of `git mailinfo --scissors` on some
/// thousand generated lines (it agrees on all but a few odd ones, such as
/// scissors lines with trailing blanks): every blank (the newline
/// included) counts as gap and perforation, `-` as perforation, the marks
/// `>8`, `8<`, `>%` and `%<` as two characters of perforation, anything
/// else as text. The non-blank span (a mark counted at its first
/// character) must be at least eight long, gaps less than two thirds and
/// text (plus two) less than twice the perforation.
fn is_scissors(line: &[u8]) -> bool {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let mut marks = 0usize;
    let mut gap = 1usize;
    let mut perforation = 1usize;
    let mut text = 0usize;
    let mut first: Option<usize> = None;
    let mut last = 0usize;
    let mut i = 0;
    while i < line.len() {
        let c = line[i];
        if is_space(c) {
            gap += 1;
            perforation += 1;
            i += 1;
            continue;
        }
        first.get_or_insert(i);
        last = i;
        if matches!(line.get(i..i + 2), Some(b">8" | b"8<" | b">%" | b"%<")) {
            marks += 1;
            perforation += 2;
            i += 2;
            continue;
        }
        if c == b'-' {
            perforation += 1;
        } else {
            text += 1;
        }
        i += 1;
    }
    let visible = first.map_or(0, |f| last - f + 1);
    marks > 0 && visible >= 8 && 3 * gap < 2 * perforation && text + 2 < 2 * perforation
}

impl<'o> State<'o> {
    fn target(&self) -> Option<&str> {
        self.opts.metainfo_charset.as_deref()
    }

    fn convert_message(&mut self, line: &[u8]) -> Vec<u8> {
        let (Some(target), Some(cs)) = (self.target(), self.part.charset.as_deref()) else {
            return line.to_vec();
        };
        match crate::charset::convert(line, cs, target) {
            Some(v) => v,
            None => {
                let m = messages::cannot_convert(cs, target);
                if !self.out.errors.contains(&m) {
                    self.out.errors.push(m);
                }
                self.out.failed = true;
                line.to_vec()
            }
        }
    }

    /// Decodes one raw body line and hands the complete decoded lines on.
    fn body_line(&mut self, raw: &[u8]) {
        let decoded = match self.part.cte {
            Cte::None => raw.to_vec(),
            Cte::Qp => rfc2047::decode_qp(raw, false),
            Cte::Base64 => rfc2047::decode_base64(raw),
        };
        self.pending.extend_from_slice(&decoded);
        while let Some(n) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=n).collect();
            self.decoded_line(line);
        }
    }

    /// Hands on decoded text that does not end in a newline.
    fn flush_pending(&mut self) {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            self.decoded_line(line);
        }
    }

    fn decoded_line(&mut self, mut line: Vec<u8>) {
        if line.ends_with(b"\r\n") {
            match self.opts.quoted_cr {
                QuotedCr::Strip => {
                    line.truncate(line.len() - 2);
                    line.push(b'\n');
                }
                QuotedCr::Warn => self.crlf_seen = true,
                QuotedCr::NoWarn => {}
            }
        }
        if self.part.flowed {
            if line.first() == Some(&b' ') {
                line.remove(0);
            }
            let content = line.strip_suffix(b"\n").unwrap_or(&line);
            if content.ends_with(b" ") && content != b"-- " && line.ends_with(b"\n") {
                let mut keep = content.len();
                if self.part.delsp {
                    keep -= 1;
                }
                self.flow.extend_from_slice(&line[..keep]);
                return;
            }
            if !self.flow.is_empty() {
                let mut joined = std::mem::take(&mut self.flow);
                joined.extend_from_slice(&line);
                line = joined;
            }
        }
        self.line(&line);
    }

    fn flush_flow(&mut self) {
        if !self.flow.is_empty() {
            let line = std::mem::take(&mut self.flow);
            self.line(&line);
        }
    }

    /// A decoded body line.
    fn line(&mut self, line: &[u8]) {
        if self.phase == Phase::Patch {
            self.out.patch.extend_from_slice(line);
            return;
        }
        if is_patch_start(line) {
            self.phase = Phase::Patch;
            self.patch_found = true;
            self.out.patch.extend_from_slice(line);
            return;
        }
        let line = self.convert_message(line);
        if self.opts.use_scissors && is_scissors(&line) {
            self.out.msg.clear();
            self.inbody = Fields::default();
            self.seen.clear();
            self.current = None;
            self.phase = Phase::Start;
            return;
        }
        if self.opts.use_inbody_headers
            && matches!(self.phase, Phase::Start | Phase::InBody)
            && is_format_patch_from(&line)
        {
            // The "From <commit>" line of a format-patch mail quoted in
            // the body (">From"); in-body headers may follow.
            self.current = None;
            self.phase = Phase::Start;
            return;
        }
        if self.phase == Phase::Start {
            if is_blank_line(&line) {
                return;
            }
            self.phase = if self.opts.use_inbody_headers {
                Phase::InBody
            } else {
                Phase::Message
            };
        }
        if self.phase == Phase::InBody && self.inbody_line(&line) {
            return;
        }
        self.out.msg.extend_from_slice(&line);
    }

    /// Handles a line of the in-body header block; false when the line
    /// ends the block and belongs to the message.
    fn inbody_line(&mut self, line: &[u8]) -> bool {
        if let Some(field) = self.current {
            if is_continuation(line) {
                let value = self.field_mut(field);
                if value.ends_with(b"\n") {
                    value.pop();
                }
                value.extend_from_slice(line);
                return true;
            }
        }
        self.current = None;
        if let Some(name) = header_name(line) {
            let field = match name.to_ascii_lowercase().as_slice() {
                b"from" => Some(Field::From),
                b"subject" => Some(Field::Subject),
                b"date" => Some(Field::Date),
                _ => None,
            };
            if let Some(field) = field {
                if !self.seen.contains(&field) {
                    self.seen.push(field);
                    let value = ltrim(&line[name.len() + 1..]).to_vec();
                    *self.field_mut(field) = value;
                    self.current = Some(field);
                    return true;
                }
            }
        }
        if let Some(rest) = line.strip_prefix(b"[PATCH]") {
            if rest.first().is_some_and(|&b| is_space(b)) {
                self.inbody.subject = Some(line.to_vec());
                return true;
            }
        }
        self.phase = Phase::Message;
        is_blank_line(line)
    }

    fn field_mut(&mut self, field: Field) -> &mut Vec<u8> {
        let slot = match field {
            Field::From => &mut self.inbody.from,
            Field::Subject => &mut self.inbody.subject,
            Field::Date => &mut self.inbody.date,
        };
        slot.get_or_insert_with(Vec::new)
    }

    /// Applies a part's (or the mail's) Content-Type and transfer
    /// encoding.
    fn set_part(&mut self, headers: &[(Vec<u8>, Vec<u8>)]) {
        self.part = Part::plain();
        for (name, value) in headers {
            match name.as_slice() {
                b"content-type" => {
                    self.part.charset = attribute(value, b"charset")
                        .map(|c| String::from_utf8_lossy(&c).into_owned());
                    self.part.flowed = attribute(value, b"format")
                        .is_some_and(|f| f.eq_ignore_ascii_case(b"flowed"));
                    self.part.delsp = attribute(value, b"delsp")
                        .is_some_and(|f| f.eq_ignore_ascii_case(b"yes"));
                    if let Some(b) = attribute(value, b"boundary") {
                        let mut full = b"--".to_vec();
                        full.extend_from_slice(&b);
                        self.boundaries.push(full);
                    }
                }
                b"content-transfer-encoding" => {
                    let v = value.to_ascii_lowercase();
                    self.part.cte = match v.trim_ascii() {
                        b"quoted-printable" => Cte::Qp,
                        b"base64" => Cte::Base64,
                        _ => Cte::None,
                    };
                }
                _ => {}
            }
        }
        if self.part.flowed {
            self.out.format_flowed = true;
        }
    }
}

/// Takes the mail `input` apart.
pub fn info(input: &[u8], opts: &Options) -> Info {
    let all = lines(input);
    let mut pos = 0;
    if all.first().is_some_and(|l| l.starts_with(b"From ")) {
        pos = 1;
    }
    let headers = read_headers(&all, &mut pos, false);
    // When the headers run to the end of the mail, the last line is read
    // again as the body, without its newline (observed: it shows up in
    // the message with --no-inbody-headers).
    let mut last_header_line: Option<Vec<u8>> = None;
    if pos == all.len() && pos > 0 && !headers.is_empty() && !is_blank_line(all[pos - 1]) {
        let l = all[pos - 1];
        last_header_line = Some(l.strip_suffix(b"\n").unwrap_or(l).to_vec());
    }
    let mut st = State {
        opts,
        out: Info::default(),
        phase: Phase::Start,
        part: Part::plain(),
        boundaries: Vec::new(),
        pending: Vec::new(),
        flow: Vec::new(),
        inbody: Fields::default(),
        seen: Vec::new(),
        current: None,
        patch_found: false,
        crlf_seen: false,
    };
    let target = opts.metainfo_charset.clone();
    let mut fields = Fields::default();
    let mut message_id: Option<Vec<u8>> = None;
    let mut decode_failed = false;
    for (name, value) in &headers {
        let slot = match name.as_slice() {
            b"from" => &mut fields.from,
            b"subject" => &mut fields.subject,
            b"date" => &mut fields.date,
            b"message-id" => {
                message_id = Some(value.clone());
                continue;
            }
            _ => continue,
        };
        match decode_value(value, target.as_deref()) {
            Ok(v) => *slot = Some(v),
            Err(e) => {
                if let DecodeError::Charset(cs) = e {
                    st.out.errors.push(messages::cannot_convert(
                        &cs,
                        target.as_deref().unwrap_or("UTF-8"),
                    ));
                }
                *slot = Some(value.clone());
                decode_failed = true;
            }
        }
    }
    if decode_failed {
        st.out.failed = true;
        st.out.info = info_text(&fields, opts);
        return st.out;
    }
    st.set_part(&headers);

    let mut skip_check = false;
    if !st.boundaries.is_empty() {
        // Skip the preamble.
        skip_check = true;
        let top = st.boundaries.last().expect("non-empty").clone();
        while pos < all.len() && !all[pos].starts_with(&top) {
            pos += 1;
        }
        if pos < all.len() {
            pos += 1;
            let h = read_headers(&all, &mut pos, true);
            st.set_part(&h);
        }
    }
    if let Some(l) = last_header_line {
        st.body_line(&l);
    }
    while pos < all.len() {
        let line = all[pos];
        pos += 1;
        if !skip_check {
            if let Some(top) = st.boundaries.last().cloned() {
                if line.starts_with(&top) {
                    if line[top.len()..].starts_with(b"--") {
                        // A closing boundary ends the part's text with a
                        // newline.
                        st.pending.push(b'\n');
                        st.flush_pending();
                        st.flush_flow();
                        st.boundaries.pop();
                        let Some(parent) = st.boundaries.last().cloned() else {
                            break;
                        };
                        while pos < all.len() && !all[pos].starts_with(&parent) {
                            pos += 1;
                        }
                        if pos >= all.len() {
                            break;
                        }
                        st.flush_pending();
                        st.flush_flow();
                        // The parent's boundary is handled as such below.
                        let next = all[pos];
                        pos += 1;
                        if next[parent.len()..].starts_with(b"--") {
                            st.boundaries.pop();
                            if st.boundaries.is_empty() {
                                break;
                            }
                            continue;
                        }
                    }
                    st.flush_pending();
                    st.flush_flow();
                    let h = read_headers(&all, &mut pos, true);
                    st.set_part(&h);
                    skip_check = true;
                    continue;
                }
            }
        }
        skip_check = false;
        st.body_line(line);
    }
    st.flush_pending();
    st.flush_flow();

    if st.crlf_seen {
        st.out.warnings.push(messages::QUOTED_CRLF.to_owned());
    }
    if st.patch_found {
        if let Some(v) = st.inbody.from.take() {
            fields.from = Some(v);
        }
        if let Some(v) = st.inbody.subject.take() {
            fields.subject = Some(v);
        }
        if let Some(v) = st.inbody.date.take() {
            fields.date = Some(v);
        }
        if opts.add_message_id {
            if let Some(id) = &message_id {
                st.out.msg.extend_from_slice(b"Message-ID: ");
                st.out.msg.extend_from_slice(id);
                st.out.msg.push(b'\n');
            }
        }
    }
    st.out.info = info_text(&fields, opts);
    st.out
}

fn info_text(fields: &Fields, opts: &Options) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(f) = &fields.from {
        let (name, email) = from::parse(f);
        out.extend_from_slice(b"Author: ");
        out.extend_from_slice(&name);
        out.extend_from_slice(b"\nEmail: ");
        out.extend_from_slice(&email);
        out.push(b'\n');
    }
    if let Some(s) = &fields.subject {
        let s = if opts.keep_subject {
            s.clone()
        } else {
            subject::cleanup(s, opts.keep_non_patch)
        };
        for l in s.split(|&b| b == b'\n') {
            out.extend_from_slice(b"Subject: ");
            out.extend_from_slice(l);
            out.push(b'\n');
        }
    }
    if let Some(d) = &fields.date {
        out.extend_from_slice(b"Date: ");
        out.extend_from_slice(&collapse(d));
        out.push(b'\n');
    }
    out.push(b'\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(mail: &str, opts: &Options) -> (String, String, String) {
        let r = info(mail.as_bytes(), opts);
        (
            String::from_utf8_lossy(&r.info).into_owned(),
            String::from_utf8_lossy(&r.msg).into_owned(),
            String::from_utf8_lossy(&r.patch).into_owned(),
        )
    }

    #[test]
    fn simple_mail() {
        let (i, m, p) = run(
            "From 1234 Mon Sep 17 00:00:00 2001\nFrom: A U Thor <a@example.com>\nDate: Fri, 9 Jun 2006 00:44:16 -0700\nSubject: [PATCH] a commit.\n\nHere is a patch.\n\n---\n foo | 2 +-\n",
            &Options::default(),
        );
        assert_eq!(
            i,
            "Author: A U Thor\nEmail: a@example.com\nSubject: a commit.\nDate: Fri, 9 Jun 2006 00:44:16 -0700\n\n"
        );
        assert_eq!(m, "Here is a patch.\n\n");
        assert_eq!(p, "---\n foo | 2 +-\n");
    }

    #[test]
    fn inbody_headers_need_a_patch() {
        let mail = "From: J <j@k>\nSubject: s\n\nFrom: A <a@b>\nDate: bogus\nSubject: t\n\nbody\n";
        let (i, m, _) = run(mail, &Options::default());
        assert_eq!(i, "Author: J\nEmail: j@k\nSubject: s\n\n");
        assert_eq!(m, "body\n");
        let (i, _, _) = run(&format!("{mail}---\n"), &Options::default());
        assert_eq!(i, "Author: A\nEmail: a@b\nSubject: t\nDate: bogus \n\n");
    }

    #[test]
    fn transfer_encodings_and_charsets() {
        let r = info(
            b"Subject: =?ISO-8859-1?Q?caf=E9?=\nContent-Type: text/plain; charset=iso-8859-1\nContent-Transfer-Encoding: quoted-printable\n\ncaf=E9 =3D=\n x\n---\npatch caf=E9\n",
            &Options::default(),
        );
        assert_eq!(r.info, "Subject: café\n\n".as_bytes());
        assert_eq!(r.msg, "café = x\n".as_bytes());
        assert_eq!(r.patch, b"---\npatch caf\xe9\n");
    }

    #[test]
    fn scissors() {
        let o = Options {
            use_scissors: true,
            ..Options::default()
        };
        for yes in ["-- >8 --", "- - >8 - -", "-- %< --", "-- >8 -- cut here", ">8 -----"] {
            assert!(is_scissors(yes.as_bytes()), "{yes}");
        }
        for no in ["-->8--", "-- 8<", ">8", "----", "---- >8", "------>8"] {
            assert!(!is_scissors(no.as_bytes()), "{no}");
        }
        let (i, m, _) = run(
            "Subject: s\n\nchatter\n-- >8 --\nSubject: real\n\nbody\n---\np\n",
            &o,
        );
        assert_eq!(i, "Subject: real\n\n");
        assert_eq!(m, "body\n");
    }

    #[test]
    fn multipart() {
        let (_, m, p) = run(
            "Subject: s\nContent-Type: multipart/mixed; boundary=XX\n\npreamble\n--XX\nContent-Type: text/plain\n\nmessage text\n\n--XX\nContent-Transfer-Encoding: base64\n\nLS0tCmRpZmYgLS1naXQgYS9mIGIvZgo=\n--XX--\nepilogue\n",
            &Options::default(),
        );
        assert_eq!(m, "message text\n\n");
        assert_eq!(p, "---\ndiff --git a/f b/f\n\n");
    }

    #[test]
    fn flowed() {
        let (_, m, p) = run(
            "Subject: s\nContent-Type: text/plain; format=flowed\n\nline one \nline two\n  spaced\n-- \nsig\n---\npatch \n x\n",
            &Options::default(),
        );
        assert_eq!(m, "line one line two\n spaced\n-- \nsig\n");
        assert_eq!(p, "---\npatch x\n");
    }
}
