// SPDX-License-Identifier: MIT

//! User-visible message texts reproduced from git for compatibility.
//!
//! Collected here so they are reviewable in one place (see `CLEANROOM.md`).
//! They were taken from the observable output of the reference `git` binary
//! (`git mktag`, `git hash-object`, `git cat-file`).

/// Tree parse errors (printed as `error: <text>` / `fatal: <text>`).
pub mod tree {
    /// A tree entry runs past the end of the object.
    pub const TOO_SHORT: &str = "too-short tree object";
    /// The mode of a tree entry is not an octal number followed by a space.
    pub const MALFORMED_MODE: &str = "malformed mode in tree entry";
    /// A tree entry has an empty file name.
    pub const EMPTY_FILENAME: &str = "empty filename in tree entry";
}

/// Detail texts of fsck findings, printed as `<msgId>: <text>`.
pub mod fsck {
    pub const UNTERMINATED_HEADER: &str = "unterminated header";
    /// Followed by the byte offset of the NUL.
    pub const NUL_IN_HEADER: &str = "unterminated header: NUL at offset ";
    pub const NUL_IN_COMMIT: &str = "NUL byte in the commit object body";

    pub const MISSING_TREE: &str = "invalid format - expected 'tree' line";
    pub const BAD_TREE_SHA1: &str = "invalid 'tree' line format - bad sha1";
    pub const BAD_PARENT_SHA1: &str = "invalid 'parent' line format - bad sha1";
    pub const MISSING_AUTHOR: &str = "invalid format - expected 'author' line";
    pub const MULTIPLE_AUTHORS: &str = "invalid format - multiple 'author' lines";
    pub const MISSING_COMMITTER: &str = "invalid format - expected 'committer' line";

    pub const MISSING_OBJECT: &str = "invalid format - expected 'object' line";
    pub const BAD_OBJECT_SHA1: &str = "invalid 'object' line format - bad sha1";
    pub const MISSING_TYPE_ENTRY: &str = "invalid format - expected 'type' line";
    pub const BAD_TYPE: &str = "invalid 'type' value";
    pub const MISSING_TAG_ENTRY: &str = "invalid format - expected 'tag' line";
    /// Followed by the tag name.
    pub const BAD_TAG_NAME: &str = "invalid 'tag' name: ";
    pub const MISSING_TAGGER_ENTRY: &str = "invalid format - expected 'tagger' line";
    pub const EXTRA_HEADER_ENTRY: &str = "invalid format - extra header(s) after 'tagger'";

    pub const MISSING_NAME_BEFORE_EMAIL: &str =
        "invalid author/committer line - missing space before email";
    pub const BAD_NAME: &str = "invalid author/committer line - bad name";
    pub const MISSING_EMAIL: &str = "invalid author/committer line - missing email";
    pub const MISSING_SPACE_BEFORE_EMAIL: &str =
        "invalid author/committer line - missing space before email";
    pub const BAD_EMAIL: &str = "invalid author/committer line - bad email";
    pub const MISSING_SPACE_BEFORE_DATE: &str =
        "invalid author/committer line - missing space before date";
    pub const ZERO_PADDED_DATE: &str = "invalid author/committer line - zero-padded date";
    pub const BAD_DATE_OVERFLOW: &str =
        "invalid author/committer line - date causes integer overflow";
    pub const BAD_DATE: &str = "invalid author/committer line - bad date";
    pub const BAD_TIMEZONE: &str = "invalid author/committer line - bad time zone";

    pub const BAD_TREE: &str = "cannot be parsed as a tree";
    pub const TREE_NOT_SORTED: &str = "not properly sorted";
    pub const DUPLICATE_ENTRIES: &str = "contains duplicate file entries";
    pub const BAD_FILEMODE: &str = "contains bad file modes";
    pub const ZERO_PADDED_FILEMODE: &str = "contains zero-padded file modes";
    pub const HAS_DOT: &str = "contains '.'";
    pub const HAS_DOTDOT: &str = "contains '..'";
    pub const HAS_DOTGIT: &str = "contains '.git'";
    pub const FULL_PATHNAME: &str = "contains full pathnames";
    pub const NULL_SHA1: &str = "contains entries pointing to null sha1";
    pub const LARGE_PATHNAME: &str = "contains excessively large pathname";
    pub const GITMODULES_SYMLINK: &str = ".gitmodules is a symbolic link";
    pub const GITATTRIBUTES_SYMLINK: &str = ".gitattributes is a symlink";
    pub const GITIGNORE_SYMLINK: &str = ".gitignore is a symlink";
    pub const MAILMAP_SYMLINK: &str = ".mailmap is a symlink";
}
