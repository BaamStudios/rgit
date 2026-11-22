// SPDX-License-Identifier: MIT
//! `git mailinfo` (Documentation/git-mailinfo.adoc): take one e-mail
//! from standard input apart into the info text (standard output), the
//! commit message and the patch. The work is done by
//! [`rgit_mailinfo::info()`].
//!
//! Configuration: `mailinfo.scissors`, `mailinfo.quotedCr`,
//! `i18n.commitEncoding` (read inside a repository or outside).

use std::io::{Read, Write};

use rgit_config::ConfigSet;
use rgit_mailinfo::{Options, QuotedCr};

use crate::messages::mailinfo as msg;
use crate::prelude::*;

/// The charset mail metadata is converted to: `i18n.commitEncoding` or
/// UTF-8.
pub(crate) fn commit_encoding(config: &ConfigSet) -> String {
    config
        .string("i18n.commitencoding")
        .ok()
        .flatten()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "UTF-8".to_owned())
}

/// The defaults of the mailinfo options from the configuration.
pub(crate) fn config_defaults(config: &ConfigSet, opts: &mut Options) -> Result<(), Die> {
    if let Some(b) = config.bool("mailinfo.scissors").ok().flatten() {
        opts.use_scissors = b;
    }
    if let Some(v) = config.string("mailinfo.quotedcr").ok().flatten() {
        let v = v.to_string();
        opts.quoted_cr = QuotedCr::parse(&v)
            .ok_or_else(|| Die::Fatal(msg::bad_quoted_cr(&v, "mailinfo.quotedcr")))?;
    }
    Ok(())
}

/// Prints what [`rgit_mailinfo::info()`] reported.
pub(crate) fn report(info: &rgit_mailinfo::Info) {
    for e in &info.errors {
        eprintln!("error: {e}");
    }
    for w in &info.warnings {
        eprintln!("warning: {w}");
    }
}

const RECODE: i32 = 1;
const NO_RECODE: i32 = 2;
const ENCODING: i32 = 3;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let keep = Cell::new(0);
    let keep_non_patch = Cell::new(0);
    let message_id = Cell::new(-1);
    let recode = Cell::new(RECODE);
    let scissors = Cell::new(-1);
    let inbody = Cell::new(1);
    let quoted_cr: Cell<Option<QuotedCr>> = Cell::new(None);
    let mut parser = Parser::new(
        msg::MAILINFO_USAGE,
        vec![
            Opt::bool('k', None, &keep, msg::HELP_K).noneg(),
            Opt::bool('b', None, &keep_non_patch, msg::HELP_B).noneg(),
            Opt::bool('m', "message-id", &message_id, msg::HELP_M),
            Opt::set_int('u', None, &recode, msg::HELP_U, RECODE),
            Opt::set_int('n', None, &recode, msg::HELP_N, NO_RECODE),
            Opt::callback(None, "encoding", Some("encoding"), msg::HELP_ENCODING, |_, _| {
                recode.set(ENCODING);
                Ok(())
            })
            .noneg(),
            Opt::bool(None, "scissors", &scissors, msg::HELP_SCISSORS),
            Opt::callback(None, "quoted-cr", Some("action"), msg::HELP_QUOTED_CR, |v, _| {
                let v = v.unwrap_or_default();
                match QuotedCr::parse(v) {
                    Some(q) => {
                        quoted_cr.set(Some(q));
                        Ok(())
                    }
                    None => Err(CallbackError::Error(msg::bad_quoted_cr(v, "--quoted-cr"))),
                }
            })
            .noneg(),
            Opt::bool(None, "inbody-headers", &inbody, msg::HELP_INBODY).hidden(),
        ],
    );
    let args = parser.parse(argv)?.args;
    if args.len() != 2 {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    // Repository setup changes to the top of the work tree; the file
    // names are relative to where the command started.
    let start = std::env::current_dir().map_err(|e| Die::Fatal(crate::global::strerror(&e)))?;
    let args: Vec<String> = args
        .iter()
        .map(|a| start.join(a).to_string_lossy().into_owned())
        .collect();
    let shown = argv[argv.len() - 2..].to_vec();
    let config = crate::comment::gentle_config(ctx)?;
    let mut opts = Options::default();
    config_defaults(&config, &mut opts)?;
    opts.keep_subject = keep.get() == 1;
    opts.keep_non_patch = keep_non_patch.get() == 1;
    opts.add_message_id = message_id.get() == 1;
    if scissors.get() != -1 {
        opts.use_scissors = scissors.get() == 1;
    }
    opts.use_inbody_headers = inbody.get() == 1;
    if let Some(q) = quoted_cr.get() {
        opts.quoted_cr = q;
    }
    opts.metainfo_charset = match recode.get() {
        NO_RECODE => None,
        // `--encoding` turns re-coding off (observed with git 2.56).
        ENCODING => None,
        _ => Some(commit_encoding(&config)),
    };

    let mut input = Vec::new();
    std::io::stdin()
        .lock()
        .read_to_end(&mut input)
        .map_err(|e| Die::Fatal(crate::global::strerror(&e)))?;
    let create = |path: &str, shown: &str| {
        std::fs::File::create(path).map_err(|e| {
            eprintln!("{}", msg::no_such_file(shown, &crate::global::strerror(&e)));
            Die::Exit(128)
        })
    };
    let mut msg_file = create(&args[0], &shown[0])?;
    let mut patch_file = create(&args[1], &shown[1])?;
    if input.is_empty() {
        eprintln!("error: {}", msg::empty_patch(&shown[1]));
        return Ok(1);
    }
    let info = rgit_mailinfo::info(&input, &opts);
    report(&info);
    let _ = msg_file.write_all(&info.msg);
    let _ = patch_file.write_all(&info.patch);
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(&info.info);
    let _ = stdout.flush();
    Ok(if info.failed { 1 } else { 0 })
}
