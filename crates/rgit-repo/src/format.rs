// SPDX-License-Identifier: MIT
//! `core.repositoryFormatVersion` and `extensions.*`
//! (`Documentation/technical/repository-version.adoc`,
//! `Documentation/config/extensions.adoc`).

use bstr::{BString, ByteSlice};
use rgit_config::{ConfigSet, Entry, IntWidth};
use rgit_hash::HashKind;

use crate::error::Error;
use crate::messages;

/// The reference storage backend (`extensions.refStorage`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RefStorage {
    /// Loose files and `packed-refs`.
    #[default]
    Files,
    /// reftable.
    Reftable,
}

/// What this build of rgit can open. Known extensions outside this set are
/// refused with [`Error::Unsupported`] before anything is read or written.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Support {
    /// `extensions.objectFormat=sha256`.
    pub sha256: bool,
    /// `extensions.compatObjectFormat`.
    pub compat_object_format: bool,
    /// `extensions.refStorage=reftable`.
    pub reftable: bool,
}

/// The repository format: version and extensions of a repository's own
/// configuration file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepositoryFormat {
    /// `core.repositoryFormatVersion` (0 when unset).
    pub version: i64,
    /// `core.bare` from the repository configuration file.
    pub bare: Option<bool>,
    /// `core.worktree` from the repository configuration file.
    pub worktree: Option<BString>,
    /// `extensions.objectFormat`.
    pub object_format: HashKind,
    /// `extensions.compatObjectFormat`.
    pub compat_object_format: Option<HashKind>,
    /// `extensions.refStorage` (format part).
    pub ref_storage: RefStorage,
    /// `extensions.refStorage` payload after `<format>://`.
    pub ref_storage_payload: Option<BString>,
    /// `extensions.preciousObjects`: never delete objects.
    pub precious_objects: bool,
    /// `extensions.partialClone`: the promisor remote.
    pub partial_clone: Option<BString>,
    /// `extensions.worktreeConfig`.
    pub worktree_config: bool,
    /// `extensions.relativeWorktrees`.
    pub relative_worktrees: bool,
    /// `extensions.submodulePathConfig`.
    pub submodule_path_config: bool,
    /// Extensions not known to this implementation, in order.
    pub unknown: Vec<String>,
    /// Known extensions that require version 1, in order.
    pub v1_only: Vec<String>,
}

impl RepositoryFormat {
    /// Read the format from the entries of a repository configuration file.
    /// Values git rejects while reading fail here; [`RepositoryFormat::check`]
    /// applies the version rules.
    pub fn from_config(set: &ConfigSet) -> Result<Self, Error> {
        let mut f = RepositoryFormat::default();
        for e in set.entries() {
            f.apply(e)?;
        }
        Ok(f)
    }

    fn apply(&mut self, e: &Entry) -> Result<(), Error> {
        let name = e.name.to_str_lossy();
        match name.as_ref() {
            "core.repositoryformatversion" => self.version = e.int(IntWidth::I32)?,
            "core.bare" => self.bare = Some(e.bool()?),
            "core.worktree" => self.worktree = Some(string(e)?.into()),
            _ => {}
        }
        let Some(ext) = name.strip_prefix("extensions.") else {
            return Ok(());
        };
        if ext.contains('.') {
            // extensions.<subsection>.<key> is not an extension.
            return Ok(());
        }
        let ext = ext.to_string();
        match ext.as_str() {
            "noop" => {}
            "preciousobjects" => self.precious_objects = e.bool()?,
            "partialclone" => self.partial_clone = Some(string(e)?.into()),
            "worktreeconfig" => self.worktree_config = e.bool()?,
            "noop-v1" => self.v1_only.push(ext),
            "objectformat" => {
                self.object_format = hash_kind(e)?;
                self.v1_only.push(ext);
            }
            "compatobjectformat" => {
                self.compat_object_format = Some(hash_kind(e)?);
                self.v1_only.push(ext);
            }
            "refstorage" => {
                let v = string(e)?;
                let (format, payload) = match v.find(b"://") {
                    Some(i) => (&v[..i], Some(BString::from(&v[i + 3..]))),
                    None => (&v[..], None),
                };
                self.ref_storage = match format.as_bytes() {
                    b"files" => RefStorage::Files,
                    b"reftable" => RefStorage::Reftable,
                    _ => return Err(invalid_value(e, v)),
                };
                self.ref_storage_payload = payload;
                self.v1_only.push(ext);
            }
            "relativeworktrees" => {
                self.relative_worktrees = e.bool()?;
                self.v1_only.push(ext);
            }
            "submodulepathconfig" => {
                self.submodule_path_config = e.bool()?;
                self.v1_only.push(ext);
            }
            _ => self.unknown.push(ext),
        }
        Ok(())
    }

    /// Apply the version rules: version above 1 is fatal, unknown
    /// extensions are fatal in version 1, extensions that need version 1
    /// are fatal in version 0.
    pub fn check(&self) -> Result<(), Error> {
        if self.version > 1 {
            return Err(Error::VersionTooNew(self.version));
        }
        if self.version == 1 && !self.unknown.is_empty() {
            return Err(Error::UnknownExtensions(self.unknown.clone()));
        }
        if self.version == 0 && !self.v1_only.is_empty() {
            return Err(Error::V1OnlyExtensions(self.v1_only.clone()));
        }
        if self.compat_object_format == Some(self.object_format) {
            return Err(Error::Unsupported(messages::same_compat_format().into()));
        }
        Ok(())
    }

    /// Refuse what `support` does not cover.
    pub fn check_support(&self, support: &Support) -> Result<(), Error> {
        if self.object_format == HashKind::Sha256 && !support.sha256 {
            return Err(Error::Unsupported("extensions.objectFormat=sha256".into()));
        }
        if let Some(k) = self.compat_object_format {
            if !support.compat_object_format {
                return Err(Error::Unsupported(format!(
                    "extensions.compatObjectFormat={}",
                    k.name()
                )));
            }
        }
        if self.ref_storage == RefStorage::Reftable && !support.reftable {
            return Err(Error::Unsupported("extensions.refStorage=reftable".into()));
        }
        Ok(())
    }

    /// Whether `config.worktree` is read.
    pub fn uses_worktree_config(&self) -> bool {
        self.worktree_config
    }
}

fn string(e: &Entry) -> Result<&bstr::BStr, Error> {
    e.string().map_err(|err| bad_entry(e, err.to_string()))
}

fn hash_kind(e: &Entry) -> Result<HashKind, Error> {
    let v = string(e)?;
    match v.to_str().ok().and_then(HashKind::from_name) {
        Some(k) => Ok(k),
        None => Err(invalid_value(e, v)),
    }
}

fn invalid_value(e: &Entry, v: &[u8]) -> Error {
    bad_entry(
        e,
        messages::invalid_extension_value(e.name.as_bstr(), v.as_bstr()),
    )
}

fn bad_entry(e: &Entry, message: String) -> Error {
    Error::BadEntry {
        message,
        fatal: format!(
            "bad config line {} in {}",
            e.line,
            e.source.origin.describe()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgit_config::{Origin, Scope, parse_bytes};

    fn fmt(text: &str) -> Result<RepositoryFormat, Error> {
        let set = parse_bytes(
            text.as_bytes(),
            Origin::File(".git/config".into()),
            Scope::Local,
        )
        .unwrap();
        let f = RepositoryFormat::from_config(&set)?;
        f.check()?;
        Ok(f)
    }

    fn err(text: &str) -> String {
        let e = fmt(text).unwrap_err();
        e.report()
            .into_iter()
            .map(|(l, m)| format!("{} {m}\n", l.prefix()))
            .collect()
    }

    #[test]
    fn versions() {
        assert_eq!(
            fmt("[core]\nrepositoryformatversion=0\n").unwrap().version,
            0
        );
        assert_eq!(
            err("[core]\nrepositoryformatversion=99\n"),
            "fatal: Expected git repo version <= 1, found 99\n"
        );
        assert_eq!(
            err("[core]\nrepositoryformatversion=1k\n"),
            "fatal: Expected git repo version <= 1, found 1024\n"
        );
        assert!(fmt("[core]\nrepositoryformatversion=-1\n[extensions]\nfoo=1\nnoop-v1\n").is_ok());
    }

    #[test]
    fn extensions() {
        assert_eq!(
            err("[core]\nrepositoryformatversion=1\n[extensions]\nno-such=1\nother=2\n"),
            "fatal: unknown repository extensions found:\n\tno-such\n\tother\n"
        );
        assert_eq!(
            err("[core]\nrepositoryformatversion=0\n[extensions]\nnoop-v1\n"),
            "fatal: repo version is 0, but v1-only extension found:\n\tnoop-v1\n"
        );
        assert!(fmt("[core]\nrepositoryformatversion=0\n[extensions]\nnoop\nfoo\n").is_ok());
        assert_eq!(
            err("[core]\nrepositoryformatversion=1\n[extensions]\nobjectformat=sha512\n"),
            "error: invalid value for 'extensions.objectformat': 'sha512'\n\
             fatal: bad config line 4 in file .git/config\n"
        );
        assert_eq!(
            err("[core]\nrepositoryformatversion=1\n[extensions]\npartialclone\n"),
            "error: missing value for 'extensions.partialclone'\n\
             fatal: bad config line 4 in file .git/config\n"
        );
        assert_eq!(
            err("[core]\nrepositoryformatversion=0\n[extensions]\nworktreeconfig=maybe\n"),
            "fatal: bad boolean config value 'maybe' for 'extensions.worktreeconfig'\n"
        );
        let f = fmt("[core]\nrepositoryformatversion=1\n[extensions]\nrefstorage=reftable://x\n")
            .unwrap();
        assert_eq!(f.ref_storage, RefStorage::Reftable);
        assert!(f.check_support(&Support::default()).is_err());
    }
}
