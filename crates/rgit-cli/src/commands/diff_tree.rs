// SPDX-License-Identifier: MIT
//! `git diff-tree` (Documentation/git-diff-tree.adoc) for two tree-ishes:
//! raw output (`:mode mode oid oid status<TAB>path`), `--name-only`,
//! `--name-status`, `-z`, and `-p` patches. No rename detection, no
//! pathspecs, no single-commit form yet.

use std::cmp::Ordering;
use std::io::Write;

use rgit_diff::{PatchFile, PatchOptions, quote_path, write_file_patch};
use rgit_hash::ObjectId;
use rgit_object::{EntryMode, Object, ObjectKind, TreeEntry};

use crate::messages::diff_tree as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;

/// One side of a changed path.
#[derive(Clone, Copy)]
struct Side {
    mode: u32,
    oid: ObjectId,
}

/// A changed path, in output order.
struct Pair {
    path: Vec<u8>,
    old: Option<Side>,
    new: Option<Side>,
}

impl Pair {
    fn status(&self) -> u8 {
        match (self.old, self.new) {
            (None, _) => b'A',
            (_, None) => b'D',
            (Some(o), Some(n)) if (o.mode & 0o170000) != (n.mode & 0o170000) => b'T',
            _ => b'M',
        }
    }
}

struct Walker<'a> {
    repo: &'a Repo,
    recursive: bool,
    show_trees: bool,
    pairs: Vec<Pair>,
}

impl Walker<'_> {
    fn entries(&self, id: Option<&ObjectId>) -> Result<Vec<TreeEntry>, Die> {
        let Some(id) = id else {
            return Ok(Vec::new());
        };
        match self.repo.read_object(id) {
            Some(Object::Tree(t)) => Ok(t.entries),
            _ => Err(Die::Fatal(msg::could_not_read(&id.to_hex()))),
        }
    }

    fn side(e: &TreeEntry) -> Side {
        Side {
            mode: e.mode.canonical().bits(),
            oid: e.oid,
        }
    }

    /// A changed entry: recorded, or entered when it is a tree and `-r`.
    fn one(
        &mut self,
        path: Vec<u8>,
        old: Option<&TreeEntry>,
        new: Option<&TreeEntry>,
    ) -> Result<(), Die> {
        let is_tree = old.or(new).is_some_and(|e| e.mode.is_tree());
        if is_tree && self.recursive {
            if self.show_trees {
                self.pairs.push(Pair {
                    path: path.clone(),
                    old: old.map(Self::side),
                    new: new.map(Self::side),
                });
            }
            let mut base = path;
            base.push(b'/');
            return self.walk(old.map(|e| &e.oid), new.map(|e| &e.oid), &base);
        }
        self.pairs.push(Pair {
            path,
            old: old.map(Self::side),
            new: new.map(Self::side),
        });
        Ok(())
    }

    fn walk(
        &mut self,
        a: Option<&ObjectId>,
        b: Option<&ObjectId>,
        base: &[u8],
    ) -> Result<(), Die> {
        let ea = self.entries(a)?;
        let eb = self.entries(b)?;
        let name_of = |e: &TreeEntry| {
            let mut p = base.to_vec();
            p.extend_from_slice(&e.name);
            p
        };
        let (mut i, mut j) = (0, 0);
        while i < ea.len() || j < eb.len() {
            let ord = match (ea.get(i), eb.get(j)) {
                (Some(x), Some(y)) => x.cmp_tree_order(y),
                (Some(_), None) => Ordering::Less,
                _ => Ordering::Greater,
            };
            match ord {
                Ordering::Less => {
                    self.one(name_of(&ea[i]), Some(&ea[i]), None)?;
                    i += 1;
                }
                Ordering::Greater => {
                    self.one(name_of(&eb[j]), None, Some(&eb[j]))?;
                    j += 1;
                }
                Ordering::Equal => {
                    let (x, y) = (&ea[i], &eb[j]);
                    if x.oid != y.oid || x.mode.canonical() != y.mode.canonical() {
                        self.one(name_of(x), Some(x), Some(y))?;
                    }
                    i += 1;
                    j += 1;
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Output {
    Raw,
    NameOnly,
    NameStatus,
    Patch,
}

fn write_path(out: &mut Vec<u8>, path: &[u8], nul: bool) {
    if nul {
        out.extend_from_slice(path);
        out.push(0);
    } else {
        quote_path(out, path, true);
        out.push(b'\n');
    }
}

fn blob_data(repo: &Repo, side: &Side) -> Vec<u8> {
    match repo.read_object(&side.oid) {
        Some(Object::Blob(b)) => b.data,
        _ => Vec::new(),
    }
}

fn is_file(s: &Option<Side>) -> bool {
    s.is_none_or(|s| s.mode != EntryMode::TREE.bits() && s.mode != EntryMode::COMMIT.bits())
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let recursive = Cell::new(false);
    let show_trees = Cell::new(false);
    let patch = Cell::new(false);
    let nul = Cell::new(false);
    let name_only = Cell::new(false);
    let name_status = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag('r', None, &recursive, msg::HELP_R),
            Opt::flag('t', None, &show_trees, msg::HELP_T),
            Opt::flag('p', "patch", &patch, msg::HELP_P),
            Opt::flag('z', None, &nul, msg::HELP_Z),
            Opt::flag(None, "name-only", &name_only, msg::HELP_NAME_ONLY),
            Opt::flag(None, "name-status", &name_status, msg::HELP_NAME_STATUS),
        ],
    );
    let args = parser.parse(argv)?.args;
    if args.len() != 2 {
        return Err(parser.usage_error().into());
    }
    drop(parser);

    let output = if name_only.get() {
        Output::NameOnly
    } else if name_status.get() {
        Output::NameStatus
    } else if patch.get() {
        Output::Patch
    } else {
        Output::Raw
    };
    let nul = nul.get();

    let mut repo = Repo::open(ctx, Mode::RequiredQuiet)?;
    let mut trees = Vec::new();
    for spec in &args {
        let id = repo
            .resolve(spec)
            .and_then(|id| repo.peel_to(id, ObjectKind::Tree))
            .ok_or_else(|| Die::Fatal(msg::bad_object(spec)))?;
        trees.push(id);
    }
    let repo = repo;
    let mut w = Walker {
        repo: &repo,
        // -p and -t imply -r
        recursive: recursive.get() || patch.get() || show_trees.get(),
        show_trees: show_trees.get(),
        pairs: Vec::new(),
    };
    w.walk(Some(&trees[0]), Some(&trees[1]), b"")?;
    let pairs = std::mem::take(&mut w.pairs);

    let mut out = Vec::new();
    let popts = PatchOptions::default();
    let null = ObjectId::null(trees[0].kind());
    for p in &pairs {
        match output {
            Output::NameOnly => write_path(&mut out, &p.path, nul),
            Output::NameStatus => {
                out.push(p.status());
                out.push(if nul { 0 } else { b'\t' });
                write_path(&mut out, &p.path, nul);
            }
            Output::Raw => {
                let (om, oo) = p.old.map_or((0, null), |s| (s.mode, s.oid));
                let (nm, no) = p.new.map_or((0, null), |s| (s.mode, s.oid));
                write!(out, ":{om:06o} {nm:06o} {} {} ", oo.to_hex(), no.to_hex()).unwrap();
                out.push(p.status());
                out.push(if nul { 0 } else { b'\t' });
                write_path(&mut out, &p.path, nul);
            }
            Output::Patch => {
                if !is_file(&p.old) || !is_file(&p.new) {
                    continue;
                }
                let od = p.old.as_ref().map(|s| blob_data(&repo, s));
                let nd = p.new.as_ref().map(|s| blob_data(&repo, s));
                let of = p.old.as_ref().map(|s| PatchFile {
                    path: &p.path,
                    mode: s.mode,
                    oid: s.oid,
                    data: od.as_deref().unwrap_or(b""),
                });
                let nf = p.new.as_ref().map(|s| PatchFile {
                    path: &p.path,
                    mode: s.mode,
                    oid: s.oid,
                    data: nd.as_deref().unwrap_or(b""),
                });
                if p.status() == b'T' {
                    write_file_patch(&mut out, of.as_ref(), None, &popts);
                    write_file_patch(&mut out, None, nf.as_ref(), &popts);
                } else {
                    write_file_patch(&mut out, of.as_ref(), nf.as_ref(), &popts);
                }
            }
        }
    }
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(&out);
    let _ = o.flush();
    Ok(0)
}
