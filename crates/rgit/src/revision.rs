// SPDX-License-Identifier: MIT
//! Revision expressions (`gitrevisions(7)`): the repository as
//! `rgit-revwalk` wants to see it, and [`Repository::rev_parse`].

use std::cell::OnceCell;

use bstr::BString;
use rgit_config::ConfigSet;
use rgit_hash::{HashKind, ObjectId, Prefix};
use rgit_index::{Index, Stage};
use rgit_object::ObjectKind;
use rgit_refs::reflog::FilesReflog;
use rgit_refs::{IterItem, IterOptions, RefStore};
use rgit_revwalk::revision::{
    self, ConfigLookup, Flags, IndexLookup, LookupError, ObjectLookup, Options, RefLookup,
    RefValue, ReflogEntry, Resolver, WorkTreeInfo, WorkTreeLookup,
};

use crate::error::{Diagnostic, Level};
use crate::{Error, Repository, Result};

fn odb_error(e: &rgit_odb::Error) -> LookupError {
    match e {
        rgit_odb::Error::InvalidType { .. } | rgit_odb::Error::Config(_) => {
            LookupError::fatal(e.to_string())
        }
        e => LookupError::corrupt(e.git_lines()),
    }
}

fn fatal(e: impl std::fmt::Display) -> LookupError {
    LookupError::fatal(e.to_string())
}

/// The object database of a [`Repository`].
struct Objects<'r>(&'r Repository);

impl ObjectLookup for Objects<'_> {
    fn hash_kind(&self) -> HashKind {
        self.0.object_format()
    }

    fn read(&self, oid: &ObjectId) -> Result<Option<(ObjectKind, Vec<u8>)>, LookupError> {
        match self.0.odb.read(oid) {
            Ok(raw) => Ok(raw.map(|r| (r.kind, r.data))),
            Err(e) => Err(odb_error(&e)),
        }
    }

    fn kind(&self, oid: &ObjectId) -> Result<Option<ObjectKind>, LookupError> {
        match self.0.odb.header(oid) {
            Ok(header) => Ok(header.map(|h| h.kind)),
            Err(e) => Err(odb_error(&e)),
        }
    }

    fn prefix_matches(&self, prefix: &Prefix) -> Result<Vec<ObjectId>, LookupError> {
        let mut out = Vec::new();
        for store in self.0.odb.stores() {
            store
                .for_each_prefix_match(prefix, &mut |id| out.push(id))
                .map_err(|e| odb_error(&e))?;
        }
        Ok(out)
    }

    fn approximate_count(&self) -> u64 {
        // Enough for the length of an abbreviation: the packed objects
        // plus sixteen times the loose objects of one fan-out directory.
        let packed: u64 = self
            .0
            .odb
            .packs()
            .map(|p| p.set().packs())
            .unwrap_or_default()
            .iter()
            .map(|p| u64::from(p.index().len()))
            .sum();
        let objects = self.0.common_dir().join("objects");
        let loose: u64 = (0..16u32)
            .filter_map(|i| std::fs::read_dir(objects.join(format!("{i:02x}"))).ok())
            .map(|dir| dir.count() as u64)
            .sum();
        packed + loose * 16
    }

    fn is_shallow(&self, oid: &ObjectId) -> bool {
        self.0.shallow().contains(oid)
    }
}

/// The references and reflogs of a [`Repository`].
struct Refs<'r>(&'r Repository);

impl RefLookup for Refs<'_> {
    fn resolve(&self, name: &str) -> Result<Option<RefValue>, LookupError> {
        let resolved = self.0.refs.resolve(name).map_err(fatal)?;
        Ok(resolved.map(|r| RefValue {
            name: r.final_name().to_string(),
            oid: r.oid,
        }))
    }

    fn for_each_ref(
        &self,
        prefix: &str,
        f: &mut dyn FnMut(&str, &ObjectId),
    ) -> Result<(), LookupError> {
        let items = self
            .0
            .refs
            .iter(&IterOptions::prefix(prefix))
            .map_err(fatal)?;
        for item in items {
            if let IterItem::Ref(r) = item {
                if let Some(oid) = r.oid {
                    f(&r.name, &oid);
                }
            }
        }
        Ok(())
    }

    fn reflog(&self, name: &str) -> Result<Option<Vec<ReflogEntry>>, LookupError> {
        let logs = FilesReflog::new(
            self.0.git_dir(),
            self.0.common_dir(),
            self.0.object_format(),
        );
        let Some(entries) = logs.read(name).map_err(fatal)? else {
            return Ok(None);
        };
        Ok(Some(
            entries
                .into_iter()
                .map(|e| ReflogEntry {
                    old: e.old,
                    new: e.new,
                    time: e.time,
                    tz: e.tz,
                    message: String::from_utf8_lossy(&e.message).into_owned(),
                })
                .collect(),
        ))
    }
}

/// The configuration of a [`Repository`].
struct Config<'r>(&'r ConfigSet);

impl ConfigLookup for Config<'_> {
    fn get(&self, key: &str) -> Option<String> {
        self.0
            .get(key)
            .and_then(|e| e.value.as_ref())
            .map(ToString::to_string)
    }

    fn get_all(&self, key: &str) -> Vec<String> {
        self.0
            .get_all(key)
            .filter_map(|e| e.value.as_ref().map(ToString::to_string))
            .collect()
    }

    fn entries_with_prefix(&self, prefix: &str) -> Vec<(String, String)> {
        self.0
            .entries()
            .iter()
            .filter(|e| e.name.starts_with(prefix.as_bytes()))
            .filter_map(|e| Some((e.name.to_string(), e.value.as_ref()?.to_string())))
            .collect()
    }
}

/// The index of a [`Repository`], read when a revision asks for it
/// (`:<path>`, `:<n>:<path>`).
struct IndexFile<'r> {
    repo: &'r Repository,
    index: OnceCell<Option<Index>>,
}

impl IndexLookup for IndexFile<'_> {
    fn entry(&self, path: &[u8], stage: u8) -> Option<ObjectId> {
        let stage = match stage {
            1 => Stage::BASE,
            2 => Stage::OURS,
            3 => Stage::THEIRS,
            _ => Stage::MERGED,
        };
        self.index
            .get_or_init(|| self.repo.read_index().ok())
            .as_ref()?
            .get(path, stage)
            .map(|e| e.id)
    }
}

/// The work tree of a [`Repository`], for the hints of failed lookups.
struct WorkTree<'r>(&'r std::path::Path);

impl WorkTreeLookup for WorkTree<'_> {
    fn exists(&self, path: &str) -> bool {
        std::fs::symlink_metadata(self.0.join(path)).is_ok()
    }
}

/// Settings from the configuration that change how names resolve.
/// Values git would reject fall back to the default here; the commands
/// that own a setting report it.
fn options(config: &ConfigSet, hash: HashKind) -> Options {
    let mut opts = Options::default();
    if let Ok(Some(warn)) = config.bool("core.warnambiguousrefs") {
        opts.warn_ambiguous_refs = warn;
    }
    if let Ok(Some(value)) = config.string("core.disambiguate") {
        if let Some(hint) = revision::Hint::from_config(&value.to_string()) {
            opts.disambiguate = hint;
        }
    }
    if let Ok(Some(value)) = config.string("core.abbrev") {
        let value = value.to_string();
        if !value.eq_ignore_ascii_case("auto") {
            if let Ok(n) = value.parse::<usize>() {
                opts.abbrev = Some(n.clamp(4, hash.hex_len()));
            }
        }
    }
    if let Ok(Some(value)) = config.string("push.default") {
        if let Some(default) = revision::PushDefault::from_config(&value.to_string()) {
            opts.push_default = default;
        }
    }
    opts
}

fn diagnostics(resolver: &Resolver<'_>) -> Vec<Diagnostic> {
    resolver
        .take_diagnostics()
        .into_iter()
        .map(|d| Diagnostic {
            level: match d.level {
                revision::Level::Warning => Level::Warning,
                revision::Level::Error => Level::Error,
                revision::Level::Hint => Level::Hint,
            },
            text: d.text,
        })
        .collect()
}

impl Repository {
    /// Run `f` with a resolver for this repository. Paths in revision
    /// expressions (`HEAD:src/lib.rs`, `:README`) count from the top of
    /// the work tree: the library has no current directory.
    pub(crate) fn with_resolver<T>(&self, f: impl FnOnce(&Resolver<'_>) -> T) -> T {
        let objects = Objects(self);
        let refs = Refs(self);
        let config = Config(&self.config);
        let index = IndexFile {
            repo: self,
            index: OnceCell::new(),
        };
        let work_tree = self.work_dir().map(WorkTree);
        let resolver = Resolver::new(
            revision::Repo {
                objects: &objects,
                refs: &refs,
                config: &config,
                index: Some(&index),
                work_tree: work_tree.as_ref().map(|w| -> &dyn WorkTreeLookup { w }),
                work_tree_info: self.work_dir().map(|top| WorkTreeInfo {
                    path: top.display().to_string(),
                    prefix: String::new(),
                    cwd_inside: true,
                }),
            },
            options(&self.config, self.object_format()),
        );
        f(&resolver)
    }

    /// The error of a failed lookup of `spec`, with the reason git would
    /// give.
    pub(crate) fn revision_error(resolver: &Resolver<'_>, spec: &str, e: revision::Error) -> Error {
        let message = match e {
            revision::Error::NotFound => resolver.diagnose(spec),
            revision::Error::Fatal(message) => Some(message),
            // The repository could not be read: not a matter of the
            // expression.
            revision::Error::Lookup(e) => {
                return Error::Corrupt {
                    message: e.message(),
                    source: None,
                };
            }
        };
        match message {
            Some(message) => Error::Revision {
                spec: spec.into(),
                message,
                diagnostics: diagnostics(resolver),
            },
            None => Error::RevisionNotFound { spec: spec.into() },
        }
    }

    /// The object a revision expression names: a full or abbreviated
    /// object name, a reference (`main`, `origin/main`, `v1.0`, `HEAD`),
    /// with the suffixes of `gitrevisions(7)` (`HEAD~2`, `v1.0^{commit}`,
    /// `main@{yesterday}`, `@{upstream}`, `HEAD:src/lib.rs`, `:/fix`).
    ///
    /// The object is returned as named: `v1.0` gives the tag object of an
    /// annotated tag, `v1.0^{commit}` the commit.
    ///
    /// ```no_run
    /// # fn main() -> rgit::Result<()> {
    /// let repo = rgit::Repository::open(".")?;
    /// let tip = repo.rev_parse("HEAD")?;
    /// let grandparent = repo.rev_parse("HEAD~2")?;
    /// let readme = repo.rev_parse("HEAD:README.md")?;
    /// match repo.rev_parse("no-such-branch") {
    ///     Err(rgit::Error::RevisionNotFound { spec }) => eprintln!("unknown: {spec}"),
    ///     other => { other?; }
    /// }
    /// # let _ = (tip, grandparent, readme);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    /// [`Error::RevisionNotFound`] if the expression names nothing,
    /// [`Error::Revision`] with git's message if it cannot be resolved for
    /// a more specific reason (ambiguous short id, reflog too short, no
    /// upstream, path not in the tree).
    pub fn rev_parse(&self, spec: impl AsRef<[u8]>) -> Result<ObjectId> {
        let spec = spec.as_ref();
        // Until reference names are byte strings in the backend (#214), an
        // expression that is not UTF-8 cannot name anything.
        let Ok(text) = std::str::from_utf8(spec) else {
            return Err(Error::RevisionNotFound {
                spec: BString::from(spec),
            });
        };
        self.with_resolver(|resolver| {
            let result = resolver.resolve_with(text, Flags::default());
            match result {
                Ok(id) => {
                    self.push_diagnostics(diagnostics(resolver));
                    Ok(id)
                }
                Err(e) => Err(Repository::revision_error(resolver, text, e)),
            }
        })
    }
}
