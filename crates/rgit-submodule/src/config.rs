// SPDX-License-Identifier: MIT
//! Parsing `.gitmodules`.

use bstr::{BStr, BString, ByteSlice};
use rgit_config::parse::{Item, Parser, SectionHeader};
use rgit_config::value::parse_bool;

use crate::messages;
use crate::name::check_name;
use crate::url::looks_like_option;

/// `submodule.<name>.ignore` (and `--ignore-submodules`): which changes of a
/// submodule make it count as modified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Ignore {
    /// Nothing is ignored: new commits, modified and untracked content count.
    None,
    /// Untracked files in the submodule are ignored.
    Untracked,
    /// All changes of the submodule's work tree are ignored; only a
    /// different `HEAD` counts.
    Dirty,
    /// The submodule never counts as modified.
    All,
}

/// Parse an `ignore` value (`none`, `untracked`, `dirty`, `all`; exact case).
pub fn parse_ignore(value: &[u8]) -> Option<Ignore> {
    match value {
        b"none" => Some(Ignore::None),
        b"untracked" => Some(Ignore::Untracked),
        b"dirty" => Some(Ignore::Dirty),
        b"all" => Some(Ignore::All),
        _ => None,
    }
}

impl Ignore {
    /// The value as written in configuration.
    pub fn as_str(self) -> &'static str {
        match self {
            Ignore::None => "none",
            Ignore::Untracked => "untracked",
            Ignore::Dirty => "dirty",
            Ignore::All => "all",
        }
    }
}

/// `submodule.<name>.update`: how `git submodule update` updates it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Update {
    Checkout,
    Rebase,
    Merge,
    None,
    /// `!<command>`; only allowed in the repository configuration, never in
    /// `.gitmodules`.
    Command(BString),
}

/// Parse an `update` value; `!<command>` only when `allow_command`.
pub fn parse_update(value: &[u8], allow_command: bool) -> Option<Update> {
    match value {
        b"checkout" => Some(Update::Checkout),
        b"rebase" => Some(Update::Rebase),
        b"merge" => Some(Update::Merge),
        b"none" => Some(Update::None),
        _ => match value.strip_prefix(b"!") {
            Some(cmd) if allow_command => Some(Update::Command(cmd.into())),
            _ => None,
        },
    }
}

/// `submodule.<name>.fetchRecurseSubmodules`, `fetch.recurseSubmodules`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FetchRecurse {
    No,
    Yes,
    OnDemand,
}

/// Parse a `fetchRecurseSubmodules` value: `on-demand` or a boolean (a
/// missing value is true).
pub fn parse_fetch_recurse(value: Option<&[u8]>) -> Option<FetchRecurse> {
    if value == Some(b"on-demand") {
        return Some(FetchRecurse::OnDemand);
    }
    parse_bool(value).map(|b| {
        if b {
            FetchRecurse::Yes
        } else {
            FetchRecurse::No
        }
    })
}

/// One `[submodule "<name>"]` section of `.gitmodules`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Submodule {
    pub name: BString,
    pub path: Option<BString>,
    pub url: Option<BString>,
    pub ignore: Option<Ignore>,
    pub update: Option<Update>,
    pub branch: Option<BString>,
    pub fetch_recurse: Option<FetchRecurse>,
    pub shallow: Option<bool>,
}

/// How strictly to parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The `.gitmodules` of the work tree, the index or `HEAD`: syntax
    /// errors and bad values are fatal, a later value replaces an earlier one.
    Current,
    /// A `.gitmodules` blob from history: a syntax error ends the parse but
    /// keeps what came before it, a bad `fetchRecurseSubmodules` is skipped,
    /// and a repeated value is skipped with a warning.
    History,
}

/// A non-fatal message produced while parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diagnostic {
    /// Print as `warning: <text>`.
    Warning(String),
    /// Print as `error: <text>`.
    Error(String),
}

/// Parsing failed: print the diagnostics, then `fatal: <fatal>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub diagnostics: Vec<Diagnostic>,
    pub fatal: String,
}

/// Where a `.gitmodules` came from, for messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// A file; the path as it should be printed.
    File(String),
    /// A blob, named `<object-id>:.gitmodules`.
    Blob(String),
}

impl Origin {
    fn describe(&self) -> String {
        match self {
            Origin::File(p) => format!("file {p}"),
            Origin::Blob(b) => format!("submodule-blob {b}"),
        }
    }

    fn label(&self) -> &str {
        match self {
            Origin::File(p) | Origin::Blob(p) => p,
        }
    }
}

/// The submodules described by a `.gitmodules` file, in order of first
/// appearance.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Gitmodules {
    modules: Vec<Submodule>,
    /// path -> index into `modules`, latest assignment wins.
    paths: Vec<(BString, usize)>,
}

impl Gitmodules {
    /// Parse a `.gitmodules` file. Sections without a name and keys this
    /// library does not know are ignored; sections with an invalid name are
    /// ignored with a warning, as are `url` and `path` values that look like
    /// options.
    pub fn parse(
        data: &[u8],
        origin: &crate::config::Origin,
        mode: Mode,
    ) -> Result<(Gitmodules, Vec<Diagnostic>), ParseError> {
        let mut out = Gitmodules::default();
        let mut diags = Vec::new();
        let mut headers: Vec<Option<SectionHeader>> = Vec::new();
        for item in Parser::new(data) {
            let item = match item {
                Ok(item) => item,
                Err(e) => {
                    let msg = messages::bad_config_line(e.line, &origin.describe());
                    return match mode {
                        Mode::Current => Err(ParseError {
                            diagnostics: diags,
                            fatal: msg,
                        }),
                        Mode::History => {
                            diags.push(Diagnostic::Error(msg));
                            Ok((out, diags))
                        }
                    };
                }
            };
            let entry = match item {
                Item::Section(h) => {
                    headers.push(Some(h));
                    continue;
                }
                Item::Trivia(_) => {
                    headers.push(None);
                    continue;
                }
                Item::Entry(e) => {
                    headers.push(None);
                    e
                }
            };
            let Some(Some(header)) = entry.header.map(|i| &headers[i]) else {
                continue;
            };
            if header.section() != "submodule" {
                continue;
            }
            let Some(name) = header.effective_subsection() else {
                continue;
            };
            let key = entry.key.to_ascii_lowercase();
            let key = key.as_bstr();
            match out.apply(
                &name,
                key,
                entry.value.as_deref().map(|v| &**v),
                origin,
                mode,
                &mut diags,
            ) {
                Ok(()) => {}
                Err(Failure::Fatal(fatal)) => {
                    return Err(ParseError {
                        diagnostics: diags,
                        fatal,
                    });
                }
                Err(Failure::BadLine(err)) => {
                    diags.push(Diagnostic::Error(err));
                    let msg = messages::bad_config_line(entry.line, &origin.describe());
                    return match mode {
                        Mode::Current => Err(ParseError {
                            diagnostics: diags,
                            fatal: msg,
                        }),
                        Mode::History => {
                            diags.push(Diagnostic::Error(msg));
                            Ok((out, diags))
                        }
                    };
                }
            }
        }
        Ok((out, diags))
    }

    fn apply(
        &mut self,
        name: &BString,
        key: &BStr,
        value: Option<&[u8]>,
        origin: &Origin,
        mode: Mode,
        diags: &mut Vec<Diagnostic>,
    ) -> Result<(), Failure> {
        const KEYS: &[&str] = &[
            "path",
            "url",
            "ignore",
            "update",
            "branch",
            "fetchrecursesubmodules",
            "shallow",
        ];
        let Some(&key) = KEYS.iter().find(|k| k.as_bytes() == key.as_bytes()) else {
            return Ok(());
        };
        if !check_name(name) {
            diags.push(Diagnostic::Warning(messages::suspicious_name(
                name.as_bstr(),
            )));
            return Ok(());
        }
        let n = name.as_bstr();
        let needs_value = !matches!(key, "fetchrecursesubmodules" | "shallow");
        let value = match value {
            Some(v) => Some(v.as_bstr()),
            None if needs_value => return Err(Failure::BadLine(messages::missing_value(n, key))),
            None => None,
        };
        if matches!(key, "path" | "url") && value.is_some_and(|v| looks_like_option(v)) {
            diags.push(Diagnostic::Warning(messages::looks_like_option(
                n,
                key,
                value.unwrap_or_default(),
            )));
            return Ok(());
        }
        let idx = self.index_or_insert(name);
        let already = {
            let m = &self.modules[idx];
            match key {
                "path" => m.path.is_some(),
                "url" => m.url.is_some(),
                "ignore" => m.ignore.is_some(),
                "update" => m.update.is_some(),
                "branch" => m.branch.is_some(),
                "fetchrecursesubmodules" => m.fetch_recurse.is_some(),
                _ => m.shallow.is_some(),
            }
        };
        if mode == Mode::History && already {
            diags.push(Diagnostic::Warning(messages::multiple_configurations(
                origin.label(),
                n,
                key,
            )));
            return Ok(());
        }
        let v = value.unwrap_or_default();
        match key {
            "path" => {
                self.paths.retain(|(p, i)| *i != idx && p != v);
                self.paths.push((v.to_owned(), idx));
                self.modules[idx].path = Some(v.to_owned());
            }
            "url" => self.modules[idx].url = Some(v.to_owned()),
            "ignore" => match parse_ignore(v) {
                Some(i) => self.modules[idx].ignore = Some(i),
                None => diags.push(Diagnostic::Warning(messages::invalid_ignore(v, n))),
            },
            "update" => match parse_update(v, false) {
                Some(u) => self.modules[idx].update = Some(u),
                None => return Err(Failure::Fatal(messages::invalid_update(n))),
            },
            "branch" => self.modules[idx].branch = Some(v.to_owned()),
            "fetchrecursesubmodules" => match parse_fetch_recurse(value.map(|v| v.as_bytes())) {
                Some(f) => self.modules[idx].fetch_recurse = Some(f),
                None if mode == Mode::History => {}
                None => return Err(Failure::Fatal(messages::bad_fetch_recurse(n, v))),
            },
            _ => match parse_bool(value.map(|v| v.as_bytes())) {
                Some(b) => self.modules[idx].shallow = Some(b),
                None => return Err(Failure::Fatal(messages::bad_bool(v, n, key))),
            },
        }
        Ok(())
    }

    fn index_or_insert(&mut self, name: &BString) -> usize {
        if let Some(i) = self.modules.iter().position(|m| &m.name == name) {
            return i;
        }
        self.modules.push(Submodule {
            name: name.clone(),
            ..Submodule::default()
        });
        self.modules.len() - 1
    }

    /// The submodule called `name`.
    pub fn by_name(&self, name: &[u8]) -> Option<&Submodule> {
        self.modules.iter().find(|m| m.name == name)
    }

    /// The submodule whose `path` is `path` (the latest one to claim it).
    pub fn by_path(&self, path: &[u8]) -> Option<&Submodule> {
        self.paths
            .iter()
            .find(|(p, _)| p == path)
            .map(|&(_, i)| &self.modules[i])
    }

    /// All submodules in order of first appearance.
    pub fn iter(&self) -> impl Iterator<Item = &Submodule> {
        self.modules.iter()
    }

    /// Number of submodules.
    pub fn len(&self) -> usize {
        self.modules.len()
    }

    /// Whether no submodule is described.
    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }
}

enum Failure {
    /// Die with this message.
    Fatal(String),
    /// Report this error, then the bad config line.
    BadLine(String),
}
