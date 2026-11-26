// SPDX-License-Identifier: MIT
//! Notes as the `git log` family shows them.

use crate::refname;

/// Appends the note `note` of notes ref `refname`: a header line
/// (`Notes:` for the default ref, else `Notes (<name>):`) and the lines of
/// the note indented by four spaces (with `indent`), the last one ending
/// with a newline.
pub fn append(out: &mut Vec<u8>, refname: &str, note: &[u8], indent: bool) {
    match refname::display_name(refname) {
        None => out.extend_from_slice(b"Notes:\n"),
        Some(name) => {
            out.extend_from_slice(b"Notes (");
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(b"):\n");
        }
    }
    append_body(out, note, indent);
}

/// Appends the lines of `note`, each indented by four spaces (with
/// `indent`) and ending with a newline.
pub fn append_body(out: &mut Vec<u8>, note: &[u8], indent: bool) {
    if note.is_empty() {
        return;
    }
    let body = note.strip_suffix(b"\n").unwrap_or(note);
    for line in body.split(|&c| c == b'\n') {
        if indent {
            out.extend_from_slice(b"    ");
        }
        out.extend_from_slice(line);
        out.push(b'\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indents_every_line() {
        let mut out = Vec::new();
        append(&mut out, "refs/notes/commits", b"a\n\nb", true);
        assert_eq!(out, b"Notes:\n    a\n    \n    b\n");
        let mut out = Vec::new();
        append(&mut out, "refs/notes/x", b"", true);
        assert_eq!(out, b"Notes (x):\n");
    }
}
