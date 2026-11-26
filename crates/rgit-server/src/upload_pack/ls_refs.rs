// SPDX-License-Identifier: MIT
//! The `ls-refs` command: `HEAD` and the references below `refs/`, in
//! name order, limited by `ref-prefix` arguments.

use std::io::{Read, Write};

use rgit_hash::ObjectId;
use rgit_refs::Peeled;

use super::graph::Graph;
use super::refs::{RefView, Visible};
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
    let view = RefView::new(repo.refs, repo.config, s.opts.namespace.as_deref());
    let graph = Graph::new(repo.odb, repo.hash, &[]);
    // The object a tag finally names, if `id` is a tag.
    let peeled = |r: &Visible| -> Option<ObjectId> {
        match r.reference.as_ref().map(|r| r.peeled) {
            Some(Peeled::Tag(p)) => Some(p),
            Some(Peeled::NotTag) => None,
            _ => match graph.peel(&r.id) {
                Some((p, _)) if p != r.id => Some(p),
                _ => None,
            },
        }
    };
    let line = |r: &Visible| {
        let mut line = format!("{} {}", r.id, r.name);
        if let Some(target) = r.symref_target.as_deref().filter(|_| args.symrefs) {
            line.push_str(" symref-target:");
            line.push_str(target);
        }
        if let Some(p) = Some(r).filter(|_| args.peel).and_then(peeled) {
            line.push_str(&format!(" peeled:{p}"));
        }
        line
    };

    // HEAD first: it is no reference below `refs/`.
    if args.matches("HEAD") {
        match view.head() {
            (Some(head), _) => s.out.line(&line(&head))?,
            (None, Some(unborn)) if args.unborn => {
                s.out
                    .line(&format!("unborn HEAD symref-target:{}", unborn.target))?;
            }
            _ => {}
        }
    }
    let refs = view.list().map_err(|e| Error::fatal(e.to_string()))?;
    for r in refs.iter().filter(|r| args.matches(&r.name)) {
        s.out.line(&line(r))?;
    }
    s.out.flush()
}
