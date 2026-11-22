// SPDX-License-Identifier: MIT
//! `git format-patch` (Documentation/git-format-patch.adoc,
//! Documentation/config/format.adoc): every non-merge commit of a range
//! as a mail in UNIX mailbox format, written to numbered files or to
//! standard output, with an optional cover letter.
//!
//! Mail headers are written with `rgit_mail` (RFC 2047 encoding, quoting,
//! folding). Not yet supported: `--attach`/`--inline`, `--base`,
//! `--notes`, `--ignore-if-in-upstream`, `--interdiff`/`--range-diff`,
//! `--commit-list-format` and binary patches.

#[path = "format_patch/mail.rs"]
mod mail;
#[path = "format_patch/options.rs"]
mod options;

use std::io::Write as _;
use std::path::PathBuf;

use rgit_config::{ConfigSet, SystemEnv};
use rgit_date::{DateContext, DateMode, DateStyle};
use rgit_diff::diffcore::Detect;
use rgit_hash::ObjectId;
use rgit_ident::ident::{IdentConfig, Resolver as IdentResolver, Role, Strictness};
use rgit_object::{Commit, Object, SignatureRef};

use crate::diff::collect::TreeDiff;
use crate::diff::options::Formats;
use crate::diff::output::Output;
use crate::messages::format_patch as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;
use options::{HeaderOp, Options, Thread};

const CHARSET: &str = "UTF-8";
const MIME_HEADERS: &[u8] =
    b"MIME-Version: 1.0\nContent-Type: text/plain; charset=UTF-8\nContent-Transfer-Encoding: 8bit\n";

/// A name and address.
#[derive(Clone, PartialEq, Eq)]
struct Person {
    name: Vec<u8>,
    email: Vec<u8>,
}

impl Person {
    fn parse(ident: &str) -> Result<Person, Die> {
        let sig = SignatureRef::parse(ident.as_bytes())
            .filter(|s| s.time_raw.trim_ascii().is_empty())
            .ok_or_else(|| Die::Fatal(msg::invalid_ident(ident)))?;
        Ok(Person {
            name: sig.name.to_vec(),
            email: sig.email.to_vec(),
        })
    }

    fn plain(&self) -> Vec<u8> {
        let mut v = self.name.clone();
        v.extend_from_slice(b" <");
        v.extend_from_slice(&self.email);
        v.push(b'>');
        v
    }
}

/// The settings after combining configuration and command line.
struct Settings {
    numbered: Numbered,
    start: usize,
    keep_subject: bool,
    signoff: bool,
    prefix: String,
    reroll: Option<String>,
    suffix: String,
    filename_max: usize,
    numbered_files: bool,
    output_dir: Option<String>,
    cover_letter: CoverLetter,
    cover_mode: CoverMode,
    to: Vec<String>,
    cc: Vec<String>,
    extra: Vec<String>,
    from: Option<Person>,
    /// `from` was given as an identity, which has no date.
    from_ident: bool,
    force_in_body_from: bool,
    in_reply_to: Option<String>,
    thread: Option<Thread>,
    signature: Option<Vec<u8>>,
    encode: bool,
    zero_commit: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Numbered {
    Yes,
    No,
    Auto,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CoverLetter {
    Yes,
    No,
    Auto,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CoverMode {
    Message,
    Subject,
    Auto,
    None,
}

impl CoverMode {
    fn parse(s: &str) -> Result<CoverMode, Die> {
        Ok(match s {
            "message" | "default" => CoverMode::Message,
            "subject" => CoverMode::Subject,
            "auto" => CoverMode::Auto,
            "none" => CoverMode::None,
            _ => return Err(Die::Fatal(msg::invalid_cover_mode(s))),
        })
    }
}

fn config_err(e: rgit_config::ValueError) -> Die {
    Die::Fatal(e.to_string())
}

fn config_string(config: &ConfigSet, key: &str) -> Result<Option<String>, Die> {
    Ok(config
        .string(key)
        .map_err(config_err)?
        .map(|v| String::from_utf8_lossy(v).into_owned()))
}

/// A `bool` or one of `words`: `Ok(Err(word))` for a word.
fn bool_or_word<'w>(
    config: &ConfigSet,
    key: &str,
    words: &[&'w str],
) -> Result<Option<Result<bool, &'w str>>, Die> {
    let Some(entry) = config.get(key) else {
        return Ok(None);
    };
    if let Some(v) = &entry.value {
        if let Some(w) = words.iter().find(|w| v.eq_ignore_ascii_case(w.as_bytes())) {
            return Ok(Some(Err(w)));
        }
    }
    entry.bool().map(|b| Some(Ok(b))).map_err(config_err)
}

/// The committer of now.
fn committer(repo: &Repo) -> Result<(Person, u64, i32), Die> {
    let env = SystemEnv::default();
    let icfg = IdentConfig::from_config(&repo.config).map_err(|p| {
        p.print();
        Die::Exit(rgit_options::EXIT_FATAL)
    })?;
    let ident = IdentResolver::new(&icfg, &env)
        .ident(Role::Committer, Strictness::Strict)
        .map_err(|e| {
            eprint!("{}", e.stderr_text());
            Die::Exit(rgit_options::EXIT_FATAL)
        })?;
    Ok((
        Person {
            name: ident.name.to_vec(),
            email: ident.email.to_vec(),
        },
        ident.time,
        ident.tz,
    ))
}

fn tz_hhmm(offset_minutes: i32) -> i32 {
    let sign = if offset_minutes < 0 { -1 } else { 1 };
    let abs = offset_minutes.abs();
    sign * (abs / 60 * 100 + abs % 60)
}

fn rfc2822(time: u64, tz: i32) -> String {
    rgit_date::show_date(
        time,
        tz,
        &DateMode::new(DateStyle::Rfc2822),
        &DateContext::from_env(),
    )
    .unwrap_or_default()
}

/// Resolve the settings from `format.*` and the command line.
fn settings(repo: &Repo, o: &Options) -> Result<Settings, Die> {
    let config = &repo.config;
    let numbered = match o.numbered {
        Some(true) => Numbered::Yes,
        Some(false) => Numbered::No,
        None => match bool_or_word(config, "format.numbered", &["auto"])? {
            Some(Ok(true)) => Numbered::Yes,
            Some(Ok(false)) => Numbered::No,
            _ => Numbered::Auto,
        },
    };
    if o.keep_subject && o.numbered == Some(true) {
        return Err(Die::Fatal(msg::cannot_be_used_together("-n", "-k")));
    }
    if o.keep_subject
        && (o.subject_prefix.is_some() || o.rfc.as_deref().is_some_and(|r| !r.is_empty()))
    {
        return Err(Die::Fatal(msg::cannot_be_used_together(
            "--subject-prefix/--rfc",
            "-k",
        )));
    }
    if o.stdout && o.output_dir.is_some() {
        return Err(Die::Fatal(msg::cannot_be_used_together(
            "--stdout",
            "--output-directory",
        )));
    }
    if o.stdout && o.output.is_some() {
        return Err(Die::Fatal(msg::cannot_be_used_together(
            "--stdout", "--output",
        )));
    }
    if o.output.is_some() && o.output_dir.is_some() {
        return Err(Die::Fatal(msg::cannot_be_used_together(
            "--output",
            "--output-directory",
        )));
    }
    let base_prefix = match &o.subject_prefix {
        Some(p) => p.clone(),
        None => config_string(config, "format.subjectprefix")?.unwrap_or_else(|| "PATCH".into()),
    };
    let prefix = mail::subject_prefix(&base_prefix, o.rfc.as_deref(), o.reroll.as_deref());
    let suffix = match &o.suffix {
        Some(s) => s.clone(),
        None => config_string(config, "format.suffix")?.unwrap_or_else(|| ".patch".into()),
    };
    let filename_max = match o.filename_max {
        Some(n) => n,
        None => match config.int("format.filenamemaxlength").map_err(config_err)? {
            Some(n) => usize::try_from(n).unwrap_or(0),
            None => 64,
        },
    };
    let output_dir = if o.stdout || o.output.is_some() {
        None
    } else {
        match &o.output_dir {
            Some(d) => Some(d.clone()),
            None => config_string(config, "format.outputdirectory")?,
        }
    };
    let cover_letter = match o.cover_letter {
        Some(true) => CoverLetter::Yes,
        Some(false) => CoverLetter::No,
        None => match bool_or_word(config, "format.coverletter", &["auto"])? {
            Some(Ok(true)) => CoverLetter::Yes,
            Some(Err(_)) => CoverLetter::Auto,
            _ => CoverLetter::No,
        },
    };
    let cover_mode = match &o.cover_from_description {
        Some(m) => CoverMode::parse(m)?,
        None => match config_string(config, "format.coverfromdescription")? {
            Some(m) => CoverMode::parse(&m)?,
            None => CoverMode::Message,
        },
    };
    let signoff = match o.signoff {
        Some(s) => s,
        None => config
            .bool("format.signoff")
            .map_err(config_err)?
            .unwrap_or(false),
    };

    // Recipients and extra headers: configuration first, then the
    // command line in order.
    let mut to: Vec<String> = Vec::new();
    let mut cc: Vec<String> = Vec::new();
    let mut extra: Vec<String> = Vec::new();
    let add_header =
        |h: &str, to: &mut Vec<String>, cc: &mut Vec<String>, extra: &mut Vec<String>| {
            for line in h.split('\n').filter(|l| !l.is_empty()) {
                let lower = line.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("to:").map(|_| line[3..].trim()) {
                    to.push(v.to_string());
                } else if let Some(v) = lower.strip_prefix("cc:").map(|_| line[3..].trim()) {
                    cc.push(v.to_string());
                } else {
                    extra.push(line.to_string());
                }
            }
        };
    for e in config.get_all("format.headers") {
        let v = e.string().map_err(config_err)?;
        add_header(&String::from_utf8_lossy(v), &mut to, &mut cc, &mut extra);
    }
    for e in config.get_all("format.to") {
        to.push(String::from_utf8_lossy(e.string().map_err(config_err)?).into_owned());
    }
    for e in config.get_all("format.cc") {
        cc.push(String::from_utf8_lossy(e.string().map_err(config_err)?).into_owned());
    }
    for op in &o.headers {
        match op {
            HeaderOp::To(v) => to.push(v.clone()),
            HeaderOp::NoTo => to.clear(),
            HeaderOp::Cc(v) => cc.push(v.clone()),
            HeaderOp::NoCc => cc.clear(),
            HeaderOp::Add(v) => add_header(v, &mut to, &mut cc, &mut extra),
            HeaderOp::NoAdd => {
                to.clear();
                cc.clear();
                extra.clear();
            }
        }
    }

    let from_setting = match &o.from {
        Some(f) => Some(f.clone()),
        // A boolean, or else an identity.
        None => match config.get("format.from") {
            None => None,
            Some(e) => match e.bool() {
                Ok(true) => Some(None),
                Ok(false) => Some(Some(String::new())),
                Err(_) => Some(Some(
                    String::from_utf8_lossy(e.string().map_err(config_err)?).into_owned(),
                )),
            },
        },
    };
    let from_setting_kind = from_setting.clone();
    let from = match from_setting {
        None => None,
        Some(Some(s)) if s.is_empty() => None,
        Some(Some(s)) => Some(Person::parse(&s)?),
        Some(None) => Some(committer(repo)?.0),
    };
    let force_in_body_from = match o.force_in_body_from {
        Some(f) => f,
        None => config
            .bool("format.forceinbodyfrom")
            .map_err(config_err)?
            .unwrap_or(false),
    };
    let thread = match o.thread {
        Some(t) => t,
        None => match bool_or_word(config, "format.thread", &["shallow", "deep"])? {
            Some(Ok(true)) | Some(Err("shallow")) => Some(Thread::Shallow),
            Some(Err(_)) => Some(Thread::Deep),
            _ => None,
        },
    };
    let read_file = |path: &str, err: fn(&str) -> String| -> Result<Vec<u8>, Die> {
        std::fs::read(cwd_path(repo, path)).map_err(|_| Die::Fatal(err(path)))
    };
    let signature = match &o.signature {
        Some(None) => None,
        Some(Some(s)) => Some(s.as_bytes().to_vec()),
        None => match &o.signature_file {
            Some(f) => Some(read_file(f, msg::cannot_read_signature)?),
            None => match config_string(config, "format.signature")? {
                Some(s) => Some(s.into_bytes()),
                None => match config_string(config, "format.signaturefile")? {
                    Some(f) => Some(read_file(&f, msg::cannot_read_signature)?),
                    None => Some(rgit_version::version().into_bytes()),
                },
            },
        },
    }
    .filter(|s| !s.is_empty());
    let encode = match o.encode_headers {
        Some(e) => e,
        None => config
            .bool("format.encodeemailheaders")
            .map_err(config_err)?
            .unwrap_or(true),
    };
    Ok(Settings {
        numbered,
        start: o.start_number.unwrap_or(1),
        keep_subject: o.keep_subject,
        signoff,
        prefix,
        reroll: o.reroll.clone(),
        suffix,
        filename_max,
        numbered_files: o.numbered_files,
        output_dir,
        cover_letter,
        cover_mode,
        to,
        cc,
        extra,
        from_ident: matches!(&from_setting_kind, Some(Some(s)) if !s.is_empty()),
        from,
        force_in_body_from,
        in_reply_to: o.in_reply_to.as_deref().map(mail::angle),
        thread,
        signature,
        encode,
        zero_commit: o.zero_commit,
    })
}

/// A path given relative to the directory the command was started in.
fn cwd_path(repo: &Repo, path: &str) -> PathBuf {
    let p = std::path::Path::new(path);
    match &repo.setup.prefix {
        Some(prefix) if p.is_relative() => std::path::Path::new(prefix).join(p),
        _ => p.to_path_buf(),
    }
}

/// The revision arguments of the walk.
fn walk_args(o: &Options) -> Vec<String> {
    let mut args = vec!["--no-merges".to_string()];
    // With paths the limit applies to the commits that touch them.
    if let Some(n) = o.max_count.as_ref().filter(|_| o.paths.is_empty()) {
        args.push(format!("--max-count={n}"));
    }
    args.extend(o.rev_options.iter().cloned());
    args.extend(range(o));
    args
}

/// The revisions of the series: `<since>` alone means `<since>..HEAD`.
fn range(o: &Options) -> Vec<String> {
    let single_since = o.revs.len() == 1
        && o.max_count.is_none()
        && !o.root
        && !o.revs[0].contains("..")
        && !o.revs[0].starts_with('^');
    if single_since {
        vec![format!("^{}", o.revs[0]), "HEAD".into()]
    } else if o.revs.is_empty() {
        if o.max_count.is_some() {
            vec!["HEAD".into()]
        } else {
            Vec::new()
        }
    } else {
        o.revs.clone()
    }
}

/// `--ignore-if-in-upstream`: the other side of the range, the commits
/// reachable from the excluded revisions but not from the series.
fn upstream_args(o: &Options) -> Vec<String> {
    let mut args = vec!["--no-merges".to_string()];
    for r in range(o) {
        if let Some((a, b)) = r.split_once("...") {
            args.push(format!("{b}...{a}"));
        } else if let Some((a, b)) = r.split_once("..") {
            let b = if b.is_empty() { "HEAD" } else { b };
            let a = if a.is_empty() { "HEAD" } else { a };
            args.push(format!("{b}..{a}"));
        } else if let Some(x) = r.strip_prefix('^') {
            args.push(x.to_string());
        } else {
            args.push(format!("^{r}"));
        }
    }
    args
}

/// What makes two changes the same for `--ignore-if-in-upstream`: the
/// patch without object names, line numbers and whitespace.
fn patch_key(patch: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(patch.len());
    for line in patch.split(|&b| b == b'\n') {
        if line.starts_with(b"index ") {
            continue;
        }
        let line = if line.starts_with(b"@@ ") {
            &b"@@"[..]
        } else {
            line
        };
        key.extend(line.iter().filter(|b| !b.is_ascii_whitespace()));
        key.push(b'\n');
    }
    key
}

/// One commit to format.
struct Patch {
    id: ObjectId,
    commit: Commit,
    diff: Vec<u8>,
    has_stat: bool,
}

/// The branch whose description goes into the cover letter.
fn description_branch(repo: &mut Repo, o: &Options) -> Option<String> {
    let positive: Vec<String> = range(o)
        .into_iter()
        .filter(|r| !r.starts_with('^'))
        .map(|r| match r.split_once("..") {
            Some((_, right)) => right.trim_start_matches('.').to_string(),
            None => r,
        })
        .collect();
    if let [one] = &positive[..] {
        if !one.is_empty() && one != "HEAD" {
            return repo
                .resolve(&format!("refs/heads/{one}"))
                .map(|_| one.clone());
        }
    }
    let head = std::fs::read(repo.setup.git_dir.join("HEAD")).ok()?;
    head.strip_prefix(b"ref: refs/heads/")
        .map(|r| String::from_utf8_lossy(r.trim_ascii_end()).into_owned())
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    if argv.len() == 2 && matches!(argv[1].as_str(), "-h" | "--help-all") {
        let _ = std::io::stdout().write_all(msg::USAGE.as_bytes());
        return Ok(0);
    }
    let mut o = Options::parse(&argv[1..])?;
    let mut repo = Repo::open(ctx, Mode::Required)?;
    let s = settings(&repo, &o)?;
    if let Some(v) = repo.config.get("format.noprefix") {
        let on = v.bool().map_err(|e| {
            eprintln!("fatal: {e}");
            for hint in msg::NOPREFIX_HINT {
                eprintln!("hint: {hint}");
            }
            Die::Exit(rgit_options::EXIT_FATAL)
        })?;
        if on && !o.default_prefix {
            o.diff.no_prefix = true;
        }
    }

    // A lone <since> must name a commit.
    let since = &o.revs;
    if let [one] = &since[..] {
        if o.max_count.is_none() && !o.root && !one.contains("..") && !one.starts_with('^') {
            if repo.resolve(one).is_none() {
                return Err(Die::Fatal(crate::messages::rev_list::ambiguous_argument(
                    one,
                )));
            }
        }
    }

    // The commits, newest first.
    let ids = crate::history::walk_commits(ctx, &walk_args(&o))?;
    if ids.is_empty() {
        return Ok(0);
    }

    // Diff settings: stat, summary and patch unless the command line
    // chose formats; renames are detected unless switched off.
    let user_formats =
        o.diff.raw || o.diff.numstat || o.diff.stat || o.diff.shortstat || o.diff.summary;
    let formats = if o.no_stat {
        Formats {
            patch: true,
            ..Formats::default()
        }
    } else if user_formats {
        Formats {
            raw: o.diff.raw,
            numstat: o.diff.numstat,
            stat: o.diff.stat,
            shortstat: o.diff.shortstat,
            summary: o.diff.summary,
            patch: true,
            ..Formats::default()
        }
    } else {
        Formats {
            stat: true,
            summary: true,
            patch: true,
            ..Formats::default()
        }
    };
    if o.diff.detect.is_none() {
        let renames = match repo.config.get("diff.renames") {
            Some(e) => {
                let v = e.value.as_ref().map(|v| v.to_string()).unwrap_or_default();
                if matches!(v.to_ascii_lowercase().as_str(), "copy" | "copies") {
                    Detect::Copies
                } else if e.bool().map_err(config_err)? {
                    Detect::Renames
                } else {
                    Detect::Off
                }
            }
            None => Detect::Renames,
        };
        o.diff.detect = Some(renames);
    }
    if o.diff.abbrev.is_none() {
        o.diff.abbrev = Some(None);
    }
    if o.diff.stat_width.is_none() {
        o.diff.stat_width = Some(72);
    }
    o.diff.resolve_objects(&mut repo)?;
    let branch = description_branch(&mut repo, &o);
    let repo = repo;
    let pathspec = crate::diff::pathspec(ctx, &repo, &o.paths)?;

    let diff_between =
        |old: Option<&ObjectId>, new: &ObjectId, formats: Formats| -> Result<Vec<u8>, Die> {
            let mut w = TreeDiff {
                repo: &repo,
                pathspec: &pathspec,
                recursive: true,
                show_trees: false,
                pairs: Vec::new(),
            };
            w.walk(old, Some(new), b"")?;
            let mut pairs = std::mem::take(&mut w.pairs);
            if o.diff.copies_harder {
                if let Some(old) = old {
                    let files = crate::diff::collect::tree_files(&repo, old, &pathspec)?;
                    pairs = crate::diff::collect::with_unmodified(pairs, files);
                }
            }
            let out = Output::new(Some(&repo), repo.hash, &o.diff, formats, false)?;
            Ok(out.render(pairs)?.0)
        };

    // Commits at a shallow boundary have no parents.
    let shallow: Vec<Vec<u8>> = std::fs::read(repo.setup.common_dir.join("shallow"))
        .map(|d| d.split(|&b| b == b'\n').map(<[u8]>::to_vec).collect())
        .unwrap_or_default();
    let first_parent = |id: &ObjectId, c: &Commit| -> Option<ObjectId> {
        if shallow.iter().any(|l| l == id.to_hex().as_bytes()) {
            return None;
        }
        c.parents().unwrap_or_default().first().copied()
    };
    let limit = match (&o.max_count, o.paths.is_empty()) {
        (Some(n), false) => n.parse::<usize>().ok(),
        _ => None,
    };
    let mut patches = Vec::new();
    for id in ids {
        if limit.is_some_and(|l| patches.len() >= l) {
            break;
        }
        let Some(Object::Commit(commit)) = repo.read_object(&id) else {
            return Err(Die::Fatal(crate::messages::rev_list::bad_object(
                &id.to_hex(),
            )));
        };
        let tree = commit.tree().map_err(|e| Die::Fatal(e.to_string()))?;
        let parent_tree = match first_parent(&id, &commit).as_ref() {
            Some(p) => match repo.read_object(p) {
                Some(Object::Commit(pc)) => Some(pc.tree().map_err(|e| Die::Fatal(e.to_string()))?),
                _ => {
                    return Err(Die::Fatal(crate::messages::rev_list::bad_object(
                        &p.to_hex(),
                    )));
                }
            },
            None => None,
        };
        let diff = diff_between(parent_tree.as_ref(), &tree, formats)?;
        if diff.is_empty() && !o.paths.is_empty() {
            continue;
        }
        patches.push(Patch {
            id,
            commit,
            diff,
            has_stat: formats.stat,
        });
    }
    if o.ignore_if_in_upstream && !patches.is_empty() {
        let mut opts = o.diff.clone();
        opts.detect = Some(Detect::Off);
        let only_patch = Formats {
            patch: true,
            ..Formats::default()
        };
        let key_of = |c: &Commit, id: &ObjectId| -> Result<Option<Vec<u8>>, Die> {
            let tree = c.tree().map_err(|e| Die::Fatal(e.to_string()))?;
            let parent_tree = match first_parent(id, c) {
                Some(p) => match repo.read_object(&p) {
                    Some(Object::Commit(pc)) => pc.tree().ok(),
                    _ => None,
                },
                None => None,
            };
            let mut w = TreeDiff {
                repo: &repo,
                pathspec: &pathspec,
                recursive: true,
                show_trees: false,
                pairs: Vec::new(),
            };
            w.walk(parent_tree.as_ref(), Some(&tree), b"")?;
            let out = Output::new(Some(&repo), repo.hash, &opts, only_patch, false)?;
            let text = out.render(std::mem::take(&mut w.pairs))?.0;
            Ok((!text.is_empty()).then(|| patch_key(&text)))
        };
        let mut upstream = std::collections::HashSet::new();
        for id in crate::history::walk_commits(ctx, &upstream_args(&o))? {
            if let Some(Object::Commit(c)) = repo.read_object(&id) {
                if let Some(k) = key_of(&c, &id)? {
                    upstream.insert(k);
                }
            }
        }
        let mut kept = Vec::with_capacity(patches.len());
        for p in patches {
            if key_of(&p.commit, &p.id)?.is_some_and(|k| upstream.contains(&k)) {
                continue;
            }
            kept.push(p);
        }
        patches = kept;
    }
    if patches.is_empty() {
        return Ok(0);
    }
    patches.reverse();

    let count = patches.len();
    let cover = match s.cover_letter {
        CoverLetter::Yes => true,
        CoverLetter::No => false,
        CoverLetter::Auto => count > 1,
    };
    let numbered = !s.keep_subject
        && match s.numbered {
            Numbered::Yes => true,
            Numbered::No => false,
            Numbered::Auto => count > 1 || cover,
        };
    let total = s.start + count - 1;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (me, my_time, my_tz) = committer(&repo)?;
    let zero = ObjectId::null(repo.hash);

    let mut writer = Writer::new(&repo, &o, &s)?;
    let mut refs: Vec<String> = s.in_reply_to.iter().cloned().collect();
    let mut first = true;
    let mut thread_headers = |out: &mut Vec<u8>, id: String, first: &mut bool| {
        if s.thread.is_some() {
            out.extend_from_slice(format!("Message-ID: {id}\n").as_bytes());
        }
        if let Some(last) = refs.last() {
            out.extend_from_slice(format!("In-Reply-To: {last}\n").as_bytes());
            out.extend_from_slice(format!("References: {}\n", refs.join("\n\t")).as_bytes());
        }
        // Shallow threads hang off the head: the cover letter, else the
        // message replied to, else the first patch.
        let head = *first && (cover || s.in_reply_to.is_none());
        if s.thread == Some(Thread::Deep) || (s.thread.is_some() && head) {
            refs.push(id);
        }
        *first = false;
    };

    if cover {
        let tip = &patches[count - 1].id;
        let mut out = Vec::new();
        let hex = if s.zero_commit {
            zero.to_hex()
        } else {
            tip.to_hex()
        };
        out.extend_from_slice(format!("From {hex} {}\n", msg::MAGIC_DATE).as_bytes());
        thread_headers(
            &mut out,
            format!("<cover.{now}.git.{}>", String::from_utf8_lossy(&me.email)),
            &mut first,
        );
        let sender = s.from.clone().unwrap_or_else(|| me.clone());
        out.extend_from_slice(b"From: ");
        rgit_mail::write_address(&mut out, &sender.name, &sender.email, CHARSET, s.encode);
        out.push(b'\n');
        // The date of the sender identity: none for one given as text.
        let (time, tz) = if s.from_ident {
            (0, 0)
        } else {
            (my_time, my_tz)
        };
        out.extend_from_slice(format!("Date: {}\n", rfc2822(time, tz)).as_bytes());

        // Subject and body from the branch description.
        let description = match &o.description_file {
            Some(f) => Some(
                std::fs::read(cwd_path(&repo, f))
                    .map_err(|_| Die::Fatal(msg::cannot_read_description(f)))?,
            ),
            None => branch.as_ref().and_then(|b| {
                repo.config
                    .get(format!("branch.{b}.description"))
                    .and_then(|e| e.value.as_ref().map(|v| v.to_vec()))
            }),
        };
        let mode = match s.cover_mode {
            CoverMode::Auto => {
                let first_para = description
                    .as_deref()
                    .map(|d| mail::split_message(d).0.len())
                    .unwrap_or(0);
                if first_para > 100 {
                    CoverMode::Message
                } else {
                    CoverMode::Subject
                }
            }
            m => m,
        };
        let (subject, body): (Vec<u8>, Vec<u8>) = match (&description, mode) {
            (Some(d), CoverMode::Subject) => {
                let (subj, _) = mail::split_message(d);
                // The rest of the description as written.
                let lines: Vec<&[u8]> = d.split(|&b| b == b'\n').collect();
                let blank = |l: &[u8]| l.iter().all(u8::is_ascii_whitespace);
                let mut i = lines.iter().position(|l| !blank(l)).unwrap_or(lines.len());
                while i < lines.len() && !blank(lines[i]) {
                    i += 1;
                }
                while i < lines.len() && blank(lines[i]) {
                    i += 1;
                }
                let mut rest = lines[i.min(lines.len())..].join(&b'\n');
                if !rest.is_empty() && !rest.ends_with(b"\n") {
                    rest.push(b'\n');
                }
                (subj, rest)
            }
            (Some(d), CoverMode::Message) => {
                let mut d = d.clone();
                if !d.is_empty() && !d.ends_with(b"\n") {
                    d.push(b'\n');
                }
                (msg::SUBJECT_HERE.as_bytes().to_vec(), d)
            }
            _ => (
                msg::SUBJECT_HERE.as_bytes().to_vec(),
                format!("{}\n", msg::BLURB_HERE).into_bytes(),
            ),
        };
        let number = numbered.then_some((0, total));
        out.extend_from_slice(b"Subject: ");
        out.extend_from_slice(mail::subject_tag(&s.prefix, number).as_bytes());
        rgit_mail::write_text(&mut out, &subject, CHARSET, s.encode);
        out.push(b'\n');

        let mut text = body;
        text.push(b'\n');
        text.extend_from_slice(&shortlog(&patches));
        // The diffstat from the one commit the series is based on to its
        // tip; there is none for a series on several bases or a root.
        let mut bases: Vec<ObjectId> = Vec::new();
        for p in &patches {
            if shallow.iter().any(|l| l == p.id.to_hex().as_bytes()) {
                continue;
            }
            for parent in p.commit.parents().unwrap_or_default() {
                if !patches.iter().any(|q| q.id == parent) && !bases.contains(&parent) {
                    bases.push(parent);
                }
            }
        }
        if let [base] = bases[..] {
            let base_tree = match repo.read_object(&base) {
                Some(Object::Commit(pc)) => pc.tree().ok(),
                _ => None,
            };
            let tip_tree = patches[count - 1]
                .commit
                .tree()
                .map_err(|e| Die::Fatal(e.to_string()))?;
            let stat = diff_between(
                base_tree.as_ref(),
                &tip_tree,
                Formats {
                    stat: true,
                    summary: true,
                    ..Formats::default()
                },
            )?;
            text.extend_from_slice(&stat);
            text.push(b'\n');
        }

        // Like the series: 8-bit when any of its messages is.
        let series_8bit = patches.iter().any(|p| !p.commit.message().is_ascii());
        if series_8bit || !subject.is_ascii() || !text.is_ascii() {
            out.extend_from_slice(MIME_HEADERS);
        }
        extra_headers(&mut out, &s);
        out.push(b'\n');
        out.extend_from_slice(&text);
        if let Some(sig) = &s.signature {
            mail::signature_block(&mut out, sig);
        }
        let name = if s.numbered_files {
            "0".to_string()
        } else {
            mail::file_name(
                0,
                Some(b"cover-letter"),
                s.reroll.as_deref(),
                &s.suffix,
                s.filename_max,
            )
        };
        writer.write(&name, &out)?;
    }

    let mboxrd = o.mboxrd
        || repo
            .config
            .bool("format.mboxrd")
            .map_err(config_err)?
            .unwrap_or(false);
    let trailer_conf = if s.signoff {
        let (conf, warnings) =
            rgit_trailer::Config::from_config(&repo.config).map_err(|entry| {
                let name = String::from_utf8_lossy(&entry.name).into_owned();
                crate::comment::bad_entry(&entry, &crate::messages::comment::missing_value(&name))
            })?;
        for w in warnings {
            eprintln!("warning: {w}");
        }
        Some(conf)
    } else {
        None
    };

    for (i, p) in patches.iter().enumerate() {
        let n = s.start + i;
        let message = p.commit.message();
        let (subject, mut body) = mail::split_message(message);
        if let Some(conf) = &trailer_conf {
            body = signoff(message, &subject, &body, &me, conf);
        }
        let author = p.commit.author();
        let author_person = author.map(|a| Person {
            name: a.name.to_vec(),
            email: a.email.to_vec(),
        });

        let mut out = Vec::new();
        let hex = if s.zero_commit {
            zero.to_hex()
        } else {
            p.id.to_hex()
        };
        out.extend_from_slice(format!("From {hex} {}\n", msg::MAGIC_DATE).as_bytes());
        thread_headers(
            &mut out,
            format!(
                "<{}.{now}.git.{}>",
                p.id.to_hex(),
                String::from_utf8_lossy(&me.email)
            ),
            &mut first,
        );
        let sender = s.from.clone().or_else(|| author_person.clone());
        if let Some(sender) = &sender {
            out.extend_from_slice(b"From: ");
            rgit_mail::write_address(&mut out, &sender.name, &sender.email, CHARSET, s.encode);
            out.push(b'\n');
        }
        if let Some(t) = author.and_then(|a| a.time()) {
            let secs = u64::try_from(t.seconds).unwrap_or(0);
            out.extend_from_slice(
                format!("Date: {}\n", rfc2822(secs, tz_hhmm(t.offset_minutes))).as_bytes(),
            );
        }
        out.extend_from_slice(b"Subject: ");
        if !s.keep_subject {
            let number = numbered.then_some((n, total));
            out.extend_from_slice(mail::subject_tag(&s.prefix, number).as_bytes());
        }
        rgit_mail::write_text(&mut out, &subject, CHARSET, s.encode);
        out.push(b'\n');

        let mut text = Vec::new();
        if let (Some(from), Some(author)) = (&s.from, &author_person) {
            if s.force_in_body_from || from != author {
                text.extend_from_slice(b"From: ");
                text.extend_from_slice(&author.plain());
                text.extend_from_slice(b"\n\n");
            }
        }
        if mboxrd {
            text.extend_from_slice(&mail::mboxrd_escape(&body));
        } else {
            text.extend_from_slice(&body);
        }
        if !subject.is_ascii() || !text.is_ascii() {
            out.extend_from_slice(MIME_HEADERS);
        }
        extra_headers(&mut out, &s);
        out.push(b'\n');
        out.extend_from_slice(&text);
        if !p.diff.is_empty() {
            out.extend_from_slice(if p.has_stat { b"---\n" } else { b"\n" });
            out.extend_from_slice(&p.diff);
        }
        if let Some(sig) = &s.signature {
            mail::signature_block(&mut out, sig);
        }
        let name = if s.numbered_files {
            n.to_string()
        } else {
            // The name comes from the first line only.
            let first_line = message
                .split(|&b| b == b'\n')
                .find(|l| !l.trim_ascii().is_empty())
                .unwrap_or_default();
            mail::file_name(
                n,
                Some(first_line),
                s.reroll.as_deref(),
                &s.suffix,
                s.filename_max,
            )
        };
        if i > 0 {
            writer.separate();
        }
        writer.write(&name, &out)?;
    }
    writer.finish()?;
    Ok(0)
}

/// The body with the committer's `Signed-off-by` added, unless the
/// trailers of the message already have it.
fn signoff(
    message: &[u8],
    subject: &[u8],
    body: &[u8],
    me: &Person,
    conf: &rgit_trailer::Config,
) -> Vec<u8> {
    let mut line = b"Signed-off-by: ".to_vec();
    line.extend_from_slice(&me.plain());
    if subject.is_empty() {
        line.push(b'\n');
        return line;
    }
    let end = rgit_trailer::message_end(message, b"");
    let info = rgit_trailer::parse(
        &message[..end],
        conf,
        &rgit_trailer::ParseOptions {
            comment: b"",
            divider: false,
        },
    );
    let ident = me.plain();
    if info.trailers().any(|t| {
        t.token.eq_ignore_ascii_case(b"Signed-off-by") && t.value.as_slice() == ident.as_slice()
    }) {
        return body.to_vec();
    }
    let signed = rgit_trailer::append_signoff(message, &line, conf, b"");
    mail::split_message(&signed).1
}

/// `To:`, `Cc:` and the extra headers.
fn extra_headers(out: &mut Vec<u8>, s: &Settings) {
    for h in &s.extra {
        out.extend_from_slice(h.as_bytes());
        out.push(b'\n');
    }
    for (name, list) in [("To", &s.to), ("Cc", &s.cc)] {
        if list.is_empty() {
            continue;
        }
        out.extend_from_slice(format!("{name}: {}\n", list.join(",\n    ")).as_bytes());
    }
}

/// The cover letter's list of commits: per author (sorted by name), the
/// subjects wrapped to 72 columns.
fn shortlog(patches: &[Patch]) -> Vec<u8> {
    let mut groups: Vec<(Vec<u8>, Vec<Vec<u8>>)> = Vec::new();
    for p in patches {
        let name = p
            .commit
            .author()
            .map(|a| a.name.to_vec())
            .unwrap_or_default();
        let (subject, _) = mail::split_message(p.commit.message());
        match groups.iter_mut().find(|g| g.0 == name) {
            Some(g) => g.1.push(subject),
            None => groups.push((name, vec![subject])),
        }
    }
    groups.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = Vec::new();
    for (name, subjects) in groups {
        out.extend_from_slice(&name);
        out.extend_from_slice(format!(" ({}):\n", subjects.len()).as_bytes());
        for subject in subjects {
            let subject = subject.trim_ascii_start();
            out.extend_from_slice(&crate::history::layout::wrap(subject, 72, 2, 4));
            out.push(b'\n');
        }
        out.push(b'\n');
    }
    out
}

/// Where the mails go: standard output, one file, or a file per mail
/// (whose names are printed).
struct Writer {
    stdout: bool,
    single: Option<std::fs::File>,
    dir: Option<(String, PathBuf)>,
    base: PathBuf,
    quiet: bool,
    out: Vec<u8>,
}

impl Writer {
    fn new(repo: &Repo, o: &Options, s: &Settings) -> Result<Writer, Die> {
        let base = cwd_path(repo, ".");
        let single = match &o.output {
            Some(f) => {
                let path = cwd_path(repo, f);
                Some(std::fs::File::create(&path).map_err(|e| {
                    Die::Fatal(msg::cannot_open_file(f, &crate::global::strerror(&e)))
                })?)
            }
            None => None,
        };
        let dir = match &s.output_dir {
            Some(d) if !o.stdout && single.is_none() => {
                let path = cwd_path(repo, d);
                std::fs::create_dir_all(&path)
                    .map_err(|_| Die::Fatal(msg::cannot_create_directory(d)))?;
                Some((d.clone(), path))
            }
            _ => None,
        };
        Ok(Writer {
            stdout: o.stdout,
            single,
            dir,
            base,
            quiet: o.quiet,
            out: Vec::new(),
        })
    }

    fn write(&mut self, name: &str, mail: &[u8]) -> Result<(), Die> {
        if self.stdout {
            let mut so = std::io::stdout().lock();
            let _ = so.write_all(mail);
            return Ok(());
        }
        if let Some(f) = &mut self.single {
            return f.write_all(mail).map_err(|e| Die::Fatal(e.to_string()));
        }
        let (shown, path) = match &self.dir {
            Some((given, dir)) => {
                let shown = if given.ends_with('/') {
                    format!("{given}{name}")
                } else {
                    format!("{given}/{name}")
                };
                (shown, dir.join(name))
            }
            None => (name.to_string(), self.base.join(name)),
        };
        std::fs::write(&path, mail)
            .map_err(|e| Die::Fatal(msg::cannot_open_file(&shown, &crate::global::strerror(&e))))?;
        if !self.quiet {
            self.out.extend_from_slice(shown.as_bytes());
            self.out.push(b'\n');
        }
        Ok(())
    }

    /// Between two patches on standard output or in one file: an empty
    /// line.
    fn separate(&mut self) {
        if self.stdout {
            let _ = std::io::stdout().lock().write_all(b"\n");
        } else if let Some(f) = &mut self.single {
            let _ = f.write_all(b"\n");
        }
    }

    fn finish(&mut self) -> Result<(), Die> {
        let mut so = std::io::stdout().lock();
        let _ = so.write_all(&self.out);
        let _ = so.flush();
        Ok(())
    }
}
