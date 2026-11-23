// SPDX-License-Identifier: MIT
//! `--remerge-diff` (Documentation/diff-options.adoc): the two parents of
//! a merge are merged again with `rgit-merge` (conflict markers included,
//! the sides labelled `<abbrev> (<subject>)`), and the result is
//! compared with the merge's tree. The conflicts and notes of that merge
//! become `remerge <message>` header lines of the first path each is
//! about: in that path's patch before its `index` line, or on their own
//! (`diff --git` line and headers) for a path without a change, which
//! counts as unmerged for `--diff-filter`. In the patch the pairs are in
//! the order of their old paths. The remerged objects are written to the
//! object database, unreferenced.

use rgit_hash::ObjectId;

use super::options::{DiffOpts, Formats};
use crate::Die;
use crate::repository::Repo;

/// The remerge diff of `parents` against `tree` in `formats`, without a
/// separator before it.
pub fn diff(
    repo: &Repo,
    opts: &DiffOpts,
    formats: Formats,
    pathspec: &rgit_pathspec::Pathspec,
    parents: [&ObjectId; 2],
    tree: &ObjectId,
) -> Result<Vec<u8>, Die> {
    let [ours, theirs] = parents;
    let len = repo.default_abbrev();
    let label = |c: &ObjectId| {
        let subject = match repo.read_object(c) {
            Some(rgit_object::Object::Commit(commit)) => {
                let m = commit.message();
                m.split(|&b| b == b'\n').next().unwrap_or_default().to_vec()
            }
            _ => Vec::new(),
        };
        let mut l = repo.abbrev(c, len).into_bytes();
        l.extend_from_slice(b" (");
        l.extend_from_slice(&subject);
        l.push(b')');
        l
    };
    let mut merge_opts = rgit_merge::Options {
        branch1: label(ours),
        branch2: label(theirs),
        allow_unrelated_histories: true,
        ..rgit_merge::Options::default()
    };
    crate::commands::merge_tree::apply_rename_config(&repo.config, &mut merge_opts);
    let commits = rgit_merge::ObjectCommits {
        objects: repo.odb(),
        shallow: std::collections::HashSet::new(),
        label: Box::new(|id: &ObjectId| repo.abbrev(id, len).into_bytes()),
    };
    let result = rgit_merge::merge_commits(repo.odb(), &commits, ours, theirs, &merge_opts)
        .map_err(crate::commands::merge_tree::merge_failed)?;
    // The header lines of each path (the first one a message is about).
    let mut headers: std::collections::BTreeMap<Vec<u8>, Vec<u8>> = Default::default();
    for m in &result.messages {
        if m.kind == rgit_merge::MessageKind::AutoMerging {
            continue;
        }
        let Some(path) = m.paths.first() else {
            continue;
        };
        let h = headers.entry(path.clone()).or_default();
        h.extend_from_slice(b"remerge ");
        h.extend_from_slice(&m.text);
        h.push(b'\n');
    }
    headers.retain(|p, _| pathspec.is_empty() || pathspec.matches(p));
    let mut w = super::collect::TreeDiff {
        repo,
        pathspec,
        recursive: true,
        show_trees: false,
        max_depth: None,
        pairs: Vec::new(),
    };
    w.walk(Some(&result.tree), Some(tree), b"")?;
    // Without `plain`, with the diffcore transformations; with it, the
    // pairs are rendered as they are.
    let new_output = |formats, plain: bool| -> Result<super::output::Output<'_>, Die> {
        let mut out = super::output::Output::new(Some(repo), repo.hash, opts, formats, true)?;
        out.set_color(opts)?;
        if plain {
            out.core = rgit_diff::diffcore::Options::default();
            out.filter = Default::default();
            out.pickaxe = None;
        }
        Ok(out)
    };
    let main = new_output(formats, false)?;
    let pairs = main.diffcore(std::mem::take(&mut w.pairs))?;
    // Header-only entries count as unmerged (`--diff-filter=U`).
    let unmerged_shown =
        main.filter.apply_by(vec![()], |_| (b'U', false)).len() == 1 && main.pickaxe.is_none();
    let mut bytes = Vec::new();
    let others = Formats {
        patch: false,
        ..formats
    };
    if others.any() {
        let (b, _) = new_output(others, true)?.render(pairs.clone())?;
        bytes = b;
    }
    if formats.patch {
        if !bytes.is_empty() {
            bytes.push(b'\n');
        }
        let patch_only = Formats {
            patch: true,
            ..Default::default()
        };
        let out = new_output(patch_only, true)?;
        let key = |p: &super::collect::Pair| {
            if p.old.is_some() {
                p.old_path.clone()
            } else {
                p.path.clone()
            }
        };
        let mut pending: Vec<(Vec<u8>, Vec<u8>)> = if unmerged_shown {
            headers
                .iter()
                .filter(|(path, _)| {
                    !pairs
                        .iter()
                        .any(|p| &p.path == *path || &p.old_path == *path)
                })
                .map(|(p, h)| (p.clone(), h.clone()))
                .collect()
        } else {
            Vec::new()
        };
        pending.reverse();
        // In the patch the pairs are in the order of their old paths.
        let mut pairs = pairs;
        pairs.sort_by_key(key);
        let header_only = |path: &[u8], h: &[u8], bytes: &mut Vec<u8>| {
            let mut a = out.patch.src_prefix.clone();
            a.extend_from_slice(path);
            let mut b = out.patch.dst_prefix.clone();
            b.extend_from_slice(path);
            bytes.extend_from_slice(b"diff --git ");
            rgit_diff::quote_path(bytes, &a, out.quote_high);
            bytes.push(b' ');
            rgit_diff::quote_path(bytes, &b, out.quote_high);
            bytes.push(b'\n');
            bytes.extend_from_slice(h);
        };
        for p in pairs {
            let k = key(&p);
            while pending.last().is_some_and(|(path, _)| *path < k) {
                let (path, h) = pending.pop().expect("checked");
                header_only(&path, &h, &mut bytes);
            }
            let h = headers
                .get(&p.old_path)
                .or_else(|| headers.get(&p.path))
                .cloned();
            let (text, _) = out.render(vec![p])?;
            match h {
                Some(h) => insert_headers(&mut bytes, &text, &h),
                None => bytes.extend_from_slice(&text),
            }
        }
        while let Some((path, h)) = pending.pop() {
            header_only(&path, &h, &mut bytes);
        }
    }
    Ok(bytes)
}

/// Inserts the header lines `headers` into the file patch `patch`
/// before its `index` line (or its end) and appends it to `out`.
fn insert_headers(out: &mut Vec<u8>, patch: &[u8], headers: &[u8]) {
    let mut at = patch.len();
    let mut pos = 0;
    for line in patch.split_inclusive(|&b| b == b'\n') {
        if pos > 0
            && (line.starts_with(b"index ")
                || line.starts_with(b"--- ")
                || line.starts_with(b"Binary files"))
        {
            at = pos;
            break;
        }
        pos += line.len();
    }
    out.extend_from_slice(&patch[..at]);
    out.extend_from_slice(headers);
    out.extend_from_slice(&patch[at..]);
}
