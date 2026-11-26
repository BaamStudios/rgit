// SPDX-License-Identifier: MIT
//! User-visible messages of the submodule library (CLEANROOM.md: reproduced
//! verbatim where scripts depend on them, collected here for review). The
//! functions return the text without the `warning: `/`error: `/`fatal: `
//! prefix.

use bstr::BStr;

pub const EMPTY_REMOTE_URL: &str = "invalid empty remote_url";

pub fn cannot_strip_component(url: &BStr) -> String {
    format!("cannot strip one component off url '{url}'")
}

pub fn suspicious_name(name: &BStr) -> String {
    format!("ignoring suspicious submodule name: {name}")
}

pub fn looks_like_option(name: &BStr, key: &str, value: &BStr) -> String {
    format!(
        "ignoring 'submodule.{name}.{key}' which may be interpreted as a command-line option: {value}"
    )
}

pub fn invalid_ignore(value: &BStr, name: &BStr) -> String {
    format!("Invalid parameter '{value}' for config option 'submodule.{name}.ignore'")
}

pub fn invalid_update(name: &BStr) -> String {
    format!("invalid value for 'submodule.{name}.update'")
}

pub fn bad_fetch_recurse(name: &BStr, value: &BStr) -> String {
    format!("bad submodule.{name}.fetchrecursesubmodules argument: {value}")
}

pub fn bad_bool(value: &BStr, name: &BStr, key: &str) -> String {
    format!("bad boolean config value '{value}' for 'submodule.{name}.{key}'")
}

pub fn missing_value(name: &BStr, key: &str) -> String {
    format!("missing value for 'submodule.{name}.{key}'")
}

pub fn bad_config_line(line: usize, origin: &str) -> String {
    format!("bad config line {line} in {origin}")
}

pub fn multiple_configurations(label: &str, name: &BStr, key: &str) -> String {
    format!(
        "{label}, multiple configurations found for 'submodule.{name}.{key}'. Skipping second one!"
    )
}

/// Findings of the `.gitmodules` object checks ([`crate::fsck`]).
pub mod fsck {
    pub const GITMODULES_PARSE: &str = "could not parse gitmodules blob";

    pub fn disallowed_name(name: &str) -> String {
        format!("disallowed submodule name: {name}")
    }

    pub fn disallowed_url(url: &str) -> String {
        format!("disallowed submodule url: {url}")
    }

    pub fn disallowed_path(path: &str) -> String {
        format!("disallowed submodule path: {path}")
    }

    pub fn disallowed_update(value: &str) -> String {
        format!("disallowed submodule update setting: {value}")
    }
}
