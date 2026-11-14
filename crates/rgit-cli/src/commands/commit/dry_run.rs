// SPDX-License-Identifier: MIT
//! `commit --short` / `--porcelain` (with `-z`, `--branch`): the short
//! status of what would be committed (Documentation/git-status.adoc,
//! "Short Format"), computed against the index being committed.

use std::collections::BTreeMap;

use crate::commands::status::{BranchInfo, Printer, header};
use crate::wt_status::{Collected, WtChange};

/// One path of the short format.
#[derive(Default)]
struct Line {
    x: u8,
    y: u8,
    old_path: Option<Vec<u8>>,
}

/// Render `c` in the short format.
pub(super) fn short(
    c: &Collected,
    branch: Option<&BranchInfo>,
    nul: bool,
    quote_high: bool,
    prefix: Vec<u8>,
) -> Vec<u8> {
    let mut p = Printer {
        nul,
        quote_high,
        space_quote: true,
        prefix,
        out: Vec::new(),
    };
    if let Some(info) = branch {
        header(&mut p, 1, info);
    }
    let mut lines: BTreeMap<Vec<u8>, Line> = BTreeMap::new();
    for pair in &c.staged {
        let x = match (pair.old, pair.new) {
            _ if pair.is_rename() => b'R',
            (None, _) => b'A',
            (_, None) => b'D',
            _ if pair.type_changed() => b'T',
            _ => b'M',
        };
        let l = lines.entry(pair.path.clone()).or_default();
        l.x = x;
        l.old_path.clone_from(&pair.old_path);
    }
    for (kind, path) in &c.unstaged {
        lines.entry(path.clone()).or_default().y = match kind {
            WtChange::Modified => b'M',
            WtChange::Deleted => b'D',
            WtChange::TypeChanged => b'T',
            WtChange::New => b'A',
        };
    }
    for (path, l) in &lines {
        p.out.push(if l.x == 0 { b' ' } else { l.x });
        p.out.push(if l.y == 0 { b' ' } else { l.y });
        p.out.push(b' ');
        match &l.old_path {
            Some(old) if nul => {
                p.path(path);
                p.end();
                p.path(old);
            }
            Some(old) => {
                p.path(old);
                p.text(" -> ");
                p.path(path);
            }
            None => p.path(path),
        }
        p.end();
    }
    for u in &c.untracked {
        p.text("?? ");
        p.path(u);
        p.end();
    }
    p.out
}
