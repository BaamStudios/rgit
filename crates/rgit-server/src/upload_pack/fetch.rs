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
//! one. A `have` for something that counts as had already (a repeated
//! line, the parent of an earlier `have`) is not acknowledged again.

use std::collections::{HashSet, VecDeque};
use std::io::{Read, Write};

use rgit_hash::ObjectId;
use rgit_object::ObjectKind;
use rgit_pack::write::ObjectFilter;

use super::graph::Graph;
use super::pack::{self, PackRequest};
use super::refs::RefView;
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

pub(crate) fn oid(repo: &Repository<'_>, hex: &str) -> Option<ObjectId> {
    ObjectId::from_hex(repo.hash, hex.as_bytes()).ok()
}

/// The reference a `deepen-not` argument names: the name itself or, like
/// a short name on the command line, a tag, branch or remote-tracking
/// branch of that name. As in git, this looks at the repository's
/// references as they are: neither a namespace nor `hideRefs` applies.
pub(crate) fn resolve_ref(repo: &Repository<'_>, name: &str) -> Option<ObjectId> {
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

/// The order in which git lists the wanted references: that of its hash
/// table of names. Observed from git 2.56 with sets of 2 to 180 names:
/// the names go into 64 buckets by the 32-bit FNV-1 hash of the name,
/// new entries in front of older ones of the same bucket; the table
/// grows fourfold when it is more than 80% full; the listing goes
/// through the buckets in order. Returns indexes into `names`.
fn wanted_refs_order(names: &[&str]) -> Vec<usize> {
    let bucket = |name: &str, size: usize| {
        let hash = name.bytes().fold(0x811c_9dc5_u32, |h, b| {
            h.wrapping_mul(0x0100_0193) ^ u32::from(b)
        });
        hash as usize & (size - 1)
    };
    let mut size = 64;
    let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); size];
    for (i, name) in names.iter().enumerate() {
        buckets[bucket(name, size)].insert(0, i);
        if (i + 1) * 5 > size * 4 {
            size *= 4;
            let mut grown: Vec<Vec<usize>> = vec![Vec::new(); size];
            for j in buckets.into_iter().flatten() {
                grown[bucket(names[j], size)].insert(0, j);
            }
            buckets = grown;
        }
    }
    buckets.into_iter().flatten().collect()
}

impl Request {
    fn parse<R: Read, W: Write>(s: &mut Session<'_, R, W>, lines: &[String]) -> Result<Request> {
        let repo = s.repo;
        let graph = Graph::new(repo.odb, repo.hash, &[]);
        let refs = RefView::new(repo.refs, repo.config, s.opts.namespace.as_deref());
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
                    // Like git, what follows the object id is not looked at.
                    let hex = hex.get(..repo.hash.hex_len()).unwrap_or(hex);
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
                    let Some(id) = refs.resolve(name) else {
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
pub(crate) fn check_filter(
    repo: &Repository<'_>,
    filter: &ObjectFilter,
) -> std::result::Result<(), String> {
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

/// What the `have` lines of a request say about the client.
#[derive(Default)]
pub(crate) struct Common {
    /// The `have`s this repository has too, in the client's order,
    /// without those that counted as had already.
    pub objects: Vec<ObjectId>,
    /// What the client is known to have: every common object and the
    /// parents of the common commits.
    they_have: HashSet<ObjectId>,
    /// The time of the oldest common commit.
    oldest: Option<i64>,
}

/// What a `have` line told, see [`Common::add`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Have {
    /// This repository does not have the object.
    Missing,
    /// A common object that is new to the list.
    New,
    /// A common object that counted as had already.
    Known,
}

impl Common {
    /// Take in one `have` line as git does: an object this repository
    /// has is common; it is new to the list unless it counts as had
    /// already (an earlier `have`, or the parent of an earlier common
    /// commit); the parents of a common commit count as had from then on.
    /// Objects of any type can be common; only commits tell about
    /// history.
    pub fn add(&mut self, repo: &Repository<'_>, graph: &mut Graph<'_>, id: &ObjectId) -> Have {
        if !repo.odb.contains(id) {
            return Have::Missing;
        }
        let known = !self.they_have.insert(*id);
        if let Some(c) = graph.commit(id) {
            self.oldest = Some(self.oldest.map_or(c.time, |t| t.min(c.time)));
            self.they_have.extend(c.parents.iter().copied());
        }
        if known {
            Have::Known
        } else {
            self.objects.push(*id);
            Have::New
        }
    }

    /// Go through the `have` lines of a request.
    fn find(repo: &Repository<'_>, graph: &mut Graph<'_>, haves: &[ObjectId]) -> Common {
        let mut common = Common::default();
        for id in haves {
            common.add(repo, graph, id);
        }
        common
    }
}

/// Whether the pack can be made: at least one common object, and every
/// wanted commit reaches a commit the client is known to have.
pub(crate) fn ready_to_send(graph: &mut Graph<'_>, wants: &[ObjectId], common: &Common) -> bool {
    if common.objects.is_empty() {
        return false;
    }
    let (they_have, oldest) = (&common.they_have, common.oldest);
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

/// The trace2 event that tells what a request asked for (`data_json`,
/// category `upload-pack`, key `fetch-info`), as git writes it.
fn trace_request(req: &Request, common: &[ObjectId]) {
    if !rgit_trace::trace2::enabled() {
        return;
    }
    let haves = common.len();
    // The kind of filter, not its arguments.
    let filter = match &req.filter {
        Some(f) => format!("\"{}\"", f.config_name()),
        None => "null".to_owned(),
    };
    let json = format!(
        "{{\"haves\":{haves},\"wants\":{},\"want-refs\":{},\"depth\":{},\"shallows\":{},\
         \"deepen-since\":{},\"deepen-not\":{},\"deepen-relative\":{},\"filter\":{filter}}}",
        req.wants.len(),
        req.wanted_refs.len(),
        req.shallow.depth.unwrap_or(0),
        req.shallow.client.len(),
        req.shallow.since.is_some(),
        req.shallow.not.len(),
        req.shallow.relative,
    );
    rgit_trace::trace2::data_json("upload-pack", "fetch-info", &json, Some(0));
}

pub(crate) fn run<R: Read, W: Write>(s: &mut Session<'_, R, W>, lines: &[String]) -> Result<()> {
    let repo = s.repo;
    let req = Request::parse(s, lines)?;
    let mut graph = Graph::new(repo.odb, repo.hash, &repo.shallow);
    let common = Common::find(repo, &mut graph, &req.haves);
    trace_request(&req, &common.objects);
    // Nothing wanted, nothing to say: unless the client only negotiates
    // (`wait-for-done`, `fetch --negotiate-only`) and waits for the
    // acknowledgments.
    if req.wants.is_empty() && !req.wait_for_done {
        return Ok(());
    }
    if let Some(filter) = &req.filter {
        if let Err(m) = check_filter(repo, filter) {
            s.out.error(&m);
            return Err(Error::fatal(m));
        }
    }
    s.out.sideband_all = req.sideband_all;
    let result = respond(s, &req, &mut graph, common);
    s.out.sideband_all = false;
    result
}

fn respond<R: Read, W: Write>(
    s: &mut Session<'_, R, W>,
    req: &Request,
    graph: &mut Graph<'_>,
    common: Common,
) -> Result<()> {
    let repo = s.repo;
    // Without `have` lines there is nothing to acknowledge: like git, the
    // pack follows at once.
    if !req.done && !req.haves.is_empty() {
        s.out.line("acknowledgments")?;
        if common.objects.is_empty() {
            s.out.line("NAK")?;
        }
        for id in &common.objects {
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
        if req.wait_for_done || !ready_to_send(graph, &wanted_commits, &common) {
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
        plan = shallow::plan(graph, &repo.shallow, &req.shallow, &req.wants)?;
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
        let names: Vec<&str> = req.wanted_refs.iter().map(|(n, _)| n.as_str()).collect();
        for i in wanted_refs_order(&names) {
            let (name, id) = &req.wanted_refs[i];
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
        allow_promisor: s.promisor_accepted,
        band: Some(rgit_protocol::sideband::SidebandMode::Large),
    };
    pack::send(s, &pack)?;
    s.out.flush()
}

#[cfg(test)]
mod tests {
    use super::wanted_refs_order;

    fn order(names: &[&str]) -> Vec<String> {
        wanted_refs_order(names)
            .into_iter()
            .map(|i| names[i].to_owned())
            .collect()
    }

    // Orders observed from `git upload-pack` 2.56.
    #[test]
    fn wanted_refs_are_listed_like_git() {
        assert_eq!(
            order(&["refs/heads/o/foo", "refs/heads/o/bar"]),
            ["refs/heads/o/foo", "refs/heads/o/bar"]
        );
        assert_eq!(
            order(&["refs/heads/main", "refs/tags/v1", "HEAD"]),
            ["HEAD", "refs/heads/main", "refs/tags/v1"]
        );
        assert_eq!(
            order(&["refs/heads/b72", "refs/heads/b58", "refs/heads/b54"]),
            ["refs/heads/b58", "refs/heads/b54", "refs/heads/b72"]
        );
        assert_eq!(
            order(&[
                "refs/heads/b274",
                "refs/heads/b261",
                "refs/heads/b258",
                "refs/heads/b182"
            ]),
            [
                "refs/heads/b258",
                "refs/heads/b182",
                "refs/heads/b274",
                "refs/heads/b261"
            ]
        );
        // More names than the first table holds.
        let many: Vec<String> = (0..300).map(|i| format!("refs/heads/n{i}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let mut listed = wanted_refs_order(&refs);
        listed.sort_unstable();
        assert_eq!(listed, (0..300).collect::<Vec<_>>());
    }
}
