// SPDX-License-Identifier: MIT
//! Protocol v0 and v1 (`Documentation/gitprotocol-pack.adoc`,
//! `Documentation/gitprotocol-capabilities.adoc`): the reference
//! advertisement, the `want` list, the negotiation with `have` lines and
//! the pack. Protocol v1 is v0 with a `version 1` packet in front of the
//! advertisement.
//!
//! What the documentation leaves open was observed from git 2.56:
//!
//! - The order of the capabilities, and which of them depend on the
//!   configuration and on `--stateless-rpc`.
//! - `symref=HEAD:<branch>` is advertised whenever `HEAD` is a symbolic
//!   reference to an existing branch, also when `HEAD` is hidden.
//! - Capabilities are taken from every `want` line, not only the first.
//! - A wanted object must be the tip of a reference the client can see.
//!   `uploadpack.allowTipSHA1InWant` adds the tips of hidden references,
//!   `uploadpack.allowReachableSHA1InWant` everything those tips reach,
//!   `uploadpack.allowAnySHA1InWant` every object. With
//!   `--stateless-rpc` what the visible tips reach is allowed too (the
//!   references may have moved since the advertisement). Only commits
//!   (and the commits that tags name) are checked for reachability.
//! - The acknowledgments in the three modes (none, `multi_ack`,
//!   `multi_ack_detailed`), see [`negotiate`].
//! - The `shallow`/`unshallow` lines are only sent for a `deepen*`
//!   request and carry no LF.

use std::collections::HashSet;
use std::io::{Read, Write};

use rgit_hash::ObjectId;
use rgit_object::ObjectKind;
use rgit_pack::write::ObjectFilter;
use rgit_protocol::sideband::SidebandMode;
use rgit_protocol::{Packet, ProtocolVersion, Reader, Writer};
use rgit_trace::trace2;

use super::fetch::{self, Common, Have};
use super::graph::Graph;
use super::pack::{self, PackRequest};
use super::refs::RefView;
use super::serve::{Output, Session};
use super::shallow::{self, ShallowRequest};
use super::{Error, Options, Repository, Result, Settings, messages as msg};

/// The capabilities every advertisement starts with, in the order git
/// 2.56 lists them.
const BASE_CAPABILITIES: [&str; 12] = [
    "multi_ack",
    "thin-pack",
    "side-band",
    "side-band-64k",
    "ofs-delta",
    "shallow",
    "deepen-since",
    "deepen-not",
    "deepen-relative",
    "no-progress",
    "include-tag",
    "multi_ack_detailed",
];

/// How common objects are acknowledged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AckMode {
    /// The first common object only.
    Single,
    /// `multi_ack`: `ACK <oid> continue`.
    Multi,
    /// `multi_ack_detailed`: `ACK <oid> common` and `ACK <oid> ready`.
    Detailed,
}

/// The tips of the references of the served namespace.
struct Tips {
    /// Tips the client can see (`HEAD` included).
    visible: HashSet<ObjectId>,
    /// Tips of hidden references.
    hidden: HashSet<ObjectId>,
}

/// What the client asked for before the negotiation.
struct Request {
    wants: Vec<ObjectId>,
    shallow: ShallowRequest,
    filter: Option<ObjectFilter>,
    ack: AckMode,
    no_done: bool,
    thin_pack: bool,
    ofs_delta: bool,
    band: Option<SidebandMode>,
    no_progress: bool,
    include_tag: bool,
    /// The client named the `filter` capability (and it is allowed).
    filter_capability: bool,
    /// The client named `deepen-relative`.
    deepen_relative: bool,
}

impl Request {
    fn new() -> Request {
        Request {
            wants: Vec::new(),
            shallow: ShallowRequest::default(),
            filter: None,
            ack: AckMode::Single,
            no_done: false,
            thin_pack: false,
            ofs_delta: false,
            band: None,
            no_progress: false,
            include_tag: false,
            filter_capability: false,
            deepen_relative: false,
        }
    }

    /// Take the capabilities named after the object id of a `want` line.
    fn features(&mut self, features: &str, settings: &Settings) {
        for feature in features.split(' ') {
            match feature {
                "deepen-relative" => self.deepen_relative = true,
                "multi_ack_detailed" => self.ack = AckMode::Detailed,
                "multi_ack" if self.ack == AckMode::Single => self.ack = AckMode::Multi,
                "no-done" => self.no_done = true,
                "thin-pack" => self.thin_pack = true,
                "ofs-delta" => self.ofs_delta = true,
                "side-band-64k" => self.band = Some(SidebandMode::Large),
                "side-band" if self.band.is_none() => self.band = Some(SidebandMode::Small),
                "no-progress" => self.no_progress = true,
                "include-tag" => self.include_tag = true,
                "filter" if settings.allow_filter => self.filter_capability = true,
                _ => {
                    if let Some(sid) = feature.strip_prefix("session-id=") {
                        trace2::data("transfer", "client-sid", sid, true);
                    }
                }
            }
        }
    }
}

pub(crate) fn serve<R: Read, W: Write>(
    repo: &Repository<'_>,
    opts: &Options,
    input: R,
    output: W,
) -> Result<()> {
    let settings = Settings::load(repo, opts)?;
    let version = match opts.protocol {
        ProtocolVersion::V1 => "1",
        _ => "0",
    };
    trace2::data("transfer", "negotiated-version", version, true);
    let mut reader = Reader::new(input, "upload-pack");
    reader.err_packets(false);
    let mut s = Session {
        repo,
        opts,
        settings,
        reader,
        out: Output {
            writer: Writer::new(output, "upload-pack"),
            sideband_all: false,
        },
        promisor_accepted: false,
    };
    let view = RefView::new(repo.refs, repo.config, opts.namespace.as_deref());
    let tips = advertise(&mut s, &view, opts.advertise_refs || !opts.stateless_rpc)?;
    s.out.writer.flush_io()?;
    if opts.advertise_refs {
        return Ok(());
    }
    let served = request(&mut s, &tips);
    // What was written before a failure still reaches the client.
    let flushed = s.out.writer.flush_io();
    served?;
    flushed?;
    Ok(())
}

/// Collect the tips and, if `send`, write the advertisement: `HEAD` and
/// the references below `refs/` in name order, each tag followed by what
/// it finally names, the capabilities behind a NUL on the first line,
/// then the repository's shallow commits.
fn advertise<R: Read, W: Write>(
    s: &mut Session<'_, R, W>,
    view: &RefView<'_>,
    send: bool,
) -> Result<Tips> {
    let repo = s.repo;
    let graph = Graph::new(repo.odb, repo.hash, &[]);
    let mut tips = Tips {
        visible: HashSet::new(),
        hidden: HashSet::new(),
    };
    let mut lines: Vec<(ObjectId, String)> = Vec::new();
    let add = |id: ObjectId, name: &str, lines: &mut Vec<(ObjectId, String)>| {
        lines.push((id, name.to_owned()));
        if let Some((peeled, _)) = graph.peel(&id).filter(|(p, _)| *p != id) {
            lines.push((peeled, format!("{name}^{{}}")));
        }
    };
    let (head, _) = view.head_even_if_hidden();
    let head_visible = view.head().0.is_some();
    if let Some(head) = &head {
        if head_visible {
            tips.visible.insert(head.id);
            add(head.id, "HEAD", &mut lines);
        } else {
            tips.hidden.insert(head.id);
        }
    }
    let refs = view
        .list_with_hidden()
        .map_err(|e| Error::fatal(e.to_string()))?;
    for (r, hidden) in &refs {
        // The namespace's own `HEAD` is listed above.
        if r.name == "HEAD" {
            continue;
        }
        if *hidden {
            tips.hidden.insert(r.id);
        } else {
            tips.visible.insert(r.id);
            add(r.id, &r.name, &mut lines);
        }
    }
    if !send {
        return Ok(tips);
    }

    let st = &s.settings;
    let mut caps = BASE_CAPABILITIES.join(" ");
    if st.allow_tip {
        caps.push_str(" allow-tip-sha1-in-want");
    }
    if st.allow_reachable {
        caps.push_str(" allow-reachable-sha1-in-want");
    }
    if s.opts.stateless_rpc || s.opts.advertise_refs {
        caps.push_str(" no-done");
    }
    if let Some(target) = head.as_ref().and_then(|h| h.symref_target.as_deref()) {
        caps.push_str(&format!(" symref=HEAD:{target}"));
    }
    if st.allow_filter {
        caps.push_str(" filter");
    }
    if st.advertise_sid {
        caps.push_str(&format!(" session-id={}", trace2::session_id()));
    }
    caps.push_str(&format!(" object-format={}", repo.hash.name()));
    caps.push_str(&format!(" agent={}", rgit_protocol::agent::agent()));

    let w = &mut s.out.writer;
    if s.opts.protocol == ProtocolVersion::V1 {
        w.write_line("version 1")?;
    }
    if lines.is_empty() {
        let zero = ObjectId::null(repo.hash);
        w.write_data(format!("{zero} capabilities^{{}}\0{caps}\n").as_bytes())?;
    }
    for (i, (id, name)) in lines.iter().enumerate() {
        if i == 0 {
            w.write_data(&rgit_bstr::to_bytes(&format!("{id} {name}\0{caps}\n")))?;
        } else {
            w.write_line(&format!("{id} {name}"))?;
        }
    }
    for id in &repo.shallow {
        w.write_line(&format!("shallow {id}"))?;
    }
    w.flush_pkt()?;
    Ok(tips)
}

/// Read the client's request and answer it.
fn request<R: Read, W: Write>(s: &mut Session<'_, R, W>, tips: &Tips) -> Result<()> {
    let repo = s.repo;
    let Some(req) = receive_needs(s)? else {
        return Ok(());
    };
    check_wants(s, tips, &req.wants)?;

    let mut graph = Graph::new(repo.odb, repo.hash, &repo.shallow);
    let deepen = req.shallow.depth.is_some() || req.shallow.since.is_some() || req.shallow.has_not;
    if req.shallow.depth.is_some() && (req.shallow.since.is_some() || req.shallow.has_not) {
        return Err(Error::fatal(msg::DEEPEN_CONFLICT));
    }
    let plan = shallow::plan(&mut graph, &repo.shallow, &req.shallow, &req.wants)?;
    if deepen {
        // These lines carry no LF.
        for id in &plan.shallow {
            s.out.data(format!("shallow {id}").as_bytes())?;
        }
        for id in &plan.unshallow {
            s.out.data(format!("unshallow {id}").as_bytes())?;
        }
        s.out.flush()?;
        s.out.writer.flush_io()?;
    }

    let Some(common) = negotiate(s, &req, &mut graph)? else {
        return Ok(());
    };

    s.out.writer.flush_io()?;
    let mut tips = req.wants.clone();
    let mut seen: HashSet<ObjectId> = tips.iter().copied().collect();
    tips.extend(plan.extra_wants.iter().filter(|id| seen.insert(**id)));
    // The client has the commits that become complete (and their trees),
    // only not what is behind them.
    let mut exclude = common.objects;
    let mut seen: HashSet<ObjectId> = exclude.iter().copied().collect();
    exclude.extend(plan.unshallow.iter().filter(|id| seen.insert(**id)));
    let pack = PackRequest {
        tips,
        exclude,
        shallow: plan.pack_shallow,
        thin: req.thin_pack,
        ofs_delta: req.ofs_delta,
        include_tag: req.include_tag,
        progress: !req.no_progress,
        filter: req.filter.as_ref().map(ObjectFilter::to_spec),
        allow_promisor: false,
        band: req.band,
    };
    pack::send(s, &pack)?;
    if req.band.is_some() {
        s.out.flush()?;
    }
    Ok(())
}

/// The `want` list with the `shallow`, `deepen*` and `filter` lines, up
/// to the flush packet. `None`: the client wants nothing.
fn receive_needs<R: Read, W: Write>(s: &mut Session<'_, R, W>) -> Result<Option<Request>> {
    let repo = s.repo;
    let graph = Graph::new(repo.odb, repo.hash, &[]);
    let mut req = Request::new();
    let mut seen_wants = HashSet::new();
    loop {
        s.opts.active();
        let line = match s.reader.read_or_eof()? {
            None => return Err(Error::fatal(msg::HUNG_UP)),
            Some(Packet::Flush) => break,
            Some(Packet::Data(d)) => String::from_utf8_lossy(d).into_owned(),
            Some(other) => {
                return Err(Error::fatal(msg::expected_object_id(&other.describe())));
            }
        };
        // Every request is a keyword, a space and an argument.
        let Some((key, arg)) = line.split_once(' ') else {
            return Err(Error::fatal(msg::expected_object_id(&line)));
        };
        match key {
            "shallow" => {
                let id = fetch::oid(repo, arg)
                    .ok_or_else(|| Error::fatal(msg::invalid_shallow_line(&line)))?;
                // A commit this repository does not have cannot matter.
                match repo.odb.header(&id).ok().flatten() {
                    None => {}
                    Some(h) if h.kind == ObjectKind::Commit => req.shallow.client.push(id),
                    Some(_) => return Err(Error::fatal(msg::invalid_shallow_object(&id))),
                }
            }
            "deepen" => {
                let depth = arg.parse::<u64>().ok().filter(|d| *d > 0);
                req.shallow.depth =
                    Some(depth.ok_or_else(|| Error::fatal(msg::invalid_deepen(&line)))?);
            }
            "deepen-since" => {
                // Like git, nothing counts as 0.
                let since = if arg.is_empty() {
                    Some(0)
                } else {
                    arg.parse::<i64>().ok().filter(|t| *t >= 0)
                };
                req.shallow.since =
                    Some(since.ok_or_else(|| Error::fatal(msg::invalid_deepen_since(&line)))?);
            }
            "deepen-not" => {
                let id = fetch::resolve_ref(repo, arg)
                    .and_then(|id| graph.peel_to_commit(&id))
                    .ok_or_else(|| Error::fatal(msg::deepen_not_is_not_a_ref(&line)))?;
                req.shallow.not.push(id);
                req.shallow.has_not = true;
            }
            "filter" => {
                if !req.filter_capability {
                    return Err(Error::fatal(msg::FILTER_NOT_NEGOTIATED));
                }
                if req.filter.is_some() {
                    return Err(Error::fatal(msg::MULTIPLE_FILTERS));
                }
                let filter = ObjectFilter::parse(arg).map_err(|e| Error::fatal(e.to_string()))?;
                if let Err(m) = fetch::check_filter(repo, &filter) {
                    s.out.error(&m);
                    return Err(Error::fatal(m));
                }
                req.filter = Some(filter);
            }
            "want" => {
                let hex_len = repo.hash.hex_len();
                let id = arg
                    .get(..hex_len)
                    .and_then(|hex| fetch::oid(repo, hex))
                    .ok_or_else(|| Error::fatal(msg::expected_object_id(&line)))?;
                req.features(&arg[hex_len..], &s.settings);
                if !repo.odb.contains(&id) {
                    s.out.error(&msg::not_our_ref_packet(&id));
                    return Err(Error::fatal(msg::not_our_ref(&id)));
                }
                if seen_wants.insert(id) {
                    req.wants.push(id);
                }
            }
            _ => return Err(Error::fatal(msg::expected_object_id(&line))),
        }
    }
    if req.wants.is_empty() {
        return Ok(None);
    }
    // Without the capability a depth counts from the wanted commits.
    req.shallow.relative = req.deepen_relative;
    Ok(Some(req))
}

/// Refuse wanted objects the client may not ask for (see the module
/// documentation).
fn check_wants<R: Read, W: Write>(
    s: &mut Session<'_, R, W>,
    tips: &Tips,
    wants: &[ObjectId],
) -> Result<()> {
    let st = &s.settings;
    if st.allow_any {
        return Ok(());
    }
    let hidden_count = st.allow_tip || st.allow_reachable;
    let is_tip =
        |id: &ObjectId| tips.visible.contains(id) || (hidden_count && tips.hidden.contains(id));
    let others: Vec<ObjectId> = wants.iter().filter(|id| !is_tip(id)).copied().collect();
    if others.is_empty() {
        return Ok(());
    }
    let mut refused = others.first().copied();
    if st.allow_reachable || s.opts.stateless_rpc {
        let repo = s.repo;
        let mut graph = Graph::new(repo.odb, repo.hash, &repo.shallow);
        // Only commits are looked for; a tag stands for its commit.
        let commits: Vec<(ObjectId, ObjectId)> = others
            .iter()
            .filter_map(|id| graph.peel_to_commit(id).map(|c| (*id, c)))
            .collect();
        let mut missing: HashSet<ObjectId> = commits.iter().map(|(_, c)| *c).collect();
        let mut queue: Vec<ObjectId> = tips
            .visible
            .iter()
            .chain(tips.hidden.iter().filter(|_| hidden_count))
            .filter_map(|id| graph.peel_to_commit(id))
            .collect();
        let mut seen: HashSet<ObjectId> = queue.iter().copied().collect();
        while let Some(id) = queue.pop() {
            missing.remove(&id);
            if missing.is_empty() {
                break;
            }
            let Some(c) = graph.commit(&id) else { continue };
            for p in c.parents.clone() {
                if seen.insert(p) {
                    queue.push(p);
                }
            }
        }
        refused = commits
            .iter()
            .find(|(_, c)| missing.contains(c))
            .map(|(id, _)| *id);
    }
    match refused {
        None => Ok(()),
        Some(id) => {
            s.out.error(&msg::not_our_ref_packet(&id));
            Err(Error::Refused(msg::not_our_ref(&id)))
        }
    }
}

/// The negotiation: read `have` lines and acknowledge them until the
/// client says `done`. Returns the common objects, or `None` when no
/// pack is to be sent (`--stateless-rpc`: the client continues with
/// another request).
///
/// As git 2.56 answers:
///
/// - `have` for an object of this repository: `ACK <oid> common`
///   (`multi_ack_detailed`), `ACK <oid> continue` (`multi_ack`), or `ACK
///   <oid>` while the list of common objects has one entry.
/// - `have` for an unknown object, once every wanted commit reaches a
///   common one: `ACK <oid> ready` or `ACK <oid> continue`, so the client
///   stops walking that way.
/// - Flush packet: `ACK <last common> ready` in `multi_ack_detailed` mode
///   when the round had common objects only and the pack can be made;
///   then `NAK` unless (without `multi_ack`) a common object is known.
///   After `ready` a client with `no-done` gets `ACK <last common>` and
///   the pack at once.
/// - `done`: `ACK <last common>` in the `multi_ack` modes when there is a
///   common object, nothing without `multi_ack`; `NAK` when there is
///   none.
/// - `--stateless-rpc`: a request may end right after the `want` list
///   (a client that only asks for the `shallow` lines); that is no
///   error.
fn negotiate<R: Read, W: Write>(
    s: &mut Session<'_, R, W>,
    req: &Request,
    graph: &mut Graph<'_>,
) -> Result<Option<Common>> {
    let repo = s.repo;
    let wanted_commits: Vec<ObjectId> = req
        .wants
        .iter()
        .filter(|id| {
            repo.odb
                .header(id)
                .ok()
                .flatten()
                .is_some_and(|h| h.kind == ObjectKind::Commit)
        })
        .copied()
        .collect();
    let mut common = Common::default();
    let mut last: Option<ObjectId> = None;
    let mut got_common = false;
    let mut got_other = false;
    let mut sent_ready = false;
    let mut first = true;
    loop {
        s.opts.active();
        let packet = s.reader.read_or_eof()?;
        if packet.is_none() && first && s.opts.stateless_rpc {
            return Ok(None);
        }
        first = false;
        let line = match packet {
            None => return Err(Error::fatal(msg::HUNG_UP)),
            Some(Packet::Flush) => {
                if let Some(last) = last.filter(|_| {
                    req.ack == AckMode::Detailed
                        && got_common
                        && !got_other
                        && fetch::ready_to_send(graph, &wanted_commits, &common)
                }) {
                    sent_ready = true;
                    s.out.line(&format!("ACK {last} ready"))?;
                }
                if common.objects.is_empty() || req.ack != AckMode::Single {
                    s.out.line("NAK")?;
                }
                if let Some(last) = last.filter(|_| req.no_done && sent_ready) {
                    s.out.line(&format!("ACK {last}"))?;
                    return Ok(Some(common));
                }
                if s.opts.stateless_rpc {
                    return Ok(None);
                }
                got_common = false;
                got_other = false;
                s.out.writer.flush_io()?;
                continue;
            }
            Some(Packet::Data(d)) => String::from_utf8_lossy(d).into_owned(),
            Some(other) => return Err(Error::fatal(msg::expected_list(&other.describe()))),
        };
        if let Some(arg) = line.strip_prefix("have ") {
            let id = arg
                .get(..repo.hash.hex_len())
                .and_then(|hex| fetch::oid(repo, hex))
                .ok_or_else(|| Error::fatal(msg::expected_object(arg)))?;
            match common.add(repo, graph, &id) {
                Have::Missing => {
                    got_other = true;
                    if req.ack != AckMode::Single
                        && fetch::ready_to_send(graph, &wanted_commits, &common)
                    {
                        if req.ack == AckMode::Detailed {
                            sent_ready = true;
                            s.out.line(&format!("ACK {id} ready"))?;
                        } else {
                            s.out.line(&format!("ACK {id} continue"))?;
                        }
                    }
                }
                Have::New | Have::Known => {
                    got_common = true;
                    last = Some(id);
                    match req.ack {
                        AckMode::Detailed => s.out.line(&format!("ACK {id} common"))?,
                        AckMode::Multi => s.out.line(&format!("ACK {id} continue"))?,
                        AckMode::Single if common.objects.len() == 1 => {
                            s.out.line(&format!("ACK {id}"))?;
                        }
                        AckMode::Single => {}
                    }
                }
            }
        } else if line == "done" {
            match last {
                Some(last) if req.ack != AckMode::Single => {
                    s.out.line(&format!("ACK {last}"))?;
                }
                Some(_) => {}
                None => s.out.line("NAK")?,
            }
            return Ok(Some(common));
        } else {
            return Err(Error::fatal(msg::expected_list(&line)));
        }
    }
}
