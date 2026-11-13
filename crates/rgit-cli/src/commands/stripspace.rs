// SPDX-License-Identifier: MIT
//! `git stripspace` (Documentation/git-stripspace.adoc): clean up text from
//! standard input like a commit message; `-s` also removes comment lines,
//! `-c` turns every line into a comment instead.
//!
//! The configuration (for the comment string) is only read with `-s` or
//! `-c`, inside a repository or outside; `core.commentChar=auto` means `#`.

use std::io::{Read, Write};

use rgit_commit::message::{comment_lines, stripspace};

use crate::messages::stripspace as msg;
use crate::prelude::*;

const STRIP: i32 = 1;
const COMMENT: i32 = 2;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mode = Cell::new(0);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::cmdmode('s', "strip-comments", &mode, msg::HELP_STRIP, STRIP),
            Opt::cmdmode('c', "comment-lines", &mode, msg::HELP_COMMENT, COMMENT),
        ],
    );
    let args = parser.parse(argv)?.args;
    if !args.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let comment = if mode.get() != 0 {
        let set = crate::comment::gentle_config(ctx)?;
        crate::comment::comment(&set)?.or_hash()
    } else {
        Vec::new()
    };
    let mut input = Vec::new();
    std::io::stdin()
        .lock()
        .read_to_end(&mut input)
        .map_err(|e| Die::Fatal(crate::global::strerror(&e)))?;
    let out = match mode.get() {
        COMMENT => comment_lines(&input, &comment),
        STRIP => stripspace(&input, Some(&comment)),
        _ => stripspace(&input, None),
    };
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(&out);
    let _ = stdout.flush();
    Ok(0)
}
