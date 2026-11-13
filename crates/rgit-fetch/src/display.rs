// SPDX-License-Identifier: MIT
//! The summary on standard error (`From <url>` and one line per ref) and
//! the contents of `FETCH_HEAD`.
//!
//! Layout, as observed from git 2.56 (the default `full` format):
//!
//! ```text
//!  <flag> <summary> <remote> -> <local><suffix>
//! ```
//!
//! - `<summary>` is padded to `2 * abbrev + 3` columns, except when any
//!   ref of the fetch was rejected: then it is not padded at all.
//! - `<remote>` is padded to the widest remote name (at least 10) among
//!   the shown lines that store into a local ref and whose whole line
//!   would stay narrower than the terminal (`21 + remote + 4 + local`
//!   below the column count; 80 when not on a terminal).
//! - Up-to-date refs are shown only with `-v`.

use bstr::{BStr, ByteSlice};
use rgit_hash::ObjectId;

use crate::messages;
use crate::refmap::{Entry, Origin};
use crate::update::{RefLine, Status};

/// Strip `refs/heads/`, `refs/tags/` or `refs/remotes/`.
pub fn pretty_ref(name: &BStr) -> String {
    let s = name.to_str_lossy();
    for p in ["refs/heads/", "refs/tags/", "refs/remotes/"] {
        if let Some(rest) = s.strip_prefix(p) {
            return rest.to_string();
        }
    }
    s.into_owned()
}

/// The URL as shown in the summary and in `FETCH_HEAD`: without
/// credentials, trailing slashes and a final `.git`.
pub fn display_url(url: &str) -> String {
    let mut u = anonymize(url);
    while u.len() > 1 && u.ends_with('/') {
        u.pop();
    }
    if let Some(s) = u.strip_suffix(".git") {
        u = s.to_string();
    }
    u
}

/// Remove `user[:password]@` from a `<scheme>://` URL.
pub fn anonymize(url: &str) -> String {
    let Some(i) = url.find("://") else {
        return url.to_string();
    };
    let rest = &url[i + 3..];
    let end = rest.find('/').unwrap_or(rest.len());
    match rest[..end].rfind('@') {
        Some(at) => format!("{}{}", &url[..i + 3], &rest[at + 1..]),
        None => url.to_string(),
    }
}

/// How the summary is printed.
pub struct DisplayOptions<'a> {
    /// -1 for `-q`, 0 by default, 1 or more for `-v`.
    pub verbosity: i32,
    /// Terminal width (80 when not a terminal or unknown).
    pub columns: usize,
    /// Abbreviated object names.
    pub abbrev: &'a dyn Fn(&ObjectId) -> String,
    /// The default abbreviation length (for the summary column).
    pub abbrev_len: usize,
}

fn kind_word(remote: &BStr) -> &'static str {
    if remote.starts_with(b"refs/tags/") {
        "tag"
    } else if remote.starts_with(b"refs/remotes/") {
        "remote-tracking branch"
    } else {
        "branch"
    }
}

fn shown(line: &RefLine, verbosity: i32) -> bool {
    match line.status {
        _ if verbosity < 0 => false,
        Status::UpToDate => verbosity > 0,
        _ => true,
    }
}

/// The columns of one line: flag, summary, remote, local, suffix.
fn columns(line: &RefLine, o: &DisplayOptions<'_>) -> (char, String, String, String, String) {
    let remote = match &line.remote {
        Some(r) => pretty_ref(r.as_bstr()),
        None => messages::NONE.to_string(),
    };
    let local = match &line.local {
        Some(l) => pretty_ref(l.as_bstr()),
        None => "FETCH_HEAD".to_string(),
    };
    let ab = o.abbrev;
    let range = |old: &ObjectId, sep: &str| format!("{}{sep}{}", ab(old), ab(&line.new));
    let (flag, summary, suffix) = summary(&line.status, line, &range);
    (flag, summary, remote, local, suffix)
}

fn summary(
    status: &Status,
    line: &RefLine,
    range: &dyn Fn(&ObjectId, &str) -> String,
) -> (char, String, String) {
    let paren = |s: &str| format!("  ({s})");
    match status {
        Status::NewBranch => ('*', messages::NEW_BRANCH.into(), String::new()),
        Status::NewTag => ('*', messages::NEW_TAG.into(), String::new()),
        Status::NewRef => ('*', messages::NEW_REF.into(), String::new()),
        Status::FastForward(old) => (' ', range(old, ".."), String::new()),
        Status::Forced(old) => ('+', range(old, "..."), paren(messages::FORCED_UPDATE)),
        Status::TagUpdate(_) => ('t', messages::TAG_UPDATE.into(), String::new()),
        Status::UpToDate => ('=', messages::UP_TO_DATE.into(), String::new()),
        Status::RejectedNonFastForward => (
            '!',
            messages::REJECTED.into(),
            paren(messages::NON_FAST_FORWARD),
        ),
        Status::RejectedTag => (
            '!',
            messages::REJECTED.into(),
            paren(messages::WOULD_CLOBBER_TAG),
        ),
        Status::Failed(inner) => {
            let (_, s, _) = summary(inner, line, range);
            ('!', s, paren(messages::UNABLE_TO_UPDATE))
        }
        Status::FetchHead => {
            let word = line
                .remote
                .as_ref()
                .map_or("branch", |r| kind_word(r.as_bstr()));
            ('*', word.into(), String::new())
        }
        Status::Deleted => ('-', messages::DELETED.into(), String::new()),
    }
}

/// The summary text (each line ends with a newline), or `""` when nothing
/// is to be shown.
pub fn render(url: &str, lines: &[RefLine], o: &DisplayOptions<'_>) -> String {
    let visible: Vec<&RefLine> = lines.iter().filter(|l| shown(l, o.verbosity)).collect();
    if visible.is_empty() {
        return String::new();
    }
    let any_rejected = lines.iter().any(|l| {
        matches!(
            l.status,
            Status::RejectedNonFastForward | Status::RejectedTag
        )
    });
    let summary_width = if any_rejected {
        0
    } else {
        2 * o.abbrev_len + 3
    };
    let cols: Vec<_> = visible.iter().map(|l| (l, columns(l, o))).collect();
    let mut refcol = 10;
    for (l, (_, _, remote, local, _)) in &cols {
        if l.local.is_none() {
            continue;
        }
        let rlen = remote.chars().count();
        let llen = local.chars().count();
        if 21 + rlen + 4 + llen < o.columns {
            refcol = refcol.max(rlen);
        }
    }
    let mut out = messages::from_url(url);
    out.push('\n');
    for (_, (flag, summary, remote, local, suffix)) in cols {
        out.push_str(&format!(
            " {flag} {summary:<summary_width$} {remote:<refcol$} -> {local}{suffix}\n"
        ));
    }
    out
}

/// The text of `FETCH_HEAD` for `entries`: merge candidates first, then
/// the rest marked `not-for-merge`. Opportunistic updates are left out.
pub fn fetch_head(url: &str, entries: &[Entry]) -> String {
    let mut out = String::new();
    let listed = entries.iter().filter(|e| e.origin != Origin::Opportunistic);
    let (merge, rest): (Vec<&Entry>, Vec<&Entry>) = listed.partition(|e| e.for_merge);
    for e in merge.into_iter().chain(rest) {
        let note = if e.for_merge { "" } else { "not-for-merge" };
        let r = e.remote.to_str_lossy();
        let desc = if r == "HEAD" {
            url.to_string()
        } else if let Some(b) = r.strip_prefix("refs/heads/") {
            format!("branch '{b}' of {url}")
        } else if let Some(t) = r.strip_prefix("refs/tags/") {
            format!("tag '{t}' of {url}")
        } else if let Some(t) = r.strip_prefix("refs/remotes/") {
            format!("remote-tracking branch '{t}' of {url}")
        } else {
            format!("'{r}' of {url}")
        };
        out.push_str(&format!("{}\t{note}\t{desc}\n", e.oid));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(display_url("../srv/.git/"), "../srv/");
        assert_eq!(display_url("https://u:p@h/x.git"), "https://h/x");
        assert_eq!(display_url("."), ".");
    }

    fn oid(c: char) -> ObjectId {
        ObjectId::from_hex(
            rgit_hash::HashKind::Sha1,
            c.to_string().repeat(40).as_bytes(),
        )
        .unwrap()
    }

    fn line(remote: &str, local: Option<&str>, status: Status) -> RefLine {
        RefLine {
            remote: Some(remote.into()),
            local: local.map(Into::into),
            new: oid('b'),
            status,
        }
    }

    fn opts(verbosity: i32) -> DisplayOptions<'static> {
        DisplayOptions {
            verbosity,
            columns: 80,
            abbrev: &|id: &ObjectId| id.to_hex()[..7].to_string(),
            abbrev_len: 7,
        }
    }

    #[test]
    fn layout() {
        let lines = [
            line(
                "refs/heads/main",
                Some("refs/remotes/o/main"),
                Status::FastForward(oid('a')),
            ),
            line("refs/tags/v1", Some("refs/tags/v1"), Status::NewTag),
            line("refs/heads/x", None, Status::FetchHead),
            line("refs/heads/u", Some("refs/remotes/o/u"), Status::UpToDate),
        ];
        assert_eq!(
            render("u", &lines, &opts(0)),
            "From u\n   aaaaaaa..bbbbbbb  main       -> o/main\n \
             * [new tag]         v1         -> v1\n \
             * branch            x          -> FETCH_HEAD\n"
        );
        assert_eq!(render("u", &lines, &opts(-1)), "");
        assert!(
            render("u", &lines, &opts(1)).ends_with(" = [up to date]      u          -> o/u\n")
        );
    }

    #[test]
    fn rejected_removes_summary_padding() {
        let lines = [
            line(
                "refs/heads/main",
                Some("refs/remotes/o/main"),
                Status::RejectedNonFastForward,
            ),
            line("refs/heads/s", Some("refs/remotes/o/s"), Status::NewBranch),
        ];
        assert_eq!(
            render("u", &lines, &opts(0)),
            "From u\n ! [rejected] main       -> o/main  (non-fast-forward)\n \
             * [new branch] s          -> o/s\n"
        );
    }

    #[test]
    fn fetch_head_text() {
        let e = |r: &str, merge: bool, origin: Origin| Entry {
            remote: r.into(),
            oid: oid('c'),
            local: None,
            force: false,
            origin,
            for_merge: merge,
        };
        let entries = [
            e("refs/heads/a", false, Origin::Refspec),
            e("HEAD", true, Origin::Refspec),
            e("refs/notes/n", false, Origin::Refspec),
            e("refs/heads/o", false, Origin::Opportunistic),
            e("refs/tags/t", false, Origin::FollowedTag),
        ];
        let c = "c".repeat(40);
        assert_eq!(
            fetch_head("u", &entries),
            format!(
                "{c}\t\tu\n{c}\tnot-for-merge\tbranch 'a' of u\n\
                 {c}\tnot-for-merge\t'refs/notes/n' of u\n{c}\tnot-for-merge\ttag 't' of u\n"
            )
        );
    }
}
