// SPDX-License-Identifier: MIT
//! `git receive-pack <git-dir>` (Documentation/git-receive-pack.adoc): the
//! server side of `git push`. The protocol, the policies and the hooks
//! are in `rgit_server::receive_pack`; this file opens the repository and
//! implements its [`Backend`]: the quarantine directory, `index-pack` /
//! `unpack-objects` / `rev-list` children (started from this binary, as
//! git starts its own programs), the reference transactions, the hooks and
//! `updateInstead`.
//!
//! Children and hooks that write to standard error while the client gets
//! a sideband have their output relayed to sideband 2.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rgit_config::ConfigSet;
use rgit_hash::{HashKind, ObjectId};
use rgit_refs::{RefUpdate, Transaction, TransactionError};
use rgit_repo::Setup;
use rgit_server::receive_pack::{
    self as rp, Backend, HookCall, IngestOptions, ProcReceive, RefChange, RefFailure, Settings,
    Sink, messages as m,
};

use crate::prelude::*;
use crate::refs_support::{self, Objects, UpdateHooks};
use crate::repo::{self, Mode};

const USAGE_TAIL_SHORT: &str = "\n    -q, --[no-]quiet      quiet\n\n";
const USAGE_TAIL_ERROR: &str = "\n    -q, --[no-]quiet      quiet\n    --[no-]advertise-refs ...\n                          alias of --http-backend-info-refs\n\n";
const USAGE_TAIL_ALL: &str = "\n    -q, --[no-]quiet      quiet\n    --[no-]skip-connectivity-check\n    --[no-]stateless-rpc\n    --[no-]http-backend-info-refs\n    --[no-]advertise-refs alias of --http-backend-info-refs\n    --[no-]reject-thin-pack-for-testing\n\n";

fn usage_error(msg: &str) -> Die {
    Die::Output(rgit_options::Exit {
        code: rgit_options::EXIT_USAGE,
        stdout: String::new(),
        stderr: format!("fatal: {msg}\n\nusage: {}\n{USAGE_TAIL_ERROR}", m::USAGE),
    })
}

/// Reads file descriptor 0 without buffering, so that children started
/// for the pack see the stream exactly where the commands ended.
struct RawStdin;

impl Read for RawStdin {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            // SAFETY: reads into a valid buffer of the given length.
            let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
            if n >= 0 {
                return Ok(n as usize);
            }
            let e = std::io::Error::last_os_error();
            if e.kind() != std::io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    }
}

/// Writes to file descriptor 1 directly: packets reach the client
/// without a buffer in between (and from relay threads).
struct RawStdout;

impl Write for RawStdout {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        loop {
            // SAFETY: writes from a valid buffer of the given length.
            let n = unsafe { libc::write(1, buf.as_ptr().cast(), buf.len()) };
            if n >= 0 {
                return Ok(n as usize);
            }
            let e = std::io::Error::last_os_error();
            if e.kind() != std::io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mut opts = rp::Options::default();
    let mut dir: Option<String> = None;
    let mut reject_thin = false;
    let mut only_args = false;
    for arg in &argv[1..] {
        if only_args || !arg.starts_with('-') || arg == "-" {
            if dir.is_some() {
                return Err(usage_error("too many arguments"));
            }
            dir = Some(arg.clone());
            continue;
        }
        match arg.as_str() {
            "--" => only_args = true,
            "-h" => {
                print!("usage: {}\n{USAGE_TAIL_SHORT}", m::USAGE);
                return Ok(0);
            }
            "--help-all" => {
                print!("usage: {}\n{USAGE_TAIL_ALL}", m::USAGE);
                return Ok(0);
            }
            "-q" | "--quiet" => opts.quiet = true,
            "--no-quiet" => opts.quiet = false,
            "--stateless-rpc" => opts.stateless_rpc = true,
            "--no-stateless-rpc" => opts.stateless_rpc = false,
            "--http-backend-info-refs" | "--advertise-refs" => opts.advertise_refs = true,
            "--no-http-backend-info-refs" | "--no-advertise-refs" => opts.advertise_refs = false,
            "--skip-connectivity-check" => opts.skip_connectivity_check = true,
            "--no-skip-connectivity-check" => opts.skip_connectivity_check = false,
            "--reject-thin-pack-for-testing" => reject_thin = true,
            "--no-reject-thin-pack-for-testing" => reject_thin = false,
            other => {
                let name = other.trim_start_matches('-');
                let msg = if other.starts_with("--") {
                    format!("error: unknown option `{name}'\n")
                } else {
                    format!("error: unknown switch `{}'\n", &name[..1.min(name.len())])
                };
                return Err(Die::Output(rgit_options::Exit {
                    code: rgit_options::EXIT_USAGE,
                    stdout: String::new(),
                    stderr: format!("{msg}usage: {}\n{USAGE_TAIL_SHORT}", m::USAGE),
                }));
            }
        }
    }
    let Some(dir) = dir else {
        return Err(usage_error(m::MUST_SPECIFY_DIRECTORY));
    };
    let git_dir = enter_repo(&dir).ok_or_else(|| Die::Fatal(m::not_a_repository(&dir)))?;
    std::env::set_current_dir(&git_dir).map_err(|_| Die::Fatal(m::not_a_repository(&dir)))?;
    ctx.git_dir = Some(".".to_string());
    let setup = repo::setup_with(
        ctx,
        Mode::Required,
        rgit_repo::format::Support {
            sha256: true,
            ..Default::default()
        },
    )?;
    let config = refs_support::load_config(ctx, &setup)?;
    if let Some(e) = config
        .get_all("receive.fsck.skiplist")
        .find(|e| e.value.is_none())
    {
        crate::messages::print_error(&format!(
            "missing value for '{}'",
            "receive.fsck.skiplist"
        ));
        return Err(Die::Fatal(crate::revisions::unable_to_parse(
            "receive.fsck.skiplist",
            e,
        )));
    }
    let mut settings = Settings::from_config(&config);
    // `receive.fsck.<msg-id>`: unknown ids are skipped with a warning,
    // unknown severities are fatal.
    let mut msgs = Vec::new();
    for (id, value) in std::mem::take(&mut settings.fsck_msgs) {
        if id == "skiplist" {
            msgs.push((id, value));
            continue;
        }
        if rgit_object::fsck::MsgId::from_name(&id).is_none() {
            eprintln!("warning: skipping unknown msg id '{id}'");
            continue;
        }
        if !["error", "warn", "ignore"].contains(&value.to_ascii_lowercase().as_str()) {
            return Err(Die::Fatal(format!(
                "Unknown fsck message type: '{value}'"
            )));
        }
        msgs.push((id, value));
    }
    settings.fsck_msgs = msgs;
    let namespace = ctx
        .namespace
        .clone()
        .or_else(|| std::env::var("GIT_NAMESPACE").ok())
        .filter(|n| !n.is_empty())
        .map(|n| {
            n.split('/')
                .filter(|c| !c.is_empty())
                .map(|c| format!("refs/namespaces/{c}/"))
                .collect::<String>()
        })
        .filter(|n| !n.is_empty());
    opts.namespace = namespace;
    opts.protocol_v1 = std::env::var("GIT_PROTOCOL")
        .ok()
        .is_some_and(|p| p.split(':').any(|v| v == "version=1"));
    opts.agent = rgit_protocol::agent::agent().to_string();
    opts.session_id = Some(rgit_trace::trace2::session_id().to_string());
    let abs_git_dir = std::env::current_dir().unwrap_or_else(|_| git_dir.clone());
    if let Some(seed) = &settings.cert_nonce_seed {
        let stamp = now();
        opts.push_cert_nonce = Some(nonce(seed, &abs_git_dir, stamp));
    }
    let hooks = crate::hooks::for_repo(ctx, &setup, &config)?;
    let mut backend = CliBackend {
        ctx: ctx.clone(),
        hash: setup.format.object_format,
        bare: config.bool("core.bare").ok().flatten().unwrap_or(false),
        work_tree: config
            .string("core.worktree")
            .ok()
            .flatten()
            .map(|w| PathBuf::from(w.to_string()))
            .unwrap_or_else(|| PathBuf::from("..")),
        setup,
        config,
        hooks,
        git_dir: abs_git_dir,
        exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("rgit")),
        quarantine: None,
        keep_file: None,
        objects: None,
        reject_thin,
        nonce_seed: settings.cert_nonce_seed.clone(),
        shallow_file: None,
    };
    let _ = std::io::stdout().flush();
    let result = rp::serve(&mut backend, &settings, &opts, RawStdin, RawStdout);
    backend.discard_quarantine();
    backend.remove_keep();
    if let Some(f) = backend.shallow_file.take() {
        let _ = std::fs::remove_file(f);
    }
    match result {
        Ok(()) => Ok(0),
        Err(rp::Fatal(msg)) => Err(Die::Fatal(msg)),
    }
}

/// `enter_repo`: `<dir>/.git`, `<dir>.git`, `<dir>` (also with `.git`
/// appended), the first that is a git directory.
fn enter_repo(dir: &str) -> Option<PathBuf> {
    let base = PathBuf::from(dir);
    let candidates = [
        PathBuf::from(format!("{dir}.git/.git")),
        base.join(".git"),
        PathBuf::from(format!("{dir}.git")),
        base.clone(),
    ];
    candidates.into_iter().find(|c| is_git_dir(c))
}

fn is_git_dir(p: &Path) -> bool {
    p.join("HEAD").is_file() && p.join("objects").is_dir() && p.join("refs").is_dir()
        || p.join("commondir").is_file() && p.join("HEAD").is_file()
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// HMAC-SHA1 (RFC 2104) over `msg` with `key`, hex.
fn hmac_sha1(key: &[u8], msg: &[u8]) -> String {
    let sha1 = |data: &[u8]| -> Vec<u8> {
        rgit_hash::hash(HashKind::Sha1, data)
            .map(|o| o.as_bytes().to_vec())
            .unwrap_or_default()
    };
    let mut k = if key.len() > 64 {
        sha1(key)
    } else {
        key.to_vec()
    };
    k.resize(64, 0);
    let mut inner: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    inner.extend_from_slice(msg);
    let ih = sha1(&inner);
    let mut outer: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    outer.extend_from_slice(&ih);
    sha1(&outer).iter().map(|b| format!("{b:02x}")).collect()
}

/// The push certificate nonce: `<stamp>-<HMAC(seed, <path>:<stamp>)>`.
fn nonce(seed: &str, path: &Path, stamp: i64) -> String {
    let msg = format!("{}:{stamp}", path.display());
    format!("{stamp}-{}", hmac_sha1(seed.as_bytes(), msg.as_bytes()))
}

struct CliBackend {
    ctx: Context,
    setup: Setup,
    config: ConfigSet,
    hooks: rgit_hook::Hooks,
    hash: HashKind,
    bare: bool,
    /// `core.worktree`, else `..` (relative to the git directory).
    work_tree: PathBuf,
    /// The absolute git directory (also the current directory).
    git_dir: PathBuf,
    exe: PathBuf,
    quarantine: Option<PathBuf>,
    /// The `.keep` file `index-pack` wrote (removed after the push).
    keep_file: Option<PathBuf>,
    objects: Option<Objects>,
    reject_thin: bool,
    nonce_seed: Option<String>,
    /// A temporary shallow file with the client's shallow commits
    /// (`GIT_SHALLOW_FILE` for the children).
    shallow_file: Option<PathBuf>,
}

/// Runs `f` with this process's standard error (and so that of the
/// children and hooks it starts) relayed to `sink` when the sink is a
/// sideband.
fn relay_stderr<T>(sink: &dyn Sink, f: impl FnOnce() -> T) -> T {
    if !sink.is_sideband() {
        return f();
    }
    let _ = std::io::stderr().flush();
    let mut fds = [0i32; 2];
    // SAFETY: plain pipe/dup calls on valid descriptors; the saved
    // descriptor is restored and every new one closed below.
    unsafe {
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return f();
        }
        let saved = libc::dup(2);
        if saved < 0 {
            libc::close(fds[0]);
            libc::close(fds[1]);
            return f();
        }
        libc::dup2(fds[1], 2);
        libc::close(fds[1]);
        let read_fd = fds[0];
        std::thread::scope(|scope| {
            let relay = scope.spawn(move || {
                let mut buf = [0u8; 4096];
                loop {
                    let n = libc::read(read_fd, buf.as_mut_ptr().cast(), buf.len());
                    if n <= 0 {
                        break;
                    }
                    sink.write(&buf[..n as usize]);
                }
                libc::close(read_fd);
            });
            let result = f();
            let _ = std::io::stderr().flush();
            libc::dup2(saved, 2);
            libc::close(saved);
            let _ = relay.join();
            result
        })
    }
}

/// Copies a child's standard error to `sink` until it ends.
fn copy_to_sink(mut from: impl Read, sink: &dyn Sink) {
    let mut buf = [0u8; 4096];
    loop {
        match from.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => sink.write(&buf[..n]),
        }
    }
}

impl CliBackend {
    fn objects_dir(&self) -> PathBuf {
        self.git_dir.join("objects")
    }

    fn objects(&mut self) -> &Objects {
        if self.objects.is_none() {
            self.objects = Some(Objects::open(&self.setup));
        }
        self.objects.as_ref().expect("opened above")
    }

    /// The environment of children and hooks while the quarantine exists.
    fn quarantine_env(&self) -> Vec<(OsString, OsString)> {
        let Some(q) = &self.quarantine else {
            return Vec::new();
        };
        // A path with the separator in it is C-quoted.
        let own = self.objects_dir().to_string_lossy().into_owned();
        let own = if own.contains(':') || own.starts_with('"') {
            let mut q = String::from("\"");
            for ch in own.chars() {
                if ch == '"' || ch == '\\' {
                    q.push('\\');
                }
                q.push(ch);
            }
            q.push('"');
            q
        } else {
            own
        };
        let mut alternates = OsString::from(own);
        if let Some(old) = std::env::var_os("GIT_ALTERNATE_OBJECT_DIRECTORIES") {
            if !old.is_empty() {
                alternates.push(":");
                alternates.push(old);
            }
        }
        vec![
            ("GIT_OBJECT_DIRECTORY".into(), q.clone().into_os_string()),
            ("GIT_ALTERNATE_OBJECT_DIRECTORIES".into(), alternates),
            ("GIT_QUARANTINE_PATH".into(), q.clone().into_os_string()),
        ]
    }

    /// A child running this binary with `args`.
    fn child(&self, args: &[&str]) -> Command {
        let mut c = Command::new(&self.exe);
        c.args(args);
        c.envs(self.ctx.child_env());
        c.envs(self.quarantine_env());
        if let Some(f) = &self.shallow_file {
            c.env("GIT_SHALLOW_FILE", f);
        }
        c
    }

    fn create_quarantine(&mut self) -> Result<PathBuf, String> {
        if let Some(q) = &self.quarantine {
            return Ok(q.clone());
        }
        let base = self.objects_dir();
        for i in 0..100u32 {
            let name = format!(
                "tmp_objdir-incoming-{:06x}",
                (std::process::id() ^ (now() as u32).wrapping_mul(2654435761) ^ i) & 0xffffff
            );
            let dir = base.join(name);
            if std::fs::create_dir(&dir).is_ok() {
                let _ = std::fs::create_dir(dir.join("pack"));
                self.quarantine = Some(dir.clone());
                return Ok(dir);
            }
        }
        Err(m::CANNOT_CREATE_QUARANTINE.to_string())
    }

    fn remove_keep(&mut self) {
        if let Some(k) = self.keep_file.take() {
            let _ = std::fs::remove_file(k);
        }
    }

    /// Runs `merge-base --is-ancestor`.
    fn merge_base_is_ancestor(&self, a: &ObjectId, b: &ObjectId) -> bool {
        self.child(&["merge-base", "--is-ancestor", &a.to_hex(), &b.to_hex()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    fn update_one(&mut self, changes: &[RefChange]) -> Result<(), TransactionError> {
        let store = refs_support::ref_store(&self.setup, &self.config);
        let hooks_dir = crate::hooks::for_repo(&self.ctx, &self.setup, &self.config).ok();
        let objects = Objects::open(&self.setup);
        let mut hooks = UpdateHooks::new(&self.setup, &self.config, &objects);
        if let Some(h) = hooks_dir {
            hooks = hooks.with_transaction_hook(h);
        }
        let mut tx = Transaction::new(&store);
        for c in changes {
            let u = if c.new.is_null() {
                RefUpdate::delete(c.name.clone(), self.hash, c.old)
            } else {
                match c.old {
                    Some(o) if o.is_null() => RefUpdate::create(c.name.clone(), c.new),
                    old => RefUpdate::update(c.name.clone(), c.new, old),
                }
            };
            tx.push(u.message("push"))?;
        }
        tx.commit(&mut hooks)
    }
}

/// The target of the symbolic `HEAD` in the git directory `dir`.
fn symbolic_head(dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(dir.join("HEAD")).ok()?;
    head.strip_prefix("ref: ").map(|t| t.trim_end().to_string())
}

/// Verifies the signature of a push certificate: signer, key and the
/// `%G?` letter.
fn verify_cert(config: &ConfigSet, cert: &[u8]) -> (Option<String>, Option<String>, String) {
    let (payload, sig) = rgit_object::split_signature(cert);
    let Some(sig) = sig else {
        return (None, None, "N".to_string());
    };
    let Ok(settings) = crate::signing::Settings::from_config(config) else {
        return (None, None, "E".to_string());
    };
    let check = crate::signing::verify::check(&settings, payload, sig);
    (
        check.signer.clone(),
        check.key.clone(),
        (check.result as char).to_string(),
    )
}

/// The `ng` reason for a failed reference update.
fn failure_reason(e: &TransactionError) -> &'static str {
    use rgit_refs::LockReason as L;
    match e {
        TransactionError::CannotLock { reason, .. } => match reason {
            L::Unresolvable(_) | L::MissingButExpected(_) => m::REASON_REF_NOT_EXIST,
            L::IsAt { .. } => m::REASON_REF_OLD_VALUE,
            L::AlreadyExists | L::DanglingSymrefExists => m::REASON_REF_EXISTS,
            L::BlockingDirectory { .. } | L::NameConflict { .. } | L::CannotProcessBoth { .. } => {
                m::REASON_REF_CONFLICT
            }
            L::ExpectedSymref(_) => m::REASON_REF_EXPECTED_SYMREF,
            _ => m::REASON_FAILED_REF,
        },
        TransactionError::NameConflict { .. } | TransactionError::CannotProcessBoth { .. } => {
            m::REASON_REF_CONFLICT
        }
        // The object checks of the update (missing object, non-commit on
        // a branch).
        TransactionError::Rejected(msg) if msg.starts_with("trying to write ") => {
            m::REASON_REF_NEW_VALUE
        }
        _ => m::REASON_FAILED_REF,
    }
}

/// Moves everything below `from` into `to`, packs' `.idx` files last;
/// existing files are kept.
fn migrate(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut files: Vec<(PathBuf, PathBuf)> = Vec::new();
    fn walk(
        from: &Path,
        to: &Path,
        files: &mut Vec<(PathBuf, PathBuf)>,
    ) -> std::io::Result<()> {
        for e in std::fs::read_dir(from)? {
            let e = e?;
            let src = e.path();
            let dst = to.join(e.file_name());
            if e.file_type()?.is_dir() {
                if !dst.is_dir() {
                    std::fs::create_dir_all(&dst)?;
                }
                walk(&src, &dst, files)?;
            } else {
                files.push((src, dst));
            }
        }
        Ok(())
    }
    walk(from, to, &mut files)?;
    files.sort_by_key(|(s, _)| s.extension().is_some_and(|e| e == "idx"));
    for (src, dst) in files {
        if dst.exists() {
            continue;
        }
        if std::fs::hard_link(&src, &dst).is_err() {
            std::fs::rename(&src, &dst)?;
        }
    }
    Ok(())
}

impl Backend for CliBackend {
    fn hash_kind(&self) -> HashKind {
        self.hash
    }

    fn refs(&mut self) -> Vec<(String, ObjectId)> {
        crate::refs::Refs::new(&self.setup).list("refs/")
    }

    fn alternate_tips(&mut self) -> Vec<ObjectId> {
        // Refs of the repositories our alternates belong to.
        // `core.alternateRefsCommand` replaces `for-each-ref`;
        // `core.alternateRefsPrefixes` limits it.
        let text = |key: &str| {
            self.config
                .string(key)
                .ok()
                .flatten()
                .map(|v| v.to_string())
        };
        let command = text("core.alternaterefscommand").filter(|c| !c.is_empty());
        let prefixes: Vec<String> = text("core.alternaterefsprefixes")
            .map(|p| p.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default();
        let mut tips = Vec::new();
        for alt in rgit_odb::resolve_alternates(&self.objects_dir()) {
            let Some(repo) = alt.parent() else { continue };
            let repo = std::path::absolute(repo).unwrap_or_else(|_| repo.to_path_buf());
            let mut c = match &command {
                Some(cmd) => {
                    let mut c = rgit_ident::editor::shell_command(cmd, &[repo.as_os_str()]);
                    c.env_remove("GIT_DIR");
                    c
                }
                None => {
                    let mut c = Command::new(&self.exe);
                    c.args(["for-each-ref", "--format=%(objectname)"])
                        .args(&prefixes)
                        .env("GIT_DIR", &repo);
                    c
                }
            };
            c.env_remove("GIT_OBJECT_DIRECTORY")
                .stdin(Stdio::null())
                .stderr(Stdio::null());
            if let Ok(out) = c.output() {
                for line in out.stdout.split(|&b| b == b'\n') {
                    if let Ok(oid) = ObjectId::from_hex(self.hash, line) {
                        tips.push(oid);
                    }
                }
            }
        }
        tips
    }

    fn shallow(&mut self) -> Vec<ObjectId> {
        let Ok(data) = std::fs::read(self.git_dir.join("shallow")) else {
            return Vec::new();
        };
        data.split(|&b| b == b'\n')
            .filter_map(|l| ObjectId::from_hex(self.hash, l).ok())
            .collect()
    }

    fn set_shallow_roots(&mut self, roots: &[ObjectId]) {
        let mut all: std::collections::BTreeSet<ObjectId> = self.shallow().into_iter().collect();
        all.extend(roots.iter().copied());
        let path = self
            .git_dir
            .join(format!("shallow_receive-pack_{}", std::process::id()));
        if rgit_fetch::shallow::write(&path, &all).is_ok() {
            self.shallow_file = Some(path);
        }
    }

    fn shallow_roots_of(&mut self, tip: &ObjectId, roots: &[ObjectId]) -> Vec<ObjectId> {
        let mut cmd = self.child(&["rev-list", &tip.to_hex(), "--not", "--all"]);
        cmd.stdin(Stdio::null()).stderr(Stdio::null());
        let Ok(out) = cmd.output() else {
            return Vec::new();
        };
        let listed: std::collections::HashSet<ObjectId> = out
            .stdout
            .split(|&b| b == b'\n')
            .filter_map(|l| ObjectId::from_hex(self.hash, l).ok())
            .collect();
        roots.iter().filter(|r| listed.contains(r)).copied().collect()
    }

    fn add_shallow(&mut self, roots: &[ObjectId]) {
        let path = self.git_dir.join("shallow");
        let mut all: std::collections::BTreeSet<ObjectId> = self.shallow().into_iter().collect();
        all.extend(roots.iter().copied());
        let _ = rgit_fetch::shallow::write(&path, &all);
    }

    fn is_bare(&self) -> bool {
        self.bare
    }

    fn symref_target(&mut self, name: &str) -> Option<String> {
        let store = refs_support::ref_store(&self.setup, &self.config);
        match rgit_refs::RefStore::read_raw(&store, name).ok()? {
            Some(rgit_refs::Target::Symbolic(t)) => Some(t),
            _ => None,
        }
    }

    fn head_branch(&mut self) -> Option<String> {
        crate::refs::Refs::new(&self.setup).head_target()
    }

    fn checked_out(&mut self, name: &str) -> Option<String> {
        if !self.bare && self.head_branch().as_deref() == Some(name) {
            return Some(String::new());
        }
        let dir = self.git_dir.join("worktrees");
        let mut ids: Vec<PathBuf> = std::fs::read_dir(&dir)
            .ok()?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        ids.sort();
        ids.into_iter()
            .find(|wt| symbolic_head(wt).as_deref() == Some(name))
            .map(|wt| wt.to_string_lossy().into_owned())
    }

    fn has_object(&mut self, oid: &ObjectId) -> bool {
        self.objects().contains(oid)
    }

    fn is_ancestor(&mut self, ancestor: &ObjectId, descendant: &ObjectId) -> bool {
        self.merge_base_is_ancestor(ancestor, descendant)
    }

    fn ingest(
        &mut self,
        _input: &mut dyn Read,
        opts: &IngestOptions,
        sink: &dyn Sink,
    ) -> Result<(), String> {
        self.create_quarantine()?;
        let header = format!("--pack_header={},{}", opts.version, opts.objects);
        let mut strict = String::from("--strict");
        if !opts.fsck_msgs.is_empty() {
            let list: Vec<String> = opts
                .fsck_msgs
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            strict = format!("--strict={}", list.join(","));
        }
        let program = if opts.keep_pack {
            "index-pack"
        } else {
            "unpack-objects"
        };
        let mut args: Vec<String> = vec![program.to_string()];
        if opts.keep_pack {
            let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
                .map(|h| h.trim().to_string())
                .unwrap_or_else(|_| "localhost".to_string());
            args.push("--stdin".into());
            if !self.reject_thin {
                args.push("--fix-thin".into());
            }
            args.push(format!(
                "--keep=receive-pack {} on {host}",
                std::process::id()
            ));
            args.push(header);
            if opts.fsck {
                args.push(strict);
            }
            if opts.max_input_size > 0 {
                args.push(format!("--max-input-size={}", opts.max_input_size));
            }
        } else {
            args.push(header);
            if opts.quiet {
                args.push("-q".into());
            }
            if opts.fsck {
                args.push(strict);
            }
            if opts.max_input_size > 0 {
                args.push(format!("--max-input-size={}", opts.max_input_size));
            }
        }
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut cmd = self.child(&argv);
        // The child reads the pack straight from our standard input.
        cmd.stdin(Stdio::inherit()).stdout(Stdio::piped());
        if sink.is_sideband() {
            cmd.stderr(Stdio::piped());
        }
        let mut child = cmd.spawn().map_err(|_| m::abnormal_exit(program))?;
        let stderr = child.stderr.take();
        let mut stdout = child.stdout.take();
        let (status, out) = std::thread::scope(|scope| {
            let relay = stderr.map(|e| scope.spawn(move || copy_to_sink(e, sink)));
            let mut out = Vec::new();
            if let Some(o) = stdout.as_mut() {
                let _ = o.read_to_end(&mut out);
            }
            let status = child.wait();
            if let Some(r) = relay {
                let _ = r.join();
            }
            (status, out)
        });
        if !status.is_ok_and(|s| s.success()) {
            return Err(m::abnormal_exit(program));
        }
        if opts.keep_pack {
            let text = String::from_utf8_lossy(&out);
            if let Some(hex) = text.strip_prefix("keep\t") {
                self.keep_file = Some(
                    self.objects_dir()
                        .join("pack")
                        .join(format!("pack-{}.keep", hex.trim())),
                );
            }
        }
        Ok(())
    }

    fn is_connected(&mut self, tips: &[ObjectId], _sink: &dyn Sink) -> bool {
        let mut cmd = self.child(&["rev-list", "--objects", "--stdin", "--not", "--all", "--quiet"]);
        cmd.stdin(Stdio::piped()).stdout(Stdio::null());
        let Ok(mut child) = cmd.spawn() else {
            return false;
        };
        if let Some(mut i) = child.stdin.take() {
            let mut input = String::new();
            for t in tips {
                input.push_str(&t.to_hex());
                input.push('\n');
            }
            let _ = i.write_all(input.as_bytes());
        }
        child.wait().is_ok_and(|s| s.success())
    }

    fn run_hook(&mut self, name: &str, call: &HookCall<'_>, sink: &dyn Sink) -> Option<i32> {
        let mut opts = rgit_hook::RunOptions::with_args(call.args.iter().cloned());
        if let Some(input) = call.stdin {
            opts = opts.stdin(rgit_hook::Input::Data(input));
        }
        for (k, v) in self.quarantine_env() {
            opts = opts.env(k, v);
        }
        for (k, v) in &call.env {
            opts = opts.env(k, v);
        }
        let hooks = &self.hooks;
        let outcome = relay_stderr(sink, || hooks.run(name, &opts));
        match outcome {
            Ok(o) if o.ran > 0 => Some(o.code),
            Ok(_) => None,
            Err(e) => {
                sink.write(format!("error: {e}\n").as_bytes());
                Some(1)
            }
        }
    }

    fn start_proc_receive(
        &mut self,
        env: &[(String, String)],
        sink: &dyn Sink,
    ) -> Option<Box<dyn ProcReceive>> {
        let hook = self.hooks.find("proc-receive").ok()?.into_iter().next()?;
        let mut cmd = match &hook.source {
            rgit_hook::Source::Configured { command, .. } => {
                rgit_ident::editor::shell_command(command, &[])
            }
            rgit_hook::Source::HookDir { path, .. } => Command::new(path),
        };
        cmd.current_dir(self.hooks.run_dir());
        cmd.envs(self.ctx.child_env());
        cmd.envs(self.quarantine_env());
        cmd.envs(env.iter().map(|(k, v)| (k, v)));
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped());
        cmd.stderr(if sink.is_sideband() {
            Stdio::piped()
        } else {
            Stdio::inherit()
        });
        let mut child = cmd.spawn().ok()?;
        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        let stderr = child
            .stderr
            .take()
            .map(|e| Box::new(e) as Box<dyn Read + Send>);
        Some(Box::new(ProcHook {
            child,
            stdin: Some(stdin),
            stdout,
            stderr,
            closed: std::io::sink(),
        }))
    }

    fn migrate_quarantine(&mut self) -> Result<(), String> {
        let Some(q) = self.quarantine.take() else {
            return Ok(());
        };
        let result = migrate(&q, &self.objects_dir());
        let _ = std::fs::remove_dir_all(&q);
        self.objects = None;
        result.map_err(|_| m::CANNOT_MIGRATE.to_string())
    }

    fn discard_quarantine(&mut self) {
        if let Some(q) = self.quarantine.take() {
            let _ = std::fs::remove_dir_all(q);
        }
        self.keep_file = None;
    }

    fn update_refs(
        &mut self,
        changes: &[RefChange],
        atomic: bool,
    ) -> Result<Vec<Result<(), RefFailure>>, String> {
        self.objects = None;
        if atomic {
            return self
                .update_one(changes)
                .map(|()| changes.iter().map(|_| Ok(())).collect())
                .map_err(|e| e.to_string());
        }
        // Like git's transaction that rejects single updates: the
        // expected old values are checked first, the rest goes into one
        // transaction. Should that fail (a conflict found while
        // locking), each update is tried on its own.
        let refs = crate::refs::Refs::new(&self.setup);
        let mut results: Vec<Result<(), RefFailure>> = Vec::new();
        let mut good = Vec::new();
        for c in changes {
            let current = refs.resolve(&c.name);
            let reason = match (c.old, current) {
                (Some(o), Some(_)) if o.is_null() && !c.new.is_null() => {
                    Some(rgit_refs::LockReason::AlreadyExists)
                }
                (Some(o), None) if !o.is_null() => {
                    Some(rgit_refs::LockReason::Unresolvable(c.name.clone()))
                }
                (Some(o), Some(cur)) if !o.is_null() && o != cur => {
                    Some(rgit_refs::LockReason::IsAt {
                        actual: cur.to_hex(),
                        expected: o.to_hex(),
                    })
                }
                _ => None,
            };
            match reason {
                Some(reason) => {
                    let e = TransactionError::CannotLock {
                        name: c.name.clone(),
                        reason,
                    };
                    results.push(Err(RefFailure {
                        reason: failure_reason(&e).to_string(),
                        message: e.to_string(),
                    }));
                }
                None => {
                    good.push(c.clone());
                    results.push(Ok(()));
                }
            }
        }
        if good.is_empty() || self.update_one(&good).is_ok() {
            return Ok(results);
        }
        let mut k = 0;
        for (i, c) in changes.iter().enumerate() {
            if results[i].is_err() {
                continue;
            }
            debug_assert_eq!(&good[k], c);
            k += 1;
            results[i] = self
                .update_one(std::slice::from_ref(c))
                .map_err(|e| RefFailure {
                    reason: failure_reason(&e).to_string(),
                    message: e.to_string(),
                });
        }
        Ok(results)
    }

    fn update_worktree(
        &mut self,
        worktree: &str,
        new: &ObjectId,
        env: &[(String, String)],
        sink: &dyn Sink,
    ) -> Result<(), String> {
        // The main work tree (key ""), or a linked one: its git directory
        // `worktrees/<id>`, whose `gitdir` names `<work tree>/.git`.
        let (wt_git_dir, work_tree) = if worktree.is_empty() {
            let w = rgit_repo::path::normalize(&self.git_dir.join(&self.work_tree));
            (self.git_dir.clone(), w)
        } else {
            let d = PathBuf::from(worktree);
            let link = std::fs::read_to_string(d.join("gitdir")).unwrap_or_default();
            let link = PathBuf::from(link.trim_end());
            let w = link
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| link.clone());
            (d, w)
        };
        let base_env: Vec<(OsString, OsString)> = vec![
            ("GIT_DIR".into(), wt_git_dir.clone().into_os_string()),
            ("GIT_WORK_TREE".into(), work_tree.clone().into_os_string()),
        ];
        let run = |args: &[&str], stdout_null: bool| -> bool {
            let mut c = Command::new(&self.exe);
            c.args(args)
                .envs(self.ctx.child_env())
                .envs(base_env.iter().cloned())
                .current_dir(&work_tree)
                .stdin(Stdio::null());
            if stdout_null {
                c.stdout(Stdio::null());
            }
            c.status().is_ok_and(|s| s.success())
        };
        let refs = crate::refs::Refs::new(&self.setup);
        let head = match symbolic_head(&wt_git_dir) {
            Some(target) => refs.resolve(&target),
            None => std::fs::read_to_string(wt_git_dir.join("HEAD"))
                .ok()
                .and_then(|h| ObjectId::from_hex(self.hash, h.trim().as_bytes()).ok()),
        };
        if self.hooks.exists("push-to-checkout") {
            let mut opts = rgit_hook::RunOptions::with_args([new.to_hex()]);
            for (k, v) in &base_env {
                opts = opts.env(k, v);
            }
            for (k, v) in env {
                opts = opts.env(k, v);
            }
            let hooks = &self.hooks;
            let outcome = relay_stderr(sink, || hooks.run("push-to-checkout", &opts));
            return match outcome {
                Ok(o) if o.ok() => Ok(()),
                _ => Err(m::REASON_PUSH_TO_CHECKOUT.to_string()),
            };
        }
        if !run(&["update-index", "-q", "--ignore-submodules", "--refresh"], true) {
            return Err(m::UP_TO_DATE_CHECK_FAILED.to_string());
        }
        if !run(&["diff-files", "--quiet", "--ignore-submodules", "--"], true) {
            return Err(m::UNSTAGED_CHANGES.to_string());
        }
        let empty_tree = rgit_hash::hash_object(self.hash, "tree", b"")
            .map(|o| o.to_hex())
            .unwrap_or_default();
        let base = head.map(|h| h.to_hex()).unwrap_or(empty_tree);
        if !run(
            &["diff-index", "--quiet", "--cached", "--ignore-submodules", &base, "--"],
            true,
        ) {
            return Err(m::STAGED_CHANGES.to_string());
        }
        let new_hex = new.to_hex();
        let ok = match head {
            Some(h) => run(&["read-tree", "-u", "-m", &h.to_hex(), &new_hex], false),
            None => run(&["read-tree", "-u", "-m", &new_hex], false),
        };
        if !ok {
            return Err(m::COULD_NOT_UPDATE_WORKTREE.to_string());
        }
        Ok(())
    }

    fn store_push_cert(&mut self, cert: &[u8]) -> Option<rp::PushCertInfo> {
        let mut cmd = self.child(&["hash-object", "-w", "--stdin"]);
        cmd.env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_QUARANTINE_PATH")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd.spawn().ok()?;
        if let Some(mut i) = child.stdin.take() {
            let _ = i.write_all(cert);
        }
        let out = child.wait_with_output().ok()?;
        let blob = ObjectId::from_hex(self.hash, String::from_utf8_lossy(&out.stdout).trim().as_bytes()).ok();
        let (signer, key, status) = verify_cert(&self.config, cert);
        Some(rp::PushCertInfo {
            blob,
            signer,
            key,
            status,
        })
    }

    fn nonce_age(&mut self, sent: &str) -> Option<i64> {
        let seed = self.nonce_seed.as_ref()?;
        let (stamp, _) = sent.split_once('-')?;
        let stamp: i64 = stamp.parse().ok()?;
        if nonce(seed, &self.git_dir, stamp) != sent {
            return None;
        }
        Some(stamp - now())
    }

    fn after_push(&mut self, auto_gc: bool, update_server_info: bool, sink: &dyn Sink) {
        self.remove_keep();
        if auto_gc {
            let ctx = &self.ctx;
            relay_stderr(sink, || crate::maintenance::run_auto_quiet(ctx, true));
        }
        if update_server_info {
            let mut c = self.child(&["update-server-info"]);
            c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
            let _ = c.status();
        }
    }
}

/// A running `proc-receive` hook.
struct ProcHook {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    stdout: std::process::ChildStdout,
    stderr: Option<Box<dyn Read + Send>>,
    /// Stands in for the standard input once it is closed.
    closed: std::io::Sink,
}

impl ProcReceive for ProcHook {
    fn input(&mut self) -> &mut dyn Write {
        match self.stdin.as_mut() {
            Some(s) => s,
            None => &mut self.closed,
        }
    }

    fn output(&mut self) -> &mut dyn Read {
        &mut self.stdout
    }

    fn take_stderr(&mut self) -> Option<Box<dyn Read + Send>> {
        self.stderr.take()
    }

    fn finish(&mut self) -> i32 {
        self.stdin = None;
        match self.child.wait() {
            Ok(s) => s.code().unwrap_or(1),
            Err(_) => 1,
        }
    }
}
