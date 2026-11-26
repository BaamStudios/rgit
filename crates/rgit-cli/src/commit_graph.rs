// SPDX-License-Identifier: MIT
//! The repository's commit-graph (rgit-commitgraph) as the commands use
//! it: configuration (`core.commitGraph`, `commitGraph.*`,
//! Documentation/config/commitgraph.adoc), object access, when the graph
//! may be read at all (not with replace refs, grafts or a shallow
//! repository, Documentation/technical/commit-graph.adoc) and writing it
//! after `gc` and `fetch` (`gc.writeCommitGraph`, `fetch.writeCommitGraph`).

use std::io::Write;
use std::path::{Path, PathBuf};

use rgit_commitgraph::{Diag, Graph, LoadOptions, ParsedCommit, write::WriteOptions};
use rgit_config::ConfigSet;
use rgit_hash::{HashKind, ObjectId};
use rgit_object::{Commit, ObjectKind};
use rgit_odb::ObjectDatabase;

/// `core.commitGraph` (default true).
pub fn enabled(config: &ConfigSet) -> bool {
    config
        .bool("core.commitgraph")
        .ok()
        .flatten()
        .unwrap_or(true)
}

/// `commitGraph.changedPathsVersion`, falling back to the deprecated
/// `commitGraph.readChangedPaths`.
pub fn changed_paths_version(config: &ConfigSet) -> i32 {
    if let Ok(Some(v)) = config.int("commitgraph.changedpathsversion") {
        return v as i32;
    }
    match config.bool("commitgraph.readchangedpaths").ok().flatten() {
        Some(false) => 0,
        _ => -1,
    }
}

/// How to load the graph of a repository with `config`.
pub fn load_options(config: &ConfigSet, hash: HashKind) -> LoadOptions {
    let mut o = LoadOptions::new(hash);
    if let Ok(Some(v)) = config.int("commitgraph.generationversion") {
        o.generation_version = v as u32;
    }
    o.changed_paths_version = changed_paths_version(config);
    o
}

/// How to load the existing graph for a write: filters of any version
/// (they may be reused when the paths allow it).
pub fn load_options_for_write(config: &ConfigSet, hash: HashKind) -> LoadOptions {
    let mut o = load_options(config, hash);
    if o.changed_paths_version != 0 {
        o.changed_paths_version = -1;
    }
    o
}

/// The object directory and its alternates.
pub fn object_dirs(objects_dir: &Path) -> Vec<PathBuf> {
    let mut v = vec![objects_dir.to_path_buf()];
    v.extend(rgit_odb::resolve_alternates(objects_dir));
    v
}

/// End the command after a fatal commit-graph error (git dies there too).
pub fn die(msg: &str) -> ! {
    let _ = std::io::stdout().flush();
    let _ = writeln!(std::io::stderr(), "fatal: {msg}");
    std::process::exit(128)
}

/// Print the loader's messages.
pub fn print_diags(diags: Vec<Diag>) {
    let mut err = std::io::stderr();
    for d in diags {
        let _ = writeln!(err, "{}", d.line());
    }
}

/// Load the graph of `objects_dir`, printing what was wrong with it.
pub fn load(objects_dir: &Path, opts: LoadOptions) -> Option<Graph> {
    let mut diags = Vec::new();
    let g = Graph::load(&object_dirs(objects_dir), opts, &mut diags);
    print_diags(diags);
    g
}

/// Whether the graph may be read: grafts and a shallow repository change
/// the history it describes.
pub fn history_unchanged(common_dir: &Path, shallow_file: &Path) -> bool {
    let grafts = std::env::var_os("GIT_GRAFT_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| common_dir.join("info").join("grafts"));
    if grafts.exists() {
        return false;
    }
    !std::fs::metadata(shallow_file).is_ok_and(|m| m.len() > 0)
}

/// `GIT_COMMIT_GRAPH_PARANOIA`: check that commits read from the graph
/// exist in the object database.
pub fn paranoid() -> bool {
    std::env::var("GIT_COMMIT_GRAPH_PARANOIA")
        .ok()
        .and_then(|v| rgit_config::value::parse_bool(Some(v.as_bytes())))
        .unwrap_or(false)
}

/// Objects for writing and verifying: straight from the object database
/// (no replacements).
pub struct DbObjects<'a> {
    pub odb: &'a ObjectDatabase,
}

impl rgit_commitgraph::Objects for DbObjects<'_> {
    fn commit(&self, id: &ObjectId) -> Result<Option<ParsedCommit>, rgit_commitgraph::Unparsable> {
        let obj = match self.odb.read(id) {
            Ok(Some(o)) => o,
            _ => return Ok(None),
        };
        if obj.kind != ObjectKind::Commit {
            return Ok(None);
        }
        let c = Commit::parse(self.odb.hash_kind(), &obj.data)
            .map_err(|_| rgit_commitgraph::Unparsable)?;
        let tree = c.tree().map_err(|_| rgit_commitgraph::Unparsable)?;
        let parents = c.parents().map_err(|_| rgit_commitgraph::Unparsable)?;
        let date = c
            .committer()
            .and_then(|s| s.time())
            .map_or(0, |t| u64::try_from(t.seconds).unwrap_or(0));
        Ok(Some(ParsedCommit {
            tree,
            parents,
            date,
        }))
    }

    fn tree(&self, id: &ObjectId) -> Option<Vec<u8>> {
        match self.odb.read(id) {
            Ok(Some(o)) if o.kind == ObjectKind::Tree => Some(o.data),
            _ => None,
        }
    }
}

/// `GIT_TEST_BLOOM_SETTINGS_*` as a number.
fn env_u32(name: &str) -> Option<u32> {
    std::env::var(name).ok()?.trim().parse().ok()
}

/// Write options from the configuration and environment.
pub fn write_options(config: &ConfigSet, hash: HashKind, shared_mode: u32) -> WriteOptions {
    let mut o = WriteOptions::new(hash);
    if let Ok(Some(v)) = config.int("commitgraph.generationversion") {
        o.generation_version = v as u32;
    }
    o.changed_paths_version = changed_paths_version(config);
    o.changed_paths_config = config
        .bool("commitgraph.changedpaths")
        .ok()
        .flatten()
        .unwrap_or(false);
    if let Ok(Some(v)) = config.int("commitgraph.maxnewfilters") {
        o.max_new_filters = u32::try_from(v).ok();
    }
    o.bloom_num_hashes = env_u32("GIT_TEST_BLOOM_SETTINGS_NUM_HASHES");
    o.bloom_bits_per_entry = env_u32("GIT_TEST_BLOOM_SETTINGS_BITS_PER_ENTRY");
    o.bloom_max_changed_paths = env_u32("GIT_TEST_BLOOM_SETTINGS_MAX_CHANGED_PATHS");
    o.file_mode = shared_mode;
    o
}

/// Permission bits for commit-graph files (`core.sharedRepository`).
pub fn file_mode(config: &ConfigSet) -> u32 {
    let shared = config
        .string("core.sharedrepository")
        .ok()
        .flatten()
        .and_then(|v| rgit_fs::SharedRepository::parse(&v.to_string()).ok());
    match shared {
        Some(s) => s.adjust_file_mode(0o444) & 0o444,
        None => 0o444,
    }
}

/// The commits the refs point to (tags peeled; other objects left out),
/// for `--reachable` and the writes after `gc` and `fetch`.
pub fn ref_commits(
    odb: &ObjectDatabase,
    refs: &dyn rgit_revwalk::revision::RefLookup,
) -> Vec<ObjectId> {
    let mut targets = Vec::new();
    let _ = refs.for_each_ref("refs/", &mut |_, id| targets.push(*id));
    let mut out = Vec::new();
    for id in targets {
        if let Some(c) = peel_to_commit(odb, &id) {
            out.push(c);
        }
    }
    out
}

/// Peel tags until a commit; `None` for other objects or missing ones.
pub fn peel_to_commit(odb: &ObjectDatabase, id: &ObjectId) -> Option<ObjectId> {
    let mut id = *id;
    for _ in 0..64 {
        let obj = odb.read(&id).ok()??;
        match obj.kind {
            ObjectKind::Commit => return Some(id),
            ObjectKind::Tag => {
                let tag = rgit_object::Tag::parse(odb.hash_kind(), &obj.data).ok()?;
                id = tag.object().ok()?;
            }
            _ => return None,
        }
    }
    None
}

/// `gc.writeCommitGraph` / `fetch.writeCommitGraph`: write the graph of
/// all reachable commits as a new split layer (gc: a single file),
/// quietly. Errors are ignored like a failed optional step.
pub fn write_reachable(
    setup: &rgit_repo::Setup,
    config: &ConfigSet,
    split: Option<rgit_commitgraph::write::Split>,
) {
    if !enabled(config) {
        return;
    }
    let hash = setup.format.object_format;
    let objects_dir = crate::revisions::objects_dir(setup);
    let shallow = setup.common_dir.join("shallow");
    if !history_unchanged(&setup.common_dir, &shallow) {
        return;
    }
    let Ok(odb) = crate::repo::odb_with_config(setup, config) else {
        return;
    };
    let refs = crate::revisions::Refs::new(setup, hash);
    let start = ref_commits(&odb, &refs);
    let mut opts = write_options(config, hash, file_mode(config));
    opts.split = split;
    let existing = load(&objects_dir, load_options_for_write(config, hash));
    let objects = DbObjects { odb: &odb };
    if let Err(e) =
        rgit_commitgraph::write::write(&objects, &objects_dir, existing.as_ref(), &start, &opts)
    {
        let _ = writeln!(std::io::stderr(), "error: {e}");
    }
}

/// What a changed-path filter says about a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BloomAnswer {
    /// No path of interest changed: the commit is TREESAME.
    DefinitelyNot,
    /// Some path of interest may have changed: diff the trees.
    Maybe,
    /// No usable filter: diff the trees.
    Unknown,
}

/// The changed-path filters of the commit-graph for a pathspec (path
/// limited `log`/`rev-list`). A commit's filter covers the diff against
/// its first parent (a root commit: against the empty tree). Usage
/// statistics go to trace2 (`bloom/statistics`) when it is dropped.
pub struct BloomFilter {
    settings: rgit_commitgraph::bloom::Settings,
    /// Per pathspec item: the keys of the path and its leading
    /// directories; an item may match when all its keys are in a filter.
    keys: Vec<Vec<rgit_commitgraph::bloom::Key>>,
    /// The single path the keys are for (`--follow`).
    path: Option<Vec<u8>>,
    verdicts: std::collections::HashMap<ObjectId, BloomAnswer>,
    false_positives: std::collections::HashSet<ObjectId>,
    not_present: u64,
    maybe: u64,
    definitely_not: u64,
}

impl BloomFilter {
    /// `None` when the graph has no usable filters or the pathspec cannot
    /// use them (no path, `icase`/`exclude` magic, a pathspec that starts
    /// with a wildcard).
    pub fn new(graph: &Graph, pathspec: &rgit_pathspec::Pathspec) -> Option<BloomFilter> {
        let settings = graph.bloom_settings()?;
        if pathspec.is_empty() {
            return None;
        }
        let mut keys = Vec::new();
        for item in pathspec.items() {
            let magic = item.magic();
            if magic.intersects(rgit_pathspec::Magic::ICASE)
                || magic.intersects(rgit_pathspec::Magic::EXCLUDE)
            {
                return None;
            }
            let pat = item.pattern();
            let mut len = item.nowildcard_len().min(pat.len());
            if len < pat.len() {
                len = pat[..len].iter().rposition(|&b| b == b'/').unwrap_or(0);
            }
            let mut p = &pat[..len];
            while let Some(rest) = p.strip_suffix(b"/") {
                p = rest;
            }
            if p.is_empty() {
                return None;
            }
            keys.push(rgit_commitgraph::bloom::keys_for_path(p, &settings));
        }
        Some(BloomFilter {
            settings,
            keys,
            path: None,
            verdicts: std::collections::HashMap::new(),
            false_positives: std::collections::HashSet::new(),
            not_present: 0,
            maybe: 0,
            definitely_not: 0,
        })
    }

    /// Filters for the single path `path` (`log --follow`).
    pub fn for_path(graph: &Graph, path: &[u8]) -> Option<BloomFilter> {
        let settings = graph.bloom_settings()?;
        if path.is_empty() {
            return None;
        }
        Some(BloomFilter {
            settings,
            keys: vec![rgit_commitgraph::bloom::keys_for_path(path, &settings)],
            path: Some(path.to_vec()),
            verdicts: std::collections::HashMap::new(),
            false_positives: std::collections::HashSet::new(),
            not_present: 0,
            maybe: 0,
            definitely_not: 0,
        })
    }

    /// Look for `path` from now on (`--follow` after a rename).
    pub fn set_path(&mut self, path: &[u8]) {
        if self.path.as_deref() != Some(path) && !path.is_empty() {
            self.keys = vec![rgit_commitgraph::bloom::keys_for_path(path, &self.settings)];
            self.path = Some(path.to_vec());
        }
    }

    /// Whether `id` may differ from `parent` (`None`: it is a root commit)
    /// in the paths of interest.
    pub fn check(
        &mut self,
        graph: Option<&Graph>,
        id: &ObjectId,
        parent: Option<&ObjectId>,
    ) -> BloomAnswer {
        let Some(g) = graph else {
            return BloomAnswer::Unknown;
        };
        let Some(pos) = g.lookup(id) else {
            return BloomAnswer::Unknown;
        };
        let Ok(data) = g.commit_at(pos) else {
            return BloomAnswer::Unknown;
        };
        let first = data.parents.first().and_then(|&p| g.id_at(p));
        if first.as_ref() != parent {
            return BloomAnswer::Unknown;
        }
        if let Some(v) = self.verdicts.get(id) {
            return *v;
        }
        let mut diags = Vec::new();
        let filter = g.bloom_filter(pos, &mut diags);
        print_diags(diags);
        let v = match filter {
            None => {
                self.not_present += 1;
                BloomAnswer::Unknown
            }
            Some(f) => {
                let hit = self
                    .keys
                    .iter()
                    .any(|ks| ks.iter().all(|k| rgit_commitgraph::bloom::contains(f, k)));
                if hit {
                    self.maybe += 1;
                    BloomAnswer::Maybe
                } else {
                    self.definitely_not += 1;
                    BloomAnswer::DefinitelyNot
                }
            }
        };
        self.verdicts.insert(*id, v);
        v
    }

    /// The filter said "maybe" but `id` is TREESAME.
    pub fn false_positive(&mut self, id: &ObjectId) {
        self.false_positives.insert(*id);
    }
}

impl Drop for BloomFilter {
    fn drop(&mut self) {
        let json = format!(
            "{{\"filter_not_present\":{},\"maybe\":{},\"definitely_not\":{},\"false_positive\":{}}}",
            self.not_present,
            self.maybe,
            self.definitely_not,
            self.false_positives.len()
        );
        rgit_trace::trace2::data_json("bloom", "statistics", &json, Some(1));
    }
}
