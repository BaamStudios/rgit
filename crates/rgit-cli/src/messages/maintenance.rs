// SPDX-License-Identifier: MIT
//! Messages of `git maintenance` and `git for-each-repo` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &["git maintenance <subcommand> [<options>]"];
pub const RUN_USAGE: &[&str] =
    &["git maintenance run [--auto] [--[no-]quiet] [--task=<task>] [--schedule]"];
pub const START_USAGE: &[&str] = &["git maintenance start [--scheduler=<scheduler>]"];
pub const STOP_USAGE: &[&str] = &["git maintenance stop"];
pub const REGISTER_USAGE: &[&str] = &["git maintenance register [--config-file <path>]"];
pub const UNREGISTER_USAGE: &[&str] =
    &["git maintenance unregister [--config-file <path>] [--force]"];
pub const IS_NEEDED_USAGE: &[&str] = &["git maintenance is-needed [--task=<task>] [--schedule]"];

pub const H_AUTO: &str = "run tasks based on the state of the repository";
pub const H_DETACH: &str = "perform maintenance in the background";
pub const H_SCHEDULE: &str = "run tasks based on frequency";
pub const H_QUIET: &str = "do not report progress or other information over stderr";
pub const H_TASK: &str = "run a specific task";
pub const H_TASK_CHECK: &str = "check a specific task";
pub const H_SCHEDULER: &str = "scheduler to trigger git maintenance run";
pub const H_CONFIG_FILE: &str = "use given config file";
pub const H_FORCE: &str = "return success even if repository was not registered";
pub const ARGH_FREQUENCY: &str = "frequency";
pub const ARGH_TASK: &str = "task";
pub const ARGH_SCHEDULER: &str = "scheduler";

pub fn unknown_strategy(name: &str) -> String {
    format!("unknown maintenance strategy: '{name}'")
}

pub fn incompatible(a: &str, b: &str) -> String {
    format!("options '{a}' and '{b}' cannot be used together")
}

pub fn unrecognized_schedule(arg: &str) -> String {
    format!("unrecognized --schedule argument '{arg}'")
}

pub fn not_a_task(arg: &str) -> String {
    format!("'{arg}' is not a valid task")
}

pub fn task_twice(arg: &str) -> String {
    format!("task '{arg}' cannot be selected multiple times")
}

pub fn task_failed(name: &str) -> String {
    format!("task '{name}' failed")
}

pub const PREFETCH_FAILED: &str = "failed to prefetch remotes";
pub const LOOSE_OBJECTS_FAILED: &str = "failed to prune packed objects";
pub const LOOSE_PACK_FAILED: &str = "failed to write a pack of loose objects";

pub fn lock_skipping(path: &str) -> String {
    format!("lock file '{path}' exists, skipping maintenance")
}

pub fn not_registered(path: &str) -> String {
    format!("repository '{path}' is not registered")
}

pub fn missing_value(key: &str) -> String {
    format!("missing value for '{key}'")
}

pub fn unrecognized_scheduler(arg: &str) -> String {
    format!("unrecognized --scheduler argument '{arg}'")
}

pub fn scheduler_unavailable(name: &str) -> String {
    format!("{name} scheduler is not available")
}

pub const NO_SCHEDULER: &str = "neither systemd timers nor crontab are available";
pub const SETUP_FAILED: &str = "failed to set up maintenance schedule";
pub const SYSTEMCTL_FAILED: &str = "failed to run systemctl";

pub fn unable_to_create(path: &str) -> String {
    format!("unable to create '{path}': File exists.")
}

pub const SCHEDULE_LOCKED: &str =
    "Another scheduled git-maintenance(1) process seems to be running in this
repository. Please make sure no other maintenance processes are running and
then try again. If it still fails, a git-maintenance(1) process may have
crashed in this repository earlier: remove the file manually to continue.";

pub fn died(program: &str) -> String {
    format!("'{program}' died")
}

pub fn bootstrap_failed(plist: &str) -> String {
    format!("failed to bootstrap service {plist}")
}

// for-each-repo

pub const FOR_EACH_REPO_USAGE: &[&str] = &["git for-each-repo --config=<config> [--] <arguments>"];
pub const H_FER_CONFIG: &str = "config key storing a list of repository paths";
pub const H_FER_KEEP_GOING: &str = "keep going even if command fails in a repository";
pub const ARGH_CONFIG: &str = "config";
pub const MISSING_CONFIG: &str = "missing --config=<config>";

pub fn bad_config(key: &str) -> String {
    format!("got bad config --config={key}")
}
