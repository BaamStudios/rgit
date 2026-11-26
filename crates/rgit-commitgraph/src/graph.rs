// SPDX-License-Identifier: MIT
//! The commit-graph of a repository: one file or a chain of layers
//! (`Documentation/technical/commit-graph.adoc`, "Commit-Graphs Chains").
//!
//! Positions are global: the commits of the bottom layer come first, a
//! layer's local position `i` is `i` plus the commits of all layers below.

use std::path::{Path, PathBuf};

use rgit_hash::{HashKind, ObjectId};

use crate::bloom;
use crate::file::{FilterLookup, Layer, LayerError, ReadOptions};
use crate::messages as msg;
use crate::{Diag, GENERATION_V1_MAX};

/// How to load the graph (from the configuration).
#[derive(Debug, Clone, Copy)]
pub struct LoadOptions {
    /// The repository's hash.
    pub hash: HashKind,
    /// `commitGraph.generationVersion` (1 or 2, default 2).
    pub generation_version: u32,
    /// `commitGraph.changedPathsVersion` (-1, 0, 1 or 2; default -1).
    pub changed_paths_version: i32,
}

impl LoadOptions {
    /// Defaults for `hash`.
    pub fn new(hash: HashKind) -> LoadOptions {
        LoadOptions {
            hash,
            generation_version: 2,
            changed_paths_version: -1,
        }
    }
}

/// A commit as stored in the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitData {
    /// The root tree.
    pub tree: ObjectId,
    /// Parent positions.
    pub parents: Vec<u32>,
    /// Commit date (34 bits).
    pub date: u64,
}

/// Errors reading a commit from the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// A parent position beyond the graph (git dies).
    InvalidParent(u32),
    /// The extra edge list is too short (git reports an error and reads
    /// the commit from the object database).
    ExtraEdgesOutOfBounds,
    /// The generation overflow chunk is too short (git dies).
    OverflowTooSmall,
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::InvalidParent(p) => f.write_str(&msg::invalid_parent_position(*p)),
            ReadError::ExtraEdgesOutOfBounds => f.write_str(msg::EXTRA_EDGES_OOB),
            ReadError::OverflowTooSmall => f.write_str(msg::OVERFLOW_TOO_SMALL),
        }
    }
}

impl std::error::Error for ReadError {}

impl From<LayerError> for ReadError {
    fn from(e: LayerError) -> ReadError {
        match e {
            LayerError::ExtraEdgesOutOfBounds => ReadError::ExtraEdgesOutOfBounds,
            LayerError::OverflowTooSmall => ReadError::OverflowTooSmall,
        }
    }
}

/// Where the layers came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// `<objects>/info/commit-graph`.
    Single,
    /// `<objects>/info/commit-graphs/commit-graph-chain`, with the hashes
    /// listed there (all of them, also those that could not be loaded).
    Chain(Vec<ObjectId>),
}

/// A loaded commit-graph.
pub struct Graph {
    /// Bottom layer first.
    layers: Vec<Layer>,
    /// Commits below each layer.
    offsets: Vec<u32>,
    /// Use corrected commit dates as generation numbers.
    read_generation_data: bool,
    /// The settings of the usable Bloom filters.
    bloom: Option<bloom::Settings>,
    source: Source,
    /// The `objects` directory whose `info` held the file or chain.
    object_dir: PathBuf,
}

/// The single file of `objects_dir`.
pub fn single_path(objects_dir: &Path) -> PathBuf {
    objects_dir.join("info").join("commit-graph")
}

/// The directory of chain layers of `objects_dir`.
pub fn chain_dir(objects_dir: &Path) -> PathBuf {
    objects_dir.join("info").join("commit-graphs")
}

/// The chain file of `objects_dir`.
pub fn chain_path(objects_dir: &Path) -> PathBuf {
    chain_dir(objects_dir).join("commit-graph-chain")
}

/// The file of chain layer `hash` in `objects_dir`.
pub fn layer_path(objects_dir: &Path, hash: &ObjectId) -> PathBuf {
    chain_dir(objects_dir).join(format!("graph-{}.graph", hash.to_hex()))
}

/// The result of reading a chain file.
pub struct ChainLines {
    /// The valid hashes, in order, up to the first bad line.
    pub hashes: Vec<ObjectId>,
    /// Number of lines in the file.
    pub lines: usize,
}

/// Read `commit-graph-chain`. `Err` is git's fatal message for verify
/// (the warning is in `diags`).
pub fn read_chain(
    path: &Path,
    hash: HashKind,
    diags: &mut Vec<Diag>,
) -> Result<ChainLines, String> {
    let text = std::fs::read(path).map_err(|e| e.to_string())?;
    let hex = hash.hex_len();
    if text.len() < hex + 1 {
        diags.push(Diag::Warning(msg::CHAIN_TOO_SMALL.to_string()));
        return Err(msg::could_not_open_chain(
            &path.to_string_lossy(),
            "Invalid argument",
        ));
    }
    let mut hashes = Vec::new();
    let mut lines = 0;
    let mut bad = false;
    for line in text.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        lines += 1;
        if bad {
            continue;
        }
        match ObjectId::from_hex(hash, line) {
            Ok(id) if line.len() == hex => hashes.push(id),
            _ => {
                diags.push(Diag::Warning(msg::chain_line_not_hash(
                    &String::from_utf8_lossy(line),
                )));
                bad = true;
            }
        }
    }
    Ok(ChainLines { hashes, lines })
}

/// Find the file of chain layer `hash` in any of `dirs`.
fn find_layer(dirs: &[PathBuf], hash: &ObjectId) -> Option<(PathBuf, PathBuf)> {
    dirs.iter().find_map(|d| {
        let p = layer_path(d, hash);
        p.is_file().then(|| (p, d.clone()))
    })
}

/// Load the layers of a chain: the valid bottom part. Returns the layers
/// and whether all lines were loaded.
pub(crate) fn load_chain_layers(
    dirs: &[PathBuf],
    lines: &ChainLines,
    ropts: ReadOptions,
    diags: &mut Vec<Diag>,
) -> (Vec<Layer>, bool) {
    let mut layers: Vec<Layer> = Vec::new();
    for (i, h) in lines.hashes.iter().enumerate() {
        let Some((path, dir)) = find_layer(dirs, h) else {
            diags.push(Diag::Warning(msg::CHAIN_INCOMPLETE.to_string()));
            return (layers, false);
        };
        let layer = match Layer::open(&path, &dir, ropts, diags) {
            Ok(l) => l,
            Err(_) => {
                diags.push(Diag::Warning(msg::CHAIN_INCOMPLETE.to_string()));
                return (layers, false);
            }
        };
        // The layer must build on the layers loaded so far (their
        // checksums, which normally are the names in the chain file).
        let bases = layer.base_hashes();
        let below: Vec<ObjectId> = layers.iter().map(Layer::checksum).collect();
        if usize::from(layer.num_bases()) != i || bases != below {
            diags.push(Diag::Warning(msg::CHAIN_MISMATCH.to_string()));
            diags.push(Diag::Warning(msg::CHAIN_INCOMPLETE.to_string()));
            return (layers, false);
        }
        layers.push(layer);
    }
    let complete = layers.len() == lines.lines;
    (layers, complete)
}

impl Graph {
    /// Load the commit-graph of the repository whose object directories
    /// are `dirs` (its own first, then the alternates). `None` without a
    /// usable graph; `diags` are git's messages for what was wrong.
    pub fn load(dirs: &[PathBuf], opts: LoadOptions, diags: &mut Vec<Diag>) -> Option<Graph> {
        let ropts = ReadOptions {
            hash: opts.hash,
            read_bloom: opts.changed_paths_version != 0,
        };
        for dir in dirs {
            let single = single_path(dir);
            if single.is_file() {
                if let Ok(layer) = Layer::open(&single, dir, ropts, diags) {
                    return Some(Graph::from_layers(
                        vec![layer],
                        Source::Single,
                        dir,
                        opts,
                        diags,
                    ));
                }
            }
            let chain = chain_path(dir);
            if chain.is_file() {
                let Ok(lines) = read_chain(&chain, opts.hash, diags) else {
                    continue;
                };
                let (layers, _) = load_chain_layers(dirs, &lines, ropts, diags);
                if layers.is_empty() {
                    return None;
                }
                let source = Source::Chain(lines.hashes.clone());
                return Some(Graph::from_layers(layers, source, dir, opts, diags));
            }
        }
        None
    }

    /// A graph of the given layers (bottom first).
    pub(crate) fn from_layers(
        mut layers: Vec<Layer>,
        source: Source,
        object_dir: &Path,
        opts: LoadOptions,
        diags: &mut Vec<Diag>,
    ) -> Graph {
        let mut offsets = Vec::with_capacity(layers.len());
        let mut total = 0u32;
        for l in &layers {
            offsets.push(total);
            total = total.saturating_add(l.num_commits());
        }
        let read_generation_data =
            opts.generation_version >= 2 && layers.iter().all(|l| l.has_generation_data());
        // Bloom settings come from the topmost layer with filters; layers
        // below with other settings are not used.
        let mut bloom: Option<bloom::Settings> = None;
        if opts.changed_paths_version != 0 {
            for l in layers.iter_mut().rev() {
                let Some(s) = l.bloom_settings() else {
                    continue;
                };
                match bloom {
                    None => {
                        let wanted = opts.changed_paths_version;
                        if wanted > 0 && s.hash_version != wanted as u32 {
                            // Filters of another version are not read.
                            break;
                        }
                        bloom = Some(s);
                    }
                    Some(b) if b.same_filters(&s) => {}
                    Some(_) => {
                        l.bloom_disabled = true;
                        diags.push(Diag::Warning(msg::bloom_layer_disabled(
                            &l.checksum().to_hex(),
                        )));
                    }
                }
            }
        }
        Graph {
            layers,
            offsets,
            read_generation_data,
            bloom,
            source,
            object_dir: object_dir.to_path_buf(),
        }
    }

    /// The layers, bottom first.
    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    /// Whether the graph came from a single file or a chain.
    pub fn source(&self) -> &Source {
        &self.source
    }

    /// The `objects` directory holding the file or chain.
    pub fn object_dir(&self) -> &Path {
        &self.object_dir
    }

    /// Whether corrected commit dates are the generation numbers.
    pub fn reads_generation_data(&self) -> bool {
        self.read_generation_data
    }

    /// The settings of the usable changed-path filters.
    pub fn bloom_settings(&self) -> Option<bloom::Settings> {
        self.bloom
    }

    /// The Bloom settings of the top layer, if it has filters (what a new
    /// write keeps; a layer without filters hides those below).
    pub fn top_bloom_settings(&self) -> Option<bloom::Settings> {
        self.layers.last()?.bloom_settings()
    }

    /// Number of commits in all layers.
    pub fn num_commits(&self) -> u32 {
        self.layers.last().map_or(0, |l| {
            self.offsets[self.offsets.len() - 1] + l.num_commits()
        })
    }

    /// The layer and local position of `pos`.
    fn locate(&self, pos: u32) -> Option<(&Layer, u32)> {
        let i = self.offsets.partition_point(|&o| o <= pos).checked_sub(1)?;
        let local = pos - self.offsets[i];
        let l = &self.layers[i];
        (local < l.num_commits()).then_some((l, local))
    }

    /// The position of `id`.
    pub fn lookup(&self, id: &ObjectId) -> Option<u32> {
        self.layers
            .iter()
            .zip(&self.offsets)
            .rev()
            .find_map(|(l, &o)| l.find(id).map(|i| i + o))
    }

    /// The id at `pos`.
    pub fn id_at(&self, pos: u32) -> Option<ObjectId> {
        self.locate(pos).map(|(l, i)| l.id_at(i))
    }

    /// The stored data of the commit at `pos`.
    pub fn commit_at(&self, pos: u32) -> Result<CommitData, ReadError> {
        let (l, i) = self.locate(pos).ok_or(ReadError::InvalidParent(pos))?;
        let mut parents = Vec::new();
        let read = l.parents_at(i, &mut parents);
        // Positions are checked as they are read: a bad one is reported
        // before a list that runs off the chunk.
        let n = self.num_commits();
        if let Some(&bad) = parents.iter().find(|&&p| p >= n) {
            return Err(ReadError::InvalidParent(bad));
        }
        read?;
        Ok(CommitData {
            tree: l.tree_at(i),
            parents,
            date: l.date_at(i),
        })
    }

    /// The parent ids of the commit at `pos`.
    pub fn parent_ids(&self, pos: u32) -> Result<Vec<ObjectId>, ReadError> {
        let c = self.commit_at(pos)?;
        c.parents
            .iter()
            .map(|&p| self.id_at(p).ok_or(ReadError::InvalidParent(p)))
            .collect()
    }

    /// The topological level of `pos`.
    pub fn level(&self, pos: u32) -> u32 {
        self.locate(pos).map_or(0, |(l, i)| l.level_at(i))
    }

    /// The corrected commit date of `pos`, if the layer has it.
    pub fn corrected_date(&self, pos: u32) -> Result<Option<u64>, ReadError> {
        let Some((l, i)) = self.locate(pos) else {
            return Ok(None);
        };
        Ok(l.generation_offset_at(i)?
            .map(|o| l.date_at(i).saturating_add(o)))
    }

    /// The generation number of `pos`: the corrected commit date when the
    /// graph has them for all layers, otherwise the topological level
    /// ([`GENERATION_V1_MAX`] at most).
    pub fn generation(&self, pos: u32) -> Result<u64, ReadError> {
        if self.read_generation_data {
            if let Some(g) = self.corrected_date(pos)? {
                return Ok(g);
            }
        }
        Ok(u64::from(self.level(pos).min(GENERATION_V1_MAX)))
    }

    /// The changed-path filter of `pos`, if usable.
    pub fn bloom_filter(&self, pos: u32, diags: &mut Vec<Diag>) -> Option<&[u8]> {
        self.bloom?;
        let (l, i) = self.locate(pos)?;
        match l.bloom_filter(i, diags) {
            FilterLookup::Filter(f) if !f.is_empty() => Some(f),
            _ => None,
        }
    }
}
