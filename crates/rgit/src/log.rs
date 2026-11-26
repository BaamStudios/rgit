// SPDX-License-Identifier: MIT
//! History: [`Repository::log`] walks commits like `git rev-list` and
//! hands out each one parsed.

use bstr::{BStr, BString, ByteSlice};
use rgit_hash::ObjectId;
use rgit_object::ObjectKind;
use rgit_revwalk::revision::{Flags, Resolver};
use rgit_revwalk::walk::{self, CommitSource, Walk, WalkOptions};

use crate::{Cancel, Error, Repository, Result, messages};

/// The order commits come out of a [`Log`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Order {
    /// Newest first by committer date (`git log` without options).
    #[default]
    CommitDate,
    /// No parent before all of its children, otherwise by committer date
    /// (`--date-order`).
    DateTopological,
    /// No parent before all of its children, lines of history kept
    /// together (`--topo-order`).
    Topological,
}

/// Which commits [`Repository::log`] visits.
///
/// ```
/// let options = rgit::LogOptions::new()
///     .rev("main")
///     .rev("^v1.0")
///     .first_parent(true)
///     .max_count(20);
/// # let _ = options;
/// ```
#[derive(Debug, Clone, Default)]
pub struct LogOptions {
    revs: Vec<BString>,
    order: Order,
    reverse: bool,
    first_parent: bool,
    max_count: Option<usize>,
    skip: usize,
    cancel: Option<Cancel>,
}

impl LogOptions {
    /// The history of `HEAD`, newest first.
    #[must_use]
    pub fn new() -> LogOptions {
        LogOptions::default()
    }

    /// Add a revision argument as `git log` takes them: a revision to
    /// start from (`main`, `v1.0`, an object name), `^<rev>` to leave out
    /// a commit and its ancestors, or a range (`v1.0..main`, `a...b`).
    /// Without any, the walk starts at `HEAD`.
    #[must_use]
    pub fn rev(mut self, rev: impl AsRef<[u8]>) -> LogOptions {
        self.revs.push(rev.as_ref().into());
        self
    }

    /// The order of the result.
    #[must_use]
    pub fn order(mut self, order: Order) -> LogOptions {
        self.order = order;
        self
    }

    /// Oldest first instead of newest first (applied after
    /// [`LogOptions::max_count`], like `git log --reverse`).
    #[must_use]
    pub fn reverse(mut self, reverse: bool) -> LogOptions {
        self.reverse = reverse;
        self
    }

    /// Follow only the first parent of merges (`--first-parent`).
    #[must_use]
    pub fn first_parent(mut self, first_parent: bool) -> LogOptions {
        self.first_parent = first_parent;
        self
    }

    /// Stop after `n` commits (`-n <n>`).
    #[must_use]
    pub fn max_count(mut self, n: usize) -> LogOptions {
        self.max_count = Some(n);
        self
    }

    /// Leave out the first `n` commits (`--skip=<n>`).
    #[must_use]
    pub fn skip(mut self, n: usize) -> LogOptions {
        self.skip = n;
        self
    }

    /// Stop with [`Error::Cancelled`] once `cancel` is cancelled.
    #[must_use]
    pub fn cancel(mut self, cancel: Cancel) -> LogOptions {
        self.cancel = Some(cancel);
        self
    }
}

/// When and in which time zone something happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Time {
    /// Seconds since the Unix epoch.
    pub seconds: i64,
    /// Offset of the author's time zone from UTC in minutes, east
    /// positive.
    pub offset_minutes: i32,
}

/// The author or the committer of a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    /// The name. A byte string: old commits are not always UTF-8 (see
    /// [`Commit::encoding`]).
    pub name: BString,
    /// The e-mail address, without the angle brackets.
    pub email: BString,
    /// The time; `None` if the commit has no well-formed date.
    pub time: Option<Time>,
}

impl Signature {
    fn parse(raw: Option<rgit_object::SignatureRef<'_>>) -> Signature {
        match raw {
            Some(s) => Signature {
                name: s.name.into(),
                email: s.email.into(),
                time: s.time().map(|t| Time {
                    seconds: t.seconds,
                    offset_minutes: t.offset_minutes,
                }),
            },
            None => Signature {
                name: BString::default(),
                email: BString::default(),
                time: None,
            },
        }
    }
}

/// A commit as [`Log`] hands it out.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Commit {
    /// The name of the commit.
    pub id: ObjectId,
    /// The tree of the commit.
    pub tree: ObjectId,
    /// The parents as recorded in the commit, first parent first.
    pub parents: Vec<ObjectId>,
    /// Who wrote the change.
    pub author: Signature,
    /// Who made the commit.
    pub committer: Signature,
    /// The `encoding` header: the character encoding of the message and
    /// the names when it is not UTF-8.
    pub encoding: Option<BString>,
    /// The message as stored, bytes after the header block.
    pub message: BString,
}

impl Commit {
    /// The first line of the message.
    #[must_use]
    pub fn summary(&self) -> &BStr {
        self.message.lines().next().unwrap_or_default().as_bstr()
    }
}

/// Reads commits for the walk.
struct Source<'r> {
    repo: &'r Repository,
    cancel: Option<Cancel>,
}

impl CommitSource for Source<'_> {
    type Error = Error;

    fn load(&mut self, id: &ObjectId) -> Result<walk::CommitInfo> {
        if let Some(cancel) = &self.cancel {
            cancel.check()?;
        }
        let commit = self.repo.parse_commit(id)?;
        let parents = if self.repo.shallow().contains(id) {
            Vec::new()
        } else {
            commit.parents
        };
        Ok(walk::CommitInfo {
            parents,
            time: commit.committer.time.map_or(0, |t| t.seconds),
        })
    }
}

/// The commits of a [`Repository::log`], in order. An iterator of
/// `Result<Commit>`: after an error (a missing or damaged commit, a
/// cancelled walk) it ends.
pub struct Log<'r> {
    walk: Walk<Source<'r>>,
    repo: &'r Repository,
    failed: bool,
}

impl std::fmt::Debug for Log<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Log").finish_non_exhaustive()
    }
}

impl Iterator for Log<'_> {
    type Item = Result<Commit>;

    fn next(&mut self) -> Option<Result<Commit>> {
        if self.failed {
            return None;
        }
        let next = match self.walk.next_commit() {
            Ok(Some(id)) => self.repo.parse_commit(&id),
            Ok(None) => return None,
            Err(e) => Err(e),
        };
        self.failed = next.is_err();
        Some(next)
    }
}

impl Repository {
    /// Commit `id`, parsed.
    pub(crate) fn parse_commit(&self, id: &ObjectId) -> Result<Commit> {
        let data = self.read_kind(id, ObjectKind::Commit)?;
        let bad = |_| Error::corrupt_message(messages::bad_commit(id));
        let commit = rgit_object::Commit::parse(self.object_format(), &data).map_err(bad)?;
        Ok(Commit {
            id: *id,
            tree: commit.tree().map_err(bad)?,
            parents: commit.parents().map_err(bad)?,
            author: Signature::parse(commit.author()),
            committer: Signature::parse(commit.committer()),
            encoding: commit.encoding().map(BString::from),
            message: commit.message().into(),
        })
    }

    /// Walk the history as `git log` does: from the revisions of
    /// `options` (or `HEAD`) back through the parents.
    ///
    /// The walk is lazy where the order allows it: with the default order
    /// and no excluded revisions, taking the first ten commits reads
    /// about ten commits.
    ///
    /// ```no_run
    /// # fn main() -> rgit::Result<()> {
    /// let repo = rgit::Repository::open(".")?;
    /// for commit in repo.log(rgit::LogOptions::new().rev("v1.0..HEAD").max_count(10))? {
    ///     let commit = commit?;
    ///     println!("{:.7} {} <{}>", commit.id, commit.summary(), commit.author.email);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    /// [`Error::RevisionNotFound`] or [`Error::Revision`] for a revision
    /// that does not name a commit; reading commits can fail later, see
    /// [`Log`].
    pub fn log(&self, options: LogOptions) -> Result<Log<'_>> {
        let walk_options = WalkOptions {
            order: match options.order {
                Order::CommitDate => walk::Order::Default,
                Order::DateTopological => walk::Order::Date,
                Order::Topological => walk::Order::Topo,
            },
            reverse: options.reverse,
            first_parent: options.first_parent,
            max_count: options.max_count,
            skip: options.skip,
            ..WalkOptions::default()
        };
        let mut walk = Walk::new(
            Source {
                repo: self,
                cancel: options.cancel,
            },
            walk_options,
        );
        let revs = if options.revs.is_empty() {
            vec![BString::from("HEAD")]
        } else {
            options.revs
        };
        for rev in &revs {
            for (id, exclude) in self.rev_argument(rev.as_bstr())? {
                walk.push(&id, exclude)?;
            }
        }
        Ok(Log {
            walk,
            repo: self,
            failed: false,
        })
    }

    /// The commits a revision argument of `git log` stands for, each with
    /// whether it is excluded: `<rev>`, `^<rev>`, `A..B`, `A...B`.
    fn rev_argument(&self, rev: &BStr) -> Result<Vec<(ObjectId, bool)>> {
        let Ok(text) = rev.to_str() else {
            return Err(Error::RevisionNotFound { spec: rev.into() });
        };
        self.with_resolver(|resolver| {
            let fail = |e| Repository::revision_error(resolver, text, e);
            let commit =
                |resolver: &Resolver<'_>, id: &ObjectId| resolver.peel_to_commit(id).map_err(fail);
            if let Some(items) = resolver.try_range(text, Flags::default()).map_err(fail)? {
                return items
                    .iter()
                    .map(|item| Ok((commit(resolver, &item.oid)?, item.negated)))
                    .collect();
            }
            let (name, exclude) = match text.strip_prefix('^') {
                Some(rest) => (rest, true),
                None => (text, false),
            };
            let id = resolver.resolve(name).map_err(fail)?;
            Ok(vec![(commit(resolver, &id)?, exclude)])
        })
    }
}
