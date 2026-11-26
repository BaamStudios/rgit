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
    let found = super::upload_pack::find_repo(dir, false)?.ok_or_else(not_repo)?;
    super::upload_pack::check_ownership(ctx, &found)?;
    std::env::set_current_dir(&found.git_dir).map_err(|_| not_repo())?;
    // Like git: the repository is named explicitly (`GIT_DIR=.`), so
    // `safe.bareRepository` does not apply.
    ctx.git_dir = Some(".".into());
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
