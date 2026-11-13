// SPDX-License-Identifier: MIT

//! Errors. The `Display` text of each variant is git's message without the
//! `fatal: ` prefix; the CLI prints it as `fatal: <text>` and exits with 128.

use std::borrow::Cow;

use crate::messages;

fn lossy(b: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(b)
}

/// Errors from parsing pathspecs, global pathspec settings and pathspec files.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathspecError {
    /// `""` was given as a pathspec.
    #[error("{}", messages::EMPTY_STRING)]
    EmptyString,
    /// Unknown word in `:(...)`.
    #[error("Invalid pathspec magic '{}' in '{}'", lossy(.word), lossy(.arg))]
    InvalidMagic {
        /// The offending word.
        word: Vec<u8>,
        /// The whole argument.
        arg: Vec<u8>,
    },
    /// `:(` without a closing `)`.
    #[error("Missing ')' at the end of pathspec magic in '{}'", lossy(.arg))]
    MissingParen {
        /// The whole argument.
        arg: Vec<u8>,
    },
    /// A reserved short magic character that has no meaning yet.
    #[error("Unimplemented pathspec magic '{}' in '{}'", char::from(*.ch), lossy(.arg))]
    UnimplementedMagic {
        /// The character.
        ch: u8,
        /// The whole argument.
        arg: Vec<u8>,
    },
    /// `literal` and `glob` on the same pathspec.
    #[error("{}: 'literal' and 'glob' are incompatible", lossy(.arg))]
    LiteralAndGlob {
        /// The whole argument.
        arg: Vec<u8>,
    },
    /// `prefix:<n>` with a malformed number.
    #[error("invalid parameter for pathspec magic 'prefix'")]
    InvalidPrefixParameter,
    /// `prefix:<n>` longer than the pattern (an internal error in git).
    #[error("{}: 'prefix' magic length {len} exceeds the pattern", lossy(.arg))]
    PrefixTooLong {
        /// The whole argument.
        arg: Vec<u8>,
        /// The given length.
        len: usize,
    },
    /// More than one `attr:` word.
    #[error("Only one 'attr:' specification is allowed.")]
    MultipleAttr,
    /// `attr:` with no requirements.
    #[error("attr spec must not be empty")]
    EmptyAttr,
    /// A malformed attribute name in `attr:`.
    #[error("invalid attribute name {}", lossy(.name))]
    InvalidAttrName {
        /// The name as written.
        name: Vec<u8>,
    },
    /// A value in `attr:NAME=VALUE` contains a character that is not allowed.
    #[error("cannot use '{}' for value matching", char::from(*.ch))]
    InvalidAttrValue {
        /// The character.
        ch: u8,
    },
    /// A value in `attr:NAME=VALUE` ends with an escaping backslash.
    #[error("Escape character '\\' not allowed as last character in attr value")]
    AttrValueEscapeAtEnd,
    /// The pathspec names a path outside the work tree.
    #[error("{}: '{}' is outside repository at '{}'", lossy(.arg), lossy(.path), lossy(.root))]
    OutsideRepository {
        /// The whole argument.
        arg: Vec<u8>,
        /// The path part (after magic).
        path: Vec<u8>,
        /// The work tree (or repository) root as given to the parser.
        root: Vec<u8>,
    },
    /// The directory part of an absolute pathspec cannot be resolved.
    #[error("Invalid path '{}': {reason}", lossy(.path))]
    InvalidPath {
        /// The directory that failed.
        path: Vec<u8>,
        /// The operating system's reason.
        reason: String,
    },
    /// The command does not support some magic used by this pathspec.
    #[error("{}: pathspec magic not supported by this command: {}", lossy(.arg), .list)]
    UnsupportedMagic {
        /// The whole argument.
        arg: Vec<u8>,
        /// Already formatted list, e.g. `'glob', 'exclude' (mnemonic: '!')`.
        list: String,
    },
    /// `GIT_GLOB_PATHSPECS` together with `GIT_NOGLOB_PATHSPECS`.
    #[error("global 'glob' and 'noglob' pathspec settings are incompatible")]
    GlobalGlobAndNoglob,
    /// `GIT_LITERAL_PATHSPECS` together with another global setting.
    #[error(
        "global 'literal' pathspec setting is incompatible with all other global pathspec settings"
    )]
    GlobalLiteralAndOther,
    /// An environment variable that is not a boolean.
    #[error("bad boolean environment value '{}' for '{name}'", lossy(.value))]
    BadBoolEnv {
        /// The variable name.
        name: &'static str,
        /// Its value.
        value: Vec<u8>,
    },
    /// A line of a `--pathspec-from-file` input with broken C quoting.
    #[error("line is badly quoted: {}", lossy(.line))]
    BadlyQuoted {
        /// The line as read.
        line: Vec<u8>,
    },
}
