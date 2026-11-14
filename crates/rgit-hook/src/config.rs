// SPDX-License-Identifier: MIT
//! Configured hooks (Documentation/config/hook.adoc): `hook.<name>.command`,
//! `hook.<name>.event`, `hook.<name>.enabled`, `hook.<name>.parallel`,
//! `hook.<event>.enabled`, `hook.<event>.jobs` and `hook.jobs`.

use std::collections::HashMap;

use bstr::ByteSlice;
use rgit_config::{ConfigSet, Scope};

use crate::{is_known, messages as msg};

/// One `hook.<name>.*` subsection.
#[derive(Debug, Clone, Default)]
pub(crate) struct Named {
    /// `hook.<name>.command` (last one wins) and the scope it came from.
    pub command: Option<(String, Scope)>,
    /// The scope of the last `hook.<name>.event`.
    pub event_scope: Option<Scope>,
    /// `hook.<name>.enabled`.
    pub enabled: Option<bool>,
    /// `hook.<name>.parallel`.
    pub parallel: Option<bool>,
    /// `hook.<name>.jobs` (only meaningful for event names).
    pub jobs: Option<String>,
    /// Whether any `hook.<name>.event` was seen (even one reset later).
    pub has_event: bool,
}

/// The `hook.*` configuration, in the order the variables were read.
#[derive(Debug, Clone, Default)]
pub(crate) struct HookConfig {
    /// Subsections by name, in order of first appearance.
    pub order: Vec<String>,
    pub named: HashMap<String, Named>,
    /// Event -> friendly names in the order of their (last) event line.
    pub events: HashMap<String, Vec<String>>,
    /// `hook.jobs`.
    pub jobs: Option<String>,
}

impl HookConfig {
    pub fn read(config: &ConfigSet) -> HookConfig {
        let mut hc = HookConfig::default();
        for e in config.entries() {
            let Some(rest) = e.name.strip_prefix(b"hook.") else {
                continue;
            };
            let value = e.value.as_ref().map(|v| v.to_str_lossy().into_owned());
            let Some(dot) = rest.rfind_byte(b'.') else {
                if rest == b"jobs" {
                    hc.jobs = Some(value.unwrap_or_default());
                }
                continue;
            };
            let name = rest[..dot].to_str_lossy().into_owned();
            let key = &rest[dot + 1..];
            if !hc.named.contains_key(&name) {
                hc.order.push(name.clone());
            }
            let n = hc.named.entry(name.clone()).or_default();
            match key {
                b"command" => n.command = value.map(|v| (v, e.source.scope)),
                b"event" => {
                    let ev = value.unwrap_or_default();
                    n.has_event = true;
                    n.event_scope = Some(e.source.scope);
                    if ev.is_empty() {
                        for list in hc.events.values_mut() {
                            list.retain(|x| *x != name);
                        }
                    } else {
                        let list = hc.events.entry(ev).or_default();
                        list.retain(|x| *x != name);
                        list.push(name);
                    }
                }
                b"enabled" => n.enabled = e.bool().ok(),
                b"parallel" => {
                    n.parallel = match e.bool() {
                        Ok(b) => Some(b),
                        Err(_) => {
                            crate::warn(&msg::not_boolean(
                                &format!("hook.{name}.parallel"),
                                value.as_deref().unwrap_or(""),
                            ));
                            None
                        }
                    }
                }
                b"jobs" => n.jobs = Some(value.unwrap_or_default()),
                _ => {}
            }
        }
        hc
    }

    /// The fatal configuration errors git checks before running any hook.
    pub fn check(&self) -> Result<(), String> {
        for name in &self.order {
            let n = &self.named[name];
            let has_events = self.events.values().any(|l| l.contains(name));
            if n.has_event && is_known(name) {
                return Err(msg::collides_with_event(name));
            }
            if has_events && n.command.is_none() && n.enabled != Some(false) {
                return Err(msg::missing_command(name));
            }
        }
        Ok(())
    }

    /// Warnings about `hook.<friendly-name>.jobs`.
    pub fn warn_misplaced_jobs(&self) {
        for name in &self.order {
            let n = &self.named[name];
            if n.jobs.is_some() && (n.command.is_some() || n.has_event || n.parallel.is_some()) {
                crate::warn(&msg::jobs_on_friendly_name(name));
            }
        }
    }

    /// Whether `hook.<event>.enabled` switches the configured hooks of
    /// `event` off. For an unknown event that is also a friendly name the
    /// setting belongs to that hook only.
    pub fn event_disabled(&self, event: &str) -> bool {
        let Some(n) = self.named.get(event) else {
            return false;
        };
        if !is_known(event) && (n.command.is_some() || n.has_event) {
            return false;
        }
        n.enabled == Some(false)
    }

    /// A jobs value: `Some(n)` for a positive count or -1 (CPU count),
    /// `None` (with a warning) for anything else.
    pub fn parse_jobs(key: &str, value: &str) -> Option<usize> {
        match value.trim().parse::<i64>() {
            Ok(-1) => Some(crate::cpus()),
            Ok(n) if n > 0 => Some(n as usize),
            Ok(_) => {
                crate::warn(&msg::jobs_not_positive(key, value));
                None
            }
            Err(_) => {
                crate::warn(&msg::jobs_not_integer(key, value));
                None
            }
        }
    }

    /// The configured job count for `event`: `hook.<event>.jobs`, then
    /// `hook.jobs`.
    pub fn jobs_for(&self, event: &str) -> Option<usize> {
        let per_event = self
            .named
            .get(event)
            .and_then(|n| n.jobs.as_deref())
            .and_then(|v| Self::parse_jobs(&format!("hook.{event}.jobs"), v));
        per_event.or_else(|| {
            self.jobs
                .as_deref()
                .and_then(|v| Self::parse_jobs("hook.jobs", v))
        })
    }
}
