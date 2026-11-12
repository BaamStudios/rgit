// SPDX-License-Identifier: MIT

//! Strict validation of object content ("fsck" checks).
//!
//! The parsers in this crate are tolerant: they accept anything that can be
//! round-tripped. This module is the separate strict layer used by
//! `mktag`, `hash-object` (without `--literally`), `index-pack --strict`,
//! `fsck` and friends. It works on the raw bytes and reports findings
//! through a callback, identified by the message ids documented in
//! `fsck-msgids` (`badDate`, `missingTaggerEntry`, ...).
//!
//! The callback decides how to go on: [`Control::Stop`] ends the check (as
//! git does when a finding is configured as an error), [`Control::Continue`]
//! looks for further problems. Structural problems after which nothing can
//! be checked (for example a missing `tree` line) always end the check.
//! Mapping `fsck.<msg-id>` configuration to severities is the caller's job.
//!
//! Checks that need other objects (the `.gitmodules` blob content, whether
//! the tagged object exists) are not done here; [`special_file`] tells the
//! caller which tree entries need them.

use std::fmt;

use rgit_hash::{HashKind, ObjectId};

use crate::messages::fsck as msg;
use crate::{Error, ObjectKind, TreeIter, entry_cmp};

/// How serious a finding is by default (before `fsck.<msg-id>` config).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// The object cannot be checked further.
    Fatal,
    /// An error.
    Error,
    /// A warning.
    Warn,
    /// Informational; an error only in strict mode.
    Info,
    /// Not reported by default.
    Ignore,
}

macro_rules! msg_ids {
    ($($variant:ident = $name:literal, $sev:ident;)*) => {
        /// The id of an object finding (the subset of `fsck-msgids` that
        /// concerns a single object's content).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum MsgId {
            $(
                #[doc = concat!("`", $name, "`")]
                $variant,
            )*
        }

        impl MsgId {
            /// All ids.
            pub const ALL: &'static [MsgId] = &[$(MsgId::$variant,)*];

            /// The camelCase name used in messages and `fsck.<msg-id>` config.
            pub const fn name(self) -> &'static str {
                match self {
                    $(MsgId::$variant => $name,)*
                }
            }

            /// The default severity.
            pub const fn default_severity(self) -> Severity {
                match self {
                    $(MsgId::$variant => Severity::$sev,)*
                }
            }

            /// Look up an id by name, ignoring ASCII case (config keys are
            /// case-insensitive).
            pub fn from_name(name: &str) -> Option<MsgId> {
                MsgId::ALL
                    .iter()
                    .copied()
                    .find(|id| id.name().eq_ignore_ascii_case(name))
            }
        }
    };
}

msg_ids! {
    BadDate = "badDate", Error;
    BadDateOverflow = "badDateOverflow", Error;
    BadEmail = "badEmail", Error;
    BadFilemode = "badFilemode", Info;
    BadName = "badName", Error;
    BadObjectSha1 = "badObjectSha1", Error;
    BadParentSha1 = "badParentSha1", Error;
    BadTagName = "badTagName", Info;
    BadTimezone = "badTimezone", Error;
    BadTree = "badTree", Error;
    BadTreeSha1 = "badTreeSha1", Error;
    BadType = "badType", Error;
    DuplicateEntries = "duplicateEntries", Error;
    ExtraHeaderEntry = "extraHeaderEntry", Ignore;
    FullPathname = "fullPathname", Warn;
    GitattributesSymlink = "gitattributesSymlink", Info;
    GitignoreSymlink = "gitignoreSymlink", Info;
    GitmodulesSymlink = "gitmodulesSymlink", Error;
    HasDot = "hasDot", Warn;
    HasDotdot = "hasDotdot", Warn;
    HasDotgit = "hasDotgit", Warn;
    LargePathname = "largePathname", Warn;
    MailmapSymlink = "mailmapSymlink", Info;
    MissingAuthor = "missingAuthor", Error;
    MissingCommitter = "missingCommitter", Error;
    MissingEmail = "missingEmail", Error;
    MissingNameBeforeEmail = "missingNameBeforeEmail", Error;
    MissingObject = "missingObject", Error;
    MissingSpaceBeforeDate = "missingSpaceBeforeDate", Error;
    MissingSpaceBeforeEmail = "missingSpaceBeforeEmail", Error;
    MissingTagEntry = "missingTagEntry", Error;
    MissingTaggerEntry = "missingTaggerEntry", Info;
    MissingTree = "missingTree", Error;
    MissingTypeEntry = "missingTypeEntry", Error;
    MultipleAuthors = "multipleAuthors", Error;
    NulInCommit = "nulInCommit", Warn;
    NulInHeader = "nulInHeader", Fatal;
    NullSha1 = "nullSha1", Warn;
    TreeNotSorted = "treeNotSorted", Error;
    UnterminatedHeader = "unterminatedHeader", Fatal;
    ZeroPaddedDate = "zeroPaddedDate", Error;
    ZeroPaddedFilemode = "zeroPaddedFilemode", Warn;
}

impl fmt::Display for MsgId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.name())
    }
}

/// One problem found in an object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// The message id.
    pub id: MsgId,
    /// The detail text, e.g. `invalid author/committer line - bad date`.
    pub detail: String,
    /// For [`MsgId::BadTree`]: the parse error (git prints it on its own
    /// line before the finding).
    pub cause: Option<Error>,
}

impl Finding {
    fn new(id: MsgId, detail: impl Into<String>) -> Finding {
        Finding {
            id,
            detail: detail.into(),
            cause: None,
        }
    }
}

impl fmt::Display for Finding {
    /// `<msgId>: <detail>`, the form git prints after
    /// `error: object fails fsck: ` or `error: tag input does not pass fsck: `.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.id, self.detail)
    }
}

/// What to do after a finding was reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    /// Keep checking.
    Continue,
    /// Stop checking this object.
    Stop,
}

/// Tunables of the checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// Longest allowed tree entry name for `largePathname`
    /// (`fsck.largePathname`, default 4096).
    pub max_pathname_len: usize,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            max_pathname_len: 4096,
        }
    }
}

/// The reporting callback.
pub type Report<'a> = dyn FnMut(Finding) -> Control + 'a;

/// Check an object of `kind` (content without header). Blobs have no checks
/// of their own.
pub fn check_object(
    kind: ObjectKind,
    hash: HashKind,
    data: &[u8],
    options: &Options,
    report: &mut Report<'_>,
) {
    match kind {
        ObjectKind::Blob => {}
        ObjectKind::Tree => check_tree(hash, data, options, report),
        ObjectKind::Commit => check_commit(hash, data, report),
        ObjectKind::Tag => check_tag(hash, data, report),
    }
}

/// Run all checks of [`check_object`], continuing after every finding that
/// is not structural, and return the findings in report order.
pub fn findings(kind: ObjectKind, hash: HashKind, data: &[u8]) -> Vec<Finding> {
    let mut out = Vec::new();
    check_object(kind, hash, data, &Options::default(), &mut |f| {
        out.push(f);
        Control::Continue
    });
    out
}

/// Report and translate the callback's answer into "stop now?".
fn stop(report: &mut Report<'_>, f: Finding) -> bool {
    report(f) == Control::Stop
}

/// The header block must not contain NUL and must end in LF.
fn verify_headers(data: &[u8], report: &mut Report<'_>) -> bool {
    for (i, &b) in data.iter().enumerate() {
        match b {
            0 => {
                report(Finding::new(
                    MsgId::NulInHeader,
                    format!("{}{i}", msg::NUL_IN_HEADER),
                ));
                return false;
            }
            b'\n' if data.get(i + 1) == Some(&b'\n') => return true,
            _ => {}
        }
    }
    if data.last() == Some(&b'\n') {
        return true;
    }
    report(Finding::new(
        MsgId::UnterminatedHeader,
        msg::UNTERMINATED_HEADER,
    ));
    false
}

/// If `data` starts with a full hex id of `hash` followed by LF, return the
/// rest after the LF.
fn hex_line(hash: HashKind, data: &[u8]) -> Option<&[u8]> {
    let n = hash.hex_len();
    if data.len() <= n || data[n] != b'\n' {
        return None;
    }
    ObjectId::from_hex(hash, &data[..n]).ok()?;
    Some(&data[n + 1..])
}

/// The rest after the next LF (or empty).
fn next_line(data: &[u8]) -> &[u8] {
    match data.iter().position(|&b| b == b'\n') {
        Some(i) => &data[i + 1..],
        None => &[],
    }
}

/// Check the commit content `data`.
pub fn check_commit(hash: HashKind, data: &[u8], report: &mut Report<'_>) {
    if !verify_headers(data, report) {
        return;
    }
    let Some(rest) = data.strip_prefix(b"tree ") else {
        report(Finding::new(MsgId::MissingTree, msg::MISSING_TREE));
        return;
    };
    let Some(mut rest) = hex_line(hash, rest) else {
        report(Finding::new(MsgId::BadTreeSha1, msg::BAD_TREE_SHA1));
        return;
    };
    while let Some(r) = rest.strip_prefix(b"parent ") {
        match hex_line(hash, r) {
            Some(r) => rest = r,
            None => {
                report(Finding::new(MsgId::BadParentSha1, msg::BAD_PARENT_SHA1));
                return;
            }
        }
    }
    let mut authors = 0;
    while let Some(r) = rest.strip_prefix(b"author ") {
        authors += 1;
        if check_ident(r).is_some_and(|f| stop(report, f)) {
            return;
        }
        rest = next_line(r);
    }
    if authors == 0 {
        report(Finding::new(MsgId::MissingAuthor, msg::MISSING_AUTHOR));
        return;
    }
    if authors > 1
        && stop(
            report,
            Finding::new(MsgId::MultipleAuthors, msg::MULTIPLE_AUTHORS),
        )
    {
        return;
    }
    let Some(r) = rest.strip_prefix(b"committer ") else {
        report(Finding::new(
            MsgId::MissingCommitter,
            msg::MISSING_COMMITTER,
        ));
        return;
    };
    if check_ident(r).is_some_and(|f| stop(report, f)) {
        return;
    }
    if data.contains(&0) {
        report(Finding::new(MsgId::NulInCommit, msg::NUL_IN_COMMIT));
    }
}

/// Check the tag content `data`.
pub fn check_tag(hash: HashKind, data: &[u8], report: &mut Report<'_>) {
    if !verify_headers(data, report) {
        return;
    }
    let Some(rest) = data.strip_prefix(b"object ") else {
        report(Finding::new(MsgId::MissingObject, msg::MISSING_OBJECT));
        return;
    };
    let Some(rest) = hex_line(hash, rest) else {
        report(Finding::new(MsgId::BadObjectSha1, msg::BAD_OBJECT_SHA1));
        return;
    };
    let Some(rest) = rest.strip_prefix(b"type ") else {
        report(Finding::new(
            MsgId::MissingTypeEntry,
            msg::MISSING_TYPE_ENTRY,
        ));
        return;
    };
    let eol = rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
    if ObjectKind::from_name(&rest[..eol]).is_none() {
        report(Finding::new(MsgId::BadType, msg::BAD_TYPE));
        return;
    }
    let rest = next_line(rest);
    let Some(rest) = rest.strip_prefix(b"tag ") else {
        report(Finding::new(MsgId::MissingTagEntry, msg::MISSING_TAG_ENTRY));
        return;
    };
    let eol = rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
    let name = &rest[..eol];
    if !is_valid_tag_name(name) {
        let detail = format!("{}{}", msg::BAD_TAG_NAME, String::from_utf8_lossy(name));
        if stop(report, Finding::new(MsgId::BadTagName, detail)) {
            return;
        }
    }
    let mut rest = next_line(rest);
    if let Some(r) = rest.strip_prefix(b"tagger ") {
        if check_ident(r).is_some_and(|f| stop(report, f)) {
            return;
        }
        rest = next_line(r);
    } else if stop(
        report,
        Finding::new(MsgId::MissingTaggerEntry, msg::MISSING_TAGGER_ENTRY),
    ) {
        return;
    }
    if !rest.is_empty() && rest[0] != b'\n' {
        report(Finding::new(
            MsgId::ExtraHeaderEntry,
            msg::EXTRA_HEADER_ENTRY,
        ));
    }
}

/// Whether `refs/tags/<name>` is a valid ref name (the rules of
/// `git check-ref-format`).
pub fn is_valid_tag_name(name: &[u8]) -> bool {
    let mut full = b"refs/tags/".to_vec();
    full.extend_from_slice(name);
    is_valid_ref_name(&full)
}

/// Whether `name` is a valid full ref name under the rules documented in
/// `git-check-ref-format` (without `--allow-onelevel`, `--refspec-pattern`
/// or `--normalize`).
pub fn is_valid_ref_name(name: &[u8]) -> bool {
    if name.is_empty() || name == b"@" || name.ends_with(b".") || name.ends_with(b"/") {
        return false;
    }
    if name
        .windows(2)
        .any(|w| w == b".." || w == b"@{" || w == b"//")
    {
        return false;
    }
    if name.iter().any(|&b| {
        b < 0x20 || b == 0x7f || matches!(b, b' ' | b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
    }) {
        return false;
    }
    let mut components = 0;
    for c in name.split(|&b| b == b'/') {
        components += 1;
        if c.is_empty() || c.starts_with(b".") || c.ends_with(b".lock") {
            return false;
        }
    }
    components >= 2
}

/// Check an identity line. `line` starts right after `author ` (or
/// `committer `, `tagger `) and runs to the end of the object; a LF is
/// known to follow.
fn check_ident(line: &[u8]) -> Option<Finding> {
    let at = |i: usize| line.get(i).copied().unwrap_or(b'\n');
    let find_any = |from: usize, set: &[u8]| {
        line[from.min(line.len())..]
            .iter()
            .position(|b| set.contains(b))
            .map_or(line.len(), |p| from + p)
    };
    if at(0) == b'<' {
        return Some(Finding::new(
            MsgId::MissingNameBeforeEmail,
            msg::MISSING_NAME_BEFORE_EMAIL,
        ));
    }
    let lt = find_any(0, b"<>\n");
    match at(lt) {
        b'>' => return Some(Finding::new(MsgId::BadName, msg::BAD_NAME)),
        b'<' => {}
        _ => return Some(Finding::new(MsgId::MissingEmail, msg::MISSING_EMAIL)),
    }
    if lt == 0 || line[lt - 1] != b' ' {
        return Some(Finding::new(
            MsgId::MissingSpaceBeforeEmail,
            msg::MISSING_SPACE_BEFORE_EMAIL,
        ));
    }
    let gt = find_any(lt + 1, b"<>\n");
    if at(gt) != b'>' {
        return Some(Finding::new(MsgId::BadEmail, msg::BAD_EMAIL));
    }
    if at(gt + 1) != b' ' {
        return Some(Finding::new(
            MsgId::MissingSpaceBeforeDate,
            msg::MISSING_SPACE_BEFORE_DATE,
        ));
    }
    let p = gt + 2;
    if at(p) == b'0' && at(p + 1) != b' ' {
        return Some(Finding::new(MsgId::ZeroPaddedDate, msg::ZERO_PADDED_DATE));
    }
    // The date may be preceded by more white space; then decimal digits
    // (no sign).
    let mut i = p;
    while matches!(at(i), b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r') && i < line.len() {
        i += 1;
    }
    let digits_start = i;
    let mut value: u64 = 0;
    let mut overflow = false;
    while i < line.len() && line[i].is_ascii_digit() {
        match value
            .checked_mul(10)
            .and_then(|v| v.checked_add(u64::from(line[i] - b'0')))
        {
            Some(v) => value = v,
            None => overflow = true,
        }
        i += 1;
    }
    let end = if i == digits_start { p } else { i };
    if i != digits_start && (overflow || value > i64::MAX as u64) {
        return Some(Finding::new(MsgId::BadDateOverflow, msg::BAD_DATE_OVERFLOW));
    }
    if end == p || at(end) != b' ' {
        return Some(Finding::new(MsgId::BadDate, msg::BAD_DATE));
    }
    let tz = end + 1;
    if !matches!(at(tz), b'+' | b'-')
        || !(1..=4).all(|k| at(tz + k).is_ascii_digit())
        || at(tz + 5) != b'\n'
    {
        return Some(Finding::new(MsgId::BadTimezone, msg::BAD_TIMEZONE));
    }
    None
}

/// Check the tree content `data`.
pub fn check_tree(hash: HashKind, data: &[u8], options: &Options, report: &mut Report<'_>) {
    let mut entries = Vec::new();
    for e in TreeIter::new(hash, data) {
        match e {
            Ok(e) => entries.push(e),
            Err(err) => {
                report(Finding {
                    id: MsgId::BadTree,
                    detail: msg::BAD_TREE.into(),
                    cause: Some(err),
                });
                return;
            }
        }
    }

    let mut has_null = false;
    let mut has_full_path = false;
    let mut has_dot = false;
    let mut has_dotdot = false;
    let mut has_dotgit = false;
    let mut has_large = false;
    let mut has_zero_pad = false;
    let mut has_bad_mode = false;
    let mut has_dups = false;
    let mut not_sorted = false;

    for (i, e) in entries.iter().enumerate() {
        let name = e.name;
        let is_link = e.mode_bits & 0o170000 == 0o120000;
        if let Some(special) = special_file(name).filter(|_| is_link) {
            let f = match special {
                SpecialFile::Gitmodules => {
                    Finding::new(MsgId::GitmodulesSymlink, msg::GITMODULES_SYMLINK)
                }
                SpecialFile::Gitattributes => {
                    Finding::new(MsgId::GitattributesSymlink, msg::GITATTRIBUTES_SYMLINK)
                }
                SpecialFile::Gitignore => {
                    Finding::new(MsgId::GitignoreSymlink, msg::GITIGNORE_SYMLINK)
                }
                SpecialFile::Mailmap => Finding::new(MsgId::MailmapSymlink, msg::MAILMAP_SYMLINK),
            };
            if stop(report, f) {
                return;
            }
        }
        has_null |= e.oid.is_null();
        has_full_path |= name.contains(&b'/');
        has_dot |= name == b".";
        has_dotdot |= name == b"..";
        has_dotgit |= is_dotgit(name);
        has_large |= name.len() > options.max_pathname_len;
        has_zero_pad |= e.mode_digits.first() == Some(&b'0');
        has_bad_mode |= !matches!(
            e.mode_bits,
            0o100644 | 0o100755 | 0o120000 | 0o040000 | 0o160000
        );
        if i > 0 {
            let prev = &entries[i - 1];
            if prev.name == name {
                has_dups = true;
            } else if entry_cmp(prev.name, prev.is_tree(), name, e.is_tree()).is_gt() {
                not_sorted = true;
            }
        }
        // A file `x` and a directory `x` are not adjacent when names like
        // `x.c` sort between them (`x` < `x.c` < `x/`); look back for it.
        if e.is_tree() {
            for prev in entries[..i].iter().rev() {
                if !prev.name.starts_with(name) {
                    break;
                }
                if prev.name == name {
                    has_dups = true;
                    break;
                }
            }
        }
    }

    let flags = [
        (has_null, MsgId::NullSha1, msg::NULL_SHA1),
        (has_full_path, MsgId::FullPathname, msg::FULL_PATHNAME),
        (has_dot, MsgId::HasDot, msg::HAS_DOT),
        (has_dotdot, MsgId::HasDotdot, msg::HAS_DOTDOT),
        (has_dotgit, MsgId::HasDotgit, msg::HAS_DOTGIT),
        (has_large, MsgId::LargePathname, msg::LARGE_PATHNAME),
        (
            has_zero_pad,
            MsgId::ZeroPaddedFilemode,
            msg::ZERO_PADDED_FILEMODE,
        ),
        (has_bad_mode, MsgId::BadFilemode, msg::BAD_FILEMODE),
        (has_dups, MsgId::DuplicateEntries, msg::DUPLICATE_ENTRIES),
        (not_sorted, MsgId::TreeNotSorted, msg::TREE_NOT_SORTED),
    ];
    for (set, id, detail) in flags {
        if set && stop(report, Finding::new(id, detail)) {
            return;
        }
    }
}

/// Files whose tree entries get extra checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SpecialFile {
    /// `.gitmodules`: must not be a symlink; its blob content is checked
    /// (`gitmodules*` ids, done by the caller with access to the blob).
    Gitmodules,
    /// `.gitattributes`.
    Gitattributes,
    /// `.gitignore`.
    Gitignore,
    /// `.mailmap`.
    Mailmap,
}

/// Whether a tree entry name refers to one of the [`SpecialFile`]s on any
/// file system git protects against: exact or ASCII-case-insensitive match,
/// with HFS+ ignorable code points removed, or in NTFS spellings (trailing
/// dots and spaces, alternate data streams, 8.3 short names).
pub fn special_file(name: &[u8]) -> Option<SpecialFile> {
    const TABLE: [(SpecialFile, &str, &str); 4] = [
        (SpecialFile::Gitmodules, "gitmodules", "gi7eba"),
        (SpecialFile::Gitattributes, "gitattributes", "gi7d29"),
        (SpecialFile::Gitignore, "gitignore", "gi250a"),
        (SpecialFile::Mailmap, "mailmap", "maba30"),
    ];
    TABLE.iter().find_map(|&(f, base, short_hash)| {
        (is_hfs_dot(name, base) || is_ntfs_dot(name, base, short_hash)).then_some(f)
    })
}

/// Whether a tree entry name would be `.git` on some file system.
pub fn is_dotgit(name: &[u8]) -> bool {
    if name.eq_ignore_ascii_case(b".git") || is_hfs_dot(name, "git") {
        return true;
    }
    // Backslash separates path components on Windows.
    name.split(|&b| b == b'\\').any(|c| {
        let tail = if c.len() >= 4 && c[0] == b'.' && c[1..4].eq_ignore_ascii_case(b"git") {
            &c[4..]
        } else if c.len() >= 5 && c[..5].eq_ignore_ascii_case(b"git~1") {
            &c[5..]
        } else {
            return false;
        };
        ntfs_tail_is_empty(tail)
    })
}

/// NTFS ignores trailing dots and spaces, and `:` starts a stream name.
fn ntfs_tail_is_empty(tail: &[u8]) -> bool {
    for &b in tail {
        match b {
            b'.' | b' ' => {}
            b':' => return true,
            _ => return false,
        }
    }
    true
}

/// Whether `name` equals `.` + `base` after dropping the code points HFS+
/// ignores, comparing ASCII case-insensitively.
fn is_hfs_dot(name: &[u8], base: &str) -> bool {
    let Ok(s) = std::str::from_utf8(name) else {
        return false;
    };
    let mut chars = s.chars().filter(|&c| !is_hfs_ignorable(c));
    if chars.next() != Some('.') {
        return false;
    }
    let mut want = base.chars();
    loop {
        match (chars.next(), want.next()) {
            (None, None) => return true,
            (Some(a), Some(b)) if a.eq_ignore_ascii_case(&b) => {}
            _ => return false,
        }
    }
}

/// Code points that HFS+ drops when comparing file names.
fn is_hfs_ignorable(c: char) -> bool {
    matches!(
        c,
        '\u{200c}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{206a}'..='\u{206f}' | '\u{feff}'
    )
}

/// Whether `name` is `.` + `base` on NTFS: case-insensitive, with trailing
/// dots/spaces or a stream suffix, or one of the 8.3 short names (the
/// first six letters of `base` or the hashed `short_hash`, then `~` and a
/// digit).
fn is_ntfs_dot(name: &[u8], base: &str, short_hash: &str) -> bool {
    let base = base.as_bytes();
    if name.first() == Some(&b'.')
        && name.len() > base.len()
        && name[1..=base.len()].eq_ignore_ascii_case(base)
    {
        return ntfs_tail_is_empty(&name[base.len() + 1..]);
    }
    if name.len() >= 8 && name[6] == b'~' {
        let digit = name[7];
        let tail = &name[8..];
        let six = &name[..6];
        if six.eq_ignore_ascii_case(&base[..6]) && (b'1'..=b'4').contains(&digit) {
            return ntfs_tail_is_empty(tail);
        }
        if six.eq_ignore_ascii_case(short_hash.as_bytes()) && (b'1'..=b'9').contains(&digit) {
            return ntfs_tail_is_empty(tail);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(kind: ObjectKind, data: &[u8]) -> Vec<MsgId> {
        findings(kind, HashKind::Sha1, data)
            .into_iter()
            .map(|f| f.id)
            .collect()
    }

    #[test]
    fn idents() {
        let c = |a: &str| {
            format!(
                "tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nauthor {a}\ncommitter C <c> 1 +0000\n\nm\n"
            )
        };
        assert!(ids(ObjectKind::Commit, c("A <a> 1 +0000").as_bytes()).is_empty());
        for (a, want) in [
            ("<a> 1 +0000", MsgId::MissingNameBeforeEmail),
            ("A a> 1 +0000", MsgId::BadName),
            ("", MsgId::MissingEmail),
            ("A<a> 1 +0000", MsgId::MissingSpaceBeforeEmail),
            ("A <a 1 +0000", MsgId::BadEmail),
            ("A <a>1 +0000", MsgId::MissingSpaceBeforeDate),
            ("A <a> 01 +0000", MsgId::ZeroPaddedDate),
            ("A <a> 9223372036854775808 +0000", MsgId::BadDateOverflow),
            ("A <a> x +0000", MsgId::BadDate),
            ("A <a> 1", MsgId::BadDate),
            ("A <a> 1 +000", MsgId::BadTimezone),
            ("A <a> 1 +0000 ", MsgId::BadTimezone),
        ] {
            assert_eq!(ids(ObjectKind::Commit, c(a).as_bytes()), vec![want], "{a}");
        }
    }

    #[test]
    fn names() {
        assert!(is_dotgit(b".GIT"));
        assert!(is_dotgit(b"git~1"));
        assert!(is_dotgit(b".git. ."));
        assert!(is_dotgit(".g\u{200c}it".as_bytes()));
        assert!(!is_dotgit(b"..git"));
        assert!(!is_dotgit(b"git~2"));
        assert_eq!(special_file(b"GITMOD~1"), Some(SpecialFile::Gitmodules));
        assert_eq!(special_file(b".gitmodules."), Some(SpecialFile::Gitmodules));
        assert_eq!(special_file(b".mailmap"), Some(SpecialFile::Mailmap));
        assert_eq!(special_file(b"gitmodules"), None);
        assert!(is_valid_tag_name(b"v1.0"));
        assert!(!is_valid_tag_name(b"v 1"));
        assert!(!is_valid_tag_name(b""));
    }
}
