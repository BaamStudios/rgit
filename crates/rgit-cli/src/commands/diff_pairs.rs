// SPDX-License-Identifier: MIT
//! `git diff-pairs` (Documentation/git-diff-pairs.adoc): the output phase
//! of the diff pipeline. Reads file pairs in the NUL-terminated raw format
//! (`diff-tree -z -r --raw`) from standard input and shows them in the
//! requested formats (a patch by default). An empty record (a single NUL
//! between records) shows the pairs read so far and writes a NUL.

use std::io::{Read, Write};

use rgit_hash::ObjectId;

use crate::diff::collect::{Pair, Side};
use crate::diff::output::Output;
use crate::diff::{self, options::usage_exit};
use crate::messages::diff_pairs as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let help = format!("usage: {}\n", msg::USAGE);
    if let Some(code) = diff::help(argv, &help) {
        return Ok(code);
    }
    // `--abbrev` has no effect on the pairs read (observed with git 2.56.1).
    let args: Vec<String> = argv[1..]
        .iter()
        .take_while(|a| *a != "--")
        .filter(|a| !(a.starts_with("--abbrev") || *a == "--no-abbrev"))
        .chain(argv[1..].iter().skip_while(|a| *a != "--"))
        .cloned()
        .collect();
    let parsed = diff::parse_args(&args, |_| Ok(false), |_| usage_exit(help.clone()))?;
    if !parsed.args.is_empty() {
        return Err(usage_exit(format!("usage: {}\n", msg::PATHSPEC_NOT_SUPPORTED)));
    }
    let mut opts = parsed.opts;
    if !opts.nul {
        return Err(usage_exit(format!("usage: {}\n", msg::WITHOUT_Z)));
    }
    let mut repo = Repo::open(ctx, Mode::RequiredQuiet)?;
    let formats = opts.formats(true)?;
    opts.resolve_objects(&mut repo)?;
    let mut input = Vec::new();
    std::io::stdin()
        .lock()
        .read_to_end(&mut input)
        .map_err(|e| Die::Fatal(crate::global::strerror(&e)))?;

    let mut stdout = std::io::stdout().lock();
    let mut code = 0;
    let mut pairs: Vec<Pair> = Vec::new();
    let mut fields = Fields {
        data: &input,
        pos: 0,
    };
    let emit = |pairs: Vec<Pair>, out: &mut dyn Write| -> Result<i32, Die> {
        let mut o = Output::new(Some(&repo), repo.hash, &opts, formats, false)?;
        o.set_color(&opts)?;
        let (bytes, changed) = o.render(pairs)?;
        let _ = out.flush();
        Ok(o.finish(&bytes, changed, &opts))
    };
    while let Some(record) = fields.next() {
        if record.is_empty() {
            // An explicit flush.
            code |= emit(std::mem::take(&mut pairs), &mut stdout)?;
            let _ = stdout.write_all(b"\0");
            let _ = stdout.flush();
            continue;
        }
        let mut pair = parse_record(record, &mut fields, repo.hash)?;
        if opts.reverse && pair.is_rename() {
            // `-R` swaps the sides of the other pairs only; the output
            // swaps them back (observed with git 2.56.1).
            std::mem::swap(&mut pair.old, &mut pair.new);
            std::mem::swap(&mut pair.old_path, &mut pair.path);
        }
        pairs.push(pair);
    }
    if !pairs.is_empty() {
        code |= emit(pairs, &mut stdout)?;
    }
    let _ = stdout.flush();
    Ok(code)
}

/// The NUL-terminated fields of the input.
struct Fields<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Fields<'a> {
    fn next(&mut self) -> Option<&'a [u8]> {
        if self.pos >= self.data.len() {
            return None;
        }
        let rest = &self.data[self.pos..];
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        self.pos += end + 1;
        Some(&rest[..end])
    }
}

/// One raw record: `:<mode> <mode> <oid> <oid> <status>[<score>]`
/// followed by one path (two for renames and copies).
fn parse_record(
    record: &[u8],
    fields: &mut Fields<'_>,
    hash: rgit_hash::HashKind,
) -> Result<Pair, Die> {
    let bad = || Die::Fatal(msg::INVALID_RAW.into());
    let meta = record.strip_prefix(b":").ok_or_else(bad)?;
    let parts: Vec<&[u8]> = meta.split(|&b| b == b' ').collect();
    let [old_mode, new_mode, old_oid, new_oid, status] = parts[..] else {
        return Err(bad());
    };
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    let mode = |m: &[u8]| {
        std::str::from_utf8(m)
            .ok()
            .and_then(|s| u32::from_str_radix(s, 8).ok())
            .ok_or_else(bad)
    };
    let (old_mode, new_mode) = (mode(old_mode)?, mode(new_mode)?);
    if old_mode & 0o170000 == 0o040000 || new_mode & 0o170000 == 0o040000 {
        return Err(Die::Fatal(msg::TREES_NOT_SUPPORTED.into()));
    }
    let oid = |o: &[u8], rest: &[u8]| {
        ObjectId::from_hex(hash, o).map_err(|_| Die::Fatal(msg::bad_object_id(&text(rest))))
    };
    let after_modes = &meta[parts[0].len() + parts[1].len() + 2..];
    let after_old = &after_modes[parts[2].len() + 1..];
    let (old_oid, new_oid) = (oid(old_oid, after_modes)?, oid(new_oid, after_old)?);
    let (&letter, score) = status.split_first().ok_or_else(bad)?;
    let path = fields.next().ok_or_else(bad)?.to_vec();
    let side = |mode: u32, oid: ObjectId| {
        (mode != 0).then_some(Side {
            mode,
            oid,
            file: None,
        })
    };
    let mut pair = Pair::new(
        path.clone(),
        side(old_mode, old_oid),
        side(new_mode, new_oid),
    );
    match letter {
        b'R' | b'C' => {
            let new_path = fields.next().ok_or_else(bad)?.to_vec();
            let percent: u32 = std::str::from_utf8(score)
                .ok()
                .and_then(|s| s.parse().ok())
                .ok_or_else(bad)?;
            pair.path = new_path;
            pair.renamed = Some(letter);
            pair.score = percent * rgit_diff::diffcore::MAX_SCORE / 100;
        }
        b'M' | b'T' if !score.is_empty() => {
            let percent: u32 = std::str::from_utf8(score)
                .ok()
                .and_then(|s| s.parse().ok())
                .ok_or_else(bad)?;
            pair.score = percent * rgit_diff::diffcore::MAX_SCORE / 100;
        }
        b'A' | b'D' | b'M' | b'T' | b'U' | b'X' => {}
        _ => return Err(Die::Fatal(msg::unknown_status(&text(status)))),
    }
    Ok(pair)
}
