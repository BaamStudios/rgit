// SPDX-License-Identifier: MIT
//! The `ls-refs` command: `HEAD` and the references below `refs/`, in
//! name order, limited by `ref-prefix` arguments.

use std::io::{Read, Write};

use rgit_hash::ObjectId;
use rgit_object::ObjectKind;
use rgit_refs::{IterItem, IterOptions, Peeled, Target};

use super::graph::Graph;
use super::serve::{END_WITHOUT_FLUSH, Session};
use super::{Error, Result, Unborn, messages as msg};

/// A client that sends this many prefixes gets all references: matching
/// them would cost more than it saves.
const TOO_MANY_PREFIXES: usize = 65536;

#[derive(Default)]
struct Args {
    symrefs: bool,
    peel: bool,
    unborn: bool,
    prefixes: Vec<String>,
}

impl Args {
    fn matches(&self, name: &str) -> bool {
        self.prefixes.is_empty() || self.prefixes.iter().any(|p| name.starts_with(p.as_str()))
    }
}

pub(crate) fn run<R: Read, W: Write>(s: &mut Session<'_, R, W>, lines: &[String]) -> Result<()> {
    let mut args = Args::default();
    for line in lines {
        match line.as_str() {
            "symrefs" => args.symrefs = true,
            "peel" => args.peel = true,
            "unborn" => args.unborn = s.settings.unborn != Unborn::Ignore,
            END_WITHOUT_FLUSH => {
                return Err(Error::fatal(msg::expected_flush_after("ls-refs")));
            }
            _ => match line.strip_prefix("ref-prefix ") {
                Some(p) => args.prefixes.push(p.to_owned()),
                None => return Err(Error::fatal(msg::unexpected_line(line))),
            },
        }
    }
    if args.prefixes.len() >= TOO_MANY_PREFIXES {
        args.prefixes.clear();
    }

    let repo = s.repo;
    let graph = Graph::new(repo.odb, repo.hash, &[]);
    let peeled = |r: &rgit_refs::Reference, id: &ObjectId| -> Option<ObjectId> {
        match r.peeled {
            Peeled::Tag(p) => Some(p),
            Peeled::NotTag => None,
            Peeled::Unknown => match graph.peel(id) {
                Some((p, _)) if p != *id => Some(p),
                _ => None,
            },
        }
    };
    let line = |name: &str, id: &ObjectId, symref: Option<&str>, peeled: Option<ObjectId>| {
        let mut line = format!("{id} {name}");
        if let Some(target) = symref {
            line.push_str(" symref-target:");
            line.push_str(target);
        }
        if let Some(p) = peeled {
            line.push_str(&format!(" peeled:{p}"));
        }
        line
    };

    // HEAD first: it is no reference below `refs/`.
    if args.matches("HEAD") {
        let direct = repo.refs.read_raw("HEAD").ok().flatten();
        let resolved = repo.refs.resolve("HEAD").ok().flatten();
        let target = match (&direct, &resolved) {
            (Some(Target::Symbolic(_)), Some(r)) => Some(r.final_name().to_owned()),
            _ => None,
        };
        match resolved.and_then(|r| r.oid) {
            Some(id) => {
                let peeled = if args.peel && is_tag(repo.odb, &id) {
                    graph.peel(&id).map(|(p, _)| p).filter(|p| *p != id)
                } else {
                    None
                };
                let symref = target.as_deref().filter(|_| args.symrefs);
                s.out.line(&line("HEAD", &id, symref, peeled))?;
            }
            None => {
                if let (true, Some(target)) = (args.unborn, &target) {
                    s.out
                        .line(&format!("unborn HEAD symref-target:{target}"))?;
                }
            }
        }
    }

    let all = IterOptions {
        prefix: String::new(),
        include_root_refs: false,
    };
    let items = repo
        .refs
        .iter(&all)
        .map_err(|e| Error::fatal(e.to_string()))?;
    for item in items {
        let IterItem::Ref(r) = item else { continue };
        if !args.matches(&r.name) {
            continue;
        }
        // A symbolic reference to nothing is not listed.
        let Some(id) = r.oid else { continue };
        let target = match (&r.target, args.symrefs) {
            (Target::Symbolic(_), true) => repo
                .refs
                .resolve(&r.name)
                .ok()
                .flatten()
                .map(|res| res.final_name().to_owned()),
            _ => None,
        };
        let peeled = if args.peel { peeled(&r, &id) } else { None };
        s.out.line(&line(&r.name, &id, target.as_deref(), peeled))?;
    }
    s.out.flush()
}

fn is_tag(odb: &rgit_odb::ObjectDatabase, id: &ObjectId) -> bool {
    odb.header(id)
        .ok()
        .flatten()
        .is_some_and(|h| h.kind == ObjectKind::Tag)
}
