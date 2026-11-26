// SPDX-License-Identifier: MIT
//! `git receive-pack` (Documentation/git-receive-pack.adoc,
//! gitprotocol-pack.adoc "Pushing Data To a Server",
//! gitprotocol-capabilities.adoc, config/receive.adoc, githooks.adoc).
//!
//! [`serve`] runs one push: the reference advertisement, the command list
//! (with shallow lines, a push certificate and push options), the pack,
//! the checks and policies, the hooks, the reference updates and the
//! status report. Everything that touches the repository goes through
//! [`Backend`], so the protocol logic can be driven in memory.
//!
//! Order of work, as git does it:
//!
//! 1. Advertise (unless `--stateless-rpc` without `--advertise-refs`).
//! 2. Read the commands; read push options if negotiated.
//! 3. If any command creates or updates a reference, read the pack header
//!    and let the backend store the pack in a quarantine directory
//!    (`unpack-objects` below `receive.unpackLimit` objects, else
//!    `index-pack`).
//! 4. Reject commands on hidden references, check connectivity, run
//!    `pre-receive` (still in quarantine), then move the objects into the
//!    repository.
//! 5. Hand commands matching `receive.procReceiveRefs` to `proc-receive`.
//! 6. Check every remaining command (name, current branch, deletions,
//!    fast-forwards, `update` hook, `updateInstead`) and update the
//!    references: all in one transaction with `atomic`, otherwise each on
//!    its own.
//! 7. Send the status report, run `post-receive` and `post-update`, then
//!    the automatic maintenance and `update-server-info` if configured.

pub mod messages;
mod proc_receive;
pub mod settings;

use std::collections::HashSet;
use std::io::{Read, Write};
use std::sync::Mutex;

use rgit_hash::{HashKind, ObjectId};
use rgit_protocol::pktline::{self, Packet, Reader, Writer};
use rgit_protocol::sideband::{self, SidebandMode};
use rgit_refs::name::{RefNameFlags, check_refname_format};

use messages as m;
pub use proc_receive::ProcReceive;
pub use settings::{Deny, Settings};

/// Command-line options of `receive-pack`.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// `--stateless-rpc`: one request, no advertisement unless
    /// `advertise_refs`.
    pub stateless_rpc: bool,
    /// `--http-backend-info-refs` / `--advertise-refs`: only advertise.
    pub advertise_refs: bool,
    /// `--skip-connectivity-check`.
    pub skip_connectivity_check: bool,
    /// `--quiet`.
    pub quiet: bool,
    /// Protocol version 1 was requested (`GIT_PROTOCOL=version=1`).
    pub protocol_v1: bool,
    /// `GIT_NAMESPACE` as a reference prefix (`refs/namespaces/a/`), if set.
    pub namespace: Option<String>,
    /// The agent string to advertise.
    pub agent: String,
    /// The session id to advertise (with `transfer.advertiseSID`).
    pub session_id: Option<String>,
    /// The push certificate nonce to advertise (`receive.certNonceSeed`).
    pub push_cert_nonce: Option<String>,
}

/// A fatal error: `fatal: <msg>`, exit 128.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fatal(pub String);

impl From<rgit_protocol::Error> for Fatal {
    fn from(e: rgit_protocol::Error) -> Self {
        Fatal(e.to_string())
    }
}

/// Where the messages for the pushing user go: sideband 2 when the client
/// asked for a sideband, standard error otherwise.
///
/// The sink is shared: threads that relay the standard error of hooks and
/// other children write to it while the main thread goes on.
pub trait Sink: Sync {
    /// Whether the messages go over the sideband (children's standard
    /// error has to be relayed through [`Sink::write`] then).
    fn is_sideband(&self) -> bool;
    /// Raw bytes (already ending in newlines).
    fn write(&self, data: &[u8]);
}

/// How the backend stores the pack.
#[derive(Clone, Debug, Default)]
pub struct IngestOptions {
    /// Use `index-pack` (keep the pack) instead of `unpack-objects`.
    pub keep_pack: bool,
    pub quiet: bool,
    /// `receive.fsckObjects`: run with `--strict`.
    pub fsck: bool,
    /// `<msg-id>=<severity>` settings for `--strict=`.
    pub fsck_msgs: Vec<(String, String)>,
    /// `receive.maxInputSize` (0: unlimited).
    pub max_input_size: u64,
    /// Pack version and object count from the header.
    pub version: u32,
    pub objects: u32,
}

/// A reference change handed to [`Backend::update_refs`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefChange {
    /// Full name (with the namespace prefix).
    pub name: String,
    /// The expected old value: `None` for no check, the null id for
    /// "must not exist".
    pub old: Option<ObjectId>,
    /// The new value; the null id deletes.
    pub new: ObjectId,
}

/// Why one reference update failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefFailure {
    /// The `ng` reason (`reference does not exist`, ...).
    pub reason: String,
    /// The error message (`cannot lock ref ...`).
    pub message: String,
}

/// A hook run: arguments, standard input and extra environment.
#[derive(Clone, Debug, Default)]
pub struct HookCall<'a> {
    pub args: Vec<String>,
    pub stdin: Option<&'a [u8]>,
    pub env: Vec<(String, String)>,
}

/// Access to the repository for [`serve`].
pub trait Backend {
    /// The repository's hash algorithm.
    fn hash_kind(&self) -> HashKind;
    /// All references (full names, sorted by name) with their values.
    fn refs(&mut self) -> Vec<(String, ObjectId)>;
    /// Tips of alternate object stores, advertised as `.have`.
    fn alternate_tips(&mut self) -> Vec<ObjectId> {
        Vec::new()
    }
    /// The server's shallow boundary (`$GIT_DIR/shallow`).
    fn shallow(&mut self) -> Vec<ObjectId> {
        Vec::new()
    }
    /// A push from a shallow repository: from now on, children treat
    /// `roots` (the client's shallow commits we do not know as shallow)
    /// as shallow too, so their missing parents are no error.
    fn set_shallow_roots(&mut self, roots: &[ObjectId]) {
        let _ = roots;
    }
    /// The commits of `roots` that the history of `tip` reaches without
    /// passing through what the references already have.
    fn shallow_roots_of(&mut self, tip: &ObjectId, roots: &[ObjectId]) -> Vec<ObjectId> {
        let _ = (tip, roots);
        Vec::new()
    }
    /// Adds commits to `$GIT_DIR/shallow` (`receive.shallowUpdate`).
    fn add_shallow(&mut self, roots: &[ObjectId]) {
        let _ = roots;
    }
    /// Whether the repository is bare.
    fn is_bare(&self) -> bool;
    /// The target of the reference `name` if it is symbolic.
    fn symref_target(&mut self, name: &str) -> Option<String> {
        let _ = name;
        None
    }
    /// The branch `HEAD` points to (full name), if it is symbolic.
    fn head_branch(&mut self) -> Option<String>;
    /// The work tree that has the branch `name` checked out (the main
    /// one of a non-bare repository or a linked one), as a key for
    /// [`Backend::update_worktree`].
    fn checked_out(&mut self, name: &str) -> Option<String> {
        let _ = name;
        None
    }
    /// Whether the object exists (after the pack was received).
    fn has_object(&mut self, oid: &ObjectId) -> bool;
    /// Whether `ancestor` is reachable from `descendant` (both commits).
    fn is_ancestor(&mut self, ancestor: &ObjectId, descendant: &ObjectId) -> bool;
    /// Stores the pack that follows on `input` (after the 12 header bytes
    /// already read) in the quarantine directory. `Err` is the unpack
    /// status (`index-pack abnormal exit`).
    fn ingest(
        &mut self,
        input: &mut dyn Read,
        opts: &IngestOptions,
        sink: &dyn Sink,
    ) -> Result<(), String>;
    /// Whether everything reachable from `tips` is present.
    fn is_connected(&mut self, tips: &[ObjectId], sink: &dyn Sink) -> bool;
    /// Runs the hook `name` (in the quarantine environment while there is
    /// one); `None` if there is no such hook.
    fn run_hook(&mut self, name: &str, call: &HookCall<'_>, sink: &dyn Sink) -> Option<i32>;
    /// Starts the `proc-receive` hook; `None` if there is none.
    fn start_proc_receive(
        &mut self,
        env: &[(String, String)],
        sink: &dyn Sink,
    ) -> Option<Box<dyn ProcReceive>> {
        let _ = (env, sink);
        None
    }
    /// Moves the received objects into the repository.
    fn migrate_quarantine(&mut self) -> Result<(), String>;
    /// Removes the quarantine directory and its objects.
    fn discard_quarantine(&mut self);
    /// Updates references. With `atomic` all or nothing (an `Err` with
    /// the message fails all); otherwise one result per change.
    fn update_refs(
        &mut self,
        changes: &[RefChange],
        atomic: bool,
    ) -> Result<Vec<Result<(), RefFailure>>, String>;
    /// `receive.denyCurrentBranch=updateInstead`: make the work tree match
    /// `new` (or run `push-to-checkout`). `Err` is the `ng` reason.
    fn update_worktree(
        &mut self,
        worktree: &str,
        new: &ObjectId,
        env: &[(String, String)],
        sink: &dyn Sink,
    ) -> Result<(), String>;
    /// Stores the push certificate as a blob and verifies its signature:
    /// `(blob, signer, key, status)`.
    fn store_push_cert(&mut self, cert: &[u8]) -> Option<PushCertInfo> {
        let _ = cert;
        None
    }
    /// Checks a nonce from a push certificate: `Some(slop)` if this
    /// repository issued it `slop` seconds ago.
    fn nonce_age(&mut self, nonce: &str) -> Option<i64> {
        let _ = nonce;
        None
    }
    /// Runs `receive.autogc` maintenance and `receive.updateServerInfo`
    /// after a push that changed something.
    fn after_push(&mut self, auto_gc: bool, update_server_info: bool, sink: &dyn Sink) {
        let _ = (auto_gc, update_server_info, sink);
    }
}

/// What the backend reports about a stored push certificate.
#[derive(Clone, Debug, Default)]
pub struct PushCertInfo {
    pub blob: Option<ObjectId>,
    pub signer: Option<String>,
    pub key: Option<String>,
    /// `%G?` status letter.
    pub status: String,
}

/// One `option` line of `report-status-v2` (from `proc-receive`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RefReport {
    pub refname: Option<String>,
    pub old: Option<ObjectId>,
    pub new: Option<ObjectId>,
    pub forced_update: bool,
}

/// One command of the push.
#[derive(Clone, Debug)]
pub struct Command {
    pub old: ObjectId,
    pub new: ObjectId,
    /// The name as sent by the client (without namespace).
    pub name: String,
    /// The `ng` reason.
    pub error: Option<String>,
    /// Not updated by us (handled by `proc-receive`, or failed earlier).
    pub skip_update: bool,
    pub did_not_exist: bool,
    pub run_proc_receive: bool,
    /// `proc-receive` reports for `report-status-v2`; a report with no
    /// fields is a plain `ok`.
    pub reports: Vec<RefReport>,
    /// Shallow commits the new value needs (`receive.shallowUpdate`).
    pub shallow_roots: Vec<ObjectId>,
}

impl Command {
    fn is_delete(&self) -> bool {
        self.new.is_null()
    }
}

/// The client's requested capabilities.
#[derive(Clone, Debug, Default)]
struct Requested {
    report_status: bool,
    report_status_v2: bool,
    sideband: Option<SidebandMode>,
    quiet: bool,
    atomic: bool,
    push_options: bool,
}

/// Output channel: the pkt-line writer plus the sideband state.
struct Out<W: Write> {
    writer: Mutex<Writer<W>>,
    sideband: Option<SidebandMode>,
}

impl<W: Write + Send> Sink for Out<W> {
    fn is_sideband(&self) -> bool {
        self.sideband.is_some()
    }

    fn write(&self, data: &[u8]) {
        match self.sideband {
            Some(mode) => {
                let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
                let _ = sideband::send(&mut w, mode, 2, data);
                let _ = w.flush_io();
            }
            None => {
                let mut e = std::io::stderr();
                let _ = e.write_all(data);
                let _ = e.flush();
            }
        }
    }
}

impl<W: Write + Send> Out<W> {
    fn writer(&self) -> std::sync::MutexGuard<'_, Writer<W>> {
        self.writer.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn error(&self, msg: &str) {
        self.write(format!("error: {msg}\n").as_bytes());
    }

    fn warning(&self, msg: &str) {
        self.write(format!("warning: {msg}\n").as_bytes());
    }
}

/// The state of one push.
struct Push<'a, B: Backend, W: Write + Send> {
    backend: &'a mut B,
    settings: &'a Settings,
    opts: &'a Options,
    out: Out<W>,
    hash: HashKind,
    req: Requested,
    commands: Vec<Command>,
    push_options: Option<Vec<String>>,
    shallow: Vec<ObjectId>,
    /// `GIT_PUSH_CERT*` variables for the hooks.
    cert_env: Vec<(String, String)>,
    head: Option<String>,
    /// The client's shallow commits that are new to us.
    shallow_roots: Vec<ObjectId>,
}

/// Runs `receive-pack` on `input`/`output`.
pub fn serve<B: Backend, R: Read, W: Write + Send>(
    backend: &mut B,
    settings: &Settings,
    opts: &Options,
    input: R,
    output: W,
) -> Result<(), Fatal> {
    let hash = backend.hash_kind();
    let mut writer = Writer::new(output, "receive-pack");
    if opts.advertise_refs || !opts.stateless_rpc {
        advertise(backend, settings, opts, &mut writer)?;
        writer.flush_io()?;
        if opts.advertise_refs {
            return Ok(());
        }
    }
    let mut reader = Reader::new(input, "receive-pack");
    let mut push = Push {
        backend,
        settings,
        opts,
        out: Out {
            writer: Mutex::new(writer),
            sideband: None,
        },
        hash,
        req: Requested::default(),
        commands: Vec::new(),
        push_options: None,
        shallow: Vec::new(),
        cert_env: Vec::new(),
        head: None,
        shallow_roots: Vec::new(),
    };
    let cert = push.read_commands(&mut reader)?;
    if push.commands.is_empty() {
        return Ok(());
    }
    if push.req.push_options {
        let mut options = Vec::new();
        loop {
            match reader.read()? {
                Packet::Flush => break,
                p => {
                    if let Some(d) = p.data() {
                        options.push(String::from_utf8_lossy(d).into_owned());
                    }
                }
            }
        }
        push.push_options = Some(options);
    } else if settings.advertise_push_options {
        // Observed with git 2.56: when push options are advertised, the
        // hooks see `GIT_PUSH_OPTION_COUNT=0` even if the client did not
        // ask for them.
        push.push_options = Some(Vec::new());
    }
    if let Some(cert) = cert {
        push.check_cert(&cert);
        // The options in the certificate must be the ones sent after it.
        if let Some(options) = push.push_options.as_ref().filter(|_| push.req.push_options) {
            let signed: Vec<String> = cert
                .split(|&b| b == b'\n')
                .take_while(|l| !l.is_empty())
                .filter_map(|l| l.strip_prefix(b"push-option "))
                .map(|o| String::from_utf8_lossy(o).into_owned())
                .collect();
            if &signed != options {
                for c in &mut push.commands {
                    c.error = Some(m::REASON_INCONSISTENT_OPTIONS.to_string());
                }
            }
        }
    }
    push.out.sideband = push.req.sideband;
    if !push.shallow.is_empty() {
        let ours: HashSet<ObjectId> = push.backend.shallow().into_iter().collect();
        push.shallow_roots = push
            .shallow
            .iter()
            .filter(|o| !ours.contains(o))
            .copied()
            .collect();
        if !push.shallow_roots.is_empty() {
            push.backend.set_shallow_roots(&push.shallow_roots);
        }
    }
    let unpack_status = if push.commands.iter().any(|c| !c.is_delete()) {
        push.unpack(reader.get_mut())
    } else {
        Ok(())
    };
    push.execute(&unpack_status);
    push.report(&unpack_status)?;
    push.after(&unpack_status);
    let mut w = push.out.writer();
    if push.out.sideband.is_some() {
        w.flush_pkt()?;
    }
    w.flush_io()?;
    Ok(())
}

/// The capability list of the first advertised line.
fn capabilities(settings: &Settings, opts: &Options, hash: HashKind) -> String {
    let mut caps = String::from(m::BASE_CAPABILITIES);
    if settings.advertise_atomic {
        caps.push_str(" atomic");
    }
    caps.push_str(" ofs-delta");
    if let Some(nonce) = &opts.push_cert_nonce {
        caps.push_str(&format!(" push-cert={nonce}"));
    }
    if settings.advertise_push_options {
        caps.push_str(" push-options");
    }
    if settings.advertise_sid {
        if let Some(sid) = &opts.session_id {
            caps.push_str(&format!(" session-id={sid}"));
        }
    }
    caps.push_str(&format!(" object-format={}", hash.name()));
    caps.push_str(&format!(" agent={}", opts.agent));
    caps
}

fn advertise<B: Backend, W: Write>(
    backend: &mut B,
    settings: &Settings,
    opts: &Options,
    writer: &mut Writer<W>,
) -> Result<(), Fatal> {
    let hash = backend.hash_kind();
    if opts.protocol_v1 {
        writer.write_line("version 1")?;
    }
    let caps = capabilities(settings, opts, hash);
    let mut lines: Vec<(ObjectId, String)> = Vec::new();
    let mut haves: HashSet<ObjectId> = HashSet::new();
    for (name, oid) in backend.refs() {
        let stripped = match &opts.namespace {
            Some(ns) => match name.strip_prefix(ns.as_str()) {
                Some(rest) => rest.to_string(),
                None => {
                    if haves.insert(oid) {
                        lines.push((oid, ".have".to_string()));
                    }
                    continue;
                }
            },
            None => name.clone(),
        };
        if settings::is_hidden(&settings.hide_refs, &name, &stripped) {
            continue;
        }
        lines.push((oid, stripped));
    }
    // Tips of alternates that a local reference already shows are left
    // out.
    let local: HashSet<ObjectId> = lines.iter().map(|(oid, _)| *oid).collect();
    for oid in backend.alternate_tips() {
        if !local.contains(&oid) && haves.insert(oid) {
            lines.push((oid, ".have".to_string()));
        }
    }
    let mut first = true;
    let mut out = Vec::new();
    for (oid, name) in &lines {
        out.clear();
        out.extend_from_slice(oid.to_hex().as_bytes());
        out.push(b' ');
        out.extend_from_slice(&rgit_bstr::to_bytes(name));
        if first {
            out.push(0);
            out.extend_from_slice(caps.as_bytes());
            first = false;
        }
        out.push(b'\n');
        writer.write_data(&out)?;
    }
    if first {
        let line = format!(
            "{} capabilities^{{}}\0{caps}\n",
            ObjectId::null(hash).to_hex()
        );
        writer.write_data(line.as_bytes())?;
    }
    for oid in backend.shallow() {
        writer.write_line(&format!("shallow {}", oid.to_hex()))?;
    }
    writer.flush_pkt()?;
    Ok(())
}

/// Parses `<old> SP <new> SP <name>`.
fn parse_command(hash: HashKind, line: &[u8]) -> Option<Command> {
    let hl = hash.hex_len();
    if line.len() < 2 * hl + 3 || line[hl] != b' ' || line[2 * hl + 1] != b' ' {
        return None;
    }
    let old = ObjectId::from_hex(hash, &line[..hl]).ok()?;
    let new = ObjectId::from_hex(hash, &line[hl + 1..2 * hl + 1]).ok()?;
    let name = rgit_bstr::from_bytes(&line[2 * hl + 2..]).into_owned();
    Some(Command {
        old,
        new,
        name,
        error: None,
        skip_update: false,
        did_not_exist: false,
        run_proc_receive: false,
        reports: Vec::new(),
        shallow_roots: Vec::new(),
    })
}

impl<B: Backend, W: Write + Send> Push<'_, B, W> {
    /// Parses the capability list sent with the first command.
    fn parse_caps(&mut self, caps: &[u8]) -> Result<(), Fatal> {
        for word in caps.split(|&b| b == b' ').filter(|w| !w.is_empty()) {
            let word = String::from_utf8_lossy(word);
            let (key, value) = match word.split_once('=') {
                Some((k, v)) => (k, Some(v)),
                None => (word.as_ref(), None),
            };
            match key {
                "report-status" => self.req.report_status = true,
                "report-status-v2" => self.req.report_status_v2 = true,
                "side-band-64k" => self.req.sideband = Some(SidebandMode::Large),
                "side-band" => {
                    if self.req.sideband.is_none() {
                        self.req.sideband = Some(SidebandMode::Small)
                    }
                }
                "quiet" => self.req.quiet = true,
                "atomic" if self.settings.advertise_atomic => self.req.atomic = true,
                "push-options" if self.settings.advertise_push_options => {
                    self.req.push_options = true
                }
                "object-format" => {
                    let v = value.unwrap_or("");
                    if HashKind::from_name(v) != Some(self.hash) {
                        return Err(Fatal(m::unsupported_object_format(v)));
                    }
                }
                "session-id" => {
                    if let Some(v) = value {
                        rgit_trace::trace2::data("transfer", "client-sid", v, true);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Reads shallow lines and the command list (or a push certificate).
    /// Returns the certificate, if one was sent.
    fn read_commands<R: Read>(&mut self, reader: &mut Reader<R>) -> Result<Option<Vec<u8>>, Fatal> {
        let mut cert: Option<Vec<u8>> = None;
        let mut first = true;
        loop {
            let pkt = reader.read()?;
            let line = match pkt {
                Packet::Flush => break,
                p => match p.data() {
                    Some(d) => d.to_vec(),
                    None => continue,
                },
            };
            if let Some(rest) = line.strip_prefix(b"shallow ") {
                match ObjectId::from_hex(self.hash, rest) {
                    Ok(oid) if rest.len() == self.hash.hex_len() => self.shallow.push(oid),
                    _ => {
                        return Err(Fatal(m::expected_shallow(&String::from_utf8_lossy(&line))));
                    }
                }
                continue;
            }
            let (body, caps) = match line.iter().position(|&b| b == 0) {
                Some(i) => (&line[..i], Some(&line[i + 1..])),
                None => (&line[..], None),
            };
            if first {
                if let Some(c) = caps {
                    self.parse_caps(c)?;
                }
                first = false;
            }
            if body == b"push-cert" {
                cert = Some(self.read_cert(reader)?);
                continue;
            }
            match parse_command(self.hash, body) {
                Some(c) => self.commands.push(c),
                None => {
                    return Err(Fatal(m::expected_old_new_ref(&String::from_utf8_lossy(
                        body,
                    ))));
                }
            }
        }
        Ok(cert)
    }

    /// Reads a push certificate up to `push-cert-end`; its commands are
    /// the push's commands.
    fn read_cert<R: Read>(&mut self, reader: &mut Reader<R>) -> Result<Vec<u8>, Fatal> {
        reader.chomp_newline(false);
        let mut cert = Vec::new();
        let result = (|| -> Result<(), Fatal> {
            loop {
                let pkt = reader.read()?;
                let Some(d) = pkt.data() else {
                    continue;
                };
                if d == b"push-cert-end\n" {
                    return Ok(());
                }
                cert.extend_from_slice(d);
            }
        })();
        reader.chomp_newline(true);
        result?;
        // The commands follow the header block, up to the signature.
        let mut in_body = false;
        for line in cert.split(|&b| b == b'\n') {
            if !in_body {
                if line.is_empty() {
                    in_body = true;
                }
                continue;
            }
            if line.starts_with(b"-----BEGIN") {
                break;
            }
            if let Some(c) = parse_command(self.hash, line) {
                self.commands.push(c);
            }
        }
        Ok(cert)
    }

    /// Stores a certificate and prepares the `GIT_PUSH_CERT*` variables.
    fn check_cert(&mut self, cert: &[u8]) {
        let Some(info) = self.backend.store_push_cert(cert) else {
            return;
        };
        let mut env = Vec::new();
        if let Some(b) = info.blob {
            env.push(("GIT_PUSH_CERT".to_string(), b.to_hex()));
        }
        env.push((
            "GIT_PUSH_CERT_SIGNER".to_string(),
            info.signer.clone().unwrap_or_default(),
        ));
        env.push((
            "GIT_PUSH_CERT_KEY".to_string(),
            info.key.clone().unwrap_or_default(),
        ));
        env.push(("GIT_PUSH_CERT_STATUS".to_string(), info.status.clone()));
        let sent_nonce = cert
            .split(|&b| b == b'\n')
            .take_while(|l| !l.is_empty())
            .find_map(|l| l.strip_prefix(b"nonce "))
            .map(|n| String::from_utf8_lossy(n).into_owned());
        if let Some(ours) = &self.opts.push_cert_nonce {
            env.push(("GIT_PUSH_CERT_NONCE".to_string(), ours.clone()));
            let status = match &sent_nonce {
                None => "MISSING".to_string(),
                Some(n) if n == ours => "OK".to_string(),
                Some(n) => match self.backend.nonce_age(n) {
                    Some(age) => {
                        if self.settings.cert_nonce_slop > 0
                            && age.abs() <= self.settings.cert_nonce_slop
                        {
                            env.pop();
                            env.push(("GIT_PUSH_CERT_NONCE".to_string(), n.clone()));
                            env.push(("GIT_PUSH_CERT_NONCE_SLOP".to_string(), age.to_string()));
                            "SLOP".to_string()
                        } else {
                            env.push(("GIT_PUSH_CERT_NONCE_SLOP".to_string(), age.to_string()));
                            "BAD".to_string()
                        }
                    }
                    None => "BAD".to_string(),
                },
            };
            env.push(("GIT_PUSH_CERT_NONCE_STATUS".to_string(), status));
        } else if let Some(n) = &sent_nonce {
            env.push(("GIT_PUSH_CERT_NONCE".to_string(), n.clone()));
            env.push((
                "GIT_PUSH_CERT_NONCE_STATUS".to_string(),
                "UNSOLICITED".to_string(),
            ));
        }
        self.cert_env = env;
    }

    /// Reads the pack header and lets the backend store the pack.
    fn unpack(&mut self, input: &mut dyn Read) -> Result<(), String> {
        let mut hdr = [0u8; 12];
        let mut got = 0;
        while got < hdr.len() {
            match input.read(&mut hdr[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        if got < hdr.len() {
            return Err(m::EOF_BEFORE_HEADER.to_string());
        }
        if &hdr[..4] != b"PACK" {
            return Err(m::SIGNATURE_MISMATCH.to_string());
        }
        let version = u32::from_be_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
        if version != 2 && version != 3 {
            return Err(m::version_unsupported(version));
        }
        let objects = u32::from_be_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);
        let opts = IngestOptions {
            keep_pack: u64::from(objects) >= self.settings.unpack_limit,
            quiet: self.req.quiet || self.opts.quiet,
            fsck: self.settings.fsck_objects,
            fsck_msgs: self.settings.fsck_msgs.clone(),
            max_input_size: self.settings.max_input_size,
            version,
            objects,
        };
        self.backend.ingest(input, &opts, &self.out)
    }

    /// The environment of the receive hooks: push options and certificate.
    fn hook_env(&self) -> Vec<(String, String)> {
        let mut env = self.cert_env.clone();
        if let Some(options) = &self.push_options {
            env.push((
                "GIT_PUSH_OPTION_COUNT".to_string(),
                options.len().to_string(),
            ));
            for (i, o) in options.iter().enumerate() {
                env.push((format!("GIT_PUSH_OPTION_{i}"), o.clone()));
            }
        }
        env
    }

    fn full_name(&self, name: &str) -> String {
        match &self.opts.namespace {
            Some(ns) => format!("{ns}{name}"),
            None => name.to_string(),
        }
    }

    /// Runs `pre-receive` or `post-receive` with one line per command
    /// that `select` accepts; `None` if there was nothing to feed or no
    /// hook.
    fn run_receive_hook(&mut self, name: &str, select: impl Fn(&Command) -> bool) -> Option<i32> {
        let mut input = Vec::new();
        for (old, new, refname) in self.updated_lines(select) {
            input.extend_from_slice(
                format!("{} {} {refname}\n", old.to_hex(), new.to_hex()).as_bytes(),
            );
        }
        if input.is_empty() {
            return None;
        }
        let call = HookCall {
            args: Vec::new(),
            stdin: Some(&input),
            env: self.hook_env(),
        };
        self.backend.run_hook(name, &call, &self.out)
    }

    /// `(old, new, name)` of the selected commands; a command handled by
    /// `proc-receive` contributes one line per reported reference.
    fn updated_lines(
        &self,
        select: impl Fn(&Command) -> bool,
    ) -> Vec<(ObjectId, ObjectId, String)> {
        let mut lines = Vec::new();
        for c in self.commands.iter().filter(|c| select(c)) {
            if c.reports.is_empty() {
                lines.push((c.old, c.new, c.name.clone()));
            }
            for r in &c.reports {
                lines.push((
                    r.old.unwrap_or(c.old),
                    r.new.unwrap_or(c.new),
                    r.refname.clone().unwrap_or_else(|| c.name.clone()),
                ));
            }
        }
        lines
    }

    fn execute(&mut self, unpack_status: &Result<(), String>) {
        if unpack_status.is_err() {
            for c in &mut self.commands {
                c.error = Some(m::REASON_UNPACKER.to_string());
            }
            self.backend.discard_quarantine();
            return;
        }
        self.head = self.backend.head_branch();
        // Hidden references cannot be updated.
        for i in 0..self.commands.len() {
            let full = self.full_name(&self.commands[i].name);
            let name = self.commands[i].name.clone();
            if self.commands[i].error.is_none()
                && settings::is_hidden(&self.settings.hide_refs, &full, &name)
            {
                self.commands[i].error = Some(m::REASON_HIDDEN.to_string());
            }
        }
        // Commands handled by proc-receive.
        let mut proc_receive = false;
        if !self.settings.proc_receive_refs.is_empty() {
            for c in &mut self.commands {
                if c.error.is_some() {
                    continue;
                }
                if self
                    .settings
                    .proc_receive_refs
                    .iter()
                    .any(|r| r.matches(&c.name, c.old.is_null(), c.new.is_null()))
                {
                    c.run_proc_receive = true;
                    proc_receive = true;
                }
            }
        }
        if !self.opts.skip_connectivity_check {
            self.check_connectivity();
        }
        if !self.shallow_roots.is_empty() {
            self.check_shallow();
        }
        if self.commands.iter().all(|c| c.error.is_some()) {
            self.backend.discard_quarantine();
            return;
        }
        if self
            .run_receive_hook("pre-receive", |c| c.error.is_none() && !c.skip_update)
            .is_some_and(|code| code != 0)
        {
            for c in &mut self.commands {
                if c.error.is_none() {
                    c.error = Some(m::REASON_PRE_RECEIVE.to_string());
                }
            }
            self.backend.discard_quarantine();
            return;
        }
        if let Err(e) = self.backend.migrate_quarantine() {
            for c in &mut self.commands {
                if c.error.is_none() {
                    c.error = Some(e.clone());
                }
            }
            return;
        }
        self.check_aliased_updates();
        if proc_receive {
            self.run_proc_receive();
            if self.req.atomic && self.commands.iter().any(|c| c.error.is_some()) {
                for c in &mut self.commands {
                    if c.error.is_none() {
                        c.error = Some(m::REASON_ATOMIC.to_string());
                    }
                }
                return;
            }
        }
        if self.req.atomic {
            self.execute_atomic();
        } else {
            self.execute_non_atomic();
        }
        let roots: Vec<ObjectId> = self
            .commands
            .iter()
            .filter(|c| c.error.is_none())
            .flat_map(|c| c.shallow_roots.iter().copied())
            .collect();
        if !roots.is_empty() {
            self.backend.add_shallow(&roots);
        }
    }

    /// Commands whose new history ends at shallow commits we do not have
    /// as shallow need `receive.shallowUpdate`.
    fn check_shallow(&mut self) {
        let roots = self.shallow_roots.clone();
        for i in 0..self.commands.len() {
            let c = &self.commands[i];
            if c.error.is_some() || c.is_delete() {
                continue;
            }
            let new = c.new;
            let needed = self.backend.shallow_roots_of(&new, &roots);
            if needed.is_empty() {
                continue;
            }
            if self.settings.shallow_update {
                self.commands[i].shallow_roots = needed;
            } else {
                self.commands[i].error = Some(m::REASON_SHALLOW.to_string());
            }
        }
    }

    /// A command on a symbolic reference and one on its target: the same
    /// change is done once (the symbolic one is skipped), different
    /// changes are both refused.
    fn check_aliased_updates(&mut self) {
        let abbrev = |o: &ObjectId| o.to_hex_with_len(7);
        for i in 0..self.commands.len() {
            if self.commands[i].error.is_some() || self.commands[i].run_proc_receive {
                continue;
            }
            let full = self.full_name(&self.commands[i].name);
            let Some(target) = self.backend.symref_target(&full) else {
                continue;
            };
            let Some(j) = (0..self.commands.len()).find(|&j| {
                j != i
                    && self.commands[j].error.is_none()
                    && self.full_name(&self.commands[j].name) == target
            }) else {
                continue;
            };
            let (a, b) = (&self.commands[i], &self.commands[j]);
            if a.old == b.old && a.new == b.new {
                self.commands[i].skip_update = true;
                continue;
            }
            let msg = m::inconsistent_aliased(
                &a.name,
                &abbrev(&a.old),
                &abbrev(&a.new),
                &b.name,
                &abbrev(&b.old),
                &abbrev(&b.new),
            );
            self.out.error(&msg);
            self.commands[i].error = Some(m::REASON_ALIASED.to_string());
            self.commands[j].error = Some(m::REASON_ALIASED.to_string());
        }
    }

    /// `missing necessary objects` for commands whose new value is not
    /// complete in the repository plus the received pack.
    fn check_connectivity(&mut self) {
        let tips: Vec<ObjectId> = self
            .commands
            .iter()
            .filter(|c| c.error.is_none() && !c.is_delete())
            .map(|c| c.new)
            .collect();
        if tips.is_empty() || self.backend.is_connected(&tips, &self.out) {
            return;
        }
        for i in 0..self.commands.len() {
            let c = &self.commands[i];
            if c.error.is_some() || c.is_delete() {
                continue;
            }
            let tip = [c.new];
            if !self.backend.is_connected(&tip, &self.out) {
                self.commands[i].error = Some(m::REASON_MISSING_OBJECTS.to_string());
            }
        }
    }

    /// The checks of one command; `Ok(Some(change))` queues an update.
    fn check(&mut self, i: usize) -> Result<Option<RefChange>, String> {
        let cmd = self.commands[i].clone();
        let name = cmd.name.as_str();
        let full = self.full_name(name);
        // A deletion may name a one-level reference (`refs/foo`).
        let flags = if cmd.is_delete() {
            RefNameFlags::ONELEVEL
        } else {
            RefNameFlags::STRICT
        };
        let refs_rest = name.strip_prefix("refs/");
        if refs_rest.is_none_or(|rest| check_refname_format(rest.as_bytes(), flags).is_err()) {
            self.out.error(&m::funny_ref(name));
            return Err(m::REASON_FUNNY.to_string());
        }
        let checkout = self.backend.checked_out(&full);
        let is_current = checkout.is_some();
        if is_current {
            match self.settings.deny_current_branch {
                Deny::Ignore | Deny::UpdateInstead => {}
                Deny::Warn => self.out.warning(m::UPDATING_CURRENT),
                Deny::Refuse | Deny::Unconfigured => {
                    self.out.error(&m::checked_out(&full));
                    if self.settings.deny_current_branch == Deny::Unconfigured {
                        self.out.error(m::REFUSE_CURRENT_BRANCH);
                    }
                    return Err(m::REASON_CHECKED_OUT.to_string());
                }
            }
        }
        if !cmd.is_delete() && !self.backend.has_object(&cmd.new) {
            self.out.error(&m::should_have_generated(&cmd.new.to_hex()));
            return Err(m::REASON_BAD_PACK.to_string());
        }
        // The deletion policies apply to a deletion of an existing value.
        if cmd.is_delete() && !cmd.old.is_null() {
            if self.settings.deny_deletes && name.starts_with("refs/heads/") {
                self.out.error(&m::deny_deletion(&full));
                return Err(m::REASON_DELETION.to_string());
            }
            if is_current || self.head.as_deref() == Some(full.as_str()) {
                match self.settings.deny_delete_current {
                    Deny::Ignore => {}
                    Deny::Warn => self.out.warning(m::DELETING_CURRENT),
                    d => {
                        if d == Deny::Unconfigured {
                            self.out.error(m::REFUSE_DELETE_CURRENT);
                        }
                        self.out.error(&m::delete_current(&full));
                        return Err(m::REASON_DELETE_CURRENT.to_string());
                    }
                }
            }
        }
        if self.settings.deny_non_fast_forwards
            && !cmd.is_delete()
            && !cmd.old.is_null()
            && name.starts_with("refs/heads/")
            && !self.backend.is_ancestor(&cmd.old, &cmd.new)
        {
            self.out.error(&m::deny_non_ff(&full));
            return Err(m::REASON_NON_FF.to_string());
        }
        let call = HookCall {
            args: vec![name.to_string(), cmd.old.to_hex(), cmd.new.to_hex()],
            stdin: None,
            // No push options for `update` (githooks(5) names them for
            // pre-receive and post-receive only).
            env: Vec::new(),
        };
        if self
            .backend
            .run_hook("update", &call, &self.out)
            .is_some_and(|code| code != 0)
        {
            self.out.error(&m::hook_declined(name));
            return Err(m::REASON_HOOK.to_string());
        }
        if is_current
            && self.settings.deny_current_branch == Deny::UpdateInstead
            && !cmd.is_delete()
        {
            let env = self.hook_env();
            if let Some(wt) = &checkout {
                self.backend
                    .update_worktree(wt, &cmd.new, &env, &self.out)?;
            }
        }
        if cmd.is_delete() {
            let mut old = Some(cmd.old);
            if cmd.old.is_null() || !self.backend.has_object(&cmd.old) {
                old = None;
                let exists = self
                    .backend
                    .refs()
                    .iter()
                    .any(|(n, _)| n.as_str() == full.as_str());
                if exists {
                    self.out.warning(m::DELETING_CORRUPT);
                } else {
                    self.out.warning(m::DELETING_NONEXISTENT);
                    self.commands[i].did_not_exist = true;
                    return Ok(None);
                }
            }
            return Ok(Some(RefChange {
                name: full,
                old,
                new: cmd.new,
            }));
        }
        Ok(Some(RefChange {
            name: full,
            old: Some(cmd.old),
            new: cmd.new,
        }))
    }

    /// The order commands are checked and applied in: deletions first
    /// (so that a deleted `a` makes room for a new `a/b`), then the rest.
    fn order(&self) -> Vec<usize> {
        let (mut del, rest): (Vec<usize>, Vec<usize>) =
            (0..self.commands.len()).partition(|&i| self.commands[i].is_delete());
        del.extend(rest);
        del
    }

    fn pending(&self, i: usize) -> bool {
        let c = &self.commands[i];
        c.error.is_none() && !c.skip_update && !c.run_proc_receive
    }

    fn execute_atomic(&mut self) {
        let mut changes = Vec::new();
        let mut failed = false;
        for i in self.order() {
            if !self.pending(i) {
                continue;
            }
            match self.check(i) {
                Ok(Some(ch)) => changes.push(ch),
                Ok(None) => {}
                Err(reason) => {
                    self.commands[i].error = Some(reason);
                    failed = true;
                    break;
                }
            }
        }
        if !failed {
            if let Some(name) = duplicate(&changes) {
                self.out.error(&m::multiple_updates(&name));
                for i in 0..self.commands.len() {
                    if self.pending(i) {
                        self.commands[i].error = Some(m::REASON_TRANSACTION.to_string());
                    }
                }
                return;
            }
            match self.backend.update_refs(&changes, true) {
                Ok(_) => return,
                Err(msg) => {
                    self.out.error(&msg);
                    for i in 0..self.commands.len() {
                        if self.pending(i) {
                            self.commands[i].error = Some(m::REASON_TRANSACTION.to_string());
                        }
                    }
                    return;
                }
            }
        }
        for c in &mut self.commands {
            if c.error.is_none() {
                c.error = Some(m::REASON_ATOMIC.to_string());
            }
        }
    }

    /// Without `atomic`: the deletions are checked and applied first, then
    /// the other commands; within each group a failing reference does not
    /// stop the others.
    fn execute_non_atomic(&mut self) {
        let (deletes, rest): (Vec<usize>, Vec<usize>) = self
            .order()
            .into_iter()
            .partition(|&i| self.commands[i].is_delete());
        self.execute_batch(&deletes);
        self.execute_batch(&rest);
    }

    fn execute_batch(&mut self, indices: &[usize]) {
        let mut changes = Vec::new();
        let mut owners = Vec::new();
        for &i in indices {
            if !self.pending(i) {
                continue;
            }
            match self.check(i) {
                Ok(Some(ch)) => {
                    changes.push(ch);
                    owners.push(i);
                }
                Ok(None) => {}
                Err(reason) => self.commands[i].error = Some(reason),
            }
        }
        if changes.is_empty() {
            return;
        }
        if let Some(name) = duplicate(&changes) {
            self.out.error(&m::multiple_updates(&name));
            for &i in &owners {
                self.commands[i].error = Some(m::REASON_FAILED_REFS.to_string());
            }
            return;
        }
        match self.backend.update_refs(&changes, false) {
            Ok(results) => {
                for (k, r) in results.into_iter().enumerate() {
                    if let Err(f) = r {
                        self.out.error(&f.message);
                        self.commands[owners[k]].error = Some(f.reason);
                    }
                }
            }
            Err(msg) => {
                self.out.error(&msg);
                for &i in &owners {
                    self.commands[i].error = Some(m::REASON_FAILED_REFS.to_string());
                }
            }
        }
    }

    fn report(&mut self, unpack_status: &Result<(), String>) -> Result<(), Fatal> {
        if !self.req.report_status && !self.req.report_status_v2 {
            return Ok(());
        }
        let mut buf = Vec::new();
        let unpack = match unpack_status {
            Ok(()) => "unpack ok\n".to_string(),
            Err(e) => format!("unpack {e}\n"),
        };
        pktline::encode_data(&mut buf, unpack.as_bytes())?;
        for c in &self.commands {
            if let Some(e) = &c.error {
                pktline::encode_data(
                    &mut buf,
                    &rgit_bstr::to_bytes(&format!("ng {} {e}\n", c.name)),
                )?;
                continue;
            }
            if c.reports.is_empty() || !self.req.report_status_v2 {
                pktline::encode_data(&mut buf, &rgit_bstr::to_bytes(&format!("ok {}\n", c.name)))?;
                continue;
            }
            for r in &c.reports {
                pktline::encode_data(&mut buf, &rgit_bstr::to_bytes(&format!("ok {}\n", c.name)))?;
                if let Some(n) = &r.refname {
                    pktline::encode_data(
                        &mut buf,
                        &rgit_bstr::to_bytes(&format!("option refname {n}\n")),
                    )?;
                }
                if let Some(o) = &r.old {
                    pktline::encode_data(
                        &mut buf,
                        format!("option old-oid {}\n", o.to_hex()).as_bytes(),
                    )?;
                }
                if let Some(n) = &r.new {
                    pktline::encode_data(
                        &mut buf,
                        format!("option new-oid {}\n", n.to_hex()).as_bytes(),
                    )?;
                }
                if r.forced_update {
                    pktline::encode_data(&mut buf, b"option forced-update\n")?;
                }
            }
        }
        buf.extend_from_slice(b"0000");
        let mut w = self.out.writer();
        match self.out.sideband {
            Some(mode) => sideband::send(&mut w, mode, 1, &buf)?,
            None => w
                .get_mut()
                .write_all(&buf)
                .map_err(|e| Fatal(e.to_string()))?,
        }
        w.flush_io()?;
        Ok(())
    }

    fn after(&mut self, unpack_status: &Result<(), String>) {
        if unpack_status.is_err() {
            return;
        }
        let succeeded = |c: &Command| c.error.is_none() && !c.did_not_exist;
        self.run_receive_hook("post-receive", |c| succeeded(c));
        let names: Vec<String> = self
            .updated_lines(succeeded)
            .into_iter()
            .map(|(_, _, n)| n)
            .collect();
        if !names.is_empty() {
            let call = HookCall {
                args: names,
                stdin: None,
                env: Vec::new(),
            };
            self.backend.run_hook("post-update", &call, &self.out);
        }
        if self.commands.iter().any(succeeded) {
            self.backend.after_push(
                self.settings.auto_gc,
                self.settings.update_server_info,
                &self.out,
            );
        }
    }

    fn run_proc_receive(&mut self) {
        proc_receive::run(self);
    }
}

/// The first reference named twice.
fn duplicate(changes: &[RefChange]) -> Option<String> {
    let mut seen = HashSet::new();
    for c in changes {
        if !seen.insert(c.name.as_str()) {
            return Some(c.name.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests;
