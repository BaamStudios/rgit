// SPDX-License-Identifier: MIT
//! `git upload-archive--writer`: the archiver `upload-archive` runs. It
//! enters the repository, reads the client's `argument` packets from
//! standard input and writes the archive to standard output with the
//! restrictions for remote clients (Documentation/git-upload-archive.adoc,
//! "SECURITY").

use rgit_archive::remote;
use rgit_protocol::pktline::Reader;

use super::archive::{Origin, archive};
use crate::messages::archive as msg;
use crate::prelude::*;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    if let Some(r) = super::archive::usage_line_help(argv, msg::UPLOAD_ARCHIVE_USAGE) {
        return r;
    }
    if argv.len() != 2 {
        return Err(super::archive::usage_line_error(msg::UPLOAD_ARCHIVE_USAGE));
    }
    let dir = &argv[1];
    let not_repo = || Die::Fatal(msg::not_a_repository(dir));
    let candidates = [
        format!("{dir}.git/.git"),
        format!("{dir}/.git"),
        format!("{dir}.git"),
        dir.clone(),
    ];
    let target = candidates
        .iter()
        .find(|c| {
            let p = std::path::Path::new(c);
            p.is_dir() && (p.join("HEAD").is_file() || p.join(".git").exists())
        })
        .ok_or_else(not_repo)?;
    std::env::set_current_dir(target).map_err(|_| not_repo())?;
    if crate::repo::setup_gently(ctx)?.is_none() {
        return Err(not_repo());
    }

    let mut reader = Reader::new(std::io::stdin().lock(), "upload-archive");
    reader.chomp_newline(true);
    let client_args = remote::read_arguments(&mut reader).map_err(Die::Fatal)?;
    let mut full = vec!["archive".to_string()];
    full.extend(client_args);
    archive(ctx, &full, None, Origin::Remote)
}
