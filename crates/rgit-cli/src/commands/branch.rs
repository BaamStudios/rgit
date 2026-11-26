// SPDX-License-Identifier: MIT
//! `git branch` (Documentation/git-branch.adoc, Documentation/config/branch.adoc).
//!
//! Implemented: listing (`-a`, `-r`, `--list <pattern>`, `--show-current`,
//! `-v`, `-vv` with upstream and ahead/behind counts, `--abbrev`),
//! creating `<name> [<start>]` with a reflog entry, deleting with `-d`
//! (merged into the upstream, else into `HEAD`) and `-D`, also `-r`;
//! renaming and copying with `-m`/`-M`/`-c`/`-C` (ref, reflog and the
//! `branch.<name>` configuration section), `-u`/`--set-upstream-to` and
//! `--unset-upstream`; upstream tracking of new branches (`-t`/`--track`,
//! `--no-track`, `branch.autoSetupMerge`, `branch.autoSetupRebase`, shared
//! with `switch`/`checkout` in [`crate::tracking`]).
//!
//! Colours (`--color`, `color.branch`, `color.branch.<slot>`) and the
//! pager (`pager.branch`, on for listing) are supported.
//!
//! Not yet: `--recurse-submodules`, `--forked`, `--delete-merged`.

use std::io::Write;
use std::path::PathBuf;

use rgit_config::write as config_write;
use rgit_config::{SetRequest, ValuePattern};
use rgit_hash::ObjectId;
use rgit_object::{Commit, ObjectKind};
use rgit_pathspec::wildmatch::{WildmatchFlags, wildmatch};
use rgit_refs::files::FilesStore;
use rgit_refs::name::check_branch_name;
use rgit_refs::{IterItem, IterOptions, RefStore, RefUpdate, Target, Transaction};
use rgit_remote::{BranchConfig, Remotes, upstream_ref};
use rgit_repo::Setup;
use rgit_revwalk::walk::{CommitInfo, CommitSource, Walk, WalkOptions};

use rgit_config::color::ColorBool;

use crate::messages::branch as msg;
use crate::messages::tracking as tmsg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, Revs, UpdateHooks};
use crate::repo::{self, Mode};
use crate::tracking::{self, AutoSetup, Track};

/// What the command works on.
struct Ctx<'a> {
    setup: &'a Setup,
    config: &'a rgit_config::ConfigSet,
    advisor: rgit_messages::Advisor,
    store: FilesStore,
    objects: &'a Objects,
    revs: Revs,
    /// The branch `HEAD` points to (short name), also when unborn.
    current: Option<String>,
    quiet: bool,
    auto: AutoSetup,
}

impl Ctx<'_> {
    fn tracking_repo(&self) -> tracking::Repo<'_> {
        tracking::Repo {
            setup: self.setup,
            config: self.config,
            store: &self.store,
            revs: &self.revs,
        }
    }

    fn hooks(&self) -> UpdateHooks<'_> {
        UpdateHooks::new(self.setup, self.config, self.objects)
    }

    fn config_path(&self) -> PathBuf {
        self.setup.common_dir.join("config")
    }

    fn branch_oid(&self, name: &str) -> Option<ObjectId> {
        match self.store.read_raw(&format!("refs/heads/{name}")) {
            Ok(Some(Target::Object(oid))) => Some(oid),
            _ => None,
        }
    }

    fn worktree_path(&self) -> String {
        self.setup
            .work_tree
            .as_deref()
            .unwrap_or(&self.setup.git_dir)
            .display()
            .to_string()
    }

    /// The worktree (other than this one) that has branch `name` checked
    /// out, as git shows it.
    fn used_elsewhere(&self, name: &str) -> Option<String> {
        crate::worktrees::checked_out_elsewhere(self.setup, self.config, &format!("refs/heads/{name}"))
            .map(|p| p.display().to_string())
    }

    /// The worktree that has branch `name` checked out: this one or
    /// another.
    fn used_by(&self, name: &str) -> Option<String> {
        if self.current.as_deref() == Some(name) && self.head_attached() {
            return Some(self.worktree_path());
        }
        // A rebase here works on it (or updates it with --update-refs).
        if crate::worktrees::busy_here(self.setup, &format!("refs/heads/{name}"))
            == Some(crate::worktrees::Busy::Rebase)
        {
            return Some(self.worktree_path());
        }
        self.used_elsewhere(name)
    }

    fn abbrev(&self, oid: &ObjectId) -> String {
        self.objects.abbrev(oid, 7)
    }

    fn run_tx(&self, update: RefUpdate, hooks: &mut UpdateHooks<'_>) -> Result<(), Die> {
        let mut tx = Transaction::new(&self.store);
        tx.push(update)
            .and_then(|()| tx.commit(hooks))
            .map_err(|e| Die::Fatal(e.to_string()))
    }

    /// The upstream ref of branch `name` from its configuration.
    fn upstream_of(&self, name: &str) -> Option<String> {
        let b = BranchConfig::from_config(self.config, &rgit_bstr::to_bytes(name)).ok()?;
        let remotes = Remotes::from_config(self.config).ok()?;
        upstream_ref(&remotes, Some(&self.setup.git_dir), Some(&b))
            .ok()
            .map(|r| rgit_bstr::from_vec(r.into()))
    }

    /// Whether `HEAD` is a symbolic ref (not detached).
    fn head_attached(&self) -> bool {
        matches!(self.store.read_raw("HEAD"), Ok(Some(Target::Symbolic(_))))
    }

    fn config_set(&self, key: &[u8], value: Option<&[u8]>) -> Result<(), Die> {
        let pattern = ValuePattern::Any;
        let req = SetRequest {
            key,
            value,
            pattern: &pattern,
            all: true,
            comment: None,
        };
        match config_write::set_in_file(&self.config_path(), &Default::default(), &req) {
            Ok(()) => Ok(()),
            Err(e) if value.is_none() && e.exit_code() == 5 => Ok(()),
            Err(e) => Err(Die::Fatal(e.to_string())),
        }
    }
}

fn not_valid_name(ctx: &Ctx<'_>, name: &str) -> Die {
    crate::argv::errln(&format!("fatal: {}", msg::not_valid_branch_name(name)));
    match ctx.advisor.advise(Advice::RefSyntax, msg::HINT_REF_SYNTAX) {
        Ok(()) => Die::Exit(128),
        Err(e) => Die::Fatal(e),
    }
}

fn valid_name(name: &str) -> bool {
    name != "HEAD" && check_branch_name(name.as_bytes()).is_ok()
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let verbose = Cell::new(0);
    let quiet = Cell::new(0);
    let set_upstream: RefCell<Option<String>> = RefCell::new(None);
    let unset_upstream = Cell::new(false);
    let remotes = Cell::new(false);
    let abbrev: RefCell<Option<String>> = RefCell::new(None);
    let all = Cell::new(false);
    let delete = Cell::new(0);
    let rename = Cell::new(0);
    let copy = Cell::new(0);
    let list = Cell::new(false);
    let show_current = Cell::new(false);
    let force = Cell::new(false);
    let create_reflog = Cell::new(false);
    let edit_description = Cell::new(false);
    let omit_empty = Cell::new(false);
    let ignore_case = Cell::new(false);
    let column: RefCell<Option<String>> = RefCell::new(None);
    let format: RefCell<Option<String>> = RefCell::new(None);
    let sorts: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let no_sort = Cell::new(false);
    // Filter options in command-line order (`tag`'s numbering: 0
    // contains, 1 no-contains, 2 merged, 3 no-merged, 4 points-at).
    let specs: RefCell<Vec<(u8, Option<String>)>> = RefCell::new(Vec::new());
    let filter_opt = |long: &'static str, help: &'static str, which: u8| {
        let specs = &specs;
        Opt::callback(None, long, Some("commit"), help, move |v, _| {
            specs
                .borrow_mut()
                .push((which, Some(v.unwrap_or("HEAD").to_owned())));
            Ok(())
        })
        .lastarg_default("HEAD")
        .noneg()
    };
    let track = Cell::new(Track::Unspecified);
    let recurse = Cell::new(-1);
    let color: Cell<Option<ColorBool>> = Cell::new(None);
    // Generic options, then the branch actions (the order of `-h`).
    let mut opts = vec![
            Opt::group(msg::GROUP_GENERIC),
            Opt::verbose(&verbose, msg::HELP_VERBOSE),
            Opt::quiet(&quiet, msg::HELP_QUIET),
            Track::option(&track),
            Opt::string('u', "set-upstream-to", &set_upstream, "upstream", msg::HELP_SET_UPSTREAM),
    ];
    opts.extend([
            Opt::flag(None, "unset-upstream", &unset_upstream, msg::HELP_UNSET_UPSTREAM),
            Opt::callback(None, "color", Some("when"), crate::messages::pager::HELP_COLOR, |v, neg| {
                color.set(Some(crate::color::parse_when(v, neg)?));
                Ok(())
            })
            .optional(None),
            Opt::flag('r', "remotes", &remotes, msg::HELP_REMOTES).noneg(),
            filter_opt("contains", msg::HELP_CONTAINS, 0),
            filter_opt("no-contains", msg::HELP_NO_CONTAINS, 1),
            filter_opt("with", msg::HELP_CONTAINS, 0).hidden(),
            filter_opt("without", msg::HELP_NO_CONTAINS, 1).hidden(),
            Opt::string(None, "abbrev", &abbrev, "n", msg::HELP_ABBREV).optional(Some("")),
            Opt::group(msg::GROUP_ACTIONS),
            Opt::flag('a', "all", &all, msg::HELP_ALL).noneg(),
            Opt::set_int('d', "delete", &delete, msg::HELP_DELETE, 1),
            Opt::set_int('D', None, &delete, msg::HELP_FORCE_DELETE, 2).noneg(),
            Opt::set_int('m', "move", &rename, msg::HELP_MOVE, 1),
            Opt::set_int('M', None, &rename, msg::HELP_FORCE_MOVE, 2).noneg(),
            Opt::flag(None, "omit-empty", &omit_empty, msg::HELP_OMIT_EMPTY),
            Opt::set_int('c', "copy", &copy, msg::HELP_COPY, 1),
            Opt::set_int('C', None, &copy, msg::HELP_FORCE_COPY, 2).noneg(),
            Opt::flag('l', "list", &list, msg::HELP_LIST),
            Opt::flag(None, "show-current", &show_current, msg::HELP_SHOW_CURRENT),
            Opt::flag(None, "create-reflog", &create_reflog, msg::HELP_CREATE_REFLOG),
            Opt::flag(None, "edit-description", &edit_description, msg::HELP_EDIT_DESCRIPTION),
            Opt::flag('f', "force", &force, msg::HELP_FORCE),
            filter_opt("merged", msg::HELP_MERGED, 2),
            filter_opt("no-merged", msg::HELP_NO_MERGED, 3),
            Opt::callback(None, "column", Some("style"), msg::HELP_COLUMN, |v, unset| {
                *column.borrow_mut() = Some(if unset {
                    "never".to_owned()
                } else {
                    v.unwrap_or("always").to_owned()
                });
                Ok(())
            })
            .optional(None),
            Opt::callback(None, "sort", Some("key"), msg::HELP_SORT, |v, unset| {
                if unset {
                    sorts.borrow_mut().clear();
                    no_sort.set(true);
                } else {
                    sorts.borrow_mut().push(v.unwrap_or_default().to_owned());
                }
                Ok(())
            }),
            Opt::callback(None, "points-at", Some("object"), msg::HELP_POINTS_AT, |v, unset| {
                let v = (!unset).then(|| v.unwrap_or("HEAD").to_owned());
                specs.borrow_mut().push((4, v));
                Ok(())
            }),
            Opt::flag('i', "ignore-case", &ignore_case, msg::HELP_IGNORE_CASE),
            Opt::bool(None, "recurse-submodules", &recurse, msg::HELP_RECURSE_SUBMODULES),
            Opt::string(None, "format", &format, "format", msg::HELP_FORMAT),
    ]);
    let mut parser = Parser::new(msg::USAGE, opts);
    let args = parser.parse(argv)?.args;
    let abbrev_given = abbrev.borrow().is_some();
    let abbrev_len = match abbrev.borrow().as_deref() {
        None | Some("") => 7,
        Some(v) => match v.parse::<usize>() {
            Ok(0) => 0,
            Ok(n) => n.clamp(4, 64),
            Err(_) => 7,
        },
    };
    let no_abbrev = argv.iter().any(|a| a == "--no-abbrev");

    let setup = repo::setup(ctx, Mode::Required)?;
    let config = refs_support::load_config(ctx, &setup)?;
    let advisor = ctx.advisor(&config)?;
    let auto = AutoSetup::from_config(&config)?;
    let store = refs_support::ref_store(&setup, &config);
    let objects = Objects::open(&setup);
    let revs = Revs::new(ctx, &setup)?;
    let head = store.read_raw("HEAD").ok().flatten();
    let current = match &head {
        Some(Target::Symbolic(t)) => t.strip_prefix("refs/heads/").map(str::to_owned),
        _ => None,
    };
    let specs = specs.borrow().clone();
    // An unknown `--merged` commit is fatal here (not a usage error).
    for (which, spec) in &specs {
        if let (2 | 3, Some(name)) = (which, spec) {
            if revs.resolve(name).is_none() {
                return Err(Die::Fatal(crate::messages::tag::malformed_object_name(name)));
            }
        }
    }
    let filters = crate::commands::tag::resolve_filters(&specs, &objects, &revs)?;
    let filtering = !specs.is_empty();
    let c = Ctx {
        setup: &setup,
        config: &config,
        advisor,
        store,
        objects: &objects,
        revs,
        current,
        quiet: quiet.get() > 0,
        auto,
    };

    let modes = [
        delete.get() > 0,
        rename.get() > 0,
        copy.get() > 0,
        list.get(),
        show_current.get(),
        edit_description.get(),
        set_upstream.borrow().is_some(),
        unset_upstream.get(),
    ];
    if modes.iter().filter(|m| **m).count() > 1 {
        return Err(parser.usage_error().into());
    }
    if filtering && (delete.get() > 0 || rename.get() > 0 || copy.get() > 0) {
        return Err(parser.usage_error().into());
    }
    // `--recurse-submodules` (or `submodule.recurse`) only creates.
    let creating = !modes.iter().any(|m| *m)
        && !(list.get() || filtering || format.borrow().is_some() || !sorts.borrow().is_empty())
        && !args.is_empty();
    let propagate = c
        .config
        .bool("submodule.propagatebranches")
        .ok()
        .flatten()
        .unwrap_or(false);
    // `submodule.recurse` alone only recurses where branches propagate.
    let recursing = match recurse.get() {
        1 => true,
        0 => false,
        _ => creating && propagate && rgit_submodule::settings::recurse(c.config),
    };
    if recursing && !creating {
        return Err(Die::Fatal(msg::RECURSE_ONLY_CREATE.to_owned()));
    }
    if recursing && !propagate {
        return Err(Die::Fatal(msg::PROPAGATE_BRANCHES_NEEDED.to_owned()));
    }
    if delete.get() > 0 {
        if args.is_empty() {
            return Err(Die::Fatal(msg::BRANCH_NAME_REQUIRED.to_owned()));
        }
        let names = args
            .iter()
            .map(|a| interpret_name(&c, a, remotes.get()))
            .collect::<Result<Vec<_>, _>>()?;
        return delete_branches(&c, &names, delete.get() > 1 || force.get(), remotes.get());
    }
    if show_current.get() {
        if let Some(cur) = &c.current {
            crate::argv::outln(cur);
        }
        return Ok(0);
    }
    // `@{-<n>}` and `<branch>@{upstream}` name the branch they stand for.
    let interpreted = |args: &[String]| -> Result<Vec<String>, Die> {
        args.iter().map(|a| interpret_name(&c, a, false)).collect()
    };
    if rename.get() > 0 || copy.get() > 0 {
        let is_copy = rename.get() == 0;
        let force = force.get() || if is_copy { copy.get() > 1 } else { rename.get() > 1 };
        return rename_or_copy(&c, &interpreted(&args)?, is_copy, force);
    }
    if edit_description.get() {
        return edit_branch_description(ctx, &c, &interpreted(&args)?);
    }
    if let Some(up) = set_upstream.borrow().clone() {
        return set_upstream_to(&c, &up, &interpreted(&args)?);
    }
    if unset_upstream.get() {
        return unset_upstream_of(&c, &interpreted(&args)?);
    }
    let listing = list.get()
        || filtering
        || format.borrow().is_some()
        || !sorts.borrow().is_empty();
    if !listing && !args.is_empty() {
        if remotes.get() || all.get() {
            return Err(Die::Fatal(msg::ALL_REMOTES_NO_NAME.to_owned()));
        }
        let mut args = args.clone();
        args[0] = interpret_name(&c, &args[0], false)?;
        if recursing {
            return create_recursive(ctx, &c, &args, &parser, track.get(), force.get(), create_reflog.get());
        }
        return create(&c, &args, &parser, track.get(), force.get(), create_reflog.get());
    }
    // Without `--abbrev`, `core.abbrev=no` shows full ids.
    let config_full = !abbrev_given
        && config
            .get("core.abbrev")
            .and_then(|e| e.value.as_ref())
            .is_some_and(|v| matches!(v.to_string().to_ascii_lowercase().as_str(), "no" | "false" | "off"));
    let abbrev_len = if no_abbrev || config_full { 0 } else { abbrev_len };
    crate::pager::setup_for(ctx, &config, "branch", true)?;
    let colors = if crate::color::want(&config, Some("color.branch"), color.get())? {
        Some(Colors::from_config(&config)?)
    } else {
        None
    };
    // `--sort`, else `branch.sort`; `--no-sort` drops both.
    let sort_specs: Vec<String> = if no_sort.get() || !sorts.borrow().is_empty() {
        sorts.borrow().clone()
    } else {
        config
            .get_all("branch.sort")
            .filter_map(|e| e.value.as_ref().map(|v| v.to_string()))
            .collect()
    };
    let mut col_mode = crate::column::Mode::from_config(&config, Some("branch"))?;
    if let Some(spec) = column.borrow().as_deref() {
        col_mode
            .apply(spec)
            .map_err(|t| Die::Error(crate::messages::column::unsupported_option(&t), 129))?;
        if col_mode.active() && verbose.get() > 0 {
            return Err(Die::Fatal(crate::messages::tag::incompatible(
                "--column",
                "--verbose",
            )));
        }
    }
    // `-v` lists one branch per line whatever the configuration says.
    let columns = (verbose.get() == 0 && col_mode.active()).then_some(col_mode);
    let list = List {
        columns,
        remotes: remotes.get(),
        all: all.get(),
        verbose: verbose.get(),
        abbrev: abbrev_len,
        colors,
        filters,
        keys: crate::ref_filter::parse_sort_keys(&sort_specs)?,
        format: match format.borrow().as_deref() {
            Some(f) => Some(crate::ref_filter::RefFormat::parse(f)?),
            None => None,
        },
        omit_empty: omit_empty.get(),
        icase: ignore_case.get(),
    };
    list_branches(&c, &args, &head, &list)
}

/// A branch name argument of `-d`/`-D`: `@{-<n>}` and
/// `[<branch>]@{upstream}` stand for the branch they name (`remote`:
/// below `refs/remotes/`).
fn interpret_name(c: &Ctx<'_>, arg: &str, remote: bool) -> Result<String, Die> {
    if !arg.contains("@{") {
        return Ok(arg.to_owned());
    }
    let d = c
        .revs
        .with_resolver(|r| r.dwim_ref(arg))
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let prefix = if remote { "refs/remotes/" } else { "refs/heads/" };
    let full = if d.count == 1 {
        Some(d.full_name)
    } else {
        // An upstream that does not exist (any more) still names it.
        let lower = arg.to_ascii_lowercase();
        ["@{u}", "@{upstream}"]
            .iter()
            .find_map(|s| lower.strip_suffix(s).map(|b| &arg[..b.len()]))
            .and_then(|b| match b {
                "" => c.current.clone(),
                b => Some(b.to_owned()),
            })
            .and_then(|b| c.upstream_of(&b))
    };
    Ok(match full.as_deref().and_then(|f| f.strip_prefix(prefix)) {
        Some(short) => short.to_owned(),
        None => arg.to_owned(),
    })
}

fn create(
    c: &Ctx<'_>,
    args: &[String],
    parser: &Parser<'_>,
    track: Track,
    force: bool,
    create_reflog: bool,
) -> CmdResult {
    if args.len() > 2 {
        return Err(parser.usage_error().into());
    }
    let name = &args[0];
    if !valid_name(name) {
        return Err(not_valid_name(c, name));
    }
    let refname = format!("refs/heads/{name}");
    let existing = match c.store.read_raw(&refname) {
        Ok(Some(_)) if !force => return Err(Die::Fatal(msg::already_exists(name))),
        Ok(Some(_)) => c.store.resolve_oid(&refname).ok().flatten(),
        _ => None,
    };
    if existing.is_some() {
        if let Some(path) = c.used_by(name) {
            return Err(Die::Fatal(msg::cannot_force_update_checked_out(name, &path)));
        }
    }
    // Without a start point the current branch is looked up by its full
    // name (a tag of the same name must not make it ambiguous).
    let (start, lookup) = match args.get(1) {
        Some(s) => (s.clone(), s.clone()),
        None => (
            tracking::default_start(&c.store),
            tracking::default_start_ref(&c.store),
        ),
    };
    let start = start.as_str();
    let oid = c
        .revs
        .resolve(&lookup)
        .or_else(|| merge_base_start(c, &lookup))
        .and_then(|id| c.objects.peel(&id).or(Some(id)));
    let repo = c.tracking_repo();
    let plan = tracking::plan(&repo, name, &lookup, oid.is_some(), track, &c.auto)?;
    if oid.is_none() {
        // A name that only matches a symbolic ref pointing nowhere.
        for pattern in ["", "refs/", "refs/tags/", "refs/heads/", "refs/remotes/"] {
            for suffix in ["", "/HEAD"] {
                if suffix == "/HEAD" && pattern != "refs/remotes/" {
                    continue;
                }
                let full = format!("{pattern}{lookup}{suffix}");
                if let Ok(Some(Target::Symbolic(_))) = c.store.read_raw(&full) {
                    if c.revs.resolve(&full).is_none() {
                        crate::argv::errln(&format!("warning: {}", msg::ignoring_dangling_symref(&full)));
                    }
                }
            }
        }
    }
    let oid = oid.ok_or_else(|| Die::Fatal(msg::not_valid_object_name(start)))?;
    let mut hooks = c.hooks();
    let update = match existing {
        Some(old) => RefUpdate::update(refname.as_str(), oid, Some(old)).message(msg::reset_to(start)),
        None => RefUpdate::create(refname.as_str(), oid).message(msg::created_from(start)),
    };
    c.run_tx(update.create_reflog(create_reflog), &mut hooks)?;
    if let Some(plan) = plan {
        plan.apply(&repo, name, &c.auto, c.quiet)?;
    }
    Ok(0)
}

/// `branch --recurse-submodules`: the branch in the superproject and in
/// the active submodules of its start point, after checking all of them.
fn create_recursive(
    ctx: &Context,
    c: &Ctx<'_>,
    args: &[String],
    parser: &Parser<'_>,
    track: Track,
    force: bool,
    create_reflog: bool,
) -> CmdResult {
    if args.len() > 2 {
        return Err(parser.usage_error().into());
    }
    let name = &args[0];
    if !valid_name(name) {
        return Err(not_valid_name(c, name));
    }
    if !force && c.branch_oid(name).is_some() {
        return Err(Die::Fatal(msg::already_exists(name)));
    }
    let lookup = match args.get(1) {
        Some(s) => s.clone(),
        None => tracking::default_start_ref(&c.store),
    };
    let start = args.get(1).cloned().unwrap_or_else(|| lookup.clone());
    let oid = c
        .revs
        .resolve(&lookup)
        .and_then(|id| crate::switch_branch::peel_commit(c.objects, &id))
        .ok_or_else(|| Die::Fatal(msg::not_valid_object_name(&start)))?;
    let repo = crate::repository::Repo::open(ctx, Mode::Required)?;
    let subs = crate::branch_submodules::submodules(&repo, &oid)?;
    let start_name = crate::branch_submodules::full_start_name(&c.store, &lookup);
    let sub = crate::branch_submodules::Create {
        name,
        start_name: &start_name,
        track,
        force,
        quiet: c.quiet,
    };
    crate::branch_submodules::run(&subs, &sub, true)?;
    let code = create(c, args, parser, track, force, create_reflog)?;
    crate::branch_submodules::run(&subs, &sub, false)?;
    Ok(code)
}

/// `<a>...<b>` as a start point: the single merge base of the two (a
/// missing side is `HEAD`).
fn merge_base_start(c: &Ctx<'_>, start: &str) -> Option<ObjectId> {
    let (a, b) = start.split_once("...")?;
    let side = |s: &str| {
        let s = if s.is_empty() { "HEAD" } else { s };
        c.revs.resolve(s).and_then(|id| c.objects.peel(&id).or(Some(id)))
    };
    let (a, b) = (side(a)?, side(b)?);
    let bases = c.revs.with_resolver(|r| r.merge_bases(&a, &b)).ok()?;
    match bases.as_slice() {
        [one] => Some(*one),
        _ => None,
    }
}

/// `--edit-description [<branch>]`: edits `branch.<name>.description` in
/// the editor (an empty result removes it).
fn edit_branch_description(ctx: &Context, c: &Ctx<'_>, args: &[String]) -> CmdResult {
    let name = match args {
        [] => match &c.current {
            Some(b) if c.head_attached() => b.clone(),
            _ => return Err(Die::Fatal(msg::DESCRIPTION_DETACHED.to_owned())),
        },
        [one] => one.clone(),
        _ => return Err(Die::Fatal(msg::DESCRIPTION_MANY.to_owned())),
    };
    if c.branch_oid(&name).is_none() {
        if c.current.as_deref() == Some(name.as_str()) {
            return Err(Die::Error(msg::no_commit_on_branch(&name), 1));
        }
        return Err(Die::Error(msg::no_branch_named(&name), 1));
    }
    let key = crate::tracking::branch_key(&name, "description");
    let comment = String::from_utf8_lossy(&crate::comment::comment(c.config)?.or_hash()).into_owned();
    let mut text = match c.config.get(&key).and_then(|e| e.value.clone()) {
        Some(v) => v.to_vec(),
        None => b"\n".to_vec(),
    };
    if !text.ends_with(b"\n") {
        text.push(b'\n');
    }
    for line in msg::description_template(&name, &comment).lines() {
        text.extend_from_slice(comment.as_bytes());
        if !line.is_empty() {
            text.push(b' ');
        }
        text.extend_from_slice(line.as_bytes());
        text.push(b'\n');
    }
    let path = c.setup.git_dir.join("EDIT_DESCRIPTION");
    std::fs::write(&path, &text).map_err(|e| Die::Fatal(e.to_string()))?;
    let env = rgit_config::SystemEnv::default();
    let editor = rgit_ident::editor::editor(c.config, &env).map_err(|p| {
        p.print();
        Die::Exit(rgit_options::EXIT_FATAL)
    })?;
    let opts = rgit_ident::editor::LaunchOptions {
        hint: c
            .advisor
            .enabled(Advice::WaitingForEditor)
            .map_err(Die::Fatal)?,
        env: ctx.child_env(),
        cwd: None,
    };
    if let Err(e) = rgit_ident::editor::launch(editor.as_deref(), &path, &opts) {
        e.print();
        return Err(Die::Exit(1));
    }
    let edited = std::fs::read(&path).map_err(|e| Die::Fatal(e.to_string()))?;
    let cleaned = rgit_commit::message::stripspace(&edited, Some(comment.as_bytes()));
    if cleaned.is_empty() {
        c.config_set(&key, None)?;
    } else {
        c.config_set(&key, Some(&cleaned))?;
    }
    Ok(0)
}

fn delete_branches(c: &Ctx<'_>, args: &[String], force: bool, remote: bool) -> CmdResult {
    let hash = c.setup.format.object_format;
    let head_oid = c.revs.resolve("HEAD");
    let mut code = 0;
    for name in args {
        let refname = if remote {
            format!("refs/remotes/{name}")
        } else {
            format!("refs/heads/{name}")
        };
        // Checked out here, or a rebase or bisect here works on it.
        let busy = (!remote)
            .then(|| crate::worktrees::busy_here(c.setup, &refname))
            .flatten();
        if busy == Some(crate::worktrees::Busy::Bisect) && c.current.as_deref() != Some(name.as_str()) {
            crate::argv::errln(&format!("error: {}", msg::cannot_delete_for_bisect(name, &c.worktree_path())));
            code = 1;
            continue;
        }
        if !remote && ((c.current.as_deref() == Some(name.as_str()) && !c.setup.is_bare()) || busy.is_some()) {
            crate::argv::errln(&format!("error: {}", msg::cannot_delete_checked_out(name, &c.worktree_path())));
            code = 1;
            continue;
        }
        let oid = match c.store.read_raw(&refname) {
            Ok(Some(Target::Object(oid))) => oid,
            // A symbolic reference goes by itself, whatever it points to.
            Ok(Some(Target::Symbolic(target))) if !remote => {
                let mut hooks = c.hooks();
                let mut tx = Transaction::new(&c.store);
                let r = tx
                    .push(RefUpdate::symref_delete(refname.as_str(), hash, None).no_deref(true))
                    .and_then(|()| tx.commit(&mut hooks));
                if let Err(e) = r {
                    crate::argv::errln(&format!("error: {e}"));
                    code = 1;
                } else if !c.quiet {
                    crate::argv::outln(&msg::deleted(name, &target));
                }
                continue;
            }
            _ => {
                if remote {
                    crate::argv::errln(&format!("error: {}", msg::remote_not_found(name)));
                } else {
                    crate::argv::errln(&format!("error: {}", msg::not_found(name)));
                }
                code = 1;
                continue;
            }
        };
        if let Some(path) = (!remote).then(|| c.used_elsewhere(name)).flatten() {
            let bisecting = crate::worktrees::list(c.setup, false).iter().any(|w| {
                w.path.display().to_string() == path
                    && std::fs::read_to_string(w.git_dir.join("BISECT_START"))
                        .is_ok_and(|s| s.trim_end() == name.as_str())
            });
            if bisecting {
                crate::argv::errln(&format!("error: {}", msg::cannot_delete_for_bisect(name, &path)));
                code = 1;
                continue;
            }
            crate::argv::errln(&format!("error: {}", msg::cannot_delete_checked_out(name, &path)));
            code = 1;
            continue;
        }
        if !force && !remote && !merged_enough(c, name, &oid, head_oid.as_ref()) {
            crate::argv::errln(&format!("error: {}", msg::not_fully_merged(name)));
            c.advisor
                .advise(Advice::ForceDeleteBranch, &msg::hint_force_delete(name))
                .map_err(Die::Fatal)?;
            code = 1;
            continue;
        }
        let mut hooks = c.hooks();
        let mut tx = Transaction::new(&c.store);
        let r = tx
            .push(RefUpdate::delete(refname.as_str(), hash, Some(oid)).no_deref(true))
            .and_then(|()| tx.commit(&mut hooks));
        if let Err(e) = r {
            crate::argv::errln(&format!("error: {e}"));
            code = 1;
            continue;
        }
        if !remote {
            let _ = config_write::rename_section_in_file(
                &c.config_path(),
                &Default::default(),
                &crate::tracking::branch_key(name, ""),
                None,
            );
        }
        if !c.quiet {
            // A reference with a bad name is shown as "broken".
            let was = if rgit_refs::name::is_valid_refname(
                &rgit_bstr::to_bytes(&refname),
                rgit_refs::name::RefNameFlags::STRICT,
            ) {
                c.abbrev(&oid)
            } else {
                "broken".to_owned()
            };
            let line = if remote {
                msg::deleted_remote(name, &was)
            } else {
                msg::deleted(name, &was)
            };
            crate::argv::outln(&line);
        }
    }
    Ok(code)
}

/// `branch -d`: the branch must be merged into its upstream, or into
/// `HEAD` when it has none. Warns when the two answers differ.
fn merged_enough(c: &Ctx<'_>, name: &str, oid: &ObjectId, head: Option<&ObjectId>) -> bool {
    let upstream = c
        .upstream_of(name)
        .and_then(|r| c.revs.resolve(&r).map(|o| (r, o)));
    let head_merged = head.is_some_and(|h| c.revs.is_ancestor(oid, h));
    let Some((up_name, up_oid)) = upstream else {
        return head_merged;
    };
    let merged = c.revs.is_ancestor(oid, &up_oid);
    if head.is_some() && merged != head_merged {
        if merged {
            crate::argv::errln(&format!("warning: {}", msg::merged_to_upstream_not_head(name, &up_name)));
        } else {
            crate::argv::errln(&format!("warning: {}", msg::merged_to_head_not_upstream(name, &up_name)));
        }
    }
    merged
}

fn rename_or_copy(c: &Ctx<'_>, args: &[String], is_copy: bool, force: bool) -> CmdResult {
    let (old, new) = match args.len() {
        0 => return Err(Die::Fatal(msg::BRANCH_NAME_REQUIRED.to_owned())),
        1 => match &c.current {
            Some(cur) => (cur.clone(), args[0].clone()),
            None if is_copy => return Err(Die::Fatal(msg::COPY_DETACHED.to_owned())),
            None => return Err(Die::Fatal(msg::RENAME_DETACHED.to_owned())),
        },
        2 => (args[0].clone(), args[1].clone()),
        _ if is_copy => return Err(Die::Fatal(msg::TOO_MANY_COPY.to_owned())),
        _ => return Err(Die::Fatal(msg::TOO_MANY_RENAME.to_owned())),
    };
    let old_ref = format!("refs/heads/{old}");
    let new_ref = format!("refs/heads/{new}");
    if !is_copy {
        if let Some(path) = crate::worktrees::rebasing(c.setup, &old_ref) {
            return Err(Die::Fatal(msg::being_rebased(&old_ref, &path.display().to_string())));
        }
    }
    let bisected = crate::worktrees::list(c.setup, false).into_iter().find(|w| {
        std::fs::read_to_string(w.git_dir.join("BISECT_START"))
            .is_ok_and(|s| s.trim_end() == old.as_str())
    });
    if let Some(w) = bisected {
        return Err(Die::Fatal(msg::being_bisected(&old_ref, &w.path.display().to_string())));
    }
    let is_current = c.current.as_deref() == Some(old.as_str());
    // Other worktrees whose HEAD is on the old branch.
    let others: Vec<crate::worktrees::Worktree> = crate::worktrees::list(c.setup, false)
        .into_iter()
        .filter(|w| !w.current && w.head_ref.as_deref() == Some(old_ref.as_str()))
        .collect();
    let oid = c.branch_oid(&old);
    if oid.is_none() {
        // An unborn branch exists as the HEAD of some worktree.
        let unborn = is_current || !others.is_empty();
        if unborn && is_copy {
            return Err(Die::Fatal(msg::no_commit_on_branch(&old)));
        }
        if !unborn {
            return Err(Die::Fatal(msg::no_branch_named(&old)));
        }
    }
    if !valid_name(&new) {
        return Err(not_valid_name(c, &new));
    }
    let target = c.branch_oid(&new);
    if target.is_some() && old != new {
        if !force {
            return Err(Die::Fatal(msg::already_exists(&new)));
        }
        let used = if c.current.as_deref() == Some(new.as_str()) {
            Some(c.worktree_path())
        } else {
            c.used_elsewhere(&new)
        };
        if let Some(path) = used {
            return Err(Die::Fatal(msg::cannot_force_update_checked_out(&new, &path)));
        }
    }

    let Some(oid) = oid else {
        // Unborn branch: only the HEADs on it change.
        if is_current {
            let mut hooks = c.hooks().without_reflogs();
            hooks.mark_symbolic("HEAD");
            c.run_tx(
                RefUpdate::symref_update("HEAD", new_ref.as_str(), None).no_deref(true),
                &mut hooks,
            )?;
        }
        if !move_other_heads(&others, &new_ref, |_| {}) {
            return Err(Die::Fatal(msg::renamed_head_not_updated(&new)));
        }
        return Ok(0);
    };
    let message = if is_copy {
        msg::copied_reflog(&old_ref, &new_ref)
    } else {
        msg::renamed_reflog(&old_ref, &new_ref)
    };
    let hash = c.setup.format.object_format;
    let hooks = c.hooks();
    let logs = hooks.reflogs().cloned();
    // A reflog that is a symbolic link is neither moved nor copied.
    if let Some(l) = logs.as_ref().filter(|_| old != new) {
        if std::fs::symlink_metadata(l.path(&old_ref)).is_ok_and(|m| m.file_type().is_symlink()) {
            crate::argv::errln(&format!("error: {}", msg::reflog_is_symlink(&old_ref)));
            let failed = if is_copy { msg::COPY_FAILED } else { msg::RENAME_FAILED };
            return Err(Die::Fatal(failed.to_owned()));
        }
    }

    if c.store.is_reftable() && old != new {
        // One table holds the whole rename (or copy): references and
        // logs, as git writes it.
        let Some(committer) = hooks.committer().cloned() else {
            return Err(Die::Fatal(
                if is_copy { msg::COPY_FAILED } else { msg::RENAME_FAILED }.to_owned(),
            ));
        };
        if let Err(e) = c
            .store
            .rename_reftable(&old_ref, &new_ref, &committer, &message, is_copy)
        {
            crate::argv::errln(&format!("error: {e}"));
            let failed = if is_copy { msg::COPY_FAILED } else { msg::RENAME_FAILED };
            return Err(Die::Fatal(failed.to_owned()));
        }
        if !is_copy && is_current {
            let mut h = c.hooks();
            h.mark_symbolic("HEAD");
            c.run_tx(
                RefUpdate::symref_update("HEAD", new_ref.as_str(), None)
                    .no_deref(true)
                    .message(message.as_str()),
                &mut h,
            )?;
        }
    } else {
        // A forced target goes away first, with its log.
        if let Some(t) = target {
            if old != new {
                let mut quiet = c.hooks();
                c.run_tx(
                    RefUpdate::delete(new_ref.as_str(), hash, Some(t)).no_deref(true),
                    &mut quiet,
                )?;
            }
        }
        if let Some(logs) = &logs {
            if is_copy {
                if old != new {
                    logs.copy(&old_ref, &new_ref)
                        .map_err(|e| Die::Fatal(e.to_string()))?;
                }
            } else {
                logs.rename(&old_ref, &new_ref)
                    .map_err(|e| Die::Fatal(e.to_string()))?;
            }
        }
        if !is_copy && old != new {
            // Deleting the old branch notes the change in HEAD's log when HEAD
            // is on it.
            let mut h = c.hooks();
            c.run_tx(
                RefUpdate::delete(old_ref.as_str(), hash, Some(oid))
                    .no_deref(true)
                    .message(message.as_str()),
                &mut h,
            )?;
            // Its log was moved above; the delete must not take it along.
        }
        if old != new {
            let mut silent = c.hooks().without_reflogs();
            // A dangling symbolic reference of the new name is overwritten.
            let dangling = matches!(c.store.read_raw(&new_ref), Ok(Some(Target::Symbolic(_))));
            let update = if dangling {
                RefUpdate::update(new_ref.as_str(), oid, None)
            } else {
                RefUpdate::create(new_ref.as_str(), oid)
            };
            c.run_tx(update.no_deref(true), &mut silent)?;
        }
        let create_log = logs
            .as_ref()
            .is_some_and(|l| l.exists(&new_ref) || l.exists(&old_ref))
            || log_all_ref_updates(c);
        hooks.log(&new_ref, oid, oid, &message, create_log);
        if !is_copy && is_current {
            let mut silent = c.hooks().without_reflogs();
            silent.mark_symbolic("HEAD");
            c.run_tx(
                RefUpdate::symref_update("HEAD", new_ref.as_str(), None).no_deref(true),
                &mut silent,
            )?;
            if old != new {
                hooks.log("HEAD", ObjectId::null(hash), oid, &message, false);
            }
        }

    }

    if !is_copy && old != new {
        // Other worktrees on the renamed branch follow it.
        let moved = move_other_heads(&others, &new_ref, |w| {
            let head = match &w.id {
                Some(id) => format!("worktrees/{id}/HEAD"),
                None => "main-worktree/HEAD".to_owned(),
            };
            hooks.log(&head, ObjectId::null(hash), oid, &message, false);
        });
        if !moved {
            return Err(Die::Fatal(msg::renamed_head_not_updated(&new)));
        }
    }

    let old_section = crate::tracking::branch_key(&old, "");
    let new_section = crate::tracking::branch_key(&new, "");
    if old != new {
        let path = c.config_path();
        let r = if is_copy {
            config_write::copy_section_in_file(
                &path,
                &Default::default(),
                &old_section,
                &new_section,
            )
        } else {
            config_write::rename_section_in_file(
                &path,
                &Default::default(),
                &old_section,
                Some(&new_section),
            )
        };
        if let Err(e) = r {
            if !matches!(
                e.edit_error(),
                Some(rgit_config::EditError::NoSuchSection(_))
            ) {
                return Err(Die::Fatal(e.to_string()));
            }
        }
    }
    Ok(0)
}

/// Points the `HEAD` of each of `others` at `new_ref` under its lock,
/// calling `done` for each one moved. Reports the ones that could not be
/// updated; false when there was one.
fn move_other_heads(
    others: &[crate::worktrees::Worktree],
    new_ref: &str,
    mut done: impl FnMut(&crate::worktrees::Worktree),
) -> bool {
    let mut ok = true;
    for w in others {
        let head = w.git_dir.join("HEAD");
        let in_stack = rgit_refs::write_head_symref(&w.git_dir, new_ref).map_err(|e| e.to_string());
        let written = match in_stack {
            Ok(true) => Ok(()),
            Err(e) => Err(e),
            Ok(false) => rgit_fs::LockFile::acquire(&head, &rgit_fs::LockOptions::new())
            .map_err(|e| rgit_refs::messages::cannot_lock("HEAD", e))
            .and_then(|mut lock| {
                lock.file_mut()
                    .write_all(&rgit_bstr::to_bytes(&format!("ref: {new_ref}\n")))
                    .and_then(|()| lock.commit())
                    .map_err(|e| e.to_string())
            }),
        };
        match written {
            Ok(()) => done(w),
            Err(e) => {
                crate::argv::errln(&format!("error: {e}"));
                crate::argv::errln(&format!("error: {}", msg::head_of_worktree_not_updated(&w.path.display().to_string())));
                ok = false;
            }
        }
    }
    ok
}

fn log_all_ref_updates(c: &Ctx<'_>) -> bool {
    match c.config.bool("core.logallrefupdates") {
        Ok(Some(v)) => v,
        _ => c.setup.work_tree.is_some(),
    }
}

/// `-u <upstream> [<branch>]`.
fn set_upstream_to(c: &Ctx<'_>, upstream: &str, args: &[String]) -> CmdResult {
    let branch = match args.len() {
        0 => match &c.current {
            Some(cur) => {
                if c.branch_oid(cur).is_none() {
                    return Err(Die::Fatal(msg::no_commit_on_branch(cur)));
                }
                cur.clone()
            }
            None => return Err(Die::Fatal(msg::set_upstream_detached(upstream))),
        },
        1 => {
            if c.branch_oid(&args[0]).is_none() {
                if c.current.as_deref() == Some(args[0].as_str()) {
                    return Err(Die::Fatal(msg::no_commit_on_branch(&args[0])));
                }
                // `-u <remote> <branch>` meant `-u <remote>/<branch>`.
                let slip = format!("{upstream}/{}", args[0]);
                let is_remote = Remotes::from_config(c.config)
                    .is_ok_and(|r| r.names().any(|n| n == upstream.as_bytes()));
                if is_remote
                    && c.store.resolve_oid(&format!("refs/remotes/{slip}")).ok().flatten().is_some()
                    && c.advisor.enabled(Advice::SetUpstreamFailure).map_err(Die::Fatal)?
                {
                    crate::argv::errln(&format!("fatal: {}", msg::SET_UPSTREAM_SINGLE_ARGUMENT));
                    c.advisor
                        .advise(Advice::SetUpstreamFailure, &msg::hint_set_upstream_slip(&slip))
                        .map_err(Die::Fatal)?;
                    return Err(Die::Exit(128));
                }
                return Err(Die::Fatal(msg::branch_does_not_exist(&args[0])));
            }
            args[0].clone()
        }
        _ => return Err(Die::Fatal(msg::TOO_MANY_SET_UPSTREAM.to_owned())),
    };

    let repo = c.tracking_repo();
    let resolves = c.revs.resolve(upstream).is_some();
    let Some(plan) = tracking::plan(&repo, &branch, upstream, resolves, Track::Direct, &c.auto)?
    else {
        return Err(Die::Fatal(tmsg::not_a_branch(upstream)));
    };
    if plan.is_own_upstream(&branch) {
        crate::argv::errln(&format!("warning: {}", tmsg::own_upstream(&branch)));
        return Ok(0);
    }
    plan.apply(&repo, &branch, &c.auto, c.quiet)?;
    Ok(0)
}

fn unset_upstream_of(c: &Ctx<'_>, args: &[String]) -> CmdResult {
    let branch = match args.len() {
        0 => match &c.current {
            Some(cur) => cur.clone(),
            None => return Err(Die::Fatal(msg::UNSET_UPSTREAM_DETACHED.to_owned())),
        },
        1 => args[0].clone(),
        _ => return Err(Die::Fatal(msg::TOO_MANY_UNSET_UPSTREAM.to_owned())),
    };
    let has = c
        .config
        .string(crate::tracking::branch_key(&branch, "merge"))
        .ok()
        .flatten()
        .is_some();
    if !has {
        return Err(Die::Fatal(msg::no_upstream_information(&branch)));
    }
    c.config_set(&crate::tracking::branch_key(&branch, "remote"), None)?;
    c.config_set(&crate::tracking::branch_key(&branch, "merge"), None)?;
    Ok(0)
}

/// A walk source over the object database.
struct Commits<'a>(&'a Objects);

impl CommitSource for Commits<'_> {
    type Error = ();
    fn load(&mut self, id: &ObjectId) -> Result<CommitInfo, ()> {
        let (kind, data) = self.0.read(id).ok_or(())?;
        if kind != ObjectKind::Commit {
            return Err(());
        }
        let commit = Commit::parse(self.0.hash(), &data).map_err(|_| ())?;
        Ok(CommitInfo {
            parents: commit.parents().map_err(|_| ())?,
            time: commit
                .committer()
                .and_then(|s| s.time())
                .map_or(0, |t| t.seconds),
        })
    }
}

/// Commits reachable from `a` but not from `b`.
fn count_only(objects: &Objects, a: &ObjectId, b: &ObjectId) -> usize {
    let mut walk = Walk::new(Commits(objects), WalkOptions::default());
    if walk.push(a, false).is_err() || walk.push(b, true).is_err() {
        return 0;
    }
    walk.collect_all().map(|v| v.len()).unwrap_or(0)
}

/// The subject of a commit: its first paragraph on one line.
fn subject(objects: &Objects, id: &ObjectId) -> String {
    let Some((_, data)) = objects.read(id) else {
        return String::new();
    };
    let text = String::from_utf8_lossy(&data);
    let body = text.split_once("\n\n").map(|(_, b)| b).unwrap_or("");
    body.lines()
        .skip_while(|l| l.trim().is_empty())
        .take_while(|l| !l.trim().is_empty())
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Short form of a full ref name for display.
fn short_ref(full: &str) -> &str {
    full.strip_prefix("refs/heads/")
        .or_else(|| full.strip_prefix("refs/remotes/"))
        .or_else(|| full.strip_prefix("refs/tags/"))
        .or_else(|| full.strip_prefix("refs/"))
        .unwrap_or(full)
}

/// ` [<upstream>: ahead 1, behind 2]` and friends.
fn tracking(
    c: &Ctx<'_>,
    branch: &str,
    oid: &ObjectId,
    verbose: i32,
    colors: Option<&Colors>,
) -> String {
    let Some(up) = c.upstream_of(branch) else {
        return String::new();
    };
    let name = match colors {
        Some(cs) => format!("{}{}{}", cs.upstream, short_ref(&up), crate::color::RESET),
        None => short_ref(&up).to_owned(),
    };
    let up_oid = match c.store.read_raw(&up) {
        Ok(Some(Target::Object(o))) => Some(o),
        Ok(Some(Target::Symbolic(_))) => c.revs.resolve(&up),
        _ => None,
    };
    let Some(up_oid) = up_oid else {
        return if verbose > 1 {
            format!("[{name}: {}] ", msg::GONE)
        } else {
            format!("[{}] ", msg::GONE)
        };
    };
    let ahead = count_only(c.objects, oid, &up_oid);
    let behind = count_only(c.objects, &up_oid, oid);
    let counts = match (ahead, behind) {
        (0, 0) => String::new(),
        (a, 0) => msg::ahead(a),
        (0, b) => msg::behind(b),
        (a, b) => msg::ahead_behind(a, b),
    };
    match (verbose > 1, counts.is_empty()) {
        (true, true) => format!("[{name}] "),
        (true, false) => format!("[{name}: {counts}] "),
        (false, true) => String::new(),
        (false, false) => format!("[{counts}] "),
    }
}

enum Line {
    Detached(ObjectId),
    Ref {
        shown: String,
        /// Below `refs/remotes/`.
        remote: bool,
        current: bool,
        /// Checked out in another worktree (at this path).
        other: Option<String>,
        branch: Option<String>,
        target: Target,
        /// The full reference name.
        full: String,
        /// The object it points to (through a symbolic reference).
        value: Option<ObjectId>,
    },
}

/// `color.branch.<slot>`.
struct Colors {
    current: String,
    local: String,
    remote: String,
    upstream: String,
    worktree: String,
}

impl Colors {
    fn from_config(config: &rgit_config::ConfigSet) -> Result<Colors, Die> {
        let slot = |name: &str, default: &str| {
            crate::color::slot(config, &format!("color.branch.{name}"), default)
        };
        // Other refs (`plain`) are not listed, but the value is checked.
        slot("plain", "")?;
        Ok(Colors {
            current: slot("current", "\x1b[32m")?,
            local: slot("local", "")?,
            remote: slot("remote", "\x1b[31m")?,
            upstream: slot("upstream", "\x1b[34m")?,
            worktree: slot("worktree", "\x1b[36m")?,
        })
    }
}

/// How to list.
struct List {
    remotes: bool,
    all: bool,
    verbose: i32,
    abbrev: usize,
    /// Colours, when colouring.
    colors: Option<Colors>,
    /// `--[no-]contains`, `--[no-]merged`, `--points-at`.
    filters: crate::commands::tag::Filters,
    /// `--sort`, `branch.sort`.
    keys: Vec<crate::ref_filter::SortKey>,
    /// `--format`.
    format: Option<crate::ref_filter::RefFormat>,
    omit_empty: bool,
    /// `-i`: patterns and sorting ignore case.
    icase: bool,
    /// `--column`, `column.branch`, `column.ui`.
    columns: Option<crate::column::Mode>,
}

impl List {
    /// Writes `lines`, in columns when asked to.
    fn emit(&self, lines: Vec<String>) -> Result<(), Die> {
        let mut out = crate::argv::stdout();
        let lines = match self.columns {
            Some(mode) => crate::column::layout(&lines, mode, crate::column::list_width(), 1, ""),
            None => lines,
        };
        for l in lines {
            writeln!(out, "{l}").map_err(|e| Die::Fatal(e.to_string()))?;
        }
        Ok(())
    }
}

impl List {
    /// Whether `oid` passes the filters (a commit for the commit filters).
    fn keeps(&self, c: &Ctx<'_>, oid: &ObjectId) -> bool {
        let f = &self.filters;
        if !f.points_at.is_empty() && !f.points_at.contains(oid) {
            return false;
        }
        if !f.by_commit() {
            return true;
        }
        let Some(commit) = c.objects.peel(oid) else {
            return false;
        };
        if c.objects.kind(&commit) != Some(ObjectKind::Commit) {
            return false;
        }
        let anc = |a: &ObjectId, d: &ObjectId| c.revs.is_ancestor(a, d);
        (f.contains.is_empty() || f.contains.iter().any(|x| anc(x, &commit)))
            && !f.no_contains.iter().any(|x| anc(x, &commit))
            && (f.merged.is_empty() || f.merged.iter().any(|m| anc(&commit, m)))
            && !f.no_merged.iter().any(|m| anc(&commit, m))
    }
}

/// "(HEAD detached at/from <name>)": the target of the last checkout in
/// the `HEAD` reflog, by name when that name still points to the commit
/// the checkout moved to.
fn detached_label(c: &Ctx<'_>, head: &ObjectId) -> String {
    let git_dir = c.setup.absolute_git_dir();
    for d in ["rebase-merge", "rebase-apply"] {
        let dir = git_dir.join(d);
        let Ok(head_name) = std::fs::read_to_string(dir.join("head-name")) else {
            continue;
        };
        let head_name = head_name.trim_end();
        let what = match head_name.strip_prefix("refs/heads/") {
            Some(b) => b.to_owned(),
            None => {
                let orig = std::fs::read_to_string(dir.join("orig-head")).unwrap_or_default();
                let orig = ObjectId::from_hex(c.setup.format.object_format, orig.trim().as_bytes());
                match orig {
                    Ok(o) => format!("detached HEAD {}", c.abbrev(&o)),
                    Err(_) => head_name.to_owned(),
                }
            }
        };
        return msg::rebasing(&what);
    }
    if let Ok(start) = std::fs::read_to_string(git_dir.join("BISECT_START")) {
        return msg::bisect_started(start.trim_end());
    }
    let found = c.revs.with_resolver(|r| {
        let log = r.repo().refs.reflog("HEAD").ok()??;
        let (e, target) = log.iter().rev().find_map(|e| {
            let rest = e.message.strip_prefix("checkout: moving from ")?;
            let pos = rest.rfind(" to ")?;
            Some((e.new, rest[pos + 4..].to_string()))
        })?;
        let by_name = r.dwim_ref(&target).ok().filter(|d| {
            d.count > 0
                && d.full_name.starts_with("refs/")
                && d.oid
                    .and_then(|o| r.peel_to_commit(&o).ok())
                    .is_some_and(|commit| commit == e)
        });
        let _ = r.take_diagnostics();
        let name = by_name.map(|d| {
            d.full_name
                .strip_prefix("refs/tags/")
                .or_else(|| d.full_name.strip_prefix("refs/remotes/"))
                .unwrap_or(&d.full_name)
                .to_string()
        });
        Some((e, name))
    });
    match found {
        Some((e, name)) => {
            let name = name.unwrap_or_else(|| c.abbrev(&e));
            if e == *head {
                msg::detached_at(&name)
            } else {
                msg::detached_from(&name)
            }
        }
        None => msg::detached_at(&c.abbrev(head)),
    }
}

enum Listing {
    Lines(Vec<Line>),
    /// `--format` output is written.
    Done,
}

/// `--sort` keys (the detached `HEAD` stays first) and `--format`.
fn sort_and_format(c: &Ctx<'_>, lines: Vec<Line>, list: &List) -> Result<Listing, Die> {
    if list.format.as_ref().is_some_and(crate::ref_filter::RefFormat::has_rest) {
        return Err(Die::Fatal(crate::messages::ref_filter::rejected_atom("rest")));
    }
    let mut lines = lines;
    if list.icase && list.keys.is_empty() {
        // `-i` sorts the names ignoring case (the detached `HEAD` first).
        lines.sort_by_key(|l| match l {
            Line::Detached(_) => (0, String::new()),
            Line::Ref { full, .. } => (1, full.to_lowercase()),
        });
    }
    if list.keys.is_empty() && list.format.is_none() {
        return Ok(Listing::Lines(lines));
    }
    let mut values = crate::ref_filter::RefValues::new(c.objects, &c.store)
        .with_repo(crate::ref_filter::RepoParts {
            setup: c.setup,
            config: c.config,
            revs: &c.revs,
        })
        .with_color(list.colors.is_some());
    values.prepare(
        list.format
            .iter()
            .flat_map(|f| f.atoms())
            .chain(list.keys.iter().map(|k| &k.atom)),
    )?;
    let (head, refs): (Vec<Line>, Vec<Line>) =
        lines.into_iter().partition(|l| matches!(l, Line::Detached(_)));
    let mut lines = head;
    if list.keys.is_empty() {
        lines.extend(refs);
    } else {
        let suffixes = if list.keys.iter().any(|k| k.version) {
            crate::ref_filter::version_suffixes(c.config)
        } else {
            Vec::new()
        };
        let mut keyed = Vec::with_capacity(refs.len());
        for l in refs {
            let Line::Ref { full, value, .. } = &l else {
                continue;
            };
            let oid = value.unwrap_or_else(|| ObjectId::null(c.setup.format.object_format));
            let mut vals = Vec::with_capacity(list.keys.len());
            for k in &list.keys {
                vals.push(values.atom_value(&k.atom, full, oid)?);
            }
            keyed.push((vals, l));
        }
        lines.extend(crate::ref_filter::sort_items(
            keyed,
            &list.keys,
            list.icase,
            &suffixes,
            |l: &Line| match l {
                Line::Ref { full, .. } => full.as_str(),
                Line::Detached(_) => "HEAD",
            },
        ));
    }
    let Some(format) = &list.format else {
        return Ok(Listing::Lines(lines));
    };
    let mut texts = Vec::new();
    for l in &lines {
        let (name, oid) = match l {
            Line::Detached(o) => (detached_label(c, o), *o),
            Line::Ref { full, value, .. } => (
                full.clone(),
                value.unwrap_or_else(|| ObjectId::null(c.setup.format.object_format)),
            ),
        };
        let text = values.render(format, &name, oid)?;
        if list.omit_empty && text.is_empty() {
            continue;
        }
        texts.push(String::from_utf8_lossy(&text).into_owned());
    }
    list.emit(texts)?;
    Ok(Listing::Done)
}

fn list_branches(c: &Ctx<'_>, patterns: &[String], head: &Option<Target>, list: &List) -> CmdResult {
    let List {
        remotes,
        all,
        verbose,
        abbrev,
        ..
    } = *list;
    let paint = |pick: fn(&Colors) -> &str, text: &str| match &list.colors {
        Some(cs) => format!("{}{text}{}", pick(cs), crate::color::RESET),
        None => text.to_owned(),
    };
    let local = !remotes || all;
    let remote = remotes || all;
    let mut lines: Vec<Line> = Vec::new();
    // A detached `HEAD` is listed unless patterns select the branches.
    if let (true, Some(Target::Object(oid)), true) = (local, head, patterns.is_empty()) {
        if list.keeps(c, oid) {
            lines.push(Line::Detached(*oid));
        }
    }
    // Branches checked out in other worktrees (git counts the `HEAD` of
    // a bare main repository too).
    let others: std::collections::HashMap<String, String> =
        crate::worktrees::list(c.setup, crate::worktrees::main_is_bare(c.setup, c.config))
            .into_iter()
            .filter(|w| !w.current)
            .filter_map(|w| Some((w.head_ref?, w.path.display().to_string())))
            .collect();
    let mut prefixes = Vec::new();
    if local {
        prefixes.push("refs/heads/");
    }
    if remote {
        prefixes.push("refs/remotes/");
    }
    for prefix in prefixes {
        let items = c
            .store
            .iter(&IterOptions::prefix(prefix))
            .map_err(|e| Die::Fatal(e.to_string()))?;
        for item in items {
            let r = match item {
                IterItem::Ref(r) => r,
                IterItem::Broken(n) => {
                    let w = rgit_refs::messages::ignoring_broken_ref(&n);
                    crate::argv::errln(&format!("warning: {w}"));
                    continue;
                }
                IterItem::BadName(n) => {
                    let w = rgit_refs::messages::ignoring_broken_name(&n);
                    crate::argv::errln(&format!("warning: {w}"));
                    continue;
                }
            };
            // A symbolic ref pointing nowhere is not listed.
            if r.oid.is_none() {
                continue;
            }
            let short = &r.name[prefix.len()..];
            let shown = if prefix == "refs/remotes/" && all {
                format!("remotes/{short}")
            } else {
                short.to_owned()
            };
            let flags = if list.icase {
                WildmatchFlags::CASEFOLD
            } else {
                WildmatchFlags::default()
            };
            if !patterns.is_empty()
                && !patterns
                    .iter()
                    .any(|p| wildmatch(&rgit_bstr::to_bytes(p), &rgit_bstr::to_bytes(&shown), flags))
            {
                continue;
            }
            let value = match &r.target {
                Target::Object(o) => Some(*o),
                Target::Symbolic(_) => r.oid,
            };
            let filtered = list.filters.by_commit() || !list.filters.points_at.is_empty();
            if filtered && !value.is_some_and(|v| list.keeps(c, &v)) {
                continue;
            }
            let is_local = prefix == "refs/heads/";
            let current = is_local && c.current.as_deref() == Some(short);
            lines.push(Line::Ref {
                remote: !is_local,
                other: (is_local && !current)
                    .then(|| others.get(&r.name).cloned())
                    .flatten(),
                current,
                branch: is_local.then(|| short.to_owned()),
                shown,
                target: r.target.clone(),
                full: r.name.clone(),
                value,
            });
        }
    }

    let lines = sort_and_format(c, lines, list)?;
    let lines = match lines {
        Listing::Lines(l) => l,
        Listing::Done => return Ok(0),
    };
    let full_hex = |o: &ObjectId| {
        if abbrev == 0 {
            o.to_hex()
        } else {
            c.objects.abbrev(o, abbrev)
        }
    };
    let width = if verbose > 0 {
        lines
            .iter()
            .map(|l| match l {
                Line::Detached(o) => detached_label(c, o).chars().count(),
                Line::Ref { shown, .. } => shown.chars().count(),
            })
            .max()
            .unwrap_or(0)
    } else {
        0
    };
    let mut texts = Vec::new();
    for line in &lines {
        let text = match line {
            Line::Detached(oid) => {
                let label = detached_label(c, oid);
                let label = format!("{label:<width$}");
                let name = paint(|cs| &cs.current, &label);
                if verbose > 0 {
                    format!("* {name} {} {}", full_hex(oid), subject(c.objects, oid))
                } else {
                    format!("* {name}")
                }
            }
            Line::Ref {
                shown,
                remote,
                current,
                other,
                branch,
                target,
                value,
                ..
            } => {
                // `-v` shows the commit a local symbolic reference points to.
                let target = match value {
                    Some(v) if verbose > 0 && !*remote => &Target::Object(*v),
                    _ => target,
                };
                let marker = if *current {
                    '*'
                } else if other.is_some() {
                    '+'
                } else {
                    ' '
                };
                let pick: fn(&Colors) -> &str = if *current {
                    |cs| &cs.current
                } else if other.is_some() {
                    |cs| &cs.worktree
                } else if *remote {
                    |cs| &cs.remote
                } else {
                    |cs| &cs.local
                };
                let name = paint(pick, &format!("{shown:<width$}"));
                match target {
                    Target::Symbolic(t) => {
                        let t = t
                            .strip_prefix("refs/remotes/")
                            .or_else(|| t.strip_prefix("refs/heads/"))
                            .unwrap_or(t);
                        format!("{marker} {name} -> {t}")
                    }
                    Target::Object(oid) if verbose > 0 => {
                        let track = branch
                            .as_deref()
                            .map(|b| tracking(c, b, oid, verbose, list.colors.as_ref()))
                            .unwrap_or_default();
                        let path = match other {
                            Some(p) if verbose > 1 => {
                                format!("({}) ", paint(|cs| &cs.worktree, p))
                            }
                            _ => String::new(),
                        };
                        format!(
                            "{marker} {name} {} {path}{track}{}",
                            full_hex(oid),
                            subject(c.objects, oid)
                        )
                    }
                    Target::Object(_) => format!("{marker} {name}"),
                }
            }
        };
        texts.push(text);
    }
    list.emit(texts)?;
    Ok(0)
}
