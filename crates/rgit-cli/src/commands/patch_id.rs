// SPDX-License-Identifier: MIT
//! `git patch-id` (Documentation/git-patch-id.adoc): read patches from
//! standard input and print `<patch id> <commit id>` for each, see
//! [`crate::patch_id`]. Works outside a repository (SHA-1 then).

use std::io::{Read, Write};

use crate::messages::patch_id as msg;
use crate::patch_id::{self, Mode};
use crate::prelude::*;

const UNSTABLE: i32 = 1;
const STABLE: i32 = 2;
const VERBATIM: i32 = 3;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mode = Cell::new(0);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::cmdmode(None, "unstable", &mode, msg::HELP_UNSTABLE, UNSTABLE),
            Opt::cmdmode(None, "stable", &mode, msg::HELP_STABLE, STABLE),
            Opt::cmdmode(None, "verbatim", &mode, msg::HELP_VERBATIM, VERBATIM),
        ],
    );
    // Arguments are ignored.
    parser.parse(argv)?;
    drop(parser);
    let setup = crate::repo::setup_gently(ctx)?;
    let kind = setup
        .as_ref()
        .map_or(rgit_hash::HashKind::Sha1, |s| s.format.object_format);
    let config = crate::comment::gentle_config(ctx)?;
    let config_bool = |key: &str| config.bool(key).ok().flatten().unwrap_or(false);
    let mode = match mode.get() {
        UNSTABLE => Mode::Unstable,
        STABLE => Mode::Stable,
        VERBATIM => Mode::Verbatim,
        _ if config_bool("patchid.verbatim") => Mode::Verbatim,
        _ if config_bool("patchid.stable") => Mode::Stable,
        _ => Mode::Unstable,
    };
    let mut input = Vec::new();
    std::io::stdin()
        .lock()
        .read_to_end(&mut input)
        .map_err(|e| Die::Fatal(crate::global::strerror(&e)))?;
    let mut ids = patch_id::Parser::new(mode, kind);
    let mut out = Vec::new();
    let mut emit = |found: Option<(rgit_hash::ObjectId, rgit_hash::ObjectId)>| {
        if let Some((patch, commit)) = found {
            out.extend_from_slice(format!("{} {}\n", patch.to_hex(), commit.to_hex()).as_bytes());
        }
    };
    for line in input.split_inclusive(|&b| b == b'\n') {
        emit(ids.line(line));
    }
    emit(ids.end());
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(&out);
    let _ = stdout.flush();
    Ok(0)
}
