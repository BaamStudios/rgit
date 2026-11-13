// SPDX-License-Identifier: MIT
//! The revision syntax of `gitrevisions(7)` (Documentation/revisions.adoc).
//!
//! [`Resolver`] turns a revision expression into an object id:
//!
//! * object names: full and abbreviated hex ids (ambiguity is reported with
//!   git's candidate list and resolved by type context and
//!   `core.disambiguate`), `git describe` output;
//! * ref names with the lookup order `<name>`, `refs/<name>`,
//!   `refs/tags/<name>`, `refs/heads/<name>`, `refs/remotes/<name>`,
//!   `refs/remotes/<name>/HEAD`, and `@` for `HEAD`;
//! * `<ref>@{<n>}`, `<ref>@{<date>}`, `@{-<n>}`, `<branch>@{upstream}`,
//!   `<branch>@{push}`;
//! * suffixes `^`, `^<n>`, `~`, `~<n>`, `^{<type>}`, `^{}`, `^{/<regex>}`;
//! * `:/<regex>`, `<rev>:<path>`, `:<path>`, `:<n>:<path>`.
//!
//! [`Resolver::rev_arg`] adds the range forms used on command lines:
//! `^<rev>`, `A..B`, `A...B`, `<rev>^@`, `<rev>^!`, `<rev>^-<n>`.
//!
//! The repository is reached through the small traits in [`lookup`]; the
//! messages a command prints along the way are collected as
//! [`Diagnostic`]s.

mod abbrev;
mod graph;
pub mod lookup;
pub mod messages;
mod parse;
mod range;
mod reach;
mod refname;
mod upstream;

use std::cell::RefCell;

use rgit_date::DateContext;
use rgit_hash::ObjectId;

pub use lookup::{
    ConfigLookup, IndexLookup, LookupError, NoConfig, ObjectLookup, RefLookup, RefValue,
    ReflogEntry, WorkTreeLookup,
};
pub use range::RevItem;
pub use refname::{DwimRef, REF_RULES};
pub use upstream::PushDefault;

/// Why a revision did not resolve.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The name does not name an object (git fails silently and the caller
    /// prints "unknown revision" or "Needed a single revision").
    #[error("unknown revision")]
    NotFound,
    /// A problem git reports with `die()` while resolving (unless the lookup
    /// is quiet): reflog too short, no upstream, path outside the
    /// repository, ...
    #[error("{0}")]
    Fatal(String),
    /// The repository could not be read.
    #[error(transparent)]
    Lookup(#[from] LookupError),
}

/// The kind of object a context expects; used to pick among objects that
/// share an abbreviated id (`core.disambiguate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Hint {
    /// No preference.
    #[default]
    None,
    /// A commit.
    Commit,
    /// A commit or a tag that peels to one.
    Committish,
    /// A tree.
    Tree,
    /// A tree, a commit, or a tag that peels to one of them.
    Treeish,
    /// A blob.
    Blob,
}

impl Hint {
    /// Parse a `core.disambiguate` value (`none`, `commit`, `committish`,
    /// `tree`, `treeish`, `blob`).
    pub fn from_config(value: &str) -> Option<Hint> {
        Some(match value.to_ascii_lowercase().as_str() {
            "none" => Hint::None,
            "commit" => Hint::Commit,
            "committish" => Hint::Committish,
            "tree" => Hint::Tree,
            "treeish" => Hint::Treeish,
            "blob" => Hint::Blob,
            _ => return None,
        })
    }
}

/// Severity of a [`Diagnostic`] (the prefix git prints).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// `warning: `
    Warning,
    /// `error: `
    Error,
    /// `hint: `
    Hint,
}

impl Level {
    /// The prefix including the colon and space.
    pub fn prefix(self) -> &'static str {
        match self {
            Level::Warning => "warning: ",
            Level::Error => "error: ",
            Level::Hint => "hint: ",
        }
    }
}

/// A message produced while resolving, to be printed to standard error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Severity.
    pub level: Level,
    /// Text without prefix; may contain newlines (only the first line gets
    /// the prefix, except for hints where every line does).
    pub text: String,
}

impl Diagnostic {
    /// Render as git prints it, with a trailing newline.
    pub fn render(&self) -> String {
        match self.level {
            Level::Hint => self
                .text
                .split('\n')
                .map(|l| {
                    if l.is_empty() {
                        "hint:\n".to_string()
                    } else {
                        format!("hint: {l}\n")
                    }
                })
                .collect(),
            l => format!("{}{}\n", l.prefix(), self.text),
        }
    }
}

/// Settings that change how names resolve.
#[derive(Debug, Clone)]
pub struct Options {
    /// `core.warnAmbiguousRefs` (default true).
    pub warn_ambiguous_refs: bool,
    /// `advice.objectNameWarning` (default true).
    pub advice_object_name_warning: bool,
    /// `core.disambiguate` (default none).
    pub disambiguate: Hint,
    /// `core.abbrev`: `None` for `auto`, otherwise the length (already
    /// clamped to 4..=hex length).
    pub abbrev: Option<usize>,
    /// `push.default` (default simple).
    pub push_default: PushDefault,
    /// Current time and local zone, for `@{<date>}`.
    pub date: DateContext,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            warn_ambiguous_refs: true,
            advice_object_name_warning: true,
            disambiguate: Hint::None,
            abbrev: None,
            push_default: PushDefault::Simple,
            date: DateContext::from_env(),
        }
    }
}

/// Where the command runs relative to the work tree.
#[derive(Debug, Clone, Default)]
pub struct WorkTreeInfo {
    /// Absolute path of the top of the work tree (for messages).
    pub path: String,
    /// The current directory relative to the top, with a trailing slash
    /// (`""` at the top).
    pub prefix: String,
    /// Whether the current directory is inside the work tree.
    pub cwd_inside: bool,
}

/// The repository as seen by the resolver.
pub struct Repo<'a> {
    /// Objects.
    pub objects: &'a dyn ObjectLookup,
    /// References and reflogs.
    pub refs: &'a dyn RefLookup,
    /// Configuration (branch upstreams, remotes, push settings).
    pub config: &'a dyn ConfigLookup,
    /// The index, if there is one.
    pub index: Option<&'a dyn IndexLookup>,
    /// The work tree, if there is one.
    pub work_tree: Option<&'a dyn WorkTreeLookup>,
    /// Location of the current directory in the work tree.
    pub work_tree_info: Option<WorkTreeInfo>,
}

/// How one lookup behaves.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags {
    /// Expected object type (from the syntax around the name).
    pub hint: Hint,
    /// Do not produce error messages (`rev-parse -q`).
    pub quiet: bool,
    /// Diagnosis mode: the caller knows the name failed and wants the most
    /// specific reason (git's "only to die" lookup).
    pub diagnose: bool,
}

impl Flags {
    /// Default flags with `hint`.
    pub fn with_hint(self, hint: Hint) -> Flags {
        Flags { hint, ..self }
    }
}

/// Resolves revision expressions against a [`Repo`].
pub struct Resolver<'a> {
    repo: Repo<'a>,
    opts: Options,
    diags: RefCell<Vec<Diagnostic>>,
}

impl<'a> Resolver<'a> {
    /// A resolver for `repo` with `opts`.
    pub fn new(repo: Repo<'a>, opts: Options) -> Resolver<'a> {
        Resolver {
            repo,
            opts,
            diags: RefCell::new(Vec::new()),
        }
    }

    /// The repository.
    pub fn repo(&self) -> &Repo<'a> {
        &self.repo
    }

    /// The options.
    pub fn options(&self) -> &Options {
        &self.opts
    }

    /// Take the messages produced so far.
    pub fn take_diagnostics(&self) -> Vec<Diagnostic> {
        std::mem::take(&mut *self.diags.borrow_mut())
    }

    fn push_diag(&self, level: Level, text: impl Into<String>) {
        self.diags.borrow_mut().push(Diagnostic {
            level,
            text: text.into(),
        });
    }

    /// Resolve `name` to an object id (git's `get_oid`).
    pub fn resolve(&self, name: &str) -> Result<ObjectId, Error> {
        self.resolve_with(name, Flags::default())
    }

    /// Resolve `name` with explicit [`Flags`].
    pub fn resolve_with(&self, name: &str, flags: Flags) -> Result<ObjectId, Error> {
        self.get_oid(name, flags)
    }

    /// After `name` failed to resolve, find the most specific reason (path
    /// not in the tree or index, invalid object name before `:`, ...).
    ///
    /// Returns the `fatal:` message, or `None` if there is nothing more
    /// specific than "unknown revision". Hints are added to the
    /// diagnostics.
    pub fn diagnose(&self, name: &str) -> Option<String> {
        let flags = Flags {
            diagnose: true,
            ..Flags::default()
        };
        match self.get_oid(name, flags) {
            Err(Error::Fatal(msg)) => Some(msg),
            Err(Error::Lookup(e)) => Some(e.message()),
            _ => None,
        }
    }
}
