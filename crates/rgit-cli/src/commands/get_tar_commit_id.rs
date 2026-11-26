// SPDX-License-Identifier: MIT
//! `git get-tar-commit-id` (Documentation/git-get-tar-commit-id.adoc):
//! the commit id `git archive` stored in a tar archive, read from the
//! first 1024 bytes of standard input.

use std::io::Read;

use crate::messages::archive as msg;
use crate::prelude::*;

pub fn run(_ctx: &mut Context, argv: &[String]) -> CmdResult {
    if let Some(r) = super::archive::usage_line_help(argv, msg::GET_TAR_COMMIT_ID_USAGE) {
        return r;
    }
    if argv.len() > 1 {
        return Err(super::archive::usage_line_error(msg::GET_TAR_COMMIT_ID_USAGE));
    }
    let mut head = vec![0u8; 1024];
    let mut got = 0;
    let mut stdin = std::io::stdin().lock();
    while got < head.len() {
        match stdin.read(&mut head[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(Die::Fatal(crate::global::strerror(&e))),
        }
    }
    if got < head.len() {
        return Err(Die::Fatal(msg::EOF_BEFORE_HEADER.into()));
    }
    match rgit_archive::tar::commit_id(&head) {
        Some(id) => {
            let mut line = id.to_vec();
            line.push(b'\n');
            std::io::Write::write_all(&mut std::io::stdout(), &line)
                .map_err(|e| Die::Fatal(crate::global::strerror(&e)))?;
            Ok(0)
        }
        None => Ok(1),
    }
}
