// SPDX-License-Identifier: MIT
//! `git merge-file` (Documentation/git-merge-file.adoc).
//!
//! The merge itself is [`rgit_diff::merge`]. Not supported yet:
//! `--diff-algorithm=patience` and `histogram` (#53).

use std::io::Write;

use rgit_diff::DiffOptions;
use rgit_diff::merge::{self, ConflictStyle, Favor, MergeOptions};
use rgit_object::ObjectKind;

use crate::messages::merge_file as msg;
use crate::prelude::*;
use crate::repo::{self, Mode};
use crate::repo_objects::Repo;
use crate::revision::Resolver;

const FAVOR_OURS: i32 = 1;
const FAVOR_THEIRS: i32 = 2;
const FAVOR_UNION: i32 = 3;
const STYLE_DIFF3: i32 = 1;
const STYLE_ZDIFF3: i32 = 2;

/// Exit code for errors (git returns -1).
const EXIT_ERROR: i32 = 255;
/// Highest exit code for the number of conflicts.
const MAX_CONFLICTS_EXIT: usize = 127;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let setup = repo::setup_gently(ctx)?;
    let prefix = setup
        .as_ref()
        .and_then(|s| s.prefix.as_ref())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let config = crate::repo_objects::load_config(ctx, setup.as_ref())?;
    let style = Cell::new(match config_style(&config)? {
        ConflictStyle::Merge => 0,
        ConflictStyle::Diff3 => STYLE_DIFF3,
        ConflictStyle::ZealousDiff3 => STYLE_ZDIFF3,
    });
    let stdout = Cell::new(false);
    let object_id = Cell::new(false);
    let favor = Cell::new(0);
    let quiet = Cell::new(false);
    let marker_size = Cell::new(0);
    let algorithm = Cell::new((rgit_diff::Algorithm::Myers, false));
    let labels = RefCell::new(Vec::<String>::new());
    let mut parser = Parser::new(
        msg::MERGE_FILE_USAGE,
        vec![
            Opt::flag('p', "stdout", &stdout, msg::HELP_STDOUT),
            Opt::flag(None, "object-id", &object_id, msg::HELP_OBJECT_ID),
            Opt::set_int(None, "diff3", &style, msg::HELP_DIFF3, STYLE_DIFF3),
            Opt::set_int(None, "zdiff3", &style, msg::HELP_ZDIFF3, STYLE_ZDIFF3),
            Opt::set_int(None, "ours", &favor, msg::HELP_OURS, FAVOR_OURS),
            Opt::set_int(None, "theirs", &favor, msg::HELP_THEIRS, FAVOR_THEIRS),
            Opt::set_int(None, "union", &favor, msg::HELP_UNION, FAVOR_UNION),
            Opt::callback(
                None,
                "diff-algorithm",
                Some("algorithm"),
                msg::HELP_DIFF_ALGORITHM,
                |v, _| {
                    match rgit_diff::Algorithm::parse(v.unwrap_or("")) {
                        Some(a) => algorithm.set(a),
                        None => return Err(CallbackError::Error(msg::BAD_DIFF_ALGORITHM.into())),
                    }
                    Ok(())
                },
            )
            .noneg(),
            Opt::integer(None, "marker-size", &marker_size, msg::HELP_MARKER_SIZE),
            Opt::flag('q', "quiet", &quiet, msg::HELP_QUIET),
            Opt::callback('L', None, Some("name"), msg::HELP_LABEL, |v, _| {
                let mut l = labels.borrow_mut();
                if l.len() >= 3 {
                    return Err(CallbackError::Error(msg::TOO_MANY_LABELS.into()));
                }
                l.push(v.unwrap_or("").to_string());
                Ok(())
            }),
        ],
    );
    let args = parser.parse(argv)?.args;
    if args.len() != 3 {
        return Err(parser.usage_error().into());
    }
    drop(parser);

    let mut repo = if object_id.get() {
        let setup = match setup {
            Some(s) => s,
            // Reports that there is no repository.
            None => repo::setup(ctx, Mode::Required)?,
        };
        Some(Repo::open(ctx, setup)?)
    } else {
        None
    };

    let mut contents = Vec::with_capacity(3);
    for name in &args {
        let data = match repo.as_mut() {
            Some(r) => read_object(r, name)?,
            None => read_file(&prefixed(&prefix, name))?,
        };
        if rgit_diff::is_binary(&data) {
            if !quiet.get() {
                crate::messages::print_error(&msg::cannot_merge_binary(name));
            }
            return Err(Die::Exit(EXIT_ERROR));
        }
        contents.push(data);
    }

    let labels = labels.into_inner();
    let label = |i: usize| labels.get(i).unwrap_or(&args[i]).as_bytes();
    let opts = MergeOptions {
        style: match style.get() {
            STYLE_DIFF3 => ConflictStyle::Diff3,
            STYLE_ZDIFF3 => ConflictStyle::ZealousDiff3,
            _ => ConflictStyle::Merge,
        },
        favor: match favor.get() {
            FAVOR_OURS => Favor::Ours,
            FAVOR_THEIRS => Favor::Theirs,
            FAVOR_UNION => Favor::Union,
            _ => Favor::None,
        },
        marker_size: usize::try_from(marker_size.get()).unwrap_or(0),
        ours_label: label(0),
        base_label: label(1),
        theirs_label: label(2),
        diff: DiffOptions {
            algorithm: algorithm.get().0,
            minimal: algorithm.get().1,
            ..merge::merge_diff_options()
        },
    };
    let result = merge::merge(&contents[1], &contents[0], &contents[2], &opts)
        .map_err(|_| Die::Exit(EXIT_ERROR))?;

    if stdout.get() {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(&result.content);
        let _ = out.flush();
    } else if let Some(r) = repo.as_ref() {
        let oid = r
            .odb
            .write(ObjectKind::Blob, &result.content)
            .map_err(|_| Die::Error(msg::unable_to_write_object(), EXIT_ERROR))?;
        println!("{oid}");
    } else {
        let path = prefixed(&prefix, &args[0]);
        std::fs::write(&path, &result.content).map_err(|e| {
            Die::Error(
                msg::could_not_open_for_writing(&args[0], &crate::global::strerror(&e)),
                EXIT_ERROR,
            )
        })?;
    }
    Ok(result.conflicts.min(MAX_CONFLICTS_EXIT) as i32)
}

/// `merge.conflictStyle`; an invalid value ends the command.
pub(crate) fn config_style(config: &rgit_config::ConfigSet) -> Result<ConflictStyle, Die> {
    let mut style = ConflictStyle::Merge;
    for e in config.get_all("merge.conflictstyle") {
        let value = match e.string() {
            Ok(v) => v.to_string(),
            Err(err) => {
                crate::messages::print_error(&err.to_string());
                return Err(Die::Fatal(crate::revisions::unable_to_parse(
                    "merge.conflictstyle",
                    e,
                )));
            }
        };
        style = match ConflictStyle::from_name(&value) {
            Some(s) => s,
            None => {
                crate::messages::print_error(&msg::unknown_conflict_style(&value));
                return Err(Die::Fatal(crate::revisions::unable_to_parse(
                    "merge.conflictstyle",
                    e,
                )));
            }
        };
    }
    Ok(style)
}

fn prefixed(prefix: &str, path: &str) -> String {
    if prefix.is_empty() || std::path::Path::new(path).is_absolute() {
        path.to_string()
    } else {
        format!("{prefix}{path}")
    }
}

fn read_file(path: &str) -> Result<Vec<u8>, Die> {
    let err = |f: fn(&str, &str) -> String, e: &std::io::Error| {
        Die::Error(f(path, &crate::global::strerror(e)), EXIT_ERROR)
    };
    std::fs::metadata(path).map_err(|e| err(msg::could_not_stat, &e))?;
    std::fs::read(path).map_err(|e| err(msg::could_not_open, &e))
}

fn read_object(repo: &mut Repo, name: &str) -> Result<Vec<u8>, Die> {
    let oid = Resolver::new(repo)
        .resolve(name)
        .map_err(|_| Die::Error(msg::object_does_not_exist(name), EXIT_ERROR))?
        .oid;
    let real = repo.replaced(&oid);
    match repo.odb.read(&real) {
        Ok(Some(raw)) if raw.kind == ObjectKind::Blob => Ok(raw.data),
        // git reads the empty blob even when it is not stored.
        Ok(None) if real == repo.hash.empty_blob() => Ok(Vec::new()),
        _ => Err(Die::Fatal(msg::unable_to_read_blob(&oid.to_string()))),
    }
}
