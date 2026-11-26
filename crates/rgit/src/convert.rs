// SPDX-License-Identifier: MIT
//! The blob form of work tree files for comparisons (gitattributes(5),
//! "Checking-out and checking-in"): line endings, `ident` and
//! `working-tree-encoding` through `rgit-convert`.
//!
//! Filter drivers (`filter=<name>` with a `clean` or `process` command)
//! are not run by the library yet: [`Filter::unsupported`] names the first
//! file that would have needed one, and the operation reports
//! [`Error::Unsupported`](crate::Error::Unsupported) instead of a
//! comparison that could be wrong.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use bstr::BString;
use rgit_attr::{AttrId, Attributes, Order, Source};
use rgit_config::{ConfigSet, Environment};
use rgit_convert::{AutoCrlf, Config, Conversion, Eol, Options, SafeCrlf, Stats};
use rgit_index::{Index, Stage};
use rgit_object::ObjectKind;
use rgit_odb::ObjectDatabase;
use rgit_worktree::stat::ContentFilter;

use crate::{Error, Repository, Result};

/// The compiled-in system attributes file.
const SYSTEM_ATTRIBUTES: &str = "/etc/gitattributes";

/// `core.autocrlf`, `core.eol`, `core.safecrlf` and
/// `core.checkRoundtripEncoding`.
fn config(cfg: &ConfigSet) -> Result<Config> {
    let mut out = Config::default();
    if let Some(e) = cfg.get("core.autocrlf") {
        out.autocrlf = if e.value.as_deref().map(|v| v.as_slice()) == Some(b"input".as_slice()) {
            AutoCrlf::Input
        } else if e.bool().map_err(Error::config)? {
            AutoCrlf::True
        } else {
            AutoCrlf::False
        };
    }
    if let Ok(Some(v)) = cfg.string("core.eol") {
        match v.to_ascii_lowercase().as_slice() {
            b"native" => out.eol = Eol::native(),
            v => {
                if let Some(e) = Eol::parse(v) {
                    out.eol = e;
                }
            }
        }
    }
    if let Some(e) = cfg.get("core.safecrlf") {
        out.safecrlf = if e.value.as_deref().map(|v| v.as_slice()) == Some(b"warn".as_slice()) {
            SafeCrlf::Warn
        } else if e.bool().map_err(Error::config)? {
            SafeCrlf::Fail
        } else {
            SafeCrlf::False
        };
    }
    if let Ok(Some(v)) = cfg.string("core.checkroundtripencoding") {
        out.roundtrip = String::from_utf8_lossy(v.as_ref())
            .split(|c: char| c == ',' || c.is_ascii_whitespace())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
    }
    Ok(out)
}

/// The attribute files outside the work tree.
fn attr_options(repo: &Repository) -> rgit_attr::Options {
    let env: &dyn Environment = &*repo.env;
    let shown = |p: PathBuf| {
        let s = p.display().to_string();
        (p, s)
    };
    let var = |name: &str| env.var(name).filter(|v| !v.is_empty());
    let nosystem = env
        .var("GIT_ATTR_NOSYSTEM")
        .and_then(|v| rgit_config::value::parse_bool(Some(v.as_encoded_bytes())))
        .unwrap_or(false);
    let global = match repo.config.path("core.attributesfile", env) {
        Ok(Some(p)) => Some(p),
        _ => var("XDG_CONFIG_HOME")
            .map(|x| PathBuf::from(x).join("git").join("attributes"))
            .or_else(|| var("HOME").map(|h| PathBuf::from(h).join(".config/git/attributes"))),
    };
    rgit_attr::Options {
        system: (!nosystem).then(|| shown(PathBuf::from(SYSTEM_ATTRIBUTES))),
        global: global.map(shown),
        info: Some(shown(repo.common_dir().join("info").join("attributes"))),
        ignore_case: repo
            .config
            .bool("core.ignorecase")
            .ok()
            .flatten()
            .unwrap_or(false),
    }
}

/// The filter drivers that change content on the way into the
/// repository: `filter.<name>.clean` or `filter.<name>.process` is set.
fn clean_drivers(cfg: &ConfigSet) -> HashSet<Vec<u8>> {
    let mut out = HashSet::new();
    for name in cfg.subsections("filter") {
        let has = |key: &str| {
            let full = [b"filter.", name.as_slice(), b".", key.as_bytes()].concat();
            cfg.string(full)
                .ok()
                .flatten()
                .is_some_and(|v| !v.is_empty())
        };
        if has("clean") || has("process") {
            out.insert(name.to_vec());
        }
    }
    out
}

/// Converts work tree content to its blob form for one operation.
pub(crate) struct Filter {
    attrs: RefCell<Attributes<'static>>,
    ids: [AttrId; 6],
    cfg: Config,
    drivers: HashSet<Vec<u8>>,
    index: Rc<Index>,
    odb: Arc<ObjectDatabase>,
    unsupported: RefCell<Option<BString>>,
}

impl Filter {
    /// For comparing the work tree of `repo` with `index`:
    /// `.gitattributes` from the work tree, else from the index.
    pub(crate) fn new(repo: &Repository, index: Rc<Index>) -> Result<Filter> {
        let source = Source::Snapshot {
            root: repo.work_dir().map(PathBuf::from),
            index: rgit_attr::snapshot(&index, &*repo.odb),
            order: Order::WorkTreeFirst,
        };
        let mut attrs = Attributes::new(&attr_options(repo), source);
        let ids = rgit_convert::ATTRIBUTES.map(|name| attrs.id(name));
        Ok(Filter {
            attrs: RefCell::new(attrs),
            ids,
            cfg: config(&repo.config)?,
            drivers: clean_drivers(&repo.config),
            index,
            odb: Arc::clone(&repo.odb),
            unsupported: RefCell::new(None),
        })
    }

    fn conversion(&self, path: &[u8]) -> Conversion {
        let mut attrs = self.attrs.borrow_mut();
        let out = attrs.lookup(path, false);
        let found = rgit_convert::Attrs::from_states(self.ids.map(|id| out.get(id)));
        Conversion::new(found, &self.cfg)
    }

    fn has_driver(&self, conv: &Conversion) -> bool {
        conv.attrs
            .filter
            .as_ref()
            .is_some_and(|name| self.drivers.contains(name))
    }

    /// Whether the blob of `path` in the index is text with CR LF line
    /// endings (then `text=auto` leaves the file alone).
    fn index_has_crlf(&self, path: &[u8]) -> bool {
        let Some(entry) = self.index.get(path, Stage::MERGED) else {
            return false;
        };
        match self.odb.read(&entry.id) {
            Ok(Some(raw)) if raw.kind == ObjectKind::Blob => {
                Stats::new(&raw.data).is_text_with_crlf()
            }
            _ => false,
        }
    }

    /// The first file whose comparison needed a filter driver.
    pub(crate) fn unsupported(&self) -> Option<BString> {
        self.unsupported.borrow().clone()
    }
}

impl ContentFilter for Filter {
    fn to_git(&self, path: &[u8], data: Vec<u8>) -> Vec<u8> {
        let conv = self.conversion(path);
        if self.has_driver(&conv) {
            self.unsupported
                .borrow_mut()
                .get_or_insert_with(|| path.into());
            return data;
        }
        if conv.is_identity() {
            return data;
        }
        let shown = String::from_utf8_lossy(path);
        // Quietly, as for every comparison: a problem leaves the content
        // as it is and the file shows up as modified.
        rgit_convert::to_git(
            &shown,
            data,
            &conv,
            &self.cfg,
            Options::QUIET,
            || self.index_has_crlf(path),
            &mut |_| {},
        )
        .unwrap_or_default()
    }

    fn converts(&self, path: &[u8]) -> bool {
        let conv = self.conversion(path);
        !conv.is_identity() || self.has_driver(&conv)
    }
}
