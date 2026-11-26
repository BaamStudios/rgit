// SPDX-License-Identifier: MIT
//! The `fetch` command: negotiation and the packfile.
//!
//! A request names what the client wants (`want`, `want-ref`) and has
//! (`have`). Without `done` the answer is an `acknowledgments` section:
//! `ACK <oid>` for every `have` this repository has too, or `NAK`. Once
//! every wanted commit reaches a commit the client has, the server says
//! `ready` and sends the pack in the same response; with `done` it sends
//! the pack at once.
//!
//! "Reaches a commit the client has" is checked as git does it: the
//! common commits and their parents count as had, and the walk from a
//! wanted commit does not go behind commits older than the oldest common
//! one.

use std::collections::{HashSet, VecDeque};
use std::io::{Read, Write};

use rgit_hash::ObjectId;
use rgit_object::ObjectKind;
use rgit_pack::write::ObjectFilter;

use super::graph::Graph;
use super::pack::{self, PackRequest};
use super::serve::{END_WITHOUT_FLUSH, Session};
use super::shallow::{self, ShallowRequest};
use super::{Error, Repository, Result, messages as msg};

/// A parsed `fetch` request.
#[derive(Default)]
struct Request {
    wants: Vec<ObjectId>,
    /// `want-ref`: the name and what it points at.
    wanted_refs: Vec<(String, ObjectId)>,
    haves: Vec<ObjectId>,
    done: bool,
    wait_for_done: bool,
    thin_pack: bool,
    no_progress: bool,
    include_tag: bool,
    ofs_delta: bool,
    sideband_all: bool,
    shallow: ShallowRequest,
    filter: Option<ObjectFilter>,
}

fn oid(repo: &Repository<'_>, hex: &str) -> Option<ObjectId> {
    ObjectId::from_hex(repo.hash, hex.as_bytes()).ok()
}

/// The reference a `deepen-not` argument names: the name itself or, like
/// a short name on the command line, a tag, branch or remote-tracking
/// branch of that name.
fn resolve_ref(repo: &Repository<'_>, name: &str) -> Option<ObjectId> {
    let candidates = [
        name.to_owned(),
        format!("refs/{name}"),
        format!("refs/tags/{name}"),
        format!("refs/heads/{name}"),
        format!("refs/remotes/{name}"),
        format!("refs/remotes/{name}/HEAD"),
    ];
    candidates
        .iter()
        .find_map(|c| repo.refs.resolve_oid(c).ok().flatten())
}

impl Request {
    fn parse<R: Read, W: Write>(s: &mut Session<'_, R, W>, lines: &[String]) -> Result<Request> {
        let repo = s.repo;
        let graph = Graph::new(repo.odb, repo.hash, &[]);
        let mut req = Request::default();
        let mut seen_wants = HashSet::new();
        let mut seen_haves = HashSet::new();
        for line in lines {
            let unexpected = || Error::fatal(msg::unexpected_line(line));
            let (key, arg) = match line.split_once(' ') {
                Some((k, a)) => (k, Some(a)),
                None => (line.as_str(), None),
            };
            match (key, arg) {
                (END_WITHOUT_FLUSH, _) => {
                    return Err(Error::fatal(msg::expected_flush_after("fetch")));
                }
                ("want", Some(hex)) => {
                    let id = oid(repo, hex).ok_or_else(|| Error::fatal(msg::expected_oid(line)))?;
                    if !repo.odb.contains(&id) {
                        s.out.error(&msg::not_our_ref_packet(&id));
                        return Err(Error::fatal(msg::not_our_ref(&id)));
                    }
                    if seen_wants.insert(id) {
                        req.wants.push(id);
                    }
                }
                ("have", Some(hex)) => {
                    let id =
                        oid(repo, hex).ok_or_else(|| Error::fatal(msg::expected_object(hex)))?;
                    if seen_haves.insert(id) {
                        req.haves.push(id);
                    }
                }
                ("done", None) => req.done = true,
                ("wait-for-done", None) => req.wait_for_done = true,
                ("thin-pack", None) => req.thin_pack = true,
                ("no-progress", None) => req.no_progress = true,
                ("include-tag", None) => req.include_tag = true,
                ("ofs-delta", None) => req.ofs_delta = true,
                ("sideband-all", None) if s.settings.allow_sideband_all => {
                    req.sideband_all = true;
                }
                ("shallow", Some(hex)) => {
                    let id = oid(repo, hex)
                        .ok_or_else(|| Error::fatal(msg::invalid_shallow_line(line)))?;
                    req.shallow.client.push(id);
                }
                ("deepen", Some(n)) => {
                    let depth = n.parse::<u64>().ok().filter(|d| *d > 0);
                    req.shallow.depth =
                        Some(depth.ok_or_else(|| Error::fatal(msg::invalid_deepen(line)))?);
                }
                ("deepen-relative", None) => req.shallow.relative = true,
                ("deepen-since", Some(t)) => {
                    // Like git, nothing counts as 0.
                    let since = if t.is_empty() {
                        Some(0)
                    } else {
                        t.parse::<i64>().ok().filter(|t| *t >= 0)
                    };
                    req.shallow.since =
                        Some(since.ok_or_else(|| Error::fatal(msg::invalid_deepen_since(line)))?);
                }
                ("deepen-not", Some(name)) => {
                    let id = resolve_ref(repo, name)
                        .and_then(|id| graph.peel_to_commit(&id))
                        .ok_or_else(|| Error::fatal(msg::deepen_not_is_not_a_ref(line)))?;
                    req.shallow.not.push(id);
                    req.shallow.has_not = true;
                }
                ("filter", Some(spec)) if s.settings.allow_filter => {
                    let filter =
                        ObjectFilter::parse(spec).map_err(|e| Error::fatal(e.to_string()))?;
                    req.filter = Some(filter);
                }
                ("want-ref", Some(name)) if s.settings.allow_ref_in_want => {
                    let Some(id) = repo.refs.resolve_oid(name).ok().flatten() else {
                        let m = msg::unknown_ref(name);
                        s.out.error(&m);
                        return Err(Error::fatal(m));
                    };
                    if !req.wanted_refs.iter().any(|(n, _)| n == name) {
                        req.wanted_refs.push((name.to_owned(), id));
                    }
                    if seen_wants.insert(id) {
                        req.wants.push(id);
                    }
                }
                _ => return Err(unexpected()),
            }
        }
        Ok(req)
    }
}

/// Whether `uploadpackfilter.*` allows `filter`; the error is the message
/// for the client.
fn check_filter(repo: &Repository<'_>, filter: &ObjectFilter) -> std::result::Result<(), String> {
    let config = repo.config;
    let default = config
        .bool("uploadpackfilter.allow")
        .ok()
        .flatten()
        .unwrap_or(true);
    let name = filter.config_name();
    let max_depth = config
        .int64("uploadpackfilter.tree.maxdepth")
        .ok()
        .flatten()
        .filter(|v| *v >= 0)
        .map(|v| v as u64);
    let explicit = config
        .bool(format!("uploadpackfilter.{name}.allow"))
        .ok()
        .flatten();
    // A depth limit allows tree filters unless they are switched off.
    let allowed = match (explicit, name, max_depth) {
        (Some(v), _, _) => v,
        (None, "tree", Some(_)) => true,
        (None, _, _) => default,
    };
    if !allowed {
        return Err(msg::filter_not_supported(name));
    }
    match filter {
        ObjectFilter::TreeDepth(depth) => {
            if let Some(max) = max_depth.filter(|max| depth > max) {
                return Err(msg::tree_depth_exceeded(max, *depth));
            }
        }
        ObjectFilter::Combine(subs) => {
            for sub in subs {
                check_filter(repo, sub)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Whether the pack can be made: at least one common object, and every
/// wanted commit reaches a commit the client is known to have (a common
/// commit or one of its parents).
fn ready_to_send(graph: &mut Graph<'_>, wants: &[ObjectId], common: &[ObjectId]) -> bool {
    if common.is_empty() {
        return false;
    }
    let mut they_have: HashSet<ObjectId> = HashSet::new();
    let mut oldest: Option<i64> = None;
    for id in common {
        let Some(c) = graph.commit(id) else { continue };
        oldest = Some(oldest.map_or(c.time, |t| t.min(c.time)));
        they_have.insert(*id);
        they_have.extend(c.parents.iter().copied());
    }
    'wants: for want in wants {
        // Only commits take part: other objects have no history.
        if graph.commit(want).is_none() {
            continue;
        }
        let mut seen: HashSet<ObjectId> = HashSet::new();
        let mut queue: VecDeque<ObjectId> = VecDeque::from([*want]);
        while let Some(id) = queue.pop_front() {
            if they_have.contains(&id) {
                continue 'wants;
            }
            let Some(c) = graph.commit(&id) else { continue };
            // What is older than everything the client named cannot lead
            // to it.
            if oldest.is_some_and(|t| c.time < t) {
                continue;
            }
            for p in &c.parents {
                if seen.insert(*p) {
                    queue.push_back(*p);
                }
            }
        }
        return false;
    }
    true
}

pub(crate) fn run<R: Read, W: Write>(s: &mut Session<'_, R, W>, lines: &[String]) -> Result<()> {
    let repo = s.repo;
    let req = Request::parse(s, lines)?;
    // Nothing wanted, nothing to say.
    if req.wants.is_empty() {
        return Ok(());
    }
    if let Some(filter) = &req.filter {
        if let Err(m) = check_filter(repo, filter) {
            s.out.error(&m);
            return Err(Error::fatal(m));
        }
    }
    s.out.sideband_all = req.sideband_all;
    let result = respond(s, &req);
    s.out.sideband_all = false;
    result
}

fn respond<R: Read, W: Write>(s: &mut Session<'_, R, W>, req: &Request) -> Result<()> {
    let repo = s.repo;
    let mut graph = Graph::new(repo.odb, repo.hash, &repo.shallow);
    // Objects of any type can be common; only commits tell about history.
    let common: Vec<ObjectId> = req
        .haves
        .iter()
        .filter(|id| repo.odb.contains(id))
        .copied()
        .collect();
    if !req.done {
        s.out.line("acknowledgments")?;
        if common.is_empty() {
            s.out.line("NAK")?;
        }
        for id in &common {
            s.out.line(&format!("ACK {id}"))?;
        }
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
        if req.wait_for_done || !ready_to_send(&mut graph, &wanted_commits, &common) {
            return s.out.flush();
        }
        s.out.line("ready")?;
        s.out.delim()?;
    }

    let shallow_wanted = !req.shallow.client.is_empty()
        || req.shallow.depth.is_some()
        || req.shallow.since.is_some()
        || req.shallow.has_not
        || !repo.shallow.is_empty();
    let mut plan = shallow::ShallowPlan::default();
    if shallow_wanted {
        // Like git, the section header goes out before the commits are
        // chosen, so a failure there leaves it behind.
        s.out.line("shallow-info")?;
        if req.shallow.depth.is_some() && (req.shallow.since.is_some() || req.shallow.has_not) {
            return Err(Error::fatal(msg::DEEPEN_CONFLICT));
        }
        plan = shallow::plan(&mut graph, &repo.shallow, &req.shallow, &req.wants)?;
        // These two lines carry no LF.
        for id in &plan.shallow {
            s.out.data(format!("shallow {id}").as_bytes())?;
        }
        for id in &plan.unshallow {
            s.out.data(format!("unshallow {id}").as_bytes())?;
        }
        s.out.delim()?;
    }
    if !req.wanted_refs.is_empty() {
        s.out.line("wanted-refs")?;
        // In name order, as git lists them.
        let mut wanted: Vec<&(String, ObjectId)> = req.wanted_refs.iter().collect();
        wanted.sort();
        for (name, id) in wanted {
            s.out.line(&format!("{id} {name}"))?;
        }
        s.out.delim()?;
    }
    s.out.line("packfile")?;
    s.out.writer.flush_io()?;

    let mut tips = req.wants.clone();
    let mut seen: HashSet<ObjectId> = tips.iter().copied().collect();
    tips.extend(plan.extra_wants.iter().filter(|id| seen.insert(**id)));
    // The client has the commits that become complete (and their trees),
    // only not what is behind them.
    let mut exclude = common;
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
    };
    pack::send(s, &pack)?;
    s.out.flush()
}
