// SPDX-License-Identifier: MIT
//! A session with a remote `upload-pack`: connect, list the refs, and
//! fetch with the pack stored through index-pack.
//!
//! The client asks for the protocol version of `protocol.version`
//! (default 2, or `GIT_TEST_PROTOCOL_VERSION` when the configuration does
//! not say). The server decides: a v2 server answers with its capability
//! advertisement and is asked with `ls-refs` and `fetch`; a v0/v1 server
//! (or one that ignores the request) sends its ref advertisement, which is
//! the ref list, and a fetch negotiates on the same connection. A v0/v1
//! service ends after sending a pack, so a second fetch on a stateful
//! connection connects again.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

use bstr::ByteSlice;
use rgit_config::{ConfigSet, Environment};
use rgit_fs::TempFile;
use rgit_hash::{HashKind, ObjectId};
use rgit_object::fsck::{MsgId, Severity};
use rgit_pack::index_pack::{self as ip, IndexVersion, Input, ObjectDb, Options};
use rgit_protocol::v0::{self, Advertisement};
use rgit_protocol::v2::{
    Client, ConsecutiveNegotiator, FetchArgs, FetchOutcome, LsRefsArgs, Negotiator, RemoteRef,
    ShallowInfo, SkippingNegotiator,
};
use rgit_protocol::{Capabilities, Packet, Progress, ProgressStyle, ProtocolVersion, Reader};
use rgit_transport::{ConnectOptions, Connection, IpVersion, Service, Settings, Transport};

use crate::repo::LocalRepo;
use crate::{Error, Result, graph, messages};

/// How to reach the remote.
pub struct ConnectSettings<'a> {
    /// The environment (`GIT_SSH*`, `GIT_ALLOW_PROTOCOL`, proxies).
    pub env: &'a dyn Environment,
    /// The configuration (`http.*`, `core.sshCommand`, `protocol.*`).
    pub config: &'a ConfigSet,
    /// Extra environment for child processes (the global options'
    /// exports).
    pub base_env: Vec<(OsString, OsString)>,
    /// `--upload-pack` / `remote.<name>.uploadpack`.
    pub upload_pack: Option<String>,
    /// `-4` / `-6`.
    pub ip: IpVersion,
    /// `-v`: report connection steps (see [`ConnectOptions::verbose`]).
    pub verbose: bool,
}

/// The protocol name `protocol.<name>.allow` uses for `url`.
pub fn protocol_name(url: &str) -> String {
    let exists = |p: &str| Path::new(p).exists();
    match rgit_transport::url::parse_remote(url, &exists) {
        Ok(rgit_transport::Remote::Local { .. }) => "file".into(),
        Ok(rgit_transport::Remote::Ssh(_)) => "ssh".into(),
        Ok(rgit_transport::Remote::Other { scheme }) => scheme,
        Err(_) => "file".into(),
    }
}

fn parse_version(value: &[u8]) -> Option<ProtocolVersion> {
    match value.trim_ascii() {
        b"0" => Some(ProtocolVersion::V0),
        b"1" => Some(ProtocolVersion::V1),
        b"2" => Some(ProtocolVersion::V2),
        _ => None,
    }
}

/// The protocol version to ask for: `protocol.version`, else
/// `GIT_TEST_PROTOCOL_VERSION`, else 2.
pub fn protocol_version(config: &ConfigSet, env: &dyn Environment) -> Result<ProtocolVersion> {
    if let Some(value) = config
        .get("protocol.version")
        .and_then(|e| e.value.as_deref())
    {
        return parse_version(value).ok_or_else(|| {
            Error::fatal(messages::unknown_protocol_version(
                &String::from_utf8_lossy(value),
            ))
        });
    }
    match env.var("GIT_TEST_PROTOCOL_VERSION") {
        Some(v) if !v.is_empty() => {
            let v = v.to_string_lossy();
            parse_version(v.as_bytes())
                .ok_or_else(|| Error::fatal(messages::unknown_test_protocol_version(&v)))
        }
        _ => Ok(ProtocolVersion::V2),
    }
}

/// The protocol spoken on a session.
enum Proto {
    /// Protocol v2.
    V2(Client<Box<dyn Connection>>),
    /// Protocol v0/v1: the advertisement, and the connection if it can
    /// still be used (a stateful one is used up by a fetch).
    V0 {
        adv: Advertisement,
        client: Option<v0::Client<Box<dyn Connection>>>,
    },
}

/// An open connection to `upload-pack`.
pub struct Session {
    proto: Proto,
    url: String,
    transport: Box<dyn Transport>,
    options: ConnectOptions,
    program: &'static str,
    shallow_info: Option<ShallowInfo>,
}

/// Open a connection and read up to the version line of the response.
fn open(
    transport: &mut dyn Transport,
    options: &ConnectOptions,
    program: &'static str,
) -> Result<(Reader<Box<dyn Connection>>, ProtocolVersion)> {
    let read_any = Arc::new(AtomicBool::new(false));
    let conn: Box<dyn Connection> = Box::new(Watched {
        inner: transport.connect(options)?,
        read_any: read_any.clone(),
    });
    let mut reader = Reader::new(conn, program);
    let hung_up = |read_any: &AtomicBool, e: rgit_protocol::Error| match e {
        // Nothing at all came back: the service failed to start (its own
        // message is already on stderr).
        rgit_protocol::Error::HungUp if !read_any.load(Ordering::Relaxed) => {
            Error::fatal(messages::COULD_NOT_READ_REMOTE)
        }
        e => e.into(),
    };
    let stateless = reader.get_ref().is_stateless();
    // HTTP responses report cut-off packets (observed: t5702).
    reader.partial_errors(stateless);
    // Over HTTP git's remote helper reads the advertisement (traced as
    // `git`); see `after_advertisement` for what follows.
    if stateless {
        reader.tracer_mut().set_name("git");
    }
    let first = reader.peek().map_err(|e| hung_up(&read_any, e))?;
    if stateless && matches!(first, Some(Packet::Data(d)) if d.starts_with(b"# service=")) {
        reader.read()?;
        match reader.read()? {
            Packet::Flush => {}
            other => return Err(other.unexpected().into()),
        }
    }
    if reader.peek().map_err(|e| hung_up(&read_any, e))?.is_none() {
        return Err(Error::fatal(messages::COULD_NOT_READ_REMOTE));
    }
    let version = ProtocolVersion::read(&mut reader)?;
    Ok((reader, version))
}

/// The trace name after the advertisement: over HTTP, git's v0/v1
/// negotiation runs in a `fetch-pack` process.
fn after_advertisement(reader: &mut Reader<Box<dyn Connection>>, program: &'static str, v0: bool) {
    let stateless = reader.get_ref().is_stateless();
    let name = if stateless && v0 {
        "fetch-pack"
    } else {
        program
    };
    reader.tracer_mut().set_name(name);
}

/// Connect again for another v0/v1 fetch.
fn reconnect(
    transport: &mut dyn Transport,
    options: &ConnectOptions,
    program: &'static str,
) -> Result<(Advertisement, v0::Client<Box<dyn Connection>>)> {
    let (mut reader, version) = open(transport, options, program)?;
    if version == ProtocolVersion::V2 {
        return Err(rgit_protocol::Error::Unexpected("version 2".into()).into());
    }
    let adv = v0::read_advertisement(&mut reader, version)?;
    after_advertisement(&mut reader, program, true);
    let client = v0::Client::new(reader, &adv);
    Ok((adv, client))
}

impl Session {
    /// Connect to `url` and read the server's advertisement. `program`
    /// names the client in packet traces (`fetch`, `clone`).
    pub fn connect(
        url: &str,
        settings: &ConnectSettings<'_>,
        program: &'static str,
    ) -> Result<Session> {
        let protocol = protocol_name(url);
        rgit_remote::check_allowed(protocol.as_bytes(), settings.config, settings.env)
            .map_err(|e| Error::fatal(e.to_string()))?;
        let version = protocol_version(settings.config, settings.env)?;
        let tsettings = Settings {
            env: settings.env,
            config: settings.config,
            ip: settings.ip,
            base_env: settings.base_env.clone(),
        };
        let Some(mut transport) = rgit_http::transport_for(url, &tsettings)? else {
            let scheme = url.split("://").next().unwrap_or(url);
            return Err(Error::fatal(messages::no_remote_helper(scheme)));
        };
        let mut options = ConnectOptions::new(Service::UploadPack);
        options.program = settings.upload_pack.clone();
        options.protocol_version = version;
        options.trace_name = Some(program.to_owned());
        options.verbose = settings.verbose;
        rgit_protocol::session_id::set_advertise(
            settings
                .config
                .bool("transfer.advertiseSID")
                .ok()
                .flatten()
                .unwrap_or(false),
        );
        // A clone over HTTP traces the advertisement as `git` (the remote
        // helper reads it, observed in t5702), the rest as `clone`.
        let http = url.starts_with("http://") || url.starts_with("https://");
        let label = if http && program == "clone" {
            "git"
        } else {
            program
        };
        let (mut reader, answered) = open(transport.as_mut(), &options, label)?;
        rgit_protocol::session_id::note_version(answered);
        let proto = match answered {
            ProtocolVersion::V2 => {
                let caps = Capabilities::read_v2(&mut reader)?;
                rgit_protocol::session_id::note_server(&caps);
                after_advertisement(&mut reader, program, false);
                Proto::V2(Client::with_capabilities(reader, caps)?)
            }
            v => {
                let adv = v0::read_advertisement(&mut reader, v)?;
                rgit_protocol::session_id::note_server(&adv.caps);
                after_advertisement(&mut reader, program, true);
                let client = v0::Client::new(reader, &adv);
                Proto::V0 {
                    adv,
                    client: Some(client),
                }
            }
        };
        Ok(Session {
            proto,
            url: url.to_owned(),
            transport,
            options,
            program,
            shallow_info: None,
        })
    }

    /// The `shallow-info` sections received so far (merged), taken out.
    pub fn take_shallow_info(&mut self) -> Option<ShallowInfo> {
        self.shallow_info.take()
    }

    /// The URL this session talks to.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The protocol version the server answered with.
    pub fn version(&self) -> ProtocolVersion {
        match &self.proto {
            Proto::V2(_) => ProtocolVersion::V2,
            Proto::V0 { adv, .. } => adv.version,
        }
    }

    /// The server's hash algorithm.
    pub fn object_format(&self) -> HashKind {
        match &self.proto {
            Proto::V2(c) => c.object_format(),
            Proto::V0 { adv, .. } => adv.format,
        }
    }

    /// Whether the server takes `want-ref` lines (protocol v2 with the
    /// `ref-in-want` fetch feature).
    pub fn supports_ref_in_want(&self) -> bool {
        matches!(self.proto, Proto::V2(_))
            && self.capabilities().has_feature("fetch", "ref-in-want")
    }

    /// The server's capabilities (v2: the capability advertisement; v0/v1:
    /// the capabilities of the first ref line).
    pub fn capabilities(&self) -> &Capabilities {
        match &self.proto {
            Proto::V2(c) => c.capabilities(),
            Proto::V0 { adv, .. } => &adv.caps,
        }
    }

    /// Server options given on the command line need protocol v2 (those
    /// from `remote.<name>.serverOption` are dropped silently): prints the
    /// hint and fails otherwise.
    pub fn check_server_options(&self, cmdline: &[String]) -> Result<()> {
        if cmdline.is_empty() || self.version() == ProtocolVersion::V2 {
            return Ok(());
        }
        eprintln!("hint: {}", crate::messages::SERVER_OPTIONS_HINT);
        Err(crate::Error::fatal(crate::messages::SERVER_OPTIONS_NEED_V2))
    }

    /// `ls-refs` with `peel`, `symrefs` and `unborn` (if supported) and the
    /// given `ref-prefix` lines (none: all refs) and server options.
    pub fn ls_refs(
        &mut self,
        prefixes: &[String],
        server_options: &[String],
    ) -> Result<Vec<RemoteRef>> {
        let mut args = LsRefsArgs::standard();
        args.ref_prefixes = prefixes.to_vec();
        args.server_options = server_options.to_vec();
        self.list_refs(&args)
    }

    /// The remote's refs: v2 `ls-refs` with `args`; v0/v1 the advertised
    /// refs (all of them; the prefixes only narrow a v2 request).
    pub fn list_refs(&mut self, args: &LsRefsArgs) -> Result<Vec<RemoteRef>> {
        match &mut self.proto {
            Proto::V2(c) => Ok(c.ls_refs(args)?),
            Proto::V0 { adv, .. } => Ok(adv.refs.clone()),
        }
    }

    /// Fetch `wants` and store the pack in `repo` (`objects/pack`, with a
    /// `.keep` file that the caller removes with
    /// [`ReceivedPack::release`] once the new refs are written).
    ///
    /// `haves` selects the negotiation: `Some(advertised)` runs the
    /// default (`consecutive`) algorithm over all local refs, using the
    /// remote's advertised tips that exist locally as known common
    /// commits; `None` sends no `have` lines (clone). Returns `None` when
    /// `wants` is empty or the server sent no pack.
    pub fn fetch_pack(
        &mut self,
        repo: &LocalRepo,
        wants: &[ObjectId],
        haves: Option<&[ObjectId]>,
        opts: &PackOptions,
    ) -> Result<Option<ReceivedPack>> {
        if wants.is_empty() && opts.want_refs.is_empty() {
            return Ok(None);
        }
        let mut args = FetchArgs {
            wants: wants.to_vec(),
            want_refs: opts.want_refs.clone(),
            thin_pack: true,
            no_progress: opts.no_progress,
            include_tag: opts.include_tag,
            ofs_delta: true,
            local_format: Some(repo.hash),
            shallow: repo.shallow(),
            deepen: opts.deepen,
            deepen_relative: opts.deepen_relative,
            deepen_since: opts.deepen_since,
            deepen_not: opts.deepen_not.clone(),
            ..FetchArgs::default()
        };
        args.server_options.clone_from(&opts.server_options);

        // A v0/v1 service that sent a pack has ended: connect again.
        if let Proto::V0 { adv, client } = &mut self.proto {
            if client.is_none() {
                let (a, c) = reconnect(self.transport.as_mut(), &self.options, self.program)?;
                *adv = a;
                *client = Some(c);
            }
        }

        let odb = &repo.odb;
        // Shallow commits have no parents here.
        let shallow: HashSet<ObjectId> = args.shallow.iter().copied().collect();
        let mut source = |id: &ObjectId| {
            let mut info = graph::commit_info(odb, id)?;
            if shallow.contains(id) {
                info.parents.clear();
            }
            Some(info)
        };
        let mut consecutive;
        let mut skipping;
        let mut noop = rgit_protocol::v2::NoopNegotiator;
        // The commits the walk knows to be common (advertised and present
        // here) and its starting points.
        // Advertised objects we have are common, unless their history is
        // incomplete (left by a fetch whose refs were rejected).
        let mut complete: HashSet<ObjectId> =
            repo.list_refs("").into_iter().map(|(_, id)| id).collect();
        let common: Vec<ObjectId> = haves
            .unwrap_or_default()
            .iter()
            .filter(|id| {
                repo.has_object(id)
                    && graph::history_complete(&repo.odb, &[**id], &mut complete, &shallow)
            })
            .copied()
            .collect();
        let starts = |known: &mut dyn FnMut(ObjectId), tip: &mut dyn FnMut(ObjectId)| {
            for id in &common {
                known(*id);
            }
            if let Some(tips) = &opts.tips {
                for id in tips {
                    tip(*id);
                }
            } else {
                if let Some(h) = repo.resolve("HEAD") {
                    tip(h);
                }
                for (_, id) in repo.list_refs("") {
                    tip(id);
                }
                // The refs of alternates are tips too: their history is
                // here.
                for id in repo.alternate_tips() {
                    if repo.has_object(&id) {
                        tip(id);
                    }
                }
            }
        };
        let negotiator: &mut dyn Negotiator = match (haves, opts.negotiation) {
            (None, _) | (Some(_), Negotiation::Noop) => &mut noop,
            (Some(_), Negotiation::Consecutive) => {
                consecutive = ConsecutiveNegotiator::new(&mut source);
                let n = std::cell::RefCell::new(&mut consecutive);
                starts(&mut |id| n.borrow_mut().known_common(id), &mut |id| {
                    n.borrow_mut().add_tip(id)
                });
                &mut consecutive
            }
            (Some(_), Negotiation::Skipping) => {
                skipping = SkippingNegotiator::new(&mut source);
                let n = std::cell::RefCell::new(&mut skipping);
                starts(&mut |id| n.borrow_mut().known_common(id), &mut |id| {
                    n.borrow_mut().add_tip(id)
                });
                &mut skipping
            }
        };

        let pack_dir = repo.objects_dir().join("pack");
        std::fs::create_dir_all(&pack_dir)?;
        let mut tmp = TempFile::with_mode(&pack_dir, rgit_fs::PACK_PREFIX, 0o444)?;
        let tmp_path = tmp.path().to_path_buf();
        let mut file = tmp.file_mut().try_clone()?;

        let ip_opts = index_pack_options(repo);
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(64);
        let proto = &mut self.proto;
        let show_progress = !opts.no_progress;
        let unpack_limit = u64::from(opts.unpack_limit.unwrap_or(0));
        let fsck_hook = opts.fsck.as_ref().map(|check| FsckHook {
            hash: repo.hash,
            check,
        });
        let (outcome, indexed) = std::thread::scope(|s| {
            let worker = s.spawn(move || {
                let mut meter =
                    ip::meter::PackMeter::new(None, true, show_progress).unpack_below(unpack_limit);
                let mut reader = ChannelReader {
                    rx,
                    buf: Vec::new(),
                    pos: 0,
                };
                ip::index_pack(
                    Input::Stream {
                        reader: &mut reader,
                        dest: &mut file,
                        dest_path: &tmp_path,
                    },
                    &ip_opts,
                    Some(odb as &dyn ObjectDb),
                    fsck_hook.as_ref().map(|h| h as &dyn ip::ObjectHook),
                    Some(&mut meter),
                )
            });
            let mut progress = Progress::new(io::stderr(), ProgressStyle::detect());
            let trace = std::cell::RefCell::new(trace_packfile());
            let sink = |data: &[u8]| {
                if let Some(f) = trace.borrow_mut().as_mut() {
                    let _ = f.write_all(data);
                }
                tx.send(data.to_vec())
                    .map_err(|_| io::Error::other("index-pack stopped"))
            };
            let outcome: rgit_protocol::Result<FetchOutcome> = match proto {
                Proto::V2(client) => client.fetch(&args, negotiator, &mut progress, sink),
                Proto::V0 { client, .. } => {
                    let c = client.as_mut().expect("connected above");
                    c.fetch(&args, negotiator, &mut progress, sink)
                }
            };
            drop(tx);
            let indexed = match worker.join() {
                Ok(r) => r,
                Err(panic) => std::panic::resume_unwind(panic),
            };
            (outcome, indexed)
        });
        let outcome = outcome?;
        if let Some(info) = &outcome.shallow_info {
            let merged = self.shallow_info.get_or_insert_with(ShallowInfo::default);
            merged.shallow.extend(info.shallow.iter().copied());
            merged.unshallow.extend(info.unshallow.iter().copied());
        }
        // A stateful v0/v1 connection is used up after the pack.
        if let Proto::V0 { client, .. } = &mut self.proto {
            let stateless = client.as_ref().is_some_and(|c| c.is_stateless());
            if outcome.packfile && !stateless {
                if let Some(c) = client.take() {
                    c.into_inner().close()?;
                }
            }
        }
        if !outcome.packfile {
            return Ok(None);
        }
        let indexed = match indexed {
            Err(ip::Error::Hook(msg)) => {
                // git: `fatal: fsck error in packed object`, then the
                // caller's own line.
                eprintln!("fatal: {msg}");
                let failed = opts
                    .fsck
                    .as_ref()
                    .map(|c| c.failed.clone())
                    .unwrap_or_default();
                return Err(Error::fatal(failed));
            }
            r => r?,
        };
        // An empty pack (e.g. deepening to the current depth) is not kept.
        if indexed.objects().is_empty() {
            return Ok(None);
        }
        let objects: HashSet<ObjectId> = indexed.objects().iter().map(|o| o.id).collect();
        // A shallow v0/v1 server lists all its shallow commits up front;
        // those that came with the pack are shallow here too (v2 sends
        // them in its shallow-info).
        if let Proto::V0 { adv, .. } = &self.proto {
            let reached: Vec<ObjectId> = adv
                .shallow
                .iter()
                .filter(|id| objects.contains(id))
                .copied()
                .collect();
            if !reached.is_empty() {
                let merged = self.shallow_info.get_or_insert_with(ShallowInfo::default);
                for id in reached {
                    if !merged.shallow.contains(&id) {
                        merged.shallow.push(id);
                    }
                }
            }
        }
        // Objects in the pack as sent (without bases appended for a thin
        // pack): below the unpack limit they become loose objects.
        let sent = indexed.objects().len() as u64 - indexed.local_objects();
        if opts
            .unpack_limit
            .is_some_and(|limit| sent < u64::from(limit))
        {
            unpack_to_loose(repo, &tmp, &indexed)?;
            drop(tmp);
            return Ok(Some(ReceivedPack {
                objects,
                pack: None,
                keep: None,
                rounds: outcome.rounds,
            }));
        }
        let stored = store_pack(repo, tmp, &indexed)?;
        repo.rescan_packs();
        Ok(Some(ReceivedPack {
            objects,
            pack: Some(stored.0),
            keep: stored.1,
            rounds: outcome.rounds,
        }))
    }

    /// `fetch --negotiate-only` over protocol v2: negotiate from `tips`
    /// with `negotiation` and return the commits the server has in common
    /// with us. `None` when the session is not protocol v2; an error when
    /// the server lacks `wait-for-done`.
    pub fn negotiate_only(
        &mut self,
        repo: &LocalRepo,
        tips: &[ObjectId],
        negotiation: Negotiation,
        server_options: &[String],
    ) -> Result<Option<Vec<ObjectId>>> {
        let Proto::V2(client) = &mut self.proto else {
            return Ok(None);
        };
        if !client.supports_wait_for_done() {
            return Err(Error::fatal(messages::NO_WAIT_FOR_DONE));
        }
        let odb = &repo.odb;
        let shallow: HashSet<ObjectId> = repo.shallow().into_iter().collect();
        let mut source = |id: &ObjectId| {
            let mut info = graph::commit_info(odb, id)?;
            if shallow.contains(id) {
                info.parents.clear();
            }
            Some(info)
        };
        let mut consecutive;
        let mut skipping;
        let mut noop = rgit_protocol::v2::NoopNegotiator;
        let negotiator: &mut dyn Negotiator = match negotiation {
            Negotiation::Noop => &mut noop,
            Negotiation::Consecutive => {
                consecutive = ConsecutiveNegotiator::new(&mut source);
                for id in tips {
                    consecutive.add_tip(*id);
                }
                &mut consecutive
            }
            Negotiation::Skipping => {
                skipping = SkippingNegotiator::new(&mut source);
                for id in tips {
                    skipping.add_tip(*id);
                }
                &mut skipping
            }
        };
        let outcome = client.negotiate_only(server_options, negotiator)?;
        Ok(Some(outcome.common))
    }

    /// End the session (flush on stateful connections) and wait for the
    /// remote side.
    pub fn close(self) -> Result<()> {
        match self.proto {
            Proto::V2(mut client) => {
                client.close()?;
                client.into_inner().close()?;
            }
            Proto::V0 {
                client: Some(mut client),
                ..
            } => {
                // The service may end any way after the final flush.
                client.close()?;
                let _ = client.into_inner().close();
            }
            Proto::V0 { client: None, .. } => {}
        }
        Ok(())
    }
}

/// Settings for [`Session::fetch_pack`].
#[derive(Debug, Clone, Default)]
pub struct PackOptions {
    /// Ask the server not to send progress (`no-progress`).
    pub no_progress: bool,
    /// Ask for tags pointing at sent objects (`include-tag`).
    pub include_tag: bool,
    /// `server-option=` lines.
    pub server_options: Vec<String>,
    /// Store packs with fewer objects than this as loose objects
    /// (`fetch.unpackLimit`); `None` always keeps the pack.
    pub unpack_limit: Option<u32>,
    /// `--negotiation-restrict`: the only tips the negotiation walks from
    /// (`None`: every ref, `HEAD` and the alternates' refs).
    pub tips: Option<Vec<ObjectId>>,
    /// `deepen <depth>` (`--depth`, `--deepen`, `--unshallow`).
    pub deepen: Option<u32>,
    /// `deepen-relative` (`--deepen`).
    pub deepen_relative: bool,
    /// `deepen-since <time>` (`--shallow-since`).
    pub deepen_since: Option<i64>,
    /// `deepen-not <rev>` (`--shallow-exclude`).
    pub deepen_not: Vec<String>,
    /// `fetch.negotiationAlgorithm`.
    pub negotiation: Negotiation,
    /// Refs asked for by name (`want-ref`, protocol v2 servers with
    /// `ref-in-want`) instead of by object.
    pub want_refs: Vec<String>,
    /// Check every received object (`fetch.fsckObjects`,
    /// `transfer.fsckObjects`).
    pub fsck: Option<FsckCheck>,
}

/// Object checks of `fetch.fsckObjects` / `transfer.fsckObjects`, at the
/// severities of `fetch.fsck.<msg-id>` (`fsck.<msg-id>` does not apply,
/// as observed from git).
#[derive(Debug, Clone)]
pub struct FsckCheck {
    /// Severities set by `fetch.fsck.<msg-id>`.
    pub severities: Vec<(MsgId, Severity)>,
    /// Objects not checked (`fetch.fsck.skipList`).
    pub skip: HashSet<ObjectId>,
    /// The last fatal line when an object fails (it differs between
    /// fetch and clone).
    pub failed: String,
}

impl FsckCheck {
    /// The checks the configuration asks for; `failed` as in
    /// [`FsckCheck::failed`].
    pub fn from_config(
        config: &rgit_config::ConfigSet,
        hash: HashKind,
        failed: &str,
    ) -> Option<FsckCheck> {
        let on = config
            .bool("fetch.fsckobjects")
            .ok()
            .flatten()
            .or_else(|| config.bool("transfer.fsckobjects").ok().flatten())
            .unwrap_or(false);
        if !on {
            return None;
        }
        let mut severities = Vec::new();
        let mut skip = HashSet::new();
        for entry in config.entries() {
            let name = String::from_utf8_lossy(&entry.name).into_owned();
            let Some(key) = name.strip_prefix("fetch.fsck.") else {
                continue;
            };
            if key == "skiplist" {
                let path = entry.value.as_ref().map(|v| v.to_str_lossy().into_owned());
                let data = path.and_then(|p| std::fs::read(p).ok()).unwrap_or_default();
                for line in data.split(|&b| b == b'\n') {
                    let line = line
                        .split(|&b| b == b'#')
                        .next()
                        .unwrap_or(&[])
                        .trim_ascii();
                    if let Ok(id) = ObjectId::from_hex(hash, line) {
                        skip.insert(id);
                    }
                }
                continue;
            }
            let Some(id) = MsgId::from_name(key) else {
                continue;
            };
            let value = entry
                .value
                .as_ref()
                .map(|v| String::from_utf8_lossy(v).to_ascii_lowercase());
            let sev = match value.as_deref() {
                Some("error") => Severity::Error,
                Some("warn") => Severity::Warn,
                Some("ignore") => Severity::Ignore,
                _ => continue,
            };
            severities.push((id, sev));
        }
        Some(FsckCheck {
            severities,
            skip,
            failed: failed.to_owned(),
        })
    }

    fn severity(&self, id: MsgId) -> Severity {
        self.severities
            .iter()
            .rev()
            .find(|(i, _)| *i == id)
            .map_or_else(|| id.default_severity(), |(_, s)| *s)
    }
}

/// Runs [`FsckCheck`] on each object index-pack resolves.
struct FsckHook<'a> {
    hash: rgit_hash::HashKind,
    check: &'a FsckCheck,
}

impl ip::ObjectHook for FsckHook<'_> {
    fn object(
        &self,
        id: &ObjectId,
        kind: rgit_pack::ObjectKind,
        data: &[u8],
    ) -> std::result::Result<(), String> {
        if self.check.skip.contains(id) {
            return Ok(());
        }
        let okind = match kind {
            rgit_pack::ObjectKind::Commit => rgit_object::ObjectKind::Commit,
            rgit_pack::ObjectKind::Tree => rgit_object::ObjectKind::Tree,
            rgit_pack::ObjectKind::Blob => rgit_object::ObjectKind::Blob,
            rgit_pack::ObjectKind::Tag => rgit_object::ObjectKind::Tag,
        };
        let mut failed = false;
        let mut lines = Vec::new();
        rgit_object::fsck::check_object(
            okind,
            self.hash,
            data,
            &rgit_object::fsck::Options::default(),
            &mut |f| {
                match self.check.severity(f.id) {
                    Severity::Ignore => {}
                    Severity::Warn | Severity::Info => {
                        lines.push(format!("warning: {}", messages::fsck_finding(id, &f)));
                    }
                    Severity::Error | Severity::Fatal => {
                        if let Some(c) = &f.cause {
                            lines.push(format!("error: {c}"));
                        }
                        lines.push(format!("error: {}", messages::fsck_finding(id, &f)));
                        failed = true;
                        return rgit_object::fsck::Control::Stop;
                    }
                }
                rgit_object::fsck::Control::Continue
            },
        );
        if !lines.is_empty() {
            let mut err = io::stderr().lock();
            for l in lines {
                let _ = writeln!(err, "{l}");
            }
        }
        if failed {
            Err(messages::FSCK_ERROR.to_string())
        } else {
            Ok(())
        }
    }
}

/// The negotiation algorithm (`fetch.negotiationAlgorithm`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Negotiation {
    /// Every commit, newest first.
    #[default]
    Consecutive,
    /// Skip a growing number of commits after each `have`.
    Skipping,
    /// No `have` lines at all.
    Noop,
}

impl Negotiation {
    /// `fetch.negotiationAlgorithm` (`default` and unset: `skipping` under
    /// `feature.experimental`, else `consecutive`). An unknown value is an
    /// error.
    pub fn from_config(config: &ConfigSet) -> Result<Negotiation> {
        let experimental = config
            .bool("feature.experimental")
            .ok()
            .flatten()
            .unwrap_or(false);
        let default = if experimental {
            Negotiation::Skipping
        } else {
            Negotiation::Consecutive
        };
        let Some(v) = config
            .string("fetch.negotiationalgorithm")
            .map_err(|e| Error::fatal(e.to_string()))?
        else {
            return Ok(default);
        };
        let v = v.to_str_lossy();
        Ok(match v.to_ascii_lowercase().as_str() {
            "consecutive" => Negotiation::Consecutive,
            "skipping" => Negotiation::Skipping,
            "noop" => Negotiation::Noop,
            "default" => default,
            _ => return Err(Error::fatal(messages::unknown_negotiation_algorithm(&v))),
        })
    }
}

impl PackOptions {
    /// Whether the history is to be deepened or cut.
    pub fn deepens(&self) -> bool {
        self.deepen.is_some() || self.deepen_since.is_some() || !self.deepen_not.is_empty()
    }
}

/// A pack stored by [`Session::fetch_pack`].
#[derive(Debug, Clone)]
pub struct ReceivedPack {
    /// Path of the `.pack` file; `None` when the objects were stored as
    /// loose objects.
    pub pack: Option<PathBuf>,
    /// The `.keep` file protecting it until the refs are written.
    pub keep: Option<PathBuf>,
    /// Ids of every object in the pack.
    pub objects: HashSet<ObjectId>,
    /// Requests sent during negotiation.
    pub rounds: usize,
}

impl ReceivedPack {
    /// Remove the `.keep` file.
    pub fn release(&self) {
        if let Some(k) = &self.keep {
            let _ = std::fs::remove_file(k);
        }
    }
}

/// Index the pack read from `reader` and store it in `repo` like a
/// fetched pack (`objects/pack`, with a `.keep` file the caller releases).
/// `GIT_TRACE_PACKFILE` (Documentation/git.adoc): the received pack is
/// written verbatim to the named file (an absolute path; appended to).
fn trace_packfile() -> Option<std::fs::File> {
    let path = std::env::var_os("GIT_TRACE_PACKFILE")?;
    if !std::path::Path::new(&path).is_absolute() {
        return None;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

pub fn store_pack_stream(repo: &LocalRepo, reader: &mut dyn Read) -> Result<ReceivedPack> {
    let pack_dir = repo.objects_dir().join("pack");
    std::fs::create_dir_all(&pack_dir)?;
    let mut tmp = TempFile::with_mode(&pack_dir, rgit_fs::PACK_PREFIX, 0o444)?;
    let tmp_path = tmp.path().to_path_buf();
    let mut file = tmp.file_mut().try_clone()?;
    let ip_opts = index_pack_options(repo);
    let indexed = ip::index_pack(
        Input::Stream {
            reader,
            dest: &mut file,
            dest_path: &tmp_path,
        },
        &ip_opts,
        Some(&repo.odb as &dyn ObjectDb),
        None,
        None,
    )?;
    let stored = store_pack(repo, tmp, &indexed)?;
    repo.rescan_packs();
    Ok(ReceivedPack {
        objects: indexed.objects().iter().map(|o| o.id).collect(),
        pack: Some(stored.0),
        keep: stored.1,
        rounds: 0,
    })
}

fn index_pack_options(repo: &LocalRepo) -> Options {
    let cfg = &repo.config;
    let mut o = Options::new(repo.hash);
    o.fix_thin = true;
    o.threads = cfg
        .int("pack.threads")
        .ok()
        .flatten()
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(0);
    o.compression_level = cfg
        .int("core.loosecompression")
        .ok()
        .flatten()
        .or_else(|| cfg.int("core.compression").ok().flatten())
        .unwrap_or(1) as i32;
    o
}

/// Write the objects of the received pack (except bases appended to
/// complete a thin pack) as loose objects, like `git unpack-objects`.
fn unpack_to_loose(repo: &LocalRepo, tmp: &TempFile, indexed: &ip::IndexedPack) -> Result<()> {
    let idx = indexed.index_bytes(IndexVersion::default())?;
    let data = std::fs::read(tmp.path())?;
    let pack = rgit_pack::Pack::from_bytes(idx, data, tmp.path(), repo.hash)
        .map_err(|e| Error::fatal(e.to_string()))?;
    let mut cache = rgit_pack::DeltaBaseCache::default();
    let sent = indexed.objects().len() - indexed.local_objects() as usize;
    for o in &indexed.objects()[..sent] {
        // The empty tree is known without being stored; git writes it.
        if o.id != repo.hash.empty_tree() && repo.has_object(&o.id) {
            continue;
        }
        let obj = pack
            .read_at(o.offset, &mut cache, &mut |_| None)
            .map_err(|e| Error::fatal(e.to_string()))?;
        repo.odb.write(obj.kind, &obj.data)?;
    }
    Ok(())
}

/// Move the received pack to `pack-<checksum>.pack` with `.keep`, `.rev`
/// and `.idx` next to it.
fn store_pack(
    repo: &LocalRepo,
    tmp: TempFile,
    indexed: &ip::IndexedPack,
) -> Result<(PathBuf, Option<PathBuf>)> {
    let dir = tmp.path().parent().expect("in objects/pack").to_path_buf();
    let base = dir.join(format!("pack-{}", indexed.checksum().to_hex()));
    let with = |ext: &str| base.with_extension(ext);
    let keep = with("keep");
    let kept = write_keep(&keep)?;
    tmp.persist(with("pack"))?;
    let write_rev = repo
        .config
        .bool("pack.writereverseindex")
        .ok()
        .flatten()
        .unwrap_or(true);
    if write_rev {
        write_file(&with("rev"), "tmp_rev_", &indexed.rev_bytes()?)?;
    }
    let version = match repo.config.int("pack.indexversion").ok().flatten() {
        Some(1) => IndexVersion {
            version: 1,
            ..IndexVersion::default()
        },
        _ => IndexVersion::default(),
    };
    write_file(&with("idx"), "tmp_idx_", &indexed.index_bytes(version)?)?;
    Ok((with("pack"), kept.then_some(keep)))
}

fn write_file(dest: &Path, prefix: &str, data: &[u8]) -> Result<()> {
    let dir = dest.parent().unwrap_or(Path::new("."));
    let mut tmp = TempFile::with_mode(dir, prefix, 0o444)?;
    tmp.file_mut().write_all(data)?;
    tmp.persist(dest)?;
    Ok(())
}

/// Create the `.keep` file; `false` if one exists already (the pack was
/// there before and stays kept).
fn write_keep(path: &Path) -> Result<bool> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    match o.open(path) {
        Ok(mut f) => {
            writeln!(f, "fetch-pack {}", std::process::id())?;
            Ok(true)
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// A connection that notes whether any byte was read.
struct Watched {
    inner: Box<dyn Connection>,
    read_any: Arc<AtomicBool>,
}

impl Read for Watched {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.read_any.store(true, Ordering::Relaxed);
        }
        Ok(n)
    }
}

impl Write for Watched {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl Connection for Watched {
    fn is_stateless(&self) -> bool {
        self.inner.is_stateless()
    }
    fn end_request(&mut self) -> rgit_transport::Result<()> {
        self.inner.end_request()
    }
    fn split(&mut self) -> (&mut (dyn Read + Send), &mut dyn rgit_transport::WriteHalf) {
        self.inner.split()
    }
    fn close(self: Box<Self>) -> rgit_transport::Result<()> {
        self.inner.close()
    }
}

/// The pack data arriving from the protocol thread.
struct ChannelReader {
    rx: mpsc::Receiver<Vec<u8>>,
    buf: Vec<u8>,
    pos: usize,
}

impl Read for ChannelReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pos == self.buf.len() {
            match self.rx.recv() {
                Ok(b) => {
                    self.buf = b;
                    self.pos = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}
