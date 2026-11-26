// SPDX-License-Identifier: MIT
//! Bundle URIs on the client (`Documentation/technical/bundle-uri.adoc`,
//! `clone --bundle-uri`, `fetch.bundleURI`, `transfer.bundleURI`).
//!
//! A URI (a local path, `file://` or `http(s)://`) is downloaded into
//! `objects/bundles/`. A bundle is unbundled when its prerequisites are
//! present, and its refs are written as `refs/bundles/*` (the refspec
//! `+refs/*:refs/bundles/*`), so that the following fetch announces them
//! as `have`s. Anything else is read as a bundle list in git's config
//! format:
//!
//! - `bundle.mode=any`: the first bundle (or list) that works is enough.
//! - `bundle.mode=all`: every bundle is downloaded, and they are unbundled
//!   as their prerequisites become available.
//! - `bundle.heuristic=creationToken`: bundles are downloaded newest
//!   (largest token) first, only those newer than the token stored in
//!   `fetch.bundleCreationToken`, until one can be unbundled; then the
//!   newer ones that were waiting are unbundled in increasing order. When
//!   no downloaded bundle is left over, the largest token unbundled is
//!   returned for the caller to store.
//!
//! Problems are warnings: the remote stays the source of truth.

use std::io::Cursor;
use std::path::PathBuf;

use rgit_bundle::list::{self, BundleList, ListMode};
use rgit_hash::ObjectId;

use crate::bundle::{self as fb, Prerequisites, UnbundleOptions};
use crate::repo::LocalRepo;
use crate::session::{ConnectSettings, FsckCheck};
use crate::update;

/// How deep bundle lists may refer to other lists.
const MAX_DEPTH: usize = 5;

/// `warning:` when a URI could not be downloaded.
pub fn failed_to_download(uri: &str) -> String {
    format!("failed to download bundle from URI '{uri}'")
}

/// `warning:` when the bundle URI given to clone gave nothing.
pub fn failed_to_fetch(uri: &str) -> String {
    format!("failed to fetch objects from bundle URI '{uri}'")
}

/// `warning:` for a file that is neither bundle nor list.
pub fn not_bundle_or_list(uri: &str) -> String {
    format!("file at URI '{uri}' is not a bundle or bundle list")
}

/// `error:` for a URI with spaces or control characters.
pub fn uri_malformed(uri: &str) -> String {
    format!("bundle-uri: URI is malformed: {uri}")
}

/// `error:` for a download target with control characters.
pub fn filename_malformed(path: &str) -> String {
    format!("bundle-uri: filename is malformed: {path}")
}

/// What a bundle URI gave.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Something was unbundled.
    pub unbundled: bool,
    /// The URI was a list with `bundle.heuristic=creationToken`.
    pub heuristic: bool,
    /// The creation token to store (see the module documentation).
    pub token: Option<u64>,
    /// The URI itself could be used (downloaded, and a bundle or list).
    pub ok: bool,
}

/// One bundle download.
enum Got {
    Failed,
    Bundle(Vec<u8>),
    List(BundleList),
}

struct Client<'a> {
    repo: &'a LocalRepo,
    connect: &'a ConnectSettings<'a>,
    fsck: Option<FsckCheck>,
}

fn is_malformed(uri: &str) -> bool {
    uri.chars().any(|c| c == ' ' || c.is_control())
}

impl Client<'_> {
    fn bundles_dir(&self) -> PathBuf {
        self.repo.objects_dir().join("bundles")
    }

    /// Read the file at `uri`; `None` (after the warning) when it fails.
    fn download(&self, uri: &str) -> Option<Vec<u8>> {
        let remote = uri.starts_with("http://") || uri.starts_with("https://");
        if remote && is_malformed(uri) {
            eprintln!("error: {}", uri_malformed(uri));
            return None;
        }
        let dir = self.bundles_dir();
        let dir_shown = dir.to_string_lossy();
        if remote && dir_shown.chars().any(|c| c.is_control()) {
            eprintln!("error: {}", filename_malformed(&dir_shown));
            return None;
        }
        let data = if uri.starts_with("http://") || uri.starts_with("https://") {
            let child = rgit_trace::trace2::child_start(
                rgit_trace::trace2::ChildInfo {
                    class: "remote-https",
                    ..Default::default()
                },
                &["git-remote-https", uri],
            );
            let settings = rgit_transport::Settings {
                env: self.connect.env,
                config: self.connect.config,
                ip: self.connect.ip,
                base_env: self.connect.base_env.clone(),
            };
            let r = rgit_http::download(uri, &settings).ok();
            if let Some(c) = child {
                c.exit(-1, if r.is_some() { 0 } else { 1 });
            }
            r
        } else if let Some(path) = uri.strip_prefix("file://") {
            std::fs::read(path).ok()
        } else if uri.contains("://") {
            None
        } else {
            std::fs::read(uri).ok()
        };
        if data.is_none() {
            eprintln!("warning: {}", failed_to_download(uri));
        }
        data
    }

    /// Download `uri` and tell bundle from list.
    fn get(&self, uri: &str) -> Got {
        let Some(data) = self.download(uri) else {
            return Got::Failed;
        };
        if rgit_bundle::header::has_signature(&data) {
            return Got::Bundle(data);
        }
        match self.parse_list(uri, &data) {
            Some(l) => Got::List(l),
            None => {
                eprintln!("warning: {}", not_bundle_or_list(uri));
                Got::Failed
            }
        }
    }

    /// Parse `data` as a bundle list found at `uri`; errors are printed.
    fn parse_list(&self, uri: &str, data: &[u8]) -> Option<BundleList> {
        let dir = self.bundles_dir();
        let _ = std::fs::create_dir_all(&dir);
        let tmp = rgit_fs::TempFile::new_in(&dir, "tmp_uri_").ok();
        let path = tmp
            .as_ref()
            .map_or_else(|| dir.join("tmp_uri"), |t| t.path().to_path_buf());
        if let Some(t) = &tmp {
            let _ = std::fs::write(t.path(), data);
        }
        list::parse_config(&list::base_of(uri), data, &path, &mut |m| {
            eprintln!("warning: {m}")
        })
        .ok()
    }

    /// Unbundle `data` and write its refs below `refs/bundles/`. `false`
    /// when the prerequisites are missing or storing failed.
    fn unbundle(&self, data: &[u8]) -> bool {
        let Ok(mut b) = fb::open_reader(Box::new(Cursor::new(data.to_vec()))) else {
            return false;
        };
        if fb::check_prerequisites(self.repo, &b.header) != Prerequisites::Ok {
            return false;
        }
        let opts = UnbundleOptions {
            progress: false,
            title: None,
            fsck: self.fsck.clone(),
        };
        let pack = match fb::unbundle(self.repo, &mut b, &opts) {
            Ok(p) => p,
            Err(e) => {
                let m = e.to_string();
                if !m.is_empty() {
                    eprintln!("error: {m}");
                }
                return false;
            }
        };
        // One transaction for all refs: a ref to a missing object fails
        // it (the objects stay).
        let mut updates = Vec::new();
        let mut missing = false;
        for r in &b.header.references {
            let Some(rest) = r.name.strip_prefix(b"refs/") else {
                continue;
            };
            let name = format!("refs/bundles/{}", String::from_utf8_lossy(rest));
            if !self.repo.has_object(&r.id) {
                eprintln!(
                    "error: {}",
                    rgit_refs::messages::nonexistent_object(&name, r.id.to_hex())
                );
                missing = true;
                continue;
            }
            updates.push(rgit_refs::RefUpdate::update(name, r.id, None).message("fetched bundle"));
        }
        if !missing {
            if let Err(e) = update::write_all(self.repo, updates) {
                eprintln!("error: {e}");
            }
        }
        pack.release();
        true
    }

    /// Fetch `uri` (a bundle or a list) at list depth `depth`.
    fn fetch(&self, uri: &str, depth: usize, stored: Option<u64>) -> Outcome {
        match self.get(uri) {
            Got::Failed => Outcome::default(),
            Got::Bundle(data) => Outcome {
                unbundled: self.unbundle(&data),
                ok: true,
                ..Outcome::default()
            },
            Got::List(l) => {
                let mut o = self.fetch_list(&l, depth, stored);
                o.ok = true;
                o
            }
        }
    }

    /// Use the bundles of `l`.
    fn fetch_list(&self, l: &BundleList, depth: usize, stored: Option<u64>) -> Outcome {
        if depth >= MAX_DEPTH {
            return Outcome::default();
        }
        if l.mode == ListMode::Any {
            for e in &l.entries {
                let Some(uri) = &e.uri else { continue };
                let o = self.fetch(uri, depth + 1, stored);
                if o.ok {
                    return o;
                }
            }
            return Outcome::default();
        }
        if l.uses_creation_token() {
            return self.fetch_by_token(l, stored);
        }
        // `all`: download everything, then unbundle as prerequisites
        // become available.
        let mut waiting: Vec<Vec<u8>> = Vec::new();
        let mut out = Outcome::default();
        for e in &l.entries {
            let Some(uri) = &e.uri else { continue };
            match self.get(uri) {
                Got::Failed => {}
                Got::Bundle(data) => waiting.push(data),
                Got::List(sub) => {
                    let o = self.fetch_list(&sub, depth + 1, stored);
                    out.unbundled |= o.unbundled;
                }
            }
        }
        loop {
            let before = waiting.len();
            let mut rest = Vec::new();
            for data in waiting {
                if self.unbundle(&data) {
                    out.unbundled = true;
                } else {
                    rest.push(data);
                }
            }
            waiting = rest;
            if waiting.is_empty() || waiting.len() == before {
                break;
            }
        }
        out
    }

    /// The `creationToken` heuristic (see the module documentation).
    fn fetch_by_token(&self, l: &BundleList, stored: Option<u64>) -> Outcome {
        #[derive(PartialEq)]
        enum State {
            Untried,
            Failed,
            Waiting(Vec<u8>),
            Done,
        }
        let mut bundles: Vec<(u64, &str)> = l
            .entries
            .iter()
            .filter_map(|e| Some((e.creation_token.unwrap_or(0), e.uri.as_deref()?)))
            .filter(|(t, _)| stored.is_none_or(|s| *t > s))
            .collect();
        bundles.sort_by_key(|b| std::cmp::Reverse(b.0));
        let mut state: Vec<State> = bundles.iter().map(|_| State::Untried).collect();
        let mut i = 0;
        while i < bundles.len() {
            let data = match self.get(bundles[i].1) {
                Got::Bundle(d) => d,
                Got::Failed | Got::List(_) => {
                    state[i] = State::Failed;
                    i += 1;
                    continue;
                }
            };
            if !self.unbundle(&data) {
                state[i] = State::Waiting(data);
                i += 1;
                continue;
            }
            state[i] = State::Done;
            // The newer bundles that were waiting for this one.
            let mut all_done = true;
            for j in (0..i).rev() {
                let State::Waiting(d) = &state[j] else {
                    continue;
                };
                if self.unbundle(d) {
                    state[j] = State::Done;
                } else {
                    all_done = false;
                    break;
                }
            }
            if all_done {
                break;
            }
            i += 1;
        }
        let unbundled: Vec<u64> = bundles
            .iter()
            .zip(&state)
            .filter(|(_, s)| **s == State::Done)
            .map(|(b, _)| b.0)
            .collect();
        let left_over = state.iter().any(|s| matches!(s, State::Waiting(_)));
        Outcome {
            unbundled: !unbundled.is_empty(),
            heuristic: true,
            token: if left_over {
                None
            } else {
                unbundled.iter().copied().max()
            },
            ok: true,
        }
    }
}

/// Fetch the bundles at `uri` into `repo` (`stored`: the
/// `fetch.bundleCreationToken` of an earlier run).
pub fn fetch_bundle_uri(
    repo: &LocalRepo,
    connect: &ConnectSettings<'_>,
    uri: &str,
    stored: Option<u64>,
) -> Outcome {
    let client = Client {
        repo,
        connect,
        fsck: FsckCheck::from_config(&repo.config, repo.hash, crate::messages::INDEX_PACK_FAILED),
    };
    let _region = rgit_trace::trace2::region("fetch", "fetch-bundle-uri", true);
    let o = client.fetch(uri, 0, stored);
    if !o.ok && stored.is_none() {
        eprintln!("warning: {}", failed_to_fetch(uri));
    }
    o
}

/// Use a bundle list the server advertised (`bundle-uri` command).
pub fn fetch_bundle_list(
    repo: &LocalRepo,
    connect: &ConnectSettings<'_>,
    l: &BundleList,
) -> Outcome {
    let client = Client {
        repo,
        connect,
        fsck: FsckCheck::from_config(&repo.config, repo.hash, crate::messages::INDEX_PACK_FAILED),
    };
    let _region = rgit_trace::trace2::region("fetch", "fetch-bundle-uri", true);
    client.fetch_list(l, 0, None)
}

/// The tips of the `refs/bundles/*` refs.
pub fn bundle_tips(repo: &LocalRepo) -> Vec<ObjectId> {
    repo.list_refs("refs/bundles/")
        .into_iter()
        .map(|(_, id)| id)
        .collect()
}
