// SPDX-License-Identifier: MIT
//! The merged view of all configuration sources and how it is loaded.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use bstr::{BStr, BString, ByteSlice};

use crate::cmdline::{self, CommandLine, Parameter};
use crate::env::Environment;
use crate::error::{Error, ValueError, ValueErrorKind};
use crate::include::{self, Condition, IncludeKind, MAX_INCLUDE_DEPTH, RepoFacts};
use crate::origin::{Origin, Scope};
use crate::parse::{Item, Parser, SectionHeader, canonical_name};
use crate::value::{self, BoolOrInt, IntWidth};

/// Where an entry came from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Source {
    /// The file, blob, standard input or command line.
    pub origin: Origin,
    /// The scope.
    pub scope: Scope,
}

/// One variable assignment with its origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Canonical name: `section[.subsection].key`, section and key
    /// lower-cased.
    pub name: BString,
    /// The value; `None` for a value-less key (boolean true).
    pub value: Option<BString>,
    /// Origin and scope.
    pub source: Arc<Source>,
    /// 1-based line in the origin; 0 for the command line.
    pub line: usize,
    /// How many includes deep the entry was found (0 = top-level source).
    pub include_depth: usize,
}

impl Entry {
    fn error(&self, kind: ValueErrorKind) -> ValueError {
        ValueError {
            kind,
            name: self.name.clone(),
            value: self.value.clone().unwrap_or_default(),
            origin: Some(self.source.origin.clone()),
        }
    }

    /// The value as a string; a value-less key is an error, as for git's
    /// string options.
    pub fn string(&self) -> Result<&BStr, ValueError> {
        self.value
            .as_ref()
            .map(|v| v.as_bstr())
            .ok_or_else(|| self.error(ValueErrorKind::Missing))
    }

    /// `--type=bool`.
    pub fn bool(&self) -> Result<bool, ValueError> {
        value::parse_bool(self.value.as_deref().map(|v| v.as_bytes()))
            .ok_or_else(|| self.error(ValueErrorKind::Bool))
    }

    /// Integer of the given width with `k`/`m`/`g` suffixes. A value-less
    /// key is parsed as the empty string (`invalid unit`).
    pub fn int(&self, width: IntWidth) -> Result<i64, ValueError> {
        value::parse_int(
            self.value
                .as_ref()
                .map(|v| v.as_bytes())
                .unwrap_or_default(),
            width,
        )
        .map_err(|e| self.error(ValueErrorKind::Int(e)))
    }

    /// `--type=bool-or-int`.
    pub fn bool_or_int(&self) -> Result<BoolOrInt, ValueError> {
        value::parse_bool_or_int(self.value.as_deref().map(|v| v.as_bytes()))
            .map_err(|e| self.error(ValueErrorKind::Int(e)))
    }

    /// `--type=path`: `Ok(None)` for a missing `:(optional)` path.
    pub fn path(&self, env: &dyn Environment) -> Result<Option<PathBuf>, ValueError> {
        let v = self.string()?;
        value::interpolate_path(v, env).map_err(|value::PathError::UserDir(p)| ValueError {
            kind: ValueErrorKind::Path,
            name: self.name.clone(),
            value: p,
            origin: Some(self.source.origin.clone()),
        })
    }
}

/// All loaded entries in reading order (later entries win).
#[derive(Debug, Clone, Default)]
pub struct ConfigSet {
    entries: Vec<Entry>,
    index: HashMap<BString, Vec<usize>>,
}

impl ConfigSet {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append an entry.
    pub fn push(&mut self, entry: Entry) {
        self.index
            .entry(entry.name.clone())
            .or_default()
            .push(self.entries.len());
        self.entries.push(entry);
    }

    /// All entries in order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All entries of a variable, oldest first. `name` is canonicalized; an
    /// invalid name has no entries.
    pub fn get_all(&self, name: impl AsRef<[u8]>) -> impl Iterator<Item = &Entry> {
        let idx = crate::key::canonicalize(name)
            .ok()
            .and_then(|n| self.index.get(&n));
        idx.into_iter().flatten().map(|&i| &self.entries[i])
    }

    /// The last (effective) entry of a variable.
    pub fn get(&self, name: impl AsRef<[u8]>) -> Option<&Entry> {
        self.get_all(name).last()
    }

    /// The effective value as a string (`Ok(None)` if unset).
    pub fn string(&self, name: impl AsRef<[u8]>) -> Result<Option<&BStr>, ValueError> {
        self.get(name).map(Entry::string).transpose()
    }

    /// The effective value as a boolean.
    pub fn bool(&self, name: impl AsRef<[u8]>) -> Result<Option<bool>, ValueError> {
        self.get(name).map(Entry::bool).transpose()
    }

    /// The effective value as an `int` (32-bit, git's `git_config_int`).
    pub fn int(&self, name: impl AsRef<[u8]>) -> Result<Option<i64>, ValueError> {
        self.get(name).map(|e| e.int(IntWidth::I32)).transpose()
    }

    /// The effective value as a 64-bit integer.
    pub fn int64(&self, name: impl AsRef<[u8]>) -> Result<Option<i64>, ValueError> {
        self.get(name).map(|e| e.int(IntWidth::I64)).transpose()
    }

    /// The effective value as a path. A missing `:(optional)` path makes the
    /// variable count as unset, falling back to earlier values like git.
    pub fn path(
        &self,
        name: impl AsRef<[u8]>,
        env: &dyn Environment,
    ) -> Result<Option<PathBuf>, ValueError> {
        let all: Vec<&Entry> = self.get_all(name).collect();
        for e in all.into_iter().rev() {
            if let Some(p) = e.path(env)? {
                return Ok(Some(p));
            }
        }
        Ok(None)
    }

    /// Distinct subsections of `section` in first-seen order (for example
    /// remote names for `remote`).
    pub fn subsections(&self, section: &str) -> Vec<BString> {
        let mut out: Vec<BString> = Vec::new();
        for e in &self.entries {
            if let (s, Some(sub), _) = crate::key::split_canonical(e.name.as_bstr()) {
                if s.eq_ignore_ascii_case(section.as_bytes()) && !out.iter().any(|o| o == sub) {
                    out.push(sub.to_owned());
                }
            }
        }
        out
    }
}

/// Description of one configuration source to load.
#[derive(Debug, Clone)]
pub enum SourceSpec {
    /// A file. A missing file is skipped.
    File {
        /// The path, as it should appear in messages.
        path: PathBuf,
        /// Its scope.
        scope: Scope,
        /// An unreadable file is an error (repository files) rather than
        /// silently skipped (system and global files).
        required: bool,
    },
    /// In-memory content: a blob or standard input.
    Bytes {
        /// The content.
        data: Vec<u8>,
        /// Its origin.
        origin: Origin,
        /// Its scope.
        scope: Scope,
    },
    /// `GIT_CONFIG_COUNT` pairs, `GIT_CONFIG_PARAMETERS` and the given
    /// `-c`/`--config-env` settings, in that order.
    CommandLine(CommandLine),
}

/// Loading options.
#[derive(Clone, Copy)]
pub struct LoadOptions<'a> {
    /// The environment.
    pub env: &'a dyn Environment,
    /// Repository facts for `includeIf`; `None` outside a repository.
    pub repo: Option<&'a dyn RepoFacts>,
    /// Follow `include.path` and `includeIf`.
    pub includes: bool,
}

/// Loads a list of sources into a [`ConfigSet`], following includes.
pub struct Loader<'a> {
    opts: LoadOptions<'a>,
    sources: Vec<SourceSpec>,
    remote_urls: Option<Vec<BString>>,
}

/// Per-source state while reading.
struct Ctx<'s> {
    source: &'s Arc<Source>,
    depth: usize,
    /// Inside a file included through `hasconfig:remote.*.url`.
    in_hasconfig: bool,
    /// Pre-scan for remote URLs: do not evaluate `hasconfig` and do not
    /// fail on errors.
    scanning: bool,
}

impl<'a> Loader<'a> {
    /// A loader with no sources.
    pub fn new(opts: LoadOptions<'a>) -> Self {
        Loader {
            opts,
            sources: Vec::new(),
            remote_urls: None,
        }
    }

    /// Add a source; sources are read in the order added.
    pub fn add(&mut self, spec: SourceSpec) -> &mut Self {
        self.sources.push(spec);
        self
    }

    /// Read all sources into `set`. On error, `set` keeps the entries read
    /// before the error, so a caller can mimic git's streaming output.
    pub fn load_into(&mut self, set: &mut ConfigSet) -> Result<(), Error> {
        let sources = self.sources.clone();
        sources
            .iter()
            .try_for_each(|spec| self.load_spec(spec, set, false))
    }

    /// Read all sources into a new set.
    pub fn load(&mut self) -> Result<ConfigSet, Error> {
        let mut set = ConfigSet::new();
        self.load_into(&mut set)?;
        Ok(set)
    }

    fn load_spec(
        &mut self,
        spec: &SourceSpec,
        set: &mut ConfigSet,
        scanning: bool,
    ) -> Result<(), Error> {
        match spec {
            SourceSpec::File {
                path,
                scope,
                required,
            } => {
                let data = match std::fs::read(path) {
                    Ok(d) => d,
                    Err(e) if is_missing(&e) => return Ok(()),
                    Err(e) if *required => {
                        return Err(Error::Access {
                            path: path.clone(),
                            source: e,
                            warn: true,
                        });
                    }
                    Err(_) => return Ok(()),
                };
                let source = Arc::new(Source {
                    origin: Origin::File(path.clone()),
                    scope: *scope,
                });
                self.parse_into(&data, &source, 0, false, scanning, set)
            }
            SourceSpec::Bytes {
                data,
                origin,
                scope,
            } => {
                let source = Arc::new(Source {
                    origin: origin.clone(),
                    scope: *scope,
                });
                self.parse_into(data, &source, 0, false, scanning, set)
            }
            SourceSpec::CommandLine(cl) => self.load_command_line(cl, set, scanning),
        }
    }

    fn load_command_line(
        &mut self,
        cl: &CommandLine,
        set: &mut ConfigSet,
        scanning: bool,
    ) -> Result<(), Error> {
        let env = self.opts.env;
        let mut params: Vec<Parameter> = cmdline::env_count_parameters(env)?;
        if let Some(p) = env.var_bytes("GIT_CONFIG_PARAMETERS") {
            params.extend(cmdline::parse_parameters(&p)?);
        }
        params.extend(cl.parameters.iter().cloned());
        let source = Arc::new(Source {
            origin: Origin::CommandLine,
            scope: Scope::Command,
        });
        for p in params {
            let name = cmdline::canonical_key(p.key.as_bstr())?;
            let ctx = Ctx {
                source: &source,
                depth: 0,
                in_hasconfig: false,
                scanning,
            };
            self.add_entry(set, &ctx, name, p.value, 0)?;
        }
        Ok(())
    }

    fn parse_into(
        &mut self,
        data: &[u8],
        source: &Arc<Source>,
        depth: usize,
        in_hasconfig: bool,
        scanning: bool,
        set: &mut ConfigSet,
    ) -> Result<(), Error> {
        let ctx = Ctx {
            source,
            depth,
            in_hasconfig,
            scanning,
        };
        let mut headers: HashMap<usize, SectionHeader> = HashMap::new();
        for (index, item) in Parser::new(data).enumerate() {
            let item = item.map_err(|e| Error::Syntax {
                line: e.line,
                origin: source.origin.clone(),
            })?;
            match item {
                Item::Section(h) => {
                    headers.insert(index, h);
                }
                Item::Entry(e) => {
                    let header = e.header.and_then(|i| headers.get(&i));
                    let name = canonical_name(header, &e.key);
                    self.add_entry(set, &ctx, name, e.value, e.line)?;
                }
                Item::Trivia(_) => {}
            }
        }
        Ok(())
    }

    fn add_entry(
        &mut self,
        set: &mut ConfigSet,
        ctx: &Ctx<'_>,
        name: BString,
        value: Option<BString>,
        line: usize,
    ) -> Result<(), Error> {
        if ctx.in_hasconfig && include::is_remote_url(name.as_bstr()) {
            return Err(Error::RemoteUrlInHasconfig);
        }
        let entry = Entry {
            name,
            value,
            source: ctx.source.clone(),
            line,
            include_depth: ctx.depth,
        };
        set.push(entry.clone());
        if !self.opts.includes {
            return Ok(());
        }
        let result = self.handle_include(set, ctx, &entry);
        match result {
            Err(e) if !ctx.scanning => Err(e.in_entry(line, &ctx.source.origin)),
            _ => Ok(()),
        }
    }

    fn handle_include(
        &mut self,
        set: &mut ConfigSet,
        ctx: &Ctx<'_>,
        entry: &Entry,
    ) -> Result<(), Error> {
        let Some(kind) = include::include_kind(entry.name.as_bstr()) else {
            return Ok(());
        };
        let Some(value) = &entry.value else {
            return Err(Error::MissingValue {
                name: entry.name.clone(),
            });
        };
        let mut via_hasconfig = false;
        if let IncludeKind::If(cond) = kind {
            let cond = include::parse_condition(cond);
            via_hasconfig = matches!(cond, Condition::RemoteUrl(_));
            if !self.condition_true(&cond, ctx)? {
                return Ok(());
            }
        }
        if value.is_empty() {
            return Ok(());
        }
        let expanded = value::expand_user_path(value, self.opts.env).map_err(|_| {
            Error::ExpandIncludePath {
                path: value.clone(),
            }
        })?;
        let path = if expanded.is_absolute() {
            expanded
        } else {
            match &ctx.source.origin {
                Origin::File(f) => match f.parent() {
                    Some(dir) if !dir.as_os_str().is_empty() => dir.join(&expanded),
                    _ => expanded,
                },
                _ => return Err(Error::RelativeInclude),
            }
        };
        if ctx.depth + 1 > MAX_INCLUDE_DEPTH {
            return Err(Error::IncludeDepth {
                depth: MAX_INCLUDE_DEPTH,
                path,
                from: from_text(&ctx.source.origin),
            });
        }
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(e) if is_missing(&e) => return Ok(()),
            Err(e) => {
                return Err(Error::Access {
                    path,
                    source: e,
                    warn: false,
                });
            }
        };
        let source = Arc::new(Source {
            origin: Origin::File(path),
            scope: ctx.source.scope,
        });
        self.parse_into(
            &data,
            &source,
            ctx.depth + 1,
            ctx.in_hasconfig || via_hasconfig,
            ctx.scanning,
            set,
        )
    }

    fn condition_true(&mut self, cond: &Condition<'_>, ctx: &Ctx<'_>) -> Result<bool, Error> {
        let env = self.opts.env;
        let repo = self.opts.repo;
        Ok(match *cond {
            Condition::GitDir { pattern, icase } => {
                let Some(dir) = repo.and_then(|r| r.git_dir()) else {
                    return Ok(false);
                };
                let patterns = include::prepare_path_pattern(pattern, &ctx.source.origin, env)?;
                include::path_matches(&patterns, &dir, icase)
            }
            Condition::Worktree { pattern, icase } => {
                let Some(dir) = repo.and_then(|r| r.worktree()) else {
                    return Ok(false);
                };
                let patterns = include::prepare_path_pattern(pattern, &ctx.source.origin, env)?;
                include::path_matches(&patterns, &dir, icase)
            }
            Condition::OnBranch(pattern) => match repo.and_then(|r| r.current_branch()) {
                Some(branch) => include::branch_matches(pattern, &branch),
                None => false,
            },
            Condition::RemoteUrl(pattern) => {
                if ctx.scanning {
                    return Ok(false);
                }
                let urls = self.remote_urls();
                urls.iter().any(|u| include::url_matches(pattern, u))
            }
            Condition::Unknown => false,
        })
    }

    /// All `remote.*.url` values of all sources (with includes, but without
    /// `hasconfig` includes), computed once.
    fn remote_urls(&mut self) -> Vec<BString> {
        if let Some(urls) = &self.remote_urls {
            return urls.clone();
        }
        let sources = self.sources.clone();
        let mut scan = ConfigSet::new();
        for spec in &sources {
            if self.load_spec(spec, &mut scan, true).is_err() {
                break;
            }
        }
        let urls: Vec<BString> = scan
            .entries()
            .iter()
            .filter(|e| include::is_remote_url(e.name.as_bstr()))
            .filter_map(|e| e.value.clone())
            .collect();
        self.remote_urls = Some(urls.clone());
        urls
    }
}

fn from_text(origin: &Origin) -> String {
    match origin {
        Origin::File(p) => p.display().to_string(),
        Origin::Blob(b) => b.to_string(),
        Origin::Stdin => "standard input".into(),
        Origin::CommandLine => "command line".into(),
    }
}

fn is_missing(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

/// Convenience: parse one file's content into a set (no includes).
pub fn parse_bytes(data: &[u8], origin: Origin, scope: Scope) -> Result<ConfigSet, Error> {
    let env = crate::env::MapEnv::default();
    Loader::new(LoadOptions {
        env: &env,
        repo: None,
        includes: false,
    })
    .add(SourceSpec::Bytes {
        data: data.to_vec(),
        origin,
        scope,
    })
    .load()
}
