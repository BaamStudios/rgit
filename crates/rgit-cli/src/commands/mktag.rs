// SPDX-License-Identifier: MIT
//! `git mktag` (Documentation/git-mktag.adoc): create a tag object after
//! checking it like `fsck --strict` (or the configured `fsck.<msg-id>`
//! severities with `--no-strict`).

use std::collections::HashMap;
use std::io::{Read, Write};

use rgit_object::fsck::{self, Control, MsgId, Severity};
use rgit_object::{ObjectKind, Tag};

use crate::messages::mktag as msg;
use crate::objects::Repo;
use crate::prelude::*;
use crate::repo::{self, Mode};

/// Every message id `fsck.<msg-id>` may name (`fsck-msgids(5)`), lower
/// case. Ids outside rgit-object's object checks are accepted and have no
/// effect here.
const KNOWN_IDS: &[&str] = &[
    "baddate", "baddateoverflow", "bademail", "badfilemode", "badgpgsig", "badheadtarget",
    "badheadercontinuation", "badname", "badobjectsha1", "badpackedrefentry",
    "badpackedrefheader", "badparentsha1", "badrefcontent", "badreffiletype", "badrefname",
    "badrefoid", "badreferentname", "badreftabletablename", "badtagname", "badtimezone",
    "badtree", "badtreesha1", "badtype", "duplicateentries", "emptyname",
    "emptypackedrefsfile", "extraheaderentry", "fullpathname", "gitattributesblob",
    "gitattributeslarge", "gitattributeslinelength", "gitattributesmissing",
    "gitattributessymlink", "gitignoresymlink", "gitmodulesblob", "gitmoduleslarge",
    "gitmodulesmissing", "gitmodulesname", "gitmodulesparse", "gitmodulespath",
    "gitmodulessymlink", "gitmodulesupdate", "gitmodulesurl", "hasdot", "hasdotdot",
    "hasdotgit", "largepathname", "mailmapsymlink", "missingauthor", "missingcommitter",
    "missingemail", "missingnamebeforeemail", "missingobject", "missingspacebeforedate",
    "missingspacebeforeemail", "missingtag", "missingtagentry", "missingtaggerentry",
    "missingtree", "missingtype", "missingtypeentry", "multipleauthors", "nulincommit",
    "nulinheader", "nullsha1", "packedrefentrynotterminated", "packedrefunsorted",
    "refmissingnewline", "symlinkref", "symreftargetisnotaref", "trailingrefcontent",
    "treenotsorted", "unknowntype", "unterminatedheader", "zeropaddeddate",
    "zeropaddedfilemode",
];

/// Ids whose findings cannot be made less severe than an error.
const FATAL_IDS: &[&str] = &["nulinheader", "unterminatedheader"];

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let strict = Cell::new(true);
    let mut parser = Parser::new(
        msg::MKTAG_USAGE,
        vec![Opt::flag(None, "strict", &strict, msg::MKTAG_HELP_STRICT)],
    );
    parser.parse(argv)?;
    drop(parser);
    let setup = repo::setup(ctx, Mode::Required)?;
    let mut repo = Repo::open(ctx, setup)?;
    let mut data = Vec::new();
    std::io::stdin()
        .lock()
        .read_to_end(&mut data)
        .map_err(|e| Die::Fatal(msg::could_not_read_from_stdin(&crate::global::strerror(&e))))?;

    let configured = fsck_config(&repo)?;
    let mut failed = false;
    // Verdict of the extra-header check, which runs last: like git, it
    // replaces the earlier verdicts when it is reached.
    let mut extra_header = None;
    {
        let mut err = std::io::stderr().lock();
        fsck::check_tag(repo.hash, &data, &mut |f| {
            if f.id == MsgId::ExtraHeaderEntry {
                extra_header = Some(false);
            }
            let level = match configured.get(&f.id) {
                Some(Severity::Ignore) => return Control::Continue,
                Some(s) => *s,
                None => match f.id.default_severity() {
                    Severity::Fatal | Severity::Error => Severity::Error,
                    _ => Severity::Warn,
                },
            };
            let error = level == Severity::Error || level == Severity::Fatal || strict.get();
            let text = msg::tag_fails_fsck(&f.to_string());
            if error {
                failed = true;
                if f.id == MsgId::ExtraHeaderEntry {
                    extra_header = Some(true);
                }
                let _ = writeln!(err, "error: {text}");
            } else {
                let _ = writeln!(err, "warning: {text}");
            }
            Control::Continue
        });
    }
    if extra_header.unwrap_or(failed) {
        return Err(Die::Fatal(msg::MKTAG_FAILED_FSCK.into()));
    }

    // The checks passed, so the object and type lines are well-formed.
    let tag = Tag::parse(repo.hash, &data).map_err(|e| Die::Fatal(e.to_string()))?;
    let target = tag.object().map_err(|e| Die::Fatal(e.to_string()))?;
    let tagged = tag.target_kind().map_err(|e| Die::Fatal(e.to_string()))?;
    let real = repo.replaced(&target);
    let actual = match repo.odb.header(&real) {
        Ok(Some(h)) => h.kind,
        _ => return Err(Die::Fatal(msg::could_not_read_tagged(&target.to_hex()))),
    };
    if actual != tagged {
        return Err(Die::Fatal(msg::tagged_type_mismatch(
            &target.to_hex(),
            tagged.name(),
            actual.name(),
        )));
    }
    let oid = repo
        .odb
        .write(ObjectKind::Tag, &data)
        .map_err(|e| Die::Fatal(e.to_string()))?;
    println!("{oid}");
    Ok(0)
}

/// The `fsck.<msg-id>` severities, validated like git does.
fn fsck_config(repo: &Repo) -> Result<HashMap<MsgId, Severity>, Die> {
    let mut out = HashMap::new();
    for entry in repo.config.entries() {
        let name = String::from_utf8_lossy(&entry.name).into_owned();
        let Some(key) = name.strip_prefix("fsck.") else {
            continue;
        };
        if key.contains('.') {
            continue;
        }
        let value = match &entry.value {
            Some(v) => String::from_utf8_lossy(v).into_owned(),
            None => {
                eprintln!("error: {}", msg::missing_config_value(&name));
                return Err(Die::Fatal(msg::unable_to_parse_config(&name)));
            }
        };
        if key == "skiplist" {
            if std::fs::File::open(&value).is_err() {
                return Err(Die::Fatal(msg::could_not_open_skiplist(&value)));
            }
            continue;
        }
        if !KNOWN_IDS.contains(&key) {
            return Err(Die::Fatal(msg::unhandled_fsck_id(key)));
        }
        let sev = match value.as_str() {
            "error" => Severity::Error,
            "warn" => Severity::Warn,
            "ignore" => Severity::Ignore,
            _ => return Err(Die::Fatal(msg::unknown_fsck_severity(&value))),
        };
        if FATAL_IDS.contains(&key) && sev != Severity::Error {
            return Err(Die::Fatal(msg::cannot_demote(key, &value)));
        }
        if let Some(id) = MsgId::from_name(key) {
            out.insert(id, sev);
        }
    }
    Ok(out)
}
