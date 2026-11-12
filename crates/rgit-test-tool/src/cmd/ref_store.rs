// SPDX-License-Identifier: MIT

//! `ref-store <store> <function> <args>...` (t1405, t0600), answered by
//! `rgit-refs` (files backend, transactions, reflogs).
//!
//! Observed behaviour of the reference binary:
//! - The repository is set up first (`fatal: not a git repository` before
//!   any argument check). Then `<store>`: `main`, `worktree:main` or
//!   `worktree:<id>`; anything else is `unknown backend`.
//! - Missing arguments die with `<what> required` (exit 128).
//! - Flag arguments are `0` or a comma separated list of `REF_*` names.
//! - `for-each-ref <prefix>` prints `<oid> <name without prefix> 0x<flags>`
//!   for every reference below the prefix, broken ones with the null id;
//!   flags: 0x1 symbolic, 0x2 packed, 0x4 broken, 0x8 bad name, 0x40 the
//!   packed entry knows its peeled value. The prefix must be empty or end
//!   in `/`.
//! - `resolve-ref <name> 0` prints `<oid> <resolved name> 0x<flags>`; a
//!   missing reference gives the null id and the name (exit 0), a broken
//!   or invalid one the null id and `(null)` (exit 1).
//! - `for-each-reflog-ent[-reverse] <name>` prints the parsable lines of the
//!   log as git writes them; without a log the exit code is 255.

use std::io::{self, Write};
use std::path::PathBuf;

use rgit_config::{ConfigSet, SystemEnv};
use rgit_hash::{HashKind, ObjectId};
use rgit_ident::ident::{Role, Strictness, ident_from};
use rgit_refs::files::{FilesOptions, FilesStore};
use rgit_refs::name::{RefNameFlags, is_per_worktree, is_root_ref_syntax, is_valid_refname};
use rgit_refs::reflog::{Committer, Entry, FilesReflog, ReflogWriter};
use rgit_refs::{
    IterItem, IterOptions, LogAllRefUpdates, NoHooks, Peeled, PreparedTransaction, RefHooks,
    RefStore, RefUpdate, Source, Target, Transaction, TransactionError,
};
use rgit_repo::{Setup, SetupOptions, StandardProtectedConfig};

use crate::messages;

const REF_ISSYMREF: u32 = 0x1;
const REF_ISPACKED: u32 = 0x2;
const REF_ISBROKEN: u32 = 0x4;
const REF_BAD_NAME: u32 = 0x8;
const REF_KNOWS_PEELED: u32 = 0x40;

/// Transaction flags accepted by the write functions.
#[derive(Default, Clone, Copy)]
struct TxFlags {
    no_deref: bool,
    force_create_reflog: bool,
    skip_refname_verification: bool,
}

/// Dies like the reference: `fatal: <msg>`, exit 128.
struct Fatal(String);

type Res = Result<i32, Fatal>;

/// The references of one worktree: per-worktree names (`HEAD`,
/// `refs/bisect/...`) in its own directory, the rest in the common
/// directory. For the main worktree both are the same store.
struct Split {
    common: FilesStore,
    own: Option<FilesStore>,
}

impl Split {
    fn for_name(&self, name: &str) -> &FilesStore {
        match &self.own {
            Some(own) if is_per_worktree(name) => own,
            _ => &self.common,
        }
    }
}

impl RefStore for Split {
    fn hash_kind(&self) -> HashKind {
        self.common.hash_kind()
    }

    fn read_raw(&self, name: &str) -> Result<Option<Target>, rgit_refs::Error> {
        self.for_name(name).read_raw(name)
    }

    fn iter(&self, options: &IterOptions) -> Result<Vec<IterItem>, rgit_refs::Error> {
        let Some(own) = &self.own else {
            return self.common.iter(options);
        };
        let mut items: Vec<IterItem> = self
            .common
            .iter(options)?
            .into_iter()
            .filter(|i| !is_per_worktree(i.name()))
            .collect();
        items.extend(
            own.iter(options)?
                .into_iter()
                .filter(|i| is_per_worktree(i.name())),
        );
        items.sort_by(|a, b| a.name().cmp(b.name()));
        Ok(items)
    }

    fn prepare<'s>(
        &'s self,
        updates: Vec<RefUpdate>,
        hooks: &mut dyn RefHooks,
    ) -> Result<Box<dyn PreparedTransaction + 's>, TransactionError> {
        let store = match updates.first() {
            Some(u) => self.for_name(&u.name),
            None => &self.common,
        };
        store.prepare(updates, hooks)
    }
}

/// The selected reference store.
struct Store {
    refs: Split,
    logs: FilesReflog,
    config: ConfigSet,
    bare: bool,
}

impl Store {
    fn kind(&self) -> HashKind {
        self.refs.hash_kind()
    }

    fn log_all(&self) -> LogAllRefUpdates {
        log_all_ref_updates(&self.config, self.bare)
    }

    fn committer(&self) -> Result<Committer, Fatal> {
        let env = SystemEnv::default();
        match ident_from(&self.config, &env, Role::Committer, Strictness::Lenient) {
            Ok(id) => Ok(Committer::new(&id.name, &id.email, id.time, id.tz)),
            Err(e) => {
                e.print();
                Err(Fatal(String::new()))
            }
        }
    }

    fn writer(&self) -> Result<ReflogWriter, Fatal> {
        Ok(ReflogWriter::new(self.logs.clone(), self.committer()?))
    }

    fn commit(&self, update: RefUpdate, hooks: &mut dyn RefHooks) -> Result<(), String> {
        let mut tx = Transaction::new(&self.refs);
        tx.push(update)
            .and_then(|()| tx.commit(hooks))
            .map_err(|e| e.to_string())
    }
}

pub fn ref_store(args: &[String]) -> i32 {
    let setup = match setup() {
        Ok(s) => s,
        Err(code) => return code,
    };
    match run(&setup, args) {
        Ok(code) => {
            let _ = io::stdout().flush();
            code
        }
        Err(Fatal(msg)) => {
            let _ = io::stdout().flush();
            if !msg.is_empty() {
                eprintln!("fatal: {msg}");
            }
            128
        }
    }
}

fn setup() -> Result<Setup, i32> {
    let env = SystemEnv::default();
    let opts = SetupOptions::from_process().map_err(report)?;
    let mut protected = StandardProtectedConfig::new(&env, rgit_config::CommandLine::default());
    let setup = rgit_repo::setup(&opts, &mut protected).map_err(report)?;
    for w in &setup.warnings {
        eprintln!("warning: {w}");
    }
    if setup.cwd != opts.cwd {
        let _ = std::env::set_current_dir(&setup.cwd);
    }
    Ok(setup)
}

fn report(e: rgit_repo::Error) -> i32 {
    let mut err = io::stderr();
    for (level, msg) in e.report() {
        let _ = writeln!(err, "{} {msg}", level.prefix());
    }
    128
}

fn load_config(setup: &Setup, git_dir: &std::path::Path) -> Result<ConfigSet, Fatal> {
    use rgit_config::sources::{self, RepoConfig, ScopeFilter};
    use rgit_config::{LoadOptions, StaticRepoFacts};

    let env = SystemEnv::default();
    let facts = StaticRepoFacts {
        git_dir: Some(git_dir.to_owned()),
        worktree: setup.work_tree.as_ref().map(|w| setup.cwd.join(w)),
        branch: None,
    };
    let opts = LoadOptions {
        env: &env,
        repo: Some(&facts),
        includes: true,
    };
    let repo = RepoConfig {
        common_dir: setup.absolute_common_dir(),
        git_dir: git_dir.to_owned(),
        worktree_config: None,
    };
    let mut set = ConfigSet::new();
    sources::load_standard(
        opts,
        Some(&repo),
        &rgit_config::CommandLine::default(),
        ScopeFilter::default(),
        &mut set,
    )
    .map_err(|e| Fatal(e.to_string()))?;
    Ok(set)
}

/// `core.logAllRefUpdates`, defaulting by whether the repository is bare.
fn log_all_ref_updates(config: &ConfigSet, bare: bool) -> LogAllRefUpdates {
    config
        .string("core.logallrefupdates")
        .ok()
        .flatten()
        .and_then(|v| LogAllRefUpdates::parse(&v.to_string()))
        .unwrap_or(LogAllRefUpdates::default_for(bare))
}

fn open_store(setup: &Setup, spec: &str) -> Result<Store, Fatal> {
    let common = setup.absolute_common_dir();
    let own = setup.absolute_git_dir();
    // The files store has one directory for all references; per-worktree
    // references of a linked worktree are not told apart (#104).
    let git_dir: PathBuf = match spec {
        "main" => own,
        "worktree:main" => common.clone(),
        _ => match spec.strip_prefix("worktree:") {
            Some(id) => {
                let dir = common.join("worktrees").join(id);
                if id.is_empty() || !dir.is_dir() {
                    return Err(Fatal(messages::no_such_worktree(id)));
                }
                dir
            }
            None if spec.starts_with("submodule:") => {
                return Err(Fatal(messages::NO_REF_STORE.to_owned()));
            }
            None => return Err(Fatal(messages::unknown_backend(spec))),
        },
    };
    let config = load_config(setup, &git_dir)?;
    let bare = setup.is_bare();
    let kind = setup.format.object_format;
    let log_all = log_all_ref_updates(&config, bare);
    let options = FilesOptions {
        log_all_ref_updates: log_all,
        ..FilesOptions::default()
    };
    let own = (git_dir != common).then(|| FilesStore::with_options(&git_dir, kind, options));
    let refs = Split {
        common: FilesStore::with_options(&common, kind, options),
        own,
    };
    Ok(Store {
        refs,
        logs: FilesReflog::new(&git_dir, &common, kind),
        config,
        bare,
    })
}

fn arg<'a>(args: &'a [String], i: usize, what: &str) -> Result<&'a str, Fatal> {
    args.get(i)
        .map(String::as_str)
        .ok_or_else(|| Fatal(messages::required(what)))
}

fn tx_flags(text: &str) -> Result<TxFlags, Fatal> {
    let mut f = TxFlags::default();
    if text == "0" {
        return Ok(f);
    }
    for word in text.split(',') {
        match word {
            "REF_NO_DEREF" => f.no_deref = true,
            "REF_FORCE_CREATE_REFLOG" => f.force_create_reflog = true,
            "REF_SKIP_REFNAME_VERIFICATION" => f.skip_refname_verification = true,
            "REF_SKIP_OID_VERIFICATION" | "REF_SKIP_CREATE_REFLOG" => {}
            _ => return Err(Fatal(messages::unknown_flag(word))),
        }
    }
    Ok(f)
}

fn resolve_flags(text: &str) -> Result<(), Fatal> {
    if text == "0" {
        Ok(())
    } else {
        let first = text.split(',').next().unwrap_or("");
        Err(Fatal(messages::unknown_flag(first)))
    }
}

fn parse_oid(text: &str, kind: HashKind) -> Result<ObjectId, Fatal> {
    ObjectId::from_hex(kind, text.as_bytes()).map_err(|_| Fatal(messages::cannot_parse_sha1(text)))
}

fn run(setup: &Setup, args: &[String]) -> Res {
    let spec = arg(args, 0, "ref store")?;
    let func = arg(args, 1, "ref function")?;
    let store = open_store(setup, spec)?;
    let a = &args[2..];
    match func {
        "create-symref" => create_symref(&store, a),
        "delete-refs" => delete_refs(&store, a),
        "rename-ref" => rename_ref(&store, a),
        "for-each-ref" => for_each_ref(&store, a),
        "resolve-ref" => resolve_ref(&store, a),
        "verify-ref" => {
            arg(a, 0, "refname")?;
            Ok(0)
        }
        "for-each-reflog" => for_each_reflog(&store),
        "for-each-reflog-ent" => reflog_ent(&store, a, false),
        "for-each-reflog-ent-reverse" => reflog_ent(&store, a, true),
        "reflog-exists" => Ok(i32::from(!store.logs.exists(arg(a, 0, "refname")?))),
        "create-reflog" => {
            let name = arg(a, 0, "refname")?;
            match store.logs.create(name) {
                Ok(()) => Ok(0),
                Err(e) => {
                    eprintln!("error: {e}");
                    Ok(1)
                }
            }
        }
        "delete-reflog" => {
            let name = arg(a, 0, "refname")?;
            let _ = store.logs.delete(name);
            prune_log_dirs(&store, name);
            Ok(0)
        }
        "update-ref" => update_ref(&store, a),
        "delete-ref" => delete_ref(&store, a),
        _ => Err(Fatal(messages::unknown_function(func))),
    }
}

fn create_symref(store: &Store, a: &[String]) -> Res {
    let name = arg(a, 0, "refname")?;
    let target = arg(a, 1, "target")?;
    let msg = a.get(2).map(String::as_str).unwrap_or("");
    let update = RefUpdate::symref_update(name, target, None)
        .no_deref(true)
        .skip_name_check(true)
        .message(msg);
    // The reference writes no reflog entry when the new target does not
    // resolve to an object.
    let resolves = matches!(store.refs.resolve_oid(target), Ok(Some(_)));
    let result = if resolves {
        store.commit(update, &mut store.writer()?)
    } else {
        store.commit(update, &mut NoHooks)
    };
    match result {
        Ok(()) => Ok(0),
        Err(e) => {
            eprintln!("error: {e}");
            Ok(255)
        }
    }
}

fn delete_refs(store: &Store, a: &[String]) -> Res {
    let flags = tx_flags(arg(a, 0, "flags")?)?;
    let msg = arg(a, 1, "msg")?;
    let names = &a[2..];
    if names.is_empty() {
        return Ok(0);
    }
    let mut hooks = store.writer()?;
    let mut tx = Transaction::new(&store.refs);
    let mut failed = false;
    for name in names {
        let update = RefUpdate::delete(name.as_str(), store.kind(), None)
            .no_deref(flags.no_deref)
            .skip_name_check(flags.skip_refname_verification)
            .message(msg);
        if let Err(e) = tx.push(update) {
            eprintln!("{}", messages::could_not_delete(name, &e.to_string()));
            failed = true;
        }
    }
    if let Err(e) = tx.commit(&mut hooks) {
        eprintln!("error: {e}");
        failed = true;
    }
    Ok(if failed { 255 } else { 0 })
}

fn rename_ref(store: &Store, a: &[String]) -> Res {
    let old = arg(a, 0, "oldref")?;
    let new = arg(a, 1, "newref")?;
    let msg = a.get(2).map(String::as_str).unwrap_or("");
    let oid = match store.refs.read_raw(old) {
        Ok(Some(Target::Object(oid))) => oid,
        Ok(Some(Target::Symbolic(_))) => {
            eprintln!("error: {}", messages::rename_symref(old));
            return Ok(255);
        }
        _ => {
            eprintln!("error: {}", messages::refname_not_found(old));
            return Ok(255);
        }
    };
    let saved = store.logs.read_raw(old).ok().flatten();
    let mut hooks = store.writer()?;
    let delete = RefUpdate::delete(old, store.kind(), Some(oid))
        .no_deref(true)
        .message(msg);
    if let Err(e) = store.commit(delete, &mut hooks) {
        eprintln!("error: {e}");
        return Ok(255);
    }
    let create = RefUpdate::create(new, oid).no_deref(true);
    if let Err(e) = store.commit(create, &mut NoHooks) {
        eprintln!("error: {e}");
        return Ok(255);
    }
    let had_log = saved.is_some();
    if let Some(data) = saved
        && let Err(e) = store.logs.append_line(new, &data, true)
    {
        eprintln!("error: {e}");
        return Ok(255);
    }
    let create_log = had_log || store.log_all().should_autocreate(new);
    let committer = store.committer()?;
    if let Err(e) = store
        .logs
        .log_update(new, oid, oid, &committer, msg.as_bytes(), create_log)
    {
        eprintln!("error: {e}");
        return Ok(255);
    }
    Ok(0)
}

fn for_each_ref(store: &Store, a: &[String]) -> Res {
    let prefix = arg(a, 0, "prefix")?;
    if !prefix.is_empty() && !prefix.ends_with('/') {
        eprintln!("{}", messages::BUG_TRAILING_SLASH);
        return Ok(99);
    }
    let items = store
        .refs
        .iter(&IterOptions::prefix(prefix))
        .map_err(|e| Fatal(e.to_string()))?;
    let null = ObjectId::null(store.kind());
    let mut out = io::stdout().lock();
    for item in items {
        let (oid, flags) = match &item {
            IterItem::Ref(r) => {
                let Some(oid) = r.oid else { continue };
                let mut flags = 0;
                if matches!(r.target, Target::Symbolic(_)) {
                    flags |= REF_ISSYMREF;
                    if packed_only(store, &r.name) {
                        flags |= REF_ISPACKED;
                    }
                } else if r.source == Source::Packed {
                    flags |= REF_ISPACKED;
                    if r.peeled != Peeled::Unknown {
                        flags |= REF_KNOWS_PEELED;
                    }
                }
                (oid, flags)
            }
            IterItem::Broken(_) => (null, REF_ISBROKEN),
            IterItem::BadName(_) => (null, REF_ISBROKEN | REF_BAD_NAME),
        };
        let name = item.name();
        let short = name.strip_prefix(prefix).unwrap_or(name);
        let _ = writeln!(out, "{oid} {short} 0x{flags:x}");
    }
    Ok(0)
}

/// Whether the reference `name` finally resolves through a packed entry.
fn packed_only(store: &Store, name: &str) -> bool {
    let Ok(Some(res)) = store.refs.resolve(name) else {
        return false;
    };
    let last = res.final_name();
    matches!(store.refs.for_name(last).read_loose_ref(last), Ok(None))
        && store
            .refs
            .common
            .packed_refs()
            .is_ok_and(|p| p.find(last).is_some())
}

fn valid_name(name: &str) -> bool {
    if name.starts_with("refs/") {
        is_valid_refname(name.as_bytes(), RefNameFlags::default())
    } else {
        is_root_ref_syntax(name) && is_valid_refname(name.as_bytes(), RefNameFlags::ONELEVEL)
    }
}

fn resolve_ref(store: &Store, a: &[String]) -> Res {
    let name = arg(a, 0, "refname")?;
    resolve_flags(arg(a, 1, "resolve-flags")?)?;
    let null = ObjectId::null(store.kind());
    let mut out = io::stdout().lock();
    if !valid_name(name) {
        let _ = writeln!(out, "{null} (null) 0x0");
        return Ok(1);
    }
    match store.refs.resolve(name) {
        Ok(None) => {
            let _ = writeln!(out, "{null} {name} 0x0");
            Ok(0)
        }
        Ok(Some(res)) => {
            let mut flags = 0;
            if res.is_symbolic() {
                flags |= REF_ISSYMREF;
            }
            if res.oid.is_some() && packed_only(store, name) {
                flags |= REF_ISPACKED;
            }
            let oid = res.oid.unwrap_or(null);
            let _ = writeln!(out, "{oid} {} 0x{flags:x}", res.final_name());
            Ok(0)
        }
        Err(_) => {
            let _ = writeln!(out, "{null} (null) 0x{REF_ISBROKEN:x}");
            Ok(1)
        }
    }
}

fn for_each_reflog(store: &Store) -> Res {
    let mut names = store.logs.list().map_err(|e| Fatal(e.to_string()))?;
    names.sort();
    names.dedup();
    let mut out = io::stdout().lock();
    for n in names {
        let _ = writeln!(out, "{n}");
    }
    Ok(0)
}

fn reflog_ent(store: &Store, a: &[String], reverse: bool) -> Res {
    let name = arg(a, 0, "refname")?;
    let Ok(Some(data)) = store.logs.read_raw(name) else {
        return Ok(255);
    };
    let mut entries = Entry::parse_all(&data, store.kind());
    if reverse {
        entries.reverse();
    }
    let mut out = io::stdout().lock();
    for e in entries {
        let _ = out.write_all(&e.to_line());
    }
    Ok(0)
}

/// Removes the directories below `logs/refs/` that deleting the log of
/// `name` left empty, as the reference does.
fn prune_log_dirs(store: &Store, name: &str) {
    let path = store.logs.path(name);
    let mut dir = path.parent();
    while let Some(d) = dir {
        if d.ends_with("logs/refs") || d.ends_with("logs") || std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

fn update_ref(store: &Store, a: &[String]) -> Res {
    let msg = arg(a, 0, "msg")?;
    let name = arg(a, 1, "refname")?;
    let new = arg(a, 2, "new-sha1")?;
    let old = arg(a, 3, "old-sha1")?;
    let flags = arg(a, 4, "flags")?;
    let old = parse_oid(old, store.kind())?;
    let new = parse_oid(new, store.kind())?;
    let flags = tx_flags(flags)?;
    let update = RefUpdate::update(name, new, Some(old))
        .no_deref(flags.no_deref)
        .create_reflog(flags.force_create_reflog)
        .skip_name_check(flags.skip_refname_verification)
        .message(msg);
    let mut hooks = store.writer()?;
    store
        .commit(update, &mut hooks)
        .map(|()| 0)
        .map_err(|e| Fatal(messages::update_ref_failed(name, &e)))
}

fn delete_ref(store: &Store, a: &[String]) -> Res {
    let msg = arg(a, 0, "msg")?;
    let name = arg(a, 1, "refname")?;
    let old = arg(a, 2, "old-sha1")?;
    let flags = arg(a, 3, "flags")?;
    let old = parse_oid(old, store.kind())?;
    let flags = tx_flags(flags)?;
    let update = RefUpdate::delete(name, store.kind(), Some(old))
        .no_deref(flags.no_deref)
        .skip_name_check(flags.skip_refname_verification)
        .message(msg);
    let mut hooks = store.writer()?;
    match store.commit(update, &mut hooks) {
        Ok(()) => Ok(0),
        Err(e) => {
            eprintln!("error: {e}");
            Ok(1)
        }
    }
}
