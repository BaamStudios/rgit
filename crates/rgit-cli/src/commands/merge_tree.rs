// SPDX-License-Identifier: MIT
//! `git merge-tree --write-tree` (Documentation/git-merge-tree.adoc): merge
//! two commits (or, with `--merge-base`, two trees) in memory with
//! [`rgit_merge`], write the result tree and print it with the conflicted
//! files and the merge messages.
//!
//! `--stdin` runs one merge per input line; the deprecated
//! `--trivial-merge` mode is in `merge_tree/trivial.rs`.

use std::io::Write;

#[path = "merge_tree/trivial.rs"]
mod trivial;

use rgit_hash::ObjectId;
use rgit_merge::{
    Algorithm, DirectoryRenames, Favor, MergeResult, MessageKind, ObjectCommits, Options, Subtree,
};
use rgit_object::ObjectKind;

use crate::messages::merge_tree as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;
use crate::revision::Resolver;

const MODE_WRITE_TREE: i32 = 1;
const MODE_TRIVIAL: i32 = 2;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mode = Cell::new(0);
    let messages = Cell::new(-1);
    let quiet = Cell::new(false);
    let nul = Cell::new(false);
    let name_only = Cell::new(false);
    let allow_unrelated = Cell::new(false);
    let stdin = Cell::new(false);
    let merge_base = RefCell::new(None::<String>);
    let xopts = RefCell::new(Vec::<String>::new());
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::cmdmode(
                None,
                "write-tree",
                &mode,
                msg::HELP_WRITE_TREE,
                MODE_WRITE_TREE,
            ),
            Opt::cmdmode(
                None,
                "trivial-merge",
                &mode,
                msg::HELP_TRIVIAL_MERGE,
                MODE_TRIVIAL,
            ),
            Opt::bool(None, "messages", &messages, msg::HELP_MESSAGES),
            Opt::flag(None, "quiet", &quiet, msg::HELP_QUIET).noneg(),
            Opt::flag('z', None, &nul, msg::HELP_Z),
            Opt::flag(None, "name-only", &name_only, msg::HELP_NAME_ONLY).noneg(),
            Opt::flag(
                None,
                "allow-unrelated-histories",
                &allow_unrelated,
                msg::HELP_ALLOW_UNRELATED,
            )
            .noneg(),
            Opt::flag(None, "stdin", &stdin, msg::HELP_STDIN).noneg(),
            Opt::string(
                None,
                "merge-base",
                &merge_base,
                "tree-ish",
                msg::HELP_MERGE_BASE,
            ),
            Opt::string_list(
                'X',
                "strategy-option",
                &xopts,
                "option=value",
                msg::HELP_STRATEGY_OPTION,
            ),
        ],
    );
    let args = parser.parse(argv)?.args;
    let explicit_trivial = mode.get() == MODE_TRIVIAL;
    let mut mode = mode.get();
    if mode == 0 {
        mode = match args.len() {
            3 => MODE_TRIVIAL,
            _ => MODE_WRITE_TREE,
        };
    }
    let wanted = match mode {
        MODE_TRIVIAL => 3,
        _ => 2,
    };
    if !stdin.get() && args.len() != wanted {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let merge_base = merge_base.into_inner();
    if stdin.get() && merge_base.is_some() {
        return Err(Die::Fatal(crate::messages::merge_base::incompatible(
            "--merge-base",
            "--stdin",
        )));
    }
    if quiet.get() && name_only.get() {
        return Err(Die::Fatal(crate::messages::merge_base::incompatible(
            "--quiet",
            "--name-only",
        )));
    }
    if mode == MODE_TRIVIAL {
        let other = argv[1..]
            .iter()
            .any(|a| a.starts_with('-') && a != "--trivial-merge");
        if other && (explicit_trivial || args.len() == 3) {
            return Err(Die::Fatal(msg::TRIVIAL_INCOMPATIBLE.into()));
        }
        let mut repo = Repo::open(ctx, Mode::Required)?;
        let base = tree_arg(&mut repo, &args[0])?;
        let ours = tree_arg(&mut repo, &args[1])?;
        let theirs = tree_arg(&mut repo, &args[2])?;
        let out = trivial::run(&repo, &base, &ours, &theirs)?;
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(&out);
        let _ = stdout.flush();
        return Ok(0);
    }
    let mut repo = Repo::open(ctx, Mode::Required)?;
    let mut opts = Options {
        style: crate::commands::merge_file::config_style(&repo.config)?,
        allow_unrelated_histories: allow_unrelated.get(),
        renormalize: super::merge::renormalize_config(&repo.config),
        ..Options::default()
    };
    apply_rename_config(&repo.config, &mut opts);
    for x in xopts.into_inner() {
        strategy_option(&mut opts, &x)?;
    }
    let attr_tree = crate::attributes::tree_source(ctx, &mut repo, None)?;
    opts.path_merge = Some(super::merge::attrs::RepoPathMerge::open(
        ctx,
        &repo,
        opts.renormalize,
        None,
        attr_tree,
    )?);

    if stdin.get() {
        return merge_stdin(ctx, &mut repo, &opts, name_only.get(), messages.get());
    }
    // --quiet writes no objects: the merge keeps them in memory.
    let result = merge_one(
        &mut repo,
        &opts,
        merge_base.as_deref(),
        &args[0],
        &args[1],
        quiet.get(),
    )?;

    advise_submodules(ctx, &repo, &result)?;
    if quiet.get() {
        return Ok(if result.clean { 0 } else { 1 });
    }
    let show_messages = match messages.get() {
        -1 => !result.clean,
        v => v != 0,
    };
    let out = render(
        &result,
        nul.get(),
        name_only.get(),
        show_messages,
        repo.quote_path(),
    );
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(&out);
    let _ = stdout.flush();
    Ok(if result.clean { 0 } else { 1 })
}

/// Applies the rename settings of the configuration to merge options:
/// `merge.renames` (else `diff.renames`), `merge.renameLimit` (else
/// `diff.renameLimit`) and `merge.directoryRenames`
/// (git-config(1)). Unparsable values keep the defaults.
pub(crate) fn apply_rename_config(config: &rgit_config::ConfigSet, opts: &mut Options) {
    let renames = |key: &str| {
        let e = config.get(key)?;
        match e.string().ok().map(|v| v.to_ascii_lowercase()) {
            Some(v) if v == b"copies" || v == b"copy" => Some(true),
            _ => e.bool().ok(),
        }
    };
    if let Some(on) = renames("merge.renames").or_else(|| renames("diff.renames")) {
        opts.renames = on;
    }
    let limit = |key: &str| config.int(key).ok().flatten();
    if let Some(n) = limit("merge.renamelimit").or_else(|| limit("diff.renamelimit")) {
        opts.rename_limit = usize::try_from(n).unwrap_or(0);
    }
    if let Some(e) = config.get("merge.directoryrenames") {
        let conflict = e
            .string()
            .is_ok_and(|v| v.eq_ignore_ascii_case(b"conflict"));
        opts.directory_renames = if conflict {
            DirectoryRenames::Conflict
        } else {
            match e.bool() {
                Ok(true) => DirectoryRenames::True,
                Ok(false) => DirectoryRenames::False,
                Err(_) => opts.directory_renames,
            }
        };
    }
}

/// One merge: of the trees `a` and `b` against the tree-ish `base`, or of
/// the commits `a` and `b`.
fn merge_one(
    repo: &mut Repo,
    opts: &Options,
    base: Option<&str>,
    a: &str,
    b: &str,
    in_memory: bool,
) -> Result<MergeResult, Die> {
    let mut opts = opts.clone();
    opts.branch1 = a.as_bytes().to_vec();
    opts.branch2 = b.as_bytes().to_vec();
    let ids = match base {
        Some(base) => {
            opts.ancestor = base.as_bytes().to_vec();
            [Some(tree_arg(repo, base)?), Some(tree_arg(repo, a)?), Some(tree_arg(repo, b)?)]
        }
        None => {
            // Replace refs apply to the commits merged (core.useReplaceRefs).
            let (x, y) = (commit_arg(repo, a)?, commit_arg(repo, b)?);
            [None, Some(repo.replaced(&x)), Some(repo.replaced(&y))]
        }
    };
    let repo = &*repo;
    let memory;
    let objects: &dyn rgit_merge::Objects = if in_memory {
        memory = InMemory {
            odb: repo.odb(),
            written: std::cell::RefCell::new(std::collections::HashMap::new()),
        };
        &memory
    } else {
        repo.odb()
    };
    let [base, ours, theirs] = ids;
    let (ours, theirs) = (ours.expect("resolved"), theirs.expect("resolved"));
    match base {
        Some(base_tree) => rgit_merge::merge_trees(objects, &base_tree, &ours, &theirs, &opts),
        None => {
            let shallow = std::fs::read_to_string(repo.setup.common_dir.join("shallow"))
                .unwrap_or_default()
                .lines()
                .filter_map(|l| ObjectId::from_hex(repo.hash, l.trim().as_bytes()).ok())
                .collect();
            let len = repo.default_abbrev();
            let commits = ObjectCommits {
                objects: repo.odb(),
                shallow,
                label: Box::new(|id| repo.abbrev(id, len).into_bytes()),
            };
            rgit_merge::merge_commits(objects, &commits, &ours, &theirs, &opts)
        }
    }
    .map_err(merge_failed)
}

/// A merge error as git reports it.
pub(crate) fn merge_failed(e: rgit_merge::Error) -> Die {
    for line in e.collect_errors().into_iter().flatten() {
        eprintln!("error: {line}");
    }
    Die::Fatal(e.to_string())
}

/// Objects read from the repository, written only to memory (`--quiet`).
struct InMemory<'a> {
    odb: &'a rgit_odb::ObjectDatabase,
    written: std::cell::RefCell<std::collections::HashMap<ObjectId, rgit_object::RawObject>>,
}

impl rgit_merge::Objects for InMemory<'_> {
    fn hash_kind(&self) -> rgit_hash::HashKind {
        self.odb.hash_kind()
    }

    fn read(&self, id: &ObjectId) -> Result<Option<rgit_object::RawObject>, String> {
        if let Some(o) = self.written.borrow().get(id) {
            return Ok(Some(o.clone()));
        }
        rgit_merge::Objects::read(self.odb, id)
    }

    fn write(&self, kind: ObjectKind, data: &[u8]) -> Result<ObjectId, String> {
        let id = rgit_hash::hash_object(self.odb.hash_kind(), kind.as_str(), data)
            .map_err(|e| e.to_string())?;
        self.written
            .borrow_mut()
            .insert(id, rgit_object::RawObject::new(kind, data.to_vec()));
        Ok(id)
    }
}

/// `--stdin`: one merge per input line (`[<base> -- ]<branch1>
/// <branch2>`), each printed as its status, the `-z` output and a NUL.
fn merge_stdin(
    ctx: &Context,
    repo: &mut Repo,
    opts: &Options,
    name_only: bool,
    messages: i32,
) -> CmdResult {
    use std::io::BufRead as _;
    let stdin = std::io::stdin();
    let mut line = Vec::new();
    let mut lock = stdin.lock();
    loop {
        line.clear();
        if lock.read_until(b'\n', &mut line).map_err(|e| Die::Fatal(e.to_string()))? == 0 {
            break;
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end_matches(['\n', '\r']);
        let words: Vec<&str> = text.split(' ').collect();
        let (base, a, b) = match words.as_slice() {
            [a, b] => (None, *a, *b),
            [base, "--", a, b] => (Some(*base), *a, *b),
            // git names the first word of a line with too many words.
            [first, _, _, ..] => return Err(Die::Fatal(msg::malformed_input_line(first))),
            _ => return Err(Die::Fatal(msg::malformed_input_line(text))),
        };
        let result = merge_one(repo, opts, base, a, b, false)?;
        advise_submodules(ctx, repo, &result)?;
        let show_messages = match messages {
            -1 => !result.clean,
            v => v != 0,
        };
        let mut out = if result.clean { b"1\0".to_vec() } else { b"0\0".to_vec() };
        out.extend_from_slice(&render(&result, true, name_only, show_messages, repo.quote_path()));
        out.push(b'\0');
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(&out);
        let _ = stdout.flush();
    }
    Ok(0)
}

/// Why a `-X` option was not applied.
pub(crate) enum StrategyOptionError {
    /// git does not know it.
    Unknown,
    /// rgit does not implement it yet (none at the moment).
    #[allow(dead_code)]
    Unsupported,
}

/// Applies one `-X` option.
fn strategy_option(opts: &mut Options, x: &str) -> Result<(), Die> {
    apply_strategy_option(opts, x).map_err(|e| match e {
        StrategyOptionError::Unknown => Die::Fatal(msg::unknown_strategy_option(x)),
        StrategyOptionError::Unsupported => Die::Fatal(msg::strategy_option_not_supported(x)),
    })
}

/// Applies one `-X` option to the merge options (shared with `merge`).
pub(crate) fn apply_strategy_option(opts: &mut Options, x: &str) -> Result<(), StrategyOptionError> {
    let unknown = || StrategyOptionError::Unknown;
    let (name, value) = match x.split_once('=') {
        Some((n, v)) => (n, Some(v)),
        None => (x, None),
    };
    match (name, value) {
        ("ours", None) => opts.favor = Favor::Ours,
        ("theirs", None) => opts.favor = Favor::Theirs,
        ("no-renames", None) => opts.renames = false,
        ("find-renames", None) => {
            opts.renames = true;
            opts.rename_score = rgit_diff::diffcore::DEFAULT_RENAME_SCORE;
        }
        ("find-renames" | "rename-threshold", Some(v)) => {
            if !valid_similarity(v) {
                return Err(unknown());
            }
            opts.renames = true;
            if let (Some(score), "") = rgit_diff::diffcore::parse_score(v) {
                opts.rename_score = score;
            }
        }
        ("diff-algorithm", Some(v)) => match Algorithm::parse(v) {
            Some((algorithm, minimal)) => {
                opts.algorithm = algorithm;
                opts.minimal = minimal;
            }
            None => return Err(unknown()),
        },
        ("patience", None) => (opts.algorithm, opts.minimal) = (Algorithm::Patience, false),
        ("histogram", None) => (opts.algorithm, opts.minimal) = (Algorithm::Histogram, false),
        ("ignore-space-change", None) => opts.whitespace.change = true,
        ("ignore-all-space", None) => opts.whitespace.all = true,
        ("ignore-space-at-eol", None) => opts.whitespace.at_eol = true,
        ("ignore-cr-at-eol", None) => opts.whitespace.cr_at_eol = true,
        ("renormalize", None) => opts.renormalize = true,
        ("no-renormalize", None) => opts.renormalize = false,
        ("subtree", None) => opts.subtree = Subtree::Guess,
        ("subtree", Some(p)) => opts.subtree = Subtree::Path(p.as_bytes().to_vec()),
        _ => return Err(unknown()),
    }
    Ok(())
}

/// A similarity score as `find-renames=<n>` takes it: digits, optionally
/// with a fraction or a percent sign.
fn valid_similarity(v: &str) -> bool {
    let digits = v.strip_suffix('%').unwrap_or(v);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit() || b == b'.')
}

/// Resolves a branch argument to a commit; git's wording on failure.
fn commit_arg(repo: &mut Repo, name: &str) -> Result<ObjectId, Die> {
    let mut r = Resolver::new(repo);
    let found = r
        .resolve(name)
        .ok()
        .and_then(|res| r.peel_to(res.oid, ObjectKind::Commit, Some(name)));
    found.ok_or_else(|| {
        eprintln!("{}", msg::not_something_we_can_merge(name));
        Die::Exit(1)
    })
}

/// Resolves a tree-ish argument (`--merge-base` mode).
fn tree_arg(repo: &mut Repo, name: &str) -> Result<ObjectId, Die> {
    let mut r = Resolver::new(repo);
    let oid = r
        .resolve(name)
        .ok()
        .map(|res| res.oid)
        .ok_or_else(|| Die::Fatal(msg::could_not_parse_as_tree(name)))?;
    // A blob is reported when the merge reads it.
    Ok(r.peel_to(oid, ObjectKind::Tree, None).unwrap_or(oid))
}

/// What follows the messages of a merge on stderr: the warnings when the
/// rename limit was hit, and `advice.submoduleMergeConflict` for
/// submodules left in conflict.
pub(crate) fn advise_submodules(ctx: &Context, repo: &Repo, result: &MergeResult) -> Result<(), Die> {
    if let Some(n) = result.rename_limit_needed {
        for w in rgit_merge::messages::rename_limit_warnings(n) {
            eprintln!("{w}");
        }
    }
    let mut paths: Vec<String> = Vec::new();
    let mut steps = String::new();
    for m in &result.messages {
        let path = String::from_utf8_lossy(&m.paths[0]).into_owned();
        match m.kind {
            MessageKind::SubmoduleNotInitialized => paths.push(path),
            MessageKind::SubmoduleConflict => {
                let theirs = result
                    .conflicts
                    .iter()
                    .find(|c| c.path == m.paths[0])
                    .and_then(|c| c.stages[2]);
                if let Some(v) = theirs {
                    let hex = v.oid.to_hex();
                    steps.push_str(&msg::submodule_merge_step(&path, &hex[..7.min(hex.len())]));
                }
                paths.push(path);
            }
            _ => {}
        }
    }
    if paths.is_empty() {
        return Ok(());
    }
    let advisor = ctx.advisor(&repo.config)?;
    advisor
        .advise(
            rgit_messages::Advice::SubmoduleMergeConflict,
            &msg::submodule_merge_conflict_advice(&paths.join(" "), &steps),
        )
        .map_err(Die::Fatal)
}

/// The output of a merge: tree, conflicted files, messages.
fn render(
    result: &MergeResult,
    nul: bool,
    name_only: bool,
    show_messages: bool,
    quote_high: bool,
) -> Vec<u8> {
    let end = if nul { b'\0' } else { b'\n' };
    let mut out = Vec::new();
    out.extend_from_slice(result.tree.to_hex().as_bytes());
    out.push(end);
    if !result.clean {
        for c in &result.conflicts {
            let mut path = Vec::new();
            if nul {
                path.extend_from_slice(&c.path);
            } else {
                crate::quote::write_quoted(&mut path, &c.path, quote_high);
            }
            for (i, v) in c.stages.iter().enumerate() {
                let Some(v) = v else { continue };
                if !name_only {
                    out.extend_from_slice(format!("{:06o} {} {}\t", v.mode, v.oid, i + 1).as_bytes());
                }
                out.extend_from_slice(&path);
                out.push(end);
                if name_only {
                    break;
                }
            }
        }
    }
    if show_messages {
        out.push(end);
        for m in &result.messages {
            if nul {
                out.extend_from_slice(m.paths.len().to_string().as_bytes());
                out.push(0);
                for p in &m.paths {
                    out.extend_from_slice(p);
                    out.push(0);
                }
                out.extend_from_slice(m.kind.as_str().as_bytes());
                out.push(0);
                out.extend_from_slice(&m.text);
                out.extend_from_slice(b"\n\0");
            } else {
                out.extend_from_slice(&m.text);
                out.push(b'\n');
            }
        }
    }
    out
}
