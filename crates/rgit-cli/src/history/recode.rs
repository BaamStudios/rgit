// SPDX-License-Identifier: MIT
//! Re-encoding commits for output (Documentation/git-log.adoc,
//! "DISCUSSION"; `i18n.logOutputEncoding`, `i18n.commitEncoding`).
//!
//! A commit whose `encoding` header names another encoding than the output
//! encoding is converted before it is shown (rgit_convert::encoding: the
//! iconv names git's scripts use, ISO-8859-*, EUC-JP, ISO-2022-JP,
//! Shift_JIS, UTF-16/32, ...); commits that cannot be converted are shown
//! as stored, as git does when iconv fails.

use rgit_config::ConfigSet;

/// The encoding output is written in.
pub fn output_encoding(config: &ConfigSet) -> Vec<u8> {
    for key in ["i18n.logOutputEncoding", "i18n.commitEncoding"] {
        if let Ok(Some(v)) = config.string(key) {
            return v.to_vec();
        }
    }
    b"UTF-8".to_vec()
}

/// The raw commit `raw` (stored in `encoding`, UTF-8 when `None`)
/// converted to `output`, or `None` when it stays as it is (the same
/// encoding, or text that cannot be converted).
pub fn reencode(raw: &[u8], encoding: Option<&[u8]>, output: &[u8]) -> Option<Vec<u8>> {
    let from = String::from_utf8_lossy(encoding.unwrap_or(b"UTF-8"));
    let to = String::from_utf8_lossy(output);
    if rgit_convert::encoding::same_encoding(&from, &to) {
        return None;
    }
    rgit_convert::encoding::transcode(raw, &from, &to)
}

/// [`reencode`] for user formats (`--format`): when the conversion to
/// `output` fails (unknown names such as `none`), the commit is shown in
/// UTF-8 (observed from git 2.56; built-in formats show it as stored).
pub fn reencode_for_format(raw: &[u8], encoding: Option<&[u8]>, output: &[u8]) -> Option<Vec<u8>> {
    let from = String::from_utf8_lossy(encoding.unwrap_or(b"UTF-8"));
    let to = String::from_utf8_lossy(output);
    if rgit_convert::encoding::same_encoding(&from, &to) {
        return None;
    }
    rgit_convert::encoding::transcode(raw, &from, &to).or_else(|| {
        if rgit_convert::encoding::same_encoding(&from, "UTF-8") {
            None
        } else {
            rgit_convert::encoding::transcode(raw, &from, "UTF-8")
        }
    })
}

/// `commit` converted to the output encoding `output`; `None` when it is
/// shown as stored.
pub fn for_output(
    commit: &rgit_object::Commit,
    output: &[u8],
    hash: rgit_hash::HashKind,
) -> Option<rgit_object::Commit> {
    let data = reencode(&commit.serialize(), commit.encoding(), output)?;
    rgit_object::Commit::parse(hash, &data).ok()
}

/// The encoding new commits are written in: `i18n.commitEncoding`, else
/// UTF-8.
pub fn commit_encoding(config: &ConfigSet) -> String {
    match config.string("i18n.commitEncoding") {
        Ok(Some(v)) => String::from_utf8_lossy(v.as_ref()).into_owned(),
        _ => "UTF-8".to_owned(),
    }
}

/// The raw commit `raw` with its `encoding` header set to `to` (removed
/// for UTF-8).
fn replace_encoding_header(raw: &[u8], to: &str) -> Vec<u8> {
    let end = raw
        .windows(2)
        .position(|w| w == b"\n\n")
        .map_or(raw.len(), |p| p + 1);
    let (head, body) = raw.split_at(end);
    let mut out = Vec::with_capacity(raw.len() + 16);
    let mut done = false;
    let utf8 = rgit_convert::encoding::same_encoding(to, "UTF-8");
    for line in head.split_inclusive(|&b| b == b'\n') {
        if line.starts_with(b"encoding ") {
            done = true;
            if !utf8 {
                out.extend_from_slice(format!("encoding {to}\n").as_bytes());
            }
        } else {
            out.extend_from_slice(line);
        }
    }
    if !done && !utf8 {
        out.extend_from_slice(format!("encoding {to}\n").as_bytes());
    }
    out.extend_from_slice(body);
    out
}

/// `commit` as the sequencer reuses it for a new commit: converted to
/// the commit encoding ([`commit_encoding`]) with the `encoding` header
/// replaced, as git does; unchanged when the conversion fails.
pub fn for_new_commit(
    config: &ConfigSet,
    hash: rgit_hash::HashKind,
    commit: rgit_object::Commit,
) -> rgit_object::Commit {
    let to = commit_encoding(config);
    let from = String::from_utf8_lossy(commit.encoding().unwrap_or(b"UTF-8")).into_owned();
    if rgit_convert::encoding::same_encoding(&from, &to) {
        return commit;
    }
    let raw = commit.serialize();
    let Some(data) = rgit_convert::encoding::transcode(&raw, &from, &to) else {
        return commit;
    };
    let data = replace_encoding_header(&data, &to);
    rgit_object::Commit::parse(hash, &data).unwrap_or(commit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions() {
        assert_eq!(
            reencode(b"a\xe4", Some(b"ISO8859-1"), b"utf-8").unwrap(),
            "aä".as_bytes()
        );
        assert_eq!(
            reencode("aä".as_bytes(), None, b"latin1").unwrap(),
            b"a\xe4"
        );
        assert_eq!(reencode(b"x", Some(b"UTF-8"), b"utf8"), None);
        assert_eq!(reencode("€".as_bytes(), None, b"ISO-8859-1"), None);
        assert_eq!(
            reencode(b"\x82\xa0", Some(b"SHIFT-JIS"), b"utf8").unwrap(),
            "\u{3042}".as_bytes()
        );
        assert_eq!(
            reencode(b"\xa4\xa2", Some(b"eucJP"), b"utf-8").unwrap(),
            "\u{3042}".as_bytes()
        );
    }
}
