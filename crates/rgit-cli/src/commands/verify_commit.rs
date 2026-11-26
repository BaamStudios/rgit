// SPDX-License-Identifier: MIT
//! `git verify-commit` (Documentation/git-verify-commit.adoc): check the
//! signatures of commits (#51). `-v` prints the commit without its
//! signature headers, `--raw` the status lines of the program instead of
//! its text. Commits without a signature fail silently.

use std::io::Write as _;

use rgit_object::ObjectKind;

use crate::messages::signing as msg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, Revs};
use crate::repo::Mode;
use crate::signing::{Settings, Trust, verify};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let verbose = Cell::new(false);
    let raw = Cell::new(false);
    let mut parser = Parser::new(
        msg::VERIFY_COMMIT_USAGE,
        vec![
            Opt::flag('v', "verbose", &verbose, msg::HELP_VERBOSE_COMMIT),
            Opt::flag(None, "raw", &raw, msg::HELP_RAW),
        ],
    );
    let args = parser.parse(argv)?.args;
    if args.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let setup = crate::repo::setup(ctx, Mode::Required)?;
    let config = refs_support::load_config(ctx, &setup)?;
    // Read when the first signature is checked (git does not complain
    // about the `gpg.*` settings before).
    let mut settings: Option<Settings> = None;
    let mut objects = Objects::open(&setup);
    objects.use_replace_refs(&setup, &config);
    let revs = Revs::new(ctx, &setup)?;
    let mut failed = false;
    for name in &args {
        let found = revs.resolve(name).and_then(|oid| objects.read(&oid));
        let Some((kind, data)) = found else {
            eprintln!("error: {}", msg::commit_not_found(name));
            failed = true;
            continue;
        };
        if kind != ObjectKind::Commit {
            eprintln!("error: {}", msg::non_commit(name, kind.name()));
            failed = true;
            continue;
        }
        let Some((payload, sig)) = verify::commit_signature(objects.hash(), &data) else {
            failed = true;
            continue;
        };
        let settings = match &settings {
            Some(s) => s,
            None => settings.insert(Settings::from_config(&config)?),
        };
        let check = verify::check(settings, &payload, &sig);
        {
            let mut err = std::io::stderr().lock();
            let _ = err.write_all(if raw.get() { &check.status } else { &check.output });
        }
        if verbose.get() {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(&payload);
            let _ = out.flush();
        }
        failed |= !check.good(settings.min_trust_or(Trust::Undefined));
    }
    Ok(i32::from(failed))
}
