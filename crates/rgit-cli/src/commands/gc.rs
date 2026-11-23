// SPDX-License-Identifier: MIT
//! `git gc` (Documentation/git-gc.adoc, Documentation/config/gc.adoc).
//!
//! Runs, as separate commands like git: `pack-refs --all --prune`
//! (`gc.packRefs`), `reflog expire --all` (unless both reflog expiries
//! are `never`),
//! `worktree prune --expire <gc.worktreePruneExpire>`, `rerere gc` (once
//! rgit has `rerere`), `repack -d -l` with `-A --unpack-unreachable=<date>`
//! (`-a` for `--prune=now`), and `prune --expire <date>`. `gc.pid` keeps
//! two runs apart (`--force` overrides it).
//!
//! `--auto` does nothing unless `objects/17` suggests more than `gc.auto`
//! loose objects, or more than `gc.autoPackLimit` packs without `.keep`
//! exist; the `pre-auto-gc` hook can veto. With `gc.autoDetach` (or
//! `--detach`) the work continues in a background process whose error
//! output goes to `gc.log`; a recent `gc.log` stops later automatic runs.
//!
//! Cruft packs (`--cruft`, `gc.cruftPacks`, #96) are not written yet:
//! unreachable objects are kept loose (`repack -A`) and pruned by age.
//! The commit-graph (#22) is not written.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use rgit_config::ConfigSet;
use rgit_repo::Setup;

use crate::housekeeping::{secs, store};
use crate::messages::housekeeping as msg;
use crate::prelude::*;
use crate::repo::{self, Mode};

/// Environment variable naming the `gc.log.lock` a background run writes
/// its error output to.
const LOG_LOCK_ENV: &str = "RGIT_GC_LOG_LOCK";

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    if std::env::var_os(LOG_LOCK_ENV).is_none() {
        return gc(ctx, argv);
    }
    // A background run: everything on standard error goes to the log,
    // which is kept as gc.log only if it is not empty.
    let result = match gc(ctx, argv) {
        Err(Die::Fatal(m)) => {
            eprintln!("fatal: {m}");
            Err(Die::Exit(rgit_messages::EXIT_FATAL))
        }
        other => other,
    };
    finish_background_log();
    result
}

fn gc(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let quiet = Cell::new(false);
    let prune: RefCell<Option<String>> = RefCell::new(None);
    let no_prune = Cell::new(false);
    let cruft = Cell::new(true);
    let aggressive = Cell::new(false);
    let auto = Cell::new(false);
    let detach: Cell<Option<bool>> = Cell::new(None);
    let force = Cell::new(false);
    let keep_largest = Cell::new(false);
    let skip_foreground = Cell::new(false);
    let mut parser = Parser::new(
        msg::GC_USAGE,
        vec![
            Opt::flag('q', "quiet", &quiet, msg::GC_H_QUIET),
            Opt::callback(
                None,
                "prune",
                Some(msg::GC_ARGH_PRUNE),
                msg::GC_H_PRUNE,
                |v: Option<&str>, unset: bool| {
                    no_prune.set(unset);
                    *prune.borrow_mut() = if unset { None } else { v.map(str::to_string) };
                    Ok(())
                },
            )
            .optional(None),
            Opt::flag(None, "cruft", &cruft, msg::GC_H_CRUFT),
            Opt::callback(
                None,
                "max-cruft-size",
                Some("n"),
                msg::GC_H_MAX_CRUFT,
                |_: Option<&str>, _| Ok(()),
            )
            .noneg(),
            Opt::flag(None, "aggressive", &aggressive, msg::GC_H_AGGRESSIVE),
            Opt::flag(None, "auto", &auto, msg::GC_H_AUTO),
            Opt::callback(
                None,
                "detach",
                None,
                msg::GC_H_DETACH,
                |_: Option<&str>, unset: bool| {
                    detach.set(Some(!unset));
                    Ok(())
                },
            )
            .noarg(),
            Opt::flag(None, "force", &force, msg::GC_H_FORCE),
            Opt::flag(None, "keep-largest-pack", &keep_largest, msg::GC_H_KEEP_LARGEST),
            Opt::callback(
                None,
                "expire-to",
                Some("dir"),
                msg::GC_H_EXPIRE_TO,
                |_: Option<&str>, _| Ok(()),
            ),
            Opt::flag(
                None,
                "skip-foreground-tasks",
                &skip_foreground,
                msg::GC_H_SKIP_FOREGROUND,
            )
            .hidden(),
        ],
    );
    let args = parser.parse(argv)?.args;
    if !args.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);

    let setup = repo::setup(ctx, Mode::Required)?;
    let config = repo::load_config(ctx, &setup)?;
    let date_ctx = rgit_date::DateContext::from_env();

    // The grace period for unreachable objects.
    let prune_expire: Option<String> = if no_prune.get() {
        None
    } else if let Some(p) = prune.take() {
        rgit_date::parse_expiry_date(&p, &date_ctx)
            .map_err(|_| Die::Fatal(msg::malformed_expiration(&p)))?;
        Some(p)
    } else {
        Some(config_expiry(&config, "gc.pruneexpire", "2.weeks.ago")?)
    };
    let worktree_expire = config_expiry(&config, "gc.worktreepruneexpire", "3.months.ago")?;
    let log_expiry = config_expiry(&config, "gc.logexpiry", "1.day")?;
    let gc_auto = config.int("gc.auto").ok().flatten().unwrap_or(6700);
    let auto_pack_limit = config.int("gc.autopacklimit").ok().flatten().unwrap_or(50);
    let objects = rgit_odb::objects_dir(&setup.common_dir);
    let common = setup.absolute_common_dir();
    let quiet = quiet.get();
    let mut full_repack = true;

    if auto.get() {
        if gc_auto <= 0 {
            return Ok(0);
        }
        let packs_too_many = auto_pack_limit > 0 && count_packs(&objects) > auto_pack_limit;
        if !packs_too_many && !too_many_loose(&objects, gc_auto, setup.format.object_format) {
            return Ok(0);
        }
        full_repack = packs_too_many;
        let hooks = crate::hooks::for_repo(ctx, &setup, &config)?;
        let outcome = crate::hooks::run(&hooks, "pre-auto-gc", &Default::default())?;
        if outcome.code != 0 {
            return Ok(0);
        }
    }

    let detach = auto.get()
        && detach.get().unwrap_or_else(|| {
            config
                .bool("gc.autodetach")
                .ok()
                .flatten()
                .unwrap_or(true)
        });
    let in_background = std::env::var_os(LOG_LOCK_ENV).is_some();
    if auto.get() && !quiet && !in_background {
        let mut err = std::io::stderr().lock();
        let _ = writeln!(
            err,
            "{}",
            if detach {
                msg::GC_AUTO_BACKGROUND
            } else {
                msg::GC_AUTO_FOREGROUND
            }
        );
        let _ = writeln!(err, "{}", msg::GC_SEE_HELP);
    }
    if detach {
        // A recent gc.log from a failed background run stops automatic gc.
        let log = common.join("gc.log");
        if let Ok(meta) = std::fs::metadata(&log) {
            let mtime = secs(meta.modified());
            if log_expiry == "now" || mtime > expiry_secs(&log_expiry, &date_ctx) {
                let text = std::fs::read_to_string(&log).unwrap_or_default();
                let shown = shown_path(&setup, &log);
                eprintln!("warning: {}", msg::gc_last_run(&shown, &text));
                return Ok(0);
            }
            let _ = std::fs::remove_file(&log);
        }
    }

    let children = Children::new(ctx, &setup)?;
    let _pid_lock = match PidLock::take(&common, force.get())? {
        Ok(lock) => lock,
        Err(_) if auto.get() => return Ok(0),
        Err((host, pid)) => return Err(Die::Fatal(msg::gc_running(&host, pid))),
    };

    if !skip_foreground.get() {
        let pack_refs = match config.string("gc.packrefs").ok().flatten() {
            Some(v) if v == "notbare" => !setup.is_bare(),
            _ => config.bool("gc.packrefs").ok().flatten().unwrap_or(true),
        };
        if pack_refs {
            children.run(&["pack-refs", "--all", "--prune"])?;
        }
        let never = |key: &str| {
            config
                .string(key)
                .ok()
                .flatten()
                .is_some_and(|v| v == "never")
        };
        if !(never("gc.reflogexpire") && never("gc.reflogexpireunreachable")) {
            children.run_optional(&["reflog", "expire", "--all"])?;
        }
    }

    if detach {
        drop(_pid_lock);
        return children.daemonize(&common, quiet);
    }

    if worktree_expire != "never" {
        children.run(&["worktree", "prune", "--expire", &worktree_expire])?;
    }
    if crate::commands::find("rerere").is_some() {
        children.run(&["rerere", "gc"])?;
    }

    // extensions.preciousObjects: nothing may be deleted, so neither
    // repack nor prune runs.
    if setup.format.precious_objects {
        return Ok(0);
    }
    let mut repack: Vec<String> = vec!["repack".into(), "-d".into(), "-l".into()];
    if aggressive.get() {
        let depth = config.int("gc.aggressivedepth").ok().flatten().unwrap_or(50);
        let window = config.int("gc.aggressivewindow").ok().flatten().unwrap_or(250);
        repack.push("-f".into());
        repack.push(format!("--depth={depth}"));
        repack.push(format!("--window={window}"));
    }
    if quiet {
        repack.push("-q".into());
    }
    if full_repack {
        match prune_expire.as_deref() {
            Some("now") => repack.push("-a".into()),
            Some(e) => {
                repack.push("-A".into());
                repack.push(format!("--unpack-unreachable={e}"));
            }
            None => repack.push("-A".into()),
        }
        let odb = repo::odb_with_config(&setup, &config)?;
        let threshold = config
            .int("gc.bigpackthreshold")
            .ok()
            .flatten()
            .unwrap_or(0);
        let packs = store::local_packs(&odb, &[]);
        let size = |p: &store::LocalPack| {
            std::fs::metadata(p.pack.path()).map_or(0, |m| m.len())
        };
        if keep_largest.get() {
            if let Some(p) = packs.iter().filter(|p| !p.keep).max_by_key(|p| size(p)) {
                repack.push(format!("--keep-pack={}", p.file_name));
            }
        } else if threshold > 0 {
            for p in packs.iter().filter(|p| !p.keep && size(p) >= threshold as u64) {
                repack.push(format!("--keep-pack={}", p.file_name));
            }
        }
    } else {
        repack.push("--no-write-bitmap-index".into());
    }
    let repack: Vec<&str> = repack.iter().map(String::as_str).collect();
    children.run(&repack)?;

    if let Some(e) = prune_expire.as_deref() {
        if e != "never" {
            children.run(&["prune", "--expire", e])?;
        }
    }

    if auto.get() && too_many_loose(&objects, gc_auto, setup.format.object_format) {
        eprintln!("warning: {}", msg::GC_TOO_MANY_LOOSE);
    }
    Ok(0)
}

/// An expiry setting, checked like git (`Invalid <key>`, then fatal).
fn config_expiry(config: &ConfigSet, key: &str, default: &str) -> Result<String, Die> {
    let Some(entry) = config.get(key) else {
        return Ok(default.to_string());
    };
    let value = entry
        .string()
        .map(|s| s.to_string())
        .unwrap_or_default();
    if rgit_date::parse_expiry_date(&value, &rgit_date::DateContext::from_env()).is_err() {
        return Err(crate::comment::bad_entry(entry, &msg::invalid_config(key, &value)));
    }
    Ok(value)
}

fn expiry_secs(v: &str, ctx: &rgit_date::DateContext) -> u64 {
    rgit_date::parse_expiry_date(v, ctx).unwrap_or(0)
}

/// `path` relative to the current directory when below it (`.git/gc.log`).
fn shown_path(setup: &Setup, path: &Path) -> String {
    path.strip_prefix(&setup.cwd)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Local packs without a `.keep` file.
fn count_packs(objects: &Path) -> i64 {
    let Ok(rd) = std::fs::read_dir(objects.join("pack")) else {
        return 0;
    };
    rd.flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|n| n.strip_suffix(".idx").map(str::to_string))
        .filter(|base| {
            let dir = objects.join("pack");
            dir.join(format!("{base}.pack")).exists() && !dir.join(format!("{base}.keep")).exists()
        })
        .count() as i64
}

/// git's estimate: more than `(gc.auto + 255) / 256` loose objects in
/// `objects/17`.
fn too_many_loose(objects: &Path, gc_auto: i64, hash: rgit_hash::HashKind) -> bool {
    let limit = (gc_auto + 255) / 256;
    let n = store::fan_entries(objects, 0x17, hash)
        .iter()
        .filter(|e| e.id.is_some())
        .count() as i64;
    n > limit
}

/// The sub-commands gc runs, as the same binary.
struct Children {
    exe: PathBuf,
    env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    dir: PathBuf,
}

impl Children {
    fn new(ctx: &Context, setup: &Setup) -> Result<Children, Die> {
        let exe = std::env::current_exe().map_err(|e| Die::Fatal(e.to_string()))?;
        let mut env = ctx.child_env();
        env.retain(|(k, _)| k != "GIT_DIR");
        env.push(("GIT_DIR".into(), setup.absolute_git_dir().into_os_string()));
        if let Some(wt) = &setup.work_tree {
            env.retain(|(k, _)| k != "GIT_WORK_TREE");
            env.push(("GIT_WORK_TREE".into(), setup.cwd.join(wt).into_os_string()));
        }
        Ok(Children {
            exe,
            env,
            dir: setup.cwd.clone(),
        })
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.exe);
        cmd.args(args).current_dir(&self.dir);
        cmd.env_remove(LOG_LOCK_ENV);
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd
    }

    /// Run a step; its failure ends gc.
    fn run(&self, args: &[&str]) -> Result<(), Die> {
        let status = rgit_trace::trace2::status(
            &mut self.command(args),
            Default::default(),
            &rgit_trace::trace2::git_argv(args),
        )
        .map_err(|e| Die::Fatal(e.to_string()))?;
        if status.success() {
            Ok(())
        } else {
            Err(Die::Fatal(msg::failed_to_run(args[0])))
        }
    }

    /// Run a step that rgit may not implement yet: when it reports so, the
    /// step is skipped.
    fn run_optional(&self, args: &[&str]) -> Result<(), Die> {
        let out = rgit_trace::trace2::output(
            self.command(args).stdin(Stdio::null()),
            Default::default(),
            &rgit_trace::trace2::git_argv(args),
        )
        .map_err(|e| Die::Fatal(e.to_string()))?;
        let _ = std::io::stdout().write_all(&out.stdout);
        if out.status.success() {
            let _ = std::io::stderr().write_all(&out.stderr);
            return Ok(());
        }
        if String::from_utf8_lossy(&out.stderr).contains("is not supported yet") {
            return Ok(());
        }
        let _ = std::io::stderr().write_all(&out.stderr);
        Err(Die::Fatal(msg::failed_to_run(args[0])))
    }

    /// Continue in a background process (`gc --auto --no-detach
    /// --skip-foreground-tasks`) whose error output goes to `gc.log`.
    fn daemonize(&self, common: &Path, quiet: bool) -> CmdResult {
        let lock = common.join("gc.log.lock");
        let log = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
        {
            Ok(f) => f,
            // Another background run is writing its log.
            Err(_) => return Ok(0),
        };
        let mut args = vec!["gc", "--auto", "--no-detach", "--skip-foreground-tasks"];
        if quiet {
            args.push("--quiet");
        }
        let spawned = self
            .command(&args)
            .env(LOG_LOCK_ENV, &lock)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn();
        if spawned.is_err() {
            let _ = std::fs::remove_file(&lock);
        }
        Ok(0)
    }
}

/// At the end of a background run: its log becomes `gc.log` when
/// anything was written, and is removed otherwise.
fn finish_background_log() {
    let Some(lock) = std::env::var_os(LOG_LOCK_ENV).map(PathBuf::from) else {
        return;
    };
    let empty = std::fs::metadata(&lock).map_or(true, |m| m.len() == 0);
    if empty {
        let _ = std::fs::remove_file(&lock);
    } else if let Some(dir) = lock.parent() {
        let _ = std::fs::rename(&lock, dir.join("gc.log"));
    }
}

/// `gc.pid`: `<pid> <hostname>` of the running gc, removed when it ends.
struct PidLock {
    path: PathBuf,
}

impl PidLock {
    /// Take the lock. `Ok(Err((host, pid)))`: another gc holds it.
    fn take(common: &Path, force: bool) -> Result<Result<PidLock, (String, u32)>, Die> {
        let path = common.join("gc.pid");
        let host = hostname();
        if !force {
            if let Ok(meta) = std::fs::metadata(&path) {
                let fresh = meta
                    .modified()
                    .ok()
                    .and_then(|m| SystemTime::now().duration_since(m).ok())
                    .is_some_and(|age| age < Duration::from_secs(12 * 3600));
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                let mut parts = text.split_whitespace();
                let pid = parts.next().and_then(|p| p.parse::<u32>().ok());
                let owner = parts.next().unwrap_or("");
                if let Some(pid) = pid {
                    if fresh && owner == host && pid != std::process::id() && alive(pid) {
                        return Ok(Err((owner.to_string(), pid)));
                    }
                }
            }
        }
        let tmp = common.join("gc.pid.lock");
        let written = std::fs::write(&tmp, format!("{} {host}", std::process::id()))
            .and_then(|()| std::fs::rename(&tmp, &path));
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(Die::Fatal(e.to_string()));
        }
        Ok(Ok(PidLock { path }))
    }
}

impl Drop for PidLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(unix)]
fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for its length; gethostname writes a
    // NUL-terminated name into it (truncated if too long).
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return "unknown".into();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

#[cfg(not(unix))]
fn hostname() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "unknown".into())
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks whether the process exists.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn alive(_pid: u32) -> bool {
    true
}
