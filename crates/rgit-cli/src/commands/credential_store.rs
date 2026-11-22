// SPDX-License-Identifier: MIT
//! `git credential-store [--file=<path>] (get|store|erase)`
//! (Documentation/git-credential-store.adoc). The file format and the
//! matching rules live in `rgit_transport::credential::store`.
//!
//! Behaviour observed with git 2.56:
//!
//! - Without `--file`, `get` reads `~/.git-credentials`, then
//!   `$XDG_CONFIG_HOME/git/credentials` (`~/.config/git/credentials` when
//!   unset or empty), first match wins; `store` writes to the first of them
//!   that exists (else `~/.git-credentials`) and only replaces matches in
//!   that file; `erase` rewrites every existing file.
//! - Missing and unreadable files count as empty; other read errors are
//!   fatal. A rewritten file gets mode 0600.
//! - The file is rewritten through `<file>.lock`, retried for one second.
//! - Unknown actions are ignored; a missing action or extra arguments are
//!   a usage error.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rgit_fs::{LockError, LockFile, LockOptions, Timeout};
use rgit_transport::credential::{Credential, read_helper_request, store};
use rgit_transport::messages as msg;

use crate::global::strerror;
use crate::prelude::*;

const LOCK_TIMEOUT_MS: u64 = 1000;

pub fn run(_ctx: &mut Context, argv: &[String]) -> CmdResult {
    let file: RefCell<Option<String>> = RefCell::new(None);
    let mut parser = Parser::new(
        &[msg::CREDENTIAL_STORE_USAGE],
        vec![Opt::string(None, "file", &file, "path", msg::CREDENTIAL_STORE_HELP_FILE)],
    );
    let args = parser.parse(argv)?.args;
    if args.len() != 1 {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let action = args[0].as_str();
    let want = read_helper_request(&mut io::stdin().lock()).map_err(|e| Die::Fatal(e.to_string()))?;

    let explicit = file.borrow().clone().map(PathBuf::from);
    let files = match explicit {
        Some(f) => vec![f],
        None => default_files(),
    };
    match action {
        "get" => get(&files, &want),
        "store" => {
            let Some(line) = store::format_line(&want) else {
                return Ok(0);
            };
            let target = files
                .iter()
                .find(|f| f.exists())
                .or(files.first())
                .cloned();
            if let Some(target) = target {
                rewrite(&target, Some(&line), &want, false)?;
            }
            Ok(0)
        }
        "erase" => {
            for f in files.iter().filter(|f| f.exists()) {
                rewrite(f, None, &want, true)?;
            }
            Ok(0)
        }
        _ => Ok(0),
    }
}

/// `~/.git-credentials` and the XDG file, in lookup order.
fn default_files() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty());
    let mut files = Vec::new();
    if let Some(h) = &home {
        files.push(Path::new(h).join(".git-credentials"));
    }
    match std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        Some(x) => files.push(Path::new(&x).join("git/credentials")),
        None => {
            if let Some(h) = &home {
                files.push(Path::new(h).join(".config/git/credentials"));
            }
        }
    }
    files
}

/// The content of a credential file; missing or unreadable files are
/// empty.
fn read_file(path: &Path) -> Result<Vec<u8>, Die> {
    let mut content = Vec::new();
    let result = std::fs::File::open(path).and_then(|mut f| f.read_to_end(&mut content));
    match result {
        Ok(_) => Ok(content),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
            ) =>
        {
            Ok(Vec::new())
        }
        Err(e) => Err(Die::Fatal(msg::store_unable_to_open(
            &path.display().to_string(),
            &strerror(&e),
        ))),
    }
}

fn get(files: &[PathBuf], want: &Credential) -> CmdResult {
    for f in files {
        let content = read_file(f)?;
        if let Some(have) = store::find(&content, want) {
            let mut out = io::stdout().lock();
            let _ = out.write_all(&store::answer(&have));
            let _ = out.flush();
            return Ok(0);
        }
    }
    Ok(0)
}

fn rewrite(
    path: &Path,
    first: Option<&[u8]>,
    want: &Credential,
    match_password: bool,
) -> Result<(), Die> {
    let options =
        LockOptions::new().timeout(Timeout::After(Duration::from_millis(LOCK_TIMEOUT_MS)));
    let mut lock = LockFile::acquire(path, &options).map_err(|e| {
        let reason = match &e {
            LockError::Exists { .. } => strerror(&io::Error::from_raw_os_error(EEXIST)),
            LockError::Io { source, .. } => strerror(source),
        };
        Die::Fatal(msg::store_lock_failed(LOCK_TIMEOUT_MS, &reason))
    })?;
    let content = read_file(path)?;
    let new = store::rewrite(&content, first, want, match_password);
    let write_err = |e: io::Error| {
        Die::Fatal(msg::store_unable_to_write(
            &path.display().to_string(),
            &strerror(&e),
        ))
    };
    restrict_mode(lock.file_mut()).map_err(write_err)?;
    lock.write_all(&new).map_err(write_err)?;
    lock.commit().map_err(write_err)
}

#[cfg(unix)]
const EEXIST: i32 = libc::EEXIST;
#[cfg(not(unix))]
const EEXIST: i32 = 17;

#[cfg(unix)]
fn restrict_mode(f: &mut std::fs::File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    f.set_permissions(std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_mode(_f: &mut std::fs::File) -> io::Result<()> {
    Ok(())
}
