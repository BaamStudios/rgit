// SPDX-License-Identifier: MIT
//! `git commit-graph verify`: check a commit-graph file or chain against
//! the object database. Messages and exit codes are git's (observed from
//! the reference binary): problems with the file's content print a line
//! each (no prefix) and give exit code 1, a checksum mismatch alone gives
//! 2, a file that cannot be loaded at all prints git's `error:` lines and
//! gives 1.

use std::path::{Path, PathBuf};

use rgit_hash::{HashKind, Hasher};
use rgit_progress::Progress;

use crate::file::{Layer, ReadOptions};
use crate::graph::{
    Graph, LoadOptions, Source, chain_path, load_chain_layers, read_chain, single_path,
};
use crate::messages as msg;
use crate::{Diag, GENERATION_V1_MAX, Objects};

/// The outcome of a verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Exit with this code.
    Exit(i32),
    /// Die with this message (`fatal: `, exit 128).
    Fatal(Error),
}

/// Fatal errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The file could not be opened (message in the output).
    Open,
    /// A parent position beyond the graph (message in the output).
    InvalidParent,
}

/// Verify the commit-graph of `object_dir`; `dirs` are all object
/// directories (for chain layers in alternates). Lines go to `out`
/// (without newline), a fatal error as the last line with `fatal: `.
pub fn verify(
    objects: &dyn Objects,
    object_dir: &Path,
    dirs: &[PathBuf],
    opts: LoadOptions,
    shallow: bool,
    progress: bool,
    out: &mut dyn FnMut(&str),
) -> Outcome {
    let ropts = ReadOptions {
        hash: opts.hash,
        read_bloom: false,
    };
    let mut diags: Vec<Diag> = Vec::new();
    let flush = |diags: &mut Vec<Diag>, out: &mut dyn FnMut(&str)| {
        for d in diags.drain(..) {
            out(&d.line());
        }
    };
    let single = single_path(object_dir);
    let graph;
    // Some layers of the chain could not be loaded: the others are still
    // verified, then that is an error of its own.
    let mut incomplete = false;
    if single.exists() {
        match Layer::open(&single, object_dir, ropts, &mut diags) {
            Ok(l) => {
                graph = Graph::from_layers(vec![l], Source::Single, object_dir, opts, &mut diags);
            }
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                flush(&mut diags, out);
                return Outcome::Exit(1);
            }
            Err(e) => {
                flush(&mut diags, out);
                out(&format!(
                    "fatal: {}",
                    msg::could_not_open_graph(&single.to_string_lossy(), &strerror(&e))
                ));
                return Outcome::Fatal(Error::Open);
            }
        }
    } else {
        let chain = chain_path(object_dir);
        if !chain.exists() {
            return Outcome::Exit(0);
        }
        let lines = match read_chain(&chain, opts.hash, &mut diags) {
            Ok(l) => l,
            Err(m) => {
                flush(&mut diags, out);
                out(&format!("fatal: {m}"));
                return Outcome::Fatal(Error::Open);
            }
        };
        let (layers, complete) = load_chain_layers(dirs, &lines, ropts, &mut diags);
        flush(&mut diags, out);
        if layers.is_empty() {
            return Outcome::Exit(1);
        }
        incomplete = !complete;
        graph = Graph::from_layers(
            layers,
            Source::Chain(lines.hashes.clone()),
            object_dir,
            opts,
            &mut diags,
        );
    }
    flush(&mut diags, out);

    let layers = graph.layers();
    let first = if shallow { layers.len() - 1 } else { 0 };
    let total: u64 = layers[first..]
        .iter()
        .map(|l| u64::from(l.num_commits()))
        .sum();
    let mut p = progress.then(|| Progress::start(msg::VERIFYING, total, true));
    let mut done = 0u64;
    let mut code = 0;
    let mut offset: u32 = layers[..first].iter().map(|l| l.num_commits()).sum();
    let mut results = Vec::new();
    for layer in &layers[first..] {
        let r = verify_layer(
            objects,
            &graph,
            layer,
            offset,
            opts.hash,
            &mut |n| {
                done += n;
                if let Some(p) = p.as_mut() {
                    p.set(done);
                }
            },
            out,
        );
        offset += layer.num_commits();
        results.push(r);
    }
    if let Some(p) = p {
        p.stop();
    }
    for r in results {
        match r {
            Err(e) => return Outcome::Fatal(e),
            Ok(c) => code |= c,
        }
    }
    if incomplete {
        out(&Diag::Error(msg::CHAIN_NOT_LOADED.to_string()).line());
        code |= 1;
    }
    Outcome::Exit(code)
}

fn strerror(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => "Permission denied".to_string(),
        std::io::ErrorKind::NotFound => "No such file or directory".to_string(),
        _ => rgit_fs::messages::strerror(e),
    }
}

/// Verify one layer whose first commit has global position `offset`.
/// `Ok(0)`: fine, `Ok(1)`: problems, `Ok(2)`: only the checksum is wrong.
fn verify_layer(
    objects: &dyn Objects,
    graph: &Graph,
    layer: &Layer,
    offset: u32,
    hash: HashKind,
    tick: &mut dyn FnMut(u64),
    out: &mut dyn FnMut(&str),
) -> Result<i32, Error> {
    let mut problems = false;
    let mut bad_checksum = false;
    let (content, sum) = layer.content_and_checksum();
    let mut h = Hasher::new(hash);
    h.update(content);
    if h.finalize().map(|id| id.as_bytes() != sum).unwrap_or(true) {
        out(msg::BAD_CHECKSUM);
        bad_checksum = true;
    }

    // Id order and fanout.
    let n = layer.num_commits();
    let mut fanout_pos = 0usize;
    let mut prev: Option<rgit_hash::ObjectId> = None;
    let mut known: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for i in 0..n {
        let id = layer.id_at(i);
        if let Some(p) = prev {
            if p >= id {
                out(&msg::incorrect_oid_order(&p.to_hex(), &id.to_hex()));
                problems = true;
            }
        }
        // The commit must be found in the graph: by its id, or as the
        // parent of a commit read before (git parses commits in this
        // order and remembers the positions of their parents).
        let found = if known.contains(&i) {
            Some(i)
        } else {
            layer.find(&id)
        };
        match found {
            Some(at) => {
                let mut parents = Vec::new();
                let _ = layer.parents_at(at, &mut parents);
                for p in parents {
                    if let Some(local) = p.checked_sub(offset) {
                        known.insert(local);
                    }
                }
            }
            None => {
                out(&msg::failed_to_parse_from_graph(&id.to_hex()));
                problems = true;
            }
        }
        let first = usize::from(id.as_bytes()[0]);
        while fanout_pos < first {
            let stored = layer.fanout_at(fanout_pos);
            if stored != i {
                out(&msg::incorrect_fanout(fanout_pos, stored, i));
                problems = true;
            }
            fanout_pos += 1;
        }
        prev = Some(id);
    }
    while fanout_pos < 256 {
        let stored = layer.fanout_at(fanout_pos);
        if stored != n {
            out(&msg::incorrect_fanout(fanout_pos, stored, n));
            problems = true;
        }
        fanout_pos += 1;
    }

    // The file's structure is broken: comparing with the object database
    // would only add noise.
    if problems {
        return Ok(1);
    }

    // Each commit against the object database.
    let mut zero: Option<String> = None;
    let mut nonzero: Option<String> = None;
    for i in 0..n {
        tick(1);
        let pos = offset + i;
        let id = layer.id_at(i);
        let hex = id.to_hex();
        let odb = match objects.commit(&id) {
            Ok(Some(c)) => c,
            Ok(None) => {
                out(&Diag::Error(msg::could_not_read(&hex)).line());
                out(&msg::failed_to_parse(&hex));
                problems = true;
                continue;
            }
            Err(crate::Unparsable) => {
                out(&msg::failed_to_parse(&hex));
                problems = true;
                continue;
            }
        };
        let data = match graph.commit_at(pos) {
            Ok(d) => d,
            Err(crate::ReadError::InvalidParent(p)) => {
                out(&format!("fatal: {}", msg::invalid_parent_position(p)));
                return Err(Error::InvalidParent);
            }
            Err(e) => {
                out(&Diag::Error(e.to_string()).line());
                problems = true;
                continue;
            }
        };
        if data.tree != odb.tree {
            out(&msg::root_tree_mismatch(
                &hex,
                &data.tree.to_hex(),
                &odb.tree.to_hex(),
            ));
            problems = true;
        }
        let mut max_gen = 0u64;
        let mut k = 0;
        for &pp in &data.parents {
            let Some(pid) = graph.id_at(pp) else {
                out(&format!("fatal: {}", msg::invalid_parent_position(pp)));
                return Err(Error::InvalidParent);
            };
            match odb.parents.get(k) {
                None => {
                    out(&msg::parents_too_long(&hex));
                    problems = true;
                    break;
                }
                Some(o) if *o != pid => {
                    out(&msg::parent_mismatch(&hex, &pid.to_hex(), &o.to_hex()));
                    problems = true;
                }
                Some(_) => {}
            }
            let g = graph.generation(pp).unwrap_or(0);
            max_gen = max_gen.max(g);
            k += 1;
        }
        if k < odb.parents.len() && k == data.parents.len() {
            out(&msg::parents_too_short(&hex));
            problems = true;
        }
        let generation = graph.generation(pos).unwrap_or(0);
        let level_zero = layer.level_at(i) == 0;
        if level_zero {
            zero = Some(hex.clone());
        } else {
            nonzero = Some(hex.clone());
        }
        if !level_zero {
            let want = if !graph.reads_generation_data() && max_gen >= u64::from(GENERATION_V1_MAX)
            {
                u64::from(GENERATION_V1_MAX)
            } else {
                max_gen + 1
            };
            if generation < want {
                out(&msg::generation_too_small(&hex, generation, want));
                problems = true;
            }
        }
        if data.date != odb.date {
            out(&msg::date_mismatch(&hex, data.date, odb.date));
            problems = true;
        }
    }
    if let (Some(z), Some(nz)) = (&zero, &nonzero) {
        out(&msg::mixed_generations(z, nz));
        problems = true;
    }
    Ok(if problems {
        1
    } else if bad_checksum {
        2
    } else {
        0
    })
}
