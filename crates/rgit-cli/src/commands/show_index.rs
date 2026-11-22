// SPDX-License-Identifier: MIT
//! `git show-index` (Documentation/git-show-index.adoc): dump a pack index
//! read from standard input, one `<offset> <id>[ (<crc32>)]` line per
//! object in index order. Format: `Documentation/gitformat-pack.adoc`.
//!
//! Like git, the input is read as a stream: a version 1 index prints each
//! entry as it is read; a version 2 index reads ids, CRCs and 32-bit
//! offsets first and then prints, reading 64-bit offsets as they are
//! needed. The trailing checksums are not read.

use std::io::{BufWriter, Read, Write};

use rgit_hash::{HashKind, ObjectId};

use crate::messages::verify_pack as msg;
use crate::prelude::*;

const MAGIC: &[u8; 4] = b"\xfftOc";

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let format: RefCell<Option<String>> = RefCell::new(None);
    let mut parser = Parser::new(
        msg::SHOW_INDEX_USAGE,
        vec![Opt::string(None, "object-format", &format, "hash-algorithm", msg::HELP_OBJECT_FORMAT)],
    );
    parser.parse(argv)?;
    drop(parser);
    let hash = match format.into_inner() {
        Some(name) => HashKind::from_name(&name).ok_or_else(|| Die::Fatal(msg::UNKNOWN_HASH.into()))?,
        None => match crate::repo::setup_gently(ctx)? {
            Some(s) => s.format.object_format,
            None => HashKind::Sha1,
        },
    };
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let result = dump(&mut input, &mut out, hash);
    let _ = out.flush();
    result.map(|()| 0)
}

fn read_exact(input: &mut dyn Read, buf: &mut [u8], what: impl FnOnce() -> String) -> Result<(), Die> {
    input.read_exact(buf).map_err(|_| Die::Fatal(what()))
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn dump(input: &mut dyn Read, out: &mut dyn Write, hash: HashKind) -> Result<(), Die> {
    let raw = hash.byte_len();
    // Eight header bytes: magic and version, or (version 1) the start of
    // the fan-out table.
    let mut head = [0u8; 8];
    read_exact(input, &mut head, || msg::UNABLE_TO_READ_HEADER.into())?;
    let mut fanout = [0u8; 256 * 4];
    let version = if &head[..4] == MAGIC {
        if be32(&head[4..]) != 2 {
            return Err(Die::Fatal(msg::UNKNOWN_INDEX_VERSION.into()));
        }
        read_exact(input, &mut fanout, || msg::UNABLE_TO_READ_INDEX.into())?;
        2
    } else {
        fanout[..8].copy_from_slice(&head);
        read_exact(input, &mut fanout[8..], || msg::UNABLE_TO_READ_INDEX.into())?;
        1
    };
    let n = be32(&fanout[255 * 4..]);
    let w = |e: std::io::Error| Die::Fatal(e.to_string());

    if version == 1 {
        let mut entry = vec![0u8; 4 + raw];
        for i in 0..n {
            read_exact(input, &mut entry, || msg::unable_to_read_entry(i, n))?;
            let id = ObjectId::from_bytes(hash, &entry[4..]).expect("length matches");
            writeln!(out, "{} {id}", be32(&entry)).map_err(w)?;
        }
        return Ok(());
    }

    let count = n as usize;
    let mut ids = Vec::with_capacity(count);
    let mut buf = vec![0u8; raw];
    for i in 0..n {
        read_exact(input, &mut buf, || msg::unable_to_read_sha1(i, n))?;
        ids.push(ObjectId::from_bytes(hash, &buf).expect("length matches"));
    }
    let mut crcs = Vec::with_capacity(count);
    let mut four = [0u8; 4];
    for i in 0..n {
        read_exact(input, &mut four, || msg::unable_to_read_crc(i, n))?;
        crcs.push(be32(&four));
    }
    let mut offsets = Vec::with_capacity(count);
    for i in 0..n {
        read_exact(input, &mut four, || msg::unable_to_read_32bit(i, n))?;
        offsets.push(be32(&four));
    }
    let mut large = 0u32;
    for ((&off, id), crc) in offsets.iter().zip(&ids).zip(&crcs) {
        let offset: u64 = if off & 0x8000_0000 == 0 {
            u64::from(off)
        } else {
            if off & 0x7fff_ffff != large {
                return Err(Die::Fatal(msg::INCONSISTENT_64BIT.into()));
            }
            let mut eight = [0u8; 8];
            read_exact(input, &mut eight, || msg::unable_to_read_64bit(large))?;
            large += 1;
            u64::from_be_bytes(eight)
        };
        writeln!(out, "{offset} {id} ({crc:08x})").map_err(w)?;
    }
    Ok(())
}
