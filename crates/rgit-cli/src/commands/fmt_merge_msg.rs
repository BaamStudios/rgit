// SPDX-License-Identifier: MIT
//! `git fmt-merge-msg` (Documentation/git-fmt-merge-msg.adoc): the default
//! merge commit message for the heads listed in `FETCH_HEAD` format
//! (`<id>\t[not-for-merge]\t<description>`) on standard input or in
//! `-F <file>`, with `-m`, `--into-name`, `--log[=<n>]`/`--no-log` and
//! `merge.log`/`merge.summary`. The message itself is built like the one
//! of `git merge` (`merge/message.rs`).

use std::io::{Read as _, Write as _};

use rgit_hash::ObjectId;
use rgit_object::ObjectKind;

use super::merge::message::{self, Head, Kind};
use crate::messages::fmt_merge_msg as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;

/// `--log` without a number (and `merge.log=true`).
const DEFAULT_LOG: usize = 20;

/// An integer with an optional `k`, `m` or `g` suffix.
fn parse_int(v: &str) -> Option<i64> {
    let (digits, factor) = match v.as_bytes().last() {
        Some(b'k' | b'K') => (&v[..v.len() - 1], 1024),
        Some(b'm' | b'M') => (&v[..v.len() - 1], 1024 * 1024),
        Some(b'g' | b'G') => (&v[..v.len() - 1], 1024 * 1024 * 1024),
        _ => (v, 1),
    };
    digits.parse::<i64>().ok().map(|n| n * factor)
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let log = Cell::new(-1i64);
    let message = RefCell::new(None::<String>);
    let into_name = RefCell::new(None::<String>);
    let file = RefCell::new(None::<String>);
    let log_cb = |v: Option<&str>, unset: bool| {
        let n = match (v, unset) {
            (_, true) => 0,
            (None, false) => DEFAULT_LOG as i64,
            (Some(v), false) => parse_int(v)
                .ok_or_else(|| CallbackError::Error(msg::LOG_NOT_A_NUMBER.to_owned()))?,
        };
        log.set(n);
        Ok(())
    };
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::callback(None, "log", Some("n"), msg::HELP_LOG, log_cb).optional(None),
            Opt::callback(None, "summary", Some("n"), msg::HELP_SUMMARY, log_cb)
                .optional(None)
                .hidden(),
            Opt::string('m', "message", &message, "text", msg::HELP_MESSAGE),
            Opt::string(None, "into-name", &into_name, "name", msg::HELP_INTO_NAME),
            Opt::string('F', "file", &file, "file", msg::HELP_FILE),
        ],
    );
    let rest = parser.parse(argv)?.args;
    if !rest.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);

    let mut repo = Repo::open(ctx, Mode::Required)?;
    let input = match file.into_inner() {
        Some(f) => {
            let path = repo.setup.original_cwd.join(&f);
            std::fs::read(&path).map_err(|e| {
                Die::Fatal(msg::cannot_open(&f, &crate::global::strerror(&e)))
            })?
        }
        None => {
            let mut v = Vec::new();
            std::io::stdin()
                .read_to_end(&mut v)
                .map_err(|e| Die::Fatal(e.to_string()))?;
            v
        }
    };

    let log = match log.get() {
        -1 => config_log(&repo),
        n => usize::try_from(n).unwrap_or(0),
    };

    let mut heads = Vec::new();
    for (n, line) in input.split(|&b| b == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, |&b| b == b'\t');
        let (Some(id), Some(flag), Some(desc)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(Die::Fatal(msg::error_in_line(
                n + 1,
                &String::from_utf8_lossy(line),
            )));
        };
        let Ok(id) = ObjectId::from_hex(repo.hash, id) else {
            return Err(Die::Fatal(msg::error_in_line(
                n + 1,
                &String::from_utf8_lossy(line),
            )));
        };
        if !flag.is_empty() {
            continue;
        }
        let Some(commit) = repo.peel_to(id, ObjectKind::Commit) else {
            continue;
        };
        heads.push(Head {
            arg: id.to_hex(),
            name: String::from_utf8_lossy(desc).into_owned(),
            kind: Kind::Fetch,
            commit,
            tag_message: match repo.read_object(&id) {
                Some(rgit_object::Object::Tag(t)) => Some(t.message().to_vec()),
                _ => None,
            },
            signed_tag: message::signed_tag(&repo, id, commit),
        });
    }
    if heads.is_empty() {
        return Ok(0);
    }
    let head = repo.resolve("HEAD");
    let head = head.and_then(|h| repo.peel_to(h, ObjectKind::Commit));
    // Heads that are proper ancestors of HEAD are merged already and left
    // out (checked against the `git` binary); with none left there is no
    // message.
    if let Some(ours) = &head {
        heads.retain(|h| {
            h.commit == *ours || !message::new_commits(&repo, &h.commit, Some(ours)).is_empty()
        });
    }
    let out = match message.into_inner() {
        Some(m) => {
            let mut out = m.into_bytes();
            if !out.ends_with(b"\n") {
                out.push(b'\n');
            }
            // With -m, git appends the tag message without the
            // separating empty line.
            let tags = message::tag_part(&repo, &heads);
            out.extend_from_slice(tags.strip_prefix(b"\n").unwrap_or(&tags));
            out.extend_from_slice(&message::log_part(&repo, &heads, head.as_ref(), log));
            out
        }
        None if heads.is_empty() => Vec::new(),
        None => {
            let into = match into_name.into_inner() {
                Some(n) => n,
                None => match repo.refs.head_target() {
                    Some(b) => b.strip_prefix("refs/heads/").unwrap_or(&b).to_owned(),
                    None => "HEAD".to_owned(),
                },
            };
            message::default_message(&repo, &heads, &into, head.as_ref(), log)
        }
    };
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(&out);
    let _ = o.flush();
    Ok(0)
}

/// `merge.log`, else `merge.summary`: a number, or a boolean (true means
/// 20).
fn config_log(repo: &Repo) -> usize {
    for key in ["merge.log", "merge.summary"] {
        let Some(e) = repo.config.get(key) else {
            continue;
        };
        return match e.value.as_ref() {
            None => DEFAULT_LOG,
            Some(v) => match String::from_utf8_lossy(v).trim().parse::<i64>() {
                Ok(n) => usize::try_from(n).unwrap_or(0),
                Err(_) => match rgit_config::value::parse_bool(Some(v)) {
                    Some(true) => DEFAULT_LOG,
                    _ => 0,
                },
            },
        };
    }
    0
}
