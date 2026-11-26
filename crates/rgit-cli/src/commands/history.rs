// SPDX-License-Identifier: MIT
//! `git history` (Documentation/git-history.adoc; experimental upstream):
//! rewrite one commit and replay its descendants.
//!
//! - `reword <commit>`: a new message from the editor;
//! - `fixup <commit>`: the staged changes merged into the commit (a
//!   three-way merge of the HEAD tree, the commit's tree and the index
//!   tree);
//! - `drop <commit>`: the commit removed, its descendants replayed onto its
//!   parent;
//! - `split <commit> [--] [<pathspec>...]`: the hunks chosen with the
//!   `add -p` selection become a new parent commit.
//!
//! The descendants of the commit (`--ancestry-path` of the updated tips:
//! all branches and a detached `HEAD`, or with `--update-refs=head` only
//! `HEAD`) are replayed oldest first with a three-way tree merge each,
//! like `git replay`; merges among them are refused. A commit that becomes
//! empty is dropped, kept or ends the command (`--empty`); commits that
//! were empty to begin with are kept. The refs are updated in one
//! transaction (`<cmd>: updating <commit>`, `drop: dropping <commit>`), or
//! printed as `update-ref --stdin` commands with `--dry-run`: in the order
//! their commits were replayed, the rewritten commit last, per commit
//! `HEAD` first and the branches in descending name order. When `HEAD`
//! moves to a different tree (`drop`, `fixup`), the index and work tree
//! follow with a two-way merge that keeps local changes. No hooks run.

use std::collections::HashMap;
use std::io::Write;

use rgit_hash::ObjectId;
use rgit_ident::ident::Role;
use rgit_object::{Commit, ObjectKind, Signature};
use rgit_refs::{RefUpdate, Transaction};

use crate::interactive::patch::{self, Mode as PatchMode};
use crate::messages::history as msg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, UpdateHooks};
use crate::repository::Repo;
use crate::sequencer::ident;

#[derive(Clone, Copy, PartialEq, Eq)]
enum UpdateRefs {
    Branches,
    Head,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Empty {
    Drop,
    Keep,
    Abort,
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let args = &argv[1.min(argv.len())..];
    let mut top = Parser::new(msg::USAGE, vec![]);
    // `-h` alone works outside a repository; everything else needs one.
    if args.len() == 1 && matches!(args[0].as_str(), "-h" | "--help-all") {
        top.parse(&["history".to_owned(), args[0].clone()])?;
    }
    let mut repo = Repo::open(ctx, crate::repo::Mode::Required)?;
    match args.first().map(String::as_str) {
        Some(sub @ ("reword" | "fixup" | "drop" | "split")) => subcommand(ctx, &mut repo, sub, args),
        Some(s) if !s.starts_with('-') => {
            Err(top.error_with_usage(&msg::unknown_subcommand(s)).into())
        }
        Some(s) => {
            top.parse(&["history".to_owned(), s.to_owned()])?;
            Err(top.error_with_usage(msg::NEED_SUBCOMMAND).into())
        }
        None => Err(top.error_with_usage(msg::NEED_SUBCOMMAND).into()),
    }
}

/// An `error: <message>` with git's exit status 255.
fn error(message: impl Into<String>) -> Die {
    Die::Error(message.into(), 255)
}

fn subcommand(ctx: &mut Context, repo: &mut Repo, sub: &str, argv: &[String]) -> CmdResult {
    let update_refs = Cell::new(UpdateRefs::Branches);
    let dry_run = Cell::new(false);
    let reedit = Cell::new(false);
    let empty = Cell::new(Empty::Drop);
    let usage = match sub {
        "reword" => msg::USAGE_REWORD,
        "fixup" => msg::USAGE_FIXUP,
        "drop" => msg::USAGE_DROP,
        _ => msg::USAGE_SPLIT,
    };
    let mut opts = vec![
        Opt::callback(
            None,
            "update-refs",
            Some(msg::ARGH_UPDATE_REFS),
            if sub == "split" {
                msg::H_UPDATE_REFS_SPLIT
            } else {
                msg::H_UPDATE_REFS
            },
            |v, _| {
                update_refs.set(match v {
                    Some("branches") => UpdateRefs::Branches,
                    Some("head") => UpdateRefs::Head,
                    _ => return Err(CallbackError::Error(msg::BAD_UPDATE_REFS.into())),
                });
                Ok(())
            },
        )
        .literal_argh()
        .noneg(),
        Opt::flag(Some('n'), "dry-run", &dry_run, msg::H_DRY_RUN),
    ];
    if sub == "fixup" {
        opts.push(Opt::flag(None, "reedit-message", &reedit, msg::H_REEDIT));
    }
    if sub == "fixup" || sub == "drop" {
        opts.push(
            Opt::callback(
                None,
                "empty",
                Some(msg::ARGH_EMPTY),
                if sub == "fixup" {
                    msg::H_EMPTY_FIXUP
                } else {
                    msg::H_EMPTY_DROP
                },
                |v, _| {
                    empty.set(match v.unwrap_or_default() {
                        "drop" => Empty::Drop,
                        "keep" => Empty::Keep,
                        "abort" => Empty::Abort,
                        other => return Err(CallbackError::Fatal(msg::bad_empty(other))),
                    });
                    Ok(())
                },
            )
            .literal_argh()
            .noneg(),
        );
    }
    let mut parser = Parser::new(&[usage], opts);
    let args = parser.parse(argv)?.args;
    drop(parser);
    let opts = Opts {
        update_refs: update_refs.get(),
        dry_run: dry_run.get(),
        reedit: reedit.get(),
        empty: empty.get(),
    };
    if sub == "split" {
        let Some(rev) = args.first() else {
            return Err(error(msg::NEED_COMMITTISH));
        };
        return split(ctx, repo, rev, &args[1..], &opts);
    }
    if args.len() != 1 {
        return Err(error(msg::SINGLE_REVISION));
    }
    let rev = args[0].as_str();
    match sub {
        "reword" => reword(ctx, repo, rev, &opts),
        "fixup" => fixup(ctx, repo, rev, &opts),
        _ => drop_commit(ctx, repo, rev, &opts),
    }
}

struct Opts {
    update_refs: UpdateRefs,
    dry_run: bool,
    reedit: bool,
    empty: Empty,
}

/// The commit `rev` names.
fn resolve_commit(repo: &mut Repo, rev: &str) -> Result<(ObjectId, Commit), Die> {
    let id = repo.resolve(rev).ok_or_else(|| error(msg::not_found(rev)))?;
    let Some(commit_id) = repo.peel_to(id, ObjectKind::Commit) else {
        if let Some(kind) = repo.object_kind(&id) {
            eprintln!("error: {}", msg::not_a_commit(&id.to_hex(), kind.name()));
        }
        return Err(error(msg::not_found(rev)));
    };
    Ok((commit_id, read_commit(repo, &commit_id)?))
}

fn bad_object(id: &ObjectId) -> Die {
    Die::Fatal(crate::messages::rev_list::bad_object(&id.to_hex()))
}

fn read_commit(repo: &Repo, id: &ObjectId) -> Result<Commit, Die> {
    let raw = repo
        .odb()
        .read(id)
        .ok()
        .flatten()
        .ok_or_else(|| bad_object(id))?;
    Commit::parse(repo.hash, &raw.data).map_err(|_| bad_object(id))
}

fn tree_of(commit: &Commit, id: &ObjectId) -> Result<ObjectId, Die> {
    commit.tree().map_err(|_| bad_object(id))
}

fn parents_of(commit: &Commit, id: &ObjectId) -> Result<Vec<ObjectId>, Die> {
    commit.parents().map_err(|_| bad_object(id))
}

/// The tree of the first parent of `commit` (the empty tree for a root).
fn parent_tree(repo: &Repo, commit: &Commit, id: &ObjectId) -> Result<ObjectId, Die> {
    match parents_of(commit, id)?.first() {
        Some(p) => tree_of(&read_commit(repo, p)?, p),
        None => Ok(repo.hash.empty_tree()),
    }
}

fn write_commit(repo: &Repo, commit: &Commit) -> Result<ObjectId, Die> {
    repo.odb()
        .write(ObjectKind::Commit, &commit.serialize())
        .map_err(|e| Die::Fatal(e.to_string()))
}

/// `commit` with another tree, parents and message, committed now.
fn recommit(
    repo: &Repo,
    orig: &Commit,
    id: &ObjectId,
    tree: ObjectId,
    parents: &[ObjectId],
    message: Option<&[u8]>,
    committer: &Signature,
) -> Result<ObjectId, Die> {
    let author = orig
        .author()
        .and_then(|a| a.to_owned())
        .ok_or_else(|| bad_object(id))?;
    let encoding = orig.encoding().map(<[u8]>::to_vec);
    let commit = Commit::new(
        tree,
        parents,
        &author,
        committer,
        encoding.as_deref(),
        Vec::new(),
        message.unwrap_or(orig.message()).to_vec(),
    );
    write_commit(repo, &commit)
}

// ----------------------------------------------------------------------
// The plan: the refs to update and the descendants to replay.

/// One ref that may be updated: its name in the output and transaction,
/// and its commit.
struct Tip {
    name: String,
    id: ObjectId,
}

struct Plan {
    /// The descendants of the rewritten commit, in replay order.
    commits: Vec<(ObjectId, Commit)>,
    tips: Vec<Tip>,
    /// The commit `HEAD` points to.
    head: Option<ObjectId>,
}

fn plan(ctx: &Context, repo: &Repo, target: ObjectId, mode: UpdateRefs) -> Result<Plan, Die> {
    let head = repo.refs.resolve("HEAD");
    let head_target = repo.refs.head_target();
    let mut tips: Vec<Tip> = Vec::new();
    match mode {
        UpdateRefs::Branches => {
            if head_target.is_none() {
                if let Some(h) = head {
                    tips.push(Tip {
                        name: "HEAD".into(),
                        id: h,
                    });
                }
            }
            let mut branches = repo.refs.list("refs/heads/");
            branches.sort_by(|a, b| b.0.cmp(&a.0));
            for (name, id) in branches {
                tips.push(Tip { name, id });
            }
        }
        UpdateRefs::Head => {
            if let Some(h) = head {
                // The rewritten commit itself is updated through `HEAD`,
                // its descendants through the branch `HEAD` is on.
                let name = if h == target {
                    "HEAD".to_owned()
                } else {
                    head_target.clone().unwrap_or_else(|| "HEAD".into())
                };
                tips.push(Tip { name, id: h });
            }
        }
    }
    let mut commits = Vec::new();
    // The walk starts from the tips in name order (as `--branches`).
    let mut bases: Vec<&Tip> = tips.iter().filter(|t| t.id != target).collect();
    bases.sort_by(|a, b| a.name.cmp(&b.name));
    if !bases.is_empty() {
        let mut walk_args: Vec<String> = vec![
            "--reverse".into(),
            "--topo-order".into(),
            format!("--ancestry-path={}", target.to_hex()),
        ];
        let mut seen = std::collections::HashSet::new();
        for t in &bases {
            if seen.insert(t.id) {
                walk_args.push(t.id.to_hex());
            }
        }
        walk_args.push(format!("^{}", target.to_hex()));
        for id in crate::history::walk_commits(ctx, &walk_args)? {
            let c = read_commit(repo, &id)?;
            commits.push((id, c));
        }
    }
    if mode == UpdateRefs::Head {
        if let Some(h) = head {
            if h != target && !commits.iter().any(|(id, _)| *id == h) {
                return Err(error(msg::NOT_ANCESTOR));
            }
        }
    }
    for (id, c) in &commits {
        if parents_of(c, id)?.len() > 1 {
            return Err(error(msg::MERGES_UNSUPPORTED));
        }
    }
    Ok(Plan {
        commits,
        tips,
        head,
    })
}

/// What happens to the rewritten commit.
struct Rewrite {
    target: ObjectId,
    /// Its replacement (the parent when it is dropped).
    new: ObjectId,
    empty: Empty,
}

/// Replays the descendants; the new commit of every replayed one.
fn replay(
    repo: &Repo,
    plan: &Plan,
    rw: &Rewrite,
    committer: &Signature,
) -> Result<HashMap<ObjectId, ObjectId>, Die> {
    let mut mapped: HashMap<ObjectId, ObjectId> = HashMap::new();
    mapped.insert(rw.target, rw.new);
    let merge_opts = {
        let mut o = rgit_merge::Options::default();
        crate::commands::merge_tree::apply_rename_config(&repo.config, &mut o);
        o
    };
    let mut trees: HashMap<ObjectId, ObjectId> = HashMap::new();
    let mut tree = |id: &ObjectId| -> Result<ObjectId, Die> {
        if let Some(t) = trees.get(id) {
            return Ok(*t);
        }
        let t = tree_of(&read_commit(repo, id)?, id)?;
        trees.insert(*id, t);
        Ok(t)
    };
    for (id, c) in &plan.commits {
        let parent = parents_of(c, id)?.first().copied();
        let Some(parent) = parent else {
            continue;
        };
        let new_parent = mapped.get(&parent).copied().unwrap_or(parent);
        let orig_tree = tree_of(c, id)?;
        let parent_tree = tree(&parent)?;
        let onto_tree = tree(&new_parent)?;
        let result = if onto_tree == parent_tree {
            orig_tree
        } else {
            let r = rgit_merge::merge_trees(
                repo.odb(),
                &parent_tree,
                &onto_tree,
                &orig_tree,
                &merge_opts,
            )
            .map_err(|e| Die::Fatal(e.to_string()))?;
            if !r.clean {
                return Err(error(msg::FAILED_REPLAY));
            }
            r.tree
        };
        if result == onto_tree && orig_tree != parent_tree {
            match rw.empty {
                Empty::Drop => {
                    mapped.insert(*id, new_parent);
                    continue;
                }
                Empty::Abort => {
                    eprintln!("error: {}", msg::became_empty(&id.to_hex()));
                    return Err(error(msg::FAILED_REPLAY));
                }
                Empty::Keep => {}
            }
        }
        let new = recommit(repo, c, id, result, &[new_parent], None, committer)?;
        mapped.insert(*id, new);
    }
    Ok(mapped)
}

/// The ref updates `(name, new, old)` in git's order.
fn ref_updates(
    plan: &Plan,
    rw: &Rewrite,
    mapped: &HashMap<ObjectId, ObjectId>,
) -> Vec<(String, ObjectId, ObjectId)> {
    let mut out = Vec::new();
    let order = plan
        .commits
        .iter()
        .map(|(id, _)| *id)
        .chain(std::iter::once(rw.target));
    for commit in order {
        for t in plan.tips.iter().filter(|t| t.id == commit) {
            if let Some(new) = mapped.get(&commit) {
                out.push((t.name.clone(), *new, t.id));
            }
        }
    }
    out
}

/// Prints (`--dry-run`) or applies the ref updates.
fn update_refs(
    ctx: &Context,
    repo: &Repo,
    updates: &[(String, ObjectId, ObjectId)],
    dry_run: bool,
    reflog: &str,
) -> Result<(), Die> {
    if dry_run {
        let mut out = Vec::new();
        for (name, new, old) in updates {
            out.extend_from_slice(format!("update {name} {new} {old}\n").as_bytes());
        }
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(&out);
        let _ = stdout.flush();
        return Ok(());
    }
    if updates.is_empty() {
        return Ok(());
    }
    let setup = &repo.setup;
    let config = refs_support::load_config(ctx, setup)?;
    let store = refs_support::ref_store(setup, &config);
    let objects = Objects::open(setup);
    let mut hooks = UpdateHooks::new(setup, &config, &objects);
    let mut tx = Transaction::new(&store);
    for (name, new, old) in updates {
        tx.push(RefUpdate::update(name, *new, Some(*old)).message(reflog.to_owned()))
            .map_err(|e| Die::Fatal(e.to_string()))?;
    }
    tx.commit(&mut hooks)
        .map_err(|e| Die::Fatal(e.to_string()))?;
    Ok(())
}

/// The commit `HEAD` will point to after the updates.
/// Only an update of the ref `HEAD` resolves to counts: with
/// `--update-refs=head` the rewritten tip itself is updated through the
/// name `HEAD`, and git then leaves the index and work tree alone
/// (observed).
fn new_head(repo: &Repo, plan: &Plan, updates: &[(String, ObjectId, ObjectId)]) -> Option<ObjectId> {
    let head = plan.head?;
    let target = repo.refs.head_target().unwrap_or_else(|| "HEAD".into());
    for (name, new, _) in updates {
        if *name == target {
            return Some(*new);
        }
    }
    Some(head)
}

/// The rewrite of `target` into `rw.new` with its descendants: replay,
/// work tree check (`check_worktree`), ref updates and the move of the
/// index and work tree.
#[allow(clippy::too_many_arguments)]
fn finish(
    ctx: &Context,
    repo: &Repo,
    plan: &Plan,
    rw: &Rewrite,
    committer: &Signature,
    opts: &Opts,
    reflog: &str,
    check_worktree: Option<&str>,
    move_worktree: bool,
) -> CmdResult {
    let mapped = replay(repo, plan, rw, committer)?;
    let updates = ref_updates(plan, rw, &mapped);
    let moves = match (plan.head, new_head(repo, plan, &updates)) {
        (Some(old), Some(new)) if move_worktree && repo.setup.work_tree.is_some() => {
            let old_tree = tree_of(&read_commit(repo, &old)?, &old)?;
            let new_tree = tree_of(&read_commit(repo, &new)?, &new)?;
            (old_tree != new_tree).then_some((old_tree, new_tree, new))
        }
        _ => None,
    };
    if let (Some((old_tree, new_tree, _)), Some(complaint)) = (moves, check_worktree) {
        if !would_move(repo, old_tree, new_tree)? {
            return Err(error(complaint));
        }
    }
    update_refs(ctx, repo, &updates, opts.dry_run, reflog)?;
    if opts.dry_run {
        return Ok(0);
    }
    if let Some((old_tree, new_tree, new)) = moves {
        if crate::switch_branch::move_index(repo, old_tree, new_tree, false).is_err() {
            return Err(error(msg::worktree_failed(&new.to_hex())));
        }
    }
    Ok(0)
}

/// Whether the index and work tree can move from `old` to `new` without
/// losing local changes; prints git's refusal when not.
fn would_move(repo: &Repo, old: ObjectId, new: ObjectId) -> Result<bool, Die> {
    let work_tree = repo.work_tree()?;
    let (index, mtime) = repo.read_index()?;
    let checker = repo.checker(mtime)?;
    // No refresh: like git, a stale stat counts as a local change.
    let mut excludes = crate::commands::add::build_excludes(repo, &work_tree);
    let wt = rgit_unpack::Worktree {
        root: &work_tree,
        checker: &checker,
    };
    let mut opts = rgit_unpack::Options {
        update: true,
        show_all_errors: true,
        paths: crate::commands::read_tree::path_options(repo),
        excludes: Some(&mut excludes),
        ..rgit_unpack::Options::default()
    };
    let merge = rgit_unpack::Merge::TwoWay { old, new };
    match rgit_unpack::plan(repo.odb(), &index, &merge, Some(&wt), &mut opts) {
        Ok(_) => Ok(true),
        Err(rgit_unpack::Error::Rejected(r)) => {
            for m in r.porcelain_messages(rgit_unpack::Porcelain::Checkout) {
                eprintln!("error: {m}");
            }
            eprintln!("{}", rgit_unpack::messages::ABORTING);
            Ok(false)
        }
        Err(e) => Err(crate::commands::read_tree::unpack_failed(e)),
    }
}

// ----------------------------------------------------------------------
// The commit message.

/// Asks for a message with the editor: `message` and the template naming
/// the changes from `old` to `new`. `None` when the editor failed or the
/// message is empty (reported, with `failed` last).
fn edit_message(
    ctx: &Context,
    repo: &Repo,
    message: &[u8],
    what: &str,
    old: ObjectId,
    new: ObjectId,
    failed: &str,
) -> Result<Vec<u8>, Die> {
    let comment = crate::commands::commit::comment_string(repo, message)?;
    let mut file = message.to_vec();
    file.push(b'\n');
    let mut lines: Vec<Vec<u8>> = msg::instructions(what, &comment)
        .lines()
        .map(|l| l.as_bytes().to_vec())
        .collect();
    let changes = changes(repo, old, new)?;
    if !changes.is_empty() {
        lines.push(msg::CHANGES_TO_BE_COMMITTED.as_bytes().to_vec());
        lines.extend(changes);
        lines.push(Vec::new());
    }
    for l in lines {
        file.extend_from_slice(comment.as_bytes());
        if !l.is_empty() && !l.starts_with(b"\t") {
            file.push(b' ');
        }
        file.extend_from_slice(&l);
        file.push(b'\n');
    }
    let editmsg = repo.setup.cwd.join(&repo.setup.git_dir).join("COMMIT_EDITMSG");
    std::fs::write(&editmsg, &file).map_err(|e| Die::Fatal(e.to_string()))?;
    let env = rgit_config::SystemEnv::default();
    let editor = rgit_ident::editor::editor(&repo.config, &env).map_err(|p| {
        p.print();
        Die::Exit(rgit_options::EXIT_FATAL)
    })?;
    let launch = rgit_ident::editor::LaunchOptions {
        hint: rgit_ident::editor::waiting_hint_enabled(&repo.config, &env),
        env: ctx.child_env(),
        cwd: None,
    };
    if let Err(e) = rgit_ident::editor::launch(editor.as_deref(), &editmsg, &launch) {
        e.print();
        eprintln!("{}", msg::EDITOR_FAILED);
        return Err(error(failed));
    }
    let edited = std::fs::read(&editmsg).map_err(|e| Die::Fatal(e.to_string()))?;
    let cleaned = rgit_commit::message::cleanup(
        &edited,
        rgit_commit::message::Cleanup::Strip,
        &comment,
    );
    if cleaned.is_empty() {
        eprintln!("{}", msg::EMPTY_MESSAGE);
        return Err(error(failed));
    }
    Ok(cleaned)
}

/// The `\t<label><path>` lines of the changes from tree `old` to `new`
/// (no rename detection).
fn changes(repo: &Repo, old: ObjectId, new: ObjectId) -> Result<Vec<Vec<u8>>, Die> {
    use crate::messages::wt_status as ws;
    let mut a = crate::tree_changes::Flat::new();
    let mut b = crate::tree_changes::Flat::new();
    crate::tree_changes::flatten_tree(repo, &old, &mut a)?;
    crate::tree_changes::flatten_tree(repo, &new, &mut b)?;
    let quote = repo.quote_path();
    let mut out = Vec::new();
    for p in crate::tree_changes::diff(repo, &a, &b, false) {
        let label = match (p.old, p.new) {
            (None, _) => ws::LABEL_NEW,
            (_, None) => ws::LABEL_DELETED,
            _ if p.type_changed() => ws::LABEL_TYPECHANGE,
            _ => ws::LABEL_MODIFIED,
        };
        let mut line = format!("\t{label:<12}").into_bytes();
        crate::quote::write_quoted(&mut line, &p.path, quote);
        out.push(line);
    }
    Ok(out)
}

// ----------------------------------------------------------------------
// The subcommands.

fn reword(ctx: &Context, repo: &mut Repo, rev: &str, opts: &Opts) -> CmdResult {
    let (id, commit) = resolve_commit(repo, rev)?;
    let plan = plan(ctx, repo, id, opts.update_refs)?;
    let tree = tree_of(&commit, &id)?;
    let base = parent_tree(repo, &commit, &id)?;
    let message = edit_message(
        ctx,
        repo,
        commit.message(),
        msg::WHAT_REWORD,
        base,
        tree,
        msg::FAILED_REWORD,
    )?;
    let committer = ident(repo, Role::Committer)?;
    let parents = parents_of(&commit, &id)?;
    let new = recommit(repo, &commit, &id, tree, &parents, Some(&message), &committer)?;
    let rw = Rewrite {
        target: id,
        new,
        empty: Empty::Drop,
    };
    finish(
        ctx,
        repo,
        &plan,
        &rw,
        &committer,
        opts,
        &msg::reflog("reword", rev),
        None,
        false,
    )
}

fn drop_commit(ctx: &Context, repo: &mut Repo, rev: &str, opts: &Opts) -> CmdResult {
    let (id, commit) = resolve_commit(repo, rev)?;
    let parents = parents_of(&commit, &id)?;
    let parent = match parents.len() {
        0 => return Err(error(msg::drop_root(rev))),
        1 => parents[0],
        _ => return Err(error(msg::drop_merge(rev))),
    };
    let plan = plan(ctx, repo, id, opts.update_refs)?;
    let committer = ident(repo, Role::Committer)?;
    let rw = Rewrite {
        target: id,
        new: parent,
        empty: opts.empty,
    };
    finish(
        ctx,
        repo,
        &plan,
        &rw,
        &committer,
        opts,
        &msg::reflog("drop", rev),
        Some(msg::DROP_OVERWRITE),
        true,
    )
}

fn fixup(ctx: &Context, repo: &mut Repo, rev: &str, opts: &Opts) -> CmdResult {
    let (id, commit) = resolve_commit(repo, rev)?;
    if repo.setup.work_tree.is_none() {
        return Err(error(msg::FIXUP_BARE));
    }
    let head = repo
        .refs
        .resolve("HEAD")
        .ok_or_else(|| error(msg::NOTHING_TO_FIXUP))?;
    let head_tree = tree_of(&read_commit(repo, &head)?, &head)?;
    let (index, _) = repo.read_index()?;
    let index_tree = crate::commands::write_tree::write_tree(repo, &index, false, None)?;
    if index_tree == head_tree {
        return Err(error(msg::NOTHING_TO_FIXUP));
    }
    let plan = plan(ctx, repo, id, opts.update_refs)?;
    let tree = tree_of(&commit, &id)?;
    let merge_opts = {
        let mut o = rgit_merge::Options::default();
        crate::commands::merge_tree::apply_rename_config(&repo.config, &mut o);
        o
    };
    let merged = rgit_merge::merge_trees(repo.odb(), &head_tree, &tree, &index_tree, &merge_opts)
        .map_err(|e| Die::Fatal(e.to_string()))?;
    if !merged.clean {
        return Err(error(msg::FIXUP_CONFLICTS));
    }
    let new_tree = merged.tree;
    let parents = parents_of(&commit, &id)?;
    let base = parent_tree(repo, &commit, &id)?;
    let committer = ident(repo, Role::Committer)?;
    let mut dropped = None;
    if new_tree == base && tree != base {
        match opts.empty {
            Empty::Abort => return Err(error(msg::fixup_empty(rev))),
            Empty::Drop => match parents.first() {
                None => return Err(error(msg::drop_root(rev))),
                Some(p) => dropped = Some(*p),
            },
            Empty::Keep => {}
        }
    }
    let new = match dropped {
        Some(p) => p,
        None => {
            let message = if opts.reedit {
                Some(edit_message(
                    ctx,
                    repo,
                    commit.message(),
                    msg::WHAT_FIXUP,
                    tree,
                    new_tree,
                    msg::FAILED_FIXUP,
                )?)
            } else {
                None
            };
            recommit(
                repo,
                &commit,
                &id,
                new_tree,
                &parents,
                message.as_deref(),
                &committer,
            )?
        }
    };
    let rw = Rewrite {
        target: id,
        new,
        empty: opts.empty,
    };
    finish(
        ctx,
        repo,
        &plan,
        &rw,
        &committer,
        opts,
        &msg::reflog("fixup", rev),
        None,
        true,
    )
}

fn split(ctx: &Context, repo: &mut Repo, rev: &str, pathspec: &[String], opts: &Opts) -> CmdResult {
    let (id, commit) = resolve_commit(repo, rev)?;
    let parents = parents_of(&commit, &id)?;
    if parents.len() > 1 {
        return Err(error(msg::SPLIT_MERGE));
    }
    let plan = plan(ctx, repo, id, opts.update_refs)?;
    let tree = tree_of(&commit, &id)?;
    let base = parent_tree(repo, &commit, &id)?;

    // The hunks are chosen like `add -p` on a temporary index that holds
    // the parent's tree.
    let git = crate::commands::stash::child::Git::new(ctx, repo)?;
    let temp = crate::commands::stash::child::TempIndex::new(repo, "history-split-index");
    let (code, _) = git
        .cmd(&["read-tree".to_owned(), base.to_hex()])
        .index(&temp.path)
        .output()?;
    if code != 0 {
        return Err(Die::Exit(code));
    }
    let mut popts = patch::Options::new(PatchMode::Split);
    popts.rev = format!("{} {}", base.to_hex(), id.to_hex());
    popts.pathspec = pathspec.to_vec();
    popts.index_file = Some(temp.path.clone());
    popts.top_relative = repo.setup.work_tree.is_none();
    patch::run_with(&git, repo, &popts)?;
    let (code, out) = git
        .cmd(&["write-tree".to_owned()])
        .index(&temp.path)
        .output()?;
    if code != 0 {
        return Err(Die::Exit(code));
    }
    let split_tree = std::str::from_utf8(&out)
        .ok()
        .and_then(|s| ObjectId::from_hex(repo.hash, s.trim().as_bytes()).ok())
        .ok_or_else(|| Die::Fatal(msg::SPLIT_EMPTY.into()))?;
    drop(temp);
    if split_tree == base {
        return Err(error(msg::SPLIT_EMPTY));
    }
    if split_tree == tree {
        return Err(error(msg::SPLIT_ALL));
    }
    let first = edit_message(
        ctx,
        repo,
        commit.message(),
        msg::WHAT_SPLIT,
        base,
        split_tree,
        msg::FAILED_SPLIT_FIRST,
    )?;
    let second = edit_message(
        ctx,
        repo,
        commit.message(),
        msg::WHAT_SPLIT,
        split_tree,
        tree,
        msg::FAILED_SPLIT_SECOND,
    )?;
    let committer = ident(repo, Role::Committer)?;
    let split_out = recommit(
        repo,
        &commit,
        &id,
        split_tree,
        &parents,
        Some(&first),
        &committer,
    )?;
    let new = recommit(
        repo,
        &commit,
        &id,
        tree,
        &[split_out],
        Some(&second),
        &committer,
    )?;
    let rw = Rewrite {
        target: id,
        new,
        empty: Empty::Drop,
    };
    finish(
        ctx,
        repo,
        &plan,
        &rw,
        &committer,
        opts,
        &msg::reflog("split", rev),
        None,
        false,
    )
}
