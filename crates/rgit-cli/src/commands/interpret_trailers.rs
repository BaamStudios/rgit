// SPDX-License-Identifier: MIT
//! `git interpret-trailers` (Documentation/git-interpret-trailers.adoc,
//! Documentation/config/trailer.adoc): add or parse the trailers of
//! messages read from files or standard input. The work is done by
//! `rgit_trailer`.
//!
//! `--where`, `--if-exists` and `--if-missing` apply to the `--trailer`
//! options after them. Like git, the command moves to the top level of a
//! repository it runs in, and file names are not adjusted for that.

use std::io::{Read, Write};
use std::path::Path;

use bstr::ByteSlice;
use rgit_trailer::{Config, IfExists, IfMissing, NewTrailer, Options, Where};

use crate::messages::interpret_trailers as msg;
use crate::prelude::*;

/// The options as they are given.
#[derive(Default)]
struct State {
    where_: Option<Where>,
    if_exists: Option<IfExists>,
    if_missing: Option<IfMissing>,
    trailers: Vec<NewTrailer>,
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let in_place = Cell::new(false);
    let trim_empty = Cell::new(false);
    let only_trailers = Cell::new(false);
    let only_input = Cell::new(false);
    let unfold = Cell::new(false);
    let no_divider = Cell::new(false);
    let state = RefCell::new(State::default());
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag(None, "in-place", &in_place, msg::HELP_IN_PLACE),
            Opt::flag(None, "trim-empty", &trim_empty, msg::HELP_TRIM_EMPTY),
            Opt::callback(
                None,
                "where",
                Some("placement"),
                msg::HELP_WHERE,
                |v, unset| {
                    let mut s = state.borrow_mut();
                    s.where_ = match (unset, v) {
                        (true, _) | (_, None) => None,
                        (false, Some(v)) => {
                            Some(Where::parse(v.as_bytes()).ok_or(CallbackError::Silent)?)
                        }
                    };
                    Ok(())
                },
            ),
            Opt::callback(
                None,
                "if-exists",
                Some("action"),
                msg::HELP_IF_EXISTS,
                |v, unset| {
                    let mut s = state.borrow_mut();
                    s.if_exists = match (unset, v) {
                        (true, _) | (_, None) => None,
                        (false, Some(v)) => {
                            Some(IfExists::parse(v.as_bytes()).ok_or(CallbackError::Silent)?)
                        }
                    };
                    Ok(())
                },
            ),
            Opt::callback(
                None,
                "if-missing",
                Some("action"),
                msg::HELP_IF_MISSING,
                |v, unset| {
                    let mut s = state.borrow_mut();
                    s.if_missing = match (unset, v) {
                        (true, _) | (_, None) => None,
                        (false, Some(v)) => {
                            Some(IfMissing::parse(v.as_bytes()).ok_or(CallbackError::Silent)?)
                        }
                    };
                    Ok(())
                },
            ),
            Opt::flag(None, "only-trailers", &only_trailers, msg::HELP_ONLY_TRAILERS),
            Opt::flag(None, "only-input", &only_input, msg::HELP_ONLY_INPUT),
            Opt::flag(None, "unfold", &unfold, msg::HELP_UNFOLD),
            Opt::callback(None, "parse", None, msg::HELP_PARSE, |_, _| {
                only_trailers.set(true);
                only_input.set(true);
                unfold.set(true);
                Ok(())
            })
            .noarg()
            .noneg(),
            Opt::flag(None, "no-divider", &no_divider, msg::HELP_NO_DIVIDER),
            Opt::callback(
                None,
                "trailer",
                Some("trailer"),
                msg::HELP_TRAILER,
                |v, unset| {
                    let mut s = state.borrow_mut();
                    match (unset, v) {
                        (true, _) | (_, None) => s.trailers.clear(),
                        (false, Some(v)) => {
                            let t = NewTrailer {
                                text: v.into(),
                                where_: s.where_,
                                if_exists: s.if_exists,
                                if_missing: s.if_missing,
                            };
                            s.trailers.push(t);
                        }
                    }
                    Ok(())
                },
            ),
        ],
    );
    let files = parser.parse(argv)?.args;
    if only_input.get() && !state.borrow().trailers.is_empty() {
        return Err(parser.usage_msg(msg::TRAILER_WITH_ONLY_INPUT).into());
    }
    drop(parser);
    let args = state.into_inner().trailers;

    let set = crate::comment::gentle_config(ctx)?;
    let comment = crate::comment::comment(&set)?.or_hash();
    let (conf, warnings) = match Config::from_config(&set) {
        Ok(c) => c,
        Err(entry) => {
            let name = entry.name.to_str_lossy();
            return Err(crate::comment::bad_entry(
                &entry,
                &crate::messages::comment::missing_value(&name),
            ));
        }
    };
    for w in warnings {
        eprintln!("warning: {w}");
    }
    let opts = Options {
        trim_empty: trim_empty.get(),
        only_trailers: only_trailers.get(),
        only_input: only_input.get(),
        unfold: unfold.get(),
        no_divider: no_divider.get(),
        comment: comment.into(),
    };
    let mut on_error = |m: &str| eprintln!("error: {m}");

    if files.is_empty() {
        if in_place.get() {
            return Err(Die::Fatal(msg::NO_INPUT_FILE.into()));
        }
        let mut input = Vec::new();
        std::io::stdin()
            .lock()
            .read_to_end(&mut input)
            .map_err(|e| Die::Fatal(msg::could_not_read("-", &crate::global::strerror(&e))))?;
        let out = rgit_trailer::process(&input, &conf, &opts, &args, &mut on_error);
        write_stdout(&out);
        return Ok(0);
    }
    for file in &files {
        let input = std::fs::read(file)
            .map_err(|e| Die::Fatal(msg::could_not_read(file, &crate::global::strerror(&e))))?;
        let out = rgit_trailer::process(&input, &conf, &opts, &args, &mut on_error);
        if in_place.get() {
            write_in_place(Path::new(file), &out)?;
        } else {
            write_stdout(&out);
        }
    }
    Ok(0)
}

fn write_stdout(out: &[u8]) {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(out);
    let _ = stdout.flush();
}

/// Replace `file` through a temporary file next to it.
fn write_in_place(file: &Path, data: &[u8]) -> Result<(), Die> {
    let dir = file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let tmp = dir.join(format!(".rgit-trailers-{}", std::process::id()));
    let shown = tmp.display().to_string();
    let err = |e: std::io::Error| Die::Fatal(msg::could_not_write(&shown, &crate::global::strerror(&e)));
    let mut f = std::fs::File::create(&tmp).map_err(err)?;
    if let Err(e) = f.write_all(data).and_then(|_| f.flush()) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err(e));
    }
    drop(f);
    if let Ok(meta) = std::fs::metadata(file) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, file).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        Die::Fatal(msg::could_not_rename(
            &shown,
            &file.display().to_string(),
            &crate::global::strerror(&e),
        ))
    })
}
