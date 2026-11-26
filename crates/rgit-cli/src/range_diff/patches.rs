// SPDX-License-Identifier: MIT
//! The patches `range-diff` compares: the output of a `log -p` child
//! (with `>`, `<` and `#` as line signs, so that diff lines cannot be
//! mistaken for headers) rewritten into a form whose diff is easy to
//! read, as observed from the `git` binary:
//!
//! ```text
//!  ## Metadata ##
//! Author: A U Thor <author@example.com>
//!
//!  ## Commit message ##
//!     subject
//!
//!  ## Notes ##
//!     a note
//!
//!  ## file => renamed (mode change 100644 => 100755) ##
//! @@ renamed: funcname
//!  context
//! -old
//! +new
//! ```
//!
//! The `Date:` and `Merge:` lines are dropped, hunk headers lose their
//! line numbers and get the file name in front of the function name, and
//! the extended header lines of a file are folded into its `## ... ##`
//! line. Only the part from the first file on (the "diff") goes into the
//! cost of a pairing.

use rgit_hash::ObjectId;

/// One commit of a range.
pub struct Patch {
    pub id: ObjectId,
    /// The whole rewritten patch.
    pub text: Vec<u8>,
    /// Where the diff part of `text` starts.
    pub diff_start: usize,
    /// The size of the diff part in lines.
    pub diff_lines: usize,
}

impl Patch {
    pub fn diff(&self) -> &[u8] {
        &self.text[self.diff_start..]
    }
}

/// The header of one file of the diff, collected from the `diff --git`
/// line and the extended header lines.
#[derive(Default)]
struct FileHeader {
    /// Both names from `diff --git <a> <b>` when they are equal.
    git_name: Option<Vec<u8>>,
    old_name: Option<Vec<u8>>,
    new_name: Option<Vec<u8>>,
    renamed: bool,
    new_file: bool,
    deleted: bool,
    old_mode: Option<Vec<u8>>,
    new_mode: Option<Vec<u8>>,
}

impl FileHeader {
    fn parse_git_line(rest: &[u8]) -> FileHeader {
        let mut h = FileHeader::default();
        // Without prefixes an unrenamed file reads `<name> <name>`.
        if rest.len() % 2 == 1 {
            let half = rest.len() / 2;
            if rest[half] == b' ' && rest[..half] == rest[half + 1..] {
                h.git_name = Some(unquote(&rest[..half]));
            }
        }
        h
    }

    /// The name shown: `old => new` for a rename.
    fn display_name(&self) -> Vec<u8> {
        let old = self.old_name.clone().or_else(|| self.git_name.clone());
        let new = self.new_name.clone().or_else(|| self.git_name.clone());
        match (old, new) {
            (Some(o), Some(n)) if self.renamed && o != n => {
                let mut s = o;
                s.extend_from_slice(b" => ");
                s.extend_from_slice(&n);
                s
            }
            (_, Some(n)) if !self.deleted => n,
            (Some(o), _) => o,
            (None, Some(n)) => n,
            (None, None) => Vec::new(),
        }
    }

    /// The name in front of function names of hunk headers.
    fn hunk_name(&self) -> Vec<u8> {
        let name = if self.deleted {
            self.old_name.clone()
        } else {
            self.new_name.clone()
        };
        name.or_else(|| self.git_name.clone())
            .or_else(|| self.old_name.clone())
            .unwrap_or_default()
    }

    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b" ## ");
        out.extend_from_slice(&self.display_name());
        if self.new_file {
            out.extend_from_slice(b" (new)");
        } else if self.deleted {
            out.extend_from_slice(b" (deleted)");
        }
        if let (Some(o), Some(n)) = (&self.old_mode, &self.new_mode) {
            out.extend_from_slice(b" (mode change ");
            out.extend_from_slice(o);
            out.extend_from_slice(b" => ");
            out.extend_from_slice(n);
            out.push(b')');
        }
        out.extend_from_slice(b" ##\n");
    }
}

/// A path of a header line: C-quoted or plain, without the tab git adds
/// after names with spaces.
fn unquote(s: &[u8]) -> Vec<u8> {
    let s = s.strip_suffix(b"\t").unwrap_or(s);
    if s.first() == Some(&b'"')
        && let Some(u) = crate::quote::c_unquote(s)
    {
        return u;
    }
    s.to_vec()
}

#[derive(PartialEq, Eq)]
enum State {
    /// `Author:`, `Date:` and the like.
    Header,
    /// The message and notes.
    Message,
    /// The extended header lines of a file.
    FileHeader,
    /// The hunks of a file.
    Hunks,
}

/// Rewrites the output of the `log -p` child. Returns `None` when it is
/// not what `log` writes.
pub fn parse(output: &[u8]) -> Option<Vec<Patch>> {
    let mut patches: Vec<Patch> = Vec::new();
    let mut current: Option<Patch> = None;
    let mut state = State::Header;
    let mut header: Option<FileHeader> = None;
    let mut hunk_name: Vec<u8> = Vec::new();
    let mut files = 0usize;

    let finish_header = |p: &mut Patch, header: &mut Option<FileHeader>, name: &mut Vec<u8>| {
        if let Some(h) = header.take() {
            h.write(&mut p.text);
            *name = h.hunk_name();
        }
    };

    let mut lines = output.split(|&b| b == b'\n').peekable();
    while let Some(line) = lines.next() {
        if lines.peek().is_none() && line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix(b"commit ") {
            if let Some(mut p) = current.take() {
                finish_header(&mut p, &mut header, &mut hunk_name);
                patches.push(finish(p));
            }
            let hex = rest.split(|&b| b == b' ').next().unwrap_or_default();
            let id = ObjectId::from_hex_any(hex).ok()?;
            let mut text = Vec::new();
            text.extend_from_slice(b" ## Metadata ##\n");
            current = Some(Patch {
                id,
                text,
                diff_start: usize::MAX,
                diff_lines: 0,
            });
            state = State::Header;
            files = 0;
            continue;
        }
        let p = current.as_mut()?;
        if let Some(rest) = line.strip_prefix(b"diff --git ") {
            finish_header(p, &mut header, &mut hunk_name);
            if files == 0 {
                p.diff_start = p.text.len();
            } else {
                p.text.push(b'\n');
            }
            files += 1;
            header = Some(FileHeader::parse_git_line(rest));
            state = State::FileHeader;
            continue;
        }
        if line.is_empty() && matches!(state, State::FileHeader | State::Hunks) {
            // The blank line between two commits.
            continue;
        }
        match state {
            State::Header => {
                if line.starts_with(b"Author: ") {
                    p.text.extend_from_slice(line);
                    p.text.push(b'\n');
                } else if line.is_empty() {
                    p.text.extend_from_slice(b"\n ## Commit message ##\n");
                    state = State::Message;
                }
                // `Date:`, `Merge:` and other header lines are dropped.
            }
            State::Message => {
                if let Some(rest) = line.strip_prefix(b"Notes") {
                    if line.ends_with(b":") && !line.starts_with(b" ") {
                        // `Notes:` or `Notes (<ref>):`.
                        p.text.extend_from_slice(b"\n ## Notes");
                        p.text.extend_from_slice(&rest[..rest.len() - 1]);
                        p.text.extend_from_slice(b" ##\n");
                        continue;
                    }
                }
                if line.trim_ascii_end().is_empty() {
                    p.text.push(b'\n');
                } else {
                    p.text.extend_from_slice(line);
                    p.text.push(b'\n');
                }
            }
            State::FileHeader => {
                let h = header.as_mut().expect("in a file header");
                if let Some(v) = line.strip_prefix(b"old mode ") {
                    h.old_mode = Some(v.to_vec());
                } else if let Some(v) = line.strip_prefix(b"new mode ") {
                    h.new_mode = Some(v.to_vec());
                } else if line.starts_with(b"new file mode ") {
                    h.new_file = true;
                } else if line.starts_with(b"deleted file mode ") {
                    h.deleted = true;
                } else if let Some(v) = line
                    .strip_prefix(b"rename from ")
                    .or_else(|| line.strip_prefix(b"copy from "))
                {
                    h.renamed = true;
                    h.old_name = Some(unquote(v));
                } else if let Some(v) = line
                    .strip_prefix(b"rename to ")
                    .or_else(|| line.strip_prefix(b"copy to "))
                {
                    h.renamed = true;
                    h.new_name = Some(unquote(v));
                } else if let Some(v) = line.strip_prefix(b"--- ") {
                    if v != b"/dev/null" {
                        h.old_name = Some(unquote(v));
                    }
                } else if let Some(v) = line.strip_prefix(b"+++ ") {
                    if v != b"/dev/null" {
                        h.new_name = Some(unquote(v));
                    }
                } else if line.starts_with(b"index ")
                    || line.starts_with(b"similarity index ")
                    || line.starts_with(b"dissimilarity index ")
                {
                } else {
                    finish_header(p, &mut header, &mut hunk_name);
                    state = State::Hunks;
                    hunk_line(p, line, &hunk_name);
                }
            }
            State::Hunks => hunk_line(p, line, &hunk_name),
        }
    }
    if let Some(mut p) = current.take() {
        finish_header(&mut p, &mut header, &mut hunk_name);
        patches.push(finish(p));
    }
    Some(patches)
}

/// One line of a hunk (or another body line such as `Binary files`).
fn hunk_line(p: &mut Patch, line: &[u8], name: &[u8]) {
    match line.first() {
        Some(b'@') if line.starts_with(b"@@ ") => {
            p.text.extend_from_slice(b"@@");
            // `@@ -a,b +c,d @@ <function>`.
            let func = line[3..]
                .windows(3)
                .position(|w| w == b" @@")
                .map(|i| &line[3 + i + 3..])
                .unwrap_or_default();
            if let Some(func) = func.strip_prefix(b" ")
                && !func.is_empty()
            {
                p.text.push(b' ');
                p.text.extend_from_slice(name);
                p.text.extend_from_slice(b": ");
                p.text.extend_from_slice(func);
            }
        }
        Some(b'>') => {
            p.text.push(b'+');
            p.text.extend_from_slice(&line[1..]);
        }
        Some(b'<') => {
            p.text.push(b'-');
            p.text.extend_from_slice(&line[1..]);
        }
        Some(b'#') => {
            p.text.push(b' ');
            p.text.extend_from_slice(&line[1..]);
        }
        _ => {
            p.text.push(b' ');
            p.text.extend_from_slice(line);
        }
    }
    p.text.push(b'\n');
}

/// Without files the whole patch stands for the diff in the cost of a
/// pairing, but its size is 0. The blank lines between files do not
/// count (observed from the `git` binary).
fn finish(mut p: Patch) -> Patch {
    if p.diff_start == usize::MAX {
        p.diff_start = 0;
        p.diff_lines = 0;
        return p;
    }
    p.diff_lines = p.text[p.diff_start..]
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .count();
    p
}
