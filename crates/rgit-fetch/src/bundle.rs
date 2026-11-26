// SPDX-License-Identifier: MIT
//! Reading bundle files into a repository (`git bundle verify|unbundle`,
//! a bundle as fetch and clone source, bundle URIs).
//!
//! The header is parsed by `rgit_bundle`; here the prerequisites are
//! checked against the local repository and the pack is stored through
//! index-pack (`--fix-thin`, kept until the refs are written), like a
//! fetched pack. A filtered bundle (`@filter`) gets a `.promisor` file.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use rgit_bundle::{Header, HeaderError};
use rgit_hash::ObjectId;
use rgit_protocol::v2::RemoteRef;

use crate::repo::LocalRepo;
use crate::session::{self, FsckCheck, ReceivedPack};
use crate::{Error, Result, graph};

/// An open bundle: the header, and the reader at the start of the pack.
pub struct OpenBundle {
    pub header: Header,
    pub reader: Box<dyn BufRead + Send>,
}

/// Why a bundle could not be opened.
#[derive(Debug)]
pub enum OpenError {
    /// The file could not be opened (`error: could not open '<path>'`).
    CannotOpen(io::Error),
    /// A header problem (see [`HeaderError`]).
    Header(HeaderError),
}

/// Open the bundle at `path` and read its header.
pub fn open(path: &Path) -> std::result::Result<OpenBundle, OpenError> {
    let file = File::open(path).map_err(OpenError::CannotOpen)?;
    if file.metadata().is_ok_and(|m| m.is_dir()) {
        return Err(OpenError::CannotOpen(io::Error::from(
            io::ErrorKind::IsADirectory,
        )));
    }
    open_reader(Box::new(BufReader::with_capacity(1 << 16, file)))
}

/// Read the header from `reader` (standard input, a downloaded file).
pub fn open_reader(
    mut reader: Box<dyn BufRead + Send>,
) -> std::result::Result<OpenBundle, OpenError> {
    let header = rgit_bundle::read_header(&mut reader).map_err(OpenError::Header)?;
    Ok(OpenBundle { header, reader })
}

/// Whether the file at `path` starts with a bundle signature.
pub fn is_bundle_file(path: &Path) -> bool {
    let Ok(mut f) = File::open(path) else {
        return false;
    };
    if !f.metadata().is_ok_and(|m| m.is_file()) {
        return false;
    }
    let mut buf = [0u8; 16];
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(_) => return false,
        }
    }
    rgit_bundle::header::has_signature(&buf[..n])
}

/// The bundle file a fetch or clone URL names: a local path (not a URL
/// with a scheme) to a file that starts with a bundle signature.
pub fn bundle_path(url: &str) -> Option<PathBuf> {
    if url.contains("://") {
        return None;
    }
    let exists = |p: &str| Path::new(p).exists();
    match rgit_transport::url::parse_remote(url, &exists) {
        Ok(rgit_transport::Remote::Local { path }) => {
            let path = PathBuf::from(String::from_utf8_lossy(&path).into_owned());
            is_bundle_file(&path).then_some(path)
        }
        _ => None,
    }
}

/// The references of a bundle as a remote lists them (git lists them in
/// reverse header order).
pub fn remote_refs(header: &Header) -> Vec<RemoteRef> {
    header
        .references
        .iter()
        .rev()
        .map(|r| RemoteRef {
            name: r.name.clone(),
            oid: Some(r.id),
            symref_target: None,
            peeled: None,
        })
        .collect()
}

/// The result of checking the prerequisites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prerequisites {
    /// All present and connected.
    Ok,
    /// These are missing (header order).
    Missing(Vec<ObjectId>),
    /// All present, but the history of some is incomplete.
    NotConnected,
}

impl Prerequisites {
    /// Print git's `error:` lines for a failed check.
    pub fn report(&self) {
        match self {
            Prerequisites::Ok => {}
            Prerequisites::Missing(ids) => {
                eprintln!("error: {}", crate::messages::LACKS_PREREQUISITES);
                for id in ids {
                    eprintln!("error: {} ", id.to_hex());
                }
            }
            Prerequisites::NotConnected => eprintln!("error: {}", crate::messages::NOT_CONNECTED),
        }
    }
}

/// Check the prerequisites of `header` in `repo`: each must exist, and
/// its history must be complete down to the local refs (or a shallow
/// boundary).
pub fn check_prerequisites(repo: &LocalRepo, header: &Header) -> Prerequisites {
    let missing: Vec<ObjectId> = header
        .prerequisites
        .iter()
        .filter(|p| !repo.has_object(&p.id))
        .map(|p| p.id)
        .collect();
    if !missing.is_empty() {
        return Prerequisites::Missing(missing);
    }
    if header.prerequisites.is_empty() {
        return Prerequisites::Ok;
    }
    let mut complete: HashSet<ObjectId> =
        repo.list_refs("").into_iter().map(|(_, id)| id).collect();
    if let Some(h) = repo.resolve("HEAD") {
        complete.insert(h);
    }
    let shallow: HashSet<ObjectId> = repo.shallow().into_iter().collect();
    let tips: Vec<ObjectId> = header.prerequisites.iter().map(|p| p.id).collect();
    if graph::history_complete(&repo.odb, &tips, &mut complete, &shallow) {
        Prerequisites::Ok
    } else {
        Prerequisites::NotConnected
    }
}

/// How to store the pack of a bundle.
#[derive(Default)]
pub struct UnbundleOptions {
    /// Show the index-pack meters.
    pub progress: bool,
    /// The title of the reading meter (`Unbundling objects` for `git
    /// bundle unbundle`, default `Receiving objects`).
    pub title: Option<String>,
    /// Check the objects (`fetch.fsckObjects`).
    pub fsck: Option<FsckCheck>,
}

/// Store the pack that follows the header in `bundle` in `repo`. The
/// prerequisites must have been checked. A filtered bundle's pack gets a
/// `.promisor` file.
pub fn unbundle(
    repo: &LocalRepo,
    bundle: &mut OpenBundle,
    opts: &UnbundleOptions,
) -> Result<ReceivedPack> {
    let pack = session::index_stream(
        repo,
        &mut bundle.reader,
        opts.title.as_deref(),
        opts.progress,
        opts.fsck.as_ref(),
    )?;
    if bundle.header.filter.is_some() {
        if let Some(p) = &pack.pack {
            std::fs::write(p.with_extension("promisor"), "from-bundle\n")
                .map_err(|e| Error::fatal(e.to_string()))?;
        }
    }
    Ok(pack)
}
