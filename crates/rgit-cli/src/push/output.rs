// SPDX-License-Identifier: MIT
//! The status table of `git push` (`Documentation/git-push.adoc`,
//! "OUTPUT"), as git 2.56.0 prints it:
//!
//! - ` <flag> <summary> <from> -> <to>[ (<reason>)]` on stderr, the
//!   summary padded to 17 columns, ref names without `refs/heads/`,
//!   `refs/tags/` or `refs/remotes/`; deletions show only `<to>`;
//! - with `--porcelain`: `<flag>\t<from>:<to>\t<summary>[ (<reason>)]` on
//!   stdout with full names, framed by `To <url>` and `Done`;
//! - up-to-date refs first (only with `-v` or `--porcelain`), then
//!   successful updates, then failures, each group in list order.

use std::io::Write;

use rgit_hash::ObjectId;

use super::{PushRef, Status};

/// Width of the summary column (twice the default abbreviation plus
/// `...`).
const SUMMARY_WIDTH: usize = 17;

/// How to print.
pub struct Style<'a> {
    pub url: &'a str,
    pub verbose: bool,
    pub quiet: bool,
    pub porcelain: bool,
    pub abbrev: &'a dyn Fn(&ObjectId) -> String,
}

/// `refs/heads/x` → `x` (also `refs/tags/`, `refs/remotes/`).
pub fn prettify(name: &str) -> &str {
    for p in ["refs/heads/", "refs/tags/", "refs/remotes/"] {
        if let Some(rest) = name.strip_prefix(p) {
            return rest;
        }
    }
    name
}

/// Flag, summary and reason of one ref.
fn describe(r: &PushRef, style: &Style<'_>) -> (char, String, Option<String>) {
    let s = |x: &str| x.to_owned();
    match r.status {
        Status::UpToDate => ('=', s("[up to date]"), None),
        Status::Ok | Status::None => match (r.old, r.new) {
            (_, None) => ('-', s("[deleted]"), None),
            (None, Some(_)) => {
                let what = if r.dst.starts_with("refs/tags/") {
                    "[new tag]"
                } else if r.dst.starts_with("refs/heads/") {
                    "[new branch]"
                } else {
                    "[new reference]"
                };
                ('*', s(what), None)
            }
            (Some(old), Some(new)) => {
                let (o, n) = ((style.abbrev)(&old), (style.abbrev)(&new));
                if r.forced_update {
                    ('+', format!("{o}...{n}"), Some(s("forced update")))
                } else {
                    (' ', format!("{o}..{n}"), None)
                }
            }
        },
        Status::RejectNonFf => ('!', s("[rejected]"), Some(s("non-fast-forward"))),
        Status::RejectAlreadyExists => ('!', s("[rejected]"), Some(s("already exists"))),
        Status::RejectFetchFirst => ('!', s("[rejected]"), Some(s("fetch first"))),
        Status::RejectNeedsForce => ('!', s("[rejected]"), Some(s("needs force"))),
        Status::AtomicFailed => ('!', s("[rejected]"), Some(s("atomic push failed"))),
        Status::RemoteReject => ('!', s("[remote rejected]"), r.remote_msg.clone()),
        Status::RemoteFailure => (
            '!',
            s("[remote failure]"),
            Some(s("remote failed to report status")),
        ),
    }
}

fn line(r: &PushRef, style: &Style<'_>) -> String {
    let (flag, summary, reason) = describe(r, style);
    let reason = reason.map(|m| format!(" ({m})")).unwrap_or_default();
    let src = r.new.and(r.src.as_deref());
    if style.porcelain {
        format!("{flag}\t{}:{}\t{summary}{reason}", src.unwrap_or(""), r.dst)
    } else {
        let names = match src {
            Some(s) => format!("{} -> {}", prettify(s), prettify(&r.dst)),
            None => prettify(&r.dst).to_owned(),
        };
        format!(" {flag} {summary:<SUMMARY_WIDTH$} {names}{reason}")
    }
}

/// Print the table; returns whether any ref failed.
pub fn print(refs: &[PushRef], style: &Style<'_>) -> bool {
    let failed = refs.iter().any(|r| r.status.is_error());
    let mut lines = Vec::new();
    if style.porcelain || !style.quiet || failed {
        let groups: [&dyn Fn(&PushRef) -> bool; 3] = [
            &|r| r.status == Status::UpToDate && (style.verbose || style.porcelain),
            &|r| matches!(r.status, Status::Ok | Status::None),
            &|r| r.status.is_error(),
        ];
        for g in groups {
            for r in refs.iter().filter(|r| g(r)) {
                lines.push(line(r, style));
            }
        }
    }
    let mut text = String::new();
    if !lines.is_empty() || style.porcelain {
        text.push_str(&format!("To {}\n", style.url));
        for l in lines {
            text.push_str(&l);
            text.push('\n');
        }
    }
    if style.porcelain {
        text.push_str("Done\n");
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
    } else {
        let _ = std::io::stderr().write_all(text.as_bytes());
    }
    failed
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgit_hash::HashKind;

    fn oid(n: u8) -> ObjectId {
        ObjectId::from_hex(HashKind::Sha1, format!("{n:02x}").repeat(20).as_bytes()).unwrap()
    }

    fn pref(src: &str, dst: &str, old: Option<u8>, new: Option<u8>, status: Status) -> PushRef {
        PushRef {
            dst: dst.into(),
            old: old.map(oid),
            new: new.map(oid),
            src: Some(src.into()),
            force: false,
            status,
            forced_update: false,
            remote_msg: None,
        }
    }

    #[test]
    fn lines() {
        let ab = |id: &ObjectId| id.to_hex()[..7].to_owned();
        let style = Style {
            url: "u",
            verbose: false,
            quiet: false,
            porcelain: false,
            abbrev: &ab,
        };
        let r = pref(
            "refs/heads/main",
            "refs/heads/main",
            None,
            Some(1),
            Status::Ok,
        );
        assert_eq!(line(&r, &style), " * [new branch]      main -> main");
        let r = pref(
            "refs/heads/main",
            "refs/heads/main",
            Some(1),
            Some(2),
            Status::Ok,
        );
        assert_eq!(line(&r, &style), "   0101010..0202020  main -> main");
        let mut r = pref("HEAD", "refs/heads/x", Some(1), Some(2), Status::Ok);
        r.forced_update = true;
        assert_eq!(
            line(&r, &style),
            " + 0101010...0202020 HEAD -> x (forced update)"
        );
        let r = pref("x", "refs/tags/v1", Some(1), None, Status::Ok);
        assert_eq!(line(&r, &style), " - [deleted]         v1");
        let r = pref(
            "refs/heads/main",
            "refs/heads/z",
            Some(1),
            Some(2),
            Status::RejectNonFf,
        );
        assert_eq!(
            line(&r, &style),
            " ! [rejected]        main -> z (non-fast-forward)"
        );
        let porcelain = Style {
            porcelain: true,
            ..style
        };
        let r = pref("x", "refs/heads/side", Some(1), None, Status::Ok);
        assert_eq!(line(&r, &porcelain), "-\t:refs/heads/side\t[deleted]");
    }
}
