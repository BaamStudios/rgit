// SPDX-License-Identifier: MIT
//! A protocol v2 session with a remote `upload-pack`: connect, `ls-refs`,
//! and `fetch` with the pack stored through index-pack.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

use rgit_config::{ConfigSet, Environment};
use rgit_fs::TempFile;
use rgit_hash::ObjectId;
use rgit_pack::index_pack::{self as ip, IndexVersion, Input, ObjectDb, Options};
use rgit_protocol::v2::{
    Client, ConsecutiveNegotiator, FetchArgs, FetchOutcome, LsRefsArgs, Negotiator, RemoteRef,
};
use rgit_protocol::{Progress, ProgressStyle, ProtocolVersion};
use rgit_transport::{ConnectOptions, Connection, IpVersion, Service, Settings};

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

/// An open protocol v2 connection to `upload-pack`.
pub struct Session {
    client: Client<Box<dyn Connection>>,
    url: String,
}

impl Session {
    /// Connect to `url` and read the capability advertisement. `program`
    /// names the client in packet traces (`fetch`, `clone`).
    pub fn connect(
        url: &str,
        settings: &ConnectSettings<'_>,
        program: &'static str,
    ) -> Result<Session> {
        let protocol = protocol_name(url);
        rgit_remote::check_allowed(protocol.as_bytes(), settings.config, settings.env)
            .map_err(|e| Error::fatal(e.to_string()))?;
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
        options.protocol_version = ProtocolVersion::V2;
        let read_any = Arc::new(AtomicBool::new(false));
        let conn: Box<dyn Connection> = Box::new(Watched {
            inner: transport.connect(&options)?,
            read_any: read_any.clone(),
        });
        let client = match Client::connect(conn, program) {
            Ok(c) => c,
            // Nothing at all came back: the service failed to start (its
            // own message is already on stderr).
            Err(rgit_protocol::Error::HungUp | rgit_protocol::Error::NotV2(0))
                if !read_any.load(Ordering::Relaxed) =>
            {
                return Err(Error::fatal(messages::COULD_NOT_READ_REMOTE));
            }
            Err(e) => return Err(e.into()),
        };
        Ok(Session {
            client,
            url: url.to_owned(),
        })
    }

    /// The URL this session talks to.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The protocol client (capabilities, object format).
    pub fn client(&mut self) -> &mut Client<Box<dyn Connection>> {
        &mut self.client
    }

    /// `ls-refs` with `peel`, `symrefs` and `unborn` (if supported) and the
    /// given `ref-prefix` lines (none: all refs).
    pub fn ls_refs(&mut self, prefixes: &[String]) -> Result<Vec<RemoteRef>> {
        let mut args = LsRefsArgs::standard();
        args.ref_prefixes = prefixes.to_vec();
        Ok(self.client.ls_refs(&args)?)
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
        let client = &mut self.client;
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
            let outcome: rgit_protocol::Result<FetchOutcome> =
                client.fetch(&args, negotiator, &mut progress, |data| {
                    tx.send(data.to_vec())
                        .map_err(|_| io::Error::other("index-pack stopped"))
                });
            drop(tx);
            let indexed = match worker.join() {
                Ok(r) => r,
                Err(panic) => std::panic::resume_unwind(panic),
            };
            (outcome, indexed)
        });
        let outcome = outcome?;
        if !outcome.packfile {
            return Ok(None);
        }
        let indexed = indexed?;
        let stored = store_pack(repo, tmp, &indexed)?;
        repo.rescan_packs();
        Ok(Some(ReceivedPack {
            objects: indexed.objects().iter().map(|o| o.id).collect(),
            pack: stored.0,
            keep: stored.1,
            rounds: outcome.rounds,
        }))
    }

    /// End the session (flush on stateful connections) and wait for the
    /// remote side.
    pub fn close(mut self) -> Result<()> {
        self.client.close()?;
        self.client.into_inner().close()?;
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
}

/// A pack stored by [`Session::fetch_pack`].
#[derive(Debug, Clone)]
pub struct ReceivedPack {
    /// Path of the `.pack` file.
    pub pack: PathBuf,
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
