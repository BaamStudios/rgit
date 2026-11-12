// SPDX-License-Identifier: MIT
//! Template directories (`git-init(1)`, "TEMPLATE DIRECTORY") and rgit's
//! own built-in default template.
//!
//! The default template is rgit's own (CLEANROOM.md, "Sample hooks and
//! `init` templates"): a `description`, an `info/exclude` and an empty
//! `hooks/` directory, no sample hooks.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rgit_config::{Origin, Scope};
use rgit_fs::SharedRepository;

use super::{InitError, Notice};
use crate::error::io_text;
use crate::format::RepositoryFormat;
use crate::messages;

/// Where the template files come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Template {
    /// rgit's built-in default template.
    Builtin,
    /// No template (`--template=` with an empty value).
    Empty,
    /// Copy this directory (absolute, or relative to the current directory
    /// of the caller).
    Dir(PathBuf),
}

/// Content of the built-in template: (path, content); a path ending in `/`
/// is a directory.
const BUILTIN: &[(&str, &str)] = &[
    (
        "description",
        "Unnamed repository; describe it here (used by repository browsers).\n",
    ),
    ("hooks/", ""),
    ("info/", ""),
    (
        "info/exclude",
        "# Patterns in this file are ignored in this repository only, like\n\
         # entries of a .gitignore file that is not committed (see gitignore(5)).\n\
         # Example:\n\
         # *.tmp\n",
    ),
];

/// Copy the template into `dest` (the common directory). Files and links
/// that already exist are kept.
pub(super) fn copy(
    template: &Template,
    dest: &Path,
    shared: SharedRepository,
    notify: &mut dyn FnMut(Notice),
) -> Result<(), InitError> {
    match template {
        Template::Empty => Ok(()),
        Template::Builtin => copy_builtin(dest, shared),
        Template::Dir(dir) => {
            if dir.as_os_str().is_empty() {
                return Ok(());
            }
            let entries = match fs::read_dir(dir) {
                Ok(e) => e,
                Err(_) => {
                    notify(Notice::Warning(messages::templates_not_found(
                        dir.display(),
                    )));
                    return Ok(());
                }
            };
            drop(entries);
            if let Some(reason) = refuse_template(dir)? {
                notify(Notice::Warning(messages::not_copying_templates(
                    dir.display(),
                    reason,
                )));
                return Ok(());
            }
            copy_dir(dir, dest, shared)
        }
    }
}

/// A template whose `config` declares a repository format rgit (like git)
/// would not understand is not copied at all; the reason is returned.
fn refuse_template(dir: &Path) -> Result<Option<String>, InitError> {
    let path = dir.join("config");
    let Ok(data) = fs::read(&path) else {
        return Ok(None);
    };
    let set = rgit_config::parse_bytes(&data, Origin::File(path), Scope::Local)?;
    let format = match RepositoryFormat::from_config(&set) {
        Ok(f) => f,
        Err(e) => return Ok(Some(e.to_string())),
    };
    Ok(format.check().err().map(|e| e.to_string()))
}

fn copy_builtin(dest: &Path, shared: SharedRepository) -> Result<(), InitError> {
    for (name, content) in BUILTIN {
        if let Some(dir) = name.strip_suffix('/') {
            make_dir(&dest.join(dir), shared)?;
        } else {
            let target = dest.join(name);
            if fs::symlink_metadata(&target).is_ok() {
                continue;
            }
            write_new(&target, content.as_bytes(), false, shared)?;
        }
    }
    Ok(())
}

fn copy_dir(src: &Path, dest: &Path, shared: SharedRepository) -> Result<(), InitError> {
    let fail = |e: io::Error| {
        InitError::Fatal(messages::cannot_copy_template(
            src.display(),
            dest.display(),
            io_text(&e),
        ))
    };
    let entries: Vec<_> = fs::read_dir(src)
        .map_err(fail)?
        .filter_map(Result::ok)
        .collect();
    for entry in entries {
        let name = entry.file_name();
        if name.as_encoded_bytes().first() == Some(&b'.') {
            continue;
        }
        let from = entry.path();
        let to = dest.join(&name);
        let fail = |e: io::Error| {
            InitError::Fatal(messages::cannot_copy_template(
                from.display(),
                to.display(),
                io_text(&e),
            ))
        };
        let meta = fs::symlink_metadata(&from).map_err(fail)?;
        if meta.is_dir() {
            make_dir(&to, shared)?;
            copy_dir(&from, &to, shared)?;
            continue;
        }
        if fs::symlink_metadata(&to).is_ok() {
            continue;
        }
        if meta.file_type().is_symlink() {
            let target = fs::read_link(&from).map_err(fail)?;
            symlink(&target, &to).map_err(fail)?;
        } else if meta.is_file() {
            let data = fs::read(&from).map_err(fail)?;
            write_new(&to, &data, is_executable(&meta), shared)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn is_executable(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_: &fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn symlink(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// Create the directory `path` unless it exists; a new directory gets the
/// shared permissions.
pub(super) fn make_dir(path: &Path, shared: SharedRepository) -> Result<(), InitError> {
    match fs::create_dir(path) {
        Ok(()) => shared.apply(path).map_err(|e| mkdir_error(path, e)),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(e) => Err(mkdir_error(path, e)),
    }
}

fn mkdir_error(path: &Path, e: io::Error) -> InitError {
    InitError::Fatal(messages::cannot_mkdir(path.display(), io_text(&e)))
}

/// Create the new file `path` (mode `0666`, or `0777` when `executable`,
/// reduced by the umask) with `data`, then apply the shared permissions.
pub(super) fn write_new(
    path: &Path,
    data: &[u8],
    executable: bool,
    shared: SharedRepository,
) -> Result<(), InitError> {
    use std::io::Write as _;
    let fail = |e: io::Error| InitError::Fatal(messages::cannot_write(path.display(), io_text(&e)));
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(if executable { 0o777 } else { 0o666 });
    }
    #[cfg(not(unix))]
    let _ = executable;
    let mut file = opts.open(path).map_err(fail)?;
    file.write_all(data).map_err(fail)?;
    drop(file);
    shared.apply(path).map_err(fail)
}
