// SPDX-License-Identifier: MIT
//! `git refs verify [--strict] [--verbose]`: consistency checks of the
//! files backend (Documentation/git-refs.adoc, fsck-msgids.adoc).
//!
//! Checked: the loose references below `refs/` of the common directory and
//! of every linked worktree (names, content, symbolic references and their
//! targets, symbolic links, file types), the root references (`HEAD`,
//! `ORIG_HEAD`, ...) of each worktree, and `packed-refs` (file type,
//! header, entries, peeled lines, order with the `sorted` trait). Findings
//! are reported as `<level>: <ref>: <msg-id>: <detail>` with the levels of
//! `fsck-msgids` and `fsck.<msg-id>`; any error makes the command exit
//! with 255. `--strict` raises warnings to errors, which concerns none of
//! these checks (their defaults are errors or informational).
//!
//! Entries are visited in name order (git follows the directory order).

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use rgit_hash::{HashKind, ObjectId};
use rgit_object::fsck::Severity;
use rgit_refs::name::{
    RefNameFlags, check_refname_format, is_pseudo_ref, is_root_ref, is_root_ref_syntax,
};

use crate::messages::refs as msg;
use crate::prelude::*;
use crate::repo::{self, Mode};

/// The message ids of the reference checks.
#[derive(Clone, Copy)]
enum Id {
    BadRefName,
    BadRefContent,
    RefMissingNewline,
    TrailingRefContent,
    BadReferentName,
    SymrefTargetIsNotARef,
    SymlinkRef,
    BadRefFiletype,
    EmptyPackedRefsFile,
    BadPackedRefHeader,
    BadPackedRefEntry,
    PackedRefEntryNotTerminated,
    PackedRefUnsorted,
}

impl Id {
    fn name(self) -> &'static str {
        match self {
            Id::BadRefName => "badRefName",
            Id::BadRefContent => "badRefContent",
            Id::RefMissingNewline => "refMissingNewline",
            Id::TrailingRefContent => "trailingRefContent",
            Id::BadReferentName => "badReferentName",
            Id::SymrefTargetIsNotARef => "symrefTargetIsNotARef",
            Id::SymlinkRef => "symlinkRef",
            Id::BadRefFiletype => "badRefFiletype",
            Id::EmptyPackedRefsFile => "emptyPackedRefsFile",
            Id::BadPackedRefHeader => "badPackedRefHeader",
            Id::BadPackedRefEntry => "badPackedRefEntry",
            Id::PackedRefEntryNotTerminated => "packedRefEntryNotTerminated",
            Id::PackedRefUnsorted => "packedRefUnsorted",
        }
    }

    /// Informational by default (reported as a warning).
    fn info(self) -> bool {
        matches!(
            self,
            Id::RefMissingNewline
                | Id::TrailingRefContent
                | Id::SymrefTargetIsNotARef
                | Id::SymlinkRef
                | Id::EmptyPackedRefsFile
        )
    }
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let verbose = Cell::new(false);
    let strict = Cell::new(false);
    let mut parser = Parser::new(
        &[msg::USAGE_VERIFY],
        vec![
            Opt::flag(None, "verbose", &verbose, msg::HELP_VERBOSE),
            Opt::flag(None, "strict", &strict, msg::HELP_STRICT),
        ],
    );
    let args = parser.parse(argv)?.args;
    if !args.is_empty() {
        eprintln!("usage: {}", msg::VERIFY_NO_ARGUMENTS);
        return Err(Die::Exit(129));
    }
    let setup = repo::setup(ctx, Mode::Required)?;
    let config = crate::refs_support::load_config(ctx, &setup)?;
    let levels = crate::commands::mktag::fsck_levels(&config)?;
    let common = setup.common_dir.clone();
    let mut c = Checker {
        levels,
        hash: setup.format.object_format,
        verbose: verbose.get(),
        failed: false,
        own: setup
            .git_dir
            .strip_prefix(&common)
            .ok()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| format!("{}/", p.display())),
    };

    c.say(msg::CHECKING_CONSISTENCY);
    c.loose_refs(&common, "", "refs");
    c.root_refs(&common, "");
    let shown = match &setup.work_tree {
        Some(w) if common == w.join(".git") => PathBuf::from(".git").join("packed-refs"),
        _ => common.join("packed-refs"),
    };
    c.say(&msg::checking_packed_refs(&shown.display().to_string()));
    c.packed_refs(&common.join("packed-refs"));
    for id in crate::worktrees::linked_ids(&common) {
        let prefix = format!("worktrees/{id}/");
        let dir = common.join("worktrees").join(&id);
        c.say(msg::CHECKING_CONSISTENCY);
        c.loose_refs(&dir, &prefix, "refs");
        c.root_refs(&dir, &prefix);
    }
    if c.failed { Err(Die::Exit(255)) } else { Ok(0) }
}

struct Checker {
    /// `fsck.<msg-id>` by lower-case id.
    levels: HashMap<String, Severity>,
    hash: HashKind,
    verbose: bool,
    /// An error was reported.
    failed: bool,
    /// In a linked worktree: its directory below the common directory
    /// (`worktrees/<id>/`); symbolic links into it are shown relative to
    /// it.
    own: Option<String>,
}

/// The entries of `dir`, by name.
fn sorted_entries(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, PathBuf)> = rd
        .filter_map(Result::ok)
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    out.sort();
    out
}

/// `refs/heads/x` + `../../logs/y` = `logs/y`, without touching the disk.
fn normalize(base: &str, target: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let joined = PathBuf::from(base).join(target);
    for comp in joined.components() {
        match comp {
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(p) => parts.push(p.to_string_lossy().into_owned()),
        }
    }
    parts.join("/")
}

impl Checker {
    fn say(&self, text: &str) {
        if self.verbose {
            eprintln!("{text}");
        }
    }

    fn report(&mut self, at: &str, id: Id, detail: &str) {
        let error = match self.levels.get(&id.name().to_ascii_lowercase()) {
            Some(Severity::Ignore) => return,
            Some(Severity::Error | Severity::Fatal) => true,
            Some(_) => false,
            None => !id.info(),
        };
        if error {
            self.failed = true;
            eprintln!("error: {at}: {}: {detail}", id.name());
        } else {
            eprintln!("warning: {at}: {}: {detail}", id.name());
        }
    }

    /// The loose references below `<gitdir>/<rel>`, shown as
    /// `<prefix><rel>/...`.
    fn loose_refs(&mut self, gitdir: &Path, prefix: &str, rel: &str) {
        for (file, path) in sorted_entries(&gitdir.join(rel)) {
            let name = format!("{rel}/{file}");
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                self.loose_refs(gitdir, prefix, &name);
                continue;
            }
            // Lock files of valid references are someone's update.
            if let Some(locked) = name.strip_suffix(".lock") {
                if check_refname_format(locked.as_bytes(), RefNameFlags::STRICT).is_ok() {
                    continue;
                }
            }
            let shown = format!("{prefix}{name}");
            self.say(&msg::checking(&shown));
            if check_refname_format(name.as_bytes(), RefNameFlags::STRICT).is_err() {
                self.report(&shown, Id::BadRefName, msg::INVALID_REFNAME_FORMAT);
            }
            self.ref_file(&path, &meta, &shown);
        }
    }

    /// The root references directly in `gitdir`.
    fn root_refs(&mut self, gitdir: &Path, prefix: &str) {
        for (file, path) in sorted_entries(gitdir) {
            if !is_root_ref(&file) || is_pseudo_ref(&file) {
                continue;
            }
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                continue;
            }
            let shown = format!("{prefix}{file}");
            self.say(&msg::checking(&shown));
            self.ref_file(&path, &meta, &shown);
        }
    }

    /// One reference file; `shown` is its name relative to the common
    /// directory.
    fn ref_file(&mut self, path: &Path, meta: &std::fs::Metadata, shown: &str) {
        if meta.file_type().is_symlink() {
            self.report(shown, Id::SymlinkRef, msg::SYMLINK_REF);
            let Ok(target) = std::fs::read_link(path) else {
                return;
            };
            let dir = shown.rsplit_once('/').map_or("", |(d, _)| d);
            let referent = normalize(dir, &target.to_string_lossy());
            let referent = match &self.own {
                Some(own) => referent.strip_prefix(own.as_str()).unwrap_or(&referent).to_owned(),
                None => referent,
            };
            self.referent(shown, &referent);
            return;
        }
        if !meta.is_file() {
            self.report(shown, Id::BadRefFiletype, msg::UNEXPECTED_FILE_TYPE);
            return;
        }
        let Ok(data) = std::fs::read(path) else {
            return;
        };
        let text = String::from_utf8_lossy(&data).into_owned();
        if let Some(rest) = text.strip_prefix("ref:") {
            let rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == '\x0b');
            let end = rest
                .find(|c: char| c.is_ascii_whitespace() || c == '\x0b')
                .unwrap_or(rest.len());
            let (referent, tail) = rest.split_at(end);
            if !tail.ends_with('\n') {
                self.report(shown, Id::RefMissingNewline, msg::MISSES_LF);
            }
            if !tail.is_empty() && tail != "\n" {
                self.report(shown, Id::TrailingRefContent, msg::TRAILING_WHITESPACE);
            }
            self.referent(shown, referent);
            return;
        }
        let hex = self.hash.hex_len();
        let oid_ok = text.len() >= hex
            && text.is_char_boundary(hex)
            && ObjectId::from_hex(self.hash, &text.as_bytes()[..hex]).is_ok();
        let rest = if oid_ok { &text[hex..] } else { "" };
        if !oid_ok || rest.starts_with(|c: char| !c.is_ascii_whitespace()) {
            let shown_content = text.strip_suffix('\n').unwrap_or(&text);
            self.report(shown, Id::BadRefContent, shown_content);
        } else if rest.is_empty() {
            self.report(shown, Id::RefMissingNewline, msg::MISSES_LF);
        } else if rest != "\n" {
            self.report(shown, Id::TrailingRefContent, &msg::trailing_garbage(rest));
        }
    }

    /// The target of a symbolic reference.
    fn referent(&mut self, shown: &str, referent: &str) {
        let valid = if referent.contains('/') {
            check_refname_format(referent.as_bytes(), RefNameFlags::STRICT).is_ok()
        } else {
            is_root_ref_syntax(referent) && !is_pseudo_ref(referent)
        };
        if !valid {
            self.report(shown, Id::BadReferentName, &msg::invalid_referent(referent));
            return;
        }
        if referent.contains('/')
            && !referent.starts_with("refs/")
            && !referent.starts_with("worktrees/")
        {
            self.report(shown, Id::SymrefTargetIsNotARef, &msg::non_ref_target(referent));
        }
    }

    /// `packed-refs`.
    fn packed_refs(&mut self, path: &Path) {
        const AT: &str = "packed-refs";
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return;
        };
        if meta.file_type().is_symlink() {
            self.report(AT, Id::BadRefFiletype, msg::NOT_REGULAR_SYMLINK);
            return;
        }
        if !meta.is_file() {
            self.report(AT, Id::BadRefFiletype, msg::NOT_REGULAR);
            return;
        }
        let Ok(data) = std::fs::read(path) else {
            return;
        };
        if data.is_empty() {
            self.report(AT, Id::EmptyPackedRefsFile, msg::FILE_IS_EMPTY);
            return;
        }
        let text = String::from_utf8_lossy(&data).into_owned();
        let mut lines: Vec<(&str, bool)> = Vec::new();
        let mut rest = text.as_str();
        while !rest.is_empty() {
            match rest.split_once('\n') {
                Some((line, more)) => {
                    lines.push((line, true));
                    rest = more;
                }
                None => {
                    lines.push((rest, false));
                    rest = "";
                }
            }
        }
        let mut sorted = false;
        let mut start = 0;
        if let Some((first, _)) = lines.first() {
            if first.starts_with('#') {
                start = 1;
                match first.strip_prefix("# pack-refs with: ") {
                    Some(traits) => sorted = traits.split_whitespace().any(|t| t == "sorted"),
                    None => self.report(
                        "packed-refs.header",
                        Id::BadPackedRefHeader,
                        &msg::bad_packed_header(first),
                    ),
                }
            }
        }
        let hash = self.hash;
        let hex = hash.hex_len();
        let parse_oid = |s: &str| {
            s.len() >= hex
                && s.is_char_boundary(hex)
                && ObjectId::from_hex(hash, &s.as_bytes()[..hex]).is_ok()
        };
        // A peeled line may follow a reference line.
        let mut after_ref = false;
        let mut previous: Option<String> = None;
        for (i, (line, terminated)) in lines.iter().enumerate().skip(start) {
            let at = msg::packed_refs_line(i + 1);
            if !terminated {
                self.report(&at, Id::PackedRefEntryNotTerminated, &msg::packed_not_terminated(line));
                after_ref = false;
                continue;
            }
            if let Some(peeled) = line.strip_prefix('^').filter(|_| after_ref) {
                after_ref = false;
                if !parse_oid(peeled) {
                    self.report(&at, Id::BadPackedRefEntry, &msg::packed_invalid_peeled(peeled));
                } else if peeled.len() > hex {
                    self.report(&at, Id::BadPackedRefEntry, &msg::packed_peeled_garbage(&peeled[hex..]));
                }
                continue;
            }
            after_ref = false;
            if !parse_oid(line) {
                self.report(&at, Id::BadPackedRefEntry, &msg::packed_invalid_oid(line));
                continue;
            }
            let (oid, rest) = line.split_at(hex);
            let Some(name) = rest.strip_prefix(' ') else {
                self.report(&at, Id::BadPackedRefEntry, &msg::packed_no_space(oid, rest));
                continue;
            };
            after_ref = true;
            if check_refname_format(name.as_bytes(), RefNameFlags::STRICT).is_err() {
                self.report(&at, Id::BadRefName, &msg::packed_bad_refname(name));
            }
            if sorted {
                if let Some(prev) = &previous {
                    if name.as_bytes() < prev.as_bytes() {
                        self.report(&at, Id::PackedRefUnsorted, &msg::packed_unsorted(name, prev));
                    }
                }
            }
            previous = Some(name.to_owned());
        }
    }
}
