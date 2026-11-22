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

use rgit_config::{ConfigSet, Environment};
use rgit_fs::TempFile;
use rgit_hash::{HashKind, ObjectId};
use rgit_pack::index_pack::{self as ip, IndexVersion, Input, ObjectDb, Options};
use rgit_protocol::v0::{self, Advertisement};
use rgit_protocol::v2::{
    Client, ConsecutiveNegotiator, FetchArgs, FetchOutcome, LsRefsArgs, Negotiator, RemoteRef,
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
        let (mut reader, answered) = open(transport.as_mut(), &options, program)?;
        let proto = match answered {
            ProtocolVersion::V2 => {
                let caps = Capabilities::read_v2(&mut reader)?;
                Proto::V2(Client::with_capabilities(reader, caps)?)
            }
            v => {
                let adv = v0::read_advertisement(&mut reader, v)?;
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
        })
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

    /// The server's capabilities (v2: the capability advertisement; v0/v1:
    /// the capabilities of the first ref line).
    pub fn capabilities(&self) -> &Capabilities {
        match &self.proto {
            Proto::V2(c) => c.capabilities(),
            Proto::V0 { adv, .. } => &adv.caps,
        }
    }

    /// `ls-refs` with `peel`, `symrefs` and `unborn` (if supported) and the
    /// given `ref-prefix` lines (none: all refs).
    pub fn ls_refs(&mut self, prefixes: &[String]) -> Result<Vec<RemoteRef>> {
        let mut args = LsRefsArgs::standard();
        args.ref_prefixes = prefixes.to_vec();
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
        if wants.is_empty() {
            return Ok(None);
        }
        let mut args = FetchArgs {
            wants: wants.to_vec(),
            thin_pack: true,
            no_progress: opts.no_progress,
            include_tag: opts.include_tag,
            ofs_delta: true,
            local_format: Some(repo.hash),
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
        let mut source = |id: &ObjectId| graph::commit_info(odb, id);
        let mut consecutive;
        let mut noop = rgit_protocol::v2::NoopNegotiator;
        let negotiator: &mut dyn Negotiator = match haves {
            Some(advertised) => {
                consecutive = ConsecutiveNegotiator::new(&mut source);
                for id in advertised {
                    if repo.has_object(id) {
                        consecutive.known_common(*id);
                    }
                }
                if let Some(h) = repo.resolve("HEAD") {
                    consecutive.add_tip(h);
                }
                for (_, id) in repo.list_refs("") {
                    consecutive.add_tip(id);
                }
                // The refs of alternates are tips too: their history is
                // here.
                for id in repo.alternate_tips() {
                    if repo.has_object(&id) {
                        consecutive.add_tip(id);
                    }
                }
                &mut consecutive
            }
            None => &mut noop,
        };

        let pack_dir = repo.objects_dir().join("pack");
        std::fs::create_dir_all(&pack_dir)?;
        let mut tmp = TempFile::with_mode(&pack_dir, rgit_fs::PACK_PREFIX, 0o444)?;
        let tmp_path = tmp.path().to_path_buf();
        let mut file = tmp.file_mut().try_clone()?;

        let ip_opts = index_pack_options(repo);
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(64);
        let proto = &mut self.proto;
        let (outcome, indexed) = std::thread::scope(|s| {
            let worker = s.spawn(move || {
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
                    None,
                    None,
                )
            });
            let mut progress = Progress::new(io::stderr(), ProgressStyle::detect());
            let sink = |data: &[u8]| {
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
        let indexed = indexed?;
        let objects: HashSet<ObjectId> = indexed.objects().iter().map(|o| o.id).collect();
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
                client.close()?;
                client.into_inner().close()?;
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
    fn split(&mut self) -> (&mut (dyn Read + Send), &mut (dyn Write + Send)) {
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
