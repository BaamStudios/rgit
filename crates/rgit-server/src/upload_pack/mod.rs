// SPDX-License-Identifier: MIT
//! `git-upload-pack`: the server side of fetch and clone.
//!
//! Protocol v2 (`Documentation/gitprotocol-v2.adoc`): the capability
//! advertisement and the commands `ls-refs`, `fetch` and `object-info`
//! (and `bundle-uri`, the `bundle.*` configuration). Protocol v0 and
//! v1 (`Documentation/gitprotocol-pack.adoc`): the reference
//! advertisement, `want`/`have` negotiation in all three acknowledgment
//! modes, shallow and filtered requests. Both with namespaces and hidden
//! references (`transfer.hideRefs`). The behaviour not written down in
//! the documentation (messages, the order of sections and capabilities,
//! when `ready` is sent, what `pack-objects` is given) follows
//! observations of the git 2.56 binary.
//!
//! The pack itself is made by a `pack-objects` child process (or by the
//! command in `uploadpack.packObjectsHook`), as git does: its standard
//! output goes to the client on sideband 1, its standard error (progress)
//! on sideband 2. See [`PackObjectsCommand`].
//!
//! ```no_run
//! # fn demo(repo: &rgit_server::upload_pack::Repository<'_>) {
//! use rgit_server::upload_pack::{Options, upload_pack};
//! let mut opts = Options::new(vec!["git".into()]);
//! opts.protocol = rgit_protocol::ProtocolVersion::V2;
//! upload_pack(repo, &opts, std::io::stdin().lock(), std::io::stdout().lock()).unwrap();
//! # }
//! ```

mod bundle_uri;
mod error;
mod fetch;
mod graph;
mod ls_refs;
pub mod messages;
mod object_info;
mod pack;
mod promisor;
mod refs;
mod serve;
mod shallow;
mod v0;

use std::ffi::OsString;
use std::io::{Read, Write};

use rgit_config::ConfigSet;
use rgit_hash::{HashKind, ObjectId};
use rgit_odb::ObjectDatabase;
use rgit_protocol::ProtocolVersion;
use rgit_refs::RefStore;

pub use error::{Error, Result};

/// The repository that is served.
pub struct Repository<'a> {
    /// Hash algorithm.
    pub hash: HashKind,
    /// Objects.
    pub odb: &'a ObjectDatabase,
    /// References.
    pub refs: &'a dyn RefStore,
    /// The configuration (`uploadpack.*`, `lsrefs.*`, `transfer.*`).
    pub config: &'a ConfigSet,
    /// The repository's own shallow commits (its `shallow` file).
    pub shallow: Vec<ObjectId>,
}

/// How to start `pack-objects`.
///
/// The service runs `<git>... [--shallow-file ""] pack-objects --revs
/// --stdout ...` and writes the revisions to its standard input, the way
/// git's `upload-pack` does. With `uploadpack.packObjectsHook` set (in
/// protected configuration) the hook is run through the shell instead,
/// with `git pack-objects ...` as its arguments.
#[derive(Debug, Clone)]
pub struct PackObjectsCommand {
    /// The command that runs git, e.g. `["/usr/bin/rgit"]`; the global
    /// options and `pack-objects ...` are appended.
    pub git: Vec<OsString>,
    /// Environment for the child on top of this process's (`GIT_DIR` and
    /// the other global options).
    pub env: Vec<(OsString, OsString)>,
}

/// A callback, see [`Options::on_activity`].
pub type Activity = Box<dyn Fn() + Send + Sync>;

/// How to serve.
pub struct Options {
    /// The protocol version the client asked for (`GIT_PROTOCOL`).
    pub protocol: ProtocolVersion,
    /// `--stateless-rpc`: one request, one response.
    pub stateless_rpc: bool,
    /// `--http-backend-info-refs` / `--advertise-refs`: only the
    /// advertisement.
    pub advertise_refs: bool,
    /// How to start `pack-objects`.
    pub pack_objects: PackObjectsCommand,
    /// Called whenever the client or `pack-objects` sent something
    /// (`--timeout`: the caller restarts its timer).
    pub on_activity: Option<Activity>,
    /// Overrides `uploadpack.allowSidebandAll` (`GIT_TEST_SIDEBAND_ALL`).
    pub sideband_all: Option<bool>,
    /// The namespace whose references are served (`GIT_NAMESPACE`,
    /// gitnamespaces(7)).
    pub namespace: Option<String>,
}

impl Options {
    /// Protocol v0, stateful, with `git` as the command that runs git.
    pub fn new(git: Vec<OsString>) -> Options {
        Options {
            protocol: ProtocolVersion::V0,
            stateless_rpc: false,
            advertise_refs: false,
            pack_objects: PackObjectsCommand {
                git,
                env: Vec::new(),
            },
            on_activity: None,
            sideband_all: None,
            namespace: None,
        }
    }

    pub(crate) fn active(&self) {
        if let Some(f) = &self.on_activity {
            f();
        }
    }
}

/// Serve one connection: read requests from `input` and write the
/// responses to `output` until the client is done.
///
/// On [`Error::Fatal`] the caller prints `fatal: <message>`, on
/// [`Error::Refused`] `error: <message>`, and exits with 128; the client
/// has been told already where the protocol allows it.
pub fn upload_pack<R: Read, W: Write>(
    repo: &Repository<'_>,
    opts: &Options,
    input: R,
    output: W,
) -> Result<()> {
    match opts.protocol {
        ProtocolVersion::V2 => serve::serve(repo, opts, input, output),
        _ => v0::serve(repo, opts, input, output),
    }
}

/// The settings of the served repository that `upload-pack` reads.
#[derive(Debug, Clone)]
pub(crate) struct Settings {
    /// `uploadpack.allowFilter`.
    pub allow_filter: bool,
    /// `uploadpack.allowTipSHA1InWant` (or `allowAnySHA1InWant`).
    pub allow_tip: bool,
    /// `uploadpack.allowReachableSHA1InWant` (or `allowAnySHA1InWant`).
    pub allow_reachable: bool,
    /// `uploadpack.allowAnySHA1InWant`.
    pub allow_any: bool,
    /// `uploadpack.allowRefInWant`.
    pub allow_ref_in_want: bool,
    /// `uploadpack.allowSidebandAll`.
    pub allow_sideband_all: bool,
    /// `transfer.advertiseSID`.
    pub advertise_sid: bool,
    /// `transfer.advertiseObjectInfo`.
    pub advertise_object_info: bool,
    /// `uploadpack.advertiseBundleURIs`.
    pub advertise_bundle_uris: bool,
    /// `lsrefs.unborn`.
    pub unborn: Unborn,
    /// `uploadpack.keepAlive` in seconds; 0: none.
    pub keep_alive: u64,
    /// `uploadpack.packObjectsHook`, from protected configuration only.
    pub pack_objects_hook: Option<String>,
    /// The `promisor-remote` capability (`promisor.advertise`).
    pub promisor_remotes: Option<String>,
}

/// `lsrefs.unborn`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unborn {
    Advertise,
    Allow,
    Ignore,
}

impl Settings {
    pub fn load(repo: &Repository<'_>, opts: &Options) -> Result<Settings> {
        let config = repo.config;
        let flag = |name: &str| -> Result<bool> {
            config
                .bool(name)
                .map(|v| v.unwrap_or(false))
                .map_err(|e| Error::fatal(e.to_string()))
        };
        let unborn = match config.string("lsrefs.unborn").ok().flatten() {
            Some(v) if v == "allow" => Unborn::Allow,
            Some(v) if v == "ignore" => Unborn::Ignore,
            _ => Unborn::Advertise,
        };
        let keep_alive = match config
            .int("uploadpack.keepalive")
            .map_err(|e| Error::fatal(e.to_string()))?
        {
            Some(v) if v > 0 => v as u64,
            Some(_) => 0,
            None => 5,
        };
        // Only protected configuration (system, global, command line) may
        // name a command to run: the repository is not trusted.
        let pack_objects_hook = config
            .get_all("uploadpack.packobjectshook")
            .filter(|e| {
                matches!(
                    e.source.scope,
                    rgit_config::Scope::System
                        | rgit_config::Scope::Global
                        | rgit_config::Scope::Command
                )
            })
            .last()
            .and_then(|e| e.value.as_ref())
            .map(|v| v.to_string())
            .filter(|v| !v.is_empty());
        let allow_any = flag("uploadpack.allowanysha1inwant")?;
        Ok(Settings {
            allow_filter: flag("uploadpack.allowfilter")?,
            allow_tip: allow_any || flag("uploadpack.allowtipsha1inwant")?,
            allow_reachable: allow_any || flag("uploadpack.allowreachablesha1inwant")?,
            allow_any,
            allow_ref_in_want: flag("uploadpack.allowrefinwant")?,
            allow_sideband_all: match opts.sideband_all {
                Some(v) => v,
                None => flag("uploadpack.allowsidebandall")?,
            },
            advertise_sid: flag("transfer.advertisesid")?,
            advertise_object_info: flag("transfer.advertiseobjectinfo")?,
            advertise_bundle_uris: flag("uploadpack.advertisebundleuris")?,
            unborn,
            keep_alive,
            pack_objects_hook,
            promisor_remotes: promisor::advertisement(config),
        })
    }
}
