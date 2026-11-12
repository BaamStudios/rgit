// SPDX-License-Identifier: MIT
//! `git mktree` (Documentation/git-mktree.adoc): build a tree object from
//! `ls-tree` formatted text.

use std::io::{BufRead, Write};

use rgit_object::{EntryMode, ObjectKind, Tree, TreeEntry};

use crate::messages::mktree as msg;
use crate::repo_objects::Repo;
use crate::prelude::*;
use crate::repo::{self, Mode};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let nul = Cell::new(false);
    let missing = Cell::new(false);
    let batch = Cell::new(false);
    let mut parser = Parser::new(
        msg::MKTREE_USAGE,
        vec![
            Opt::flag('z', None, &nul, msg::MKTREE_HELP_Z),
            Opt::flag(None, "missing", &missing, msg::MKTREE_HELP_MISSING),
            Opt::flag(None, "batch", &batch, msg::MKTREE_HELP_BATCH),
        ],
    );
    parser.parse(argv)?;
    drop(parser);
    let setup = repo::setup(ctx, Mode::Required)?;
    let repo = Repo::open(ctx, setup)?;
    let term = if nul.get() { 0 } else { b'\n' };
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = std::io::stdout().lock();
    loop {
        let mut tree = Tree::new();
        let mut eof = false;
        loop {
            let mut rec = Vec::new();
            let n = input
                .read_until(term, &mut rec)
                .map_err(|e| Die::Fatal(crate::global::strerror(&e)))?;
            if n == 0 {
                eof = true;
                break;
            }
            if rec.last() == Some(&term) {
                rec.pop();
            }
            if rec.is_empty() {
                if !batch.get() {
                    return Err(Die::Fatal(msg::BLANK_LINE_ONLY_BATCH.into()));
                }
                break;
            }
            tree.entries
                .push(parse_entry(&repo, &rec, nul.get(), missing.get())?);
        }
        if eof && tree.entries.is_empty() && batch.get() {
            break;
        }
        // Stable sort: duplicates stay in input order.
        tree.entries.sort_by(TreeEntry::cmp_tree_order);
        let oid = repo
            .odb
            .write(ObjectKind::Tree, &tree.serialize())
            .map_err(|e| Die::Fatal(e.to_string()))?;
        let _ = writeln!(out, "{oid}");
        let _ = out.flush();
        if eof {
            break;
        }
    }
    Ok(0)
}

/// The type an entry's mode stands for.
fn mode_kind(bits: u32) -> ObjectKind {
    match bits & 0o170000 {
        0o040000 => ObjectKind::Tree,
        0o160000 => ObjectKind::Commit,
        _ => ObjectKind::Blob,
    }
}

/// Parse `<mode> SP <type> SP <oid> TAB <path>`.
fn parse_entry(repo: &Repo, rec: &[u8], nul: bool, allow_missing: bool) -> Result<TreeEntry, Die> {
    let format_error = || Die::Fatal(msg::input_format_error(&String::from_utf8_lossy(rec)));
    // The mode is read like strtoul(.., 8): leading blanks are skipped.
    let start = rec
        .iter()
        .position(|c| !matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c))
        .unwrap_or(rec.len());
    let digits = rec[start..]
        .iter()
        .take_while(|c| (b'0'..=b'7').contains(*c))
        .count();
    if digits == 0 || rec.get(start + digits) != Some(&b' ') {
        return Err(format_error());
    }
    let mut bits: u32 = 0;
    for &d in &rec[start..start + digits] {
        bits = bits
            .checked_mul(8)
            .and_then(|b| b.checked_add(u32::from(d - b'0')))
            .ok_or_else(format_error)?;
    }
    let rest = &rec[start + digits + 1..];
    let sp = rest.iter().position(|&c| c == b' ').ok_or_else(format_error)?;
    let type_name = &rest[..sp];
    let rest = &rest[sp + 1..];
    let hex_len = repo.hash.hex_len();
    if rest.len() <= hex_len || rest[hex_len] != b'\t' {
        return Err(format_error());
    }
    let oid = rgit_hash::ObjectId::from_hex(repo.hash, &rest[..hex_len]).map_err(|_| format_error())?;
    let raw_path = &rest[hex_len + 1..];
    let path = if !nul && raw_path.first() == Some(&b'"') {
        crate::quote::c_unquote(raw_path).ok_or_else(|| Die::Fatal(msg::INVALID_QUOTING.into()))?
    } else {
        raw_path.to_vec()
    };
    let shown = String::from_utf8_lossy(&path).into_owned();
    if path.contains(&b'/') {
        return Err(Die::Fatal(msg::path_contains_slash(&shown)));
    }
    let type_str = String::from_utf8_lossy(type_name);
    let given = ObjectKind::from_name(type_name)
        .ok_or_else(|| Die::Fatal(msg::invalid_object_type(&type_str)))?;
    let expected = mode_kind(bits);
    if given != expected {
        return Err(Die::Fatal(msg::mode_type_mismatch(
            &shown,
            given.name(),
            expected.name(),
        )));
    }
    match repo.odb.header(&oid) {
        Ok(Some(h)) => {
            if h.kind != expected {
                return Err(Die::Fatal(msg::entry_wrong_type(
                    &shown,
                    &oid.to_hex(),
                    h.kind.name(),
                    given.name(),
                )));
            }
        }
        _ => {
            if !allow_missing && expected != ObjectKind::Commit {
                return Err(Die::Fatal(msg::entry_unavailable(&shown, &oid.to_hex())));
            }
        }
    }
    Ok(TreeEntry::new(EntryMode::from_bits(bits), path, oid))
}
