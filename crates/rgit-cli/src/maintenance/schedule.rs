// SPDX-License-Identifier: MIT
//! `git maintenance register | unregister | start | stop`: the list of
//! maintained repositories (`maintenance.repo`) and the background
//! schedulers (Documentation/git-maintenance.adoc, "BACKGROUND MAINTENANCE
//! ON ..."): `crontab`, `systemd-timer`, `launchctl` and `schtasks`.
//!
//! Each scheduler runs `<rgit> --exec-path=<path> -c
//! credential.interactive=false -c core.askPass=true for-each-repo
//! --keep-going --config=maintenance.repo maintenance run
//! --schedule=<frequency>` hourly, daily and weekly at a random minute; the
//! files and command lines are those of the reference git.
//!
//! `GIT_TEST_MAINT_SCHEDULER=<name>:<command>[,...]` (the variable git's
//! test suite uses) replaces the scheduler programs: only the listed ones
//! (`crontab`, `systemctl`, `launchctl`, `schtasks`) are available, each
//! run as the given command (split at spaces) followed by its arguments.
//! Without it, `systemctl --user list-timers` decides whether systemd
//! timers are available and a `crontab` program on `PATH` whether cron is.
//! A crontab is rewritten only when the git maintenance region changes.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use bstr::ByteSlice;
use rgit_config::write::set_in_file;
use rgit_config::{ConfigSet, Origin, Scope, SetRequest, ValuePattern};

use super::tasks::Schedule;
use crate::messages::maintenance as msg;
use crate::prelude::*;
use crate::repository::Repo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheduler {
    Crontab,
    Systemd,
    Launchctl,
    Schtasks,
}

/// The order in which schedulers are disabled.
const ORDER: [Scheduler; 4] = [
    Scheduler::Crontab,
    Scheduler::Systemd,
    Scheduler::Launchctl,
    Scheduler::Schtasks,
];

impl Scheduler {
    /// `--scheduler=<name>`: `None` for `auto`.
    pub fn parse(s: &str) -> Result<Option<Scheduler>, ()> {
        match s {
            "auto" => Ok(None),
            "crontab" => Ok(Some(Scheduler::Crontab)),
            "systemd" | "systemd-timer" => Ok(Some(Scheduler::Systemd)),
            "launchctl" => Ok(Some(Scheduler::Launchctl)),
            "schtasks" => Ok(Some(Scheduler::Schtasks)),
            _ => Err(()),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Scheduler::Crontab => "crontab",
            Scheduler::Systemd => "systemd-timer",
            Scheduler::Launchctl => "launchctl",
            Scheduler::Schtasks => "schtasks",
        }
    }

    /// The program name, also the key in `GIT_TEST_MAINT_SCHEDULER`.
    fn program(self) -> &'static str {
        match self {
            Scheduler::Crontab => "crontab",
            Scheduler::Systemd => "systemctl",
            Scheduler::Launchctl => "launchctl",
            Scheduler::Schtasks => "schtasks",
        }
    }
}

/// How the scheduler programs are found.
struct Programs {
    /// `GIT_TEST_MAINT_SCHEDULER` entries: program name and command.
    test: Option<Vec<(String, Vec<String>)>>,
}

impl Programs {
    fn from_env() -> Programs {
        let test = std::env::var("GIT_TEST_MAINT_SCHEDULER").ok().map(|v| {
            v.split(',')
                .filter_map(|e| e.split_once(':'))
                .map(|(name, cmd)| {
                    (
                        name.to_string(),
                        cmd.split(' ')
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .collect(),
                    )
                })
                .collect()
        });
        Programs { test }
    }

    /// The command (program and leading arguments) for `s`, if available.
    fn command(&self, s: Scheduler) -> Option<Vec<String>> {
        match &self.test {
            Some(list) => list
                .iter()
                .find(|(name, _)| name == s.program())
                .map(|(_, cmd)| cmd.clone()),
            None => available_natively(s).then(|| vec![s.program().to_string()]),
        }
    }

    fn run(&self, s: Scheduler, args: &[&str]) -> Option<Command> {
        let cmd = self.command(s)?;
        let (prog, rest) = cmd.split_first()?;
        let mut c = Command::new(prog);
        c.args(rest).args(args);
        Some(c)
    }
}

fn available_natively(s: Scheduler) -> bool {
    match s {
        Scheduler::Launchctl => cfg!(target_os = "macos"),
        Scheduler::Schtasks => cfg!(windows),
        Scheduler::Systemd => {
            cfg!(target_os = "linux")
                && Command::new("systemctl")
                    .args(["--user", "list-timers"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|s| s.success())
        }
        Scheduler::Crontab => {
            !cfg!(windows)
                && std::env::var_os("PATH").is_some_and(|path| {
                    std::env::split_paths(&path).any(|d| d.join("crontab").is_file())
                })
        }
    }
}

/// What the schedulers run.
struct Invocation {
    exe: String,
    exec_path: String,
}

impl Invocation {
    fn new(ctx: &Context) -> Result<Invocation, Die> {
        let exe = std::env::current_exe().map_err(|e| Die::Fatal(e.to_string()))?;
        Ok(Invocation {
            exe: exe.to_string_lossy().into_owned(),
            exec_path: ctx.exec_path.to_string_lossy().into_owned(),
        })
    }

    /// The command line for crontab and systemd.
    fn line(&self, schedule: &str) -> String {
        format!(
            "\"{}\" --exec-path=\"{}\" -c credential.interactive=false -c core.askPass=true  \
             for-each-repo --keep-going --config=maintenance.repo maintenance run --schedule={schedule}",
            self.exe, self.exec_path
        )
    }
}

fn random_minute() -> u32 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    (nanos ^ std::process::id().wrapping_mul(2_654_435_761)) % 60
}

/// `git maintenance start`.
pub fn start(ctx: &Context, repo: &Repo, choice: Option<Scheduler>) -> Result<i32, Die> {
    let lock = schedule_lock(repo)?;
    let programs = Programs::from_env();
    let chosen = match choice {
        Some(s) => {
            if programs.command(s).is_none() {
                return Err(Die::Fatal(msg::scheduler_unavailable(s.name())));
            }
            s
        }
        None => auto_scheduler(&programs)?,
    };
    let inv = Invocation::new(ctx)?;
    for s in ORDER {
        if s != chosen && programs.command(s).is_some() {
            disable(&programs, s, repo);
        }
    }
    let minute = random_minute();
    if let Err(e) = enable(&programs, chosen, repo, &inv, minute) {
        drop(lock);
        if let Some(m) = e {
            return Err(Die::Fatal(m));
        }
        return Err(Die::Fatal(msg::SETUP_FAILED.into()));
    }
    drop(lock);
    register(ctx, repo, None)
}

/// `git maintenance stop`.
pub fn stop(repo: &Repo) -> Result<i32, Die> {
    let lock = schedule_lock(repo)?;
    let programs = Programs::from_env();
    for s in ORDER {
        if programs.command(s).is_some() {
            disable(&programs, s, repo);
        }
    }
    drop(lock);
    Ok(0)
}

fn auto_scheduler(programs: &Programs) -> Result<Scheduler, Die> {
    let candidates: &[Scheduler] = if cfg!(target_os = "macos") {
        &[Scheduler::Launchctl]
    } else if cfg!(windows) {
        &[Scheduler::Schtasks]
    } else {
        &[Scheduler::Systemd, Scheduler::Crontab]
    };
    candidates
        .iter()
        .copied()
        .find(|s| programs.command(*s).is_some())
        .ok_or_else(|| Die::Fatal(msg::NO_SCHEDULER.into()))
}

/// `objects/schedule.lock` keeps two `start`/`stop` apart.
fn schedule_lock(repo: &Repo) -> Result<rgit_fs::LockFile, Die> {
    let objects = repo
        .setup
        .cwd
        .join(crate::repo_objects::objects_dir(&repo.setup));
    let path = objects.join("schedule");
    rgit_fs::LockFile::acquire(&path, &rgit_fs::LockOptions::new()).map_err(|_| {
        let lock = objects.join("schedule.lock");
        eprintln!(
            "error: {}\n\n{}",
            msg::unable_to_create(&lock.display().to_string()),
            msg::SCHEDULE_LOCKED
        );
        Die::Fatal(msg::SETUP_FAILED.into())
    })
}

/// Enable `s`; `Err(Some(m))` dies with `m`, `Err(None)` with the
/// general message.
fn enable(
    programs: &Programs,
    s: Scheduler,
    repo: &Repo,
    inv: &Invocation,
    minute: u32,
) -> Result<(), Option<String>> {
    match s {
        Scheduler::Crontab => crontab_update(programs, Some((inv, minute))).map_err(|()| None),
        Scheduler::Systemd => systemd_enable(programs, inv, minute),
        Scheduler::Launchctl => launchctl_enable(programs, inv, minute),
        Scheduler::Schtasks => schtasks_enable(programs, repo, inv, minute),
    }
}

fn disable(programs: &Programs, s: Scheduler, _repo: &Repo) {
    match s {
        Scheduler::Crontab => {
            let _ = crontab_update(programs, None);
        }
        Scheduler::Systemd => systemd_disable(programs),
        Scheduler::Launchctl => launchctl_disable(programs),
        Scheduler::Schtasks => {
            for f in Schedule::ALL {
                if let Some(mut c) = programs.run(s, &["/delete", "/tn", &schtasks_name(f), "/f"]) {
                    let _ = c.stdin(Stdio::null()).status();
                }
            }
        }
    }
}

// crontab

const CRON_BEGIN: &str = "# BEGIN GIT MAINTENANCE SCHEDULE";
const CRON_END: &str = "# END GIT MAINTENANCE SCHEDULE";

/// Replace (`Some`) or remove (`None`) the maintenance region of the
/// crontab.
fn crontab_update(programs: &Programs, region: Option<(&Invocation, u32)>) -> Result<(), ()> {
    let Some(mut list) = programs.run(Scheduler::Crontab, &["-l"]) else {
        return Err(());
    };
    let old = list
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| o.stdout)
        .unwrap_or_default();
    let old = String::from_utf8_lossy(&old).into_owned();
    let mut out = String::new();
    let mut inside = false;
    for line in old.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\n', '\r']);
        if bare == CRON_BEGIN {
            inside = true;
        } else if bare == CRON_END {
            inside = false;
        } else if !inside {
            out.push_str(line);
        }
    }
    if let Some((inv, minute)) = region {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(CRON_BEGIN);
        out.push('\n');
        out.push_str(msg::CRON_HEADER);
        out.push_str(&format!("{minute} 1-23 * * * {}\n", inv.line("hourly")));
        out.push_str(&format!("{minute} 0 * * 1-6 {}\n", inv.line("daily")));
        out.push_str(&format!("{minute} 0 * * 0 {}\n", inv.line("weekly")));
        out.push('\n');
        out.push_str(CRON_END);
        out.push('\n');
    }
    if out == old {
        return Ok(());
    }
    let tmp = std::env::temp_dir().join(format!(
        ".git_cron_edit_tmp{}{}",
        std::process::id(),
        random_minute()
    ));
    let written = std::fs::write(&tmp, out.as_bytes());
    let tmp_arg = tmp.to_string_lossy().into_owned();
    let ok = written.is_ok()
        && programs
            .run(Scheduler::Crontab, &[&tmp_arg])
            .is_some_and(|mut c| c.stdin(Stdio::null()).status().is_ok_and(|s| s.success()));
    let _ = std::fs::remove_file(&tmp);
    if ok {
        Ok(())
    } else {
        eprintln!("error: {}", msg::died("crontab"));
        Err(())
    }
}

// systemd

fn systemd_dir() -> Option<PathBuf> {
    match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        Some(x) => Some(PathBuf::from(x).join("systemd").join("user")),
        None => std::env::var_os("HOME").filter(|v| !v.is_empty()).map(|h| {
            PathBuf::from(h)
                .join(".config")
                .join("systemd")
                .join("user")
        }),
    }
}

fn timer_unit(f: Schedule) -> String {
    format!("git-maintenance@{}.timer", f.name())
}

fn systemd_enable(
    programs: &Programs,
    inv: &Invocation,
    minute: u32,
) -> Result<(), Option<String>> {
    let dir = systemd_dir().ok_or(None)?;
    std::fs::create_dir_all(&dir).map_err(|_| None)?;
    for f in Schedule::ALL {
        let calendar = match f {
            Schedule::Hourly => format!("*-*-* 1..23:{minute:02}:00"),
            Schedule::Daily => format!("Tue..Sun *-*-* 0:{minute:02}:00"),
            Schedule::Weekly => format!("Mon 0:{minute:02}:00"),
        };
        let text = msg::systemd_timer(&calendar);
        std::fs::write(dir.join(timer_unit(f)), text).map_err(|_| None)?;
    }
    let service = msg::systemd_service(&inv.line("%i"));
    std::fs::write(dir.join("git-maintenance@.service"), service).map_err(|_| None)?;
    for f in Schedule::ALL {
        let unit = timer_unit(f);
        let ok = programs
            .run(Scheduler::Systemd, &["--user", "enable", "--now", &unit])
            .is_some_and(|mut c| c.stdin(Stdio::null()).status().is_ok_and(|s| s.success()));
        if !ok {
            eprintln!("error: {}", msg::SYSTEMCTL_FAILED);
            // Undo what was set up so far.
            systemd_disable(programs);
            return Err(None);
        }
    }
    Ok(())
}

fn systemd_disable(programs: &Programs) {
    for f in Schedule::ALL {
        let unit = timer_unit(f);
        if let Some(mut c) =
            programs.run(Scheduler::Systemd, &["--user", "disable", "--now", &unit])
        {
            let _ = c.stdin(Stdio::null()).status();
        }
    }
    if let Some(dir) = systemd_dir() {
        for f in Schedule::ALL {
            let _ = std::fs::remove_file(dir.join(timer_unit(f)));
        }
        let _ = std::fs::remove_file(dir.join("git-maintenance@.service"));
    }
}

// launchctl

fn launch_agents() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(|h| PathBuf::from(h).join("Library").join("LaunchAgents"))
}

fn launch_label(f: Schedule) -> String {
    format!("org.git-scm.git.{}", f.name())
}

fn gui_domain() -> String {
    #[cfg(unix)]
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    #[cfg(not(unix))]
    let uid = 0;
    format!("gui/{uid}")
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn plist(inv: &Invocation, f: Schedule, minute: u32) -> String {
    let mut s = msg::plist_start(&launch_label(f));
    let schedule = format!("--schedule={}", f.name());
    let exec = format!("--exec-path={}", inv.exec_path);
    for a in [
        inv.exe.as_str(),
        &exec,
        "-c",
        "credential.interactive=false",
        "-c",
        "core.askPass=true",
        "for-each-repo",
        "--keep-going",
        "--config=maintenance.repo",
        "maintenance",
        "run",
        &schedule,
    ] {
        s.push_str(&format!("<string>{}</string>\n", xml_escape(a)));
    }
    s.push_str(msg::PLIST_CALENDAR);
    let entry = |weekday: Option<u32>, hour: u32| msg::plist_interval(weekday, hour, minute);
    match f {
        Schedule::Hourly => (1..=23).for_each(|h| s.push_str(&entry(None, h))),
        Schedule::Daily => (1..=6).for_each(|d| s.push_str(&entry(Some(d), 0))),
        Schedule::Weekly => s.push_str(&entry(Some(0), 0)),
    }
    s.push_str("</array>\n</dict>\n</plist>\n");
    s
}

fn launchctl_ok(programs: &Programs, args: &[&str]) -> bool {
    programs
        .run(Scheduler::Launchctl, args)
        .is_some_and(|mut c| {
            c.stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        })
}

fn launchctl_enable(
    programs: &Programs,
    inv: &Invocation,
    minute: u32,
) -> Result<(), Option<String>> {
    let dir = launch_agents().ok_or(None)?;
    std::fs::create_dir_all(&dir).map_err(|_| None)?;
    let domain = gui_domain();
    for f in Schedule::ALL {
        let path = dir.join(format!("{}.plist", launch_label(f)));
        let shown = path.to_string_lossy().into_owned();
        let label = launch_label(f);
        // An identical, loaded definition needs no work. The minute is
        // random, so compare without it.
        if let Ok(old) = std::fs::read_to_string(&path) {
            let same = (0..60).any(|m| old == plist(inv, f, m));
            if same && launchctl_ok(programs, &["list", &label]) {
                continue;
            }
        }
        std::fs::write(&path, plist(inv, f, minute)).map_err(|_| None)?;
        launchctl_ok(programs, &["bootout", &domain, &shown]);
        if !launchctl_ok(programs, &["bootstrap", &domain, &shown]) {
            return Err(Some(msg::bootstrap_failed(&shown)));
        }
    }
    Ok(())
}

fn launchctl_disable(programs: &Programs) {
    let Some(dir) = launch_agents() else {
        return;
    };
    let domain = gui_domain();
    for f in Schedule::ALL {
        let path = dir.join(format!("{}.plist", launch_label(f)));
        let shown = path.to_string_lossy().into_owned();
        launchctl_ok(programs, &["bootout", &domain, &shown]);
        let _ = std::fs::remove_file(&path);
    }
}

// schtasks

fn schtasks_name(f: Schedule) -> String {
    format!("Git Maintenance ({})", f.name())
}

fn schtasks_xml(inv: &Invocation, f: Schedule, minute: u32) -> String {
    let (hour, trigger) = match f {
        Schedule::Hourly => (1, msg::SCHTASKS_HOURLY),
        Schedule::Daily => (0, msg::SCHTASKS_DAILY),
        Schedule::Weekly => (0, msg::SCHTASKS_WEEKLY),
    };
    msg::schtasks_xml(
        hour,
        minute,
        trigger,
        &xml_escape(&inv.exe),
        &xml_escape(&inv.exec_path),
        f.name(),
    )
}

fn schtasks_enable(
    programs: &Programs,
    repo: &Repo,
    inv: &Invocation,
    minute: u32,
) -> Result<(), Option<String>> {
    let git_dir = repo.setup.absolute_git_dir();
    for f in Schedule::ALL {
        let tmp = git_dir.join(format!(
            "schedule_{}_{:06}",
            f.name(),
            std::process::id() % 1_000_000
        ));
        std::fs::write(&tmp, schtasks_xml(inv, f, minute)).map_err(|_| None)?;
        let name = schtasks_name(f);
        let file = tmp.to_string_lossy().into_owned();
        let ok = programs
            .run(
                Scheduler::Schtasks,
                &["/create", "/tn", &name, "/f", "/xml", &file],
            )
            .is_some_and(|mut c| c.stdin(Stdio::null()).status().is_ok_and(|s| s.success()));
        let _ = std::fs::remove_file(&tmp);
        if !ok {
            return Err(None);
        }
    }
    Ok(())
}

// register / unregister

/// The path `maintenance.repo` lists for this repository: the work tree,
/// or the git directory of a bare repository.
fn repo_path(repo: &Repo) -> String {
    let p = match &repo.setup.work_tree {
        Some(wt) => repo.setup.cwd.join(wt),
        None => repo.setup.absolute_git_dir(),
    };
    let p = std::fs::canonicalize(&p).unwrap_or_else(|_| rgit_repo::path::normalize(&p));
    p.to_string_lossy().into_owned()
}

/// The global file `git config --global` writes.
fn global_file() -> Option<PathBuf> {
    if let Some(g) = std::env::var_os("GIT_CONFIG_GLOBAL") {
        return Some(PathBuf::from(g));
    }
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    let user = PathBuf::from(home).join(".gitconfig");
    let env = rgit_config::SystemEnv::default();
    let paths = rgit_config::sources::global_config_paths(&env);
    Some(match paths.first() {
        Some(xdg) if *xdg != user && xdg.exists() && !user.exists() => xdg.clone(),
        _ => user,
    })
}

/// The values of `maintenance.repo`. A value-less entry is reported and
/// makes the list empty.
fn registered(config: &ConfigSet) -> Vec<String> {
    let mut out = Vec::new();
    for e in config.get_all("maintenance.repo") {
        match &e.value {
            Some(v) => out.push(v.to_str_lossy().into_owned()),
            None => {
                eprintln!("error: {}", msg::missing_value("maintenance.repo"));
                return Vec::new();
            }
        }
    }
    out
}

fn file_config(path: &Path) -> ConfigSet {
    let data = std::fs::read(path).unwrap_or_default();
    rgit_config::parse_bytes(&data, Origin::File(path.to_path_buf()), Scope::Command)
        .unwrap_or_default()
}

fn set(path: &Path, key: &str, value: Option<&str>, pattern: &ValuePattern, all: bool) -> bool {
    let req = SetRequest {
        key: key.as_bytes(),
        value: value.map(str::as_bytes),
        pattern,
        all,
        comment: None,
    };
    set_in_file(path, &Default::default(), &req).is_ok()
}

/// `git maintenance register [--config-file <path>]`.
pub fn register(_ctx: &Context, repo: &Repo, config_file: Option<&str>) -> Result<i32, Die> {
    let path = repo_path(repo);
    let local = repo.setup.absolute_common_dir().join("config");
    let (target, list_config) = match config_file {
        Some(f) => {
            let p = PathBuf::from(f);
            let c = file_config(&p);
            (p, c)
        }
        None => match global_file() {
            Some(p) => (p, repo.config.clone()),
            None => return Ok(0),
        },
    };
    // Foreground maintenance is off for registered repositories.
    set(
        &local,
        "maintenance.auto",
        Some("false"),
        &ValuePattern::Any,
        false,
    );
    if repo.config.get("maintenance.strategy").is_none() {
        set(
            &local,
            "maintenance.strategy",
            Some("incremental"),
            &ValuePattern::Any,
            false,
        );
    }
    if registered(&list_config).contains(&path) {
        return Ok(0);
    }
    set(
        &target,
        "maintenance.repo",
        Some(&path),
        &ValuePattern::Never,
        false,
    );
    Ok(0)
}

/// `git maintenance unregister [--config-file <path>] [--force]`.
pub fn unregister(repo: &Repo, config_file: Option<&str>, force: bool) -> Result<i32, Die> {
    let path = repo_path(repo);
    let (target, list_config) = match config_file {
        Some(f) => {
            let p = PathBuf::from(f);
            let c = file_config(&p);
            (Some(p), c)
        }
        None => (global_file(), repo.config.clone()),
    };
    let found = registered(&list_config).contains(&path);
    if !found {
        if force {
            return Ok(0);
        }
        return Err(Die::Fatal(msg::not_registered(&path)));
    }
    if let Some(target) = target {
        let pattern = ValuePattern::exact(path.as_bytes());
        set(&target, "maintenance.repo", None, &pattern, true);
    }
    Ok(0)
}
