// SPDX-License-Identifier: MIT
//! `git verify-tag` (Documentation/git-verify-tag.adoc): check the
//! signatures of tags (#51). `-v` prints the tag without its signature,
//! `--raw` the status lines of the program instead of its text, and
//! `--format` prints the tag with the format language of `for-each-ref`
//! when the signature is good (and nothing else). Shared with `tag -v`.

use std::io::Write as _;

use rgit_hash::ObjectId;
use rgit_object::ObjectKind;

use crate::messages::signing as msg;
use crate::prelude::*;
use crate::ref_filter::{RefFormat, RefValues, RepoParts};
use crate::refs_support::{self, Objects, Revs};
use crate::repo::Mode;
use crate::signing::{Settings, Trust, verify};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let verbose = Cell::new(false);
    let raw = Cell::new(false);
    let format: RefCell<Option<String>> = RefCell::new(None);
    let mut parser = Parser::new(
        msg::VERIFY_TAG_USAGE,
        vec![
            Opt::flag('v', "verbose", &verbose, msg::HELP_VERBOSE_TAG),
            Opt::flag(None, "raw", &raw, msg::HELP_RAW),
            Opt::string(None, "format", &format, "format", msg::HELP_FORMAT),
        ],
    );
    let args = parser.parse(argv)?.args;
    if args.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let setup = crate::repo::setup(ctx, Mode::Required)?;
    let config = refs_support::load_config(ctx, &setup)?;
    let mut settings = None;
    let objects = Objects::open(&setup);
    let revs = Revs::new(ctx, &setup)?;
    let store = refs_support::ref_store(&setup, &config);
    let format = match format.into_inner() {
        Some(f) => {
            let f = RefFormat::parse(&f)?;
            if f.has_rest() {
                return Err(Die::Fatal(crate::messages::tag::rejected_atom("rest")));
            }
            Some(f)
        }
        None => None,
    };
    let mut values = RefValues::new(&objects, &store).with_repo(RepoParts {
        setup: &setup,
        config: &config,
        revs: &revs,
    });
    if let Some(f) = &format {
        values.prepare(f.atoms())?;
    }
    let flags = Flags {
        verbose: verbose.get() && format.is_none(),
        raw: raw.get(),
        quiet: format.is_some(),
    };
    let mut failed = false;
    for name in &args {
        let Some(oid) = revs.resolve(name) else {
            eprintln!("error: {}", msg::tag_not_found(name));
            failed = true;
            continue;
        };
        let good = verify_tag(&mut settings, &config, &objects, name, &oid, flags)?;
        if good {
            if let Some(f) = &format {
                let mut line = values.render(f, name, oid)?;
                line.push(b'\n');
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(&line);
                let _ = out.flush();
            }
        }
        failed |= !good;
    }
    Ok(i32::from(failed))
}

/// What to print while verifying.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Flags {
    /// Print the tag (without its signature) to standard output.
    pub(crate) verbose: bool,
    /// Print the program's status lines instead of its text.
    pub(crate) raw: bool,
    /// Print nothing (`--format`).
    pub(crate) quiet: bool,
}

/// Verifies the tag `oid` (named `name` in messages); true when the
/// signature is good.
/// `settings` are read from `config` when the first signature is checked.
pub(crate) fn verify_tag(
    settings: &mut Option<Settings>,
    config: &rgit_config::ConfigSet,
    objects: &Objects,
    name: &str,
    oid: &ObjectId,
    flags: Flags,
) -> Result<bool, Die> {
    let Some((kind, data)) = objects.read(oid) else {
        eprintln!("error: {}", msg::tag_not_found(name));
        return Ok(false);
    };
    if kind != ObjectKind::Tag {
        eprintln!("error: {}", msg::non_tag(name, kind.name()));
        return Ok(false);
    }
    let signed = verify::tag_signature(&data);
    let print_payload = |payload: &[u8]| {
        if flags.verbose {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(payload);
            let _ = out.flush();
        }
    };
    let Some((payload, sig)) = signed else {
        print_payload(&data);
        if !flags.quiet {
            eprintln!("error: {}", msg::NO_SIGNATURE);
        }
        return Ok(false);
    };
    let settings = match settings {
        Some(s) => s,
        None => settings.insert(Settings::from_config(config)?),
    };
    let check = verify::check(settings, &payload, &sig);
    if !flags.quiet {
        let mut err = std::io::stderr().lock();
        let _ = err.write_all(if flags.raw { &check.status } else { &check.output });
    }
    print_payload(&payload);
    Ok(check.good(settings.min_trust_or(Trust::Undefined)))
}
