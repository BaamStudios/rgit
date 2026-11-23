// SPDX-License-Identifier: MIT
//! The pieces of a patch mail: the subject prefix, the commit message
//! split into subject and body, file names, threading headers and the
//! signature.

/// The subject paragraph of `message` joined with spaces, and the rest
/// (without the blank lines after the subject and at the end), ending
/// with a newline unless empty.
pub fn split_message(message: &[u8]) -> (Vec<u8>, Vec<u8>) {
    split_message_with(message, b' ')
}

/// [`split_message`] with the subject lines joined by `sep` (`-k` keeps
/// them apart with a newline, which then gets encoded).
pub fn split_message_with(message: &[u8], sep: u8) -> (Vec<u8>, Vec<u8>) {
    let lines: Vec<&[u8]> = message.split(|&b| b == b'\n').collect();
    let blank = |l: &[u8]| l.iter().all(u8::is_ascii_whitespace);
    let mut i = 0;
    while i < lines.len() && blank(lines[i]) {
        i += 1;
    }
    let mut subject = Vec::new();
    while i < lines.len() && !blank(lines[i]) {
        if !subject.is_empty() {
            subject.push(sep);
        }
        subject.extend_from_slice(lines[i].trim_ascii_end());
        i += 1;
    }
    while i < lines.len() && blank(lines[i]) {
        i += 1;
    }
    let mut end = lines.len();
    while end > i && blank(lines[end - 1]) {
        end -= 1;
    }
    let mut body = Vec::new();
    for line in &lines[i..end] {
        body.extend_from_slice(line.trim_ascii_end());
        body.push(b'\n');
    }
    (subject, body)
}

/// mboxrd quoting: one more `>` before every line that is `From ` after
/// any number of `>`.
pub fn mboxrd_escape(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for line in text.split_inclusive(|&b| b == b'\n') {
        let quotes = line.iter().take_while(|&&b| b == b'>').count();
        if line[quotes..].starts_with(b"From ") {
            out.push(b'>');
        }
        out.extend_from_slice(line);
    }
    out
}

/// The subject prefix before numbers: `[--rfc] <prefix> [v<n>]`.
pub fn subject_prefix(base: &str, rfc: Option<&str>, reroll: Option<&str>) -> String {
    let mut prefix = base.to_string();
    match rfc {
        Some(r) if r.starts_with('-') => {
            prefix = join_words(&prefix, &r[1..]);
        }
        Some(r) if !r.is_empty() => prefix = join_words(r, &prefix),
        _ => {}
    }
    if let Some(v) = reroll {
        prefix = join_words(&prefix, &format!("v{v}"));
    }
    prefix
}

fn join_words(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_string(),
        (_, true) => a.to_string(),
        _ => format!("{a} {b}"),
    }
}

/// The bracketed part of a subject: `[PATCH n/m] `, `[PATCH] ` or nothing.
pub fn subject_tag(prefix: &str, number: Option<(usize, usize)>) -> String {
    match number {
        Some((n, total)) => {
            let width = total.to_string().len();
            if prefix.is_empty() {
                format!("[{n:0width$}/{total}] ")
            } else {
                format!("[{prefix} {n:0width$}/{total}] ")
            }
        }
        None if prefix.is_empty() => String::new(),
        None => format!("[{prefix}] "),
    }
}

/// Turn a subject into a file name part: letters, digits, `.` and `_`
/// stay, other runs become one `-`, repeated dots collapse, and nothing
/// separates at the start or ends the name with a dot.
pub fn sanitize(text: &[u8]) -> String {
    let mut out = String::new();
    let mut pending = false;
    for &b in text {
        if b.is_ascii_alphanumeric() || b == b'.' || b == b'_' {
            if pending && !out.is_empty() {
                out.push('-');
            }
            pending = false;
            if b == b'.' && out.ends_with('.') {
                continue;
            }
            out.push(b as char);
        } else {
            pending = true;
        }
    }
    while out.ends_with('.') {
        out.pop();
    }
    out
}

/// The file name of a patch: `[v<n>-]<nnnn>[-<subject>]<suffix>`, cut so
/// that it stays shorter than `max` (the number is never cut).
pub fn file_name(
    number: usize,
    subject: Option<&[u8]>,
    reroll: Option<&str>,
    suffix: &str,
    max: usize,
) -> String {
    let mut name = String::new();
    if let Some(v) = reroll {
        name.push_str(&sanitize(format!("v{v}").as_bytes()));
        name.push('-');
    }
    name.push_str(&format!("{number:04}"));
    let fixed = name.len();
    if let Some(s) = subject {
        let s = sanitize(s);
        if !s.is_empty() {
            name.push('-');
            name.push_str(&s);
        }
    }
    let budget = max.saturating_sub(suffix.len() + 1).max(fixed);
    if name.len() > budget {
        let mut cut = budget;
        while !name.is_char_boundary(cut) {
            cut -= 1;
        }
        name.truncate(cut);
    }
    name.push_str(suffix);
    name
}

/// The signature block: `-- `, the signature, an empty line.
pub fn signature_block(out: &mut Vec<u8>, signature: &[u8]) {
    out.extend_from_slice(b"-- \n");
    out.extend_from_slice(signature);
    if !signature.ends_with(b"\n") {
        out.push(b'\n');
    }
    out.push(b'\n');
}

/// A patch as the prerequisite patch ids of `--base` see it: per file
/// the `diff --git` line, the mode lines and always a `---`/`+++` pair
/// (also for renames, mode changes and empty files, whose patches have
/// none), without the similarity, rename, copy and index lines. (Found
/// by comparing the ids git writes with `git patch-id --stable` of
/// constructed patches.)
pub fn patch_id_text(patch: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(patch.len());
    let lines: Vec<&[u8]> = patch.split_inclusive(|&b| b == b'\n').collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let Some(names) = line.strip_prefix(b"diff --git ") else {
            out.extend_from_slice(line);
            i += 1;
            continue;
        };
        out.extend_from_slice(line);
        i += 1;
        // Both names: halves of the line unless a rename or copy says.
        let names = names.strip_suffix(b"\n").unwrap_or(names);
        let half = names.len().saturating_sub(1) / 2;
        let mut old = names[..half].to_vec();
        let mut new = names[(half + 1).min(names.len())..].to_vec();
        let (mut created, mut deleted, mut has_lines) = (false, false, false);
        while i < lines.len() && !lines[i].starts_with(b"diff --git ") {
            let l = lines[i];
            if l.starts_with(b"--- ") || l.starts_with(b"@@") || l.starts_with(b"Binary files") {
                has_lines = true;
                break;
            }
            if let Some(p) = l
                .strip_prefix(b"rename from ")
                .or_else(|| l.strip_prefix(b"copy from "))
            {
                old = [b"a/", p.trim_ascii_end()].concat();
            } else if let Some(p) = l
                .strip_prefix(b"rename to ")
                .or_else(|| l.strip_prefix(b"copy to "))
            {
                new = [b"b/", p.trim_ascii_end()].concat();
            } else if l.starts_with(b"new file mode ") {
                created = true;
                out.extend_from_slice(l);
            } else if l.starts_with(b"deleted file mode ") {
                deleted = true;
                out.extend_from_slice(l);
            } else if l.starts_with(b"old mode ") || l.starts_with(b"new mode ") {
                out.extend_from_slice(l);
            }
            i += 1;
        }
        if !has_lines {
            out.extend_from_slice(b"--- ");
            out.extend_from_slice(if created { b"/dev/null" } else { &old[..] });
            out.extend_from_slice(b"\n+++ ");
            out.extend_from_slice(if deleted { b"/dev/null" } else { &new[..] });
            out.push(b'\n');
        }
    }
    out
}

/// `<id>` for a message id given on the command line.
pub fn angle(id: &str) -> String {
    let id = id.trim();
    let id = id.strip_prefix('<').unwrap_or(id);
    let id = id.strip_suffix('>').unwrap_or(id);
    format!("<{id}>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizing() {
        assert_eq!(
            sanitize(b"  [foo] Fix: a..b_c  -- x.y/z"),
            "foo-Fix-a.b_c-x.y-z"
        );
        assert_eq!(sanitize(b"...dots..."), ".dots");
        assert_eq!(sanitize(b"a.. .b"), "a.-.b");
        assert_eq!(sanitize("\u{dc}n\u{ef}c".as_bytes()), "n-c");
        assert_eq!(sanitize(b"v4---..././../--1/.2//"), "v4-.-.-.-1-.2");
    }

    #[test]
    fn file_names() {
        let s = b"Rename g to h with a pretty long subject line that goes on and on";
        assert_eq!(
            file_name(1, Some(s), None, ".patch", 64),
            "0001-Rename-g-to-h-with-a-pretty-long-subject-line-that-g.patch"
        );
        assert_eq!(
            file_name(1, Some(s), Some("12"), ".patch", 64),
            "v12-0001-Rename-g-to-h-with-a-pretty-long-subject-line-th.patch"
        );
        assert_eq!(file_name(1, Some(s), None, ".patch", 12), "0001-.patch");
        assert_eq!(file_name(1, Some(s), None, ".patch", 5), "0001.patch");
        assert_eq!(file_name(0, None, None, ".patch", 64), "0000.patch");
    }

    #[test]
    fn prefixes() {
        assert_eq!(
            subject_prefix("PATCH", Some("RFC"), Some("3")),
            "RFC PATCH v3"
        );
        assert_eq!(subject_prefix("PATCH", Some("-(WIP)"), None), "PATCH (WIP)");
        assert_eq!(subject_prefix("", None, None), "");
        assert_eq!(subject_tag("PATCH", Some((1, 10))), "[PATCH 01/10] ");
        assert_eq!(subject_tag("", Some((1, 1))), "[1/1] ");
        assert_eq!(subject_tag("PATCH", None), "[PATCH] ");
    }

    #[test]
    fn messages() {
        let (s, b) = split_message(b"\none\ntwo\n\n\nbody\n\n\nmore\n\n\n");
        assert_eq!(s, b"one two");
        assert_eq!(b, b"body\n\n\nmore\n");
        let (_, b) = split_message(b"s\n\ntrailing  \n\tx\t\n");
        assert_eq!(b, b"trailing\n\tx\n");
        let (s, _) = split_message_with(b"one  \n two\n\nbody\n", b'\n');
        assert_eq!(s, b"one\n two");
    }

    #[test]
    fn patch_id_texts() {
        assert_eq!(
            patch_id_text(
                b"diff --git a/g b/h\nsimilarity index 100%\nrename from g\nrename to h\n\
                  diff --git a/m b/m\nold mode 100644\nnew mode 100755\n\
                  diff --git a/e b/e\nnew file mode 100644\nindex 0000000..e69de29\n\
                  diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+b\n"
            ),
            b"diff --git a/g b/h\n--- a/g\n+++ b/h\n\
              diff --git a/m b/m\nold mode 100644\nnew mode 100755\n--- a/m\n+++ b/m\n\
              diff --git a/e b/e\nnew file mode 100644\n--- /dev/null\n+++ b/e\n\
              diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+b\n"
        );
    }

    #[test]
    fn mboxrd() {
        assert_eq!(
            mboxrd_escape(b"From a\n>From b\nfrom c\n From d\nFromm\n"),
            b">From a\n>>From b\nfrom c\n From d\nFromm\n"
        );
    }
}
