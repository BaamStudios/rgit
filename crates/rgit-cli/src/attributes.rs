// SPDX-License-Identifier: MIT
//! Reading attributes in commands: which files and which tree
//! `rgit_attr::Attributes` reads for a repository, and the computed
//! `builtin_objectmode` attribute.
//!
//! Tree source precedence (gitattributes(5), git(1) `--attr-source`,
//! git-config(1) `attr.tree`, checked against the `git` binary): a
//! command's own `--source`, then `--attr-source` / `GIT_ATTR_SOURCE`
//! (an unknown name is fatal), then `attr.tree` (an unknown name is
//! ignored). A name that resolves to something other than a tree-ish
//! stands for the empty tree.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use rgit_attr::{Attributes, Options, Order, Source};
use rgit_config::SystemEnv;
use rgit_hash::ObjectId;
use rgit_index::{Index, Stage};
use rgit_object::ObjectKind;

use crate::messages::check_attr as msg;
use crate::repository::Repo;
use crate::{Context, Die};

/// The compiled-in system attributes file.
const SYSTEM_ATTRIBUTES: &str = "/etc/gitattributes";

/// The files outside the work tree and `core.ignoreCase`.
pub fn options(repo: &Repo) -> Options {
    let env = SystemEnv::default();
    let nosystem = std::env::var_os("GIT_ATTR_NOSYSTEM")
        .and_then(|v| rgit_config::value::parse_bool(Some(v.as_encoded_bytes())))
        .unwrap_or(false);
    let system = (!nosystem).then(|| shown(PathBuf::from(SYSTEM_ATTRIBUTES)));
    let global = match repo.config.path("core.attributesfile", &env) {
        Ok(Some(p)) => Some(p),
        _ => default_global(|k| std::env::var_os(k)),
    }
    .map(shown);
    let info = Some(shown(repo.setup.common_dir.join("info").join("attributes")));
    Options {
        system,
        global,
        info,
        ignore_case: repo
            .config
            .bool("core.ignorecase")
            .ok()
            .flatten()
            .unwrap_or(false),
    }
}

fn shown(p: PathBuf) -> (PathBuf, String) {
    let s = p.display().to_string();
    (p, s)
}

/// `$XDG_CONFIG_HOME/git/attributes`, else `$HOME/.config/git/attributes`.
fn default_global(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    if let Some(x) = env("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(x).join("git").join("attributes"));
    }
    let home = env("HOME").filter(|v| !v.is_empty())?;
    Some(PathBuf::from(home).join(".config/git/attributes"))
}

/// The tree attributes are read from instead of the work tree, if any:
/// `Some(None)` is the empty tree. `source` is a command's `--source`.
pub fn tree_source(
    ctx: &Context,
    repo: &mut Repo,
    source: Option<&str>,
) -> Result<Option<Option<ObjectId>>, Die> {
    if let Some(name) = source {
        let id = repo
            .resolve(name)
            .ok_or_else(|| Die::Fatal(msg::invalid_source(name)))?;
        return Ok(Some(repo.peel_to(id, ObjectKind::Tree)));
    }
    let env = ctx
        .attr_source
        .clone()
        .or_else(|| std::env::var("GIT_ATTR_SOURCE").ok());
    if let Some(name) = env {
        let id = repo
            .resolve(&name)
            .ok_or_else(|| Die::Fatal(msg::BAD_ATTR_SOURCE.into()))?;
        return Ok(Some(repo.peel_to(id, ObjectKind::Tree)));
    }
    if let Ok(Some(name)) = repo.config.string("attr.tree") {
        let name = name.to_string();
        if let Some(id) = repo.resolve(&name) {
            return Ok(Some(repo.peel_to(id, ObjectKind::Tree)));
        }
    }
    Ok(None)
}

/// The attributes of `repo` as git's porcelain reads them: from `tree`
/// (see [`tree_source`]) when given, else from the work tree with the
/// index as fall-back, else (bare) from the top-level files only. With
/// `cached`, `.gitattributes` come from the index alone.
pub fn open<'a>(
    repo: &'a Repo,
    index: &'a Index,
    tree: Option<Option<ObjectId>>,
    cached: bool,
) -> Attributes<'a> {
    let odb = repo.odb();
    let source = if cached {
        Source::Index(index, odb)
    } else if let Some(tree) = tree {
        Source::Tree(tree, odb)
    } else if let Some(root) = &repo.setup.work_tree {
        Source::WorkTree {
            root: root.clone(),
            index: Some((index, odb)),
            order: Order::WorkTreeFirst,
        }
    } else {
        Source::None
    };
    Attributes::new(&options(repo), source)
}

/// `builtin_objectmode` of `path` (relative to the top, no trailing `/`):
/// from the index entry with `cached` (`None` without one), else from the
/// file in the work tree (`root`), whose absence is fatal.
pub fn object_mode(
    root: &Path,
    index: &Index,
    path: &[u8],
    cached: bool,
) -> Result<Option<String>, Die> {
    if cached {
        return Ok(index
            .get(path, Stage::MERGED)
            .map(|e| format!("{:06o}", e.mode.0)));
    }
    let full = root.join(path_from_bytes(path));
    let meta = std::fs::symlink_metadata(&full).map_err(|e| {
        let err = e.to_string();
        let err = err.split(" (os error").next().unwrap_or_default();
        Die::Fatal(msg::unable_to_stat(&String::from_utf8_lossy(path), err))
    })?;
    let ft = meta.file_type();
    let mode = if ft.is_symlink() {
        0o120000
    } else if ft.is_dir() {
        if crate::repository::submodule_head(&full, index.hash_kind()).is_some() {
            0o160000
        } else {
            0o040000
        }
    } else if is_executable(&meta) {
        0o100755
    } else {
        0o100644
    };
    Ok(Some(format!("{mode:06o}")))
}

/// Evaluates the `attr:` magic of pathspecs for a command: the
/// attributes read lazily on first use, `builtin_objectmode` from the
/// work tree file, else from the index entry.
pub struct PathspecAttrs<'a> {
    repo: &'a Repo,
    index: &'a Index,
    tree: Option<Option<ObjectId>>,
    attrs: Option<Attributes<'a>>,
}

impl<'a> PathspecAttrs<'a> {
    /// The tree source for `pathspec` (see [`tree_source`]), `None` when it
    /// has no `attr:` magic. Fails like git when `--attr-source` /
    /// `GIT_ATTR_SOURCE` names no object.
    #[allow(clippy::type_complexity)]
    pub fn prepare(
        ctx: &Context,
        repo: &mut Repo,
        pathspec: &rgit_pathspec::Pathspec,
    ) -> Result<Option<Option<Option<ObjectId>>>, Die> {
        if !pathspec.items().iter().any(|i| i.attr().is_some()) {
            return Ok(None);
        }
        Ok(Some(tree_source(ctx, repo, None)?))
    }

    /// The evaluator; `tree` from [`PathspecAttrs::prepare`].
    pub fn new(repo: &'a Repo, index: &'a Index, tree: Option<Option<ObjectId>>) -> Self {
        PathspecAttrs {
            repo,
            index,
            tree,
            attrs: None,
        }
    }

    /// Whether `path` meets the requirements of `spec`.
    pub fn check(&mut self, path: &[u8], spec: &rgit_pathspec::AttrSpec) -> bool {
        let (repo, index, tree) = (self.repo, self.index, self.tree);
        let attrs = self
            .attrs
            .get_or_insert_with(|| open(repo, index, tree, false));
        let out = attrs.lookup(path, false);
        for w in attrs.take_warnings() {
            eprintln!("{w}");
        }
        spec.requirements.iter().all(|r| {
            if r.name() == rgit_attr::BUILTIN_OBJECTMODE {
                let mode = repo
                    .setup
                    .work_tree
                    .as_deref()
                    .and_then(|root| object_mode(root, index, path, false).ok().flatten())
                    .or_else(|| object_mode(Path::new(""), index, path, true).ok().flatten());
                let state = mode.map_or(rgit_attr::State::Unspecified, |m| {
                    rgit_attr::State::Value(m.into_bytes())
                });
                rgit_attr::requirement_met(r, &state)
            } else {
                let id = attrs.id(r.name());
                rgit_attr::requirement_met(r, out.get(id))
            }
        })
    }
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_: &std::fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn path_from_bytes(b: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(b))
}

#[cfg(not(unix))]
fn path_from_bytes(b: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(b).into_owned())
}
