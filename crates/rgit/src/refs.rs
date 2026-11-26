// SPDX-License-Identifier: MIT
//! References (ADR 0005, "Names are byte strings").
//!
//! This module is the only place of the crate that talks to `rgit-refs`.
//! Reference names are byte strings in this API. The reference backend
//! still takes `str` (#214); [`backend_name`] and [`api_name`] are the two
//! conversions that go away when it takes byte strings as well.

use bstr::{BStr, BString, ByteSlice};
use rgit_hash::ObjectId;
use rgit_refs::{IterItem, IterOptions, RefStore};

use crate::{Error, Repository, Result};

/// A name of this API as the backend takes it.
pub(crate) fn backend_name(name: &[u8]) -> Result<&str> {
    std::str::from_utf8(name).map_err(|_| Error::InvalidRefName { name: name.into() })
}

/// A name of the backend as this API gives it out.
fn api_name(name: &str) -> BString {
    BString::from(name)
}

fn refs_error(e: rgit_refs::Error) -> Error {
    Error::corrupt(e)
}

/// What a reference holds.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Target {
    /// An object.
    Object(ObjectId),
    /// The full name of another reference (a symbolic reference such as
    /// `HEAD`).
    Symbolic(BString),
}

/// A reference: its full name, what it holds and the object it leads to.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Reference {
    /// The full name (`refs/heads/main`, `HEAD`). A byte string: git does
    /// not require reference names to be UTF-8.
    pub name: BString,
    /// What the reference holds.
    pub target: Target,
    /// The object the reference leads to after following symbolic
    /// references; `None` when the chain ends at a reference that does not
    /// exist (an unborn branch).
    pub id: Option<ObjectId>,
}

impl Reference {
    /// The name without `refs/heads/`, `refs/tags/` or `refs/remotes/`
    /// (the full name for other references).
    #[must_use]
    pub fn shorthand(&self) -> &BStr {
        shorthand(self.name.as_bstr())
    }
}

fn shorthand(name: &BStr) -> &BStr {
    for prefix in ["refs/heads/", "refs/tags/", "refs/remotes/"] {
        if let Some(rest) = name.strip_prefix(prefix.as_bytes()) {
            return rest.as_bstr();
        }
    }
    name
}

/// Where `HEAD` points.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Head {
    /// On a branch.
    Branch {
        /// The full name of the branch (`refs/heads/main`).
        name: BString,
        /// Its commit; `None` for a branch without commits (a new
        /// repository, `git switch --orphan`).
        id: Option<ObjectId>,
    },
    /// Directly at a commit ("detached HEAD").
    Detached(ObjectId),
}

impl Head {
    /// The commit `HEAD` leads to, if there is one.
    #[must_use]
    pub fn id(&self) -> Option<ObjectId> {
        match self {
            Head::Branch { id, .. } => *id,
            Head::Detached(id) => Some(*id),
        }
    }

    /// The full name of the current branch; `None` when detached.
    #[must_use]
    pub fn branch(&self) -> Option<&BStr> {
        match self {
            Head::Branch { name, .. } => Some(name.as_bstr()),
            Head::Detached(_) => None,
        }
    }

    /// The short name of the current branch (`main`); `None` when
    /// detached.
    #[must_use]
    pub fn shorthand(&self) -> Option<&BStr> {
        self.branch().map(shorthand)
    }

    /// Whether `HEAD` is on a branch that has no commit yet.
    #[must_use]
    pub fn is_unborn(&self) -> bool {
        matches!(self, Head::Branch { id: None, .. })
    }

    /// Whether `HEAD` points at a commit instead of a branch.
    #[must_use]
    pub fn is_detached(&self) -> bool {
        matches!(self, Head::Detached(_))
    }
}

impl Repository {
    /// The commit checked out in the repository whose work tree is `dir`
    /// (a submodule): its `.git` directory or `gitdir:` file, then
    /// `HEAD`. `None` if there is no repository or no commit.
    pub(crate) fn checked_out_commit(&self, dir: &std::path::Path) -> Option<ObjectId> {
        let dot_git = dir.join(".git");
        let git_dir = if std::fs::metadata(&dot_git).ok()?.is_dir() {
            dot_git
        } else {
            let data = std::fs::read(&dot_git).ok()?;
            let target = data.strip_prefix(b"gitdir:")?.to_str().ok()?.trim();
            dir.join(target)
        };
        rgit_refs::files::FilesStore::new(git_dir, self.object_format())
            .resolve_oid("HEAD")
            .ok()
            .flatten()
    }

    /// Where `HEAD` points.
    ///
    /// ```no_run
    /// # fn main() -> rgit::Result<()> {
    /// let repo = rgit::Repository::open(".")?;
    /// match repo.head()? {
    ///     rgit::Head::Branch { name, id: Some(id) } => println!("{name} at {id}"),
    ///     rgit::Head::Branch { name, id: None } => println!("{name} has no commits yet"),
    ///     rgit::Head::Detached(id) => println!("detached at {id}"),
    ///     _ => {}
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    /// [`Error::Corrupt`] if `HEAD` is missing or unreadable.
    pub fn head(&self) -> Result<Head> {
        match self.refs.read_raw("HEAD").map_err(refs_error)? {
            Some(rgit_refs::Target::Object(id)) => Ok(Head::Detached(id)),
            Some(rgit_refs::Target::Symbolic(name)) => {
                let id = self.refs.resolve_oid(&name).map_err(refs_error)?;
                Ok(Head::Branch {
                    name: api_name(&name),
                    id,
                })
            }
            None => Err(Error::corrupt_message(
                rgit_repo::Error::NotAGitDir(self.git_dir().to_path_buf()).to_string(),
            )),
        }
    }

    /// The reference with the full name `name` (`refs/heads/main`,
    /// `HEAD`); `None` if it does not exist. Short names are not looked
    /// up here, see [`Repository::rev_parse`] for those.
    ///
    /// # Errors
    /// [`Error::InvalidRefName`] for a name the backend cannot take,
    /// [`Error::Corrupt`] for an unreadable reference.
    pub fn find_reference(&self, name: impl AsRef<[u8]>) -> Result<Option<Reference>> {
        let name = name.as_ref();
        let backend = backend_name(name)?;
        let Some(target) = self.refs.read_raw(backend).map_err(refs_error)? else {
            return Ok(None);
        };
        let (target, id) = match target {
            rgit_refs::Target::Object(id) => (Target::Object(id), Some(id)),
            rgit_refs::Target::Symbolic(to) => {
                let id = self.refs.resolve_oid(backend).map_err(refs_error)?;
                (Target::Symbolic(api_name(&to)), id)
            }
        };
        Ok(Some(Reference {
            name: name.into(),
            target,
            id,
        }))
    }

    /// The references whose full name starts with `prefix`
    /// (`refs/heads/`, `refs/tags/v1.`; `refs/` for all of them), sorted
    /// by name.
    ///
    /// ```no_run
    /// # fn main() -> rgit::Result<()> {
    /// let repo = rgit::Repository::open(".")?;
    /// for branch in repo.references("refs/heads/")? {
    ///     println!("{} {:?}", branch.shorthand(), branch.id);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    /// As [`Repository::find_reference`].
    pub fn references(&self, prefix: impl AsRef<[u8]>) -> Result<Vec<Reference>> {
        let prefix = backend_name(prefix.as_ref())?;
        let items = self
            .refs
            .iter(&IterOptions::prefix(prefix))
            .map_err(refs_error)?;
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let IterItem::Ref(r) = item else {
                // A reference file the backend could not read; git warns
                // and goes on.
                continue;
            };
            let target = match r.target {
                rgit_refs::Target::Object(id) => Target::Object(id),
                rgit_refs::Target::Symbolic(to) => Target::Symbolic(api_name(&to)),
            };
            out.push(Reference {
                name: api_name(&r.name),
                target,
                id: r.oid,
            });
        }
        Ok(out)
    }
}
