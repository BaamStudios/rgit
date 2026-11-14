// SPDX-License-Identifier: MIT
//! User-visible messages of this crate, reproduced verbatim from what the
//! reference `git` binary prints (see CLEANROOM.md, "User-visible
//! messages"). Texts are given without the `fatal: ` / `error: ` /
//! `warning: ` prefix unless noted.

/// The `advice.ignoredHook` hint (two `hint: ` lines; the second is shown
/// whatever `advice.ignoredHook` is set to).
pub fn ignored_hook(path: &str) -> String {
    format!(
        "The '{path}' hook was ignored because it's not set as executable.\n\
         You can disable this warning with `git config set advice.ignoredHook false`."
    )
}

pub fn cannot_find_hook(name: &str) -> String {
    format!("cannot find a hook named {name}")
}

pub fn unknown_hook_event(name: &str) -> String {
    format!(
        "unknown hook event '{name}';\nuse --allow-unknown-hook-name to allow non-native hook names"
    )
}

pub fn no_hooks_found(name: &str) -> String {
    format!("no hooks found for event '{name}'")
}

pub fn missing_command(name: &str) -> String {
    format!(
        "'hook.{name}.command' must be configured or 'hook.{name}.event' must be removed; aborting."
    )
}

pub fn disabled_without_command(name: &str) -> String {
    format!("disabled hook '{name}' has no command configured")
}

pub fn collides_with_event(name: &str) -> String {
    format!(
        "hook friendly-name '{name}' collides with a known event name; please choose a different friendly-name"
    )
}

pub fn same_as_event(name: &str) -> String {
    format!(
        "hook friendly-name '{name}' is the same as its event; this may cause ambiguity with hook.{name}.enabled"
    )
}

pub fn jobs_not_positive(key: &str, value: &str) -> String {
    format!("{key} must be a positive integer or -1, ignoring: '{value}'")
}

pub fn jobs_not_integer(key: &str, value: &str) -> String {
    format!("{key} must be an integer, ignoring: '{value}'")
}

pub fn not_boolean(key: &str, value: &str) -> String {
    format!("{key} must be a boolean, ignoring: '{value}'")
}

pub fn jobs_on_friendly_name(name: &str) -> String {
    format!(
        "hook.{name}.jobs is set but '{name}' looks like a hook friendly-name, not an event name; \
         hook.<event>.jobs uses the event name (e.g. hook.post-receive.jobs), so this setting will be ignored"
    )
}

pub fn not_marked_parallel(name: &str, jobs: &str) -> String {
    format!(
        "hook '{name}' is not marked as parallel=true, running in parallel anyway due to -j{jobs}"
    )
}

pub fn invalid_jobs_flag(value: i64) -> String {
    format!("invalid value for -j: {value} (use -1 for CPU count or a positive integer)")
}

pub fn cannot_exec(path: &str, reason: &str) -> String {
    format!("fatal: cannot exec '{path}': {reason}")
}

pub fn died_of_signal(cmd: &str, signal: i32) -> String {
    format!("error: {cmd} died of signal {signal}")
}

pub fn could_not_open_for_reading(path: &str, reason: &str) -> String {
    format!("could not open '{path}' for reading: {reason}")
}

/// `git hook list` for the hook in the hook directory.
pub const FROM_HOOKDIR: &str = "hook from hookdir";
/// `git hook list` prefix of a disabled configured hook.
pub const DISABLED: &str = "disabled";
/// `git hook list` prefix of a configured hook whose event is disabled.
pub const EVENT_DISABLED: &str = "event-disabled";
